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
#   Mac 端  下载 mihomo（校验 sha256）与 16 个 .mrs、CN CIDR 列表
#   推送    bin/mihomo、ruleset/*、chill.sh、chill.init、template.yaml
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
CACHE="$HERE/test/.cache"
TEMPLATE="$HERE/template.yaml"

MIHOMO_VER="${MIHOMO_VER:-v1.19.31}"
# 校验的是下载下来的 .gz（不是解压后的二进制）。2026-09-16 实测值，与 spike.sh 同源。
MIHOMO_SHA256="${MIHOMO_SHA256:-9e0f11afbf38426b8bd88fdc594678f8161c57eccb4e1b77acb12b493904f1d4}"
MRS_BASE="https://github.com/MetaCubeX/meta-rules-dat/raw/meta/geo"
# 2026-09-17 真机验证过的版本与下载地址（Zephyruso/zashboard 官方发布）。
# 默认自动下载，不需要预先手动准备文件。
ZASHBOARD_VER="${ZASHBOARD_VER:-v3.27.0}"
# 留空则按上面的版本自动下载官方 dist.zip；也可指向本地已有的 .zip 文件（离线
# 安装或试用未发布的构建）。⚠ 官方包是 **.zip**，不是 .tgz——这个变量原名
# ZASHBOARD_TGZ 且假设 tar 格式，从一开始就没对过，2026-09-17 手动部署时才
# 发现，已改名并修正处理逻辑，不存在需要兼容的旧行为。
ZASHBOARD_ZIP="${ZASHBOARD_ZIP:-}"

say()  { printf '[*] %s\n' "$*"; }
good() { printf '[+] %s\n' "$*"; }
warn() { printf '[!] %s\n' "$*"; }
die()  { printf '[-] %s\n' "$*" >&2; exit 1; }
would() { printf '    (干跑) %s\n' "$*"; }

run_local() { if [ "$APPLY" = 1 ]; then eval "$@"; else would "$*"; fi; }

# ── 0. 前置检查 ──────────────────────────────────────────────────────────────
say "前置检查"
for c in curl shasum ssh awk sed; do
  command -v "$c" >/dev/null 2>&1 || die "缺少命令：$c"
done
[ -f "$HERE/chill.sh" ]     || die "缺 chill.sh"
[ -f "$HERE/chill.init" ]   || die "缺 chill.init"
[ -f "$TEMPLATE" ]          || die "缺 template.yaml"
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
mkdir -p "$CACHE/ruleset"

BIN="$CACHE/mihomo-$MIHOMO_VER"
if [ ! -f "$BIN" ]; then
  say "下载 mihomo $MIHOMO_VER"
  if [ "$APPLY" = 1 ]; then
    gz="$CACHE/mihomo-$MIHOMO_VER.gz"
    curl -fsSL -o "$gz" \
      "https://github.com/MetaCubeX/mihomo/releases/download/$MIHOMO_VER/mihomo-linux-arm64-$MIHOMO_VER.gz"
    got="$(shasum -a 256 "$gz" | awk '{print $1}')"
    [ "$got" = "$MIHOMO_SHA256" ] || die "sha256 不符：期望 $MIHOMO_SHA256 实际 $got"
    gunzip -c "$gz" > "$BIN"; chmod +x "$BIN"
    good "mihomo 已下载并通过 sha256 校验"
  else
    would "下载 mihomo 并校验 sha256=$MIHOMO_SHA256"
  fi
else
  good "mihomo 已在缓存中（${BIN}）"
fi

# 规则集清单**从 template.yaml 解析**，不另立一份。
# 两份清单迟早会不同步，而不同步的后果是规则集缺失 —— chill.sh 会直接拒绝启动。
say "从 template.yaml 解析规则集清单"
MRS_LIST="$(awk '
  /^ *[a-z_]+: *\{type: http/ {
    name = $1; sub(/:$/, "", name)
    if (match($0, /geo\/[a-z]+\/[^.]+\.mrs/)) {
      print name "=" substr($0, RSTART + 4, RLENGTH - 4)
    }
  }' "$TEMPLATE")"
n_mrs=$(printf '%s\n' "$MRS_LIST" | grep -c . || true)
[ "$n_mrs" -gt 0 ] || die "没能从 template.yaml 解析出规则集，检查解析逻辑"
good "解析到 $n_mrs 个规则集"

for item in $MRS_LIST; do
  name="${item%%=*}"; path="${item#*=}"
  out="$CACHE/ruleset/${name}.mrs"
  if [ -f "$out" ]; then continue; fi
  if [ "$APPLY" = 1 ]; then
    curl -fsSL -o "$out" "$MRS_BASE/${path}" || die "下载 $path 失败"
  else
    would "下载 $MRS_BASE/${path} -> ruleset/${name}.mrs"
  fi
done
[ "$APPLY" = 1 ] && good "规则集就绪"

CN_LIST="$CACHE/geoip_cn_cn.list"
if [ ! -f "$CN_LIST" ]; then
  if [ "$APPLY" = 1 ]; then
    curl -fsSL -o "$CN_LIST" "$MRS_BASE/geoip/cn.list" || die "下载 cn.list 失败"
  else
    would "下载 CN CIDR 列表（route-exclude-address 用，约 9646 条）"
  fi
fi

# ── 2. 推送 ─────────────────────────────────────────────────────────────────
# 设备上没有 scp，用 `ssh 'cat > 路径' < 文件`。
push() { # push <本地文件> <设备路径>
  if [ "$APPLY" = 1 ]; then
    ssh -o BatchMode=yes "$HOST" "cat > '$2'" < "$1"
  else
    would "推送 $(basename "$1") -> $2"
  fi
}

say "推送到设备"
if [ "$APPLY" = 1 ]; then
  ssh -o BatchMode=yes "$HOST" 'mkdir -p /data/chill/bin /data/chill/ruleset /data/chill/run /data/chill/providers /data/u60-kit'
else
  would "mkdir -p /data/chill/{bin,ruleset,run,providers} /data/u60-kit"
fi

push "$BIN"               /data/chill/bin/mihomo
push "$HERE/chill.sh"     /data/chill/chill.sh
push "$HERE/chill.init"   /data/chill/chill.init
push "$TEMPLATE"          /data/chill/template.yaml
# 不要写成 `[ -f ... ] && push ... || would ...`：set -e 下 push 真的失败时会滑到
# || 分支，把推送失败伪装成一条干跑提示，属于静默失败。
if [ -f "$CN_LIST" ]; then
  push "$CN_LIST" /data/chill/ruleset/cn.list
else
  would "推送 cn.list（当前缓存中没有，--apply 时会先下载）"
fi

for item in $MRS_LIST; do
  name="${item%%=*}"
  [ -f "$CACHE/ruleset/${name}.mrs" ] && push "$CACHE/ruleset/${name}.mrs" "/data/chill/ruleset/${name}.mrs"
done

say "准备 zashboard"
Z="${ZASHBOARD_ZIP:-$CACHE/zashboard-${ZASHBOARD_VER}.zip}"
if [ -n "$ZASHBOARD_ZIP" ]; then
  [ -f "$ZASHBOARD_ZIP" ] || die "ZASHBOARD_ZIP 指向的文件不存在：$ZASHBOARD_ZIP"
elif [ ! -f "$Z" ]; then
  if [ "$APPLY" = 1 ]; then
    curl -fsSL -o "$Z" \
      "https://github.com/Zephyruso/zashboard/releases/download/${ZASHBOARD_VER}/dist.zip" \
      || die "下载 zashboard ${ZASHBOARD_VER} 失败"
    good "zashboard ${ZASHBOARD_VER} 已下载"
  else
    would "下载 zashboard ${ZASHBOARD_VER} 的 dist.zip"
  fi
fi

if [ "$APPLY" = 1 ] && [ -f "$Z" ]; then
  say "推送 zashboard"
  command -v unzip >/dev/null 2>&1 || die "本机缺 unzip：官方包是 .zip，设备上没有 unzip，只能在本机先解开再转格式推送"
  ZTMP="$(mktemp -d)"
  unzip -q "$Z" -d "$ZTMP" || die "解压 zashboard 失败"
  [ -f "$ZTMP/dist/index.html" ] || die "解压结果里没有 dist/index.html，官方包结构可能变了，先手动核实"
  # 2026-09-17 真机踩过的坑：/data/chill/ui 里混进过来源不明的文件（疑似别的
  # 面板遗留，Nuxt.js/GitHub Pages 结构，跟 zashboard 官方包对不上），干净
  # 覆盖比增量合并可靠——每次推送前先整个清空重建。
  ssh -o BatchMode=yes "$HOST" 'rm -rf /data/chill/ui && mkdir -p /data/chill/ui' \
    || die "清空设备端 /data/chill/ui 失败"
  COPYFILE_DISABLE=1 tar czf - -C "$ZTMP/dist" . \
    | ssh -o BatchMode=yes "$HOST" 'cd /data/chill/ui && tar xzf -' \
    || die "推送 zashboard 失败"
  rm -rf "$ZTMP"
  good "zashboard 已推送到 /data/chill/ui"
elif [ "$APPLY" != 1 ]; then
  would "清空 /data/chill/ui 并重新推送 zashboard（干净覆盖，不做增量合并）"
else
  warn "zashboard 包不存在，跳过（:9999/ui 将不可用，不影响代理本身）"
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
warn "关于 CHILL_API_LAN=1 的风险：开了之后局域网内能访问 :9999 的设备，"
warn "可以切节点、关代理、并从 /connections 看到全屋实时访问的域名。"
warn "不设 CHILL_SECRET 就只能靠来源白名单挡，所以把自己的设备绑静态租约"
warn "或走 tailnet，别让客人/电视/Switch 够得着。设了 CHILL_SECRET 是在白名单"
warn "之外再加一层，不是替代它——这里没有改成默认关闭 CHILL_API_LAN。"
echo
echo "回滚：ssh $HOST 'cp /data/u60-kit/rc.local.before-chill /etc/rc.local && /data/chill/chill.sh stop'"
