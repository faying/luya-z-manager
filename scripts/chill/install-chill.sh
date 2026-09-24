#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# CHILL 安装脚本（T7）。在 **Mac 上** 运行，经 SSH 部署到 U60 Pro。
#
#   ./install-chill.sh              # 干跑：只打印会做什么，不碰设备（默认）
#   ./install-chill.sh --apply      # 真正执行
#   HOST=u60 ./install-chill.sh --apply
#
# **默认干跑是刻意的**：这个脚本会改 /etc/rc.local 和防火墙。rc.local 改坏了
# 设备开不了机，而这台设备没有公开的救砖工具。先干跑、看清楚、再 --apply。
#
# 会做的事：
#   Mac 端  fetch-assets.sh 下载校验 mihomo、.mrs、CN CIDR 列表、zashboard；
#           stage.sh 摆成 /data/chill 的样子（装机包 install.sh chill 用的是同一套）
#   推送    bin/mihomo、ruleset/*、chill.sh、chill.init、template.yaml、ui/
#   设备端  备份 dhcp / firewall / rc.local 到 /data/u60-kit 与 /data/chill
#           建 uci 防火墙区 chill（chill0 放行、与 lan 互转）
#           装 /etc/init.d/chill（**不 enable**）
#           rc.local 加一行自启，并 sh -n 校验
#
# 不会做的事（要你自己来）：
#   - 不写 chill.env（含订阅地址，永不进仓库、不由脚本代填）
#   - 不启动核心。装完用 `chill.sh safe-start` 起，5 分钟内 `chill.sh confirm`
#     确认；不 confirm 会自动停掉，避免把自己锁在外面。
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HOST="${HOST:-u60}"
APPLY=0
[ "${1:-}" = "--apply" ] && APPLY=1

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE="${CHILL_CACHE:-$HERE/test/.cache}"
MIHOMO_VER="${MIHOMO_VER:-v1.19.31}"
ZASHBOARD_VER="${ZASHBOARD_VER:-v3.27.0}"

say()  { printf '[*] %s\n' "$*"; }
good() { printf '[+] %s\n' "$*"; }
warn() { printf '[!] %s\n' "$*"; }
die()  { printf '[-] %s\n' "$*" >&2; exit 1; }
would() { printf '    (干跑) %s\n' "$*"; }

run_local() { if [ "$APPLY" = 1 ]; then eval "$@"; else would "$*"; fi; }

# ── 0. 前置检查 ──────────────────────────────────────────────────────────────
say "前置检查"
for c in curl shasum ssh awk sed unzip; do
  command -v "$c" >/dev/null 2>&1 || die "缺少命令：$c"
done
[ -f "$HERE/chill.sh" ]     || die "缺 chill.sh"
[ -f "$HERE/chill.init" ]   || die "缺 chill.init"
[ -f "$HERE/template.yaml" ] || die "缺 template.yaml"
sh -n "$HERE/chill.sh"      || die "chill.sh 语法不过，先修"
sh -n "$HERE/chill.init"    || die "chill.init 语法不过，先修"

ssh -o BatchMode=yes -o ConnectTimeout=8 "$HOST" true 2>/dev/null \
  || die "SSH 连不上 ${HOST}（需要免密登录）"
good "本地文件齐备，SSH 可达 $HOST"

if [ "$APPLY" != 1 ]; then
  echo
  warn "当前是干跑模式，不会对设备做任何改动。确认无误后加 --apply 执行。"
  echo
fi

# ── 1. Mac 端准备 ───────────────────────────────────────────────────────────
# 下载校验（fetch-assets.sh）和摆成 /data/chill 的样子（stage.sh）与装机包共用。
STAGE="$CACHE/stage"
if [ "$APPLY" = 1 ]; then
  "$HERE/stage.sh" "$CACHE" "$STAGE"
else
  would "下载并校验 mihomo ${MIHOMO_VER}、template.yaml 里的规则集 + cn.list、zashboard ${ZASHBOARD_VER}（fetch-assets.sh）"
  would "解压并注入 zashboard 的 :9999 自动配置（stage.sh）"
fi

# ── 2. 推送 ─────────────────────────────────────────────────────────────────
# 设备上没有 scp，用 tar 管道。chill.env、providers/、cache 不在包里，不会被碰。
say "推送到设备"
if [ "$APPLY" = 1 ]; then
  ssh -o BatchMode=yes "$HOST" 'mkdir -p /data/chill/run /data/chill/providers /data/u60-kit'
  COPYFILE_DISABLE=1 tar czf - -C "$STAGE" bin ruleset chill.sh chill.init template.yaml chill.env.example \
    | ssh -o BatchMode=yes "$HOST" 'cd /data/chill && tar xzf -' || die "推送 CHILL 文件失败"
  # 2026-09-16 真机踩过的坑：/data/chill/ui 里混进过来源不明的文件，干净覆盖比增量合并可靠。
  COPYFILE_DISABLE=1 tar czf - -C "$STAGE/ui" . \
    | ssh -o BatchMode=yes "$HOST" 'rm -rf /data/chill/ui.new && mkdir -p /data/chill/ui.new && cd /data/chill/ui.new && tar xzf - && rm -rf /data/chill/ui && mv /data/chill/ui.new /data/chill/ui' \
    || die "推送 zashboard 失败"
  good "mihomo、规则集、脚本、zashboard 已推送到 /data/chill"
else
  would "推送 bin/mihomo、ruleset/*、chill.sh、chill.init、template.yaml -> /data/chill"
  would "整个替换 /data/chill/ui（zashboard，干净覆盖，不做增量合并）"
fi

# ── 3. 设备端配置 ───────────────────────────────────────────────────────────
say "设备端：备份、防火墙区、init.d、rc.local"
if [ "$APPLY" != 1 ]; then
  would "备份 /etc/config/dhcp -> /data/chill/dhcp.backup（restore_dns 用，决定 3）"
  would "备份 /etc/config/firewall -> /data/u60-kit/firewall.orig（仅首次）"
  would "备份 /etc/rc.local -> /data/u60-kit/rc.local.before-chill"
  would "uci 建防火墙区 chill：device=chill0，input/output/forward=ACCEPT，masq=0，lan<->chill 互转"
  would "安装 /etc/init.d/chill（chmod +x，**不 enable**）"
  would "rc.local 插入一行：[ -f /data/chill/disabled ] || /etc/init.d/chill start"
  would "sh -n /etc/rc.local 校验"
else
  ssh -o BatchMode=yes "$HOST" 'sh -s' <<'REMOTE'
set -e
chmod +x /data/chill/chill.sh /data/chill/bin/mihomo

# restore_dns 要用的备份（决定 3）。放 /data，/tmp 是内存盘会丢。
# 只在首次建立：之后 /etc/config/dhcp 可能已被 CHILL 改过，再备份就会把
# "指向 mihomo" 的状态当成原始状态，回滚就永远回不到真正的原样了。
[ -f /data/chill/dhcp.backup ] || cp /etc/config/dhcp /data/chill/dhcp.backup

mkdir -p /data/u60-kit
[ -f /data/u60-kit/firewall.orig ] || cp /etc/config/firewall /data/u60-kit/firewall.orig
cp /etc/rc.local /data/u60-kit/rc.local.before-chill

# 防火墙区：UDP 从 br-lan 进 chill0 要过 FORWARD（实测默认 DROP，不放行全屋断网）。
# 幂等：已存在就不重复建。uci 配置扛得住 fw3 reload。
if ! uci show firewall | grep -q "name='chill'"; then
  Z=$(uci add firewall zone)
  uci set firewall.$Z.name=chill
  uci add_list firewall.$Z.device=chill0
  uci set firewall.$Z.input=ACCEPT
  uci set firewall.$Z.output=ACCEPT
  uci set firewall.$Z.forward=ACCEPT
  uci set firewall.$Z.masq=0
  F=$(uci add firewall forwarding); uci set firewall.$F.src=chill; uci set firewall.$F.dest=lan
  F=$(uci add firewall forwarding); uci set firewall.$F.src=lan;   uci set firewall.$F.dest=chill
  uci commit firewall
  /etc/init.d/firewall reload >/dev/null 2>&1 || true
  echo "  已建立防火墙区 chill"
else
  echo "  防火墙区 chill 已存在，跳过"
fi

# init.d：**不要 enable**。自启只走 rc.local；enable/disable 会在
# /zteoverlay/etc-upper_a/rc.d/ 留下 whiteout 字符设备，很难排查。
cp /data/chill/chill.init /etc/init.d/chill
chmod +x /etc/init.d/chill

# rc.local 幂等加一行，插在 exit 0 之前
grep -q '/etc/init.d/chill start' /etc/rc.local \
  || sed -i '/^exit 0/i [ -f /data/chill/disabled ] || /etc/init.d/chill start' /etc/rc.local
sh -n /etc/rc.local
echo "  rc.local 已更新且语法通过"

echo "  设备端完成"
REMOTE
fi

# ── 4. 收尾提示 ─────────────────────────────────────────────────────────────
echo
if [ "$APPLY" != 1 ]; then
  good "干跑结束。以上都没有执行。确认无误后：$0 --apply"
  exit 0
fi

good "安装完成。接下来要你手动做两件事："
echo
echo "  1. 写订阅地址（脚本不代填，订阅 URL 永不进仓库）："
echo "       ssh $HOST 'cat > /data/chill/chill.env' <<'EOF'"
echo "       SUB_OIX='…'"
echo "       SUB_SHOUHOU='…'"
echo "       SUB_NEXI='…'"
echo "       # 可选：CHILL_BYPASS_IP='10.0.66.23 10.0.66.31'（按源 IP 绕行，设备需固定 IP）"
echo "       # 可选：CHILL_API_LAN=1 与 CHILL_API_ALLOW_IP='…'"
echo "       # 可选：CHILL_SECRET='…'（给 :9999 加 token；开之前先给触屏写"
echo "       #   /data/plugins/u60pro-devui/chill.conf 的 secret= 并重启 devui，"
echo "       #   再给 zte-agent 一样带上 Bearer token，最后才 reload 打开这个值——"
echo "       #   顺序反了会让触屏/后台在换上新配置前先收到 401）"
echo "       EOF"
echo "       ssh $HOST 'chmod 600 /data/chill/chill.env'"
echo
echo "  2. 首次启动用 safe-start，它挂 5 分钟死人开关："
echo "       ssh $HOST '/data/chill/chill.sh safe-start'"
echo "       # 网络正常就在 5 分钟内确认，否则会自动停掉："
echo "       ssh $HOST '/data/chill/chill.sh confirm'"
echo
good "面板：后台「服务 → CHILL」的按钮，经 http://<设备>:9090/chill-ui/ 打开（zte-agent 反代，"
good "带登录后拿到的密钥，:9999 可以只留在本机：CHILL_API_LAN=0）。"
warn "关于 CHILL_API_LAN=1 的风险：开了之后局域网内能访问 :9999 的设备，"
warn "可以切节点、关代理、并从 /connections 看到全屋实时访问的域名。"
warn "不设 CHILL_SECRET 就只能靠来源白名单挡，所以把自己的设备绑静态租约"
warn "或走 tailnet，别让客人/电视/Switch 够得着。设了 CHILL_SECRET 是在白名单"
warn "之外再加一层，不是替代它——这里没有改成默认关闭 CHILL_API_LAN。"
echo
echo "回滚：ssh $HOST 'cp /data/u60-kit/rc.local.before-chill /etc/rc.local && /data/chill/chill.sh stop'"
