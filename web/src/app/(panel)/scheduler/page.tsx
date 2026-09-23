"use client";

import { useState } from "react";
import { fmtDevice, fromWallInput, toWallInput } from "@/lib/deviceClock";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { useSWRConfig } from "swr";
import { Plus, Trash2, Edit2, Clock } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const METHODS = ["GET", "POST", "PUT", "DELETE"];

interface SchedulerAction {
  method: string;
  path: string;
  body?: unknown;
}

interface SchedulerSchedule {
  type: "once" | "recurring";
  at?: number;
  time?: string;
  days?: number[];
}

interface SchedulerJob {
  id: number;
  name: string;
  enabled: boolean;
  schedule: SchedulerSchedule;
  action: SchedulerAction;
  restore?: unknown;
  last_run?: number; // device epoch seconds (was typed string and shown as ms → 1970)
  last_error?: string;
}

const EMPTY_FORM = {
  name: "",
  method: "POST",
  path: "",
  bodyStr: "",
  schedType: "recurring" as "once" | "recurring",
  time: "03:00",
  days: [0, 1, 2, 3, 4, 5, 6] as number[],
  onceAt: "",
  restoreEnabled: false,
  restoreTime: "06:00",
};

type FormState = typeof EMPTY_FORM;

const DAY_KEYS = ["daySun", "dayMon", "dayTue", "dayWed", "dayThu", "dayFri", "daySat"];

function dayLabel(t: TFunction, i: number): string {
  return t(`scheduler.${DAY_KEYS[i]}`, DAYS[i]);
}

function fmtSchedule(t: TFunction, s: SchedulerSchedule): string {
  if (s.type === "once") {
    return s.at
      ? t("scheduler.onceAtTime", "Once at {{time}}", { time: fmtDevice(s.at) })
      : t("scheduler.once", "Once");
  }
  const dayStr = s.days?.length
    ? s.days.map((d) => dayLabel(t, d)).join(", ")
    : t("scheduler.daily", "daily");
  return `${s.time ?? "?"} — ${dayStr}`;
}

function buildBody(form: FormState, editId?: number): Record<string, unknown> {
  let body: unknown = undefined;
  try { body = form.bodyStr ? JSON.parse(form.bodyStr) : undefined; } catch { /* ignore */ }

  const action: SchedulerAction = { method: form.method, path: form.path };
  if (body !== undefined) action.body = body;

  const schedule: SchedulerSchedule =
    form.schedType === "once"
      // The picker shows device wall time; the device compares against its own
      // clock, which is not the browser's (lib/deviceClock.ts).
      ? { type: "once", at: form.onceAt ? fromWallInput(form.onceAt) : undefined }
      : { type: "recurring", time: form.time, days: form.days };

  const result: Record<string, unknown> = {
    name: form.name,
    action,
    schedule,
  };
  if (editId !== undefined) {
    result.id = editId;
    result.enabled = true;
  }
  return result;
}

function JobForm({
  initial,
  editId,
  onSave,
  onCancel,
  saving,
}: {
  initial?: FormState;
  editId?: number;
  onSave: (form: FormState) => void;
  onCancel: () => void;
  saving: boolean;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState<FormState>(initial ?? { ...EMPTY_FORM });

  function set<K extends keyof FormState>(k: K, v: FormState[K]) {
    setForm((f) => ({ ...f, [k]: v }));
  }

  function toggleDay(d: number) {
    set("days", form.days.includes(d) ? form.days.filter((x) => x !== d) : [...form.days, d].sort());
  }

  return (
    <div className="space-y-3">
      <div>
        <label className="mb-1 block text-xs text-text-dim">{t("scheduler.jobName", "Job name")}</label>
        <Input value={form.name} onChange={(e) => set("name", e.target.value)} placeholder={t("scheduler.jobNamePlaceholder", "My job")} />
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <div>
          <label className="mb-1 block text-xs text-text-dim">{t("scheduler.method", "Method")}</label>
          <select
            value={form.method}
            onChange={(e) => set("method", e.target.value)}
            className="h-9 w-full rounded-md border border-border bg-bg-input px-3 text-sm outline-none transition focus:border-accent"
          >
            {METHODS.map((m) => <option key={m} value={m}>{m}</option>)}
          </select>
        </div>
        <div>
          <label className="mb-1 block text-xs text-text-dim">{t("scheduler.path", "Path")}</label>
          <Input value={form.path} onChange={(e) => set("path", e.target.value)} placeholder="/api/device/reboot" />
        </div>
      </div>
      <div>
        <label className="mb-1 block text-xs text-text-dim">{t("scheduler.bodyLabel", "Body (JSON, optional)")}</label>
        <textarea
          value={form.bodyStr}
          onChange={(e) => set("bodyStr", e.target.value)}
          className="h-16 w-full rounded-md border border-border bg-bg-input px-3 py-2 font-mono text-xs outline-none transition focus:border-accent"
          placeholder='{}'
        />
      </div>
      <div>
        <label className="mb-1 block text-xs text-text-dim">{t("scheduler.scheduleType", "Schedule type")}</label>
        <div className="flex gap-4">
          {(["recurring", "once"] as const).map((st) => (
            <label key={st} className="flex cursor-pointer items-center gap-1.5 text-sm">
              <input type="radio" checked={form.schedType === st} onChange={() => set("schedType", st)} />
              {st === "recurring" ? t("scheduler.recurring", "Recurring") : t("scheduler.oneTime", "One-time")}
            </label>
          ))}
        </div>
      </div>
      {form.schedType === "recurring" ? (
        <>
          <div>
            <label className="mb-1 block text-xs text-text-dim">{t("scheduler.timeLabel", "Time (HH:mm)")}</label>
            <Input type="time" value={form.time} onChange={(e) => set("time", e.target.value)} className="w-40" />
          </div>
          <div>
            <label className="mb-1 block text-xs text-text-dim">{t("scheduler.days", "Days")}</label>
            <div className="flex flex-wrap gap-1.5">
              {DAYS.map((_d, i) => (
                <button
                  key={i}
                  type="button"
                  onClick={() => toggleDay(i)}
                  className={`rounded px-2 py-0.5 text-xs font-medium transition ${form.days.includes(i) ? "bg-accent text-white" : "border border-border text-text-dim hover:border-accent"}`}
                >
                  {dayLabel(t, i)}
                </button>
              ))}
            </div>
          </div>
        </>
      ) : (
        <div>
          <label className="mb-1 block text-xs text-text-dim">{t("scheduler.dateTime", "Date/time")}</label>
          <Input type="datetime-local" value={form.onceAt} onChange={(e) => set("onceAt", e.target.value)} className="w-64" />
        </div>
      )}
      <div className="flex items-center gap-2 pt-1">
        <Button onClick={() => onSave(form)} loading={saving}>{editId !== undefined ? t("scheduler.update", "Update") : t("scheduler.create", "Create")}</Button>
        <Button variant="ghost" onClick={onCancel}>{t("scheduler.cancel", "Cancel")}</Button>
      </div>
    </div>
  );
}

export default function SchedulerPage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<SchedulerJob[]>("/api/scheduler/jobs");
  const { mutate: globalMutate } = useSWRConfig();
  const [showNew, setShowNew] = useState(false);
  const [editId, setEditId] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);

  function showMsg(text: string, err = false) {
    setMsg({ text, err });
    setTimeout(() => setMsg(null), 4000);
  }

  async function saveJob(form: FormState) {
    setSaving(true);
    try {
      if (editId !== null) {
        await apiFetch("/api/scheduler/jobs", { method: "PUT", body: buildBody(form, editId) });
        showMsg(t("scheduler.jobUpdated", "Job updated"));
        setEditId(null);
      } else {
        await apiFetch("/api/scheduler/jobs", { method: "POST", body: buildBody(form) });
        showMsg(t("scheduler.jobCreated", "Job created"));
        setShowNew(false);
      }
      await mutate();
      globalMutate("/api/scheduler/jobs");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : t("scheduler.failed", "Failed"), true);
    } finally { setSaving(false); }
  }

  async function toggleJob(job: SchedulerJob) {
    try {
      await apiFetch("/api/scheduler/jobs/toggle", { method: "PUT", body: { id: job.id, enabled: !job.enabled } });
      await mutate();
      globalMutate("/api/scheduler/jobs");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : t("scheduler.failedToggle", "Failed to toggle"), true);
    }
  }

  async function deleteJob(id: number) {
    try {
      await apiFetch("/api/scheduler/jobs", { method: "DELETE", body: { id } });
      showMsg(t("scheduler.jobDeleted", "Job deleted"));
      await mutate();
      globalMutate("/api/scheduler/jobs");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : t("scheduler.failedDelete", "Failed to delete"), true);
    }
  }

  function formFromJob(job: SchedulerJob): FormState {
    return {
      name: job.name,
      method: job.action.method,
      path: job.action.path,
      bodyStr: job.action.body ? JSON.stringify(job.action.body, null, 2) : "",
      schedType: job.schedule.type,
      time: job.schedule.time ?? "03:00",
      days: job.schedule.days ?? [0, 1, 2, 3, 4, 5, 6],
      onceAt: job.schedule.at ? toWallInput(job.schedule.at) : "",
      restoreEnabled: false,
      restoreTime: "06:00",
    };
  }

  const jobs = data ?? [];

  return (
    <>
      <PageHeader
        title={t("scheduler.title", "Scheduler")}
        description={t("scheduler.desc", "Automate API calls on a schedule.")}
        actions={
          <Button onClick={() => { setShowNew(true); setEditId(null); }}>
            <Plus size={14} /> {t("scheduler.newJob", "New Job")}
          </Button>
        }
      />

      {error && <ErrorBanner message={error.message} />}
      {msg && (
        <div className={`mb-4 rounded-md border px-3 py-2 text-sm ${msg.err ? "border-error/40 bg-error/10 text-error" : "border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      {showNew && (
        <SectionCard title={t("scheduler.newJob", "New Job")} className="mb-4">
          <JobForm onSave={saveJob} onCancel={() => setShowNew(false)} saving={saving} />
        </SectionCard>
      )}

      <SectionCard>
        {jobs.length === 0 && !showNew && (
          <p className="text-sm text-text-dim">{t("scheduler.empty", "No scheduled jobs yet. Create one above.")}</p>
        )}
        {jobs.map((job) => (
          <div key={job.id}>
            {editId === job.id ? (
              <div className="py-3">
                <JobForm
                  initial={formFromJob(job)}
                  editId={job.id}
                  onSave={saveJob}
                  onCancel={() => setEditId(null)}
                  saving={saving}
                />
              </div>
            ) : (
              <div className="flex items-center gap-3 border-b border-border/60 py-3 last:border-0">
                <Toggle checked={job.enabled} onChange={() => toggleJob(job)} />
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium">{job.name}</div>
                  <div className="flex items-center gap-1 text-xs text-text-dim">
                    <Clock size={11} />
                    {fmtSchedule(t, job.schedule)} — {job.action.method} {job.action.path}
                  </div>
                  {job.last_error && (
                    <div className="mt-0.5 text-xs text-error">{job.last_error}</div>
                  )}
                  {job.last_run && !job.last_error && (
                    <div className="mt-0.5 text-xs text-text-dim">{t("scheduler.last", "Last: {{time}}", { time: fmtDevice(job.last_run) })}</div>
                  )}
                </div>
                <div className="flex items-center gap-1.5 shrink-0">
                  <Button size="sm" variant="ghost" onClick={() => setEditId(job.id)}>
                    <Edit2 size={13} />
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => deleteJob(job.id)}>
                    <Trash2 size={13} className="text-error" />
                  </Button>
                </div>
              </div>
            )}
          </div>
        ))}
      </SectionCard>
    </>
  );
}
