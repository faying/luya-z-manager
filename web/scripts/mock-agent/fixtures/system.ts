// Mock fixtures: processes (handlers.rs + system.rs), alerts (alerts.rs),
// health (health.rs + touch-ui scripts/doctor.sh), scheduler (scheduler.rs).
//
// Persona: 1 unread alert (a touch-screen crash ~31 h ago, kept in
// shared.alertsUnread — public/status reads the same number), SMS alerts
// configured but suppressed because the device is abroad (Taiwan). Health:
// every watched program OK; the only warn is the unread alert, and it goes
// away on the next doctor run after the alert is marked read. Crash logs are
// all older than 24 h so the "crashes" check stays OK. Two scheduler jobs.
//
// All times are device epochs (local wall time labelled UTC, +8 h).

import type { Route, Ctx } from "../lib.ts";
import { ok, fail, bodyField, clone } from "../lib.ts";
import { shared } from "../shared.ts";
import { deviceNow, deviceBootAt, deviceUptime, UTC_OFFSET } from "./device.ts";
import type {
  ProcessEntry,
  SystemTop,
  KillBloatResult,
  AlertEvent,
  AlertSmsRecord,
  AlertsData,
  AlertsSms,
  HealthCheck,
  HealthCrashLog,
  Health,
  HealthCrashLogText,
  SchedulerJob,
  SchedulerSchedule,
  SchedulerAction,
  SchedulerRestore,
} from "../../../src/lib/api/schemas/system.ts";

const T0 = deviceNow();
const CLOCK_SANE_AFTER = 1_704_067_200;

function pad(n: number): string {
  return n < 10 ? `0${n}` : `${n}`;
}

/** Device epoch → "YYYYmmdd-HHMMSS" (UTC getters: the digits are already local). */
function stamp(ts: number): string {
  const d = new Date(ts * 1000);
  return `${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}-${pad(d.getUTCHours())}${pad(d.getUTCMinutes())}${pad(d.getUTCSeconds())}`;
}

function isObject(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

/** serde `as_u64()`. */
function asU64(v: unknown): number | undefined {
  return typeof v === "number" && Number.isInteger(v) && v >= 0 ? v : undefined;
}

// ── Processes ───────────────────────────────────────────────────────────────

/** system.rs:412 BLOAT_DAEMONS (current list, after fb4aedc). */
const BLOAT_DAEMONS = [
  "zte_topsw_tr069",
  "zte_topsw_tr069_sub",
  "zte_mqtt_sdk_st",
  "zte_topsw_diag",
  "zte_topsw_samba",
  "zte_topsw_nfc",
  "zte_topsw_get_brand",
  "zte_topsw_jwxk_query",
  "zte-topsw-tunnel",
  "zte_dua",
];

interface ProcSeed {
  pid: number;
  name: string;
  rss_kb: number;
  /** Typical CPU % (the sample wobbles around it). */
  cpu: number;
}

// zte_topsw_get_brand is stopped by rc.local on this device, so it is not listed.
const PROCS: ProcSeed[] = [
  { pid: 1, name: "procd", rss_kb: 2_140, cpu: 0 },
  { pid: 312, name: "ubusd", rss_kb: 1_380, cpu: 0.3 },
  { pid: 340, name: "logd", rss_kb: 1_220, cpu: 0 },
  { pid: 611, name: "zte_topsw_daemon", rss_kb: 3_904, cpu: 0.1 },
  { pid: 702, name: "zte_topsw_mc", rss_kb: 9_812, cpu: 0.6 },
  { pid: 718, name: "zte_topsw_wms", rss_kb: 6_020, cpu: 0.1 },
  { pid: 734, name: "zte_dm", rss_kb: 4_388, cpu: 0 },
  { pid: 751, name: "zte_topsw_tr069", rss_kb: 5_216, cpu: 0.2 },
  { pid: 760, name: "zte_mqtt_sdk_st", rss_kb: 7_448, cpu: 0.4 },
  { pid: 774, name: "zte_topsw_diag", rss_kb: 3_120, cpu: 0 },
  { pid: 781, name: "zte_topsw_samba", rss_kb: 2_964, cpu: 0 },
  { pid: 790, name: "zte_topsw_nfc", rss_kb: 2_300, cpu: 0 },
  { pid: 802, name: "zte_dua", rss_kb: 4_096, cpu: 0.1 },
  { pid: 845, name: "zte_topsw_wlan", rss_kb: 5_480, cpu: 0.2 },
  { pid: 862, name: "zte_nwinfo", rss_kb: 6_672, cpu: 0.5 },
  { pid: 901, name: "hostapd", rss_kb: 4_812, cpu: 0.3 },
  { pid: 905, name: "hostapd", rss_kb: 5_036, cpu: 0.4 },
  { pid: 988, name: "dnsmasq", rss_kb: 2_508, cpu: 0.1 },
  { pid: 1004, name: "dropbear", rss_kb: 1_096, cpu: 0 },
  { pid: 1210, name: "sh", rss_kb: 1_020, cpu: 0 },
  { pid: 1224, name: "zte-agent", rss_kb: 11_264, cpu: 1.2 },
  { pid: 1231, name: "sh", rss_kb: 1_016, cpu: 0 },
  { pid: 1245, name: "zwrt-datad", rss_kb: 6_904, cpu: 0.8 },
  { pid: 1260, name: "sh", rss_kb: 1_132, cpu: 0.1 },
  { pid: 1302, name: "u60-uid", rss_kb: 1_048, cpu: 0 },
  { pid: 1318, name: "u60pro-devui", rss_kb: 18_540, cpu: 1.6 },
  { pid: 1406, name: "mihomo", rss_kb: 42_760, cpu: 2.4 },
  { pid: 1455, name: "tailscaled", rss_kb: 31_228, cpu: 0.9 },
  { pid: 19, name: "ksoftirqd/1", rss_kb: 0, cpu: 0.2 },
  { pid: 87, name: "kworker/u16:2", rss_kb: 0, cpu: 0.1 },
  { pid: 9, name: "rcu_preempt", rss_kb: 0, cpu: 0.1 },
];

const killedPids = new Set<number>();
let lastTopAt: number | null = null;

function livingProcs(): ProcSeed[] {
  return PROCS.filter((p) => !killedPids.has(p.pid));
}

function sampleTop(now: number): SystemTop {
  const first = lastTopAt === null;
  lastTopAt = now;
  const t = now / 1000;
  const entries: ProcessEntry[] = livingProcs().map((p, i) => {
    const wob = p.cpu === 0 ? 0 : p.cpu * (0.6 + 0.4 * Math.abs(Math.sin(t / 7 + i)));
    return {
      pid: p.pid,
      name: p.name,
      // First sample after start: no previous ticks → 0 for everyone (system.rs:521-528).
      cpu_pct: first ? 0 : Math.round(wob * 10) / 10,
      rss_kb: p.rss_kb,
      state: p.name === "zte-agent" ? "running" : "sleeping",
      is_bloat: BLOAT_DAEMONS.includes(p.name),
    };
  });
  entries.sort((a, b) => b.cpu_pct - a.cpu_pct);
  const bloat = entries.filter((e) => e.is_bloat);
  return {
    processes: entries.slice(0, 50),
    total_count: entries.length,
    bloat_count: bloat.length,
    bloat_cpu_pct: Math.round(bloat.reduce((s, e) => s + e.cpu_pct, 0) * 10) / 10,
    bloat_rss_kb: bloat.reduce((s, e) => s + e.rss_kb, 0),
  } satisfies SystemTop;
}

function killBloat(pids: number[] | null): KillBloatResult {
  const res: KillBloatResult = { killed: [], skipped: [], freed_rss_kb: 0 };
  const living = livingProcs();
  const targets =
    pids === null
      ? living.filter((p) => BLOAT_DAEMONS.includes(p.name))
      : // Unknown pids are dropped silently (read_proc_name_rss → None).
        pids.map((pid) => living.find((p) => p.pid === pid)).filter((p): p is ProcSeed => !!p);
  for (const p of targets) {
    if (!BLOAT_DAEMONS.includes(p.name)) {
      res.skipped.push({ pid: p.pid, name: p.name });
      continue;
    }
    killedPids.add(p.pid);
    res.freed_rss_kb += p.rss_kb;
    res.killed.push({ pid: p.pid, name: p.name });
  }
  return res;
}

// ── Alerts ──────────────────────────────────────────────────────────────────

const BOOT = deviceBootAt();
const PREV_BOOT_TIME = T0 - 9 * 86400 - 4 * 3600;
const DEVUI_CRASH_AT = T0 - 31 * 3600 - 12 * 60;

/** /data/alerts/queue, oldest first. */
const EVENTS: Omit<AlertEvent, "unread">[] = [
  { seq: 7, time: PREV_BOOT_TIME, uptime: 51_234, kind: "agent-crash", text: "zte-agent killed by SIGABRT" },
  { seq: 8, time: PREV_BOOT_TIME + 312, uptime: 51_546, kind: "wifi-takeover", text: "agent silent 300s, AP off; forcing Wi-Fi on" },
  // Current boot, before SNTP set the clock → time null.
  { seq: 9, time: null, uptime: 41, kind: "datad-crash", text: "zwrt-datad exit 1" },
  { seq: 10, time: DEVUI_CRASH_AT, uptime: DEVUI_CRASH_AT - BOOT, kind: "devui-crash", text: "u60pro-devui killed by SIGSEGV" },
];

/** /data/alerts/sms-log, oldest first. */
const SMS_LOG: AlertSmsRecord[] = [
  { time: PREV_BOOT_TIME + 45, seq: 7, kind: "agent-crash", result: "sent" },
  { time: PREV_BOOT_TIME + 350, seq: 8, kind: "wifi-takeover", result: "suppressed-rate" },
  { time: BOOT + 180, seq: 9, kind: "datad-crash", result: "suppressed-abroad" },
  { time: DEVUI_CRASH_AT + 40, seq: 10, kind: "devui-crash", result: "suppressed-abroad" },
];

const smsCfg = {
  number: "+8612300000000" as string | null,
  abroadAllowed: false,
  /** Scenario engine's abroad flag: roaming in Taiwan. */
  abroadNow: true,
  processed: 10,
};

/**
 * The read cursor, derived from shared.alertsUnread so public/status and this
 * page always agree: the newest `alertsUnread` events are unread.
 */
function readCursor(): number {
  const n = EVENTS.length;
  const unread = Math.max(0, Math.min(n, Math.floor(shared.alertsUnread)));
  return unread >= n ? 0 : EVENTS[n - unread - 1].seq;
}

function validNumber(n: string): boolean {
  const digits = n.startsWith("+") ? n.slice(1) : n;
  return n.length <= 20 && digits.length >= 3 && /^[0-9]+$/.test(digits);
}

function smsBlock(): AlertsSms {
  const now = deviceNow();
  const clockOk = now >= CLOCK_SANE_AFTER;
  const configured = smsCfg.number !== null && validNumber(smsCfg.number);
  return {
    configured,
    number: configured ? smsCfg.number : null,
    abroad_allowed: smsCfg.abroadAllowed,
    abroad_now: smsCfg.abroadNow,
    processed: smsCfg.processed,
    sent_24h: SMS_LOG.filter((r) => r.result === "sent" && (!clockOk || r.time > now - 86_400)).length,
    recent: clone(SMS_LOG.slice(-10).reverse()),
  } satisfies AlertsSms;
}

function alertsSummary(): AlertsData {
  const read = readCursor();
  const events: AlertEvent[] = EVENTS.slice(-50)
    .reverse()
    .map((e) => ({
      seq: e.seq,
      time: e.time !== null && e.time >= CLOCK_SANE_AFTER ? e.time : null,
      uptime: e.uptime,
      kind: e.kind,
      text: e.text,
      unread: e.seq > read,
    }));
  return {
    events,
    unread: events.filter((e) => e.unread).length,
    read,
    uptime_now: deviceUptime(),
    clock_set: deviceNow() >= CLOCK_SANE_AFTER,
    utc_offset: UTC_OFFSET,
    sms: smsBlock(),
  } satisfies AlertsData;
}

// ── Health ──────────────────────────────────────────────────────────────────

interface CrashFile extends HealthCrashLog {
  text: string;
}

function crashFile(program: string, at: number, uptime: number, status: string, tail: string[]): CrashFile {
  const d = new Date(at * 1000);
  const human = `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
  const text =
    [
      `program: ${program}`,
      `status:  ${status}`,
      `time:    ${human} device local (uptime ${uptime}s; unreliable before the network sets the clock)`,
      `--- last 200 lines of /tmp/${program}.log ---`,
      ...tail,
    ].join("\n") + "\n";
  return { program, file: `${stamp(at)}-up${uptime}.log`, time: at, size: text.length, status, text };
}

/** /data/crashlog/<program>/*.log — all older than 24 h. */
const CRASH_FILES: CrashFile[] = [
  crashFile("u60pro-devui", DEVUI_CRASH_AT, DEVUI_CRASH_AT - BOOT, "killed by SIGSEGV", [
    `${stamp(DEVUI_CRASH_AT - 2)} devui: page functions/chill.html -> home`,
    `${stamp(DEVUI_CRASH_AT - 1)} devui: datad sse reconnect`,
    `${stamp(DEVUI_CRASH_AT)} supervise: u60pro-devui ended: killed by SIGSEGV`,
  ]),
  // Written before SNTP: the RTC was still around 1971, so the name and mtime say so.
  crashFile("zwrt-datad", 31_536_000 + 41, 41, "exit 1", [
    "zwrt-datad 0.10.0 starting",
    "error: ubus connect failed: Connection refused (os error 111)",
    "19710101-000041 supervise: zwrt-datad ended: exit 1",
  ]),
  crashFile("zte-agent", PREV_BOOT_TIME, 51_234, "killed by SIGABRT", [
    "[health] doctor run ok",
    "thread 'http-3' panicked at src/server.rs: called `Option::unwrap()` on a `None` value",
    `${stamp(PREV_BOOT_TIME)} supervise: zte-agent ended: killed by SIGABRT`,
  ]),
];

function doctorChecks(now: number): HealthCheck[] {
  const d = new Date(now * 1000);
  const clock = `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`;
  const unread = alertsSummary().unread;
  const recentCrashes = CRASH_FILES.filter((c) => c.time > now - 86_400).length;
  const c = (level: HealthCheck["level"], id: string, label: string, detail: string): HealthCheck => ({ level, id, label, detail });
  return [
    c("ok", "boot-sync", "开机同步", "sync success"),
    c("ok", "fota", "ZTE 自动升级", "已关闭"),
    c("ok", "whiteout", "开机链接", "开机同步名单里的服务没有被屏蔽"),
    c("ok", "clock", "时钟", `已对时（${clock} 设备当地时间）`),
    c("ok", "svc-zte-agent", "高级后台", "procd 监督中"),
    c("ok", "svc-zwrt-datad", "数据服务", "procd 监督中"),
    c("ok", "svc-u60-guard", "Wi-Fi 兜底看门狗", "procd 监督中"),
    c("ok", "svc-u60-uid", "屏幕守护进程", "procd 监督中"),
    c("ok", "agent-http", "管理网页 :9090", "能访问"),
    c("ok", "datad-http", "数据服务 :9460", "能访问"),
    c("ok", "heartbeat", "后台心跳", "4 秒前"),
    shared.wifiOn ? c("ok", "wifi", "Wi-Fi", "在广播") : c("warn", "wifi", "Wi-Fi", "没有在广播（在家情景下正常）"),
    c("ok", "screen", "触屏界面", "运行中"),
    unread > 0 ? c("warn", "alerts", "告警", `${unread} 条未读（管理网页「系统 → 告警」）`) : c("ok", "alerts", "告警", "没有未读"),
    smsBlock().configured ? c("ok", "sms", "短信告警", "已配置") : c("warn", "sms", "短信告警", "没配置号码：后台挂了你不会知道"),
    recentCrashes > 0
      ? c("warn", "crashes", "崩溃记录", `最近 24 小时 ${recentCrashes} 份（/data/crashlog）`)
      : c("ok", "crashes", "崩溃记录", "最近 24 小时没有"),
    c("ok", "disk", "/data 空间", "剩 1843 MB"),
    c("ok", "standby", "待机", "正常（蜂窝每分钟 3 个包）"),
  ];
}

/** Cached doctor snapshot (health.rs LAST). The agent refreshes it every 60 s. */
const snapshot = { checks: [] as HealthCheck[], at: 0 };

function runDoctor(): void {
  const now = deviceNow();
  snapshot.checks = doctorChecks(now);
  snapshot.at = now;
}
runDoctor();

function healthData(): Health {
  if (deviceNow() - snapshot.at >= 60) runDoctor();
  const checks = clone(snapshot.checks);
  return {
    checks,
    bad: checks.filter((x) => x.level === "bad").length,
    warn: checks.filter((x) => x.level === "warn").length,
    checked_at: snapshot.at,
    error: null,
    crashlogs: CRASH_FILES.map(({ text: _t, ...meta }) => meta).sort((a, b) => b.time - a.time),
  } satisfies Health;
}

/** health.rs:140 safe_name. */
function safeName(n: string): boolean {
  return n.length > 0 && n.length <= 96 && !n.startsWith(".") && /^[A-Za-z0-9._-]+$/.test(n);
}

// ── Scheduler ───────────────────────────────────────────────────────────────

/** Most recent device epoch at HH:MM on one of `days` (0 = Monday), at or before `now`. */
function lastOccurrence(now: number, hh: number, mm: number, days: number[]): number | null {
  const midnight = Math.floor(now / 86400) * 86400;
  for (let back = 0; back < 8; back++) {
    const at = midnight - back * 86400 + hh * 3600 + mm * 60;
    const dow = (new Date(at * 1000).getUTCDay() + 6) % 7;
    if (at <= now && days.includes(dow)) return at;
  }
  return null;
}

const ALL_DAYS = [0, 1, 2, 3, 4, 5, 6];
const psRun = lastOccurrence(T0, 0, 30, ALL_DAYS);
const psRestore = lastOccurrence(T0, 7, 30, ALL_DAYS);

const jobs: SchedulerJob[] = [
  {
    id: 1,
    name: "夜间省电",
    enabled: true,
    schedule: { type: "recurring", time: "00:30", days: ALL_DAYS },
    action: { method: "PUT", path: "/api/device/power-save", body: { deviceInfoList: { power_saver_mode: "1" } } },
    restore: { time: "07:30", body: { deviceInfoList: { power_saver_mode: "0" } } },
    last_run: psRun,
    last_status: psRun === null ? null : 200,
    last_restore: psRestore,
    last_restore_status: psRestore === null ? null : 200,
    created_at: T0 - 12 * 86400,
  },
  {
    id: 2,
    name: "每周一凌晨重启",
    enabled: false,
    schedule: { type: "recurring", time: "04:30", days: [0] },
    action: { method: "POST", path: "/api/device/reboot" },
    last_run: null,
    last_status: null,
    last_restore: null,
    last_restore_status: null,
    created_at: T0 - 12 * 86400 + 300,
  },
] satisfies SchedulerJob[];

type Parsed<T> = { ok: true; value: T } | { ok: false; error: string };

function bad<T>(error: string): Parsed<T> {
  return { ok: false, error };
}

/** serde of scheduler.rs Schedule (tag "type"). Errors are "invalid JSON: …" like the agent's. */
function parseSchedule(v: unknown): Parsed<SchedulerSchedule> {
  if (v === undefined) return bad("invalid JSON: missing field `schedule`");
  if (!isObject(v) || typeof v.type !== "string") return bad("invalid JSON: missing field `type`");
  if (v.type === "once") {
    if (v.at === undefined) return bad("invalid JSON: missing field `at`");
    if (typeof v.at !== "number" || !Number.isInteger(v.at)) return bad("invalid JSON: invalid type for `at`, expected i64");
    return { ok: true, value: { type: "once", at: v.at } };
  }
  if (v.type === "recurring") {
    if (v.time === undefined) return bad("invalid JSON: missing field `time`");
    if (typeof v.time !== "string") return bad("invalid JSON: invalid type for `time`, expected a string");
    if (v.days === undefined) return bad("invalid JSON: missing field `days`");
    if (!Array.isArray(v.days) || !v.days.every((d) => typeof d === "number" && Number.isInteger(d) && d >= 0 && d <= 255)) {
      return bad("invalid JSON: invalid type for `days`, expected a sequence of u8");
    }
    return { ok: true, value: { type: "recurring", time: v.time, days: v.days as number[] } };
  }
  return bad(`invalid JSON: unknown variant \`${v.type}\`, expected \`once\` or \`recurring\``);
}

function parseAction(v: unknown): Parsed<SchedulerAction> {
  if (v === undefined) return bad("invalid JSON: missing field `action`");
  if (!isObject(v)) return bad("invalid JSON: invalid type for `action`, expected struct Action");
  if (typeof v.method !== "string") return bad("invalid JSON: missing field `method`");
  if (typeof v.path !== "string") return bad("invalid JSON: missing field `path`");
  const a: SchedulerAction = { method: v.method, path: v.path };
  if (v.body !== undefined && v.body !== null) a.body = v.body;
  return { ok: true, value: a };
}

function parseRestore(v: unknown): Parsed<SchedulerRestore | undefined> {
  if (v === undefined || v === null) return { ok: true, value: undefined };
  if (!isObject(v)) return bad("invalid JSON: invalid type for `restore`, expected struct Restore");
  if (typeof v.time !== "string") return bad("invalid JSON: missing field `time`");
  const r: SchedulerRestore = { time: v.time };
  if (v.body !== undefined && v.body !== null) r.body = v.body;
  return { ok: true, value: r };
}

/** scheduler.rs:93 validate_time. */
function validateTime(s: string): string | null {
  const parts = s.split(":");
  if (parts.length !== 2) return "time must be HH:MM";
  const u8 = (x: string) => (/^\+?[0-9]+$/.test(x) && Number(x) <= 255 ? Number(x) : null);
  const h = u8(parts[0]);
  if (h === null) return "invalid hour";
  const m = u8(parts[1]);
  if (m === null) return "invalid minute";
  if (h > 23) return "hour must be 0-23";
  if (m > 59) return "minute must be 0-59";
  return null;
}

/** scheduler.rs:113 validate_job. */
function validateJob(action: SchedulerAction, schedule: SchedulerSchedule, restore: SchedulerRestore | undefined): string | null {
  if (!action.path.startsWith("/api/")) return "action.path must start with /api/";
  if (action.path.startsWith("/api/scheduler/")) return "cannot schedule scheduler endpoints";
  if (action.path.startsWith("/api/auth/")) return "cannot schedule auth endpoints";
  if (!["GET", "POST", "PUT", "DELETE"].includes(action.method)) return "action.method must be GET, POST, PUT, or DELETE";
  if (schedule.type === "recurring") {
    const e = validateTime(schedule.time);
    if (e) return e;
    if (schedule.days.length === 0) return "days must not be empty";
    if (schedule.days.some((d) => d > 6)) return "days must be 0-6 (Mon-Sun)";
  } else if (schedule.at <= 1_000_000_000) {
    return "once.at must be a valid unix timestamp";
  }
  if (restore) {
    const e = validateTime(restore.time);
    if (e) return e;
  }
  return null;
}

interface JobInput {
  name: string;
  schedule: SchedulerSchedule;
  action: SchedulerAction;
  restore: SchedulerRestore | undefined;
}

function parseJobInput(body: unknown): Parsed<JobInput> {
  if (!isObject(body)) return bad("invalid JSON: expected value");
  if (typeof body.name !== "string") return bad("invalid JSON: missing field `name`");
  const schedule = parseSchedule(body.schedule);
  if (!schedule.ok) return schedule;
  const action = parseAction(body.action);
  if (!action.ok) return action;
  const restore = parseRestore(body.restore);
  if (!restore.ok) return restore;
  return { ok: true, value: { name: body.name, schedule: schedule.value, action: action.value, restore: restore.value } };
}

/** Serde output of a Job: drop None-skipped fields. */
function jobOut(j: SchedulerJob): SchedulerJob {
  const out = clone(j);
  if (out.restore === undefined) delete out.restore;
  if (out.last_error === undefined) delete out.last_error;
  if (out.last_restore_error === undefined) delete out.last_restore_error;
  if (out.action.body === undefined) delete out.action.body;
  return out;
}

function idField(body: unknown): Parsed<number> {
  if (!isObject(body)) return bad("invalid JSON");
  const id = asU64(body.id);
  if (id === undefined) return bad("missing 'id' field");
  return { ok: true, value: id };
}

// ── Routes ──────────────────────────────────────────────────────────────────

export const routes: Route[] = [
  {
    method: "GET",
    path: "/api/system/top",
    handler: (ctx: Ctx) => ok(sampleTop(ctx.now)),
  },
  {
    // handlers.rs:187
    method: "POST",
    path: "/api/system/kill-bloat",
    handler: (ctx: Ctx) => {
      if (ctx.body === undefined) return fail("invalid JSON", 400);
      if (bodyField(ctx.body, "all") === true) return ok(killBloat(null));
      const arr = bodyField(ctx.body, "pids");
      if (Array.isArray(arr)) {
        const ids = arr.map(asU64).filter((n): n is number => n !== undefined);
        if (ids.length === 0) return fail("pids array is empty", 400);
        return ok(killBloat(ids));
      }
      return fail("expected 'all' or 'pids'", 400);
    },
  },
  {
    method: "GET",
    path: "/api/alerts",
    handler: () => ok(alertsSummary()),
  },
  {
    // alerts.rs:268 — body parse failure falls through to the same 400.
    method: "POST",
    path: "/api/alerts/read",
    handler: (ctx: Ctx) => {
      const seq = asU64(bodyField(ctx.body, "seq"));
      if (seq === undefined) return fail('expected {"seq": number}', 400);
      const newest = EVENTS.length ? EVENTS[EVENTS.length - 1].seq : 0;
      const read = Math.max(Math.min(seq, newest), readCursor());
      shared.alertsUnread = EVENTS.filter((e) => e.seq > read).length;
      return ok({ read });
    },
  },
  {
    // alerts.rs:284
    method: "PUT",
    path: "/api/alerts/sms",
    handler: (ctx: Ctx) => {
      if (!isObject(ctx.body)) return fail("invalid JSON", 400);
      const b = ctx.body;
      if ("number" in b) {
        if (typeof b.number !== "string") return fail("number must be a string", 400);
        const n = b.number.trim().replace(/[ -]/g, "");
        if (n === "") smsCfg.number = null;
        else if (validNumber(n)) smsCfg.number = n;
        else return fail("number: digits only, optional leading +, at most 20 characters", 400);
      }
      if ("abroad" in b) {
        if (typeof b.abroad !== "boolean") return fail("abroad must be true or false", 400);
        smsCfg.abroadAllowed = b.abroad;
      }
      return ok(smsBlock());
    },
  },
  {
    // health.rs:184 — refresh=1 runs doctor.sh synchronously (a few seconds on the device).
    method: "GET",
    path: "/api/health",
    handler: (ctx: Ctx) => {
      if (ctx.query.getAll("refresh").includes("1")) {
        runDoctor();
        return { data: healthData(), delayMs: 2500 };
      }
      return ok(healthData());
    },
  },
  {
    // health.rs:209
    method: "GET",
    path: "/api/health/crashlog",
    handler: (ctx: Ctx) => {
      const program = ctx.query.get("program");
      const file = ctx.query.get("file");
      if (program === null || file === null) return fail("program and file are required", 400);
      if (!safeName(program) || !safeName(file) || !file.endsWith(".log")) return fail("bad name", 400);
      const c = CRASH_FILES.find((x) => x.program === program && x.file === file);
      if (!c) return fail("no such crash log", 404);
      return ok({ program, file, text: c.text } satisfies HealthCrashLogText);
    },
  },
  {
    method: "GET",
    path: "/api/scheduler/jobs",
    handler: () => ok(jobs.map(jobOut)),
  },
  {
    // scheduler.rs:324 — 201 Created.
    method: "POST",
    path: "/api/scheduler/jobs",
    handler: (ctx: Ctx) => {
      const p = parseJobInput(ctx.body);
      if (!p.ok) return fail(p.error, 400);
      const { name, schedule, action, restore } = p.value;
      const err = validateJob(action, schedule, restore);
      if (err) return fail(err, 400);
      const job: SchedulerJob = {
        id: jobs.reduce((m, j) => Math.max(m, j.id), 0) + 1,
        name,
        enabled: true,
        schedule,
        action,
        restore,
        last_run: null,
        last_status: null,
        last_restore: null,
        last_restore_status: null,
        created_at: deviceNow(),
      };
      jobs.push(job);
      return { status: 201, data: jobOut(job) };
    },
  },
  {
    // scheduler.rs:371 — full replace; absent restore clears it.
    method: "PUT",
    path: "/api/scheduler/jobs",
    handler: (ctx: Ctx) => {
      if (!isObject(ctx.body)) return fail("invalid JSON: expected value", 400);
      const id = asU64(ctx.body.id);
      if (id === undefined) return fail("invalid JSON: missing field `id`", 400);
      if (typeof ctx.body.enabled !== "boolean") return fail("invalid JSON: missing field `enabled`", 400);
      const enabled = ctx.body.enabled;
      const p = parseJobInput(ctx.body);
      if (!p.ok) return fail(p.error, 400);
      const { name, schedule, action, restore } = p.value;
      const err = validateJob(action, schedule, restore);
      if (err) return fail(err, 400);
      const job = jobs.find((j) => j.id === id);
      if (!job) return fail("job not found", 404);
      job.name = name;
      job.enabled = enabled;
      job.schedule = schedule;
      job.action = action;
      job.restore = restore;
      return ok(jobOut(job));
    },
  },
  {
    // scheduler.rs:398
    method: "DELETE",
    path: "/api/scheduler/jobs",
    handler: (ctx: Ctx) => {
      const id = idField(ctx.body);
      if (!id.ok) return fail(id.error, 400);
      const i = jobs.findIndex((j) => j.id === id.value);
      if (i < 0) return fail("job not found", 404);
      jobs.splice(i, 1);
      return ok();
    },
  },
  {
    // scheduler.rs:420
    method: "PUT",
    path: "/api/scheduler/jobs/toggle",
    handler: (ctx: Ctx) => {
      const id = idField(ctx.body);
      if (!id.ok) return fail(id.error, 400);
      const enabled = bodyField(ctx.body, "enabled");
      if (typeof enabled !== "boolean") return fail("missing 'enabled' field", 400);
      const job = jobs.find((j) => j.id === id.value);
      if (!job) return fail("job not found", 404);
      job.enabled = enabled;
      return ok(jobOut(job));
    },
  },
];
