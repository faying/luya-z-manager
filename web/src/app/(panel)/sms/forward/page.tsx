"use client";
// SMS forwarding (new design). Settings page: status first (forwarding on or
// off, how many rules are live, how far the forwarder got), then three tabs:
// settings, rules, log. The log is fetched only while its tab is open.
//
// Writes (controls-inventory §/sms/forward, design §3.1). One write op serves
// them all (they never overlap); each change is read back from
// GET /api/sms/forward/config or /log:
//   保存设置            tier 2  PUT  /config            readback: config.* equal
//   rule on/off        tier 2  PUT  /rules/toggle      readback: rules[id].enabled
//   创建 / 更新规则      tier 2  POST / PUT /rules       readback: rule with these fields
//   测试规则            tier 2  POST /test {destination} no readback (sends a real message)
//   删除规则            tier 3  DELETE /rules {id}       readback: rule gone        (was window.confirm)
//   重试失败项          tier 2  POST /retry {index} × n  per-entry result from the log
//   清空日志            tier 3  POST /log/clear          readback: log empty        (was window.confirm)
//
// Kept from the already-fixed code (inventory §9, 2c612db): test sends the
// rule's destination; retry sends one {index} per failed entry and keeps
// going past an entry whose destination still fails.
//
// Fixed vs the old page:
// - Log `content_preview` is already decoded text (sms_forward.rs:901-907);
//   the page ran it through decodeSms, which turns a 4- or 8-digit code such
//   as "1234" into garbage (it looks like UCS2 hex). Shown as-is now.
// - Poll interval: the agent rejects < 10 (sms_forward.rs:1119); the input
//   allowed 5. Minimum is 10 and checked before sending.
// - Webhook rules: a new rule carries `headers: []` and editing keeps the
//   rule's existing headers (the agent always serialises them).
// - Log rows have no `id` (the page keyed on one); keyed on sms/rule/time.
// - A log entry's error was only in a hover title; it is shown in the row.
// - `last_forwarded_id` is shown in the status.
// - Pattern / keyword inputs split on every keystroke, so a comma could not
//   be typed; they are kept as text and split on save.
// - Required fields per filter / destination are checked before sending
//   (the agent answered 400 for a missing one).
// - Retry: each entry is looked up again right before its request, so a
//   message forwarded during the retry (the log is newest-first) doesn't
//   shift the index onto the wrong entry.
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, Broom, PencilSimple, Play, Plus, Trash, ArrowCounterClockwise } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { classifyError, useWriteOp, type Tier, type WriteStep } from "@/lib/api/writeOp";
import { fmtDevice } from "@/lib/deviceClock";
import type {
  ForwardDestination,
  ForwardLogEntry,
  ForwardRule,
  SmsFilter,
  SmsForwardConfig,
  SmsForwardConfigResponse,
  SmsForwardLog,
} from "@/lib/api/schemas/sms";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";
import { FieldRow, SelectField, TextField } from "../../router/wifi/fields";
import { NdTabs } from "@/components/nd";

type TabId = "config" | "rules" | "log";
type FilterType = SmsFilter["type"];
type DestType = ForwardDestination["type"];
type T = (k: string, d: string, o?: Record<string, unknown>) => string;

const CFG = "/api/sms/forward/config";
const LOG = "/api/sms/forward/log";
const SEND_TIMEOUT = 30000; // the agent's own HTTP timeout is 15 s

/** Rule being edited: patterns / keywords as the typed text. */
interface Draft {
  id?: number;
  name: string;
  enabled: boolean;
  filterType: FilterType;
  patterns: string;
  keywords: string;
  destination: ForwardDestination;
}

function emptyDest(type: DestType): ForwardDestination {
  switch (type) {
    case "telegram":
      return { type, bot_token: "", chat_id: "", silent: false };
    case "webhook":
      return { type, url: "", method: "POST", headers: [] };
    case "sms":
      return { type, forward_number: "" };
    case "ntfy":
      return { type, url: "", topic: "" };
    case "discord":
      return { type, webhook_url: "" };
    case "slack":
      return { type, webhook_url: "" };
  }
}

const newDraft = (): Draft => ({ name: "", enabled: true, filterType: "all", patterns: "", keywords: "", destination: emptyDest("telegram") });

function draftOf(r: ForwardRule): Draft {
  const f = r.filter;
  return {
    id: r.id,
    name: r.name,
    enabled: r.enabled,
    filterType: f.type,
    patterns: "patterns" in f ? f.patterns.join(", ") : "",
    keywords: "keywords" in f ? f.keywords.join(", ") : "",
    destination: { ...r.destination },
  };
}

const splitList = (s: string) =>
  s
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);

function filterOf(d: Draft): SmsFilter {
  switch (d.filterType) {
    case "all":
      return { type: "all" };
    case "sender":
      return { type: "sender", patterns: splitList(d.patterns) };
    case "content":
      return { type: "content", keywords: splitList(d.keywords) };
    case "sender_and_content":
      return { type: "sender_and_content", patterns: splitList(d.patterns), keywords: splitList(d.keywords) };
  }
}

/** Trimmed destination; an empty ntfy token is left out (it is optional). */
function destOf(d: ForwardDestination): ForwardDestination {
  switch (d.type) {
    case "telegram":
      return { ...d, bot_token: d.bot_token.trim(), chat_id: d.chat_id.trim() };
    case "webhook":
      return { ...d, url: d.url.trim(), headers: d.headers ?? [] };
    case "sms":
      return { ...d, forward_number: d.forward_number.trim() };
    case "ntfy": {
      const { token, ...rest } = d;
      const tk = token?.trim();
      return tk ? { ...rest, url: d.url.trim(), topic: d.topic.trim(), token: tk } : { ...rest, url: d.url.trim(), topic: d.topic.trim() };
    }
    case "discord":
    case "slack":
      return { ...d, webhook_url: d.webhook_url.trim() };
  }
}

function draftProblem(t: T, d: Draft): string | null {
  if (!d.name.trim()) return t("smsfwd.errName", "Give the rule a name.");
  const f = filterOf(d);
  if ((f.type === "sender" || f.type === "sender_and_content") && f.patterns.length === 0)
    return t("smsfwd.errPatterns", "Enter at least one sender pattern.");
  if ((f.type === "content" || f.type === "sender_and_content") && f.keywords.length === 0)
    return t("smsfwd.errKeywords", "Enter at least one keyword.");
  const x = destOf(d.destination);
  const missing =
    (x.type === "telegram" && (!x.bot_token || !x.chat_id)) ||
    (x.type === "webhook" && !x.url) ||
    (x.type === "sms" && !x.forward_number) ||
    (x.type === "ntfy" && (!x.url || !x.topic)) ||
    ((x.type === "discord" || x.type === "slack") && !x.webhook_url);
  return missing ? t("smsfwd.errDest", "Fill in the destination fields.") : null;
}

function filterLabel(t: T, f: FilterType) {
  return {
    all: t("smsfwd.filterAll", "All messages"),
    sender: t("smsfwd.filterSender", "By sender (patterns)"),
    content: t("smsfwd.filterContent", "By content (keywords)"),
    sender_and_content: t("smsfwd.filterBoth", "Sender + Content"),
  }[f];
}

function destLabel(t: T, d: string) {
  const m: Record<string, string> = {
    telegram: "Telegram",
    webhook: "Webhook",
    sms: t("smsfwd.destSms", "SMS Forward"),
    ntfy: "Ntfy",
    discord: "Discord",
    slack: "Slack",
  };
  return m[d] ?? d;
}

/** Where a destination sends to, for confirms (no secrets). */
function destTarget(d: ForwardDestination): string {
  switch (d.type) {
    case "telegram":
      return `Telegram ${d.chat_id}`;
    case "webhook":
      return `${d.method} ${d.url}`;
    case "sms":
      return d.forward_number;
    case "ntfy":
      return `${d.url.replace(/\/$/, "")}/${d.topic}`;
    case "discord":
      return "Discord";
    case "slack":
      return "Slack";
  }
}

const sameJson = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

interface Pending {
  section: "config" | "rules" | "form" | "log";
  /** Which control opened it, for aria-expanded on that control. */
  key: string;
  tier: Tier;
  action: string;
  title: string;
  consequence: ReactNode;
  steps: WriteStep[];
  check?: () => Promise<boolean>;
  onApplied?: () => void;
  danger?: boolean;
}

export default function SMSForwardPage() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<TabId>("config");

  const cfgApi = useApi<SmsForwardConfigResponse>(CFG, { refreshInterval: 30000 });
  const logApi = useApi<SmsForwardLog>(tab === "log" ? LOG : null, { refreshInterval: 30000 });
  const cfg = cfgApi.data?.config;
  const rules = cfg?.rules ?? [];

  const readCfg = async () => {
    const d = await apiFetch<SmsForwardConfigResponse>(CFG);
    await cfgApi.mutate(d, { revalidate: false });
    return d.config;
  };
  const readLog = async () => {
    const d = await apiFetch<SmsForwardLog>(LOG);
    if (tab === "log") await logApi.mutate(d, { revalidate: false });
    return d;
  };

  // ── one write op for every change ──
  const [pending, setPending] = useState<Pending | null>(null);
  const [shownAt, setShownAt] = useState<Pending["section"] | null>(null);
  const runRef = useRef<Pending | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);
  // The op captures its config at start(), which go() calls while `pending`
  // is still set; runRef keeps the running one for the effects below.
  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: pending?.steps ?? [],
    verify: pending?.check
      ? async () => {
          const p = runRef.current;
          const ok = (await p?.check?.()) ?? false;
          if (ok) p?.onApplied?.();
          return ok;
        }
      : undefined,
  });
  const busy = op.busy;
  const phase = op.phase;
  useEffect(() => {
    // Things with no readback (test, retry) still refresh what they touched.
    if (phase === "accepted") {
      runRef.current?.onApplied?.();
      void cfgApi.mutate();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase]);

  function ask(p: Pending) {
    if (busy) return;
    setPending(p);
  }
  function go() {
    if (!pending) return;
    runRef.current = pending;
    setShownAt(pending.section);
    op.start();
    op.confirm();
    setPending(null);
  }

  function confirmHere(section: Pending["section"]) {
    return (
      <>
        {pending?.section === section && pending.tier === 2 && (
          <ConfirmInline
            id={inline.id}
            open
            actionLabel={pending.action}
            consequence={pending.consequence}
            onCancel={() => setPending(null)}
            onConfirm={go}
          />
        )}
        {shownAt === section && op.phase !== "idle" && !(op.phase === "accepted" && runRef.current?.key === "retry") && (
          <div className="mt-2 px-1">
            <OpResult op={op} />
          </div>
        )}
      </>
    );
  }
  const trig = (key: string) => (pending?.key === key && pending.tier === 2 ? inline.triggerProps : {});

  // ── settings ──
  const [draftCfg, setDraftCfg] = useState<Omit<SmsForwardConfig, "rules"> | null>(null);
  const [pollText, setPollText] = useState<string | null>(null);
  const [cfgErr, setCfgErr] = useState<string | null>(null);
  const base = cfg ? { enabled: cfg.enabled, poll_interval_secs: cfg.poll_interval_secs, mark_read_after_forward: cfg.mark_read_after_forward, delete_after_forward: cfg.delete_after_forward } : null;
  const shownCfg = draftCfg ?? base;
  const pollShown = pollText ?? (shownCfg ? String(shownCfg.poll_interval_secs) : "");
  const cfgLocked = !cfg || cfgApi.stale || busy;
  const edit = (patch: Partial<Omit<SmsForwardConfig, "rules">>) => {
    if (!shownCfg) return;
    setDraftCfg({ ...shownCfg, ...patch });
    setCfgErr(null);
  };

  function askSave() {
    if (!shownCfg) return;
    const n = Number(pollShown.trim());
    if (!Number.isInteger(n) || n < 10 || n > 3600) {
      setCfgErr(t("smsfwd.errPoll", "Poll interval: a whole number of seconds from 10 to 3600."));
      return;
    }
    setCfgErr(null);
    const want = { ...shownCfg, poll_interval_secs: n };
    const parts: string[] = [
      want.enabled ? t("smsfwd.cOn", "New messages are forwarded by the enabled rules") : t("smsfwd.cOff", "Nothing is forwarded until you turn it back on"),
      t("smsfwd.cPoll", "checked every {{n}} s", { n }),
    ];
    if (want.mark_read_after_forward) parts.push(t("smsfwd.cMarkRead", "forwarded messages are marked read"));
    const consequence = (
      <>
        {parts.join(t("smsfwd.listSep", ", ")) + t("smsfwd.fullStop", ".")}
        {want.delete_after_forward && (
          <strong className="block">
            {t("smsfwd.cDelete", "Every message that is forwarded successfully is then deleted from the device; it can't be recovered there.")}
          </strong>
        )}
      </>
    );
    ask({
      section: "config",
      key: "save",
      tier: 2,
      action: t("smsfwd.saveSettingsAction", "save forwarding settings"),
      title: t("smsfwd.saveSettings", "Save Settings"),
      consequence,
      steps: [{ label: t("smsfwd.saveSettings", "Save Settings"), run: () => apiFetch(CFG, { method: "PUT", body: want }) }],
      check: async () => {
        const c = await readCfg();
        return (
          c.enabled === want.enabled &&
          c.poll_interval_secs === want.poll_interval_secs &&
          c.mark_read_after_forward === want.mark_read_after_forward &&
          c.delete_after_forward === want.delete_after_forward
        );
      },
      onApplied: () => {
        setDraftCfg(null);
        setPollText(null);
      },
    });
  }

  // ── rules ──
  const [form, setForm] = useState<Draft | null>(null);
  const [formErr, setFormErr] = useState<string | null>(null);

  function askToggle(r: ForwardRule, enabled: boolean) {
    ask({
      section: "rules",
      key: `toggle-${r.id}`,
      tier: 2,
      action: enabled ? t("smsfwd.enableRuleAction", "turn rule {{name}} on", { name: r.name }) : t("smsfwd.disableRuleAction", "turn rule {{name}} off", { name: r.name }),
      title: r.name,
      consequence: enabled
        ? t("smsfwd.enableRuleC", "Matching messages start going to {{dest}}.", { dest: destTarget(r.destination) })
        : t("smsfwd.disableRuleC", "Messages stop going to {{dest}} through this rule.", { dest: destTarget(r.destination) }),
      steps: [{ label: r.name, run: () => apiFetch("/api/sms/forward/rules/toggle", { method: "PUT", body: { id: r.id, enabled } }) }],
      check: async () => (await readCfg()).rules.find((x) => x.id === r.id)?.enabled === enabled,
    });
  }

  function askSaveRule() {
    if (!form) return;
    const problem = draftProblem(t, form);
    if (problem) return setFormErr(problem);
    setFormErr(null);
    const body = { name: form.name.trim(), enabled: form.enabled, filter: filterOf(form), destination: destOf(form.destination) };
    const matches = (r: ForwardRule) =>
      r.name === body.name && r.enabled === body.enabled && sameJson(r.filter, body.filter) && r.destination.type === body.destination.type;
    const consequence = t("smsfwd.saveRuleC", "{{filter}} go to {{dest}}{{off}}.", {
      filter: filterLabel(t, body.filter.type),
      dest: destTarget(body.destination),
      off: body.enabled ? "" : t("smsfwd.whenEnabled", " once the rule is turned on"),
    });
    if (form.id === undefined) {
      const before = rules.filter(matches).length;
      ask({
        section: "form",
        key: "form",
        tier: 2,
        action: t("smsfwd.createAction", "create rule {{name}}", { name: body.name }),
        title: t("smsfwd.newRule", "New Rule"),
        consequence,
        steps: [{ label: t("smsfwd.create", "Create"), run: () => apiFetch("/api/sms/forward/rules", { method: "POST", body }) }],
        check: async () => (await readCfg()).rules.filter(matches).length > before,
        onApplied: () => setForm(null),
      });
    } else {
      const id = form.id;
      ask({
        section: "form",
        key: "form",
        tier: 2,
        action: t("smsfwd.updateAction", "update rule {{name}}", { name: body.name }),
        title: t("smsfwd.editRule", "Edit Rule"),
        consequence,
        steps: [{ label: t("smsfwd.update", "Update"), run: () => apiFetch("/api/sms/forward/rules", { method: "PUT", body: { id, ...body } }) }],
        check: async () => {
          const r = (await readCfg()).rules.find((x) => x.id === id);
          return !!r && matches(r);
        },
        onApplied: () => setForm(null),
      });
    }
  }

  function askTest(r: ForwardRule) {
    ask({
      section: "rules",
      key: `test-${r.id}`,
      tier: 2,
      action: t("smsfwd.testAction", "send a test to {{dest}}", { dest: destTarget(r.destination) }),
      title: t("smsfwd.testRule", "Test rule"),
      consequence:
        r.destination.type === "sms"
          ? t("smsfwd.testSmsC", "The device sends a test SMS to {{n}} over the mobile network; your carrier may charge for it.", { n: r.destination.forward_number })
          : t("smsfwd.testC", "The device sends a test message to {{dest}} now.", { dest: destTarget(r.destination) }),
      steps: [
        {
          label: t("smsfwd.testRule", "Test rule"),
          run: () => apiFetch("/api/sms/forward/test", { method: "POST", body: { destination: r.destination }, timeoutMs: SEND_TIMEOUT }),
        },
      ],
    });
  }

  function askDeleteRule(r: ForwardRule) {
    ask({
      section: "rules",
      key: `delete-${r.id}`,
      tier: 3,
      danger: true,
      action: t("smsfwd.deleteRuleNamed", "Delete rule {{name}}", { name: r.name }),
      title: t("smsfwd.deleteRuleTitle", "Delete rule “{{name}}”?", { name: r.name }),
      consequence: t("smsfwd.deleteRuleC", "Messages stop going to {{dest}} through this rule. The rule's settings are gone; to undo, create it again.", {
        dest: destTarget(r.destination),
      }),
      steps: [{ label: r.name, run: () => apiFetch("/api/sms/forward/rules", { method: "DELETE", body: { id: r.id } }) }],
      check: async () => !(await readCfg()).rules.some((x) => x.id === r.id),
      onApplied: () => {
        if (form?.id === r.id) setForm(null);
      },
    });
  }

  // ── log ──
  const log = logApi.data;
  const failedEntries = (log ?? []).filter((e) => !e.success);
  type RetryNote = { total: number; failing: { sender: string; error: string }[] };
  const [retryNote, setRetryNote] = useState<RetryNote | null>(null);

  function askRetry() {
    // Matched on sms + rule: the agent rewrites an entry's timestamp on every
    // retry, failed or not.
    const targets = failedEntries.map((e) => ({ sms_id: e.sms_id, rule_id: e.rule_id, sender: e.sender }));
    if (!targets.length) return;
    const same = (x: (typeof targets)[number]) => (e: ForwardLogEntry) => !e.success && e.sms_id === x.sms_id && e.rule_id === x.rule_id;
    const steps: WriteStep[] = targets.map((x, i) => ({
      label: t("smsfwd.retryStep", "entry {{n}} ({{sender}})", { n: i + 1, sender: x.sender }),
      run: async () => {
        // Look the entry up again: a newly forwarded message shifts the indices.
        const fresh = await apiFetch<SmsForwardLog>(LOG);
        const index = fresh.findIndex(same(x));
        if (index < 0) return null; // already sent or gone
        try {
          return await apiFetch("/api/sms/forward/retry", { method: "POST", body: { index }, timeoutMs: SEND_TIMEOUT });
        } catch (e) {
          // The destination still refuses (the agent answered 4xx/5xx): carry
          // on with the rest; the log re-read below reports it. Timeouts,
          // dropped connections and an expired login stop the op as usual.
          if (classifyError(e) === "device") return null;
          throw e;
        }
      },
    }));
    ask({
      section: "log",
      key: "retry",
      tier: 2,
      action: t("smsfwd.retryAction", "retry {{n}} failed", { n: targets.length }),
      title: t("smsfwd.retryFailed", "Retry Failed"),
      consequence: t("smsfwd.retryC", "The device sends the {{n}} failed message(s) again to their rules' destinations.", { n: targets.length }),
      steps,
      onApplied: () => {
        // The result comes from the log itself, not from the replies.
        void readLog().then(
          (fresh) => {
            const failing = targets.flatMap((x) => {
              const e = fresh.find(same(x));
              return e ? [{ sender: x.sender, error: e.error ?? "" }] : [];
            });
            setRetryNote({ total: targets.length, failing });
          },
          () => setRetryNote(null),
        );
      },
    });
  }

  function askClear() {
    ask({
      section: "log",
      key: "clear",
      tier: 3,
      danger: true,
      action: t("smsfwd.clearLog", "Clear Log"),
      title: t("smsfwd.clearLogTitle", "Clear the forwarding log?"),
      consequence: t("smsfwd.clearLogC", "All {{n}} log entries are deleted. Failed entries can no longer be retried. This can't be undone.", { n: log?.length ?? 0 }),
      steps: [{ label: t("smsfwd.clearLog", "Clear Log"), run: () => apiFetch("/api/sms/forward/log/clear", { method: "POST", body: {} }) }],
      check: async () => (await readLog()).length === 0,
      onApplied: () => setRetryNote(null),
    });
  }

  // ── status ──
  const enabledRules = rules.filter((r) => r.enabled).length;
  let tone: Tone = "neutral";
  let state: ReactNode = t("smsfwd.loadingNd", "Reading forwarding settings…");
  let reason: ReactNode = null;
  if (!cfg && cfgApi.error) {
    tone = "bad";
    state = t("smsfwd.unreadable", "Can't read forwarding settings");
    reason = cfgApi.error.message;
  } else if (cfg && !cfg.enabled) {
    state = t("smsfwd.stOff", "Forwarding off");
    reason = t("smsfwd.stOffReason", "Messages stay on the device. Turn it on under Settings.");
  } else if (cfg && enabledRules === 0) {
    tone = "warn";
    state = t("smsfwd.stNoRules", "Forwarding on · no rule enabled");
    reason = t("smsfwd.stNoRulesReason", "Nothing is forwarded. Add or turn on a rule under Rules.");
  } else if (cfg) {
    tone = "ok";
    state = t("smsfwd.stOn", "Forwarding on · {{n}} rule(s) enabled", { n: enabledRules });
    reason = t("smsfwd.stOnReason", "Checks for new messages every {{s}} s.", { s: cfg.poll_interval_secs });
  }
  if (cfg && cfgApi.stale) tone = "stale";
  const lastId = cfgApi.data?.last_forwarded_id;

  // ── panels ──
  const settingsPanel = (
    <section aria-labelledby="fwd-settings-title" className="max-w-[720px]">
      <h2 id="fwd-settings-title" className="sr-only">
        {t("smsfwd.forwardSettings", "Forward Settings")}
      </h2>
      <Group stale={cfgApi.stale}>
        {!shownCfg ? (
          [0, 1, 2, 3].map((i) => (
            <div key={i} className="nd-row">
              <span className="nd-skel" style={{ width: "16ch" }} />
            </div>
          ))
        ) : (
          <>
            <Row
              label={t("smsfwd.enableForwarding", "Enable SMS Forwarding")}
              control={
                <Switch label={t("smsfwd.enableForwarding", "Enable SMS Forwarding")} isSelected={shownCfg.enabled} isDisabled={cfgLocked} onChange={(v) => edit({ enabled: v })} />
              }
            />
            <PollRow
              label={t("smsfwd.pollInterval", "Poll Interval (seconds)")}
              sub={t("smsfwd.pollHint", "10 to 3600 seconds")}
              value={pollShown}
              disabled={cfgLocked}
              invalid={!!cfgErr}
              onChange={(v) => {
                setPollText(v);
                if (!draftCfg && shownCfg) setDraftCfg(shownCfg);
                setCfgErr(null);
              }}
            />
            <Row
              label={t("smsfwd.markRead", "Mark read after forwarding")}
              control={
                <Switch
                  label={t("smsfwd.markRead", "Mark read after forwarding")}
                  isSelected={shownCfg.mark_read_after_forward}
                  isDisabled={cfgLocked}
                  onChange={(v) => edit({ mark_read_after_forward: v })}
                />
              }
            />
            <Row
              label={t("smsfwd.deleteAfter", "Delete after forwarding")}
              sub={t("smsfwd.deleteAfterSub", "Deletes each message from the device once it has been forwarded.")}
              control={
                <Switch
                  label={t("smsfwd.deleteAfter", "Delete after forwarding")}
                  isSelected={shownCfg.delete_after_forward}
                  isDisabled={cfgLocked}
                  onChange={(v) => edit({ delete_after_forward: v })}
                />
              }
            />
          </>
        )}
      </Group>
      {cfgErr && (
        <p role="alert" className="mt-2 px-1 nd-error">
          {cfgErr}
        </p>
      )}
      {cfgApi.stale && cfg && (
        <p className="nd-aux mt-2 px-1">
          <Freshness stale lastOkAt={cfgApi.lastOkAt} what={t("smsfwd.settingsWord", "Settings")} />
          {t("smsfwd.refreshToEdit", " — refresh before changing anything.")}
        </p>
      )}
      <div className="mt-3 flex flex-wrap gap-2">
        <span {...trig("save")}>
          <Button onPress={askSave} isDisabled={cfgLocked || (!draftCfg && pollText === null)}>
            {t("smsfwd.saveSettings", "Save Settings")}
          </Button>
        </span>
        {(draftCfg || pollText !== null) && (
          <Button
            variant="secondary"
            isDisabled={busy}
            onPress={() => {
              setDraftCfg(null);
              setPollText(null);
              setCfgErr(null);
            }}
          >
            {t("smsfwd.discard", "Discard changes")}
          </Button>
        )}
      </div>
      {confirmHere("config")}
    </section>
  );

  const rulesPanel = (
    <section aria-labelledby="fwd-rules-title" className="max-w-[720px]">
      <div className="mb-2 flex flex-wrap items-center gap-2">
        <h2 id="fwd-rules-title" className="nd-group-title m-0 flex-1">
          {cfg ? t("smsfwd.rulesCount", "{{count}} rules", { count: rules.length }) : t("smsfwd.tabRules", "Rules")}
        </h2>
        <Button
          variant="secondary"
          size="sm"
          isDisabled={!cfg || busy || (form !== null && form.id === undefined)}
          onPress={() => {
            setForm(newDraft());
            setFormErr(null);
          }}
        >
          <Plus size={18} weight="bold" aria-hidden />
          {t("smsfwd.addRule", "Add Rule")}
        </Button>
      </div>

      {form && form.id === undefined && (
        <RuleForm
          draft={form}
          setDraft={setForm}
          err={formErr}
          busy={busy}
          trigger={trig("form")}
          onSave={askSaveRule}
          onCancel={() => {
            setForm(null);
            setFormErr(null);
            if (pending?.section === "form") setPending(null);
          }}
        />
      )}
      {form && form.id === undefined && confirmHere("form")}

      <div className={`nd-group${cfgApi.stale ? " nd-stale" : ""}`}>
        {!cfg && !cfgApi.error ? (
          [0, 1].map((i) => (
            <div key={i} className="nd-row nd-row--two">
              <span className="nd-skel" style={{ width: "20ch" }} />
            </div>
          ))
        ) : !cfg ? (
          <div className="nd-row text-nd-t2">{t("smsfwd.rulesUnreadable", "Rules can't be shown until the settings are read.")}</div>
        ) : rules.length === 0 ? (
          <div className="nd-row text-nd-t2">{t("smsfwd.noRulesNd", "No rules yet. Use “Add Rule” to forward messages somewhere.")}</div>
        ) : (
          rules.map((r) =>
            form?.id === r.id ? (
              <div key={r.id} className="p-2">
                <RuleForm
                  draft={form}
                  setDraft={setForm}
                  err={formErr}
                  busy={busy}
                  trigger={trig("form")}
                  onSave={askSaveRule}
                  onCancel={() => {
                    setForm(null);
                    setFormErr(null);
                    if (pending?.section === "form") setPending(null);
                  }}
                />
                {confirmHere("form")}
              </div>
            ) : (
              <div key={r.id} className="nd-row nd-row--two flex-wrap gap-y-1">
                <span {...trig(`toggle-${r.id}`)}>
                  <Switch
                    label={t("smsfwd.enableRuleNamed", "Enable rule {{name}}", { name: r.name })}
                    isSelected={r.enabled}
                    isDisabled={busy || cfgApi.stale}
                    onChange={(v) => askToggle(r, v)}
                  />
                </span>
                <span className="nd-row__text min-w-[12ch]">
                  <span className="nd-row__label">{r.name}</span>
                  <span className="nd-row__sub block">
                    {t("smsfwd.filterCol", "Filter")}: {filterLabel(t, r.filter.type)} · {t("smsfwd.destCol", "Dest")}: {destLabel(t, r.destination.type)}
                  </span>
                </span>
                <span className="flex items-center">
                  <span {...trig(`test-${r.id}`)}>
                    <Button
                      variant="ghost"
                      iconOnly
                      isDisabled={busy}
                      aria-label={t("smsfwd.testRuleNamed", "Test rule {{name}}", { name: r.name })}
                      onPress={() => askTest(r)}
                    >
                      <Play size={20} weight="bold" aria-hidden />
                    </Button>
                  </span>
                  <Button
                    variant="ghost"
                    iconOnly
                    isDisabled={busy}
                    aria-label={t("smsfwd.editRuleNamed", "Edit rule {{name}}", { name: r.name })}
                    onPress={() => {
                      setForm(draftOf(r));
                      setFormErr(null);
                    }}
                  >
                    <PencilSimple size={20} weight="bold" aria-hidden />
                  </Button>
                  <Button
                    variant="ghost"
                    iconOnly
                    isDisabled={busy || cfgApi.stale}
                    aria-label={t("smsfwd.deleteRuleNamed", "Delete rule {{name}}", { name: r.name })}
                    onPress={() => askDeleteRule(r)}
                  >
                    <Trash size={20} weight="bold" aria-hidden />
                  </Button>
                </span>
              </div>
            ),
          )
        )}
      </div>
      {confirmHere("rules")}
    </section>
  );

  const logPanel = (
    <section aria-labelledby="fwd-log-title">
      <h2 id="fwd-log-title" className="sr-only">
        {t("smsfwd.tabLog", "Log")}
      </h2>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <Button variant="ghost" size="sm" onPress={() => logApi.mutate()}>
          <ArrowClockwise size={18} weight="bold" aria-hidden />
          {t("common.refresh", "Refresh")}
        </Button>
        <span {...trig("retry")}>
          <Button variant="secondary" size="sm" onPress={askRetry} isDisabled={busy || failedEntries.length === 0 || logApi.stale}>
            <ArrowCounterClockwise size={18} weight="bold" aria-hidden />
            {t("smsfwd.retryFailed", "Retry Failed")}
            {failedEntries.length > 0 && ` (${failedEntries.length})`}
          </Button>
        </span>
        <Button variant="secondary" size="sm" onPress={askClear} isDisabled={busy || !log || log.length === 0}>
          <Broom size={18} weight="bold" aria-hidden />
          {t("smsfwd.clearLog", "Clear Log")}
        </Button>
      </div>
      {confirmHere("log")}
      {retryNote && op.phase === "accepted" && shownAt === "log" && (
        <p className="mb-3 mt-1 grid gap-1 px-1" role={retryNote.failing.length ? "alert" : "status"}>
          {retryNote.failing.length ? (
            <>
              <StatusMark tone="bad">
                {t("smsfwd.retryStillFailing", "{{n}} of {{total}} still failing", { n: retryNote.failing.length, total: retryNote.total })}
              </StatusMark>
              {retryNote.failing.map((f, i) => (
                <span key={i} className="nd-aux break-words">
                  <span className="nd-mono">{f.sender}</span>
                  {f.error ? `: ${f.error}` : ""}
                </span>
              ))}
            </>
          ) : (
            <StatusMark tone="ok">{t("smsfwd.retryAllOk", "All {{n}} sent again", { n: retryNote.total })}</StatusMark>
          )}
        </p>
      )}
      {logApi.stale && log && (
        <p className="nd-aux mb-2 px-1">
          <Freshness stale lastOkAt={logApi.lastOkAt} what={t("smsfwd.logWord", "Log")} />
        </p>
      )}
      <div className={`nd-group${logApi.stale ? " nd-stale" : ""}`}>
        {log === undefined && !logApi.error ? (
          [0, 1, 2].map((i) => (
            <div key={i} className="nd-row nd-row--two">
              <span className="nd-skel" style={{ width: "24ch" }} />
            </div>
          ))
        ) : log === undefined ? (
          <div className="nd-row flex-wrap">
            <span className="flex-1 text-nd-t2">{t("smsfwd.logUnreadable", "Couldn't read the log: {{e}}", { e: logApi.error?.message ?? "" })}</span>
            <Button variant="secondary" size="sm" onPress={() => logApi.mutate()}>
              {t("common.retry", "Retry")}
            </Button>
          </div>
        ) : log.length === 0 ? (
          <div className="nd-row text-nd-t2">{t("smsfwd.noLogNd", "No log entries. Forwarded messages show up here.")}</div>
        ) : (
          log.map((e, i) => <LogRow key={`${e.sms_id}-${e.rule_id}-${e.timestamp}-${i}`} e={e} t={t} />)
        )}
      </div>
    </section>
  );

  const d = pending?.tier === 3 ? pending : null;

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("smsfwd.title", "SMS Forwarding")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("smsfwd.desc", "Automatically forward SMS to Telegram, webhooks, and more.")}</p>

      <div className="grid gap-6">
        <div className="max-w-[720px]">
          <StatusBlock
            tone={tone}
            state={state}
            reason={
              <>
                {reason && <span className="block">{reason}</span>}
                {lastId !== undefined && lastId > 0 && (
                  <span className="block">
                    {t("smsfwd.lastForwarded", "Last processed message")} <span className="nd-mono">#{lastId}</span>
                  </span>
                )}
              </>
            }
            meta={cfg && cfgApi.stale ? <Freshness stale lastOkAt={cfgApi.lastOkAt} what={t("smsfwd.settingsWord", "Settings")} /> : undefined}
            actions={
              !cfg && cfgApi.error ? (
                <Button variant="secondary" size="sm" onPress={() => cfgApi.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              ) : undefined
            }
          />
        </div>

        <NdTabs<TabId>
          label={t("smsfwd.tabsLabel", "Forwarding sections")}
          value={tab}
          onChange={(k) => {
            setTab(k);
            if (pending?.tier === 2) setPending(null);
          }}
          tabs={[
            { id: "config", label: t("smsfwd.tabConfig", "Settings"), content: settingsPanel },
            { id: "rules", label: t("smsfwd.tabRules", "Rules"), content: rulesPanel },
            { id: "log", label: t("smsfwd.tabLog", "Log"), content: logPanel },
          ]}
        />
      </div>

      <ConfirmDialog
        open={d !== null}
        onOpenChange={(o) => !o && setPending(null)}
        title={d?.title ?? ""}
        what={d?.consequence}
        actionLabel={d?.action ?? ""}
        danger={d?.danger}
        onConfirm={go}
      />
    </>
  );
}

// ── pieces ──────────────────────────────────────────────────────────────

function PollRow({
  label,
  sub,
  value,
  disabled,
  invalid,
  onChange,
}: {
  label: string;
  sub: string;
  value: string;
  disabled: boolean;
  invalid: boolean;
  onChange: (v: string) => void;
}) {
  const id = useId();
  return (
    <FieldRow id={id} label={label} sub={sub}>
      <input
        id={id}
        type="number"
        inputMode="numeric"
        min={10}
        max={3600}
        step={1}
        className="nd-field nd-mono"
        value={value}
        disabled={disabled}
        aria-invalid={invalid || undefined}
        onChange={(e) => onChange(e.target.value)}
      />
    </FieldRow>
  );
}

function LogRow({ e, t }: { e: ForwardLogEntry; t: T }) {
  return (
    <div className="nd-row nd-row--two items-start py-3">
      <span className="nd-row__text grid gap-0.5">
        <span className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5">
          <span className="nd-mono font-semibold">{e.sender || "—"}</span>
          <span className="text-[14px]">
            <StatusMark tone={e.success ? "ok" : "bad"}>{e.success ? t("smsfwd.ok", "OK") : t("smsfwd.failed", "Failed")}</StatusMark>
          </span>
          <span className="nd-aux nd-mono ml-auto">{fmtDevice(e.timestamp)}</span>
        </span>
        <span className="line-clamp-2 break-words text-[15px] leading-[22px] text-nd-t2">{e.content_preview}</span>
        <span className="nd-aux">
          {t("smsfwd.colRule", "Rule")} {e.rule_name || "—"} · {t("smsfwd.colDest", "Dest")} {e.destination_type ? destLabel(t, e.destination_type) : "—"}
        </span>
        {!e.success && e.error && <span className="nd-aux break-words text-nd-badT">{e.error}</span>}
      </span>
    </div>
  );
}

function RuleForm({
  draft,
  setDraft,
  err,
  busy,
  trigger,
  onSave,
  onCancel,
}: {
  draft: Draft;
  setDraft: (d: Draft) => void;
  err: string | null;
  busy: boolean;
  trigger: object;
  onSave: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const ids = {
    name: useId(),
    filter: useId(),
    patterns: useId(),
    keywords: useId(),
    dest: useId(),
    a: useId(),
    b: useId(),
    c: useId(),
  };
  const dst = draft.destination;
  const set = (patch: Partial<Draft>) => setDraft({ ...draft, ...patch });
  const setDest = (patch: Record<string, unknown>) => set({ destination: { ...dst, ...patch } as ForwardDestination });
  const isEdit = draft.id !== undefined;

  return (
    <div className="nd-group mb-3">
      <h3 className="nd-row nd-row__label font-semibold">{isEdit ? t("smsfwd.editRule", "Edit Rule") : t("smsfwd.newRule", "New Rule")}</h3>
      <FieldRow id={ids.name} label={t("smsfwd.ruleName", "Rule Name")} stacked>
        <TextField id={ids.name} mono={false} value={draft.name} disabled={busy} placeholder={t("smsfwd.ruleNamePh", "My rule")} onChange={(v) => set({ name: v })} />
      </FieldRow>
      <Row
        label={t("common.enabled", "Enabled")}
        control={<Switch label={t("common.enabled", "Enabled")} isSelected={draft.enabled} isDisabled={busy} onChange={(v) => set({ enabled: v })} />}
      />
      <FieldRow id={ids.filter} label={t("smsfwd.filter", "Filter")} stacked>
        <SelectField
          id={ids.filter}
          value={draft.filterType}
          disabled={busy}
          onChange={(v) => set({ filterType: v as FilterType })}
          options={(["all", "sender", "content", "sender_and_content"] as const).map((f) => ({ value: f, label: filterLabel(t, f) }))}
        />
      </FieldRow>
      {(draft.filterType === "sender" || draft.filterType === "sender_and_content") && (
        <FieldRow id={ids.patterns} label={t("smsfwd.senderPatterns", "Sender patterns (comma-separated)")} stacked>
          <TextField id={ids.patterns} value={draft.patterns} disabled={busy} placeholder="+1234, Carrier" onChange={(v) => set({ patterns: v })} />
        </FieldRow>
      )}
      {(draft.filterType === "content" || draft.filterType === "sender_and_content") && (
        <FieldRow id={ids.keywords} label={t("smsfwd.keywords", "Keywords (comma-separated)")} stacked>
          <TextField id={ids.keywords} mono={false} value={draft.keywords} disabled={busy} placeholder="OTP, 验证码" onChange={(v) => set({ keywords: v })} />
        </FieldRow>
      )}
      <FieldRow id={ids.dest} label={t("smsfwd.destination", "Destination")} stacked>
        <SelectField
          id={ids.dest}
          value={dst.type}
          disabled={busy}
          onChange={(v) => set({ destination: emptyDest(v as DestType) })}
          options={(["telegram", "webhook", "sms", "ntfy", "discord", "slack"] as const).map((x) => ({ value: x, label: destLabel(t, x) }))}
        />
      </FieldRow>

      {dst.type === "telegram" && (
        <>
          <FieldRow id={ids.a} label="Bot Token" stacked>
            <TextField id={ids.a} value={dst.bot_token} disabled={busy} placeholder="123456:ABC-..." onChange={(v) => setDest({ bot_token: v })} />
          </FieldRow>
          <FieldRow id={ids.b} label="Chat ID" stacked>
            <TextField id={ids.b} value={dst.chat_id} disabled={busy} placeholder="-100123456" onChange={(v) => setDest({ chat_id: v })} />
          </FieldRow>
          <Row
            label={t("smsfwd.silent", "Silent notifications")}
            control={<Switch label={t("smsfwd.silent", "Silent notifications")} isSelected={dst.silent} isDisabled={busy} onChange={(v) => setDest({ silent: v })} />}
          />
        </>
      )}
      {dst.type === "webhook" && (
        <>
          <FieldRow id={ids.a} label="URL" stacked>
            <TextField id={ids.a} value={dst.url} disabled={busy} placeholder="https://example.com/hook" onChange={(v) => setDest({ url: v })} />
          </FieldRow>
          <FieldRow id={ids.b} label={t("smsfwd.method", "Method")}>
            <SelectField id={ids.b} value={dst.method} disabled={busy} onChange={(v) => setDest({ method: v })} options={["POST", "PUT", "GET"].map((m) => ({ value: m, label: m }))} />
          </FieldRow>
          {(dst.headers?.length ?? 0) > 0 && (
            <Row label={t("smsfwd.headersKept", "Custom headers")} value={t("smsfwd.headersN", "{{n}} kept as they are", { n: dst.headers.length })} />
          )}
        </>
      )}
      {dst.type === "sms" && (
        <FieldRow id={ids.a} label={t("smsfwd.forwardNumber", "Forward to number")} sub={t("smsfwd.smsCost", "Sent over the mobile network; your carrier may charge for each message.")} stacked>
          <TextField id={ids.a} value={dst.forward_number} disabled={busy} placeholder="+886912345678" onChange={(v) => setDest({ forward_number: v })} />
        </FieldRow>
      )}
      {dst.type === "ntfy" && (
        <>
          <FieldRow id={ids.a} label={t("smsfwd.serverUrl", "Server URL")} stacked>
            <TextField id={ids.a} value={dst.url} disabled={busy} placeholder="https://ntfy.sh" onChange={(v) => setDest({ url: v })} />
          </FieldRow>
          <FieldRow id={ids.b} label={t("smsfwd.topic", "Topic")} stacked>
            <TextField id={ids.b} value={dst.topic} disabled={busy} placeholder="my-topic" onChange={(v) => setDest({ topic: v })} />
          </FieldRow>
          <FieldRow id={ids.c} label={t("smsfwd.tokenOptional", "Token (optional)")} stacked>
            <TextField id={ids.c} value={dst.token ?? ""} disabled={busy} placeholder="tk_..." onChange={(v) => setDest({ token: v })} />
          </FieldRow>
        </>
      )}
      {(dst.type === "discord" || dst.type === "slack") && (
        <FieldRow id={ids.a} label={t("smsfwd.webhookUrl", "Webhook URL")} stacked>
          <TextField id={ids.a} value={dst.webhook_url} disabled={busy} placeholder="https://..." onChange={(v) => setDest({ webhook_url: v })} />
        </FieldRow>
      )}

      <div className="nd-row flex-wrap gap-2">
        {err && (
          <p role="alert" className="w-full nd-error">
            {err}
          </p>
        )}
        <span {...trigger}>
          <Button onPress={onSave} isDisabled={busy} pending={busy}>
            {isEdit ? t("smsfwd.update", "Update") : t("smsfwd.create", "Create")}
          </Button>
        </span>
        <Button variant="secondary" onPress={onCancel} isDisabled={busy}>
          {t("common.cancel", "Cancel")}
        </Button>
      </div>
    </div>
  );
}
