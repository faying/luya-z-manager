"use client";

import { useApi } from "@/lib/hooks/useApi";
import { deviceNow, useDeviceOffset } from "@/lib/deviceClock";
import { PageHeader, SectionCard, ErrorBanner, Status } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { useSWRConfig } from "swr";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { RefreshCw, MonitorSmartphone, Wifi } from "lucide-react";

interface DhcpLease {
  ipaddr?: string;
  macaddr?: string;
  hostname?: string;
  expires?: number;
  duid?: string;
}

interface ClientsResp {
  hosts?: Record<string, string>;
  dhcp_leases?: DhcpLease[];
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
      {healthy ? t("clients.live", "Live") : t("clients.reconnecting", "Reconnecting")}
    </span>
  );
}

// Lease expiry comes from dnsmasq, on the device clock (lib/deviceClock.ts).
function fmtLease(expires: number | undefined, now: number): string {
  if (!expires) return "—";
  const secs = expires - now;
  if (secs <= 0) return "Expired";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return h ? `${h}h ${m}m` : `${m}m`;
}

export default function ClientsPage() {
  const { t } = useTranslation();
  const { mutate } = useSWRConfig();
  const [showExpired, setShowExpired] = useState(false);
  const { data, error, isLoading } = useApi<ClientsResp>("/api/network/clients", {
    refreshInterval: 5000,
  });
  const refresh = () => mutate("/api/network/clients");

  const leases = data?.dhcp_leases ?? [];
  const hosts = data?.hosts ?? {};
  const now = deviceNow(useDeviceOffset());
  const activeLeases = leases.filter((l) => (l.expires ?? 0) > now);
  const expiredLeases = leases.filter((l) => (l.expires ?? 0) <= now);
  const active = activeLeases.length;
  const shown = showExpired ? leases : activeLeases;

  return (
    <>
      <PageHeader
        title={t("clients.title", "Clients")}
        description={t("clients.desc", "Devices connected to your router.")}
        actions={
          <Button variant="outline" size="sm" onClick={refresh}>
            <RefreshCw size={13} /> {t("common.refresh", "Refresh")}
          </Button>
        }
      />

      {error && (
        <div className="mb-4">
          <ErrorBanner message={String(error.message ?? error)} onRetry={refresh} />
        </div>
      )}

      <SectionCard
        title={t("clients.connectedDevices", "Connected devices")}
        description={leases.length ? t("clients.summary", "{{total}} total · {{active}} active leases", { total: leases.length, active }) : undefined}
        actions={
          <div className="flex items-center gap-3">
            <Freshness healthy={!error} hasData={!!data} />
            {expiredLeases.length > 0 && (
              <Button variant="ghost" size="sm" onClick={() => setShowExpired((v) => !v)}>
                {showExpired
                  ? t("clients.hideExpired", "Hide expired ({{count}})", { count: expiredLeases.length })
                  : t("clients.showExpired", "Show expired ({{count}})", { count: expiredLeases.length })}
              </Button>
            )}
          </div>
        }
      >
        {isLoading && leases.length === 0 ? (
          <div className="py-10 text-center text-sm text-text-dim">{t("common.loading", "Loading…")}</div>
        ) : leases.length === 0 ? (
          <div className="flex flex-col items-center gap-3 py-12 text-center">
            <span className="flex h-12 w-12 items-center justify-center rounded-full bg-accent-soft text-accent">
              <Wifi size={22} />
            </span>
            <div className="space-y-1">
              <p className="text-sm font-medium text-text">{t("clients.emptyTitle", "No devices yet")}</p>
              <p className="mx-auto max-w-[34ch] text-[13px] leading-relaxed text-text-dim">
                {t("clients.emptyDesc", "Devices show up here once they connect over Wi-Fi or the LAN port.")}
              </p>
            </div>
          </div>
        ) : (
          <div className="-my-1">
            {shown.map((lease, i) => {
              const mac = lease.macaddr ?? "";
              const hostname = lease.hostname || hosts[mac] || t("clients.unknownDevice", "Unknown device");
              const isActive = (lease.expires ?? 0) > now;
              return (
                <div
                  key={lease.ipaddr || mac || i}
                  className={cn(
                    "flex items-center gap-3 border-b border-border/50 py-3 last:border-0",
                    !isActive && "opacity-60"
                  )}
                >
                  <span
                    className={cn(
                      "flex h-9 w-9 shrink-0 items-center justify-center rounded-full",
                      isActive ? "bg-accent-soft text-accent" : "bg-bg-input text-text-dim"
                    )}
                  >
                    <MonitorSmartphone size={17} />
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-medium text-text">{hostname}</div>
                    <div className="truncate font-mono text-xs text-text-dim">
                      {lease.ipaddr ?? "—"}
                      {mac && <span className="opacity-60"> · {mac}</span>}
                    </div>
                  </div>
                  <div className="shrink-0 text-right">
                    {isActive ? (
                      <Status tone="success">{fmtLease(lease.expires, now)}</Status>
                    ) : (
                      <span className="text-xs text-text-dim">{t("clients.expired", "Expired")}</span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </SectionCard>
    </>
  );
}
