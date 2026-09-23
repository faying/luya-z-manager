"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useState } from "react";
import {
  Activity,
  Antenna,
  Bug,
  Cable,
  CalendarClock,
  ChevronDown,
  Cloud,
  Cpu,
  CreditCard,
  EyeOff,
  FileCog,
  Forward,
  Gauge,
  Globe,
  HardDrive,
  LayoutDashboard,
  Lock,
  LogOut,
  MapPin,
  Menu,
  MessageSquare,
  Network,
  Power,
  Radar,
  RadioTower,
  Server,
  Settings,
  Shield,
  ShieldCheck,
  Signal,
  SlidersHorizontal,
  SlidersVertical,
  Smartphone,
  Terminal,
  Timer,
  UserPlus,
  Usb,
  Users,
  Waves,
  Waypoints,
  Wifi,
  X,
  Zap,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { useAuth } from "@/lib/hooks/useAuth";
import { getApiBase } from "@/lib/api/client";
import { StatusStrip } from "@/components/admin/StatusStrip";
import { LangToggle } from "@/components/admin/LangToggle";

interface NavItem {
  href: string;
  label: string;
  tKey: string;
  icon: React.ComponentType<{ size?: number; className?: string }>;
}

interface NavGroup {
  label: string;
  tKey: string;
  items: NavItem[];
}

const NAV: NavGroup[] = [
  {
    label: "Overview",
    tKey: "navGroup.overview",
    items: [
      { href: "/", label: "Dashboard", tKey: "nav.dashboard", icon: LayoutDashboard },
      { href: "/signal", label: "Signal", tKey: "nav.signal", icon: Signal },
      { href: "/clients", label: "Clients", tKey: "nav.clients", icon: Users },
      { href: "/device-info", label: "Device Info", tKey: "nav.deviceInfo", icon: HardDrive },
    ],
  },
  {
    label: "Mobile Network",
    tKey: "navGroup.mobile",
    items: [
      { href: "/router/mobile-network", label: "Mobile Network", tKey: "nav.mobileNetwork", icon: Network },
      { href: "/router/network-mode", label: "Network Mode", tKey: "nav.networkMode", icon: Waypoints },
      { href: "/router/apn", label: "APN", tKey: "nav.apn", icon: Globe },
      { href: "/router/sim", label: "SIM / PIN", tKey: "nav.sim", icon: Lock },
      { href: "/router/esim", label: "eSIM", tKey: "nav.esim", icon: CreditCard },
      { href: "/router/qci", label: "QCI / Bearers", tKey: "nav.qci", icon: SlidersHorizontal },
    ],
  },
  {
    label: "Wi-Fi & LAN",
    tKey: "navGroup.wifiLan",
    items: [
      { href: "/router/wifi", label: "Wi-Fi", tKey: "nav.wifi", icon: Wifi },
      { href: "/router/wifi-guest", label: "Guest Wi-Fi", tKey: "nav.wifiGuest", icon: UserPlus },
      { href: "/router/scenario", label: "Scenarios", tKey: "nav.scenario", icon: MapPin },
      { href: "/router/lan", label: "LAN / DHCP", tKey: "nav.lan", icon: Cable },
      { href: "/router/dns", label: "DNS / DoH", tKey: "nav.dns", icon: Server },
      { href: "/router/firewall", label: "Firewall", tKey: "nav.firewall", icon: Shield },
      { href: "/router/qos", label: "QoS", tKey: "nav.qos", icon: Gauge },
      { href: "/router/vpn", label: "VPN Passthrough", tKey: "nav.vpn", icon: ShieldCheck },
      { href: "/router/telemetry", label: "Telemetry Block", tKey: "nav.telemetry", icon: EyeOff },
    ],
  },
  {
    label: "Messaging",
    tKey: "navGroup.messaging",
    items: [
      { href: "/sms", label: "SMS", tKey: "nav.sms", icon: MessageSquare },
      { href: "/sms/forward", label: "SMS Forward", tKey: "nav.smsForward", icon: Forward },
      { href: "/router/stk", label: "STK / USSD", tKey: "nav.stk", icon: Smartphone },
    ],
  },
  {
    label: "Services",
    tKey: "navGroup.services",
    items: [
      { href: "/services/tailscale", label: "Tailscale", tKey: "nav.tailscale", icon: Cloud },
      { href: "/services/chill", label: "CHILL", tKey: "nav.chill", icon: Waves },
    ],
  },
  {
    label: "Radio Tuning",
    tKey: "navGroup.radio",
    items: [
      { href: "/bandlock", label: "Band Lock", tKey: "nav.bandlock", icon: Antenna },
      { href: "/router/celllock", label: "Cell Lock", tKey: "nav.celllock", icon: RadioTower },
      { href: "/router/signal-detect", label: "Signal Detect", tKey: "nav.signalDetect", icon: Radar },
      { href: "/router/stc", label: "STC", tKey: "nav.stc", icon: SlidersVertical },
    ],
  },
  {
    label: "Tools",
    tKey: "navGroup.tools",
    items: [
      { href: "/tools/at", label: "AT Terminal", tKey: "nav.at", icon: Terminal },
      { href: "/tools/speedtest", label: "Speed Test", tKey: "nav.speedtest", icon: Zap },
      { href: "/tools/processes", label: "Processes", tKey: "nav.processes", icon: Cpu },
      { href: "/tools/enable-adb", label: "Enable ADB", tKey: "nav.enableAdb", icon: Bug },
    ],
  },
  {
    label: "System",
    tKey: "navGroup.system",
    items: [
      { href: "/router/device", label: "Device Control", tKey: "nav.deviceControl", icon: Power },
      { href: "/router/schedule", label: "Schedule Reboot", tKey: "nav.scheduleReboot", icon: Timer },
      { href: "/scheduler", label: "Scheduler", tKey: "nav.scheduler", icon: CalendarClock },
      { href: "/usb", label: "USB Mode", tKey: "nav.usb", icon: Usb },
      { href: "/config", label: "Config Tool", tKey: "nav.config", icon: FileCog },
      { href: "/settings", label: "Settings", tKey: "nav.settings", icon: Settings },
    ],
  },
];

// Longest-prefix match → exactly one active item (fixes parent/child both
// highlighting, e.g. /sms lighting up on /sms/forward).
function norm(p: string): string {
  return p.length > 1 && p.endsWith("/") ? p.slice(0, -1) : p;
}
function activeHrefFor(pathname: string): string {
  const cur = norm(pathname);
  let best = "";
  for (const g of NAV)
    for (const it of g.items) {
      const h = it.href;
      if (cur === h || (h !== "/" && cur.startsWith(h + "/"))) {
        if (h.length > best.length) best = h;
      }
    }
  return best || (cur === "/" ? "/" : "");
}

// Mobile bottom tab bar: 4 primary destinations + a "More" tab that opens the
// full sidebar drawer. `href: null` ⇒ the More button. `match` is the section
// prefix used to highlight the tab (e.g. Services covers all /services/*).
const BOTTOM_TABS: { tKey: string; icon: NavItem["icon"]; href: string | null; match: string }[] = [
  { tKey: "bottomTab.home", icon: Activity, href: "/", match: "/" },
  { tKey: "bottomTab.signal", icon: Signal, href: "/signal", match: "/signal" },
  { tKey: "bottomTab.wifi", icon: Wifi, href: "/router/wifi", match: "/router/wifi" },
  { tKey: "bottomTab.services", icon: Cloud, href: "/services/tailscale", match: "/services" },
  { tKey: "bottomTab.more", icon: Menu, href: null, match: " " },
];

export function AdminShell({ children }: { children: React.ReactNode }) {
  const pathname = usePathname() ?? "";
  const { logout } = useAuth();
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  // Show the actual agent host, not a hard-coded IP (computed client-side to
  // avoid a hydration mismatch with the static export).
  const [apiHost, setApiHost] = useState("");
  useEffect(() => {
    try {
      setApiHost(new URL(getApiBase()).host);
    } catch {
      /* ignore malformed base */
    }
  }, []);

  const activeHref = activeHrefFor(pathname);
  const cur = norm(pathname);
  const tabActive = (m: string) => (m === "/" ? cur === "/" : cur === m || cur.startsWith(m + "/"));

  return (
    <div className="flex min-h-screen">
      {/* Sidebar (desktop static; mobile slide-in drawer behind the "More" tab) */}
      <aside
        className={cn(
          "fixed inset-y-0 left-0 z-50 w-[244px] transform border-r border-border bg-bg-card transition-transform lg:static lg:translate-x-0",
          open ? "translate-x-0" : "-translate-x-full"
        )}
      >
        <div className="flex h-16 items-center justify-between px-5">
          <Link href="/" className="flex items-baseline gap-1.5">
            <span className="font-display text-[17px] font-semibold tracking-tight text-text">
              U60 Pro
            </span>
            <span className="text-[10px] font-medium uppercase tracking-[0.14em] text-text-dim">
              {t("common.advanced")}
            </span>
          </Link>
          <button
            onClick={() => setOpen(false)}
            className="text-text-dim transition hover:text-text lg:hidden"
            aria-label="Close menu"
          >
            <X size={16} />
          </button>
        </div>
        <hr className="admin-hairline-soft mx-5" />
        <nav className="h-[calc(100vh-4rem)] overflow-y-auto px-3 py-4">
          {NAV.map((group) => (
            <SidebarGroup key={group.label} label={t(group.tKey)} activeHref={activeHref}>
              {group.items.map((item) => {
                const active = item.href === activeHref;
                const Icon = item.icon;
                return (
                  <Link
                    key={item.href}
                    href={item.href}
                    onClick={() => setOpen(false)}
                    data-active={active}
                    className="admin-nav-item"
                  >
                    <Icon size={14} className="shrink-0" />
                    {t(item.tKey)}
                  </Link>
                );
              })}
            </SidebarGroup>
          ))}
        </nav>
      </aside>

      {/* Mobile overlay */}
      {open && (
        <button
          aria-label="Close menu"
          className="fixed inset-0 z-30 bg-text/30 backdrop-blur-sm lg:hidden"
          onClick={() => setOpen(false)}
        />
      )}

      {/* Main */}
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="sticky top-0 z-30 flex h-14 items-center gap-3 border-b border-border bg-bg-card/80 px-4 backdrop-blur-md lg:h-16 lg:px-8">
          <h1 className="min-w-0 flex-1 truncate font-display text-[15px] font-semibold tracking-tight text-text">
            {t(currentPageTitleKey(pathname))}
          </h1>
          <div className="flex shrink-0 items-center gap-2 sm:gap-3">
            <StatusStrip />
            <span className="h-4 w-px bg-border" />
            <LangToggle />
            {apiHost && (
              <span className="hidden font-mono text-[11px] text-text-dim sm:inline">{apiHost}</span>
            )}
            <button
              onClick={logout}
              className="flex items-center gap-1.5 text-[12px] text-text-dim transition hover:text-text"
              title={t("common.logOut")}
            >
              <LogOut size={13} />
              <span className="hidden sm:inline">{t("common.signOut")}</span>
            </button>
          </div>
        </header>
        <main className="mx-auto w-full max-w-[1152px] flex-1 px-4 pb-24 pt-6 sm:px-5 lg:px-10 lg:py-10">
          {children}
        </main>
      </div>

      {/* Mobile bottom tab bar */}
      <nav
        className="fixed inset-x-0 bottom-0 z-40 flex h-[58px] items-stretch border-t border-border bg-bg-card/95 backdrop-blur-md lg:hidden"
        style={{ paddingBottom: "env(safe-area-inset-bottom)" }}
      >
        {BOTTOM_TABS.map((tab) => {
          const active = tab.href ? tabActive(tab.match) : open;
          const Icon = tab.icon;
          const cls = cn(
            "relative flex flex-1 flex-col items-center justify-center gap-0.5 text-[10px] font-medium transition-colors",
            active ? "text-accent" : "text-text-dim"
          );
          const inner = (
            <>
              {active && (
                <span className="absolute top-0 h-0.5 w-8 rounded-full bg-[var(--admin-glow)]" />
              )}
              <Icon size={20} className="shrink-0" />
              <span>{t(tab.tKey)}</span>
            </>
          );
          return tab.href ? (
            <Link key={tab.tKey} href={tab.href} onClick={() => setOpen(false)} className={cls}>
              {inner}
            </Link>
          ) : (
            <button key={tab.tKey} onClick={() => setOpen((v) => !v)} className={cls} aria-label={t("bottomTab.more")}>
              {inner}
            </button>
          );
        })}
      </nav>
    </div>
  );
}

function SidebarGroup({
  label,
  activeHref,
  children,
}: {
  label: string;
  activeHref: string;
  children: React.ReactNode;
}) {
  const items = (children as Array<{ props: { href: string } }> | undefined) ?? [];
  const containsActive = items.some?.((c) => c.props?.href === activeHref);
  const [open, setOpen] = useState<boolean>(containsActive);
  return (
    <div className="mb-5">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-3 pb-1.5 text-sm font-semibold uppercase tracking-[0.04em] text-text-dim transition hover:text-text"
      >
        <ChevronDown
          size={14}
          className={cn("transition-transform", open ? "rotate-0" : "-rotate-90")}
        />
        {label}
      </button>
      {open && <div className="space-y-0.5 pl-5">{children}</div>}
    </div>
  );
}

function currentPageTitleKey(pathname: string): string {
  const active = activeHrefFor(pathname);
  for (const group of NAV)
    for (const item of group.items) if (item.href === active) return item.tKey;
  return "common.advanced";
}
