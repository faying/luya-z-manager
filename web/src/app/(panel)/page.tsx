"use client";

import Link from "next/link";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { PageHeader, SectionCard, Status, MetaRow } from "@/components/admin/StatCard";
import { Help } from "@/components/admin/Help";
import { cn } from "@/lib/utils";
import { ArrowDown, ArrowUp } from "lucide-react";

interface NetworkSignal {
  network_type?: string;
  network_provider?: string;
  network_provider_fullname?: string;
  net_select_mode?: string;
  nr5g_rsrp?: number;
  nr5g_rsrq?: number;
  nr5g_snr?: string;
  nr5g_action_band?: string;
  nr5g_action_channel?: number;
  nr5g_cell_id?: number;
  nr5g_pci?: number;
  nr5g_bandwidth?: string;
  lte_rsrp?: number;
  lte_rsrq?: number;
  lte_snr?: string;
  signalbar?: string;
}

interface BatteryInfo {
  battery_capacity?: number;
  battery_online?: number;
  battery_time_to_full?: number;
}

interface Speed {
  rx_speed?: number; // bytes/sec
  tx_speed?: number;
}

interface DeviceSystem {
  uptime?: number;
}

interface WiFiStatus {
  ssid_2g?: string;
  ssid_5g?: string;
  actual_channel_2g?: string;
  actual_channel_5g?: string;
  actual_bw_2g?: string;
  actual_bw_5g?: string;
  wifi_onoff?: string;
  clients_total?: number;
  encryption_5g?: string;
}

interface PublicStatus {
  services: {
    tailscale: { running: boolean; installed?: boolean; node: string };
    chill: { state: string; reason?: string | null };
    home_mode: { present: boolean; enabled: boolean; mode: string };
  };
  sms?: { unread?: number };
}

export default function DashboardPage() {
  const { t } = useTranslation();
  const { data: sys } = useApi<DeviceSystem>("/api/device/system", { refreshInterval: 5000 });
  const { data: sig, error: sigErr } = useApi<NetworkSignal>("/api/network/signal", { refreshInterval: 2000 });
  const { data: bat } = useApi<BatteryInfo>("/api/device/battery-info", { refreshInterval: 5000 });
  const { data: spd } = useApi<Speed>("/api/network/speed", { refreshInterval: 1000 });
  const { data: wifi } = useApi<WiFiStatus>("/api/wifi/status", { refreshInterval: 10000 });
  const { data: pub } = useApi<PublicStatus>("/api/public/status", { refreshInterval: 10000 });

  const isNR =
    (sig?.network_type || "").toUpperCase().includes("SA") || (sig?.nr5g_rsrp ?? 0) !== 0;
  const rsrp = isNR ? sig?.nr5g_rsrp : sig?.lte_rsrp;
  const rsrq = isNR ? sig?.nr5g_rsrq : sig?.lte_rsrq;
  const sinr = isNR ? sig?.nr5g_snr : sig?.lte_snr;

  const batPct = bat?.battery_capacity;
  const charging = bat?.battery_online === 1 && (bat?.battery_time_to_full ?? -1) >= 0;
  const link = rsrpTone(rsrp);
  const carrier = sig?.network_provider_fullname || sig?.network_provider;
  const wifiOn = wifi?.wifi_onoff === "1";

  return (
    <>
      <PageHeader title={t("dashboard.title", "Dashboard")} description={t("dashboard.desc", "Live status of your U60 Pro router.")} />

      {/* ── Hero: leads with plain language; raw figures stay for the owner ── */}
      <section className="admin-card p-5 sm:p-6">
        <div className="flex flex-col gap-5 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-4">
            <SignalBars rsrp={rsrp} tone={link.tone} />
            <div className="min-w-0">
              <div className="flex items-center gap-3">
                <Status tone={link.tone}>{t(link.labelKey, link.label)}</Status>
                <Freshness healthy={!sigErr} hasData={!!sig} />
              </div>
              <h3 className="mt-1.5 font-display text-xl font-semibold leading-tight tracking-tight text-text">
                {t(link.plainKey, link.plain)}
              </h3>
              <MetaRow
                className="mt-1.5"
                items={[
                  rsrp ? <span className="tabular-nums">{rsrp} dBm</span> : null,
                  carrier,
                  sig?.network_type,
                  sig?.nr5g_action_band && `Band ${sig.nr5g_action_band}`,
                ]}
              />
            </div>
          </div>
          <div className="grid grid-cols-3 gap-5 border-t border-border/60 pt-4 sm:border-t-0 sm:pt-0 sm:text-right">
            <Metric label="SINR" value={sinr || "—"} unit="dB" help={t("help.sinr", "Signal-to-noise — higher is cleaner. Above 13 dB is good.")} />
            <Metric label="RSRQ" value={fmtNum(rsrq)} unit="dB" help={t("help.rsrq", "Signal quality. Closer to 0 is better; below −15 is poor.")} />
            <Metric label="Bars" value={sig?.signalbar ?? "—"} unit="/5" help={t("help.bars", "The carrier's own 0–5 strength estimate.")} />
          </div>
        </div>
      </section>

      {/* ── Vitals: one flat strip, hairline-divided cells — no card-per-metric ── */}
      <div className="admin-card mt-4 overflow-hidden">
        <div className="grid grid-cols-2 gap-px bg-border sm:grid-cols-3 lg:grid-cols-6">
          <Vital label={t("dashboard.download", "Download")} value={fmtBps(spd?.rx_speed)} icon={ArrowDown} accent />
          <Vital label={t("dashboard.upload", "Upload")} value={fmtBps(spd?.tx_speed)} icon={ArrowUp} />
          <Vital
            label={t("dashboard.battery", "Battery")}
            value={batPct != null ? `${batPct}%` : "—"}
            hint={charging ? t("dashboard.charging", "Charging") : bat?.battery_online === 1 ? t("dashboard.pluggedIn", "Plugged in") : t("dashboard.onBattery", "On battery")}
          />
          <Vital label={t("dashboard.clients", "Clients")} value={wifi?.clients_total ?? "—"} hint={t("dashboard.connected", "Connected")} />
          <Vital label="Wi-Fi" value={wifiOn ? t("common.on", "On") : t("common.off", "Off")} hint={wifiOn ? wifi?.encryption_5g : t("common.disabled", "Disabled")} />
          <Vital label={t("dashboard.uptime", "Uptime")} value={fmtUptime(sys?.uptime)} hint={t("dashboard.sinceBoot", "Since boot")} />
        </div>
      </div>

      {/* ── Services (mirrors the pre-login overview) ── */}
      {pub?.services && (
        <SectionCard title={t("dashboard.servicesTitle", "Services")} description={t("dashboard.servicesDesc", "Background features running on the router.")} className="mt-4">
          <SvcRow label="Tailscale">
            {pub.services.tailscale.running ? (
              <Status tone="success"><span className="font-mono">{pub.services.tailscale.node || "up"}</span></Status>
            ) : pub.services.tailscale.installed ? (
              <Status tone="warning">{t("common.stopped", "Stopped")}</Status>
            ) : (
              <Status tone="neutral">{t("common.notInstalled", "Not installed")}</Status>
            )}
          </SvcRow>
          <SvcRow label="CHILL">
            {pub.services.chill.state === "running" ? (
              <Status tone="success">{t("common.running", "Running")}</Status>
            ) : pub.services.chill.state === "direct" ? (
              <Status tone="warning">{t("chill.stDirect", "Direct")}</Status>
            ) : (
              <Status tone="neutral">{t("chill.stUnknown", "Not started")}</Status>
            )}
          </SvcRow>
          <SvcRow label={t("nav.homeMode", "Home Mode")}>
            {!pub.services.home_mode.present ? (
              <Status tone="neutral">{t("common.notInstalled", "Not installed")}</Status>
            ) : !pub.services.home_mode.enabled ? (
              <Status tone="neutral">{t("dashboard.paused", "Paused")}</Status>
            ) : pub.services.home_mode.mode === "home" ? (
              <Status tone="warning">{t("dashboard.activeWifiOff", "Active · Wi-Fi off")}</Status>
            ) : (
              <Status tone="success">{t("dashboard.activeWifiOn", "Active · Wi-Fi on")}</Status>
            )}
          </SvcRow>
          <SvcRow label={t("dashboard.messages", "Messages")}>
            <Link href="/sms" className="underline-offset-2 hover:underline">
              {(pub.sms?.unread ?? 0) > 0 ? (
                <Status tone="warning">{t("dashboard.unread", "{{count}} unread", { count: pub.sms?.unread })}</Status>
              ) : (
                <Status tone="neutral">{t("common.none", "None")}</Status>
              )}
            </Link>
          </SvcRow>
        </SectionCard>
      )}

      {/* ── Detail — dual-layer: friendly framing + raw figures with inline help ── */}
      <div className="mt-6 grid gap-4 lg:grid-cols-2">
        <SectionCard title={t("dashboard.cellTitle", "Cell")} description={t("dashboard.cellDesc", "The mobile tower you're connected to.")}>
          <DescList
            items={[
              ["Cell ID", sig?.nr5g_cell_id, t("help.cellId", "Unique ID of the tower sector serving you.")],
              ["PCI", sig?.nr5g_pci, t("help.pci", "Physical Cell ID — tells nearby towers apart.")],
              ["EARFCN", sig?.nr5g_action_channel, t("help.earfcn", "The radio channel number your device is tuned to.")],
              ["Band", sig?.nr5g_action_band, t("help.band", "The frequency band currently in use.")],
              ["Bandwidth", sig?.nr5g_bandwidth ? `${sig.nr5g_bandwidth} MHz` : undefined, t("help.bandwidth", "Channel width — wider generally means faster.")],
              ["Net Select", sig?.net_select_mode, t("help.netSelect", "Whether the network is chosen automatically or manually.")],
            ]}
          />
        </SectionCard>

        <SectionCard title="Wi-Fi" description={t("dashboard.wifiDesc", "Your local wireless network.")}>
          <DescList
            items={[
              [t("dashboard.descState", "State"), wifiOn ? t("common.on", "On") : t("common.off", "Off")],
              ["2.4G SSID", wifi?.ssid_2g],
              ["2.4G Channel", wifi?.actual_channel_2g],
              ["5G SSID", wifi?.ssid_5g],
              ["5G Channel", wifi?.actual_channel_5g],
              ["5G Width", wifi?.actual_bw_5g, t("help.wifiWidth", "Wider channels (e.g. 80/160 MHz) are faster but shorter-range.")],
              [t("dashboard.descClients", "Clients"), wifi?.clients_total],
            ]}
          />
        </SectionCard>
      </div>
    </>
  );
}

function SignalBars({ rsrp, tone }: { rsrp?: number; tone: ToneKey }) {
  const lvl =
    rsrp == null || rsrp === 0
      ? 0
      : rsrp >= -85 ? 5 : rsrp >= -95 ? 4 : rsrp >= -105 ? 3 : rsrp >= -115 ? 2 : 1;
  const fill = {
    success: "bg-success",
    warning: "bg-warning",
    danger: "bg-error",
    neutral: "bg-text-dim",
    accent: "bg-accent",
  }[tone];
  return (
    <div className="flex h-12 items-end gap-1" aria-label={`Signal ${lvl} of 5`}>
      {[1, 2, 3, 4, 5].map((i) => (
        <span
          key={i}
          className={cn("w-2 rounded-[3px] transition-colors", i <= lvl ? fill : "bg-border")}
          style={{ height: `${i * 16 + 16}%` }}
        />
      ))}
    </div>
  );
}

/** Live/Reconnecting indicator — surfaces silent polling failure. */
function Freshness({ healthy, hasData }: { healthy: boolean; hasData: boolean }) {
  const { t } = useTranslation();
  if (!hasData) return null;
  return (
    <span className="inline-flex items-center gap-1.5 text-[10px] font-medium uppercase tracking-[0.1em] text-text-dim">
      <span
        className={cn(
          "inline-block h-1.5 w-1.5 rounded-full",
          healthy ? "animate-pulse bg-success" : "bg-warning"
        )}
      />
      {healthy ? t("dashboard.live", "Live") : t("dashboard.reconnecting", "Reconnecting")}
    </span>
  );
}

function Metric({ label, value, unit, help }: { label: string; value: string | number; unit?: string; help?: string }) {
  return (
    <div>
      <div className="text-[10px] font-medium uppercase tracking-[0.12em] text-text-dim">
        {label}
        {help && <Help text={help} />}
      </div>
      <div className="mt-1 font-display text-lg font-semibold tabular-nums text-text">
        {value}
        {unit && <span className="ml-0.5 text-xs font-medium text-text-dim">{unit}</span>}
      </div>
    </div>
  );
}

function Vital({
  label,
  value,
  unit,
  hint,
  icon: Icon,
  accent,
}: {
  label: string;
  value: string | number;
  unit?: string;
  hint?: string;
  icon?: React.ComponentType<{ size?: number; className?: string }>;
  accent?: boolean;
}) {
  return (
    <div className="bg-bg-card px-4 py-3.5">
      <div className="flex items-center gap-1 text-[10px] font-medium uppercase tracking-[0.12em] text-text-dim">
        {Icon && <Icon size={11} className={accent ? "text-accent" : undefined} />}
        {label}
      </div>
      <div className="mt-1 flex items-baseline gap-1">
        <span data-numeric className="font-display text-[19px] font-semibold leading-none tracking-tight text-text">
          {value}
        </span>
        {unit && <span className="text-[12px] font-medium text-text-dim">{unit}</span>}
      </div>
      {hint && <div className="mt-1 truncate text-[11px] text-text-dim">{hint}</div>}
    </div>
  );
}

function DescList({ items }: { items: Array<[string, string | number | null | undefined, string?]> }) {
  return (
    <dl className="divide-y divide-border/60">
      {items.map(([k, v, help]) => (
        <div key={k} className="flex items-center justify-between gap-3 py-2 text-[13px]">
          <dt className="flex items-center text-text-dim">
            {k}
            {help && <Help text={help} />}
          </dt>
          <dd data-numeric className="text-right font-mono text-[12.5px] text-text">
            {v == null || v === "" ? <span className="text-text-dim">—</span> : String(v)}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function SvcRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3 border-b border-border/50 py-2 text-sm last:border-0">
      <span className="text-text-dim">{label}</span>
      <span className="text-right font-medium text-text">{children}</span>
    </div>
  );
}

function fmtNum(n?: number, fallback = "—"): string | number {
  if (n == null || n === 0) return fallback;
  return n;
}

type ToneKey = "neutral" | "success" | "warning" | "danger" | "accent";

function rsrpTone(rsrp?: number): { tone: ToneKey; labelKey: string; label: string; plainKey: string; plain: string } {
  if (rsrp == null || rsrp === 0)
    return { tone: "neutral", labelKey: "dashboard.linkNoLink", label: "No link", plainKey: "dashboard.plainNone", plain: "No signal — check the SIM or coverage" };
  if (rsrp >= -85)
    return { tone: "success", labelKey: "dashboard.linkExcellent", label: "Excellent", plainKey: "dashboard.plainExcellent", plain: "Strong, stable connection" };
  if (rsrp >= -100)
    return { tone: "success", labelKey: "dashboard.linkGood", label: "Good", plainKey: "dashboard.plainGood", plain: "Solid connection" };
  if (rsrp >= -110)
    return { tone: "warning", labelKey: "dashboard.linkFair", label: "Fair", plainKey: "dashboard.plainFair", plain: "Usable, but the signal is weak" };
  return { tone: "danger", labelKey: "dashboard.linkPoor", label: "Poor", plainKey: "dashboard.plainPoor", plain: "Weak signal — try repositioning the router" };
}

/** rx_speed/tx_speed are bytes/sec from zte-agent; convert to bits/sec. */
function fmtBps(bytesPerSec?: number): string {
  if (bytesPerSec == null || bytesPerSec < 0) return "0 b/s";
  const bps = bytesPerSec * 8;
  if (bps < 1000) return `${bps.toFixed(0)} b/s`;
  if (bps < 1_000_000) return `${(bps / 1000).toFixed(1)} kb/s`;
  if (bps < 1_000_000_000) return `${(bps / 1_000_000).toFixed(2)} Mb/s`;
  return `${(bps / 1_000_000_000).toFixed(2)} Gb/s`;
}

function fmtUptime(secs?: number): string {
  if (!secs) return "—";
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m`;
}
