"use client";

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ExternalLink,
  RefreshCw,
  ShieldOff,
  Power,
  Plus,
  Upload as UploadIcon,
  Check,
  Trash2,
  RotateCw,
  Globe,
  FileText,
  Server,
  Pencil,
} from "lucide-react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import {
  PageHeader,
  SectionCard,
  Status,
  MetaRow,
  ErrorBanner,
} from "@/components/admin/StatCard";
import { Button, Input, Textarea } from "@/components/admin/Button";
import { Help } from "@/components/admin/Help";

interface ProxyGroup {
  name: string;
  type: string;
  now?: string;
  size: number;
  udp: boolean;
}

interface ShellCrashStatus {
  installed: boolean;
  running: boolean;
  pid?: number | null;
  version?: string;
  premium?: boolean;
  meta?: boolean;
  started_at_unix?: number | null;
  memory?: number | null;
  memory_oslimit?: number | null;
  config?: {
    mode?: string;
    log_level?: string;
    http_port?: number;
    socks_port?: number;
    mixed_port?: number;
    tun_enable?: boolean | null;
    external_controller?: string;
  };
  connections?: {
    active: number;
    upload_total: number;
    download_total: number;
  };
  groups?: ProxyGroup[];
}

interface LogResp {
  path: string;
  lines: string[];
  limit: number;
}

interface Profile {
  id: string;
  name: string;
  kind: "url" | "upload";
  source_url: string;
  added_unix: number;
  updated_unix: number;
  node_count: number;
  active: boolean;
  current_node?: string | null;
  dns_servers: string[];
}

interface ProfilesResp {
  profiles: Profile[];
  active: string;
  busy: boolean;
}

interface JobResp {
  id: number;
  kind: string;
  status: "idle" | "running" | "done" | "error";
  message: string;
  profile_id: string;
  started_unix: number;
  finished_unix: number;
}

const REFRESH_INTERVAL = 4000;
const LOG_LINES = 200;
const MAX_UPLOAD_BYTES = 8 * 1024 * 1024;

export default function ShellCrashPage() {
  const { t } = useTranslation();
  const { data: status, error, mutate, isLoading } = useApi<ShellCrashStatus>(
    "/api/services/shellcrash",
    { refreshInterval: REFRESH_INTERVAL }
  );

  const tone = runningTone(status);
  const dashboardUrl = useDashboardUrl(status?.config?.external_controller);

  return (
    <>
      <PageHeader
        title="ShellCrash"
        description={t("sc.desc", "Mihomo (CrashCore) running locally. Embedded dashboard below; full state mirrors zashboard.")}
        actions={
          <div className="flex items-center gap-3">
            <Status tone={tone.tone}>{t(tone.labelKey, tone.label)}</Status>
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
        <SectionCard title={t("sc.stNotInstalled", "Not installed")}>
          <div className="flex flex-col items-center gap-3 py-8 text-center">
            <span className="flex h-12 w-12 items-center justify-center rounded-full bg-accent-soft text-accent">
              <ShieldOff size={22} />
            </span>
            <div className="space-y-1">
              <p className="text-sm font-medium text-text">{t("sc.notInstalledTitle", "ShellCrash isn't installed")}</p>
              <p className="mx-auto max-w-[44ch] text-[13px] leading-relaxed text-text-dim">
                {t("sc.notInstalledDesc", "No ShellCrash config directory was found on the device. Run the ShellCrash installer from the device menu, then refresh this page.")}
              </p>
            </div>
          </div>
        </SectionCard>
      )}

      {status?.installed && !status.running && (
        <SectionCard title={t("sc.stStopped", "Stopped")}>
          <div className="flex flex-col items-center gap-3 py-8 text-center">
            <span className="flex h-12 w-12 items-center justify-center rounded-full bg-bg-input text-text-dim">
              <Power size={22} />
            </span>
            <div className="space-y-1.5">
              <p className="text-sm font-medium text-text">{t("sc.stoppedTitle", "ShellCrash is stopped")}</p>
              <p className="mx-auto max-w-[44ch] text-[13px] leading-relaxed text-text-dim">
                {t("sc.stoppedDesc", "It's installed but not running. Start it over SSH:")}
              </p>
              <code className="inline-block rounded-md border border-border bg-bg-input/50 px-2.5 py-1 font-mono text-[12px] text-text">
                /etc/init.d/shellcrash start
              </code>
            </div>
          </div>
        </SectionCard>
      )}

      {/* Profile manager — rendered for any installed state (not gated on
          `running`) so it does NOT unmount during the brief core restart a
          profile switch triggers, which would otherwise drop its job-polling. */}
      {status?.installed && <ProfilesSection />}

      {status?.installed && status.running && (
        <>
          {/* Top-line metrics — one flat, hairline-divided strip */}
          <div className="admin-card overflow-hidden">
            <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
              <Vital
                label={t("sc.mode", "Mode")}
                value={status.config?.mode ?? "—"}
                hint={status.config?.tun_enable ? t("sc.tunOn", "TUN on") : t("sc.tunOff", "TUN off")}
              />
              <Vital label={t("sc.connections", "Connections")} value={status.connections?.active ?? 0} hint={t("sc.activeFlows", "Active flows")} />
              <Vital
                label={t("sc.memory", "Memory")}
                value={fmtMemory(status.memory)}
                hint={status.memory_oslimit ? t("sc.ofLimit", "of {{limit}}", { limit: fmtMemory(status.memory_oslimit) }) : undefined}
              />
              <Vital label={t("sc.uptime", "Uptime")} value={fmtUptime(status.started_at_unix)} hint={status.version} />
            </div>
          </div>

          <DashboardEmbed url={dashboardUrl} />

          <div className="mt-6 grid gap-6 lg:grid-cols-[1fr_1fr]">
            <SectionCard title={t("sc.runtime", "Runtime")}>
              <DescList
                items={[
                  ["PID", status.pid, t("sc.helpPid", "Process ID of the running core.")],
                  [t("ts.version", "Version"), status.version],
                  [
                    t("sc.engine", "Engine"),
                    status.meta
                      ? "mihomo (Meta)"
                      : status.premium
                        ? "Clash Premium"
                        : "Clash",
                    t("sc.helpEngine", "The proxy core powering ShellCrash."),
                  ],
                  [t("sc.logLevel", "Log level"), status.config?.log_level],
                  [t("sc.extController", "External controller"), status.config?.external_controller, t("sc.helpExtController", "Address of the core's REST API — the dashboard talks to this.")],
                ]}
              />
            </SectionCard>

            <SectionCard title={t("sc.listeners", "Listeners")}>
              <DescList
                items={[
                  [t("sc.httpPort", "HTTP port"), portValue(status.config?.http_port), t("sc.helpHttp", "Local port for HTTP proxy clients.")],
                  [t("sc.socksPort", "SOCKS port"), portValue(status.config?.socks_port), t("sc.helpSocks", "Local port for SOCKS5 proxy clients.")],
                  [t("sc.mixedPort", "Mixed port"), portValue(status.config?.mixed_port), t("sc.helpMixed", "One port that accepts both HTTP and SOCKS.")],
                  [t("sc.tun", "TUN"), status.config?.tun_enable ? t("common.enabled", "Enabled") : t("common.disabled", "Disabled"), t("sc.helpTun", "Routes all device traffic through the proxy at the network layer.")],
                ]}
              />
              <div className="mt-4 border-t border-border/60 pt-4">
                <div className="text-[10px] font-medium uppercase tracking-[0.12em] text-text-dim">
                  {t("sc.totalTransferred", "Total transferred")}
                </div>
                <div className="mt-1 flex items-baseline gap-3 font-mono text-[13px] text-text">
                  <span data-numeric>↓ {fmtBytes(status.connections?.download_total)}</span>
                  <span className="opacity-50">·</span>
                  <span data-numeric>↑ {fmtBytes(status.connections?.upload_total)}</span>
                </div>
              </div>
            </SectionCard>
          </div>

          {status.groups && status.groups.length > 0 && (
            <SectionCard
              title={t("sc.proxyGroups", "Proxy groups")}
              description={t("sc.proxyGroupsDesc", "{{count}} selectable groups · change selection in the dashboard above", { count: status.groups.length })}
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
                      <MetaRow
                        className="mt-1"
                        items={[g.type, t("sc.nodes", "{{count}} nodes", { count: g.size }), g.udp && "UDP"]}
                      />
                    </div>
                    <span className="shrink-0 font-mono text-[12px] text-text">{g.now || "—"}</span>
                  </div>
                ))}
              </div>
            </SectionCard>
          )}
        </>
      )}

      <LogSection
        path="/api/services/shellcrash/log"
        title={t("sc.serviceLog", "Service log")}
        description={t("sc.serviceLogDesc", "Tailing /tmp/ShellCrash/ShellCrash.log")}
      />
    </>
  );
}

/**
 * Profile manager — list/switch/add/upload/delete ShellCrash subscriptions.
 *
 * Switching and downloading are slow (they restart the proxy), so they run as
 * a background job on the agent; we fire the POST, then poll `…/profiles/job`
 * by exact job id until it reports done/error. Uploading and deleting are
 * synchronous. The agent re-applies the DoH + region-filter pipeline on every
 * switch, so DNS/region behaviour is preserved across profile changes.
 */
function ProfilesSection() {
  const { t } = useTranslation();
  const { data, mutate, error } = useApi<ProfilesResp>(
    "/api/services/shellcrash/profiles",
    { refreshInterval: 6000 }
  );

  const [pendingJob, setPendingJob] = useState<number | null>(null);
  const [opLabel, setOpLabel] = useState("");
  const { data: job } = useApi<JobResp>(
    pendingJob != null ? "/api/services/shellcrash/profiles/job" : null,
    { refreshInterval: pendingJob != null ? 1500 : 0 }
  );

  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const msgTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  function flash(text: string, err = false) {
    if (msgTimer.current) clearTimeout(msgTimer.current);
    setMsg({ text, err });
    if (!err) msgTimer.current = setTimeout(() => setMsg(null), 4000);
  }

  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);
  const [localBusy, setLocalBusy] = useState(false);
  // the profile being edited inline (name + DNS), + draft values
  const [edit, setEdit] = useState<{ id: string; name: string; dns: string } | null>(null);

  // React to background-job completion (match by exact id so a stale "done"
  // from a previous op never fires).
  useEffect(() => {
    if (pendingJob == null || !job || job.id !== pendingJob) return;
    if (job.status === "done") {
      flash(job.message || t("scp.opDone", "Done"));
      setPendingJob(null);
      mutate();
    } else if (job.status === "error") {
      flash(job.message || t("scp.opFailed", "Operation failed"), true);
      setPendingJob(null);
      mutate();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [job, pendingJob]);

  // Drop a stale edit draft if its profile vanished (deleted / replaced) so the
  // editor can't save edits to a profile that no longer exists.
  useEffect(() => {
    if (edit && data?.profiles && !data.profiles.some((p) => p.id === edit.id)) {
      setEdit(null);
    }
  }, [data, edit]);

  const busy = (data?.busy ?? false) || pendingJob != null || localBusy;

  const ago = (unix: number) => {
    const diff = Math.floor(Date.now() / 1000 - unix);
    if (diff < 60) return t("scp.justNow", "just now");
    if (diff < 3600) return t("scp.minsAgo", "{{n}}m ago", { n: Math.floor(diff / 60) });
    if (diff < 86400) return t("scp.hrsAgo", "{{n}}h ago", { n: Math.floor(diff / 3600) });
    return t("scp.daysAgo", "{{n}}d ago", { n: Math.floor(diff / 86400) });
  };

  async function startJob(path: string, body: unknown, label: string) {
    if (busy) return;
    setOpLabel(label);
    try {
      const res = await apiFetch<{ job_id: number; status: string }>(path, {
        method: "POST",
        body,
      });
      setPendingJob(res.job_id);
      mutate();
    } catch (e) {
      flash(e instanceof ApiError ? e.message : t("scp.opFailed", "Operation failed"), true);
    }
  }

  function addUrl() {
    const u = url.trim();
    if (!/^https?:\/\//.test(u)) {
      flash(t("scp.badUrl", "Enter a valid http(s) URL"), true);
      return;
    }
    startJob(
      "/api/services/shellcrash/profiles/url",
      { name: name.trim() || "Subscription", url: u },
      t("scp.downloading", "Downloading…")
    );
    setUrl("");
    setName("");
  }

  function applyProfile(p: Profile) {
    startJob("/api/services/shellcrash/profiles/apply", { id: p.id }, t("scp.applying", "Switching…"));
  }
  function refreshProfile(p: Profile) {
    startJob("/api/services/shellcrash/profiles/refresh", { id: p.id }, t("scp.refreshing", "Refreshing…"));
  }

  async function deleteProfile(p: Profile) {
    if (busy) return;
    if (!window.confirm(t("scp.confirmDelete", 'Delete profile "{{name}}"?', { name: p.name }))) return;
    setLocalBusy(true);
    try {
      await apiFetch("/api/services/shellcrash/profiles", { method: "DELETE", body: { id: p.id } });
      flash(t("scp.deleted", "Deleted"));
      mutate();
    } catch (e) {
      flash(e instanceof ApiError ? e.message : t("scp.opFailed", "Operation failed"), true);
    } finally {
      setLocalBusy(false);
    }
  }

  async function onFile(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = ""; // allow re-selecting the same file
    if (!file) return;
    if (file.size > MAX_UPLOAD_BYTES) {
      flash(t("scp.tooLarge", "File too large (max 8 MB)"), true);
      return;
    }
    setLocalBusy(true);
    try {
      const content = await file.text();
      const pname = file.name.replace(/\.(ya?ml|json|txt)$/i, "") || "Uploaded";
      await apiFetch("/api/services/shellcrash/profiles/upload", {
        method: "POST",
        body: { name: pname, content },
      });
      flash(t("scp.uploaded", "Uploaded — switch to it when ready"));
      mutate();
    } catch (err) {
      flash(err instanceof ApiError ? err.message : t("scp.opFailed", "Operation failed"), true);
    } finally {
      setLocalBusy(false);
    }
  }

  async function saveEdit(p: Profile) {
    if (busy || !edit || edit.id !== p.id) return;
    const newName = edit.name.trim();
    if (!newName) {
      flash(t("scp.nameEmpty", "Name cannot be empty"), true);
      return;
    }
    const list = edit.dns
      .split(/[\n,]/)
      .map((s) => s.trim())
      .filter(Boolean);
    const dnsChanged = JSON.stringify(list) !== JSON.stringify(p.dns_servers);
    const nameChanged = newName !== p.name;
    if (!nameChanged && !dnsChanged) {
      setEdit(null);
      return;
    }
    setEdit(null);
    setLocalBusy(true);
    let renamed = false;
    try {
      if (nameChanged) {
        await apiFetch("/api/services/shellcrash/profiles/rename", {
          method: "PUT",
          body: { id: p.id, name: newName },
        });
        renamed = true;
      }
      if (dnsChanged) {
        if (p.active) setOpLabel(t("scp.applying", "Switching…"));
        const res = await apiFetch<{ applied: boolean; job_id?: number }>(
          "/api/services/shellcrash/profiles/dns",
          { method: "PUT", body: { id: p.id, dns_servers: list } }
        );
        if (res.applied && res.job_id != null) {
          setPendingJob(res.job_id); // active profile re-applies; poll job
        } else {
          flash(t("scp.saved", "Saved"));
        }
      } else {
        flash(t("scp.saved", "Saved"));
      }
      mutate();
    } catch (e) {
      const m = e instanceof ApiError ? e.message : t("scp.opFailed", "Operation failed");
      // If the rename landed but DNS failed, say so — the two are independent
      // writes, so the user knows the name change took and only DNS needs retry.
      flash(renamed ? t("scp.nameOkDnsFail", "Name saved, but DNS failed: {{msg}}", { msg: m }) : m, true);
      mutate(); // reflect whatever did persist
    } finally {
      setLocalBusy(false);
    }
  }

  const profiles = data?.profiles ?? [];

  return (
    <SectionCard
      title={t("scp.title", "Profiles")}
      description={t("scp.desc", "Download, upload, and switch ShellCrash subscriptions. Switching restarts the proxy.")}
      className="mt-6"
      actions={busy ? <Status tone="accent">{opLabel || t("scp.working", "Working…")}</Status> : undefined}
    >
      {error && !data && (
        <div className="mb-4">
          <ErrorBanner
            message={t("scp.loadFailed", "Couldn't load profiles: {{msg}}", { msg: error.message ?? "unknown" })}
            onRetry={() => mutate()}
          />
        </div>
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

      <div className="-mt-1">
        {profiles.length === 0 && (
          <p className="py-6 text-center text-[13px] text-text-dim">
            {t("scp.empty", "No profiles yet. Add one from a URL or upload a config below.")}
          </p>
        )}
        {profiles.map((p) => {
          const editing = edit?.id === p.id;
          return (
          <div
            key={p.id}
            className={`border-b border-border/50 py-3 last:border-0 ${p.active ? "rounded-md bg-accent/5 px-2.5 -mx-0.5" : ""}`}
          >
            <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="truncate font-medium text-text">{p.name}</span>
                  {p.active && (
                    <span className="inline-flex items-center gap-1 rounded-full bg-accent-soft px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-accent">
                      <Check size={10} />
                      {t("scp.active", "Active")}
                    </span>
                  )}
                </div>
                <MetaRow
                  className="mt-1"
                  items={[
                    <span key="kind" className="inline-flex items-center gap-1">
                      {p.kind === "url" ? <Globe size={11} /> : <FileText size={11} />}
                      {p.kind === "url" ? t("scp.kindUrl", "URL") : t("scp.kindUpload", "Upload")}
                    </span>,
                    t("scp.nodes", "{{count}} nodes", { count: p.node_count }),
                    p.active && p.current_node ? `▶ ${p.current_node}` : null,
                    p.updated_unix ? ago(p.updated_unix) : null,
                  ]}
                />
              </div>
              <div className="flex shrink-0 items-center gap-1.5">
                {p.active ? (
                  <span className="select-none rounded-lg border border-border/70 px-3 py-1.5 text-[12px] text-text-dim">
                    {t("scp.inUse", "In use")}
                  </span>
                ) : (
                  <Button variant="primary" size="sm" disabled={busy} onClick={() => applyProfile(p)}>
                    {t("scp.switch", "Switch")}
                  </Button>
                )}
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy}
                  onClick={() =>
                    setEdit(editing ? null : { id: p.id, name: p.name, dns: p.dns_servers.join("\n") })
                  }
                  aria-label={t("scp.edit", "Rename / edit DNS")}
                  title={t("scp.edit", "Rename / edit DNS")}
                >
                  <Pencil size={13} />
                </Button>
                {p.kind === "url" && (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy}
                    onClick={() => refreshProfile(p)}
                    aria-label={t("scp.refresh", "Refresh subscription")}
                  >
                    <RotateCw size={13} />
                  </Button>
                )}
                {!p.active && (
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy}
                    onClick={() => deleteProfile(p)}
                    aria-label={t("scp.delete", "Delete profile")}
                  >
                    <Trash2 size={13} />
                  </Button>
                )}
              </div>
            </div>

            {/* DNS summary (collapsed) or name+DNS editor (expanded) */}
            {!editing ? (
              <div className="mt-1.5 flex items-center gap-1.5 text-[11px] text-text-dim">
                <Server size={11} className="shrink-0" />
                <span className="truncate font-mono">
                  {p.dns_servers.length
                    ? p.dns_servers.join(", ")
                    : t("scp.dnsDefault", "default (global DoH)")}
                </span>
              </div>
            ) : (
              <div className="mt-2 space-y-2.5 rounded-md border border-border/70 bg-bg p-2.5">
                <div>
                  <label className="mb-1 block text-[11px] font-medium text-text-dim">
                    {t("scp.nameLabel", "Name")}
                  </label>
                  <Input
                    value={edit.name}
                    onChange={(e) => setEdit({ ...edit, name: e.target.value })}
                    onKeyDown={(e) => e.key === "Enter" && saveEdit(p)}
                    placeholder={t("scp.namePlaceholder", "Name (optional)")}
                    aria-label={t("scp.nameLabel", "Name")}
                    maxLength={64}
                    disabled={busy}
                  />
                </div>
                <div>
                  <label className="mb-1 block text-[11px] font-medium text-text-dim">
                    {t("scp.dnsLabel", "Proxy-server DNS (one per line — resolves node domains)")}
                  </label>
                  <Textarea
                    rows={3}
                    value={edit.dns}
                    onChange={(e) => setEdit({ ...edit, dns: e.target.value })}
                    placeholder={"https://example.com/dns-query\nudp://1.2.3.4:53"}
                    className="font-mono text-[12px]"
                    disabled={busy}
                  />
                  <p className="mt-1 text-[10.5px] leading-snug text-text-dim">
                    {t("scp.dnsHelp", "Leave empty to use the global default. Applied immediately if this profile is active (restarts the proxy).")}
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  <Button size="sm" disabled={busy} onClick={() => saveEdit(p)}>
                    {t("common.save", "Save")}
                  </Button>
                  <Button variant="ghost" size="sm" disabled={busy} onClick={() => setEdit(null)}>
                    {t("common.cancel", "Cancel")}
                  </Button>
                </div>
              </div>
            )}
          </div>
          );
        })}
      </div>

      <div className="mt-4 space-y-2 border-t border-border/60 pt-4">
        <div className="text-[10px] font-medium uppercase tracking-[0.12em] text-text-dim">
          {t("scp.addNew", "Add a profile")}
        </div>
        <div className="flex flex-col gap-2 sm:flex-row">
          <Input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={t("scp.namePlaceholder", "Name (optional)")}
            aria-label={t("scp.name", "Profile name")}
            className="sm:max-w-[180px]"
            disabled={busy}
          />
          <Input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder={t("scp.urlPlaceholder", "https://… subscription URL")}
            aria-label={t("scp.url", "Subscription URL")}
            onKeyDown={(e) => e.key === "Enter" && addUrl()}
            disabled={busy}
          />
          <Button variant="primary" disabled={busy || !url.trim()} onClick={addUrl} className="shrink-0">
            <Plus size={14} />
            {t("scp.add", "Add")}
          </Button>
          <Button variant="outline" disabled={busy} onClick={() => fileRef.current?.click()} className="shrink-0">
            <UploadIcon size={14} />
            {t("scp.upload", "Upload")}
          </Button>
        </div>
        <input
          ref={fileRef}
          type="file"
          accept=".yaml,.yml,.json,.txt,text/yaml,application/json"
          className="hidden"
          onChange={onFile}
        />
        <p className="text-[11px] leading-relaxed text-text-dim">
          {t(
            "scp.hint",
            "URL profiles download and switch immediately. Uploaded configs are saved — switch to one when ready. DoH & region filter are re-applied automatically."
          )}
        </p>
      </div>
    </SectionCard>
  );
}

function Vital({
  label,
  value,
  hint,
}: {
  label: string;
  value: string | number;
  hint?: string;
}) {
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

/**
 * Embedded zashboard inside the admin page.
 *
 * mihomo's `/ui/` route serves the dashboard as static files on the same
 * origin its REST API listens on (port 9999). When the iframe loads from
 * that origin, zashboard's API calls are same-origin to itself, so no
 * CORS or proxy plumbing is needed in the agent.
 *
 * The URL is computed client-side (`window.location.hostname`) so the
 * embed works whether the user reaches admin via LAN, tailnet, or any
 * other hostname mapping. Until hydration resolves the URL we render a
 * placeholder rather than an empty `src` (which would otherwise navigate
 * the iframe to the parent's origin).
 */
function DashboardEmbed({ url }: { url?: string }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  return (
    <SectionCard
      title={t("sc.dashboard", "Dashboard")}
      description={t("sc.dashboardDesc", "zashboard — proxy selection, connections, configs")}
      className="mt-6"
      actions={
        <>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setExpanded((v) => !v)}
          >
            {expanded ? t("sc.collapse", "Collapse") : t("sc.expand", "Expand")}
          </Button>
          {url && (
            <a
              href={url}
              target="_blank"
              rel="noreferrer"
              className="inline-flex h-7 items-center gap-1.5 rounded-md border border-border bg-bg px-3 text-[12px] font-medium text-text transition-colors hover:border-accent hover:text-accent"
            >
              <ExternalLink size={12} />
              {t("sc.open", "Open")}
            </a>
          )}
        </>
      }
    >
      <div
        className="overflow-hidden rounded-md border border-border bg-bg"
        style={{ height: expanded ? "85vh" : "70vh" }}
      >
        {url ? (
          <iframe
            src={url}
            title="ShellCrash dashboard"
            className="block h-full w-full"
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-downloads"
          />
        ) : (
          <div className="flex h-full items-center justify-center text-[12px] text-text-dim">
            {t("sc.resolvingUrl", "Resolving dashboard URL…")}
          </div>
        )}
      </div>
    </SectionCard>
  );
}

/**
 * Resolve the ShellCrash dashboard URL.
 *
 * Mihomo's external-controller is usually bound to 127.0.0.1:9999 (only
 * the port is meaningful for outsiders). We open the dashboard at the
 * same hostname the user is reaching the admin panel from, so the link
 * works whether they came in via 192.168.0.1, a tailnet name, or a
 * hostname alias. SSR returns an empty string until hydration completes.
 */
function useDashboardUrl(externalController?: string): string | undefined {
  const [url, setUrl] = useState<string | undefined>(undefined);
  useEffect(() => {
    if (typeof window === "undefined" || !externalController) return;
    const port = externalController.split(":").pop();
    if (!port || !/^\d+$/.test(port)) return;
    setUrl(`${window.location.protocol}//${window.location.hostname}:${port}/ui/`);
  }, [externalController]);
  return url;
}

function runningTone(s?: ShellCrashStatus): {
  tone: "neutral" | "success" | "warning" | "danger";
  labelKey: string;
  label: string;
} {
  if (!s) return { tone: "neutral", labelKey: "sc.stLoading", label: "Loading" };
  if (!s.installed) return { tone: "neutral", labelKey: "sc.stNotInstalled", label: "Not installed" };
  if (!s.running) return { tone: "danger", labelKey: "sc.stStopped", label: "Stopped" };
  return { tone: "success", labelKey: "sc.stRunning", label: "Running" };
}

function portValue(p?: number): string {
  if (p == null) return "—";
  if (p === 0) return "disabled";
  return String(p);
}

function fmtBytes(n?: number | null): string {
  if (n == null) return "0";
  if (n < 1024) return `${n}B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)}KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)}MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)}GB`;
}

function fmtMemory(n?: number | null): string {
  if (n == null || n === 0) return "—";
  return fmtBytes(n);
}

function fmtUptime(startedAtUnix?: number | null): string {
  if (!startedAtUnix) return "—";
  const diff = Math.floor(Date.now() / 1000 - startedAtUnix);
  if (diff < 0) return "—";
  const d = Math.floor(diff / 86400);
  const h = Math.floor((diff % 86400) / 3600);
  const m = Math.floor((diff % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m`;
}
