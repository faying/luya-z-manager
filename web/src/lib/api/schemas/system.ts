// Response shapes for the system / alerts / health / scheduler endpoints of
// zte-agent (zte-agent/src/handlers.rs + system.rs, alerts.rs, health.rs,
// scheduler.rs + action.rs). Every handler here builds its JSON itself, so the
// Rust shape is the source of truth; page-side disagreements are noted inline.
// Types only — no runtime code.

/** One row of GET /api/system/top — system.rs:426 `ProcessEntry`. */
export interface ProcessEntry {
  pid: number;
  /** cmdline basename, falling back to /proc/<pid>/stat comm. */
  name: string;
  /** CPU % since the previous /api/system/top call (any caller), one decimal. 0 on the first call. */
  cpu_pct: number;
  rss_kb: number;
  /** "running" | "sleeping" | "disk" | "zombie" | "stopped" | "other". */
  state: string;
  /** Name is in system.rs BLOAT_DAEMONS. */
  is_bloat: boolean;
  // page expects optional `vsz_kb` — the handler never returns it.
}

/**
 * GET /api/system/top — handlers.rs:181 `system_top` → system.rs:468
 * `ProcessTracker::sample`. Top 50 by CPU%; counts cover all processes.
 */
export interface SystemTop {
  processes: ProcessEntry[];
  total_count: number;
  bloat_count: number;
  bloat_cpu_pct: number;
  bloat_rss_kb: number;
}

/**
 * POST /api/system/kill-bloat reply — handlers.rs:187 → system.rs:614
 * `kill_bloat`. Body: `{all: true}` or `{pids: number[]}`.
 *
 * Page disagreement: tools/processes/page.tsx types `killed` as `number[]` and
 * `join(", ")`s it (renders "[object Object]"); it ignores `skipped`.
 */
export interface KillBloatResult {
  killed: { pid: number; name: string }[];
  skipped: { pid: number; name: string }[];
  freed_rss_kb: number;
}

/** One event in GET /api/alerts — alerts.rs:183-194. Newest first, at most 50. */
export interface AlertEvent {
  seq: number;
  /** Device epoch seconds, or null when the clock was not set yet (< 2024-01-01). */
  time: number | null;
  /** Seconds since boot when it happened. */
  uptime: number;
  /** e.g. "agent-crash", "devui-crash", "datad-crash", "wifi-takeover", "sms-failed" (lib/alerts.ts kindLabel). */
  kind: string;
  text: string;
  unread: boolean;
}

/** One line of the alert-SMS log — alerts.rs:208. Newest first, at most 10. */
export interface AlertSmsRecord {
  /** Device epoch seconds (not nulled even before the clock is set). */
  time: number;
  seq: number;
  kind: string;
  /** "sent" | "failed" | "suppressed-rate" | "suppressed-abroad" | "no-number". */
  result: string;
}

/** `sms` block of GET /api/alerts, and the whole `data` of PUT /api/alerts/sms — alerts.rs:218-226, 316. */
export interface AlertsSms {
  configured: boolean;
  number: string | null;
  abroad_allowed: boolean;
  abroad_now: boolean;
  /** Highest seq u60-guard has processed for SMS (sms-done). */
  processed: number;
  sent_24h: number;
  recent: AlertSmsRecord[];
}

/**
 * GET /api/alerts — alerts.rs:262 `alerts_get` → `Alerts::summary`.
 * Mirrors web/src/lib/alerts.ts `AlertsData` (which is what the pages import).
 *
 * Page disagreement: lib/alerts.ts `AlertsData` has no `utc_offset`.
 */
export interface AlertsData {
  events: AlertEvent[];
  unread: number;
  /** The read cursor: every seq <= read has been seen. */
  read: number;
  /** /proc/uptime seconds, null when unreadable. */
  uptime_now: number | null;
  clock_set: boolean;
  /** Seconds the device clock runs ahead of real UTC (clock.rs). Not in lib/alerts.ts. */
  utc_offset: number;
  sms: AlertsSms;
}

/** POST /api/alerts/read reply — alerts.rs:268. Body `{seq}`; cursor never moves back nor past newest. */
export interface AlertsReadResult {
  read: number;
}

/** PUT /api/alerts/sms body — alerts.rs:284. "" number = SMS alerts off. */
export interface AlertsSmsSet {
  number?: string;
  abroad?: boolean;
}

/** One doctor.sh check — health.rs:190-193. */
export interface HealthCheck {
  level: "ok" | "warn" | "bad";
  /** e.g. "boot-sync", "fota", "svc-zte-agent", "screen", "alerts", "disk", "standby". */
  id: string;
  label: string;
  detail: string;
}

/** One crash log — health.rs:173-176. Newest first. */
export interface HealthCrashLog {
  program: string;
  /** "<YYYYmmdd-HHMMSS>-up<uptime>.log" (supervise.sh record_crash). */
  file: string;
  /** mtime, device epoch seconds. */
  time: number;
  size: number;
  /** The log's "status:" line, e.g. "killed by SIGSEGV", "exit 3"; "" when absent. */
  status: string;
}

/**
 * GET /api/health[?refresh=1] — health.rs:184 `health_get` (routed in
 * server.rs:139 with the raw query). The cached result of the last
 * `doctor.sh --tsv` run (every 60 s; `refresh=1` runs it synchronously first,
 * up to 20 s).
 */
export interface Health {
  checks: HealthCheck[];
  bad: number;
  warn: number;
  /** Device epoch of the last run; null before the first run (~20 s after agent start). */
  checked_at: number | null;
  /** Why the last run failed (checks then keep the previous result). */
  error: string | null;
  crashlogs: HealthCrashLog[];
}

/**
 * GET /api/health/crashlog?program=&file= — health.rs:209 `crashlog_get`.
 * `text` is the first 256 KB of the file.
 */
export interface HealthCrashLogText {
  program: string;
  file: string;
  text: string;
}

/** scheduler.rs:38 `Schedule` (serde tag "type"). */
export type SchedulerSchedule =
  | { type: "once"; at: number }
  /** days: 0 = Monday … 6 = Sunday (scheduler.rs:88). time "HH:MM" device local. */
  | { type: "recurring"; time: string; days: number[] };

/** action.rs:34 `Action`. `body` omitted when None. */
export interface SchedulerAction {
  method: string;
  path: string;
  body?: unknown;
}

/** scheduler.rs:51 `Restore`. Replays the action at `time` with `body`. */
export interface SchedulerRestore {
  time: string;
  body?: unknown;
}

/**
 * One job — scheduler.rs:12 `Job`. Element of GET /api/scheduler/jobs and the
 * `data` of POST (201) / PUT /api/scheduler/jobs and PUT /api/scheduler/jobs/toggle.
 *
 * Nullable fields are always present (null); the `?` ones are omitted when None.
 * Page disagreement: scheduler/page.tsx types `last_run?: number` and
 * `last_error?: string` only; it does not know `last_status` / restore fields,
 * and its form never sends `restore`, so PUT (edit) clears an existing restore.
 */
export interface SchedulerJob {
  id: number;
  name: string;
  enabled: boolean;
  schedule: SchedulerSchedule;
  action: SchedulerAction;
  restore?: SchedulerRestore;
  /** Device epoch seconds. */
  last_run: number | null;
  /** HTTP status of the last run; null = never ran or unparseable method. */
  last_status: number | null;
  last_error?: string;
  last_restore: number | null;
  last_restore_status: number | null;
  last_restore_error?: string;
  /** Device epoch seconds. */
  created_at: number;
}

/** POST /api/scheduler/jobs body — scheduler.rs:317. */
export interface SchedulerJobCreate {
  name: string;
  schedule: SchedulerSchedule;
  action: SchedulerAction;
  restore?: SchedulerRestore | null;
}

/** PUT /api/scheduler/jobs body — scheduler.rs:362. All but restore required. */
export interface SchedulerJobUpdate extends SchedulerJobCreate {
  id: number;
  enabled: boolean;
}

/** GET endpoints of this area (keys without query string). */
export interface SystemGetMap {
  "/api/system/top": SystemTop;
  "/api/alerts": AlertsData;
  "/api/health": Health;
  "/api/health/crashlog": HealthCrashLogText;
  "/api/scheduler/jobs": SchedulerJob[];
}
