#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# spike.sh — CHILL 阶段零验证（不持久化）
#
#   spike.sh prepare   # Mac 上下载 mihomo + .mrs，校验后推到设备 /tmp/chill-spike
#   spike.sh run       # 挂死人开关，起核心，逐项验证，结束后自动清理
#   spike.sh clean     # 手动清理（run 异常中断时用）
#
# 设计依据：`/plan-design-review` 2026-09-16
# 决定 1A（临时 FORWARD 放行）、1B（unreachable 路由）、2F（按段清理）、
# O2/O3（开销与基线）、O9（IPv6）、O10（reload 项挪到安装后）。
#
# 硬约束（来自工作区 CLAUDE.md 与设计文档）：
#   • 设备写入只落 /tmp/chill-spike，不碰 /data、/etc/config、/etc/rc.local
#   • 不改 uci，不重启设备，不动 Tailscale
#   • 所有退出路径都要删掉临时 iptables 规则、nft 表、ip rule、unreachable 路由
#   • 死人开关：设备端 300 秒后无条件自毁，SSH 断了也能恢复
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HOST="${HOST:-u60}"
# 设备端沙盒目录。故意放 /data/local/tmp 而不是 /tmp：/tmp 是内存盘，解压后的
# mihomo 约 50MB 会直接占用无 swap 设备的内存。这里只放临时文件，不碰
# /data/chill、/etc/config、/etc/rc.local；clean 会整个删掉。
SB=/data/local/tmp/chill-spike
DEADMAN=300                          # 死人开关秒数
MIHOMO_VER="${MIHOMO_VER:-v1.19.31}"
# 校验的是下载下来的 .gz（不是解压后的二进制）。v1.19.31 实测值，2026-09-16 核对。
# 解压后二进制 sha256 = 1b315bc038d05f84ee86d232f3c3d2b020b5044e9b971bb8fe215b6e6a2148f3
MIHOMO_SHA256="${MIHOMO_SHA256:-9e0f11afbf38426b8bd88fdc594678f8161c57eccb4e1b77acb12b493904f1d4}"
MRS_BASE=https://github.com/MetaCubeX/meta-rules-dat/raw/meta/geo
CACHE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.cache"

dev() { ssh -o BatchMode=yes -o ConnectTimeout=8 "$HOST" "$@"; }
ok()  { printf '  ✓ %s\n' "$*"; }
bad() { printf '  ✗ %s\n' "$*"; FAILED=$((FAILED+1)); }
sec() { printf '\n=== %s\n' "$*"; }
FAILED=0

# 手动阶段每轮最长等待。必须明显小于 DEADMAN，否则等人的时候心跳就过期了。
HOLD_TIMEOUT=120

# 不占用本地 stdin 的 dev。手动阶段要用 read 读键盘，普通 dev() 走的 ssh
# 会把用户敲的字吞掉，所以那里一律用 devn。
devn() { ssh -o BatchMode=yes -o ConnectTimeout=8 -n "$HOST" "$@"; }

# 手动检查阶段的驻留循环。之前这里没有任何等待：打印完 M1–M6 就直接往下
# 跑到 clean，手动项根本来不及做。现在每次回车续一次心跳，q 结束。
hold_for_manual() {
  local ans=""
  echo
  echo "  死人开关：${DEADMAN}s 内没有续期就自动清理（host 挂了也能收尾）。"
  while :; do
    devn "date +%s > $SB/heartbeat" >/dev/null 2>&1 || true
    printf '  [回车] 续期并继续　[q] 结束并清理 > '
    if ! read -r -t "$HOLD_TIMEOUT" ans; then
      echo; echo "  ${HOLD_TIMEOUT}s 无输入，结束手动阶段。"; return
    fi
    [ "$ans" = q ] && return
  done
}

# ── prepare ──────────────────────────────────────────────────────────────────
prepare() {
  mkdir -p "$CACHE"
  local gz="$CACHE/mihomo-$MIHOMO_VER.gz" bin="$CACHE/mihomo-$MIHOMO_VER"
  if [ ! -f "$bin" ]; then
    echo "[*] 下载 mihomo $MIHOMO_VER"
    curl -fsSL -o "$gz" \
      "https://github.com/MetaCubeX/mihomo/releases/download/$MIHOMO_VER/mihomo-linux-arm64-$MIHOMO_VER.gz"
    got="$(shasum -a 256 "$gz" | awk '{print $1}')"
    if [ -n "${MIHOMO_SHA256:-}" ] && [ "$got" != "$MIHOMO_SHA256" ]; then
      echo "[-] sha256 不符：期望 $MIHOMO_SHA256 实际 $got" >&2; exit 1
    fi
    [ -n "${MIHOMO_SHA256:-}" ] || echo "[!] 未设 MIHOMO_SHA256，本次实际值：${got}（填进脚本后重跑以启用校验）"
    gunzip -c "$gz" > "$bin"; chmod +x "$bin"
  fi

  echo "[*] 下载规则集（只取 spike 需要的三个）"
  for n in geosite/cn geoip/cn geosite/private; do
    out="$CACHE/$(echo "$n" | tr / _).mrs"
    [ -f "$out" ] || curl -fsSL -o "$out" "$MRS_BASE/$n.mrs"
  done

  # CN CIDR 文本列表：2026-09-16 实测确认 auto-redirect 在本机用不了（iptables-legacy
  # 占住 nat hook，nft 建链回 EEXIST），连带 route-exclude-address-set 也失效。
  # 唯一可行的内核级国内绕行是把这 9646 条静态写进 route-exclude-address。
  CN_LIST="$CACHE/geoip_cn_cn.list"
  [ -f "$CN_LIST" ] || curl -fsSL -o "$CN_LIST" "$MRS_BASE/geoip/cn.list"
  echo "[*] CN CIDR 列表 $(wc -l < "$CN_LIST" | tr -d ' ') 条"

  # 变量一律加花括号：bash 在中文 locale 下会把紧跟其后的全角字符（这里是「（」）
  # 并进变量名，配上 set -u 就报 SB（ 未绑定。$SB（…… 这种写法必须写成 ${SB}（……
  echo "[*] 推送到 ${HOST}:${SB}（/data 上的可写分区，不占内存盘；clean 会整个删掉）"
  dev "mkdir -p $SB/ruleset"
  dev "cat > $SB/mihomo" < "$bin"
  dev "chmod +x $SB/mihomo"
  for n in geosite_cn geoip_cn geosite_private; do
    dev "cat > $SB/ruleset/$n.mrs" < "$CACHE/$n.mrs"
  done
  dev "ls -l $SB $SB/ruleset | head -20; $SB/mihomo -v | head -1"
  echo "[+] prepare 完成。配置由 run 现场生成（含订阅地址，不落盘到仓库）。"
}

# ── 设备端清理（幂等，任何退出路径都要能跑）────────────────────────────────
CLEAN_SNIPPET='
  kill $(cat '"$SB"'/core.pid 2>/dev/null) 2>/dev/null || true
  sleep 1; kill -9 $(cat '"$SB"'/core.pid 2>/dev/null) 2>/dev/null || true
  nft delete table inet mihomo 2>/dev/null || true
  nft delete table inet chill 2>/dev/null || true
  for f in 4 6; do
    [ $f = 4 ] && C=ip || C="ip -6"
    while $C rule show 2>/dev/null | grep -qE "^90[0-9][0-9]:"; do
      P=$($C rule show | grep -oE "^90[0-9][0-9]" | head -1); $C rule del pref $P 2>/dev/null || break
    done
    $C route flush table 2022 2>/dev/null || true
  done
  ip route del unreachable 198.18.0.0/16 2>/dev/null || true
  iptables -D FORWARD -i br-lan -o chill0 -j ACCEPT 2>/dev/null || true
  iptables -D FORWARD -i chill0 -o br-lan -j ACCEPT 2>/dev/null || true
'

# 先撤掉死人开关再清理。否则正常收尾之后守护循环还在设备上空转，
# 直到 TTL 到期又跑一次 clean.sh 才退出。
# 注意只在 host 侧撤：watcher 自己触发时跑的是 clean.sh，跑完自己 exit，
# 不能让 clean.sh 把自己的父进程杀掉，否则后半段清理就断了。
clean() {
  echo "[*] 清理设备端"
  dev "kill \$(cat $SB/deadman.pid 2>/dev/null) 2>/dev/null || true; rm -f $SB/deadman.pid"
  dev "$CLEAN_SNIPPET"
  dev "rm -rf $SB/run" 2>/dev/null || true
  echo "[+] 已清理"
}

# ── run ──────────────────────────────────────────────────────────────────────
run() {
  trap 'echo; echo "[!] 中断，执行清理"; clean' INT TERM EXIT

  sec "0. 前置检查"
  dev "[ -x $SB/mihomo ]" && ok "mihomo 已就位" || { bad "先跑 prepare"; exit 1; }
  dev "ip rule show | grep -qE '^90[0-9][0-9]:'" && bad "已存在 9000 段 ip rule，先 clean" || ok "9000 段 ip rule 干净"
  BEFORE_RULES=$(dev "ip rule show; echo ---; ip -6 rule show")
  BEFORE_MARTIAN=$(dev "awk 'NR==2{print \$8}' /proc/net/stat/rt_cache")
  ok "已记录 ip rule 快照与 in_martian_src=$BEFORE_MARTIAN"

  sec "1. 生成最小配置（订阅地址从 chill.env 读，不写进仓库）"
  # shellcheck disable=SC2016
  # 配置先在本机生成再推送：要往 route-exclude-address 注入 9646 条 CN CIDR
  # （单行约 163KB）。在设备上用 sed/awk 拼这个会踩引号嵌套的坑，已经因此
  # 白跑过一次实验（生成出来的行只有 149 字节，等于什么都没测）。
  CN_LIST="$CACHE/geoip_cn_cn.list"
  [ -f "$CN_LIST" ] || { bad "缺 $CN_LIST，先跑 prepare"; exit 1; }
  TMPL="$(mktemp -t chill-tmpl)"
  cat > "$TMPL" <<'YAML'
mode: rule
ipv6: false
log-level: warning
allow-lan: false
find-process-mode: 'off'
geodata-mode: false
geo-auto-update: false
external-controller: 127.0.0.1:9999
secret: ''
tun:
  enable: true
  stack: system
  device: chill0
  auto-route: true
  # 必须是 false：设备是 iptables v1.8.8 (legacy)，legacy 已占住 raw/mangle/nat/filter
  # 的 hook，mihomo 的 auto-redirect 用 nftables 建链时内核直接回 EEXIST
  # （Start TUN listening error: ... netlink receive: file exists），TUN 整个起不来。
  # 2026-09-16 实测：关掉它之后 chill0 正常 UP、9000 段 ip rule 全部建立。
  auto-redirect: false
  auto-detect-interface: true
  strict-route: false
  include-interface: [br-lan]
  iproute2-table-index: 2022
  iproute2-rule-index: 9000
  route-exclude-address: [0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16, 224.0.0.0/4, 240.0.0.0/4]
  # route-exclude-address-set: [cn_ip]   ← 停用。它依赖 auto-redirect 建立的 nft 打 mark，
  # auto-redirect 关掉后完全不生效（实测国内 IP 和被墙 IP 一样全进 TUN）。
  # 国内绕行改由上面 route-exclude-address 的静态 CN CIDR 全量列表承担。
  # 注意 cn_ip 这个 rule-provider 在规则层仍要保留（RULE-SET,cn_ip,DIRECT）。
  dns-hijack: ["any:53"]
dns:
  enable: true
  # 独立监听，不能只靠 TUN 的 dns-hijack。2026-09-16 实测：客户端的 DNS 实际走
  # IPv6 link-local 发给 dnsmasq（45 秒抓到 375 个 DNS 包，全是 v6），而 dns-hijack
  # 只作用于 TUN 的 v4，结果 DNS 完全绕过 mihomo，fake-ip 对真实客户端不生效。
  # 本 spike 只开这个监听（零风险，仅多占一个本机端口），不自动改 dnsmasq——
  # 那要动全屋 DNS 并需要备份回滚，得先问用户。手工验证用：
  #   uci set dhcp.lan_dns.noresolv=1
  #   uci add_list dhcp.lan_dns.server=127.0.0.1
  #   uci commit dhcp && /etc/init.d/dnsmasq reload
  # 注意段名是 lan_dns 不是 @dnsmasq[0]；回滚务必连同 mihomo 一起做，否则
  # dnsmasq 会指着一个已死的上游，全屋断网而不是直连。
  listen: 127.0.0.1:53
  ipv6: false
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-filter: ["rule-set:cn_domain", "rule-set:private_domain", "+.lan", "+.local", "+.ts.net"]
  default-nameserver: [223.5.5.5]
  nameserver: [223.5.5.5, 119.29.29.29]
  nameserver-policy:
    "+.lan": [10.0.66.1]
    # 裸主机名（Johns-iPhone 这类）在这里写不了，已于 2026-09-16 阶段零实测确认：
    # mihomo 把 nameserver-policy 的键当域名模式解析，"^[^.]+$" 直接报
    # invalid domain（"+" wildcard must occupy the entire label），整份配置校验
    # 不通过（mihomo -t 退出码 1）。它的通配也表达不了「不含点的单标签」。
    # 按决定 O14 交给 dnsmasq。因此只有 mihomo 劫持 DNS 时，M6 预期失败。
rule-providers:
  cn_domain: {type: file, behavior: domain, format: mrs, path: ./ruleset/geosite_cn.mrs}
  cn_ip:     {type: file, behavior: ipcidr, format: mrs, path: ./ruleset/geoip_cn.mrs}
  private_domain: {type: file, behavior: domain, format: mrs, path: ./ruleset/geosite_private.mrs}
proxies: []
proxy-groups:
  - {name: 🚀 节点选择, type: select, proxies: [DIRECT]}
rules:
  - RULE-SET,private_domain,DIRECT
  - RULE-SET,cn_domain,DIRECT
  - MATCH,🚀 节点选择
YAML
  CFG="$(mktemp -t chill-cfg)"
  python3 - "$CN_LIST" "$TMPL" "$CFG" <<'PY'
import sys
cn = [l.strip() for l in open(sys.argv[1]) if l.strip()]
out = []
for line in open(sys.argv[2]):
    if line.startswith('  route-exclude-address: ['):
        out.append('  route-exclude-address: [' + ', '.join(cn) + ', ' + line.split('[', 1)[1])
    else:
        out.append(line)
open(sys.argv[3], 'w').writelines(out)
print("  已注入 %d 条 CN CIDR，配置共 %d 字节" % (len(cn), sum(len(x) for x in out)))
PY
  dev "cat > $SB/config.yaml" < "$CFG"
  rm -f "$TMPL" "$CFG"
  dev "cd $SB && ./mihomo -t -d $SB -f $SB/config.yaml" >/dev/null 2>&1 \
    && ok "配置校验通过（-f 绝对路径）" || bad "mihomo -t 失败"
  dev "cd / && $SB/mihomo -t -d $SB -f $SB/config.yaml" >/dev/null 2>&1 \
    && ok "从 / 目录校验同样通过" || bad "从 / 校验失败（-f 相对路径坑）"

  sec "2. 临时 FORWARD 放行 + 死人开关（${DEADMAN}s）"
  dev "iptables -I FORWARD 1 -i br-lan -o chill0 -j ACCEPT; iptables -I FORWARD 1 -i chill0 -o br-lan -j ACCEPT"
  ok "已插入两条临时规则"
  # clean.sh 用 herestring 原样落盘：CLEAN_SNIPPET 里有 $(cat ...) 之类的构造，
  # 走未加引号的 heredoc 会被 Mac 上的 shell 先展开一遍，等于在本机执行清理。
  dev "cat > $SB/clean.sh" <<< "$CLEAN_SNIPPET"
  # 心跳式而不是固定 sleep：手动检查阶段能反复续期，同时 host 断线或脚本被杀
  # 时照样会自动收尾。setsid/timeout 这台设备都没有，只能用 nohup。
  dev "cat > $SB/deadman.sh" <<'EOF'
#!/bin/sh
TTL="$1"; SB="$2"
while :; do
  now=$(date +%s)
  hb=$(cat "$SB/heartbeat" 2>/dev/null || echo 0)
  if [ "$((now - hb))" -ge "$TTL" ]; then
    sh "$SB/clean.sh" >/dev/null 2>&1
    exit 0
  fi
  sleep 5
done
EOF
  dev "date +%s > $SB/heartbeat"
  dev "nohup sh $SB/deadman.sh $DEADMAN $SB >/dev/null 2>&1 </dev/null & echo \$! > $SB/deadman.pid"
  ok "死人开关已挂：${DEADMAN}s 无续期即清理（已实测能在 ssh 断开后触发）"

  sec "3. 启动核心"
  # </dev/null 不能省：stdin 还挂在 ssh 通道上时，这条 dev 调用会卡住不返回。
  dev "cd $SB && GOMEMLIMIT=160MiB nohup ./mihomo -d $SB -f $SB/config.yaml > $SB/core.log 2>&1 </dev/null & echo \$! > $SB/core.pid"
  sleep 6
  dev "kill -0 \$(cat $SB/core.pid)" 2>/dev/null && ok "进程存活" || { bad "核心没起来"; dev "tail -20 $SB/core.log"; return; }
  # 进程存活 ≠ TUN 建立成功。2026-09-16 实测：auto-redirect 失败时进程照样在跑、
  # API 照样返回 version，但什么流量都没接管，后面几项"通过"全是假象。必须单独验。
  if dev "grep -q 'Start TUN listening error' $SB/core.log"; then
    bad "TUN 没起来（后面各项通过与否都不作数），原因："
    dev "grep -iE 'TUN|redirect|error' $SB/core.log | tail -10"
  else
    ok "core.log 里没有 TUN 启动错误"
  fi

  sec "4. 逐项验证"
  # 判据已按 2026-09-16 实测结果改写。原来这里查的是「有没有 inet mihomo 表」和
  # 「nft 排除集合里有没有 198.18」，两条都过时了：auto-redirect 停用后根本不建 nft 表，
  # 于是第二条 grep 一个不存在的表必然「通过」——假性通过比失败更危险。
  if dev "nft list tables 2>/dev/null | grep -q 'inet mihomo'"; then
    bad "出现了 inet mihomo 表：auto-redirect 被改回 true 了？本机上它会让 TUN 整个起不来"
  else
    ok "无 inet mihomo 表（auto-redirect=false 的预期结果）"
  fi

  # 静态 CN CIDR 是否真的注入进了路由表（满配实测约 12731 条）
  R2022="$(dev "ip route show table 2022 2>/dev/null | wc -l" | tr -d ' ')"
  [ "${R2022:-0}" -gt 10000 ] \
    && ok "table 2022 已注入 $R2022 条路由" \
    || bad "table 2022 只有 ${R2022:-0} 条，CN CIDR 没注入成功（国内流量会全进 mihomo）"

  # 真正要证明的是分流行为本身。用内核路由查询，不需要客户端配合，
  # 所以这几条在没有手机的情况下也能跑。10.0.66.23 是 LAN 网段内的样本地址。
  if dev "ip route get 114.114.114.114 from 10.0.66.23 iif br-lan 2>/dev/null | head -1" | grep -q chill0; then
    bad "国内 IP 仍走 chill0，内核级绕行没生效"
  else
    ok "国内 IP 绕过 TUN，走 WAN 直连"
  fi
  if dev "ip route get 1.1.1.1 from 10.0.66.23 iif br-lan 2>/dev/null | head -1" | grep -q chill0; then
    ok "被墙 IP 走 chill0"
  else
    bad "被墙 IP 没进 TUN，代理不会生效"
  fi
  dev "ip rule show | grep -E '^90[0-9][0-9]:'" | head -5 || true
  dev "ip rule show | grep -qE '^90[0-9][0-9]:'" && ok "9000 段 ip rule 已建立" || bad "没有 9000 段 ip rule"
  dev "curl -s -m 8 --noproxy '*' -o /dev/null -w '%{http_code}' http://www.gstatic.com/generate_204" \
    | grep -q 204 && ok "路由器自身出网正常（应为直连）" || bad "路由器自身出网异常"
  AFTER_MARTIAN=$(dev "awk 'NR==2{print \$8}' /proc/net/stat/rt_cache")
  [ "$AFTER_MARTIAN" = "$BEFORE_MARTIAN" ] && ok "in_martian_src 未增长（${AFTER_MARTIAN}）" || bad "in_martian_src 增长：$BEFORE_MARTIAN → $AFTER_MARTIAN"
  dev "$SB/mihomo -v >/dev/null; wget -q -O- -T 3 http://127.0.0.1:9999/version" | head -c 80; echo
  # 这里原来只跑了个 ssh_ok=1 的空赋值就宣称通过，什么都没验。
  # 真正要证明的是 TUN 起来之后 tailscale0 没被抢掉。
  if dev "ip -4 addr show tailscale0 2>/dev/null | grep -q 'inet '"; then
    ok "tailscale0 仍有地址，tailnet 未被 TUN 抢走"
  else bad "tailscale0 没有地址或已消失（TUN 与 Tailscale 冲突）"; fi

  cat <<'MANUAL'

  —— 以下需要你配合（手机连 U60 Wi-Fi 并关掉 Surge）——
  M1 局域网 DNS：手机能正常解析（验证 DNAT 走 FORWARD）
  M2 局域网 TCP/UDP 都能通
  M3 删掉两条临时 FORWARD 规则后，UDP 和 DNS 立刻失败（反证正式 chill 区必需）
  M4 持续 5 分钟下载，观察 /connections 与 chill0 接口收发字节是否持续增长
     （原来写的是「nft 计数器」，但 auto-redirect 停用后根本没有 nft 表，那条没法看了。
      用 ip -s link show chill0 代替。这项仍能验证高通 IPA 加速有没有绕过转发路径）
  M5 v6 客户端访问 AI 站点，确认命中的是 AI 组而不是漏网之鱼
  M6 裸主机名 ping 一台局域网设备能解析
     ⚠ 已知会出错，但**出错方式不是解析失败，而是解析成错误的 fake-ip**（2026-09-16 实测修正）。
       实测裸主机名 Mac 经 mihomo 得到 198.18.0.5，正确答案是 10.0.66.109（dnsmasq 用 DHCP
       租约能答对）。所以现象是「能解析、但连不上那台局域网设备」，属静默错误。
       起因：nameserver-policy 表达不了「不含点的单标签」，写 "^[^.]+$" 会让整份配置校验不过。
       另注：配置里的 "+.lan" 同样无效——Mac.lan 在 mihomo 与 dnsmasq 下均为 NXDOMAIN。
       两者都由 O14 的 dnsmasq 前置方案一并解决，本 spike 没有搭。别当成回归。
  每做完一项回车续期（我来读数据），全部做完按 q 结束并清理。

MANUAL

  hold_for_manual

  sec "5. 结果"
  [ "$FAILED" -eq 0 ] && echo "  全部自动项通过" || echo "  失败 $FAILED 项"
  AFTER_RULES=$(dev "ip rule show; echo ---; ip -6 rule show")
  clean
  trap - INT TERM EXIT
  FINAL_RULES=$(dev "ip rule show; echo ---; ip -6 rule show")
  if [ "$BEFORE_RULES" = "$FINAL_RULES" ]; then ok "清理后 ip rule 与启动前完全一致"
  else bad "清理后 ip rule 有残留："; diff <(echo "$BEFORE_RULES") <(echo "$FINAL_RULES") || true; fi
}

case "${1:-}" in
  prepare) prepare ;;
  run)     run ;;
  clean)   clean ;;
  *) echo "usage: $0 {prepare|run|clean}"; exit 1 ;;
esac
