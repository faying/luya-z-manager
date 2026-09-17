"use client";

import Link from "next/link";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { cn } from "@/lib/utils";
import { Wifi, Signal, Cloud, Waves, MessageSquare } from "lucide-react";

interface PublicStatus {
  network?: { connected?: boolean; type?: string; rsrp?: number };
  wifi?: { on?: boolean };
  services?: {
    tailscale?: { running?: boolean; installed?: boolean };
    chill?: { state?: string; reason?: string | null };
  };
  sms?: { unread?: number };
}

type Tone = "success" | "warning" | "danger" | "accent" | "neutral";

/**
 * Compact status strip for the top header — one health-colored icon per
 * subsystem, each linking to its page. Tones: green = healthy, amber =
 * configured-but-stopped, red = down when it matters (WAN), dim = off /
 * not applicable. SMS blinks with a count badge when unread.
 * Backed by a single /api/public/status poll.
 */
export function StatusStrip() {
  const { t } = useTranslation();
  const { data } = useApi<PublicStatus>("/api/public/status", { refreshInterval: 10000 });
  const net = data?.network;
  const connected = !!net?.connected;
  const rsrp = net?.rsrp ?? 0;
  const netType = net?.type ? net.type.toUpperCase() : "";
  const weak = rsrp !== 0 && rsrp <= -110;

  // WAN is a CPE's core job: connected = good (weak signal → amber), down = red.
  const netTone: Tone = connected ? (weak ? "warning" : "success") : "danger";
  const netLabel = connected
    ? `${t("status.mobileConnected")}${netType ? ` · ${netType}` : ""}${weak ? ` · ${t("status.weakSignal")}` : ""}`
    : t("status.wanDown");

  const wifiOn = !!data?.wifi?.on;

  const svcLabel = (name: string, installed?: boolean, running?: boolean): string =>
    running
      ? t("status.svcRunning", { name })
      : installed
        ? t("status.svcStopped", { name })
        : t("status.svcNotInstalled", { name });

  const ts = data?.services?.tailscale;
  const chill = data?.services?.chill;
  const chillTone: Tone = chill?.state === "running" ? "success" : chill?.state === "direct" ? "warning" : "neutral";
  const chillLabel =
    chill?.state === "running"
      ? t("status.svcRunning", { name: "CHILL" })
      : chill?.state === "direct"
        ? t("status.svcStopped", { name: "CHILL" })
        : t("status.svcNotInstalled", { name: "CHILL" });
  const unread = data?.sms?.unread ?? 0;

  return (
    <div className="flex items-center gap-0.5">
      <StatusIcon href="/signal" icon={Signal} tone={netTone} label={netLabel} />
      <StatusIcon
        href="/router/wifi"
        icon={Wifi}
        tone={wifiOn ? "success" : "neutral"}
        label={wifiOn ? t("status.wifiOn") : t("status.wifiOff")}
      />
      <StatusIcon
        href="/services/tailscale"
        icon={Cloud}
        tone={svcTone(ts?.installed, ts?.running)}
        label={svcLabel("Tailscale", ts?.installed, ts?.running)}
      />
      <StatusIcon href="/services/chill" icon={Waves} tone={chillTone} label={chillLabel} />
      <StatusIcon
        href="/sms"
        icon={MessageSquare}
        tone={unread > 0 ? "accent" : "neutral"}
        blink={unread > 0}
        badge={unread}
        label={unread > 0 ? t("status.smsUnread", { count: unread }) : t("status.smsNone")}
      />
    </div>
  );
}

/** installed + running → green; installed + stopped → amber; not installed → dim. */
function svcTone(installed?: boolean, running?: boolean): Tone {
  if (running) return "success";
  if (installed) return "warning";
  return "neutral";
}

const TONE_TEXT: Record<Tone, string> = {
  success: "text-success",
  warning: "text-warning",
  danger: "text-error",
  accent: "text-accent",
  neutral: "text-text-dim/40",
};

const TONE_BADGE: Record<Tone, string> = {
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-error",
  accent: "bg-accent",
  neutral: "bg-text-dim",
};

function StatusIcon({
  href,
  icon: Icon,
  tone,
  blink,
  badge,
  label,
}: {
  href: string;
  icon: React.ComponentType<{ size?: number; className?: string }>;
  tone: Tone;
  blink?: boolean;
  badge?: number;
  label: string;
}) {
  return (
    <Link
      href={href}
      title={label}
      aria-label={label}
      className={cn(
        "relative flex h-7 w-7 items-center justify-center rounded-md transition-colors hover:bg-bg",
        TONE_TEXT[tone]
      )}
    >
      <Icon size={15} className={cn(blink && "animate-pulse")} />
      {!!badge && badge > 0 && (
        <span
          className={cn(
            "absolute right-0 top-0 flex h-3.5 min-w-[0.875rem] items-center justify-center rounded-full px-1 text-[8px] font-bold leading-none text-white",
            TONE_BADGE[tone]
          )}
        >
          {badge > 9 ? "9+" : badge}
        </span>
      )}
    </Link>
  );
}
