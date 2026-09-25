// One table for every panel route: navigation anchors, the Functions /
// System hub lists, the ⌘K search index and page titles all read it.
// URLs never change (bookmarks keep working). /login is not listed: it is
// outside navigation and search.
//
//   anchor    home │ charts │ functions │ system      (the four tabs)
//   group     the grouped list on a hub page / sidebar section
//   aliases   extra search terms: Chinese, English, pinyin (full and initials)

// Icons: Phosphor (MIT). Rendered bold in lists and filled where they
// mark a selection or a tile (design choice 2026-09-24, option C).
import {
  ArrowBendUpRight, Bell, Broadcast, Bug, CalendarBlank, CellSignalFull, CellTower, ChatCircleText,
  ClockCountdown, Cloud, Cpu, Crosshair, DeviceMobile, EyeSlash, FadersHorizontal, FileText, Gauge, Gear,
  Globe, HardDrive, HardDrives, Heartbeat, House, HouseLine, LockSimple, MapPin, Network, NotePencil,
  Path, Plugs, Power, Shield, ShieldCheck, SimCard, SlidersHorizontal, Speedometer, TerminalWindow,
  UserPlus, Users, Usb, WifiHigh, type Icon,
} from "@phosphor-icons/react";

export type Anchor = "home" | "charts" | "functions" | "system";

export type RouteGroup =
  | "charts"
  | "network"
  | "wifiLan"
  | "services"
  | "messages"
  | "device"
  | "maintenance"
  | "tools";

export interface RouteDef {
  href: string;
  tKey: string;
  fallback: string;
  icon: Icon;
  anchor: Anchor;
  group?: RouteGroup;
  aliases: string[];
  /** Shown only in search (reached from its parent page). */
  hidden?: boolean;
}

export const ROUTES: RouteDef[] = [
  { href: "/", tKey: "nav.dashboard", fallback: "Home", icon: House, anchor: "home", aliases: ["首页", "shouye", "home", "dashboard", "仪表盘"] },

  { href: "/signal", tKey: "nav.signal", fallback: "Signal", icon: CellSignalFull, anchor: "charts", group: "charts", aliases: ["信号", "xinhao", "xh", "rsrp", "sinr", "趋势"] },
  { href: "/router/signal-detect", tKey: "nav.signalDetect", fallback: "Signal Detect", icon: Broadcast, anchor: "charts", group: "charts", aliases: ["信号检测", "信号探测", "xinhaojiance", "detect"] },
  { href: "/tools/speedtest", tKey: "nav.speedtest", fallback: "Speed Test", icon: Gauge, anchor: "charts", group: "charts", aliases: ["测速", "cesu", "cs", "speed"] },
  { href: "/clients", tKey: "nav.clients", fallback: "Clients", icon: Users, anchor: "charts", group: "charts", aliases: ["在线设备", "已连设备", "zaixianshebei", "终端", "clients", "devices"] },

  { href: "/router/mobile-network", tKey: "nav.mobileNetwork", fallback: "Mobile Network", icon: Network, anchor: "functions", group: "network", aliases: ["移动网络", "yidongwangluo", "运营商", "飞行模式", "mobile", "carrier", "airplane"] },
  { href: "/router/network-mode", tKey: "nav.networkMode", fallback: "Network Mode", icon: Path, anchor: "functions", group: "network", aliases: ["选网", "网络模式", "xuanwang", "5g", "sa", "nsa", "mode"] },
  { href: "/router/apn", tKey: "nav.apn", fallback: "APN", icon: Globe, anchor: "functions", group: "network", aliases: ["apn", "接入点", "jierudian", "jrd"] },
  { href: "/router/sim", tKey: "nav.sim", fallback: "SIM / PIN", icon: LockSimple, anchor: "functions", group: "network", aliases: ["sim", "pin", "卡", "ka"] },
  { href: "/router/esim", tKey: "nav.esim", fallback: "eSIM", icon: SimCard, anchor: "functions", group: "network", aliases: ["esim", "e卡", "profile", "lpac", "换卡"] },
  { href: "/router/qci", tKey: "nav.qci", fallback: "QCI / Bearers", icon: SlidersHorizontal, anchor: "functions", group: "network", aliases: ["qci", "承载", "chengzai", "bearer"] },
  { href: "/bandlock", tKey: "nav.bandlock", fallback: "Band Lock", icon: CellTower, anchor: "functions", group: "network", aliases: ["锁频", "suopin", "sp", "band", "bandlock", "频段", "pinduan"] },
  { href: "/router/celllock", tKey: "nav.celllock", fallback: "Cell Lock", icon: Crosshair, anchor: "functions", group: "network", aliases: ["锁小区", "suoxiaoqu", "sxq", "celllock", "cell", "pci"] },
  { href: "/router/stc", tKey: "nav.stc", fallback: "STC", icon: FadersHorizontal, anchor: "functions", group: "network", aliases: ["stc", "白名单"] },

  { href: "/router/wifi", tKey: "nav.wifi", fallback: "Wi-Fi", icon: WifiHigh, anchor: "functions", group: "wifiLan", aliases: ["wifi", "无线", "wuxian", "ssid", "密码"] },
  { href: "/router/wifi-guest", tKey: "nav.wifiGuest", fallback: "Guest Wi-Fi", icon: UserPlus, anchor: "functions", group: "wifiLan", aliases: ["访客", "fangke", "guest"] },
  { href: "/router/lan", tKey: "nav.lan", fallback: "LAN / DHCP", icon: Plugs, anchor: "functions", group: "wifiLan", aliases: ["局域网", "juyuwang", "lan", "dhcp", "ip"] },
  { href: "/router/dns", tKey: "nav.dns", fallback: "DNS / DoH", icon: HardDrives, anchor: "functions", group: "wifiLan", aliases: ["dns", "doh", "域名"] },
  { href: "/router/firewall", tKey: "nav.firewall", fallback: "Firewall", icon: Shield, anchor: "functions", group: "wifiLan", aliases: ["防火墙", "fanghuoqiang", "firewall", "端口转发", "dmz"] },
  { href: "/router/qos", tKey: "nav.qos", fallback: "QoS", icon: Speedometer, anchor: "functions", group: "wifiLan", aliases: ["qos", "限速", "xiansu"] },
  { href: "/router/vpn", tKey: "nav.vpn", fallback: "VPN Passthrough", icon: ShieldCheck, anchor: "functions", group: "wifiLan", aliases: ["vpn", "穿透", "直通"] },
  { href: "/router/telemetry", tKey: "nav.telemetry", fallback: "Telemetry Block", icon: EyeSlash, anchor: "functions", group: "wifiLan", aliases: ["遥测", "yaoce", "telemetry", "拦截"] },

  { href: "/services/tailscale", tKey: "nav.tailscale", fallback: "Tailscale", icon: Cloud, anchor: "functions", group: "services", aliases: ["tailscale", "远程", "yuancheng", "ts"] },
  { href: "/router/scenario", tKey: "nav.scenario", fallback: "Scenarios", icon: MapPin, anchor: "functions", group: "services", aliases: ["情景", "qingjing", "scenario", "场景"] },
  { href: "/router/home-mode", tKey: "nav.homeMode", fallback: "Home Mode", icon: HouseLine, anchor: "functions", group: "services", aliases: ["在家", "回家", "zaijia", "home mode"] },

  { href: "/sms", tKey: "nav.sms", fallback: "SMS", icon: ChatCircleText, anchor: "functions", group: "messages", aliases: ["短信", "duanxin", "dx", "sms", "消息"] },
  { href: "/sms/compose", tKey: "nav.smsCompose", fallback: "New SMS", icon: NotePencil, anchor: "functions", group: "messages", aliases: ["发短信", "写短信", "compose"], hidden: true },
  { href: "/sms/forward", tKey: "nav.smsForward", fallback: "SMS Forward", icon: ArrowBendUpRight, anchor: "functions", group: "messages", aliases: ["短信转发", "zhuanfa", "forward", "telegram"] },
  { href: "/router/stk", tKey: "nav.stk", fallback: "STK / USSD", icon: DeviceMobile, anchor: "functions", group: "messages", aliases: ["stk", "ussd", "菜单"] },

  { href: "/device-info", tKey: "nav.deviceInfo", fallback: "Device Info", icon: HardDrive, anchor: "system", group: "device", aliases: ["设备信息", "shebeixinxi", "imei", "iccid", "版本", "固件"] },
  { href: "/health", tKey: "nav.health", fallback: "Health", icon: Heartbeat, anchor: "system", group: "device", aliases: ["健康", "jiankang", "health", "体检", "doctor"] },
  { href: "/alerts", tKey: "nav.alerts", fallback: "Alerts", icon: Bell, anchor: "system", group: "device", aliases: ["告警", "gaojing", "alerts", "警告"] },

  { href: "/router/device", tKey: "nav.deviceControl", fallback: "Device Control", icon: Power, anchor: "system", group: "maintenance", aliases: ["设备控制", "重启", "chongqi", "reboot", "恢复出厂", "关机"] },
  { href: "/router/schedule", tKey: "nav.scheduleReboot", fallback: "Schedule Reboot", icon: ClockCountdown, anchor: "system", group: "maintenance", aliases: ["定时重启", "dingshi"] },
  { href: "/scheduler", tKey: "nav.scheduler", fallback: "Scheduler", icon: CalendarBlank, anchor: "system", group: "maintenance", aliases: ["计划任务", "jihua", "cron", "scheduler"] },
  { href: "/usb", tKey: "nav.usb", fallback: "USB Mode", icon: Usb, anchor: "system", group: "maintenance", aliases: ["usb", "rndis"] },
  { href: "/config", tKey: "nav.config", fallback: "Config Tool", icon: FileText, anchor: "system", group: "maintenance", aliases: ["配置", "peizhi", "config", "备份"] },
  { href: "/settings", tKey: "nav.settings", fallback: "Settings", icon: Gear, anchor: "system", group: "maintenance", aliases: ["设置", "shezhi", "settings", "深色", "浅色", "语言", "theme"] },

  { href: "/tools/at", tKey: "nav.at", fallback: "AT Terminal", icon: TerminalWindow, anchor: "system", group: "tools", aliases: ["at", "终端", "zhongduan", "terminal"] },
  { href: "/tools/cpu", tKey: "nav.cpu", fallback: "CPU & Memory", icon: Cpu, anchor: "system", group: "tools", aliases: ["cpu", "内存", "neicun", "频率", "memory", "占用"] },
  { href: "/tools/processes", tKey: "nav.processes", fallback: "Processes", icon: Cpu, anchor: "system", group: "tools", aliases: ["进程", "jincheng", "processes", "top"] },
  { href: "/tools/enable-adb", tKey: "nav.enableAdb", fallback: "Enable ADB", icon: Bug, anchor: "system", group: "tools", aliases: ["adb", "调试"] },
];

export const HUBS: Record<Exclude<Anchor, "home">, { href: string; tKey: string; fallback: string }> = {
  charts: { href: "/charts", tKey: "anchor.charts", fallback: "Charts" },
  functions: { href: "/functions", tKey: "anchor.functions", fallback: "Functions" },
  system: { href: "/system", tKey: "anchor.system", fallback: "System" },
};

export const GROUPS: Record<RouteGroup, { tKey: string; fallback: string; anchor: Anchor }> = {
  charts: { tKey: "routeGroup.charts", fallback: "Charts", anchor: "charts" },
  network: { tKey: "routeGroup.network", fallback: "Network & bands", anchor: "functions" },
  wifiLan: { tKey: "routeGroup.wifiLan", fallback: "Wi-Fi & LAN", anchor: "functions" },
  services: { tKey: "routeGroup.services", fallback: "Services", anchor: "functions" },
  messages: { tKey: "routeGroup.messages", fallback: "Messages", anchor: "functions" },
  device: { tKey: "routeGroup.device", fallback: "Device", anchor: "system" },
  maintenance: { tKey: "routeGroup.maintenance", fallback: "Maintenance", anchor: "system" },
  tools: { tKey: "routeGroup.tools", fallback: "Tools", anchor: "system" },
};

export function normPath(p: string): string {
  return p.length > 1 && p.endsWith("/") ? p.slice(0, -1) : p;
}

/** Longest-prefix match, so /sms/forward is not also /sms. */
export function routeFor(pathname: string): RouteDef | undefined {
  const cur = normPath(pathname);
  let best: RouteDef | undefined;
  for (const r of ROUTES) {
    if (cur === r.href || (r.href !== "/" && cur.startsWith(r.href + "/"))) {
      if (!best || r.href.length > best.href.length) best = r;
    }
  }
  return best;
}

/** Which of the four anchors a path belongs to (hub pages included). */
export function anchorFor(pathname: string): Anchor {
  const cur = normPath(pathname);
  for (const [a, h] of Object.entries(HUBS)) if (cur === h.href) return a as Anchor;
  return routeFor(cur)?.anchor ?? "home";
}

// ---- search ---------------------------------------------------------------

function fold(s: string): string {
  return s.toLowerCase().normalize("NFKC").replace(/[\s/·\-_]+/g, "");
}

/**
 * Rank routes for a query against the visible title and aliases. Exact
 * alias → 100, title/alias prefix → 60, substring → 30. Ties keep table
 * order, so the table order is the tiebreak.
 */
export function searchRoutes(query: string, title: (r: RouteDef) => string): RouteDef[] {
  const q = fold(query);
  if (!q) return [];
  const scored: { r: RouteDef; s: number; i: number }[] = [];
  ROUTES.forEach((r, i) => {
    const terms = [title(r), r.fallback, ...r.aliases].map(fold);
    let s = 0;
    for (const t of terms) {
      if (t === q) s = Math.max(s, 100);
      else if (t.startsWith(q)) s = Math.max(s, 60);
      else if (t.includes(q)) s = Math.max(s, 30);
    }
    if (s > 0) scored.push({ r, s, i });
  });
  return scored.sort((a, b) => b.s - a.s || a.i - b.i).map((x) => x.r);
}
