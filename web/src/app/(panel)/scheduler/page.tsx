"use client";
// Scheduler (new design). A job calls any /api/* path at a time; the agent
// refuses only /api/scheduler/* and /api/auth/* (scheduler.rs:113-129).
//
// Writes (controls-inventory §/scheduler, design §3.1), one write op:
//   create / update   tier 2; tier 3 when the job's action is a tier-3
//                     request (reboot, factory reset, eSIM, APN, locks, …:
//                     jobs.ts isTier3Action) — a scheduled reboot is one.
//   switch on         same rule as create; switch off is tier 2.
//   delete            tier 2 (the old page deleted without any confirm).
// Readback: GET /api/scheduler/jobs shows the job / its fields / its enabled
// flag / that it is gone. Caveat (agent, not fixable here): save() to /data
// ignores write errors, so a change can read back fine and still be lost at
// the next agent restart.
//
// Fixed against the old page: an edit no longer forces enabled:true and no
// longer drops the job's `restore`; a body that isn't valid JSON blocks the
// submit instead of being silently dropped; a one-time job needs a time;
// `last_run` is null (not undefined) when it never ran; last_status and the
// restore fields are shown. Weekdays: 0 = Monday (inventory §9).
import { useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useSWRConfig } from "swr";
import { Clock, PencilSimple, Plus, Trash } from "@phosphor-icons/react";
import { fmtDevice, fromWallInput, toWallInput } from "@/lib/deviceClock";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp } from "@/lib/api/writeOp";
import type { SchedulerAction, SchedulerJob, SchedulerSchedule } from "@/lib/api/schemas/system";
import {
  Button,
  Chips,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  Segmented,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  useToast,
  type Tone,
} from "@/components/nd";
import { dayLabel, fmtSchedule, isRebootJob, isTier3Action } from "./jobs";

const METHODS = ["GET", "POST", "PUT", "DELETE"];

interface FormState {
  name: string;
  method: string;
  path: string;
  bodyStr: string;
  schedType: "once" | "recurring";
  time: string;
  days: number[];
  onceAt: string;
}

const EMPTY_FORM: FormState = {
  name: "",
  method: "POST",
  path: "",
  bodyStr: "",
  schedType: "recurring",
  time: "03:00",
  days: [0, 1, 2, 3, 4, 5, 6],
  onceAt: "",
};

type Problem = "name" | "path" | "body" | "time" | "at";

function check(f: FormState): { problems: Set<Problem>; body?: unknown } {
  const problems = new Set<Problem>();
  if (!f.name.trim()) problems.add("name");
  const p = f.path.trim();
  if (!p.startsWith("/api/") || p.startsWith("/api/scheduler") || p.startsWith("/api/auth")) problems.add("path");
  let body: unknown = undefined;
  if (f.bodyStr.trim()) {
    try {
      body = JSON.parse(f.bodyStr);
    } catch {
      problems.add("body");
    }
  }
  if (f.schedType === "recurring" && !/^\d{2}:\d{2}$/.test(f.time)) problems.add("time");
  if (f.schedType === "once" && !f.onceAt) problems.add("at");
  return { problems, body };
}

function toAction(f: FormState, body: unknown): SchedulerAction {
  const a: SchedulerAction = { method: f.method, path: f.path.trim() };
  if (body !== undefined) a.body = body;
  return a;
}

function toSchedule(f: FormState): SchedulerSchedule {
  // The picker shows device wall time; the device compares against its own
  // clock, which is not the browser's (lib/deviceClock.ts).
  return f.schedType === "once"
    ? { type: "once", at: fromWallInput(f.onceAt) }
    : { type: "recurring", time: f.time, days: f.days };
}

function formFromJob(job: SchedulerJob): FormState {
  return {
    name: job.name,
    method: job.action.method,
    path: job.action.path,
    bodyStr: job.action.body !== undefined && job.action.body !== null ? JSON.stringify(job.action.body, null, 2) : "",
    schedType: job.schedule.type,
    time: job.schedule.type === "recurring" ? job.schedule.time : "03:00",
    days: job.schedule.type === "recurring" ? job.schedule.days : [0, 1, 2, 3, 4, 5, 6],
    onceAt: job.schedule.type === "once" && job.schedule.at ? toWallInput(job.schedule.at) : "",
  };
}

// ── form ──

function JobForm({
  initial,
  editing,
  locked,
  onSubmit,
  onCancel,
  trigger,
  after,
}: {
  initial: FormState;
  editing: boolean;
  locked: boolean;
  onSubmit: (f: FormState, action: SchedulerAction, schedule: SchedulerSchedule) => void;
  onCancel: () => void;
  trigger: Record<string, unknown>;
  after: ReactNode;
}) {
  const { t } = useTranslation();
  const uid = useId();
  const [form, setForm] = useState<FormState>(initial);
  const [tried, setTried] = useState(false);
  const set = <K extends keyof FormState>(k: K, v: FormState[K]) => setForm((f) => ({ ...f, [k]: v }));
  const { problems, body } = check(form);
  const show = (p: Problem) => tried && problems.has(p);
  const dayNames = [0, 1, 2, 3, 4, 5, 6].map((i) => dayLabel(t, i));

  function submit() {
    setTried(true);
    if (problems.size > 0) return;
    onSubmit(form, toAction(form, body), toSchedule(form));
  }

  const err = (p: Problem, text: string) =>
    show(p) ? (
      <span id={`${uid}-${p}-err`} className="nd-aux block text-nd-badT" role="alert">
        {text}
      </span>
    ) : null;
  const inv = (p: Problem) => ({
    "aria-invalid": show(p) || undefined,
    "aria-describedby": show(p) ? `${uid}-${p}-err` : undefined,
  });

  return (
    <div className="grid gap-4 p-4 lg:p-5">
      <div className="grid gap-2">
        <label htmlFor={`${uid}-name`} className="nd-row__label">{t("scheduler.jobName", "Job name")}</label>
        <input
          id={`${uid}-name`}
          className="nd-field"
          value={form.name}
          disabled={locked}
          placeholder={t("scheduler.jobNamePlaceholder", "My job")}
          onChange={(e) => set("name", e.target.value)}
          {...inv("name")}
        />
        {err("name", t("scheduler.errName", "Give the job a name."))}
      </div>
      <div className="grid gap-4 sm:grid-cols-[10rem_minmax(0,1fr)]">
        <div className="grid gap-2">
          <label htmlFor={`${uid}-method`} className="nd-row__label">{t("scheduler.method", "Method")}</label>
          <select id={`${uid}-method`} className="nd-field" value={form.method} disabled={locked} onChange={(e) => set("method", e.target.value)}>
            {METHODS.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
        </div>
        <div className="grid gap-2">
          <label htmlFor={`${uid}-path`} className="nd-row__label">{t("scheduler.path", "Path")}</label>
          <input
            id={`${uid}-path`}
            className="nd-field nd-mono"
            value={form.path}
            disabled={locked}
            placeholder="/api/device/reboot"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            onChange={(e) => set("path", e.target.value)}
            {...inv("path")}
          />
          {err("path", t("scheduler.errPath", "Starts with /api/ (not /api/scheduler or /api/auth)."))}
        </div>
      </div>
      <div className="grid gap-2">
        <label htmlFor={`${uid}-body`} className="nd-row__label">{t("scheduler.bodyLabel", "Body (JSON, optional)")}</label>
        <textarea
          id={`${uid}-body`}
          className="nd-field nd-mono min-h-24 py-2"
          value={form.bodyStr}
          disabled={locked}
          placeholder="{}"
          spellCheck={false}
          onChange={(e) => set("bodyStr", e.target.value)}
          {...inv("body")}
        />
        {err("body", t("scheduler.errBody", "Not valid JSON. Fix it or leave the box empty."))}
      </div>
      <div className="grid gap-2">
        <span className="nd-row__label" aria-hidden>
          {t("scheduler.scheduleType", "Schedule type")}
        </span>
        <Segmented<"recurring" | "once">
          label={t("scheduler.scheduleType", "Schedule type")}
          value={form.schedType}
          onChange={(v) => set("schedType", v)}
          isDisabled={locked}
          options={[
            { id: "recurring", label: t("scheduler.recurring", "Recurring") },
            { id: "once", label: t("scheduler.oneTime", "One-time") },
          ]}
        />
      </div>
      {form.schedType === "recurring" ? (
        <>
          <div className="grid gap-2">
            <label htmlFor={`${uid}-time`} className="nd-row__label">{t("scheduler.timeLabel", "Time (HH:mm)")}</label>
            <input
              id={`${uid}-time`}
              type="time"
              className="nd-field nd-mono w-40"
              value={form.time}
              disabled={locked}
              onChange={(e) => set("time", e.target.value)}
              {...inv("time")}
            />
            {err("time", t("scheduler.errTime", "Pick a time."))}
          </div>
          <div className="grid gap-2">
            <span className="nd-row__label">{t("scheduler.days", "Days")}</span>
            <Chips
              label={t("scheduler.days", "Days")}
              options={dayNames}
              value={new Set(form.days.map((d) => dayNames[d]))}
              isDisabled={locked}
              onChange={(v) =>
                set(
                  "days",
                  dayNames.map((n, i) => (v.has(n) ? i : -1)).filter((i) => i >= 0)
                )
              }
            />
            {form.days.length === 0 && <span className="nd-aux">{t("scheduler.noDaysDaily", "No day picked: runs every day.")}</span>}
          </div>
        </>
      ) : (
        <div className="grid gap-2">
          <label htmlFor={`${uid}-at`} className="nd-row__label">{t("scheduler.dateTime", "Date/time")}</label>
          <input
            id={`${uid}-at`}
            type="datetime-local"
            className="nd-field nd-mono w-64"
            value={form.onceAt}
            disabled={locked}
            onChange={(e) => set("onceAt", e.target.value)}
            {...inv("at")}
            aria-describedby={[`${uid}-at-hint`, show("at") ? `${uid}-at-err` : ""].filter(Boolean).join(" ")}
          />
          <span id={`${uid}-at-hint`} className="nd-aux">{t("scheduler.deviceTimeHint", "In the device's own clock.")}</span>
          {err("at", t("scheduler.errAt", "Pick a date and time."))}
        </div>
      )}
      {editing && <p className="nd-aux">{t("scheduler.editKeeps", "Saving keeps the job's on/off state and its restore step.")}</p>}
      <div className="flex flex-wrap items-center gap-3">
        <span {...trigger}>
          <Button onPress={submit} isDisabled={locked}>
            {editing ? t("scheduler.update", "Update") : t("scheduler.create", "Create")}
          </Button>
        </span>
        <Button variant="secondary" onPress={onCancel}>
          {t("scheduler.cancel", "Cancel")}
        </Button>
      </div>
      {after}
    </div>
  );
}

// ── page ──

type Where = "new" | number;

interface Pending {
  where: Where;
  kind: "create" | "update" | "toggle" | "delete";
  tier: 2 | 3;
  action: string; // verb phrase for the confirm
  title: string;
  consequence: string;
  downtime?: string;
  run: () => Promise<unknown>;
  check: (jobs: SchedulerJob[]) => boolean;
  onApplied?: () => void;
}

export default function SchedulerPage() {
  const { t } = useTranslation();
  const jobsApi = useApi<SchedulerJob[]>("/api/scheduler/jobs");
  const { mutate: globalMutate } = useSWRConfig();
  const toast = useToast();
  const list = Array.isArray(jobsApi.data) ? jobsApi.data : null;

  const [showNew, setShowNew] = useState(false);
  const [editId, setEditId] = useState<number | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [shownAt, setShownAt] = useState<Where | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);
  const createdId = useRef<number | null>(null);

  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: pending ? [{ label: pending.action, run: pending.run }] : [],
    verify: pending
      ? async () => {
          const d = await apiFetch<SchedulerJob[]>("/api/scheduler/jobs");
          await jobsApi.mutate(d, { revalidate: false });
          void globalMutate("/api/scheduler/jobs");
          const ok = Array.isArray(d) && pending.check(d);
          if (ok) pending.onApplied?.();
          return ok;
        }
      : undefined,
  });
  const busy = op.busy;
  const locked = !list || jobsApi.stale || busy;

  function ask(p: Pending) {
    if (busy) return;
    setPending(p);
  }
  function go() {
    if (!pending) return;
    setShownAt(pending.where);
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }
  const trig = (w: Where, kinds: Pending["kind"][]) =>
    pending?.where === w && pending.tier === 2 && kinds.includes(pending.kind) ? inline.triggerProps : {};

  const whenText = (s: SchedulerSchedule) => fmtSchedule(t, s);
  const tier3Consequence = (a: SchedulerAction, s: SchedulerSchedule) =>
    t("scheduler.c3What", "At {{when}} the device sends {{req}} by itself, with nobody there to confirm. Done by hand, this request needs a confirm dialog.", {
      when: whenText(s),
      req: `${a.method} ${a.path}`,
    });
  const tier3Downtime = (a: SchedulerAction) =>
    isRebootJob(a)
      ? t("scheduler.c3RebootDowntime", "Nothing changes now. Each time it runs, the device restarts and is unreachable for about 90 seconds.")
      : t("scheduler.c3Downtime", "Nothing changes now. What happens when it runs depends on the request.");

  // ── actions ──
  function askSave(where: Where, job: SchedulerJob | null, f: FormState, action: SchedulerAction, schedule: SchedulerSchedule) {
    const tier3 = isTier3Action(action);
    const name = f.name.trim();
    if (!job) {
      const body = { name, action, schedule };
      ask({
        where,
        kind: "create",
        tier: tier3 ? 3 : 2,
        action: t("scheduler.createAction", "create “{{name}}”", { name }),
        title: t("scheduler.confirmCreateTitle", "Create the job “{{name}}”?", { name }),
        consequence: tier3
          ? tier3Consequence(action, schedule)
          : t("scheduler.cCreate", "From now on the device sends {{req}} at {{when}}.", { req: `${action.method} ${action.path}`, when: whenText(schedule) }),
        downtime: tier3 ? tier3Downtime(action) : undefined,
        run: async () => {
          createdId.current = null;
          const j = await apiFetch<SchedulerJob>("/api/scheduler/jobs", { method: "POST", body });
          createdId.current = typeof j?.id === "number" ? j.id : null;
          return j;
        },
        check: (jobs) =>
          createdId.current !== null
            ? jobs.some((j) => j.id === createdId.current)
            : jobs.some((j) => j.name === name && j.action.path === action.path && j.action.method === action.method),
        // The form (and its inline result) closes on success, so say it here.
        onApplied: () => {
          setShowNew(false);
          toast.show("ok", t("scheduler.jobCreated", "Job created"));
        },
      });
      return;
    }
    // Keep the job's enabled flag and restore step (the old page reset both).
    const body: Record<string, unknown> = { id: job.id, name, enabled: job.enabled, action, schedule };
    if (job.restore) body.restore = job.restore;
    ask({
      where,
      kind: "update",
      tier: tier3 && job.enabled ? 3 : 2,
      action: t("scheduler.updateAction", "save “{{name}}”", { name }),
      title: t("scheduler.confirmUpdateTitle", "Save the changes to “{{name}}”?", { name }),
      consequence:
        tier3 && job.enabled
          ? tier3Consequence(action, schedule)
          : t("scheduler.cUpdate", "The job sends {{req}} at {{when}}{{off}}.", {
              req: `${action.method} ${action.path}`,
              when: whenText(schedule),
              off: job.enabled ? "" : t("scheduler.whileOff", " once it is switched on"),
            }),
      downtime: tier3 && job.enabled ? tier3Downtime(action) : undefined,
      run: () => apiFetch("/api/scheduler/jobs", { method: "PUT", body }),
      check: (jobs) => {
        const j = jobs.find((x) => x.id === job.id);
        return !!j && j.name === name && j.action.method === action.method && j.action.path === action.path && j.enabled === job.enabled;
      },
      onApplied: () => setEditId(null),
    });
  }

  function askToggle(job: SchedulerJob, on: boolean) {
    const tier3 = on && isTier3Action(job.action);
    ask({
      where: job.id,
      kind: "toggle",
      tier: tier3 ? 3 : 2,
      action: on
        ? t("scheduler.onAction", "switch “{{name}}” on", { name: job.name })
        : t("scheduler.offAction", "switch “{{name}}” off", { name: job.name }),
      title: t("scheduler.confirmOnTitle", "Switch on “{{name}}”?", { name: job.name }),
      consequence: tier3
        ? tier3Consequence(job.action, job.schedule)
        : on
          ? t("scheduler.cOn", "The job runs again at {{when}}.", { when: whenText(job.schedule) })
          : t("scheduler.cOff", "The job stops running until you switch it back on. It stays in the list."),
      downtime: tier3 ? tier3Downtime(job.action) : undefined,
      run: () => apiFetch("/api/scheduler/jobs/toggle", { method: "PUT", body: { id: job.id, enabled: on } }),
      check: (jobs) => jobs.find((j) => j.id === job.id)?.enabled === on,
    });
  }

  function askDelete(job: SchedulerJob) {
    ask({
      where: job.id,
      kind: "delete",
      tier: 2,
      action: t("scheduler.deleteAction", "delete “{{name}}”", { name: job.name }),
      title: t("scheduler.deleteAction", "delete “{{name}}”", { name: job.name }),
      consequence: t("scheduler.cDelete", "The job is removed and won't run again. To get it back, create it again."),
      run: () => apiFetch("/api/scheduler/jobs", { method: "DELETE", body: { id: job.id } }),
      check: (jobs) => !jobs.some((j) => j.id === job.id),
      // The row (and its inline result) disappears on success.
      onApplied: () => toast.show("ok", t("scheduler.jobDeleted", "Job deleted")),
    });
  }

  function confirmHere(w: Where) {
    return (
      <>
        {pending?.where === w && pending.tier === 2 && (
          <ConfirmInline
            id={inline.id}
            open
            actionLabel={pending.action}
            consequence={pending.consequence}
            onCancel={() => setPending(null)}
            onConfirm={go}
          />
        )}
        {shownAt === w && op.phase !== "idle" && op.phase !== "confirming" && (
          <div className="mt-2 px-1">
            <OpResult op={op} />
          </div>
        )}
      </>
    );
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: string = t("scheduler.loading", "Reading scheduled jobs…");
  let reason: string | null = null;
  if (!list && jobsApi.error) {
    tone = "bad";
    state = t("scheduler.unreadable", "Can't read the scheduled jobs");
    reason = jobsApi.error.message;
  } else if (list) {
    const on = list.filter((j) => j.enabled).length;
    const failing = list.filter((j) => j.last_error || (j.last_status !== null && j.last_status >= 400));
    if (list.length === 0) {
      state = t("scheduler.stNone", "No scheduled jobs");
      reason = t("scheduler.stNoneReason", "Use “New Job” to call an API on a timetable, e.g. a nightly reboot.");
    } else if (failing.length > 0) {
      tone = "warn";
      state = t("scheduler.stFailing", "{{n}} job(s) failed last time", { n: failing.length });
      reason = failing.map((j) => j.name).join(" · ");
    } else {
      tone = on > 0 ? "ok" : "neutral";
      state = t("scheduler.stCount", "{{on}} of {{n}} jobs on", { on, n: list.length });
    }
    if (jobsApi.stale) tone = "stale";
  }

  const dialog = pending?.tier === 3 ? pending : null;

  return (
    <>
      <div className="mb-4 mt-2 flex flex-wrap items-center justify-between gap-3">
        <h1 className="nd-title">{t("scheduler.title", "Scheduler")}</h1>
        <Button
          onPress={() => {
            setShowNew(true);
            setEditId(null);
          }}
          isDisabled={showNew || locked}
        >
          <Plus size={20} weight="bold" aria-hidden />
          {t("scheduler.newJob", "New Job")}
        </Button>
      </div>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("scheduler.desc", "Automate API calls on a schedule.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={list && jobsApi.stale ? <Freshness stale lastOkAt={jobsApi.lastOkAt} what={t("schedreboot.listWord", "List")} /> : undefined}
          actions={
            jobsApi.error ? (
              <Button variant="secondary" size="sm" onPress={() => jobsApi.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {showNew && (
          <section aria-labelledby="sch-new">
            <GroupTitle id="sch-new">{t("scheduler.newJob", "New Job")}</GroupTitle>
            <div className="nd-group">
              <JobForm
                initial={EMPTY_FORM}
                editing={false}
                locked={locked}
                onSubmit={(f, a, s) => askSave("new", null, f, a, s)}
                onCancel={() => {
                  setShowNew(false);
                  if (pending?.where === "new") setPending(null);
                }}
                trigger={trig("new", ["create"])}
                after={confirmHere("new")}
              />
            </div>
          </section>
        )}

        <section aria-labelledby="sch-list">
          <GroupTitle id="sch-list">{t("scheduler.jobsTitle", "Jobs")}</GroupTitle>
          <div className={`nd-group${jobsApi.stale ? " nd-stale" : ""}`}>
            {!list ? (
              [0, 1, 2].map((i) => (
                <div key={i} className="nd-row">
                  <span className="nd-skel" style={{ width: "18ch" }} />
                </div>
              ))
            ) : list.length === 0 ? (
              <div className="nd-row">
                <span className="nd-body text-nd-t2">{t("scheduler.emptyNd", "No scheduled jobs yet. Press “New Job” above to create one.")}</span>
              </div>
            ) : (
              list.map((job) =>
                editId === job.id ? (
                  <div key={job.id} className="border-b border-[var(--nd-sep)] last:border-0">
                    <JobForm
                      initial={formFromJob(job)}
                      editing
                      locked={locked}
                      onSubmit={(f, a, s) => askSave(job.id, job, f, a, s)}
                      onCancel={() => {
                        setEditId(null);
                        if (pending?.where === job.id) setPending(null);
                      }}
                      trigger={trig(job.id, ["update"])}
                      after={confirmHere(job.id)}
                    />
                  </div>
                ) : (
                  <JobRow
                    key={job.id}
                    job={job}
                    locked={locked}
                    toggleTrigger={trig(job.id, ["toggle"])}
                    deleteTrigger={trig(job.id, ["delete"])}
                    onToggle={(on) => askToggle(job, on)}
                    onEdit={() => {
                      setEditId(job.id);
                      setShowNew(false);
                    }}
                    onDelete={() => askDelete(job)}
                    after={confirmHere(job.id)}
                  />
                )
              )
            )}
          </div>
          {jobsApi.stale && list && (
            <p className="nd-aux mt-2 px-1">
              <Freshness stale lastOkAt={jobsApi.lastOkAt} what={t("schedreboot.listWord", "List")} />
              {t("wifi.refreshToEdit", " — refresh before changing anything.")}{" "}
              <Button variant="secondary" size="sm" onPress={() => jobsApi.mutate()}>
                {t("wifi.refresh", "Refresh")}
              </Button>
            </p>
          )}
        </section>
      </div>

      {dialog && (
        <ConfirmDialog
          open
          onOpenChange={(o) => !o && setPending(null)}
          title={dialog.title}
          what={dialog.consequence}
          downtime={dialog.downtime}
          recovery={t("scheduler.c3Recovery", "Switch the job off or delete it here before it runs.")}
          actionLabel={t("nd.confirmAction", "Confirm: {{action}}", { action: dialog.action })}
          onConfirm={go}
        />
      )}
    </>
  );
}

function JobRow({
  job,
  locked,
  toggleTrigger,
  deleteTrigger,
  onToggle,
  onEdit,
  onDelete,
  after,
}: {
  job: SchedulerJob;
  locked: boolean;
  toggleTrigger: Record<string, unknown>;
  deleteTrigger: Record<string, unknown>;
  onToggle: (on: boolean) => void;
  onEdit: () => void;
  onDelete: () => void;
  after: ReactNode;
}) {
  const { t } = useTranslation();
  const failed = !!job.last_error || (job.last_status !== null && job.last_status >= 400);
  return (
    <div className="border-b border-[var(--nd-sep)] px-4 py-3 last:border-0">
      <div className="flex items-start gap-3">
        <span {...toggleTrigger} className="shrink-0">
          <Switch
            label={t("scheduler.enableJob", "Enable {{name}}", { name: job.name })}
            isSelected={job.enabled}
            isDisabled={locked}
            onChange={onToggle}
          />
        </span>
        <div className="min-w-0 flex-1 pt-2">
          <div className="nd-body font-medium">{job.name || t("schedreboot.unnamed", "(no name)")}</div>
          <div className="nd-aux flex flex-wrap items-center gap-x-2">
            <Clock size={16} weight="bold" aria-hidden />
            <span>{fmtSchedule(t, job.schedule)}</span>
          </div>
          <div className="nd-aux nd-mono break-all">
            {job.action.method} {job.action.path}
          </div>
          {job.restore && (
            <div className="nd-aux">
              {t("scheduler.restoreAt", "Restores at {{time}}", { time: job.restore.time })}
              {job.last_restore != null && ` · ${t("scheduler.lastRestore", "last restore {{time}}", { time: fmtDevice(job.last_restore) })}`}
              {job.last_restore_status != null && <span className="nd-mono"> · HTTP {job.last_restore_status}</span>}
            </div>
          )}
          {job.last_restore_error && (
            <div className="nd-aux">
              <StatusMark tone="bad">{t("scheduler.restoreFailed", "Restore failed: {{e}}", { e: job.last_restore_error })}</StatusMark>
            </div>
          )}
          <div className="nd-aux mt-1">
            {job.last_run == null ? (
              t("scheduler.neverRan", "Hasn't run yet")
            ) : failed ? (
              <StatusMark tone="bad">
                {t("scheduler.lastFailed", "Last run {{time}} failed: {{e}}", {
                  time: fmtDevice(job.last_run),
                  e: job.last_error || `HTTP ${job.last_status}`,
                })}
              </StatusMark>
            ) : (
              <StatusMark tone="ok">
                {t("scheduler.last", "Last: {{time}}", { time: fmtDevice(job.last_run) })}
                {job.last_status != null ? ` · HTTP ${job.last_status}` : ""}
              </StatusMark>
            )}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button variant="ghost" iconOnly aria-label={t("scheduler.editJob", "Edit {{name}}", { name: job.name })} onPress={onEdit} isDisabled={locked}>
            <PencilSimple size={20} weight="bold" aria-hidden />
          </Button>
          <span {...deleteTrigger}>
            <Button
              variant="ghost"
              iconOnly
              aria-label={t("scheduler.deleteJob", "Delete {{name}}", { name: job.name })}
              onPress={onDelete}
              isDisabled={locked}
            >
              <Trash size={20} weight="bold" aria-hidden />
            </Button>
          </span>
        </div>
      </div>
      {after}
    </div>
  );
}
