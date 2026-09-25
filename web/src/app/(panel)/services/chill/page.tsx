"use client";
// CHILL (design doc §6, 11A/15A/16A). Reading order:
//
//   status (running / direct + reason / not started; node; start·stop)
//   exit ×4 ─ region cards ─┬─ AI exit · profile · device bypass
//                           └─ subscriptions · proxy groups        (≥1024: two columns)
//   advanced: temperature · memory · uptime · PID, dashboard, service log
//
// Tiers (controls-inventory): exit proxy/global/direct_keep_ai, region, AI
// exit, profile = 1; start, stop, all-direct, bypass changes, subscription
// refresh / URL save = 2 (inline confirm). Start / stop / profile / URL save
// run as agent jobs; the step polls the job to its end (keeps going when the
// tab is hidden) so the write op reports the real outcome.
import { useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, ArrowSquareOut, PencilSimple, Play, Power } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { TimeoutError } from "@/lib/api/types";
import { chillValid } from "@/lib/api/freshness";
import { useWriteOp, type Tier, type UseWriteOp } from "@/lib/api/writeOp";
import { CHILL_REASON_KEYS } from "@/lib/chill";
import { deviceIso, deviceNow, fmtDevice, useDeviceOffset } from "@/lib/deviceClock";
import { useMedia } from "@/lib/useMedia";
import { bytes } from "@/lib/home";
import type {
  ChillBypass, ChillDashboard, ChillExit, ChillJob, ChillJobStarted, ChillLog, ChillProfile, ChillProvider,
  ChillProviders, ChillStatus,
} from "@/lib/api/schemas/services";
import type { NetworkClients } from "@/lib/api/schemas/network";
import {
  Button, ChoiceGrid, ConfirmInline, ConsoleBand, Group, GroupTitle, OpResult, Readout, ReadoutWall, Row,
  Segmented, StatusBlock, StatusMark, Switch, useConfirmInline, useToast, type Tone,
} from "@/components/nd";

// Must match the whitelist in zte-agent/src/chill.rs (MAIN_GROUP / AI_GROUP).
const MAIN_GROUP = "🚀 节点选择";
const AI_GROUP = "🤖 AI";
const LOG_LINES = 200;
const JOB_LIMIT_MS = 180_000;

/** POST/PUT that starts an agent job, then poll /job until it ends. */
async function runJob(path: string, method: "POST" | "PUT", body?: unknown): Promise<ChillJob> {
  const started = await apiFetch<ChillJobStarted>(path, { method, body });
  const until = Date.now() + JOB_LIMIT_MS;
  for (;;) {
    await new Promise((r) => setTimeout(r, 1500));
    const j = await apiFetch<ChillJob>("/api/services/chill/job");
    if (j.id === started.job_id && j.status === "done") return j;
    if (j.id === started.job_id && j.status === "error") throw new Error(j.message || "job failed");
    if (Date.now() > until) throw new TimeoutError(JOB_LIMIT_MS);
  }
}

type T = (k: string, d: string, o?: Record<string, unknown>) => string;

export default function ChillPage() {
  const { t } = useTranslation();
  const chill = useApi<ChillStatus>("/api/services/chill", { refreshInterval: 4000, isValid: chillValid });
  const s = chill.data;
  const running = s?.state === "running";
  const on = s?.state === "running" || s?.state === "direct";

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">CHILL</h1>
        <Button variant="ghost" iconOnly onPress={() => chill.mutate()} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>

      <div className="grid gap-6">
        {running && s?.manual_first?.notice && (
          <p role="status" className="px-1">
            <StatusMark tone={s.manual_first.on_backup ? "warn" : "ok"}>{s.manual_first.notice}</StatusMark>
          </p>
        )}
        <PowerBlock chill={s} stale={chill.stale} error={!!chill.error && !s} invalid={chill.invalidReason} on={on} onDone={() => chill.mutate()} />

        {running && <ExitSection exit={s?.exit} onDone={() => chill.mutate()} />}

        {running && (
          <section>
            <GroupTitle>{t("chill.region", "Main exit region")}</GroupTitle>
            <p className="nd-aux -mt-1 mb-3 px-1">{t("chill.regionDesc", "Main exit for everything on the router.")}</p>
            <GroupPicker group={MAIN_GROUP} label={t("chill.region", "Main exit region")} choice={s?.region} onDone={() => chill.mutate()} />
          </section>
        )}

        <div className="grid gap-6 lg:grid-cols-2">
          <div className="grid content-start gap-6">
            {running && (
              <section>
                <GroupTitle>{t("chill.aiExit", "AI exit")}</GroupTitle>
                <p className="nd-aux -mt-1 mb-3 px-1">{t("chill.aiExitDesc", "Exit used for AI-service traffic specifically.")}</p>
                <GroupPicker group={AI_GROUP} label={t("chill.aiExit", "AI exit")} choice={s?.ai_exit} onDone={() => chill.mutate()} />
              </section>
            )}
            {s && s.state !== "unknown" && <ProfileSection chill={s} onDone={() => chill.mutate()} />}
            <BypassSection />
          </div>
          <div className="grid content-start gap-6">
            {running && <ProvidersSection onDone={() => chill.mutate()} />}
            {running && s?.groups && s.groups.length > 0 && (
              <Group title={t("chill.proxyGroups", "Proxy groups")}>
                {s.groups.map((g) => (
                  <Row key={g.name} label={g.name} sub={t("chill.nodes", "{{count}} nodes", { count: g.size })} value={g.now || "—"} />
                ))}
              </Group>
            )}
          </div>
        </div>

        <Advanced chill={s} stale={chill.stale} running={running} />
      </div>
    </>
  );
}

// ── status + start/stop ───────────────────────────────────────────────

function PowerBlock({
  chill, stale, error, invalid, on, onDone,
}: { chill: ChillStatus | undefined; stale: boolean; error: boolean; invalid?: string; on: boolean; onDone: () => void }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const inline = useConfirmInline(open);
  const toast = useToast();
  const op = useWriteOp({
    tier: 2,
    steps: [
      {
        label: on ? t("chill.stop", "Stop") : t("chill.start", "Start"),
        run: () => runJob(on ? "/api/services/chill/disable" : "/api/services/chill/enable", "POST"),
      },
    ],
    verify: async () => {
      const st = await apiFetch<ChillStatus>("/api/services/chill");
      onDone();
      return on ? st.state !== "running" : st.state === "running" || st.state === "direct";
    },
  });

  let tone: Tone = "neutral";
  let word: string = t("chill.stLoading", "Loading");
  let reason: ReactNode = null;
  if (error) {
    tone = "bad";
    word = t("home.chillUnreadable", "Can't read CHILL status");
  } else if (chill?.state === "running") {
    tone = stale ? "stale" : "ok";
    word = t("chill.stRunning", "Running");
    reason = invalid ? t("home.chillCoreDown", "The proxy core is not answering") : chill.region?.active ?? null;
  } else if (chill?.state === "direct") {
    tone = "warn";
    word = t("chill.stDirect", "Direct");
    reason = chill.reason
      ? t("chill.directReason", "Traffic is going direct: {{reason}}", { reason: t(CHILL_REASON_KEYS[chill.reason] ?? chill.reason, chill.reason) })
      : null;
  } else if (chill?.state === "unknown") {
    word = t("chill.stUnknown", "Not started");
    reason = t("chill.stUnknownDesc", "CHILL hasn't run since the device last booted. Press Start to bring it up.");
  }
  const meta = chill?.state === "running" ? [chill.version && `mihomo ${chill.version}`, chill.mode && `mode ${chill.mode}`].filter(Boolean).join(" · ") : undefined;

  const actionLabel = on ? t("chill.stop", "Stop") : t("chill.start", "Start");
  return (
    <div>
      <StatusBlock
        tone={tone}
        state={word}
        reason={reason}
        meta={meta || undefined}
        actions={
          chill && (
            <Button
              variant={on ? "secondary" : "primary"}
              onPress={() => setOpen((o) => !o)}
              isDisabled={op.busy}
              pending={op.busy}
              {...inline.triggerProps}
            >
              {on ? <Power size={18} weight="bold" aria-hidden /> : <Play size={18} weight="fill" aria-hidden />}
              {actionLabel}
            </Button>
          )
        }
      />
      <ConfirmInline
        id={inline.id}
        open={open}
        actionLabel={actionLabel}
        consequence={
          on
            ? t("chill.stopConsequence", "All traffic goes direct, AI and VoWiFi too. You can start it again at any time.")
            : t("chill.startConsequence", "Traffic starts going through CHILL; it takes about 10 seconds.")
        }
        onCancel={() => setOpen(false)}
        onConfirm={() => {
          setOpen(false);
          op.start();
          op.confirm();
          toast.show("ok", on ? t("chill.disabling", "Stopping…") : t("chill.enabling", "Starting…"));
        }}
      />
      <div className="mt-2 px-1">
        <OpResult op={op} />
      </div>
    </div>
  );
}

// ── exit ──────────────────────────────────────────────────────────────

const EXITS: ChillExit[] = ["proxy", "direct_keep_ai", "direct_all", "global"];

function exitLabel(t: T, x: ChillExit) {
  return {
    proxy: t("chill.exit.proxy", "Proxy"),
    direct_keep_ai: t("chill.exit.direct_keep_ai", "Direct · AI stays"),
    direct_all: t("chill.exit.direct_all", "All direct"),
    global: t("chill.exit.global", "Global"),
  }[x];
}
function exitHint(t: T, x: ChillExit) {
  return {
    proxy: t("chill.exitHint.proxy", "Normal, as at home."),
    direct_keep_ai: t("chill.exitHint.direct_keep_ai", "Abroad on a local SIM: everything direct except AI and VoWiFi."),
    direct_all: t("chill.exitHint.direct_all", "AI and VoWiFi go direct too."),
    global: t("chill.exitHint.global", "mihomo's global mode, same as before."),
  }[x];
}

function ExitSection({ exit, onDone }: { exit?: ChillExit; onDone: () => void }) {
  const { t } = useTranslation();
  // The write op snapshots its config at start(), before a state update
  // would land: the chosen value travels in a ref.
  const wantRef = useRef<ChillExit | null>(null);
  const [want, setWant] = useState<ChillExit | null>(null);
  const [confirmAll, setConfirmAll] = useState(false);
  const inline = useConfirmInline(confirmAll);
  const op = useSimpleWrite(1, t("chill.exitTitle", "Exit"), () =>
    apiFetch("/api/services/chill/exit", { method: "PUT", body: { state: wantRef.current } }),
    async () => {
      const st = await apiFetch<ChillStatus>("/api/services/chill");
      onDone();
      return st.exit === wantRef.current;
    },
  );
  const allOp = useSimpleWrite(2, t("chill.exit.direct_all", "All direct"), () =>
    apiFetch("/api/services/chill/exit", { method: "PUT", body: { state: "direct_all" } }),
    async () => {
      const st = await apiFetch<ChillStatus>("/api/services/chill");
      onDone();
      return st.exit === "direct_all";
    },
  );
  const busy = op.busy || allOp.busy;
  const shown = busy ? (allOp.busy ? "direct_all" : want) : exit ?? null;

  return (
    <section>
      <GroupTitle>{t("chill.exitTitle", "Exit")}</GroupTitle>
      <div className="nd-group grid gap-3 p-4 lg:p-5">
        <Segmented<ChillExit>
          label={t("chill.exitTitle", "Exit")}
          block
          value={shown}
          isDisabled={busy}
          onChange={(v) => {
            if (v === "direct_all") {
              setConfirmAll(true);
              return;
            }
            wantRef.current = v;
            setWant(v);
            op.start();
          }}
          options={EXITS.map((id) => ({ id, label: exitLabel(t, id) }))}
        />
        {shown && <p className="nd-aux">{exitHint(t, shown)}</p>}
        <p className="nd-aux">
          {t("chill.exitDesc", "Abroad on a local SIM the device switches to “Direct · AI stays” by itself, and back to Proxy once a home SIM is in again.")}
        </p>
        <span {...inline.triggerProps} hidden />
        <ConfirmInline
          id={inline.id}
          open={confirmAll}
          actionLabel={exitLabel(t, "direct_all")}
          consequence={t("chill.allDirectConsequence", "All traffic goes direct, AI and VoWiFi too. You can switch back at any time.")}
          onCancel={() => setConfirmAll(false)}
          onConfirm={() => {
            setConfirmAll(false);
            allOp.start();
            allOp.confirm();
          }}
        />
        <OpResult op={op.phase !== "idle" ? op : allOp} />
      </div>
    </section>
  );
}

// ── region / AI group ─────────────────────────────────────────────────

function GroupPicker({
  group, label, choice, onDone,
}: { group: string; label: string; choice?: { active: string | null; options: string[] } | null; onDone: () => void }) {
  const { t } = useTranslation();
  const wantRef = useRef<string | null>(null);
  const [want, setWant] = useState<string | null>(null);
  const op = useSimpleWrite(1, label, () =>
    apiFetch("/api/services/chill/regions", { method: "PUT", body: { group, member: wantRef.current } }),
    async () => {
      const st = await apiFetch<ChillStatus>("/api/services/chill");
      onDone();
      const c = group === MAIN_GROUP ? st.region : st.ai_exit;
      return c?.active === wantRef.current;
    },
  );
  const options = choice?.options ?? [];
  if (options.length === 0) return <p className="nd-aux px-1">{t("chill.noOptions", "No members configured.")}</p>;
  return (
    <div className="grid gap-2">
      <ChoiceGrid
        label={label}
        options={options}
        value={op.busy ? want : choice?.active}
        isDisabled={op.busy}
        onChange={(v) => {
          wantRef.current = v;
          setWant(v);
          op.start();
        }}
      />
      <div className="px-1">
        <OpResult op={op} />
      </div>
    </div>
  );
}

// ── profile ───────────────────────────────────────────────────────────

const PROFILES: ChillProfile[] = ["eco", "standard", "perf"];

function ProfileSection({ chill, onDone }: { chill: ChillStatus; onDone: () => void }) {
  const { t } = useTranslation();
  const profile = chill.profile ?? "standard";
  const effective = chill.profile_effective ?? profile;
  const wantRef = useRef<ChillProfile | null>(null);
  const [want, setWant] = useState<ChillProfile | null>(null);
  const op = useSimpleWrite(1, t("chill.profileTitle", "Profile"), () =>
    runJob("/api/services/chill/profile", "PUT", { profile: wantRef.current }),
    async () => {
      const st = await apiFetch<ChillStatus>("/api/services/chill");
      onDone();
      return st.profile === wantRef.current;
    },
  );
  const shown = op.busy ? want : profile;
  const hint: Record<ChillProfile, string> = {
    eco: t("chill.profileHint.eco", "Probes nodes hourly, fewer keep-alives, core limited to 2 cores. Cooler and lighter on battery; a dead node takes longer to be noticed."),
    standard: t("chill.profileHint.standard", "The settings CHILL has always used."),
    perf: t("chill.profileHint.perf", "Probes nodes every 10 minutes and dials several addresses at once. Faster failover and connects, a little more power."),
  };
  return (
    <section>
      <GroupTitle>{t("chill.profileTitle", "Profile")}</GroupTitle>
      <div className="nd-group grid gap-3 p-4 lg:p-5">
        <Segmented<ChillProfile>
          label={t("chill.profileTitle", "Profile")}
          block
          value={shown}
          isDisabled={op.busy}
          onChange={(v) => {
            wantRef.current = v;
            setWant(v);
            op.start();
          }}
          options={PROFILES.map((id) => ({
            id,
            label: { eco: t("chill.profile.eco", "Eco"), standard: t("chill.profile.standard", "Standard"), perf: t("chill.profile.perf", "Performance") }[id],
          }))}
        />
        {shown && <p className="nd-aux">{hint[shown]}</p>}
        <p className="nd-aux">
          {t("chill.profileDesc", "Trade battery and heat against failover speed. Switching into or out of Eco restarts the proxy core: connections drop for about 10 seconds.")}
        </p>
        {chill.thermal_eco && effective !== profile && (
          <p className="text-[14px] font-semibold text-nd-warnT">
            <StatusMark tone="warn">
              {t("chill.profileThermal", "Running as Eco for now because the device is hot; back to your choice once it has cooled down.")}
            </StatusMark>
          </p>
        )}
        {chill.state !== "running" && <p className="nd-aux">{t("chill.profileNotRunning", "Applies the next time CHILL starts.")}</p>}
        <OpResult op={op} />
      </div>
    </section>
  );
}

// ── subscriptions ─────────────────────────────────────────────────────

function ProvidersSection({ onDone }: { onDone: () => void }) {
  const { t } = useTranslation();
  const prov = useApi<ChillProviders>("/api/services/chill/providers", {
    refreshInterval: 15000,
    // mihomo unreachable → {providers: []} with ok:true (fake success)
    isValid: (d) => (d.providers.length > 0 ? true : { ok: false, reason: t("chill.noProviders", "No subscriptions found.") }),
  });
  const list = prov.data?.providers ?? [];
  return (
    <section>
      <GroupTitle>{t("chill.providers", "Subscriptions")}</GroupTitle>
      <div className={`nd-group${prov.stale ? " nd-stale" : ""}`}>
        {!prov.data && !prov.error && <div className="nd-row nd-aux">{t("common.loading", "Loading…")}</div>}
        {prov.error && !prov.data && (
          <div className="nd-row flex-wrap">
            <span className="flex-1 text-nd-t2">
              {prov.invalidReason ?? t("chill.providersLoadFailed", "Couldn't load subscriptions: {{msg}}", { msg: prov.error.message ?? "" })}
            </span>
            <Button variant="secondary" size="sm" onPress={() => prov.mutate()}>{t("common.retry", "Retry")}</Button>
          </div>
        )}
        {list.map((p) => (
          <Provider key={p.name} p={p} onDone={() => { prov.mutate(); onDone(); }} />
        ))}
      </div>
      <p className="nd-aux mt-2 px-1">{t("chill.providersDesc", "Node providers feeding the region groups above.")}</p>
    </section>
  );
}

function Provider({ p, onDone }: { p: ChillProvider; onDone: () => void }) {
  const { t } = useTranslation();
  const offset = useDeviceOffset();
  const [confirm, setConfirm] = useState<null | "refresh" | "save">(null);
  const inline = useConfirmInline(confirm !== null);
  const [editing, setEditing] = useState(false);
  const [url, setUrl] = useState("");
  const [urlErr, setUrlErr] = useState<string | null>(null);
  const refreshOp = useSimpleWrite(2, t("chill.refreshProvider", "Refresh"), () =>
    apiFetch("/api/services/chill/providers/refresh", { method: "POST", body: { name: p.name } }),
  );
  const saveOp = useSimpleWrite(2, t("common.save", "Save"), () =>
    runJob("/api/services/chill/providers", "PUT", { name: p.name, url: url.trim() }),
  );
  const sub = p.subscription;
  const used = sub ? sub.Download + sub.Upload : null;
  const pct = sub && sub.Total > 0 && used != null ? Math.min(100, (used / sub.Total) * 100) : null;
  const shownOp = saveOp.phase !== "idle" ? saveOp : refreshOp;

  return (
    <div className="nd-row flex-col items-stretch gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-0 flex-1">
          <div className="nd-row__label truncate">{p.name}</div>
          <div className="nd-row__sub">
            {[p.vehicle_type, t("chill.nodes", "{{count}} nodes", { count: p.node_count }), p.updated_at ? fmtAgo(t, p.updated_at, offset) : null]
              .filter(Boolean)
              .join(" · ")}
          </div>
        </div>
        <Button
          variant="secondary"
          size="sm"
          onPress={() => setConfirm(confirm === "refresh" ? null : "refresh")}
          isDisabled={refreshOp.busy}
          pending={refreshOp.busy}
          {...(confirm === "refresh" ? inline.triggerProps : {})}
        >
          {t("chill.refreshProvider", "Refresh")}
        </Button>
        {p.editable && (
          <Button
            variant="ghost"
            size="sm"
            iconOnly
            aria-label={t("chill.editUrl", "Edit subscription URL")}
            aria-expanded={editing}
            onPress={() => {
              setEditing((e) => !e);
              setUrl("");
              setUrlErr(null);
            }}
          >
            <PencilSimple size={18} weight="bold" aria-hidden />
          </Button>
        )}
      </div>
      {sub && (
        <div className="grid gap-1">
          {pct != null && (
            <div className="h-1.5 overflow-hidden rounded-full bg-nd-track" role="img" aria-label={`${pct.toFixed(0)}%`}>
              <div className="h-full rounded-full bg-nd-t1" style={{ width: `${pct}%` }} />
            </div>
          )}
          <span className="nd-aux">
            {t("chill.subUsage", "{{used}} / {{total}}", { used: bytes(used) ?? "—", total: bytes(sub.Total) ?? "—" })}
            {sub.Expire ? ` · ${t("chill.subExpires", "Expires {{date}}", { date: new Date(sub.Expire * 1000).toLocaleDateString() })}` : ""}
          </span>
        </div>
      )}
      {editing && (
        <form
          className="grid gap-2 sm:flex"
          onSubmit={(e) => {
            e.preventDefault();
            if (!/^https?:\/\//.test(url.trim())) {
              setUrlErr(t("chill.badUrl", "Enter a valid http(s) URL"));
              return;
            }
            setUrlErr(null);
            setConfirm("save");
          }}
        >
          <input
            className="nd-field flex-1"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder={t("chill.urlPlaceholder", "https://… subscription URL")}
            aria-label={t("chill.editUrl", "Edit subscription URL")}
            inputMode="url"
            autoCapitalize="off"
            spellCheck={false}
          />
          <div className="flex gap-2">
            <Button type="submit" isDisabled={!url.trim() || saveOp.busy} {...(confirm === "save" ? inline.triggerProps : {})}>
              {t("common.save", "Save")}
            </Button>
            <Button variant="secondary" onPress={() => { setEditing(false); setConfirm(null); }}>
              {t("common.cancel", "Cancel")}
            </Button>
          </div>
        </form>
      )}
      {urlErr && <p className="nd-error" role="alert">{urlErr}</p>}
      <ConfirmInline
        id={inline.id}
        open={confirm !== null}
        actionLabel={confirm === "save" ? t("chill.saveUrl", "save the subscription URL") : t("chill.refreshProvider", "Refresh")}
        consequence={
          confirm === "save"
            ? t("chill.saveUrlConsequence", "CHILL reloads with the new subscription; connections may drop for a few seconds.")
            : t("chill.refreshConsequence", "Downloads the node list again now; nodes that disappeared stop being used.")
        }
        onCancel={() => setConfirm(null)}
        onConfirm={() => {
          const which = confirm;
          setConfirm(null);
          if (which === "save") {
            saveOp.start();
            saveOp.confirm();
            setEditing(false);
          } else {
            refreshOp.start();
            refreshOp.confirm();
          }
          setTimeout(onDone, 500);
        }}
      />
      <OpResult op={shownOp} />
    </div>
  );
}

// ── device bypass ─────────────────────────────────────────────────────

function BypassSection() {
  const { t } = useTranslation();
  const bp = useApi<ChillBypass>("/api/services/chill/bypass", { refreshInterval: 8000 });
  const clients = useApi<NetworkClients>("/api/network/clients", { refreshInterval: 15000 });
  const [pending, setPending] = useState<null | { ip: string; on: boolean; name: string }>(null);
  const inline = useConfirmInline(pending !== null);
  const ips = bp.data?.ips ?? [];
  const stale = bp.data?.stale ?? [];
  const nextIps = pending ? Array.from(new Set(pending.on ? [...ips, pending.ip] : ips.filter((x) => x !== pending.ip))) : ips;
  const op = useSimpleWrite(2, t("chill.bypass", "Device bypass"), () =>
    apiFetch("/api/services/chill/bypass", { method: "PUT", body: { ips: nextIps } }),
    async () => {
      const d = await apiFetch<ChillBypass>("/api/services/chill/bypass");
      await bp.mutate(d, { revalidate: false });
      return nextIps.every((ip) => d.ips.includes(ip)) && d.ips.every((ip) => nextIps.includes(ip));
    },
  );

  const hosts = clients.data?.hosts ?? {};
  const leases = (clients.data?.dhcp_leases ?? []).filter((l) => l.ipaddr);
  const nameOf = (ip: string) => {
    const l = leases.find((x) => x.ipaddr === ip);
    if (l?.hostname) return l.hostname;
    const mac = l?.macaddr;
    const hint = mac ? hosts[mac] ?? hosts[mac.toUpperCase()] ?? hosts[mac.toLowerCase()] : undefined;
    return hint?.name || ip;
  };
  const rows = [
    ...leases.map((l) => l.ipaddr as string),
    ...ips.filter((ip) => !leases.some((l) => l.ipaddr === ip)),
  ];

  return (
    <section>
      <GroupTitle>{t("chill.bypass", "Device bypass")}</GroupTitle>
      <p className="nd-aux -mt-1 mb-3 px-1">{t("chill.bypassDesc", "Devices listed here skip CHILL entirely and go straight out to the internet.")}</p>
      {stale.length > 0 && (
        <div className="mb-3 grid gap-2 rounded-nd-card bg-nd-washW px-4 py-3">
          <StatusMark tone="warn">
            {t("chill.bypassStale", "{{count}} bypassed device(s) no longer match a known lease — their traffic is silently going through CHILL again.", { count: stale.length })}
          </StatusMark>
          <div className="flex flex-wrap gap-2">
            {stale.map((ip) => (
              <Button
                key={ip}
                variant="secondary"
                size="sm"
                isDisabled={op.busy}
                aria-label={t("chill.removeStale", "Remove {{ip}} from the bypass list", { ip })}
                onPress={() => setPending({ ip, on: false, name: ip })}
              >
                <span className="nd-mono">{ip}</span> ×
              </Button>
            ))}
          </div>
        </div>
      )}
      <div className={`nd-group${bp.stale ? " nd-stale" : ""}`}>
        {bp.error && !bp.data && (
          <div className="nd-row flex-wrap">
            <span className="flex-1 text-nd-t2">{t("chill.bypassLoadFailed", "Couldn't load bypass list: {{msg}}", { msg: bp.error.message ?? "" })}</span>
            <Button variant="secondary" size="sm" onPress={() => bp.mutate()}>{t("common.retry", "Retry")}</Button>
          </div>
        )}
        {rows.length === 0 && bp.data && <div className="nd-row text-nd-t2">{t("chill.noClients", "No devices seen on the LAN yet.")}</div>}
        {rows.map((ip) => {
          const checked = pending?.ip === ip ? pending.on : ips.includes(ip);
          return (
            <Row
              key={ip}
              label={nameOf(ip)}
              sub={<span className="nd-mono">{ip}{stale.includes(ip) ? ` · ${t("chill.stale", "stale")}` : ""}</span>}
              control={
                <span {...(pending?.ip === ip ? inline.triggerProps : {})}>
                  <Switch
                    label={t("chill.bypassToggleFor", "Bypass CHILL for {{name}}", { name: nameOf(ip) })}
                    isSelected={checked}
                    isDisabled={op.busy || (pending !== null && pending.ip !== ip)}
                    onChange={(v) => setPending({ ip, on: v, name: nameOf(ip) })}
                  />
                </span>
              }
            />
          );
        })}
      </div>
      <ConfirmInline
        id={inline.id}
        open={pending !== null}
        actionLabel={pending?.on ? t("chill.bypassOn", "bypass {{name}}", { name: pending?.name ?? "" }) : t("chill.bypassOff", "stop bypassing {{name}}", { name: pending?.name ?? "" })}
        consequence={
          pending?.on
            ? t("chill.bypassOnConsequence", "{{name}} goes straight to the internet without CHILL, AI and VoWiFi included.", { name: pending?.name ?? "" })
            : t("chill.bypassOffConsequence", "{{name}} goes through CHILL again.", { name: pending?.name ?? "" })
        }
        onCancel={() => setPending(null)}
        onConfirm={() => {
          op.start();
          op.confirm();
          setTimeout(() => setPending(null), 0);
        }}
      />
      <div className="mt-2 px-1">
        <OpResult op={op} />
      </div>
    </section>
  );
}

// ── advanced: vitals, dashboard, log ──────────────────────────────────

function Advanced({ chill, stale, running }: { chill: ChillStatus | undefined; stale: boolean; running: boolean }) {
  const { t } = useTranslation();
  const offset = useDeviceOffset();
  const known = !!chill && chill.state !== "unknown";
  return (
    <section className="grid gap-4">
      <h2 className="nd-group-title">{t("chill.advanced", "Advanced")}</h2>
      {known && (
        <ReadoutWall label={t("chill.vitals", "CHILL vitals")}>
          <Readout label={t("chill.temp", "Temperature")} value={chill.cpuss_c ?? null} unit="°C" stale={stale} />
          <Readout
            label={t("chill.memAvail", "Memory available")}
            value={chill.mem_avail_mb ?? null}
            unit="MB"
            sub={chill.mem_pressure ? t("chill.memPressure", "Under pressure") : undefined}
            stale={stale}
          />
          <Readout label={t("chill.uptime", "Uptime")} value={uptime(chill.started_at, offset)} sub={chill.version ?? undefined} stale={stale} />
          <Readout label="PID" value={chill.core_pid || null} stale={stale} />
        </ReadoutWall>
      )}
      {running && <Dashboard />}
      <LogBand />
    </section>
  );
}

function Dashboard() {
  const { t } = useTranslation();
  const wide = useMedia("(min-width: 1024px)");
  const [open, setOpen] = useState(false);
  const info = useApi<ChillDashboard>("/api/services/chill/dashboard", { revalidateOnFocus: false });
  const url = dashboardUrl(info.data);
  return (
    <Group>
      <Row
        label={t("chill.dashboard", "Dashboard")}
        sub={t("chill.dashboardDesc", "zashboard — mihomo's own panel, not restyled for CHILL")}
        control={
          <div className="flex flex-wrap justify-end gap-2">
            {url ? (
              <a href={url} target="_blank" rel="noreferrer" className="nd-btn nd-btn--secondary nd-btn--sm">
                <ArrowSquareOut size={18} weight="bold" aria-hidden />
                {t("chill.open", "Open")}
              </a>
            ) : (
              <span className="nd-aux">{t("chill.resolvingUrl", "Resolving dashboard URL…")}</span>
            )}
            {wide && url && (
              <Button variant="ghost" size="sm" aria-expanded={open} onPress={() => setOpen((o) => !o)}>
                {open ? t("chill.collapse", "Collapse") : t("chill.expandHere", "Show here")}
              </Button>
            )}
          </div>
        }
      />
      {wide && open && url && (
        <div className="px-4 pb-4 lg:px-5">
          <iframe
            src={url}
            title="CHILL dashboard"
            className="block h-[75vh] w-full rounded-nd-field bg-nd-bg"
            sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-downloads"
          />
        </div>
      )}
    </Group>
  );
}

/** zashboard opens already connected: the agent serves it (/chill-ui/) and
 *  proxies mihomo (/chill-api), all on :9090 (also over Tailscale). */
function dashboardUrl(d: ChillDashboard | undefined): string | undefined {
  if (!d || typeof window === "undefined") return undefined;
  const { protocol, hostname, port, host } = window.location;
  const q = new URLSearchParams({
    hostname,
    port: port || (protocol === "https:" ? "443" : "80"),
    secondaryPath: d.api,
    secret: d.secret,
    label: "CHILL",
    ...(protocol === "https:" ? { https: "1" } : {}),
  });
  return `${protocol}//${host}${d.ui}#/setup?${q.toString()}`;
}

function LogBand() {
  const { t } = useTranslation();
  const [paused, setPaused] = useState(false);
  const log = useApi<ChillLog>(`/api/services/chill/log?lines=${LOG_LINES}`, { refreshInterval: paused ? 0 : 4000 });
  const lines = log.data?.lines ?? [];
  return (
    <ConsoleBand label={t("chill.serviceLog", "Service log")}>
      <div className="mb-3 flex flex-wrap items-center gap-2 font-[family-name:var(--nd-font)]">
        <span className="nd-console__title flex-1">
          {t("chill.serviceLog", "Service log")} <span className="nd-console__muted text-[13px] font-semibold">/tmp/chill.log</span>
        </span>
        <button type="button" className="nd-btn nd-btn--sm bg-nd-console-card text-nd-console-t1" onClick={() => setPaused((p) => !p)}>
          {paused ? t("services.resume", "Resume") : t("services.pause", "Pause")}
        </button>
        <button type="button" className="nd-btn nd-btn--sm bg-nd-console-card text-nd-console-t1" onClick={() => log.mutate()}>
          {t("common.refresh", "Refresh")}
        </button>
      </div>
      <pre className="max-h-[420px] overflow-auto whitespace-pre-wrap break-words">
        {lines.length ? lines.join("\n") : <span className="nd-console__muted">{t("services.noLogs", "No log lines yet.")}</span>}
      </pre>
    </ConsoleBand>
  );
}

// ── helpers ───────────────────────────────────────────────────────────

/** One-step write op with the current run function captured at start(). */
function useSimpleWrite(tier: Tier, label: string, run: () => Promise<unknown>, verify?: () => Promise<boolean>): UseWriteOp {
  return useWriteOp({ tier, steps: [{ label, run }], verify });
}

function uptime(iso: string | undefined, offset: number): string | null {
  if (!iso) return null;
  const at = deviceIso(iso);
  if (Number.isNaN(at)) return null;
  const s = deviceNow(offset) - at;
  if (s < 0) return null;
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  return d ? `${d}d ${h}h` : h ? `${h}h ${m}m` : `${m}m`;
}

function fmtAgo(t: T, iso: string, offset: number): string {
  const at = deviceIso(iso);
  if (Number.isNaN(at) || at <= 0) return "—";
  const s = deviceNow(offset) - at;
  if (s < 60) return t("chill.justNow", "just now");
  if (s < 3600) return t("chill.minAgo", "{{n}} min ago", { n: Math.floor(s / 60) });
  if (s < 86400) return t("chill.hAgo", "{{n}} h ago", { n: Math.floor(s / 3600) });
  if (s < 7 * 86400) return t("chill.dAgo", "{{n}} d ago", { n: Math.floor(s / 86400) });
  return fmtDevice(at, "date");
}
