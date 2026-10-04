#!/bin/bash
# ─────────────────────────────────────────────────────────────────────────────
# install.sh — interactive, menu-driven installer for the Open U60 Pro toolkit.
#
# ShellCrash-style menu. Each component installs/updates/uninstalls independently
# and is idempotent. Boot persistence is via /etc/rc.local and cron — never
# init.d (avoids the zte_topsw_daemon sync barrier; see CLAUDE.md).
#
#   ./install.sh                 # menu
#   GATEWAY=192.168.0.1 ./install.sh
#
# Requires SSH access to the device (run ./setup.sh once if SSH isn't set up).
# A Tailscale auth key is entered interactively, used once to log in and not
# stored anywhere; settings go to /data/tailscale/tuning.env on the device.
# ─────────────────────────────────────────────────────────────────────────────
set -u

RED=$'\033[0;31m'; GREEN=$'\033[0;32m'; YELLOW=$'\033[1;33m'; CYAN=$'\033[0;36m'; BOLD=$'\033[1m'; DIM=$'\033[2m'; NC=$'\033[0m'
info() { printf "${CYAN}[*]${NC} %s\n" "$1"; }
ok()   { printf "${GREEN}[+]${NC} %s\n" "$1"; }
warn() { printf "${YELLOW}[!]${NC} %s\n" "$1"; }
err()  { printf "${RED}[-]${NC} %s\n" "$1" >&2; }

DIR="$(cd "$(dirname "$0")" && pwd)"
GATEWAY="${GATEWAY:-192.168.0.1}"
SSH_PORT="${SSH_PORT:-2222}"
TS_VERSION_DEFAULT="1.102.3"

# Auth: empty SSH_PASS ⇒ use SSH key/agent; otherwise use sshpass with the
# password. Connection multiplexing reuses one TCP/auth session for the burst
# of calls each module makes (faster, and avoids dropbear's reconnect throttle).
SSH_PASS=""
CTL_DIR="$(mktemp -d 2>/dev/null || echo /tmp/u60ctl.$$)"; mkdir -p "$CTL_DIR"
cleanup() { ssh -O exit -o ControlPath="$CTL_DIR/cm" "root@$GATEWAY" 2>/dev/null; rm -rf "$CTL_DIR"; }
trap cleanup EXIT

ssh_opts() {
  local o="-p $SSH_PORT -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8"
  o="$o -o ControlMaster=auto -o ControlPath=$CTL_DIR/cm -o ControlPersist=120"
  [ -n "$SSH_PASS" ] && o="$o -o PreferredAuthentications=password -o PubkeyAuthentication=no"
  echo "$o"
}
_ssh() {
  # stdin from /dev/null: a command-mode ssh otherwise reads (and swallows) the
  # interactive menu's stdin. rpush, which needs stdin, has its own invocation.
  if [ -n "$SSH_PASS" ]; then sshpass -p "$SSH_PASS" ssh $(ssh_opts) "root@$GATEWAY" "$@" </dev/null
  else ssh $(ssh_opts) "root@$GATEWAY" "$@" </dev/null; fi
}
rcmd() { _ssh "$@"; }
rpush() { # <local> <remote> [mode]
  local s="$1" d="$2" m="${3:-755}"
  if [ -n "$SSH_PASS" ]; then
    sshpass -p "$SSH_PASS" ssh $(ssh_opts) "root@$GATEWAY" "cat > '$d' && chmod $m '$d'" < "$s"
  else
    ssh $(ssh_opts) "root@$GATEWAY" "cat > '$d' && chmod $m '$d'" < "$s"
  fi
}

# Establish auth: try key first, else prompt for a password (sshpass).
connect_setup() {
  info "Connecting to root@$GATEWAY:$SSH_PORT …"
  SSH_PASS=""
  if ssh $(ssh_opts) -o BatchMode=yes "root@$GATEWAY" "echo ok" </dev/null >/dev/null 2>&1; then
    ok "Connected with SSH key."; return 0
  fi
  if ! command -v sshpass >/dev/null 2>&1; then
    warn "sshpass not installed — needed for password login."
    if command -v brew >/dev/null 2>&1; then
      printf "${CYAN}Install sshpass via Homebrew? [Y/n]:${NC} "; read -r a
      case "$a" in [Nn]*) ;; *) brew install sshpass 2>/dev/null || brew install hudochenkov/sshpass/sshpass 2>/dev/null;; esac
    elif command -v apt-get >/dev/null 2>&1; then
      printf "${CYAN}Install sshpass via apt? [Y/n]:${NC} "; read -r a
      case "$a" in [Nn]*) ;; *) sudo apt-get install -y sshpass;; esac
    fi
  fi
  printf "${CYAN}Device root password (blank = try interactive ssh):${NC} "; read -rs pw; echo
  if [ -n "$pw" ] && command -v sshpass >/dev/null 2>&1; then
    SSH_PASS="$pw"
  elif [ -n "$pw" ]; then
    warn "sshpass unavailable — ssh will prompt for the password on each step."
  fi
  if rcmd "echo ok" >/dev/null 2>&1; then ok "Connected."; return 0; fi
  err "Connection failed — check the device IP and password."; SSH_PASS=""; return 1
}

# Idempotent insert of a line into /etc/rc.local (before `exit 0`), keyed by marker.
# The previous file is kept as /data/rc.local.bak-installer and put back when
# the result fails `sh -n`.
rc_add() { # <line> <unique-marker>
  local line="$1" mark="$2"
  rcmd "
    grep -qF '$mark' /etc/rc.local 2>/dev/null && exit 0
    touch /etc/rc.local
    cp /etc/rc.local /data/rc.local.bak-installer
    if grep -q '^exit 0' /etc/rc.local; then
      awk -v ins='$line' '/^exit 0/ && !d {print ins; d=1} {print}' /etc/rc.local > /tmp/rc.\$\$ \
        && cat /tmp/rc.\$\$ > /etc/rc.local && rm -f /tmp/rc.\$\$
    else
      echo '$line' >> /etc/rc.local
    fi
    sh -n /etc/rc.local || { cat /data/rc.local.bak-installer > /etc/rc.local; echo 'rc.local failed sh -n; previous file restored' >&2; exit 1; }
  "
}
rc_del() { rcmd "cp /etc/rc.local /data/rc.local.bak-installer 2>/dev/null; sed -i '\\#$1#d' /etc/rc.local 2>/dev/null; true"; }

require_conn() {
  rcmd "echo ok" >/dev/null 2>&1 && return 0
  connect_setup
}

pause() { printf "\n${DIM}Press Enter to continue…${NC}"; read -r _; }

# ── Module: Agent + Web UI + SSH (delegates to the proven setup.sh) ──────────
module_agent() {
  info "Launching setup.sh (agent binary + admin UI + SSH/startup/rc.local)…"
  warn "setup.sh does its own auth (SSH key, or ADB for first-time SSH setup) — it won't use the password entered here."
  GATEWAY="$GATEWAY" bash "$DIR/setup.sh"
}

# ── Module: Tailscale (interactive) ──────────────────────────────────────────
module_tailscale() {
  require_conn || return 1
  echo; printf "${BOLD}── Tailscale ──${NC}\n"

  # 1) binary
  if rcmd "[ -x /data/tailscale/tailscaled ] && [ -x /data/tailscale/tailscale ]" 2>/dev/null; then
    ok "tailscale binary already on device ($(rcmd '/data/tailscale/tailscale version 2>/dev/null | head -1'))."
  else
    printf "${CYAN}Tailscale version to download [%s]:${NC} " "$TS_VERSION_DEFAULT"; read -r ver
    ver="${ver:-$TS_VERSION_DEFAULT}"
    local url="https://pkgs.tailscale.com/stable/tailscale_${ver}_arm64.tgz"
    info "Downloading $url …"
    local tmp; tmp=$(mktemp -d)
    if ! curl -fsSL "$url" -o "$tmp/ts.tgz"; then
      err "Download failed. Check the version or your connection."; rm -rf "$tmp"; return 1
    fi
    tar xzf "$tmp/ts.tgz" -C "$tmp" || { err "Extract failed."; rm -rf "$tmp"; return 1; }
    local d; d=$(find "$tmp" -maxdepth 1 -type d -name 'tailscale_*' | head -1)
    [ -x "$d/tailscaled" ] || { err "binary not found in package."; rm -rf "$tmp"; return 1; }
    rcmd "mkdir -p /data/tailscale"
    info "Pushing tailscaled + tailscale (~30 MB)…"
    rpush "$d/tailscaled" /data/tailscale/tailscaled
    rpush "$d/tailscale" /data/tailscale/tailscale
    rm -rf "$tmp"
    ok "tailscale $ver installed to /data/tailscale/."
  fi

  # 2) interactive config (scripts/tailscale/README.md lists the settings)
  local def_route
  def_route=$(rcmd "ip -o -f inet addr show br-lan 2>/dev/null | awk '{print \$4}'" 2>/dev/null | head -1 \
              | awk -F'[./]' 'NF>=4{printf "%s.%s.%s.0/24",$1,$2,$3}')
  echo
  printf "${CYAN}Auth key (tskey-…; used once to log in, never stored; blank = log in by URL or keep the saved login):${NC} "
  read -rs authkey; echo
  printf "${CYAN}Advertise LAN subnet [%s] (blank to keep, - to skip):${NC} " "${def_route:-none}"; read -r routes
  routes="${routes:-$def_route}"; [ "$routes" = - ] && routes=
  printf "${CYAN}Exit node (IP or name, blank = none):${NC} "; read -r exit_node
  local def_host
  def_host=$(rcmd "/data/tailscale/tailscale --socket=/tmp/tailscaled.sock debug prefs 2>/dev/null | grep Hostname | head -1 | cut -d: -f2- | tr -d ' \",'" 2>/dev/null | tr -d '\r')
  printf "${CYAN}Node name in the tailnet [%s]:${NC} " "${def_host:-u60pro}"; read -r ts_hostname
  ts_hostname="${ts_hostname:-${def_host:-u60pro}}"
  printf "${CYAN}Accept routes from your tailnet? [Y/n]:${NC} "; read -r a
  case "$a" in [Nn]*) accept_routes=false;; *) accept_routes=true;; esac
  printf "${CYAN}Let Tailscale manage DNS? [y/N]:${NC} "; read -r a
  case "$a" in [Yy]*) accept_dns=true;; *) accept_dns=false;; esac
  # values go into a shell file on the device: only plain characters
  case "$routes$exit_node$ts_hostname" in *[!A-Za-z0-9._:/,-]*)
    err "Only letters, digits and . _ : / , - are allowed in routes, exit node and name."; return 1 ;; esac
  case "$authkey" in *[!A-Za-z0-9_-]*) err "That does not look like an auth key."; return 1 ;; esac

  # 3) scripts + settings. Other lines of an existing tuning.env are kept.
  rcmd "mkdir -p /data/tailscale"
  rpush "$DIR/scripts/tailscale/start.sh" /data/tailscale/start.sh 644
  rpush "$DIR/scripts/tailscale/apply.sh" /data/tailscale/apply.sh 755
  local cfg; cfg=$(mktemp)
  {
    printf "TS_HOSTNAME='%s'\n" "$ts_hostname"
    printf "TS_ROUTES='%s'\n" "$routes"
    printf "TS_EXIT_NODE='%s'\n" "$exit_node"
    printf "TS_ACCEPT_ROUTES='%s'\n" "$accept_routes"
    printf "TS_ACCEPT_DNS='%s'\n" "$accept_dns"
  } > "$cfg"
  rpush "$cfg" /tmp/ts-prefs 600
  rm -f "$cfg"
  rcmd "cd /data/tailscale && { grep -v -E '^(TS_HOSTNAME|TS_ROUTES|TS_EXIT_NODE|TS_ACCEPT_ROUTES|TS_ACCEPT_DNS|TS_AUTHKEY)=' tuning.env 2>/dev/null; cat /tmp/ts-prefs; } > tuning.env.tmp && mv -f tuning.env.tmp tuning.env; rm -f /tmp/ts-prefs"

  # 4) the old tailscale-start.sh (deprecated): its rc.local line and its
  #    tsconfig, which held the auth key. start.sh moves an old state file.
  if rcmd "grep -qF tailscale-start.sh /etc/rc.local" 2>/dev/null; then
    rc_del 'tailscale-start.sh'; ok "Removed the old tailscale-start.sh line from rc.local."
  fi
  rcmd "rm -f /data/tailscale/tsconfig /data/tailscale-start.sh"

  # 5) check before anything starts at boot
  if ! rcmd "sh /data/tailscale/start.sh check"; then
    err "Check failed (above). Nothing was added to rc.local; fix it and run this again."; return 1
  fi

  # 6) start (a running tailscaled is restarted so the settings apply), log in once
  printf "${CYAN}Start Tailscale now? [Y/n]:${NC} "; read -r a
  case "$a" in
    [Nn]*) info "Not started.";;
    *) info "Starting…"
       rcmd "sh /data/tailscale/start.sh stop >/dev/null 2>&1; sh /data/tailscale/start.sh"
       rcmd "i=0; while [ ! -S /tmp/tailscaled.sock ] && [ \$i -lt 60 ]; do sleep 2; i=\$((i+2)); done"
       if [ -n "$authkey" ]; then
         local kf; kf=$(mktemp); printf '%s' "$authkey" > "$kf"
         rpush "$kf" /tmp/ts-authkey 600; rm -f "$kf"
         rcmd "/data/tailscale/tailscale --socket=/tmp/tailscaled.sock login --auth-key=file:/tmp/ts-authkey --timeout=60s; rm -f /tmp/ts-authkey"
       elif rcmd "/data/tailscale/tailscale --socket=/tmp/tailscaled.sock status --json 2>/dev/null | grep -q '\"BackendState\": *\"NeedsLogin\"'"; then
         info "Open the link below to log this device in (waits up to 3 minutes)."
         rcmd "/data/tailscale/tailscale --socket=/tmp/tailscaled.sock login --timeout=180s"
       fi
       rcmd "/data/tailscale/tailscale --socket=/tmp/tailscaled.sock status 2>&1 | head -8" ;;
  esac
  rc_add 'sh /data/tailscale/start.sh' '/data/tailscale/start.sh' && ok "Starts at boot (rc.local). Off switch: sh /data/tailscale/start.sh disable"
}

# ── Module: Home Mode ────────────────────────────────────────────────────────
module_homemode() {
  require_conn || return 1
  echo; printf "${BOLD}── Home Mode (auto Wi-Fi off when home) ──${NC}\n"
  rpush "$DIR/scripts/homemode.sh" /data/homemode.sh
  rpush "$DIR/scripts/homemode-bootsafe.sh" /data/homemode-bootsafe.sh
  rcmd "
    mkdir -p /data/homemode /data/log
    touch /etc/crontabs/root
    grep -qF '/data/homemode.sh' /etc/crontabs/root || echo '* * * * * /data/homemode.sh' >> /etc/crontabs/root
    /etc/init.d/cron restart >/dev/null 2>&1 || killall -HUP crond 2>/dev/null
  "
  rc_add 'sh /data/homemode-bootsafe.sh &' 'homemode-bootsafe'
  ok "Home Mode installed (cron every minute; boot-safe reset wired)."
  echo "    Configure SSIDs/timing in the admin UI → Router → Home Mode."
}

# ── Module: System monitor ───────────────────────────────────────────────────
module_monitor() {
  require_conn || return 1
  echo; printf "${BOLD}── System monitor (temp/battery/signal log + forensics) ──${NC}\n"
  rpush "$DIR/scripts/monitor.sh" /data/local/tmp/monitor.sh
  rcmd "mkdir -p /data/local/tmp /data/log
    cat > /data/local/tmp/start_monitor.sh <<'EOF'
#!/bin/sh
start-stop-daemon -S -b -m -p /tmp/monitor.pid -x /bin/sh -- -c 'exec /data/local/tmp/monitor.sh >/tmp/monitor.err 2>&1'
EOF
    chmod +x /data/local/tmp/start_monitor.sh"
  rc_add 'sh /data/local/tmp/start_monitor.sh' 'start_monitor.sh'
  rcmd "sh /data/local/tmp/start_monitor.sh 2>/dev/null; true"
  ok "Monitor installed and started → /data/log/monitor.log"
}

# ── Module: Crash recovery (SSR) ─────────────────────────────────────────────
module_recovery() {
  require_conn || return 1
  echo; printf "${BOLD}── Crash recovery (per-subsystem SSR instead of full reboot) ──${NC}\n"
  rpush "$DIR/scripts/recovery_setup.sh" /data/local/tmp/recovery_setup.sh
  rc_add 'sh /data/local/tmp/recovery_setup.sh &' 'recovery_setup.sh'
  rcmd "sh /data/local/tmp/recovery_setup.sh 2>/dev/null &"
  ok "Recovery setup installed (applies on boot; ran once now)."
}

# ── Status ───────────────────────────────────────────────────────────────────
module_status() {
  require_conn || return 1
  echo; printf "${BOLD}── Status @ %s ──${NC}\n" "$GATEWAY"
  rcmd '
    p(){ printf "  %-16s %s\n" "$1" "$2"; }
    p "zte-agent" "$(pidof zte-agent >/dev/null && echo running\ \(pid\ $(pidof zte-agent)\) || echo stopped)"
    p "admin UI" "$([ -d /data/admin ] && echo deployed || echo absent)"
    p "dropbear SSH" "$(pidof dropbear >/dev/null && echo running || echo stopped)"
    p "tailscale" "$([ -x /data/tailscale/tailscaled ] && (pidof tailscaled >/dev/null && echo running || echo installed/stopped) || echo not installed)"
    p "  ts node" "$(/data/tailscale/tailscale --socket=/tmp/tailscaled.sock status 2>/dev/null | awk "NR==1{print \$1, \$2}")"
    p "home mode" "$(grep -q /data/homemode.sh /etc/crontabs/root 2>/dev/null && echo "cron on (state: $(cat /data/homemode/state 2>/dev/null))" || echo absent)"
    p "monitor" "$(pidof -x monitor.sh >/dev/null 2>&1 || pgrep -f monitor.sh >/dev/null 2>&1 && echo running || ([ -f /tmp/monitor.pid ] && echo running || echo stopped))"
    p "shellcrash" "$(ps w | grep -E "CrashCore|sing-box" | grep -qv grep && echo running || echo stopped/absent)"
    echo "  --- rc.local autostarts ---"
    grep -E "start_zte_agent|start_dropbear|start_monitor|tailscale-start|tailscale/start.sh|homemode-bootsafe|recovery_setup" /etc/rc.local 2>/dev/null | sed "s/^/    /"
  '
}

# ── Uninstall ────────────────────────────────────────────────────────────────
module_uninstall() {
  require_conn || return 1
  echo; printf "${BOLD}── Uninstall ──${NC}\n"
  echo "  1) Home Mode    2) Monitor    3) Recovery    4) Tailscale    0) back"
  printf "${CYAN}Remove which? ${NC}"; read -r u
  case "$u" in
    1) rcmd "sed -i '\\#/data/homemode.sh#d' /etc/crontabs/root 2>/dev/null
            /etc/init.d/cron restart >/dev/null 2>&1
            uci set wireless.wifi0.disabled=0; uci set wireless.wifi1.disabled=0; uci commit wireless
            ubus call zwrt_wlan reload >/dev/null 2>&1
            rm -f /data/homemode.sh /data/homemode-bootsafe.sh; rm -rf /data/homemode"
       rc_del 'homemode-bootsafe'; ok "Home Mode removed; Wi-Fi restored ON." ;;
    2) rc_del 'start_monitor.sh'; rcmd "[ -f /tmp/monitor.pid ] && kill \$(cat /tmp/monitor.pid) 2>/dev/null; rm -f /data/local/tmp/monitor.sh /data/local/tmp/start_monitor.sh"; ok "Monitor removed." ;;
    3) rc_del 'recovery_setup.sh'; rcmd "rm -f /data/local/tmp/recovery_setup.sh"; ok "Recovery removed (takes effect next boot)." ;;
    4) rcmd "if [ -f /data/tailscale/start.sh ]; then sh /data/tailscale/start.sh stop; else killall tailscaled 2>/dev/null; fi; rm -f /data/tailscale/tsconfig /data/tailscale-start.sh"
       rc_del '/data/tailscale/start.sh'; rc_del 'tailscale-start.sh'
       warn "Stopped Tailscale and removed it from rc.local. Binaries and the node identity are kept in /data/tailscale/." ;;
    *) ;;
  esac
}

# ── Menu ─────────────────────────────────────────────────────────────────────
menu() {
  connect_setup || warn "Not connected yet — pick a component and I'll prompt again."
  while true; do
    echo
    printf "${BOLD}╔══ Open U60 Pro installer ══╗${NC}\n"
    printf "  device: ${CYAN}root@%s:%s${NC}\n" "$GATEWAY" "$SSH_PORT"
    echo   "  1) Agent + Admin Web + SSH   (runs setup.sh)"
    echo   "  2) Tailscale                 (interactive: key, routes, exit node)"
    echo   "  3) Home Mode                 (Wi-Fi auto-off at home)"
    echo   "  4) System monitor            (temp/signal log + forensics)"
    echo   "  5) Crash recovery (SSR)"
    echo   "  6) Status"
    echo   "  7) Uninstall a component"
    echo   "  c) Change device IP (current: $GATEWAY)"
    echo   "  0) Exit"
    printf "${CYAN}> ${NC}"; read -r choice
    case "$choice" in
      1) module_agent; pause ;;
      2) module_tailscale; pause ;;
      3) module_homemode; pause ;;
      4) module_monitor; pause ;;
      5) module_recovery; pause ;;
      6) module_status; pause ;;
      7) module_uninstall; pause ;;
      c|C) printf "Device IP [%s]: " "$GATEWAY"; read -r g; GATEWAY="${g:-$GATEWAY}"; cleanup; mkdir -p "$CTL_DIR"; connect_setup ;;
      0|q|Q) exit 0 ;;
      *) warn "Unknown choice." ;;
    esac
  done
}

menu
