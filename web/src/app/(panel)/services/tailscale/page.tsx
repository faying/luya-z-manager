"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCw, Network } from "lucide-react";
import { useApi } from "@/lib/hooks/useApi";
import {
  PageHeader,
  SectionCard,
  Status,
  MetaRow,
  ErrorBanner,
} from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { Help } from "@/components/admin/Help";

interface TailscalePeer {
  id?: string;
  hostname?: string;
  dns_name?: string;
  os?: string;
  ips?: string[];
  online?: boolean;
  exit_node?: boolean;
  rx_bytes?: number | null;
  tx_bytes?: number | null;
  last_seen?: string | null;
  last_handshake?: string | null;
}

interface TailscaleStatus {
  installed: boolean;
  running: boolean;
  backend_state?: string;
  version?: string;
  auth_url?: string | null;
  error?: string;
  self?: {
    hostname?: string;
    dns_name?: string;
    ips?: string[];
    online?: boolean;
    relay?: string;
    exit_node_option?: boolean;
  };
  exit_node?: {
    hostname?: string;
    ips?: string[];
    online?: boolean;
  } | null;
  peer_count?: number;
  peer_online?: number;
  peers?: TailscalePeer[];
}

interface LogResp {
  path: string;
  lines: string[];
  limit: number;
}

const REFRESH_INTERVAL = 5000;
const LOG_LINES = 200;

export default function TailscalePage() {
  const { t } = useTranslation();
  const { data: status, error, mutate, isLoading } = useApi<TailscaleStatus>(
    "/api/services/tailscale",
    { refreshInterval: REFRESH_INTERVAL }
  );

  const tone = backendStateTone(status);

  return (
    <>
      <PageHeader
        title="Tailscale"
        description={t("ts.desc", "Mesh VPN running on the router. Read-only — manage peers from the Tailscale admin console.")}
        actions={
          <div className="flex items-center gap-3">
            <Status tone={tone.tone}>{tone.labelKey ? t(tone.labelKey, tone.label) : tone.label}</Status>
            <Button
              variant="outline"
              size="sm"
              onClick={() => mutate()}
              disabled={isLoading}
            >
              <RefreshCw size={12} />
              {t("common.refresh", "Refresh")}
            </Button>
          </div>
        }
      />

      {error && (
        <ErrorBanner message={t("services.failedToLoad", "Failed to load: {{msg}}", { msg: error.message ?? "unknown" })} onRetry={() => mutate()} />
      )}

      {status && !status.installed && (
        <SectionCard title={t("ts.stNotInstalled", "Not installed")}>
          <div className="flex flex-col items-center gap-3 py-8 text-center">
            <span className="flex h-12 w-12 items-center justify-center rounded-full bg-accent-soft text-accent">
              <Network size={22} />
            </span>
            <div className="space-y-1">
              <p className="text-sm font-medium text-text">{t("ts.notInstalledTitle", "Tailscale isn't installed")}</p>
              <p className="mx-auto max-w-[42ch] text-[13px] leading-relaxed text-text-dim">
                {t("ts.notInstalledDesc", "No tailscaled binary was found in /data/tailscale/. Run the toolkit installer's Tailscale module on the device, then refresh this page.")}
              </p>
            </div>
          </div>
        </SectionCard>
      )}

      {status?.error && (
        <ErrorBanner message={`Tailscale: ${status.error}`} />
      )}

      {status?.auth_url && (
        <SectionCard title={t("ts.loginRequired", "Login required")} className="border-warning/40">
          <p className="text-[13px] text-text">
            {t("ts.loginDesc", "Tailscale needs authentication. Open this URL on a logged-in device:")}
          </p>
          <a
            href={status.auth_url}
            target="_blank"
            rel="noreferrer"
            className="mt-2 block break-all font-mono text-[12px] text-accent underline-offset-2 hover:underline"
          >
            {status.auth_url}
          </a>
        </SectionCard>
      )}

      {status?.installed && (
        <div className="grid gap-6 lg:grid-cols-[1.4fr_1fr]">
          <SectionCard title={t("ts.thisNode", "This node")} description={t("ts.thisNodeDesc", "How the router appears on your Tailscale network.")}>
            <DescList
              items={[
                [t("ts.hostname", "Hostname"), status.self?.hostname],
                [t("ts.tsName", "Tailscale name"), status.self?.dns_name],
                [t("ts.state", "State"), status.backend_state, t("ts.helpState", "Connection state of the Tailscale daemon.")],
                [
                  "IP (v4)",
                  status.self?.ips?.find((ip) => !ip.includes(":")) ?? "—",
                ],
                [
                  "IP (v6)",
                  status.self?.ips?.find((ip) => ip.includes(":")) ?? "—",
                ],
                [t("ts.derp", "DERP relay"), status.self?.relay || "—", t("ts.helpDerp", "The Tailscale relay region used when a direct peer-to-peer link isn't possible.")],
                [t("ts.version", "Version"), status.version],
                [t("ts.exitAvail", "Exit-node available"), status.self?.exit_node_option ? t("common.yes", "Yes") : t("common.no", "No"), t("ts.helpExit", "Whether other devices can route their internet through this router.")],
              ]}
            />
          </SectionCard>

          <SectionCard title={t("ts.mesh", "Mesh")}>
            <div className="grid grid-cols-2 gap-4">
              <Metric label={t("ts.peers", "Peers")} value={status.peer_count ?? 0} />
              <Metric
                label={t("ts.online", "Online")}
                value={status.peer_online ?? 0}
                tone={(status.peer_online ?? 0) > 0 ? "success" : "neutral"}
              />
            </div>
            {status.exit_node ? (
              <div className="mt-5 border-t border-border/60 pt-4">
                <div className="text-[11px] font-medium uppercase tracking-[0.1em] text-text-dim">
                  {t("ts.exitInUse", "Exit node in use")}
                </div>
                <div className="mt-1.5 font-display text-[15px] font-semibold text-text">
                  {status.exit_node.hostname || "—"}
                </div>
                <MetaRow
                  className="mt-1"
                  items={[
                    status.exit_node.ips?.[0],
                    status.exit_node.online ? t("services.online", "Online") : t("services.offline", "Offline"),
                  ]}
                />
              </div>
            ) : (
              <div className="mt-5 border-t border-border/60 pt-4 text-[12px] text-text-dim">
                {t("ts.noExit", "No exit node selected.")}
              </div>
            )}
          </SectionCard>
        </div>
      )}

      {status?.peers && status.peers.length > 0 && (
        <SectionCard
          title={t("ts.peersTitle", "Peers")}
          description={t("ts.peersSummary", "{{total}} total · {{online}} online", { total: status.peer_count, online: status.peer_online })}
          className="mt-6"
        >
          <PeerTable peers={status.peers} />
        </SectionCard>
      )}

      <LogSection
        path="/api/services/tailscale/log"
        title={t("ts.daemonLog", "Daemon log")}
        description={t("ts.daemonLogDesc", "Tailing /data/tailscaled.log")}
      />
    </>
  );
}

function PeerTable({ peers }: { peers: TailscalePeer[] }) {
  const { t } = useTranslation();
  return (
    <div className="-my-1">
      {peers.map((p) => (
        <div
          key={p.id}
          className="flex items-start justify-between gap-3 border-b border-border/50 py-3 last:border-0"
        >
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className="truncate font-medium text-text">{p.hostname || "—"}</span>
              {p.exit_node && (
                <span className="shrink-0 rounded-full bg-accent-soft px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-[0.08em] text-accent">
                  {t("ts.exitNodeChip", "exit node")}
                </span>
              )}
            </div>
            <MetaRow
              className="mt-1"
              items={[
                p.ips?.[0] && <span className="font-mono">{p.ips[0]}</span>,
                p.os,
                <span className="tabular-nums">↑{fmtBytes(p.tx_bytes)} ↓{fmtBytes(p.rx_bytes)}</span>,
                fmtRelative(p.last_handshake, t),
              ]}
            />
          </div>
          <div className="shrink-0">
            <Status tone={p.online ? "success" : "neutral"}>{p.online ? t("services.online", "Online") : t("services.offline", "Offline")}</Status>
          </div>
        </div>
      ))}
    </div>
  );
}

function Metric({
  label,
  value,
  tone = "neutral",
}: {
  label: string;
  value: string | number;
  tone?: "neutral" | "success" | "warning" | "danger";
}) {
  const color = {
    neutral: "text-text",
    success: "text-success",
    warning: "text-warning",
    danger: "text-error",
  }[tone];
  return (
    <div>
      <div className="text-[11px] font-medium uppercase tracking-[0.1em] text-text-dim">
        {label}
      </div>
      <div
        data-numeric
        className={`mt-1 font-display text-[26px] font-semibold leading-none tracking-tight ${color}`}
      >
        {value}
      </div>
    </div>
  );
}

function DescList({
  items,
}: {
  items: Array<[string, string | number | null | undefined, string?]>;
}) {
  return (
    <dl className="divide-y divide-border/60">
      {items.map(([k, v, help]) => (
        <div
          key={k}
          className="flex items-center justify-between gap-4 py-2 text-[13px]"
        >
          <dt className="flex items-center text-text-dim">
            {k}
            {help && <Help text={help} />}
          </dt>
          <dd
            data-numeric
            className="break-all text-right font-mono text-[12.5px] text-text"
          >
            {v == null || v === "" ? <span className="text-text-dim">—</span> : String(v)}
          </dd>
        </div>
      ))}
    </dl>
  );
}

function LogSection({
  path,
  title,
  description,
}: {
  path: string;
  title: string;
  description?: string;
}) {
  const { t } = useTranslation();
  const [paused, setPaused] = useState(false);
  const { data, mutate, isLoading } = useApi<LogResp>(
    `${path}?lines=${LOG_LINES}`,
    { refreshInterval: paused ? 0 : 4000 }
  );
  return (
    <SectionCard
      title={title}
      description={description}
      className="mt-6"
      actions={
        <>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setPaused((v) => !v)}
          >
            {paused ? t("services.resume", "Resume") : t("services.pause", "Pause")}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => mutate()}
            disabled={isLoading}
          >
            <RefreshCw size={12} />
            {t("common.refresh", "Refresh")}
          </Button>
        </>
      }
    >
      <pre className="max-h-[420px] overflow-auto rounded-md border border-border/60 bg-bg-input/40 p-3 font-mono text-[11.5px] leading-[1.55] text-text">
        {data?.lines?.length ? (
          data.lines.join("\n")
        ) : (
          <span className="text-text-dim">{t("services.noLogs", "No log lines yet.")}</span>
        )}
      </pre>
    </SectionCard>
  );
}

function backendStateTone(s?: TailscaleStatus): {
  tone: "neutral" | "success" | "warning" | "danger";
  labelKey: string;
  label: string;
} {
  if (!s) return { tone: "neutral", labelKey: "ts.stLoading", label: "Loading" };
  if (!s.installed) return { tone: "neutral", labelKey: "ts.stNotInstalled", label: "Not installed" };
  if (!s.running) return { tone: "danger", labelKey: "ts.stStopped", label: "Stopped" };
  if (s.auth_url) return { tone: "warning", labelKey: "ts.stAuthRequired", label: "Auth required" };
  if (s.backend_state === "Running") return { tone: "success", labelKey: "ts.stRunning", label: "Running" };
  if (s.backend_state === "NeedsLogin") return { tone: "warning", labelKey: "ts.stNeedsLogin", label: "Needs login" };
  if (s.backend_state === "Stopped") return { tone: "danger", labelKey: "ts.stStopped", label: "Stopped" };
  return { tone: "neutral", labelKey: "", label: s.backend_state ?? "Unknown" };
}

function fmtBytes(n?: number | null): string {
  if (n == null) return "0";
  if (n < 1024) return `${n}B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)}KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)}MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)}GB`;
}

type TFn = (key: string, def: string, opts?: Record<string, unknown>) => string;

function fmtRelative(iso: string | null | undefined, t: TFn): string | null {
  if (!iso || iso.startsWith("0001-01-01")) return null;
  const parsed = Date.parse(iso);
  if (Number.isNaN(parsed)) return iso;
  // Tailscaled on this OpenWrt build sometimes stamps timestamps with
  // a "Z" suffix while writing local time, producing a small skew vs.
  // the browser's UTC clock. Treat anything within ~12h of "the future"
  // as effectively "just now" rather than printing a negative duration.
  const diff = (Date.now() - parsed) / 1000;
  if (diff < 0) {
    if (diff > -12 * 3600) return t("services.justNow", "just now");
    return null;
  }
  if (diff < 60) return t("services.agoSec", "{{n}}s ago", { n: Math.floor(diff) });
  if (diff < 3600) return t("services.agoMin", "{{n}}m ago", { n: Math.floor(diff / 60) });
  if (diff < 86400) return t("services.agoHour", "{{n}}h ago", { n: Math.floor(diff / 3600) });
  return t("services.agoDay", "{{n}}d ago", { n: Math.floor(diff / 86400) });
}

