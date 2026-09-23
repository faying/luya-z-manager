"use client";

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ExternalLink,
  RefreshCw,
  Power,
  Pencil,
  Check,
  X,
  AlertTriangle,
} from "lucide-react";
import { useApi } from "@/lib/hooks/useApi";
import { deviceIso, deviceNow, useDeviceOffset } from "@/lib/deviceClock";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import {
  PageHeader,
  SectionCard,
  Status,
  MetaRow,
  ErrorBanner,
} from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";

// Must match the whitelist in zte-agent/src/chill.rs (REGION_GROUPS/MAIN_GROUP/AI_GROUP).
const MAIN_GROUP = "🚀 节点选择";
const AI_GROUP = "🤖 AI";

interface ProxyGroup {
  name: string;
  now?: string;
  size: number;
}

interface GroupChoice {
  active?: string;
  options: string[];
}

interface ChillState {
  state: "running" | "direct" | "unknown" | string;
  reason?: string | null;
  cpuss_c?: number;
  mem_avail_mb?: number;
  core_pid?: number | null;
  started_at?: string;
  updated_at?: string;
  bypass_stale?: string[];
  rules_drift?: boolean;
  mem_pressure?: boolean;
  version?: string;
  groups?: ProxyGroup[];
  region?: GroupChoice | null;
  ai_exit?: GroupChoice | null;
}

interface Subscription {
  Upload: number;
  Download: number;
  Total: number;
  Expire: number;
}

interface ChillProvider {
  name: string;
  vehicle_type?: string;
  updated_at?: string;
  node_count: number;
  subscription?: Subscription | null;
  editable: boolean;
}

interface ProvidersResp {
  providers: ChillProvider[];
}

interface BypassResp {
  ips: string[];
  stale: string[];
}

interface DhcpLease {
  ipaddr?: string;
  macaddr?: string;
  hostname?: string;
  expires?: number;
}

interface ClientsResp {
  hosts?: Record<string, string>;
  dhcp_leases?: DhcpLease[];
}

interface JobResp {
  id: number;
  kind: string;
  status: "idle" | "running" | "done" | "error";
  message: string;
  started_unix: number;
  finished_unix: number;
}

interface LogResp {
  path: string;
  lines: string[];
  limit: number;
}

const REFRESH_INTERVAL = 4000;
const LOG_LINES = 200;

const REASON_LABELS: Record<string, string> = {
  overheat: "chill.reasonOverheat",
  lowmem: "chill.reasonLowmem",
  gaveup: "chill.reasonGaveup",
  ruleset_missing: "chill.reasonRulesetMissing",
  captive_wan: "chill.reasonCaptiveWan",
  paused: "chill.reasonPaused",
  disabled: "chill.reasonDisabled",
};

export default function ChillPage() {
  const { t } = useTranslation();
  const offset = useDeviceOffset();
  const { data: status, error, mutate, isLoading } = useApi<ChillState>(
    "/api/services/chill",
    { refreshInterval: REFRESH_INTERVAL }
  );

  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const msgTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  function flash(text: string, err = false) {
    if (msgTimer.current) clearTimeout(msgTimer.current);
    setMsg({ text, err });
    if (!err) msgTimer.current = setTimeout(() => setMsg(null), 4000);
  }

  // Single shared job slot on the agent — enable/disable and a provider URL
  // change (which triggers chill.sh reload) all go through it, same job-poll
  // idiom as the old page's ProfilesSection.
  const [pendingJob, setPendingJob] = useState<number | null>(null);
  const [opLabel, setOpLabel] = useState("");
  const { data: job } = useApi<JobResp>(
    pendingJob != null ? "/api/services/chill/job" : null,
    { refreshInterval: pendingJob != null ? 1500 : 0 }
  );
  useEffect(() => {
    if (pendingJob == null || !job || job.id !== pendingJob) return;
    if (job.status === "done") {
      flash(job.message || t("chill.opDone", "Done"));
      setPendingJob(null);
      mutate();
    } else if (job.status === "error") {
      flash(job.message || t("chill.opFailed", "Operation failed"), true);
      setPendingJob(null);
      mutate();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [job, pendingJob]);

  const busy = pendingJob != null;

  async function toggleEnabled() {
    if (busy) return;
    const enabling = status?.state !== "running" && status?.state !== "direct";
    setOpLabel(enabling ? t("chill.enabling", "Starting…") : t("chill.disabling", "Stopping…"));
    try {
      const path = enabling ? "/api/services/chill/enable" : "/api/services/chill/disable";
      const res = await apiFetch<{ job_id: number }>(path, { method: "POST" });
      setPendingJob(res.job_id);
    } catch (e) {
      flash(e instanceof ApiError ? e.message : t("chill.opFailed", "Operation failed"), true);
    }
  }

  const tone = stateTone(status);
  const running = status?.state === "running";
  const known = !!status && status.state !== "unknown";
  const dashboardUrl = useDashboardUrl();

  return (
    <>
      <PageHeader
        title="CHILL"
        description={t("chill.desc", "Native mihomo proxy — regions, AI exit, per-device bypass.")}
        actions={
          <div className="flex items-center gap-3">
            <Status tone={tone.tone}>{t(tone.labelKey, tone.label)}</Status>
            <Button
              variant={running ? "outline" : "primary"}
              size="sm"
              disabled={busy || !known}
              onClick={toggleEnabled}
            >
              <Power size={12} />
              {busy ? opLabel : running ? t("chill.stop", "Stop") : t("chill.start", "Start")}
            </Button>
            <Button variant="outline" size="sm" onClick={() => mutate()} disabled={isLoading}>
              <RefreshCw size={12} />
              {t("common.refresh", "Refresh")}
            </Button>
          </div>
        }
      />

      {error && (
        <ErrorBanner
          message={t("services.failedToLoad", "Failed to load: {{msg}}", { msg: error.message ?? "unknown" })}
          onRetry={() => mutate()}
        />
      )}

      {msg && (
        <div
          role={msg.err ? "alert" : "status"}
          aria-live="polite"
          className={`mb-4 rounded-md border px-3 py-2 text-[13px] ${
            msg.err ? "border-error/40 bg-error/10 text-error" : "border-success/40 bg-success/10 text-success"
          }`}
        >
          {msg.text}
        </div>
      )}

      {status?.state === "unknown" && (
        <SectionCard title={t("chill.stUnknownTitle", "Not started yet")}>
          <div className="flex flex-col items-center gap-3 py-8 text-center">
            <span className="flex h-12 w-12 items-center justify-center rounded-full bg-bg-input text-text-dim">
              <Power size={22} />
            </span>
            <p className="mx-auto max-w-[44ch] text-[13px] leading-relaxed text-text-dim">
              {t("chill.stUnknownDesc", "CHILL hasn't run since the device last booted. Press Start above to bring it up.")}
            </p>
          </div>
        </SectionCard>
      )}

      {known && (
        <div className="admin-card overflow-hidden">
          <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
            <Vital label={t("chill.temp", "Temperature")} value={status.cpuss_c != null ? `${status.cpuss_c}°C` : "—"} />
            <Vital label={t("chill.memAvail", "Memory available")} value={status.mem_avail_mb != null ? `${status.mem_avail_mb} MB` : "—"} hint={status.mem_pressure ? t("chill.memPressure", "Under pressure") : undefined} />
            <Vital label={t("chill.uptime", "Uptime")} value={fmtUptimeIso(status.started_at, offset)} hint={status.version} />
            <Vital label="PID" value={status.core_pid ?? "—"} />
          </div>
        </div>
      )}

      {status?.state === "direct" && status.reason && (
        <ErrorBanner
          message={t(
            "chill.directReason",
            "Traffic is going direct: {{reason}}",
            { reason: t(REASON_LABELS[status.reason] ?? status.reason, status.reason) }
          )}
        />
      )}

      {running && (
        <>
          <div className="mt-6 grid gap-6 lg:grid-cols-2">
            <RegionCard
              title={t("chill.region", "Region")}
              description={t("chill.regionDesc", "Main exit for everything on the router.")}
              group={MAIN_GROUP}
              choice={status.region}
              busy={busy}
              onSwitched={() => mutate()}
              onError={(m) => flash(m, true)}
            />
            <RegionCard
              title={t("chill.aiExit", "AI exit")}
              description={t("chill.aiExitDesc", "Exit used for AI-service traffic specifically.")}
              group={AI_GROUP}
              choice={status.ai_exit}
              busy={busy}
              onSwitched={() => mutate()}
              onError={(m) => flash(m, true)}
            />
          </div>

          <ProvidersCard onBusyChange={setPendingJob} onOpLabel={setOpLabel} onMessage={flash} />

          {status.groups && status.groups.length > 0 && (
            <SectionCard
              title={t("chill.proxyGroups", "Proxy groups")}
              description={t("chill.proxyGroupsDesc", "{{count}} groups", { count: status.groups.length })}
              className="mt-6"
            >
              <div className="-my-1">
                {status.groups.map((g) => (
                  <div
                    key={g.name}
                    className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 border-b border-border/50 py-3 last:border-0"
                  >
                    <div className="min-w-0 flex-1">
                      <span className="truncate font-medium text-text">{g.name}</span>
                      <MetaRow className="mt-1" items={[t("chill.nodes", "{{count}} nodes", { count: g.size })]} />
                    </div>
                    <span className="shrink-0 font-mono text-[12px] text-text">{g.now || "—"}</span>
                  </div>
                ))}
              </div>
            </SectionCard>
          )}

          <DashboardEmbed url={dashboardUrl} />
        </>
      )}

      <BypassCard />

      <LogSection
        path="/api/services/chill/log"
        title={t("chill.serviceLog", "Service log")}
        description={t("chill.serviceLogDesc", "Tailing /tmp/chill.log")}
      />
    </>
  );
}

/* ------------------------------------------------------------------ *
 *  Region / AI-exit switch — a plain PUT to mihomo's own group select,
 *  no reload involved (see chill.rs::regions_set).
 * ------------------------------------------------------------------ */

function RegionCard({
  title,
  description,
  group,
  choice,
  busy,
  onSwitched,
  onError,
}: {
  title: string;
  description: string;
  group: string;
  choice?: GroupChoice | null;
  busy: boolean;
  onSwitched: () => void;
  onError: (msg: string) => void;
}) {
  const { t } = useTranslation();
  const [switching, setSwitching] = useState(false);
  const options = choice?.options ?? [];

  async function pick(member: string) {
    if (busy || switching || member === choice?.active) return;
    setSwitching(true);
    try {
      await apiFetch("/api/services/chill/regions", { method: "PUT", body: { group, member } });
      onSwitched();
    } catch (e) {
      onError(e instanceof ApiError ? e.message : t("chill.opFailed", "Operation failed"));
    } finally {
      setSwitching(false);
    }
  }

  return (
    <SectionCard title={title} description={description}>
      {options.length === 0 ? (
        <p className="py-4 text-center text-[13px] text-text-dim">{t("chill.noOptions", "No members configured.")}</p>
      ) : (
        <div role="radiogroup" aria-label={title} className="flex flex-wrap gap-2">
          {options.map((opt) => {
            const active = opt === choice?.active;
            return (
              <button
                key={opt}
                type="button"
                role="radio"
                aria-checked={active}
                disabled={busy || switching}
                onClick={() => pick(opt)}
                className={`inline-flex items-center gap-1.5 rounded-full border px-3 py-1.5 text-[12.5px] font-medium transition-colors disabled:opacity-50 ${
                  active
                    ? "border-accent bg-accent-soft text-accent"
                    : "border-border bg-bg-card text-text hover:border-accent/50"
                }`}
              >
                {active && <Check size={11} />}
                {opt}
              </button>
            );
          })}
        </div>
      )}
    </SectionCard>
  );
}

/* ------------------------------------------------------------------ *
 *  Subscriptions (proxy-providers)
 * ------------------------------------------------------------------ */

function ProvidersCard({
  onBusyChange,
  onOpLabel,
  onMessage,
}: {
  onBusyChange: (jobId: number | null) => void;
  onOpLabel: (label: string) => void;
  onMessage: (msg: string, err?: boolean) => void;
}) {
  const { t } = useTranslation();
  const offset = useDeviceOffset();
  const { data, mutate, error } = useApi<ProvidersResp>("/api/services/chill/providers", {
    refreshInterval: 15000,
  });
  const [refreshing, setRefreshing] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [url, setUrl] = useState("");
  const [saving, setSaving] = useState(false);

  async function refresh(name: string) {
    if (refreshing) return;
    setRefreshing(name);
    try {
      await apiFetch("/api/services/chill/providers/refresh", { method: "POST", body: { name } });
      onMessage(t("chill.providerRefreshed", "Refreshed"));
      mutate();
    } catch (e) {
      onMessage(e instanceof ApiError ? e.message : t("chill.opFailed", "Operation failed"), true);
    } finally {
      setRefreshing(null);
    }
  }

  async function saveUrl(name: string) {
    const u = url.trim();
    if (!/^https?:\/\//.test(u)) {
      onMessage(t("chill.badUrl", "Enter a valid http(s) URL"), true);
      return;
    }
    setSaving(true);
    try {
      onOpLabel(t("chill.reloading", "Reloading…"));
      const res = await apiFetch<{ job_id: number }>("/api/services/chill/providers", {
        method: "PUT",
        body: { name, url: u },
      });
      onBusyChange(res.job_id);
      setEditing(null);
    } catch (e) {
      onMessage(e instanceof ApiError ? e.message : t("chill.opFailed", "Operation failed"), true);
    } finally {
      setSaving(false);
    }
  }

  const providers = data?.providers ?? [];

  return (
    <SectionCard
      title={t("chill.providers", "Subscriptions")}
      description={t("chill.providersDesc", "Node providers feeding the region groups above.")}
      className="mt-6"
    >
      {error && !data && (
        <div className="mb-4">
          <ErrorBanner message={t("chill.providersLoadFailed", "Couldn't load subscriptions: {{msg}}", { msg: error.message ?? "unknown" })} onRetry={() => mutate()} />
        </div>
      )}
      {providers.length === 0 && !error && (
        <p className="py-6 text-center text-[13px] text-text-dim">{t("chill.noProviders", "No subscriptions found.")}</p>
      )}
      <div className="-my-1">
        {providers.map((p) => {
          const sub = p.subscription;
          const usedPct = sub && sub.Total > 0 ? Math.min(100, (sub.Download + sub.Upload) / sub.Total * 100) : null;
          return (
            <div key={p.name} className="border-b border-border/50 py-3 last:border-0">
              <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2">
                <div className="min-w-0 flex-1">
                  <span className="truncate font-medium text-text">{p.name}</span>
                  <MetaRow
                    className="mt-1"
                    items={[
                      p.vehicle_type,
                      t("chill.nodes", "{{count}} nodes", { count: p.node_count }),
                      p.updated_at ? fmtUpdatedAt(p.updated_at, offset) : null,
                    ]}
                  />
                </div>
                <div className="flex shrink-0 items-center gap-1.5">
                  <Button variant="outline" size="sm" disabled={refreshing === p.name} onClick={() => refresh(p.name)}>
                    <RefreshCw size={12} />
                    {t("chill.refreshProvider", "Refresh")}
                  </Button>
                  {p.editable && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        setEditing(editing === p.name ? null : p.name);
                        setUrl("");
                      }}
                      aria-label={t("chill.editUrl", "Edit subscription URL")}
                    >
                      <Pencil size={13} />
                    </Button>
                  )}
                </div>
              </div>

              {sub && (
                <div className="mt-2 space-y-1">
                  {usedPct != null && (
                    <div className="h-1.5 overflow-hidden rounded-full bg-bg-input">
                      <div className="h-full rounded-full bg-accent" style={{ width: `${usedPct}%` }} />
                    </div>
                  )}
                  <MetaRow
                    items={[
                      t("chill.subUsage", "{{used}} / {{total}}", { used: fmtBytes(sub.Download + sub.Upload), total: fmtBytes(sub.Total) }),
                      sub.Expire ? t("chill.subExpires", "Expires {{date}}", { date: new Date(sub.Expire * 1000).toLocaleDateString() }) : null,
                    ]}
                  />
                </div>
              )}

              {editing === p.name && (
                <div className="mt-2 flex flex-col gap-2 rounded-md border border-border/70 bg-bg p-2.5 sm:flex-row">
                  <Input
                    value={url}
                    onChange={(e) => setUrl(e.target.value)}
                    placeholder={t("chill.urlPlaceholder", "https://… subscription URL")}
                    aria-label={t("chill.editUrl", "Edit subscription URL")}
                    onKeyDown={(e) => e.key === "Enter" && saveUrl(p.name)}
                    disabled={saving}
                  />
                  <div className="flex shrink-0 gap-1.5">
                    <Button size="sm" disabled={saving || !url.trim()} onClick={() => saveUrl(p.name)}>
                      <Check size={13} />
                      {t("common.save", "Save")}
                    </Button>
                    <Button variant="ghost" size="sm" disabled={saving} onClick={() => setEditing(null)}>
                      <X size={13} />
                      {t("common.cancel", "Cancel")}
                    </Button>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
    </SectionCard>
  );
}

/* ------------------------------------------------------------------ *
 *  Per-device bypass — source-IP `ip rule`, works regardless of whether
 *  CHILL is currently running.
 * ------------------------------------------------------------------ */

function BypassCard() {
  const { t } = useTranslation();
  const { data, mutate, error } = useApi<BypassResp>("/api/services/chill/bypass", {
    refreshInterval: 8000,
  });
  const { data: clients } = useApi<ClientsResp>("/api/network/clients", { refreshInterval: 15000 });
  const [saving, setSaving] = useState<string | null>(null);

  const ips = data?.ips ?? [];
  const stale = data?.stale ?? [];
  const hosts = clients?.hosts ?? {};
  const leases = (clients?.dhcp_leases ?? []).filter((l) => l.ipaddr);

  async function setIps(next: string[]) {
    const dedup = Array.from(new Set(next));
    setSaving(dedup.join(","));
    try {
      await apiFetch("/api/services/chill/bypass", { method: "PUT", body: { ips: dedup } });
      mutate();
    } finally {
      setSaving(null);
    }
  }

  function deviceName(ip: string): string {
    const lease = leases.find((l) => l.ipaddr === ip);
    if (lease?.hostname) return lease.hostname;
    if (lease?.macaddr && hosts[lease.macaddr]) return hosts[lease.macaddr];
    return ip;
  }

  // Union of known LAN clients and any bypassed IP that's no longer a known
  // lease (so a stale entry still shows up with something to act on).
  const rows: Array<{ ip: string; name: string }> = [
    ...leases.map((l) => ({ ip: l.ipaddr as string, name: l.hostname || hosts[l.macaddr ?? ""] || (l.ipaddr as string) })),
    ...ips.filter((ip) => !leases.some((l) => l.ipaddr === ip)).map((ip) => ({ ip, name: ip })),
  ];

  return (
    <SectionCard
      title={t("chill.bypass", "Device bypass")}
      description={t("chill.bypassDesc", "Devices listed here skip CHILL entirely and go straight out to the internet.")}
      className="mt-6"
    >
      {error && (
        <div className="mb-4">
          <ErrorBanner message={t("chill.bypassLoadFailed", "Couldn't load bypass list: {{msg}}", { msg: error.message ?? "unknown" })} onRetry={() => mutate()} />
        </div>
      )}
      {stale.length > 0 && (
        <div className="mb-4 flex items-start gap-2.5 rounded-md border border-warning/30 bg-warning/[0.06] px-3.5 py-2.5 text-[13px] text-warning">
          <AlertTriangle size={15} className="mt-0.5 shrink-0" />
          <div className="flex-1">
            {t("chill.bypassStale", "{{count}} bypassed device(s) no longer match a known lease — their traffic is silently going through CHILL again.", { count: stale.length })}
            <div className="mt-1.5 flex flex-wrap gap-1.5">
              {stale.map((ip) => (
                <button
                  key={ip}
                  type="button"
                  onClick={() => setIps(ips.filter((x) => x !== ip))}
                  disabled={saving != null}
                  className="inline-flex items-center gap-1 rounded-full border border-warning/40 px-2 py-0.5 font-mono text-[11px] text-warning hover:bg-warning/10 disabled:opacity-50"
                >
                  <X size={10} />
                  {ip}
                </button>
              ))}
            </div>
          </div>
        </div>
      )}
      {rows.length === 0 ? (
        <p className="py-6 text-center text-[13px] text-text-dim">{t("chill.noClients", "No devices seen on the LAN yet.")}</p>
      ) : (
        <div className="-my-1">
          {rows.map((r) => {
            const checked = ips.includes(r.ip);
            const isStale = stale.includes(r.ip);
            return (
              <div key={r.ip} className="flex items-center justify-between gap-3 border-b border-border/50 py-2.5 last:border-0">
                <div className="min-w-0">
                  <div className="truncate text-[13px] font-medium text-text">{deviceName(r.ip)}</div>
                  <MetaRow className="mt-0.5" items={[<span key="ip" className="font-mono">{r.ip}</span>, isStale && t("chill.stale", "stale")]} />
                </div>
                <Toggle
                  checked={checked}
                  disabled={saving != null}
                  onChange={(next) => setIps(next ? [...ips, r.ip] : ips.filter((x) => x !== r.ip))}
                  label={t("chill.bypassToggle", "Bypass")}
                />
              </div>
            );
          })}
        </div>
      )}
    </SectionCard>
  );
}

/* ------------------------------------------------------------------ *
 *  Shared bits (vitals / log / dashboard embed)
 * ------------------------------------------------------------------ */

function Vital({ label, value, hint }: { label: string; value: string | number; hint?: string }) {
  return (
    <div className="bg-bg-card px-4 py-3.5">
      <div className="text-[10px] font-medium uppercase tracking-[0.12em] text-text-dim">{label}</div>
      <div className="mt-1 font-display text-[19px] font-semibold leading-none tracking-tight text-text">
        <span data-numeric>{value}</span>
      </div>
      {hint && <div className="mt-1 truncate text-[11px] text-text-dim">{hint}</div>}
    </div>
  );
}

function LogSection({ path, title, description }: { path: string; title: string; description?: string }) {
  const { t } = useTranslation();
  const [paused, setPaused] = useState(false);
  const { data, mutate, isLoading } = useApi<LogResp>(`${path}?lines=${LOG_LINES}`, {
    refreshInterval: paused ? 0 : 4000,
  });
  return (
    <SectionCard
      title={title}
      description={description}
      className="mt-6"
      actions={
        <>
          <Button variant="ghost" size="sm" onClick={() => setPaused((v) => !v)}>
            {paused ? t("services.resume", "Resume") : t("services.pause", "Pause")}
          </Button>
          <Button variant="outline" size="sm" onClick={() => mutate()} disabled={isLoading}>
            <RefreshCw size={12} />
            {t("common.refresh", "Refresh")}
          </Button>
        </>
      }
    >
      <pre className="max-h-[420px] overflow-auto rounded-md border border-border/60 bg-bg-input/40 p-3 font-mono text-[11.5px] leading-[1.55] text-text">
        {data?.lines?.length ? data.lines.join("\n") : <span className="text-text-dim">{t("services.noLogs", "No log lines yet.")}</span>}
      </pre>
    </SectionCard>
  );
}

/**
 * Embedded zashboard. CHILL's controller is a fixed port (9999) and, per
 * chill.env, already LAN-bound with an IP allowlist (CHILL_API_LAN=1 +
 * CHILL_API_ALLOW_IP) — no agent-side reverse proxy needed for this to work
 * from the user's own devices.
 */
function DashboardEmbed({ url }: { url?: string }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  return (
    <SectionCard
      title={t("chill.dashboard", "Dashboard")}
      description={t("chill.dashboardDesc", "zashboard — mihomo's own panel, not restyled for CHILL")}
      className="mt-6"
      actions={
        <>
          <Button variant="ghost" size="sm" onClick={() => setExpanded((v) => !v)}>
            {expanded ? t("chill.collapse", "Collapse") : t("chill.expand", "Expand")}
          </Button>
          {url && (
            <a
              href={url}
              target="_blank"
              rel="noreferrer"
              className="inline-flex h-7 items-center gap-1.5 rounded-md border border-border bg-bg px-3 text-[12px] font-medium text-text transition-colors hover:border-accent hover:text-accent"
            >
              <ExternalLink size={12} />
              {t("chill.open", "Open")}
            </a>
          )}
        </>
      }
    >
      <div className="overflow-hidden rounded-md border border-border bg-bg" style={{ height: expanded ? "85vh" : "70vh" }}>
        {url ? (
          <iframe
            src={url}
            title="CHILL dashboard"
            className="block h-full w-full"
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-downloads"
          />
        ) : (
          <div className="flex h-full items-center justify-center text-[12px] text-text-dim">
            {t("chill.resolvingUrl", "Resolving dashboard URL…")}
          </div>
        )}
      </div>
    </SectionCard>
  );
}

function useDashboardUrl(): string | undefined {
  const [url, setUrl] = useState<string | undefined>(undefined);
  useEffect(() => {
    // Deliberately deferred to an effect (not computed inline) so SSR and the
    // first client render both produce the "resolving" placeholder — reading
    // window.location during render would mismatch the server-rendered HTML.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setUrl(`${window.location.protocol}//${window.location.hostname}:9999/ui/`);
  }, []);
  return url;
}

function stateTone(s?: ChillState): { tone: "neutral" | "success" | "warning" | "danger"; labelKey: string; label: string } {
  if (!s) return { tone: "neutral", labelKey: "chill.stLoading", label: "Loading" };
  if (s.state === "running") return { tone: "success", labelKey: "chill.stRunning", label: "Running" };
  if (s.state === "direct") return { tone: "warning", labelKey: "chill.stDirect", label: "Direct" };
  return { tone: "neutral", labelKey: "chill.stUnknown", label: "Not started" };
}

function fmtBytes(n?: number | null): string {
  if (n == null) return "0";
  if (n < 1024) return `${n}B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)}KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)}MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)}GB`;
}

// Both ISO times below come from the device clock ("Z" but local digits):
// compare them with device-now, not the browser's (lib/deviceClock.ts).
function fmtUpdatedAt(iso: string, offset: number): string {
  const t = deviceIso(iso);
  if (Number.isNaN(t) || t <= 0) return "—";
  const diff = deviceNow(offset) - t;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86400)}d ago`;
}

function fmtUptimeIso(iso: string | undefined, offset: number): string {
  if (!iso) return "—";
  const t = deviceIso(iso);
  if (Number.isNaN(t)) return "—";
  const diff = deviceNow(offset) - t;
  if (diff < 0) return "—";
  const d = Math.floor(diff / 86400);
  const h = Math.floor((diff % 86400) / 3600);
  const m = Math.floor((diff % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m`;
}
