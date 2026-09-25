"use client";
// Clients (list page, design doc §5.1): status first (count · live /
// "list stopped at hh:mm" · retry), then the lease list, expired leases
// behind a toggle. Read-only.
//
// /api/network/clients is two luci-rpc calls; when getDHCPLeases fails the
// agent still answers ok and just omits `dhcp_leases` (network_ext.rs:28-32).
// That is treated as a failure (R9) so it can't read as "no devices".
// `hosts` is getHostHints: MAC → {ipaddrs, ip6addrs, name} (not a string).
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, DeviceMobile, WifiHigh } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import type { ValidResult } from "@/lib/api/freshness";
import type { DhcpLease, HostHint, NetworkClients, WifiStation } from "@/lib/api/schemas/network";
import { deviceNow, useDeviceOffset } from "@/lib/deviceClock";
import { Button, Freshness, Group, Row, StatusBlock, StatusMark, type Tone } from "@/components/nd";

const PATH = "/api/network/clients";

function clientsValid(d: NetworkClients | null | undefined): ValidResult {
  if (!d || !Array.isArray(d.dhcp_leases)) return { ok: false, reason: "no DHCP lease table" };
  return true;
}

/** Hint keys' MAC casing vs the lease's is unconfirmed: try all three. */
function hintFor(hosts: Record<string, HostHint> | null | undefined, mac: string): HostHint | undefined {
  if (!hosts || !mac) return undefined;
  return hosts[mac] ?? hosts[mac.toLowerCase()] ?? hosts[mac.toUpperCase()];
}

export default function ClientsPage() {
  const { t } = useTranslation();
  const [showExpired, setShowExpired] = useState(false);
  const cl = useApi<NetworkClients>(PATH, { refreshInterval: 5000, isValid: clientsValid });
  const now = deviceNow(useDeviceOffset());
  const refresh = () => void cl.mutate();

  const d = cl.data;
  const leases = d?.dhcp_leases ?? [];
  // Lease expiry is on the device clock (lib/deviceClock.ts).
  const active = leases.filter((l) => (l.expires ?? 0) > now);
  const expired = leases.filter((l) => (l.expires ?? 0) <= now);
  const shown = showExpired ? [...active, ...expired] : active;

  const loading = !d && !cl.error;
  const failedNoData = !d && !!cl.error;

  let tone: Tone;
  let state: string;
  if (loading) {
    tone = "neutral";
    state = t("clients.loading", "Reading the device list…");
  } else if (failedNoData) {
    tone = "bad";
    state = t("clients.loadFailed", "Couldn't read the device list");
  } else {
    tone = cl.stale ? "stale" : "ok";
    state = leases.length
      ? t("clients.summary", "{{total}} total · {{active}} active leases", { total: leases.length, active: active.length })
      : t("clients.emptyNow", "No devices connected right now");
  }

  const reason = cl.invalidReason
    ? t("clients.noLeaseTable", "The device didn't return its DHCP lease table.")
    : cl.error
      ? String(cl.error.message ?? cl.error)
      : undefined;

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">{t("clients.title", "Clients")}</h1>
        <Button variant="ghost" iconOnly onPress={refresh} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason && (cl.stale || failedNoData) ? <span role="alert">{reason}</span> : undefined}
          meta={
            d && !cl.stale ? (
              <StatusMark tone="ok">{t("clients.live", "Live")}</StatusMark>
            ) : d && cl.stale ? (
              <>
                <StatusMark tone="warn">{t("clients.reconnecting", "Reconnecting")}</StatusMark>{" "}
                <Freshness stale lastOkAt={cl.lastOkAt} what={t("clients.listWhat", "List")} />
              </>
            ) : undefined
          }
          actions={
            (cl.error || cl.stale) && (
              <Button variant="secondary" size="sm" onPress={refresh}>
                {t("common.retry", "Retry")}
              </Button>
            )
          }
        />

        <section>
          <h2 className="nd-group-title">{t("clients.connectedDevices", "Connected devices")}</h2>
          {loading ? (
            <div className="nd-group" aria-busy="true">
              {[0, 1, 2].map((i) => (
                <Row key={i} label={<span className="nd-skel" style={{ width: "10ch" }} />} sub={<span className="nd-skel" style={{ width: "18ch" }} />} />
              ))}
            </div>
          ) : failedNoData ? (
            <div className="nd-group">
              <Row label={t("clients.loadFailed", "Couldn't read the device list")} sub={t("clients.retryHint", "Check the connection, then press Retry above.")} />
            </div>
          ) : leases.length === 0 ? (
            <Group stale={cl.stale}>
              <Row label={t("clients.emptyTitle", "No devices yet")} sub={t("clients.emptyDesc", "Devices show up here once they connect over Wi-Fi or the LAN port.")} />
              <Row icon={WifiHigh} label={t("clients.wifiOn", "Is Wi-Fi on?")} href="/router/wifi" />
            </Group>
          ) : (
            <>
              <Group stale={cl.stale}>
                {shown.length === 0 ? (
                  <Row label={t("clients.noActive", "No active leases")} sub={t("clients.noActiveNext", "Only expired leases are left; show them below.")} />
                ) : (
                  shown.map((l, i) => (
                    <LeaseRow key={l.ipaddr || l.macaddr || i} lease={l} hosts={d?.hosts} wifi={d?.wifi} now={now} />
                  ))
                )}
              </Group>
              {expired.length > 0 && (
                <div className="mt-2">
                  <Button variant="ghost" onPress={() => setShowExpired((v) => !v)} aria-expanded={showExpired}>
                    {showExpired
                      ? t("clients.hideExpired", "Hide expired ({{count}})", { count: expired.length })
                      : t("clients.showExpired", "Show expired ({{count}})", { count: expired.length })}
                  </Button>
                </div>
              )}
            </>
          )}
        </section>
      </div>
    </>
  );
}

/** Wi-Fi signal in words (RSSI at the AP). */
function wifiSignalWord(dbm: number, t: (k: string, d: string) => string): string {
  if (dbm >= -55) return t("clients.sigGreat", "Strong signal");
  if (dbm >= -67) return t("clients.sigGood", "Good signal");
  if (dbm >= -75) return t("clients.sigFair", "Fair signal");
  return t("clients.sigWeak", "Weak signal");
}

/** "5 GHz · Wi-Fi 6 · 2402 Mbps · Strong signal"; null when the device isn't on Wi-Fi. */
function wifiLine(st: WifiStation | undefined, t: (k: string, d: string, o?: Record<string, unknown>) => string): string | null {
  if (!st) return null;
  const parts: string[] = [];
  if (st.band) parts.push(st.band);
  if (st.wifi_gen) parts.push(`Wi-Fi ${st.wifi_gen}`);
  if (st.link_down_mbps) parts.push(t("clients.link", "{{n}} Mbps", { n: st.link_down_mbps }));
  if (st.signal != null) parts.push(wifiSignalWord(st.signal, t));
  return parts.join(" · ") || t("clients.onWifi", "On Wi-Fi");
}

function LeaseRow({
  lease,
  hosts,
  wifi,
  now,
}: {
  lease: DhcpLease;
  hosts: Record<string, HostHint> | null | undefined;
  wifi: WifiStation[] | undefined;
  now: number;
}) {
  const { t } = useTranslation();
  const mac = lease.macaddr ?? "";
  const name = lease.hostname || hintFor(hosts, mac)?.name || t("clients.unknownDevice", "Unknown device");
  const isActive = (lease.expires ?? 0) > now;
  const st = mac ? wifi?.find((w) => w.mac.toLowerCase() === mac.toLowerCase()) : undefined;
  const line = wifiLine(st, t);
  return (
    <Row
      icon={DeviceMobile}
      label={name}
      sub={
        <>
          {line ? (
            <span className="block" title={st?.signal != null ? `${st.signal} dBm · ${t("clients.channel", "channel {{c}}", { c: st.channel ?? "—" })}` : undefined}>
              {line}
            </span>
          ) : (
            isActive &&
            wifi && <span className="block">{t("clients.notOnWifi", "Not on Wi-Fi (cable, USB, or left)")}</span>
          )}
          <span className="nd-mono">
            {lease.ipaddr ?? "—"}
            {mac && <> · {mac}</>}
          </span>
        </>
      }
      value={
        isActive ? (
          <span title={t("clients.leaseLeft", "Lease left")}>{fmtLease(lease.expires, now, t)}</span>
        ) : (
          <StatusMark tone="neutral">{t("clients.expired", "Expired")}</StatusMark>
        )
      }
    />
  );
}

function fmtLease(expires: number | undefined, now: number, t: (k: string, d: string, o?: Record<string, unknown>) => string): string {
  if (!expires) return "—";
  const secs = expires - now;
  if (secs <= 0) return t("clients.expired", "Expired");
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return h ? t("clients.leaseHM", "{{h}}h {{m}}m", { h, m }) : t("clients.leaseM", "{{m}}m", { m });
}
