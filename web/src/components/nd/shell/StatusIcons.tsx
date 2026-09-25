"use client";
// Header status icons (was admin/StatusStrip): network, Wi-Fi, Tailscale,
// SMS — each a 44px link to its page, tone from /api/public/status.
// Every icon carries its state in the accessible name and tooltip, so the
// colour is never the only cue. Filled when on, bold when off; the signal
// icon stays bold (filled it is a plain wedge, not bars).
import Link from "next/link";
import { ChatCircleText, CellSignalFull, Cloud, WifiHigh, type Icon as PhIcon } from "@phosphor-icons/react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";

interface PublicStatus {
  network?: { connected?: boolean; type?: string; rsrp?: number };
  wifi?: { on?: boolean };
  services?: {
    tailscale?: { running?: boolean; installed?: boolean };
  };
  sms?: { unread?: number };
}

type IconTone = "ok" | "warn" | "bad" | "acc" | "off";

const TONE_CLASS: Record<IconTone, string> = {
  ok: "text-nd-t2",
  warn: "text-nd-warnT",
  bad: "text-nd-badT",
  acc: "text-nd-t2",
  off: "text-nd-t3",
};

export function StatusIcons() {
  const { t } = useTranslation();
  const { data } = useApi<PublicStatus>("/api/public/status", { refreshInterval: 10000 });
  const net = data?.network;
  const connected = !!net?.connected;
  const rsrp = net?.rsrp ?? 0;
  const netType = net?.type ? net.type.toUpperCase() : "";
  const weak = rsrp !== 0 && rsrp <= -110;
  const netLabel = connected
    ? `${t("status.mobileConnected")}${netType ? ` · ${netType}` : ""}${weak ? ` · ${t("status.weakSignal")}` : ""}`
    : t("status.wanDown");
  const wifiOn = !!data?.wifi?.on;
  const ts = data?.services?.tailscale;
  const svc = (name: string, installed?: boolean, running?: boolean) =>
    running ? t("status.svcRunning", { name }) : installed ? t("status.svcStopped", { name }) : t("status.svcNotInstalled", { name });
  const unread = data?.sms?.unread ?? 0;

  if (!data) return null;
  return (
    <div className="flex items-center">
      <StatusIcon href="/signal" icon={CellSignalFull} tone={connected ? (weak ? "warn" : "ok") : "bad"} label={netLabel} />
      <StatusIcon href="/router/wifi" icon={WifiHigh} tone={wifiOn ? "ok" : "off"} label={wifiOn ? t("status.wifiOn") : t("status.wifiOff")} />
      <StatusIcon
        href="/services/tailscale"
        icon={Cloud}
        tone={ts?.running ? "ok" : ts?.installed ? "warn" : "off"}
        label={svc("Tailscale", ts?.installed, ts?.running)}
      />
      <StatusIcon
        href="/sms"
        icon={ChatCircleText}
        tone={unread > 0 ? "acc" : "off"}
        badge={unread}
        label={unread > 0 ? t("status.smsUnread", { count: unread }) : t("status.smsNone")}
      />
    </div>
  );
}

function StatusIcon({ href, icon: Icon, tone, badge, label }: { href: string; icon: PhIcon; tone: IconTone; badge?: number; label: string }) {
  return (
    <Link
      href={href}
      title={label}
      aria-label={label}
      className={`relative flex h-11 w-11 items-center justify-center rounded-full hover:bg-nd-track ${TONE_CLASS[tone]}`}
    >
      <Icon size={20} weight={tone === "off" || Icon === CellSignalFull ? "bold" : "fill"} aria-hidden />
      {!!badge && badge > 0 && (
        <span className="absolute right-1 top-1 flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-nd-primary px-1 text-[11px] font-semibold leading-none text-nd-onPrimary">
          {badge > 9 ? "9+" : badge}
        </span>
      )}
    </Link>
  );
}
