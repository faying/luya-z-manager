"use client";
// Home: the readout wall (design doc §5). Reading order is the same on
// every width — title → status → services → readouts → detail → carriers →
// scenario, Tailscale — and on ≥1024 it splits 60/40 with the service
// modules, scenario and Tailscale in the right column. Polling stays within 2.10 req/s:
//
//   speed 1 s · signal 2 s · battery, wifi, public status, proxy 10 s ·
//   thermal, tailscale 15 s · system, data usage 30 s
import Link from "next/link";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { mutate as globalMutate } from "swr";
import { Bell, ChatCircleText, type Icon } from "@phosphor-icons/react";
import { useApi, type UseApiResponse } from "@/lib/hooks/useApi";
import { tailscaleValid } from "@/lib/api/freshness";
import type { NetworkSignal, NetworkSpeed, DataUsage, NetInfo, NetInfoExit, NetInfoOperator } from "@/lib/api/schemas/network";
import type { BatteryInfo, DeviceSystem } from "@/lib/api/schemas/device";
import type { TailscaleStatus } from "@/lib/api/schemas/services";
import type { PublicStatus } from "@/lib/api/schemas/public";
import { Group, Help, ModuleCard, Readout, ReadoutWall, Row, StatusMark } from "@/components/nd";
import { selectionWord, bytes, carrierCounts, carriers, cpuTempC, mbps, servingCellId, sigState, totalBandwidth, uptimeParts, type Carrier } from "@/lib/home";
import { fmtDevice } from "@/lib/deviceClock";
import { useMedia } from "@/lib/useMedia";
import { carrierSummary, rsrpWord, sinrWord, type T } from "@/lib/signalWords";
import { SignalStatus } from "@/components/signal/SignalStatus";
import { useDeviceLabel } from "@/lib/publicStatus";

interface WifiStatus {
  wifi_onoff?: string;
  clients_total?: number;
  ssid_2g?: string;
  ssid_5g?: string;
  actual_channel_2g?: string;
  actual_channel_5g?: string;
  actual_bw_2g?: string;
  actual_bw_5g?: string;
  encryption_2g?: string;
  encryption_5g?: string;
}

export default function HomePage() {
  const { t } = useTranslation();
  const spd = useApi<NetworkSpeed>("/api/network/speed", { refreshInterval: 1000 });
  const sig = useApi<NetworkSignal>("/api/network/signal", { refreshInterval: 2000 });
  const bat = useApi<BatteryInfo>("/api/device/battery-info", { refreshInterval: 10000 });
  const wifi = useApi<WifiStatus>("/api/wifi/status", { refreshInterval: 10000 });
  const pub = useApi<PublicStatus>("/api/public/status", { refreshInterval: 10000 });
  const thermal = useApi<Record<string, unknown>>("/api/device/thermal", { refreshInterval: 15000 });
  const ts = useApi<TailscaleStatus>("/api/services/tailscale", { refreshInterval: 15000, isValid: tailscaleValid });
  const sys = useApi<DeviceSystem>("/api/device/system", { refreshInterval: 30000 });
  const usage = useApi<DataUsage>("/api/data-usage", { refreshInterval: 30000 });
  // lite: the agent does not keep reading the selection mode (an AT command) for this card.
  const ni = useApi<NetInfo>("/api/netinfo?lite=1", { refreshInterval: 30000 });

  const s = sig.data;
  const cs = useMemo(() => carriers(s), [s]);
  const bw = totalBandwidth(cs);
  const cc = carrierCounts(cs);
  const serving = cs[0];
  const bars = s?.signalbar != null && s.signalbar !== "" ? Number(s.signalbar) : null;
  const state = sigState({
    everValid: sig.lastOkAt != null,
    valid: !sig.stale,
    bars,
    sinr: serving?.sinr ?? null,
  });
  const allDown = sig.stale && spd.stale && pub.stale;

  const retry = () => globalMutate(() => true);
  // Each module is mounted once (it owns a write op and toasts); only its
  // column changes with the width.
  const wide = useMedia("(min-width: 1024px)");
  const side = (
    <>
      <ScenarioModule pub={pub.data} stale={pub.stale} />
      <TailscaleModule ts={ts} />
    </>
  );

  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] lg:gap-6">
      <div className="grid content-start gap-4 lg:col-span-2">
        <HomeHeader pub={pub.data} />
      </div>

      {/* Left column (and the whole page on phones, in reading order). */}
      <div className="grid content-start gap-4">
        <SignalStatus
          state={state}
          allDown={allDown}
          sig={s}
          serving={serving}
          counts={cc}
          bars={bars}
          lastOkAt={sig.lastOkAt}
          speedStale={spd.stale}
          onRetry={retry}
        />
        <ReadoutWall label={t("home.readouts", "Readings")}>
          <Readout
            wide
            hero
            label={t("home.aggBw", "Aggregate bandwidth · MHz")}
            value={state === "loading" ? undefined : bw}
            unit="MHz"
            sub={bw != null ? carrierSummary(t, cc) : t("home.noCarriers", "No active carriers")}
            stale={sig.stale}
          />
          <Readout
            label={<>RSRP <Help label="RSRP" text={t("help.rsrp", "Received signal power. Closer to 0 is stronger; above −100 dBm is good.")} /></>}
            value={state === "loading" ? undefined : serving?.rsrp ?? null}
            unit="dBm"
            sub={serving?.rsrq != null ? `RSRQ ${serving.rsrq} · ${rsrpWord(t, serving.rsrp)}` : rsrpWord(t, serving?.rsrp ?? null)}
            stale={sig.stale}
          />
          <Readout
            label={<>SINR <Help label="SINR" text={t("help.sinr", "Signal-to-noise — higher is cleaner. Above 13 dB is good.")} /></>}
            value={state === "loading" ? undefined : serving?.sinr ?? null}
            unit="dB"
            sub={sinrWord(t, serving?.sinr ?? null)}
            stale={sig.stale}
          />
          <Readout label={t("home.down", "Download")} value={spd.data ? mbps(spd.data.rx_speed) : undefined} unit="Mbps" sub="↓" stale={spd.stale} />
          <Readout label={t("home.up", "Upload")} value={spd.data ? mbps(spd.data.tx_speed) : undefined} unit="Mbps" sub="↑" stale={spd.stale} />
          <Readout
            label={t("home.temp", "Temperature")}
            value={thermal.data ? cpuTempC(thermal.data) : thermal.error ? null : undefined}
            unit="°C"
            sub={tempWord(t, thermal.data ? cpuTempC(thermal.data) : null)}
            stale={thermal.stale}
          />
          <Readout
            label={t("home.battery", "Battery")}
            value={bat.data ? bat.data.battery_capacity ?? null : bat.error ? null : undefined}
            unit="%"
            sub={batteryWord(t, bat.data)}
            stale={bat.stale}
          />
        </ReadoutWall>

        <Group title={t("home.detail", "Detail")} stale={wifi.stale || usage.stale || sys.stale}>
          <Row label={t("home.clients", "Connected devices")} value={wifi.data?.clients_total ?? "—"} href="/clients" />
          <Row label={t("home.today", "Today")} value={usageText(usage.data?.day)} />
          <Row label={t("home.month", "This month")} value={usageText(usage.data?.month)} />
          <Row label={t("home.uptime", "Uptime")} value={uptimeText(t, sys.data?.uptime)} />
        </Group>

        <NetIdentityGroup ni={ni.data} stale={ni.stale} />

        <WifiGroup wifi={wifi.data} stale={wifi.stale} />

        <CarrierTable cs={cs} stale={sig.stale} netSelect={selectionWord(s?.net_select_mode, t)} cellId={servingCellId(s, cs[0])} />

        {!wide && side}
      </div>

      {/* Right column, ≥1024 only. */}
      {wide && (
        <div className="grid content-start gap-4">
          {side}
        </div>
      )}
    </div>
  );
}

// ── header ────────────────────────────────────────────────────────────

function HomeHeader({ pub }: { pub: PublicStatus | undefined }) {
  const { t } = useTranslation();
  const sms = pub?.sms?.unread ?? 0;
  const alerts = pub?.alerts?.unread ?? 0;
  const sub = pub?.network?.operator || pub?.wifi?.ssid || "";
  const deviceLabel = useDeviceLabel();
  return (
    <header className="flex items-start gap-3 pt-2">
      <div className="min-w-0 flex-1">
        <h1 className="nd-product">{deviceLabel}</h1>
        {sub && <p className="nd-aux mt-0.5">{sub}</p>}
      </div>
      <CountButton href="/sms" icon={ChatCircleText} n={sms} label={sms > 0 ? t("home.smsUnread", "{{n}} unread SMS", { n: sms }) : t("home.sms", "SMS")} />
      <CountButton href="/alerts" icon={Bell} n={alerts} label={alerts > 0 ? t("home.alertsUnread", "{{n}} new alerts", { n: alerts }) : t("home.alerts", "Alerts")} />
    </header>
  );
}

function CountButton({ href, icon: I, n, label }: { href: string; icon: Icon; n: number; label: string }) {
  return (
    <Link href={href} aria-label={label} title={label} className="relative inline-flex h-11 w-11 items-center justify-center rounded-full bg-nd-card shadow-[inset_0_0_0_1px_var(--nd-hair)] text-nd-t2 hover:text-nd-t1">
      <I size={20} weight={n > 0 ? "fill" : "bold"} aria-hidden />
      {n > 0 && (
        <span className="absolute -right-0.5 -top-0.5 flex h-5 min-w-5 items-center justify-center rounded-full bg-nd-primary px-1 text-[11px] font-semibold leading-none text-nd-onPrimary">
          {n > 99 ? "99+" : n}
        </span>
      )}
    </Link>
  );
}


// ── scenario, Tailscale ───────────────────────────────────────────────

function ScenarioModule({ pub, stale }: { pub: PublicStatus | undefined; stale: boolean }) {
  const { t } = useTranslation();
  const sc = pub?.scenario;
  const hm = pub?.services?.home_mode;
  return (
    <ModuleCard className="nd-module--cream" href="/router/scenario" title={t("nav.scenario", "Scenarios")} stale={stale}>
      {!pub ? (
        <span className="nd-skel" style={{ width: "10ch" }} />
      ) : !sc?.configured ? (
        <p className="nd-body text-nd-t2">{t("home.noScenario", "No scenarios yet · set up ›")}</p>
      ) : (
        <>
          <p className="nd-body">
            {sc.name || sc.current || "—"}
            {sc.pin && <span className="nd-aux"> · {t("home.pinned", "pinned")}</span>}
            {!sc.enabled && <span className="nd-aux"> · {t("home.engineOff", "engine off")}</span>}
          </p>
          {sc.last_switch != null && (
            <p className="nd-aux">{t("home.lastSwitch", "Switched {{time}}", { time: fmtDevice(sc.last_switch, "time") })}</p>
          )}
        </>
      )}
      {hm?.present && (
        <p className="nd-aux mt-1">
          {t("nav.homeMode", "Home Mode")} ·{" "}
          {!hm.enabled
            ? t("dashboard.paused", "Paused")
            : hm.mode === "home"
              ? t("dashboard.activeWifiOff", "Active · Wi-Fi off")
              : t("dashboard.activeWifiOn", "Active · Wi-Fi on")}
        </p>
      )}
    </ModuleCard>
  );
}

function TailscaleModule({ ts }: { ts: UseApiResponse<TailscaleStatus> }) {
  const { t } = useTranslation();
  const d = ts.data;
  let body: React.ReactNode;
  if (!d && !ts.error) body = <span className="nd-skel" style={{ width: "10ch" }} />;
  else if (!d) body = <p className="nd-body text-nd-t2">{t("home.tsUnreadable", "Can't read Tailscale status")}</p>;
  else if (!d.installed) body = <p className="nd-body text-nd-t2">{t("nd.notInstalled", "Not installed")}</p>;
  else if (!d.running) body = <StatusMark tone="warn">{t("home.tsStopped", "Not running · open ›")}</StatusMark>;
  else {
    const online = d.peer_online ?? d.peers?.filter((p) => p.online).length ?? 0;
    const capped = (d.peers?.length ?? 0) >= 32 && (d.peer_count ?? 0) > 32;
    body = (
      <>
        <StatusMark tone={d.self?.online ? "ok" : "warn"}>{d.self?.online ? t("nd.online", "Online") : t("home.tsOffline", "Offline")}</StatusMark>
        <p className="nd-aux mt-1">
          {t("home.tsPeers", "{{n}} devices online", { n: capped ? "32+" : online })}
          {d.self?.relay && <> · {t("home.tsRelay", "relay {{r}}", { r: d.self.relay })}</>}
        </p>
      </>
    );
  }
  return (
    <ModuleCard className="nd-module--navy" href="/services/tailscale" title="Tailscale" stale={ts.stale}>
      {body}
      {ts.invalidReason && <p className="nd-aux mt-1">{ts.invalidReason}</p>}
    </ModuleCard>
  );
}

// ── network identity ──────────────────────────────────────────────────

// Two exits when the proxy runs and they differ, one otherwise (design D1).
function NetIdentityGroup({ ni, stale }: { ni: NetInfo | undefined; stale: boolean }) {
  const { t } = useTranslation();
  const d = ni?.direct ?? null;
  const p = ni?.proxy ?? null;
  const two = !!p && p.ip !== d?.ip;
  const exitSub = (e: NetInfoExit | null, extra: (string | null | undefined)[]) => {
    if (!e) return ni ? t("home.lookingUp", "Looking up…") : undefined;
    if (!e.ip && e.error) return t("home.lookupFailed", "Lookup failed: {{e}}", { e: e.error });
    const parts = [e.geo, ...extra].filter(Boolean).join(" · ");
    return e.error ? `${parts}${parts ? " · " : ""}${t("home.lookupStale", "refresh failed")}` : parts || undefined;
  };
  const op = (o: NetInfoOperator | null | undefined) => {
    if (!o || (!o.name && !o.mcc)) return "—";
    const plmn = o.mcc && o.mnc ? `${o.mcc}${o.mnc}` : "";
    const where = o.country && o.country !== "中国" ? `（${o.country}）` : " ";
    return `${o.name ?? "?"}${where}${plmn}`.trim();
  };
  return (
    <Group title={t("home.netId", "Network identity")} stale={stale}>
      <Row
        label={two ? t("home.exitCell", "Cellular exit") : t("home.exitIp", "Public IP")}
        value={d?.ip ?? "—"}
        mono
        sub={exitSub(d, [d?.isp])}
      />
      <Row label={t("home.simOperator", "SIM operator")} value={op(ni?.home_operator)} />
      <Row label={t("home.servingOperator", "Registered on")} value={op(ni?.serving_operator)} href="/router/mobile-network" />
      <Row
        label={t("home.roaming", "Roaming")}
        value={ni?.roaming == null ? "—" : ni.roaming ? <StatusMark tone="warn">{t("home.roamingYes", "Roaming")}</StatusMark> : t("home.roamingNo", "Home")}
      />
    </Group>
  );
}

// ── Wi-Fi and carriers ────────────────────────────────────────────────

function WifiGroup({ wifi, stale }: { wifi: WifiStatus | undefined; stale: boolean }) {
  const { t } = useTranslation();
  const on = wifi?.wifi_onoff === "1";
  const mhz = (bw?: string) => (bw ? (/mhz/i.test(bw) ? bw : `${bw} MHz`) : null);
  const line = (ssid?: string, ch?: string, bw?: string, enc?: string) =>
    [ssid, ch && t("home.channel", "ch {{c}}", { c: ch }), mhz(bw), enc].filter(Boolean).join(" · ") || "—";
  return (
    <Group title="Wi-Fi" stale={stale}>
      <Row label={t("home.wifiState", "Wi-Fi")} value={wifi ? (on ? t("nd.on", "On") : t("nd.off", "Off")) : "—"} href="/router/wifi" />
      <Row label="2.4 GHz" sub={line(wifi?.ssid_2g, wifi?.actual_channel_2g, wifi?.actual_bw_2g, wifi?.encryption_2g)} />
      <Row label="5 GHz" sub={line(wifi?.ssid_5g, wifi?.actual_channel_5g, wifi?.actual_bw_5g, wifi?.encryption_5g)} />
    </Group>
  );
}

function CarrierTable({ cs, stale, netSelect, cellId }: { cs: Carrier[]; stale: boolean; netSelect?: string; cellId?: number }) {
  const { t } = useTranslation();
  const active = cs.filter((c) => c.active);
  const inactive = cs.filter((c) => !c.active);
  return (
    <section aria-labelledby="carriers-title">
      <h2 id="carriers-title" className="nd-group-title">{t("home.carriers", "Carriers")}</h2>
      <div className={`nd-group${stale ? " nd-stale" : ""}`}>
        {active.length === 0 ? (
          <Row label={t("home.noCarriers", "No active carriers")} href="/signal" />
        ) : (
          active.map((c, i) => (
            <div key={i} className="nd-row flex-wrap gap-y-1">
              <span className="nd-row__label w-16 shrink-0">{c.kind === "nr" ? `n${c.band}` : `B${c.band}`}</span>
              <span className="nd-row__value flex-1 whitespace-nowrap text-start">
                {[c.bw != null && `${c.bw} MHz`, c.rsrp != null && `${c.rsrp} dBm`, c.sinr != null && `SINR ${c.sinr}`].filter(Boolean).join(" · ")}
              </span>
              <span className="nd-mono nd-aux w-full min-w-0 xl:w-auto">
                PCI {c.pci ?? "—"} · ARFCN {c.arfcn ?? "—"}
                {c.serving && cellId ? ` · Cell ${cellId}` : ""}
                {c.serving && <> · {t("home.serving", "serving")}</>}
              </span>
            </div>
          ))
        )}
        {inactive.length > 0 && (
          <div className="nd-row nd-aux">
            {t("home.inactive", "Not scheduled")}: {inactive.map((c) => (c.kind === "nr" ? `n${c.band}` : `B${c.band}`)).join(" · ")}
          </div>
        )}
        {netSelect && <Row label={t("home.netSelect", "Network selection")} value={netSelect} />}
      </div>
    </section>
  );
}

// ── words ─────────────────────────────────────────────────────────────

function tempWord(t: T, c: number | null) {
  if (c == null) return "";
  if (c >= 80) return t("home.hot", "Hot");
  if (c >= 65) return t("home.warm", "Warm");
  return t("home.normal", "Normal");
}

function batteryWord(t: T, b: BatteryInfo | undefined) {
  if (!b) return "";
  const charging = b.battery_online === 1 && (b.battery_time_to_full ?? -1) >= 0;
  if (charging) return t("dashboard.charging", "Charging");
  if (b.battery_online === 1) return t("dashboard.pluggedIn", "Plugged in");
  return t("dashboard.onBattery", "On battery");
}

function usageText(p: { rx_bytes: number | string | null; tx_bytes: number | string | null } | undefined) {
  if (!p) return "—";
  const rx = bytes(p.rx_bytes);
  const tx = bytes(p.tx_bytes);
  if (!rx && !tx) return "—";
  return `↓ ${rx ?? "—"} · ↑ ${tx ?? "—"}`;
}

function uptimeText(t: T, secs?: number) {
  const u = uptimeParts(secs);
  if (!u) return "—";
  if (u.d) return t("home.uptimeDH", "{{d}} d {{h}} h", { d: u.d, h: u.h });
  if (u.h) return t("home.uptimeHM", "{{h}} h {{m}} min", { h: u.h, m: u.m });
  return t("home.uptimeM", "{{m}} min", { m: u.m });
}
