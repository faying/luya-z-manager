"use client";
// Scenarios (new design). Status first (where the device thinks it is, engine
// on/off, pin), then what can be changed (home networks, nearby scan, abroad,
// what each scenario does, timing), then the run log.
//
// Deliberately NOT a generic action editor. The actions a scenario runs are
// fixed in the config the device ships with; exposing an arbitrary
// method/path/body builder here would turn "walking into a room" into a trigger
// for any API on the box. This page edits a fixed set of things only: which
// networks count as home, how eagerly the device reacts, whether the shipped
// abroad scenario is present, and per scenario two fixed switches — Wi-Fi on
// or off, and which node CHILL's main group uses.
//
// Writes (controls-inventory §/router/scenario, design §3.1):
//   engine on/off, pin/unpin, every config edit, scan  → tier 2 (ConfirmInline)
//   a scenario's Wi-Fi set to off                      → tier 2 local / tier 3 remote
// Every config write is read back from GET /api/scenario (the agent drops
// write-to-disk errors, so a readback is the only proof). Bodies are plain
// objects — apiFetch encodes them once (inventory §9, 7546a4d).
// /api/scenario/scan touches the radios, so it only runs on an explicit,
// confirmed press — never on load or on a timer.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { ArrowClockwise, Plus, PushPin, Trash } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { fmtDevice } from "@/lib/deviceClock";
import { apiFetch } from "@/lib/api/client";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp, type WriteStep } from "@/lib/api/writeOp";
import type {
  ScenarioAction,
  ScenarioConfig,
  ScenarioDef,
  ScenarioLog,
  ScenarioParams,
  ScenarioScan,
  ScenarioScanNetwork,
  ScenarioSsidEntry,
  ScenarioState,
} from "@/lib/api/schemas/scenario";
import type { ChillStatus } from "@/lib/api/schemas/services";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  ConsoleBand,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  Segmented,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const HOME_ID = "home";
const WIFI_PATH = "/api/wifi/radio";
const REGION_PATH = "/api/services/chill/regions";
// Must match CHILL_MAIN_GROUP in scenario.rs and scripts/chill/template.yaml.
const CHILL_GROUP = "🚀 节点选择";
// Used when CHILL is not running and cannot list its own members.
const CHILL_FALLBACK_OPTIONS = ["DIRECT", "🇹🇼 台湾", "🇯🇵 日本", "🇸🇬 新加坡", "🇺🇸 美国"];
// `abroad_scenarios()` in zte-agent/src/scenario.rs ships exactly one.
const SHIPPED_ABROAD = 1;
const AUTO = "__auto";

type Section = "engine" | "pin" | "setup" | "home" | "nearby" | "abroad" | "does" | "timing";

interface Pending {
  section: Section;
  /** Short verb phrase, shown as "Confirm: <action>". */
  action: string;
  consequence: ReactNode;
  tier: 2 | 3;
  steps: WriteStep[];
  /** Readback on GET /api/scenario. */
  check?: (s: ScenarioState) => boolean;
  /** Tier-3 dialog only. */
  dialog?: { title: string; downtime?: string; recovery?: string };
}

// ── config helpers (shape of zte-agent/src/scenario.rs) ────────────────

function wifiOf(s: ScenarioDef): boolean | null {
  const a = s.actions.find((x) => x.path === WIFI_PATH);
  return a ? a.body?.ap_2g !== false : null;
}

function isMainGroup(a: ScenarioAction) {
  return a.path === REGION_PATH && a.body?.group === CHILL_GROUP;
}

function nodeOf(s: ScenarioDef): string {
  const a = s.actions.find(isMainGroup);
  return typeof a?.body?.member === "string" ? a.body.member : "";
}

// Wi-Fi goes first: arriving from home means the APs are down, and anything
// after it may need the network.
function withWifi(s: ScenarioDef, on: boolean): ScenarioDef {
  const rest = s.actions.filter((a) => a.path !== WIFI_PATH);
  const wifi: ScenarioAction = {
    method: "PUT",
    path: WIFI_PATH,
    body: { ap_2g: on, ap_5g: on },
    snapshot: ["wireless.main_2g.disabled", "wireless.main_5g.disabled"],
  };
  return { ...s, actions: [wifi, ...rest] };
}

// Same shape as chill_main_group_action() in scenario.rs: best-effort (CHILL
// may be off), verified by reading the group back, restored on leaving.
function withNode(s: ScenarioDef, member: string): ScenarioDef {
  const rest = s.actions.filter((a) => !isMainGroup(a));
  if (!member) return { ...s, actions: rest };
  const status = "/api/services/chill";
  const active = "/data/region/active";
  return {
    ...s,
    actions: [
      ...rest,
      {
        method: "PUT",
        path: REGION_PATH,
        body: { group: CHILL_GROUP, member },
        verify_field: { path: status, pointer: active, equals: member },
        best_effort: true,
        restore_on_exit: { read_path: status, read_pointer: active, field: "member" },
      },
    ],
  };
}

function isAbroad(s: ScenarioDef) {
  return s.detect.type === "mcc" || s.detect.type === "abroad";
}

function entriesOf(s: ScenarioDef | undefined): ScenarioSsidEntry[] {
  return s && s.detect.type === "ssid" ? s.detect.entries : [];
}

function sameEntry(a: ScenarioSsidEntry, b: ScenarioSsidEntry) {
  return a.ssid === b.ssid && (a.bssid ?? "").toLowerCase() === (b.bssid ?? "").toLowerCase();
}

function homeEntries(s: ScenarioState) {
  return entriesOf(s.config.scenarios.find((x) => x.id === HOME_ID));
}

function when(ts: number | null | undefined) {
  return ts ? fmtDevice(ts) : "—";
}

const PARAM_KEYS: (keyof ScenarioParams)[] = [
  "scan_interval_away_charging_secs",
  "scan_interval_away_battery_secs",
  "scan_interval_home_secs",
  "enter_hits",
  "exit_misses",
  "min_rssi_dbm",
];

// ── page ──────────────────────────────────────────────────────────────

export default function ScenarioPage() {
  const { t } = useTranslation();
  const sc = useApi<ScenarioState>("/api/scenario", { refreshInterval: 10000 });
  // Read only for the node list; no polling (it changes rarely).
  const chill = useApi<ChillStatus>("/api/services/chill");
  const log = useApi<ScenarioLog>("/api/scenario/log", { refreshInterval: 15000 });

  const data = sc.data;
  const cfg = data?.config;
  const configured = (cfg?.scenarios.length ?? 0) > 0;
  const home = cfg?.scenarios.find((s) => s.id === HOME_ID);
  const entries = useMemo(() => entriesOf(home), [home]);
  const nameOf = (id: string) => cfg?.scenarios.find((s) => s.id === id)?.name ?? id;

  // ── one write op for every change on this page (they never overlap) ──
  const [pending, setPending] = useState<Pending | null>(null);
  const [shownAt, setShownAt] = useState<Section | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);
  const check = pending?.check;
  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: pending?.steps ?? [],
    verify: check
      ? async () => {
          const s = await apiFetch<ScenarioState>("/api/scenario");
          await sc.mutate(s, { revalidate: false });
          return check(s);
        }
      : undefined,
  });
  const busy = op.busy;
  const locked = !data || sc.stale || busy;

  const { phase } = op;
  const mutateLog = log.mutate;
  useEffect(() => {
    if (phase === "applied" || phase === "accepted") void mutateLog();
  }, [phase, mutateLog]);

  // Stable, so ConfirmInline doesn't re-focus Cancel on every poll re-render.
  const cancelPending = useCallback(() => {
    setPending(null);
  }, []);

  function ask(p: Pending) {
    if (busy) return;
    setPending(p);
  }
  function go() {
    if (!pending) return;
    setShownAt(pending.section);
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }
  const trig = (section: Section) =>
    pending?.section === section && pending.tier === 2 ? inline.triggerProps : {};

  function putConfig(next: ScenarioConfig): WriteStep {
    return {
      label: t("scenario.stepSave", "Save scenarios"),
      run: () => apiFetch("/api/scenario", { method: "PUT", body: next }),
    };
  }

  function withEntries(next: ScenarioSsidEntry[]): ScenarioConfig | null {
    if (!cfg || !home) return null;
    return {
      ...cfg,
      scenarios: cfg.scenarios.map((s) => (s.id === HOME_ID ? { ...s, detect: { type: "ssid", entries: next } } : s)),
    };
  }

  // ── engine ──
  function askEngine(on: boolean) {
    ask({
      section: "engine",
      tier: 2,
      action: on ? t("scenario.engineTurnOn", "turn the engine on") : t("scenario.engineTurnOff", "turn the engine off"),
      consequence: on
        ? t("scenario.engineOnConsequence", "The device starts switching scenarios by itself. When it recognises home it switches its own Wi-Fi off.")
        : t("scenario.engineOffConsequence", "The device stops switching scenarios and goes back to the away settings: Wi-Fi on, a changed CHILL node put back. This can take up to a minute."),
      steps: [
        {
          label: t("scenario.engineOn", "Engine on"),
          run: () => apiFetch("/api/scenario/enabled", { method: "PUT", body: { enabled: on } }),
        },
      ],
      check: (s) => s.enabled === on,
    });
  }

  // ── pin ──
  function askPin(id: string | null) {
    ask({
      section: "pin",
      tier: 2,
      action: id ? t("scenario.pinTo", "pin to “{{name}}”", { name: nameOf(id) }) : t("scenario.unpinAction", "clear the pin"),
      consequence: id
        ? t("scenario.pinConsequence", "The device switches to “{{name}}” now and stays there whatever it sees, also after a reboot.", { name: nameOf(id) })
        : t("scenario.unpinConsequence", "The device goes back to choosing the scenario from what it sees."),
      steps: [
        {
          label: t("scenario.pin", "Pinned to"),
          run: () => apiFetch("/api/scenario/pin", { method: "POST", body: { id } }),
        },
      ],
      check: (s) => s.pin === id,
    });
  }

  // ── home networks ──
  const [listErr, setListErr] = useState<string | null>(null);
  function askAddEntry(e: ScenarioSsidEntry) {
    setListErr(null);
    const ssid = e.ssid.trim();
    if (!ssid) return;
    const entry: ScenarioSsidEntry = e.bssid ? { ssid, bssid: e.bssid } : { ssid };
    if (entries.some((x) => sameEntry(x, entry))) {
      setListErr(t("scenario.alreadyListed", "Already in the list"));
      return;
    }
    const next = withEntries([...entries, entry]);
    if (!next) return;
    ask({
      section: "nearby",
      tier: 2,
      action: t("scenario.addAria", "Add {{ssid}}", { ssid }),
      consequence: t("scenario.addConsequence", "Whenever the device sees “{{ssid}}” it counts as home and switches its own Wi-Fi off.", { ssid }),
      steps: [putConfig(next)],
      check: (s) => homeEntries(s).some((x) => sameEntry(x, entry)),
    });
  }
  function askRemoveEntry(e: ScenarioSsidEntry) {
    const next = withEntries(entries.filter((x) => !sameEntry(x, e)));
    if (!next) return;
    ask({
      section: "home",
      tier: 2,
      action: t("scenario.removeAria", "Remove {{ssid}}", { ssid: e.ssid }),
      consequence: t("scenario.removeConsequence", "“{{ssid}}” no longer counts as home. You can add it again from a scan.", { ssid: e.ssid }),
      steps: [putConfig(next)],
      check: (s) => !homeEntries(s).some((x) => sameEntry(x, e)),
    });
  }

  // ── set-up and abroad: template read + save (two steps, R10) ──
  const tplRef = useRef<ScenarioConfig | null>(null);
  const readTemplate: WriteStep = {
    label: t("scenario.stepTemplate", "Read the template"),
    run: async () => {
      tplRef.current = await apiFetch<ScenarioConfig>("/api/scenario/template");
    },
  };
  function askCreateDefaults() {
    ask({
      section: "setup",
      tier: 2,
      action: t("scenario.createDefaults", "Create defaults"),
      consequence: t("scenario.notSetUpDesc", "Create the two default scenarios — at home and away. Nothing happens until you add a home network, so this is safe to do now."),
      steps: [
        readTemplate,
        {
          label: t("scenario.stepSave", "Save scenarios"),
          run: () => apiFetch("/api/scenario", { method: "PUT", body: tplRef.current }),
        },
      ],
      check: (s) => s.config.scenarios.length > 0,
    });
  }
  // Append whichever shipped abroad scenarios are missing. Never rewrites what
  // is already there — the home networks in particular took effort to pick.
  function askAddAbroad() {
    if (!cfg) return;
    const base = cfg;
    ask({
      section: "abroad",
      tier: 2,
      action: t("scenario.addAbroad", "Add abroad scenario"),
      consequence: t("scenario.addAbroadConsequence", "With a foreign SIM in the slot, CHILL's main group goes direct on arrival and comes back when you are home. Your home networks are kept."),
      steps: [
        readTemplate,
        {
          label: t("scenario.stepSave", "Save scenarios"),
          run: () => {
            const have = new Set(base.scenarios.map((s) => s.id));
            const missing = (tplRef.current?.scenarios ?? []).filter((s) => isAbroad(s) && !have.has(s.id));
            return apiFetch("/api/scenario", { method: "PUT", body: { ...base, scenarios: [...base.scenarios, ...missing] } });
          },
        },
      ],
      check: (s) => s.config.scenarios.some(isAbroad),
    });
  }

  // ── what each scenario does ──
  function editStep(next: ScenarioDef): WriteStep | null {
    if (!cfg) return null;
    return putConfig({ ...cfg, scenarios: cfg.scenarios.map((s) => (s.id === next.id ? next : s)) });
  }
  function askWifi(s: ScenarioDef, on: boolean) {
    const step = editStep(withWifi(s, on));
    if (!step) return;
    // Setting a scenario's Wi-Fi to off makes the device switch Wi-Fi off by
    // itself later: a Wi-Fi switch in effect (§3.1: tier 2, tier 3 remote).
    const tier = !on && isRemoteAccess() ? 3 : 2;
    const consequence = on
      ? t("scenario.wifiOnConsequence", "On entering “{{name}}” the device switches its Wi-Fi on. Nothing changes right now.", { name: s.name })
      : t("scenario.wifiOffConsequence", "On entering “{{name}}” the device switches its own Wi-Fi off; everything connected to it drops off. Nothing changes right now.", { name: s.name });
    ask({
      section: "does",
      tier,
      action: on
        ? t("scenario.wifiOnAction", "Wi-Fi on in “{{name}}”", { name: s.name })
        : t("scenario.wifiOffAction", "Wi-Fi off in “{{name}}”", { name: s.name }),
      consequence,
      steps: [step],
      check: (st) => {
        const x = st.config.scenarios.find((y) => y.id === s.id);
        return !!x && (wifiOf(x) !== false) === on;
      },
      dialog: {
        title: t("scenario.wifiOffTitle", "Switch Wi-Fi off in “{{name}}”?", { name: s.name }),
        downtime: t("scenario.wifiOffDowntime", "From the moment the device enters “{{name}}” until it leaves it.", { name: s.name }),
        recovery: t("scenario.wifiOffRecovery", "Turn this switch back on, or turn the engine off (it puts Wi-Fi back on). Over Tailscale the page stays reachable through the mobile connection."),
      },
    });
  }
  function askNode(s: ScenarioDef, member: string) {
    const step = editStep(withNode(s, member));
    if (!step) return;
    const label = member ? nodeLabel(member) : t("scenario.nodeUnchanged", "leave as is");
    ask({
      section: "does",
      tier: 2,
      action: t("scenario.nodeAction", "CHILL node in “{{name}}”: {{node}}", { name: s.name, node: label }),
      consequence: member
        ? t("scenario.nodeConsequence", "On entering “{{name}}” CHILL's main group switches to {{node}}, and back on leaving. Nothing changes right now.", { name: s.name, node: label })
        : t("scenario.nodeClearConsequence", "Entering “{{name}}” no longer touches CHILL's node.", { name: s.name }),
      steps: [step],
      check: (st) => {
        const x = st.config.scenarios.find((y) => y.id === s.id);
        return !!x && nodeOf(x) === member;
      },
    });
  }
  const nodeLabel = (o: string) => (o === "DIRECT" ? t("scenario.direct", "Direct") : o);

  // ── timing ──
  // Seeded from the device, then edited locally (the draft) so a background
  // refresh cannot yank a half-typed number out from under you.
  const [draft, setForm] = useState<Record<keyof ScenarioParams, string> | null>(null);
  const [formErr, setFormErr] = useState<string | null>(null);
  const form =
    draft ??
    (cfg?.params
      ? (Object.fromEntries(PARAM_KEYS.map((k) => [k, String(cfg.params[k])])) as Record<keyof ScenarioParams, string>)
      : null);
  function askParams() {
    if (!cfg || !form) return;
    setFormErr(null);
    const p = Object.fromEntries(PARAM_KEYS.map((k) => [k, Number(form[k].trim() === "" ? NaN : form[k])])) as unknown as ScenarioParams;
    if (Object.values(p).some((v) => !Number.isFinite(v))) {
      setFormErr(t("scenario.badNumber", "Every field must be a number"));
      return;
    }
    if (p.enter_hits < 1 || p.exit_misses < 1) {
      setFormErr(t("scenario.minOne", "Confirmations must be at least 1"));
      return;
    }
    ask({
      section: "timing",
      tier: 2,
      action: t("scenario.saveTiming", "save the timing"),
      consequence: t("scenario.timingConsequence", "The device scans and decides with these numbers from its next scan on."),
      steps: [putConfig({ ...cfg, params: p })],
      check: (s) => PARAM_KEYS.every((k) => s.config.params[k] === p[k]),
    });
  }

  // ── scan (GET with device side effects: user-triggered, confirmed) ──
  const [scanAsk, setScanAsk] = useState(false);
  const cancelScan = useCallback(() => setScanAsk(false), []);
  const scanInline = useConfirmInline(scanAsk);
  const [scanned, setScanned] = useState<ScenarioScanNetwork[] | null>(null);
  const scanOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("scenario.scan", "Scan"),
        run: async () => {
          const r = await apiFetch<ScenarioScan>("/api/scenario/scan");
          setScanned(r.networks ?? []);
        },
      },
    ],
  });
  // Hide what is already listed.
  const pickable = useMemo(
    () => (scanned ?? []).filter((n) => !entries.some((e) => sameEntry(e, n))),
    [scanned, entries],
  );

  const nameless = entries.filter((e) => !e.bssid);
  const abroad = cfg?.scenarios.filter(isAbroad) ?? [];
  const nodeOptions = chill.data?.region?.options?.length ? chill.data.region.options : CHILL_FALLBACK_OPTIONS;

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("scenario.loading", "Reading scenarios…");
  let reason: ReactNode = null;
  if (!data && sc.error) {
    tone = "bad";
    state = t("scenario.unreadable", "Can't read scenarios");
    reason = sc.error.message;
  } else if (data && !configured) {
    state = t("scenario.noneYet", "No scenarios yet");
    reason = t("scenario.noneYetNext", "Create the defaults below, then add your home network.");
  } else if (data && !data.enabled) {
    state = t("scenario.engineOffState", "Engine off");
    reason = t("scenario.engineOffReason", "The device is not switching scenarios by itself. Wi-Fi stays on.");
  } else if (data) {
    const cur = data.current ? nameOf(data.current) : null;
    state = cur ?? t("scenario.notDecided", "Not decided yet");
    tone = cur ? "ok" : "neutral";
    if (data.guard_takeover) {
      tone = "warn";
      reason = t("scenario.guardTakeover", "The Wi-Fi watchdog took over while the agent was not responding. Staying in away; the pin is paused until 10 minutes of steady running.");
    } else if (data.last_error) {
      tone = "warn";
      reason = data.last_error;
    } else if (data.pin) {
      reason = t("scenario.pinnedReason", "Pinned to “{{name}}” — detection is not changing it.", { name: nameOf(data.pin) });
    }
  }
  if (data && sc.stale) tone = "stale";
  const candidateText =
    data?.candidate && data.candidate !== data.current
      ? t("scenario.pending", "Seeing {{name}} — {{hits}} of {{need}} confirmations so far.", {
          name: nameOf(data.candidate),
          hits: data.hits,
          need: cfg?.params.enter_hits ?? 2,
        })
      : null;

  const pinValue = data ? data.pin ?? AUTO : null;

  // Renders the tier-2 confirm (or nothing) for a section, plus that section's last result.
  function confirmHere(section: Section) {
    return (
      <>
        {pending?.section === section && pending.tier === 2 && (
          <ConfirmInline
            id={inline.id}
            open
            actionLabel={pending.action}
            consequence={pending.consequence}
            onCancel={cancelPending}
            onConfirm={go}
          />
        )}
        {shownAt === section && (
          <div className="mt-2 px-1">
            <OpResult op={op} />
          </div>
        )}
      </>
    );
  }

  const staleNote = sc.stale && data && (
    <p className="nd-aux mt-1 px-1">
      <Freshness stale lastOkAt={sc.lastOkAt} what={t("scenario.settingsWord", "Settings")} />
      {t("scenario.refreshToEdit", " — refresh before changing anything.")}
    </p>
  );

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("scenario.title", "Scenarios")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">
        {t("scenario.desc", "The device watches for networks you name here and reconfigures itself when it recognises where it is.")}
      </p>

      <div className="grid max-w-[720px] gap-6">
        {/* ── status ── */}
        <div>
          <StatusBlock
            tone={tone}
            state={state}
            reason={reason}
            meta={
              <>
                {candidateText}
                {data && sc.stale && (
                  <>
                    {candidateText ? " · " : null}
                    <Freshness stale lastOkAt={sc.lastOkAt} />
                  </>
                )}
              </>
            }
            actions={
              !data && sc.error ? (
                <Button variant="secondary" size="sm" onPress={() => sc.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              ) : undefined
            }
          />
        </div>

        {data && configured && (
          <>
            <section>
              <Group title={t("scenario.statusTitle", "Right now")} stale={sc.stale}>
                <Row label={t("scenario.current", "Scenario")} value={data.current ? nameOf(data.current) : "—"} />
                <Row label={t("scenario.lastSwitch", "Last change")} value={when(data.last_switch)} mono />
                <Row label={t("scenario.lastScan", "Last scan")} value={when(data.last_scan)} mono />
                <Row
                  label={t("scenario.simMcc", "SIM country code")}
                  value={data.sim_mcc ?? t("scenario.simUnknown", "unknown")}
                  mono={!!data.sim_mcc}
                />
                <Row
                  label={t("scenario.engineOn", "Engine on")}
                  sub={t("scenario.engineSub", "Off: the device stays in away and never switches Wi-Fi off.")}
                  control={
                    <span {...trig("engine")}>
                      <Switch
                        label={t("scenario.engineOn", "Engine on")}
                        isSelected={data.enabled}
                        isDisabled={locked}
                        onChange={askEngine}
                      />
                    </span>
                  }
                />
              </Group>
              {confirmHere("engine")}
              {staleNote}
            </section>

            {/* ── pin ── */}
            <section>
              <Group title={t("scenario.pinTitle", "Pin a scenario")} stale={sc.stale}>
                <Row
                  icon={PushPin}
                  label={t("scenario.pin", "Pinned to")}
                  value={data.pin ? nameOf(data.pin) : t("scenario.notPinned", "auto")}
                  control={
                    data.pin ? (
                      <span {...trig("pin")}>
                        <Button variant="secondary" size="sm" isDisabled={locked} onPress={() => askPin(null)}>
                          {t("scenario.unpin", "Clear pin")}
                        </Button>
                      </span>
                    ) : undefined
                  }
                />
                <div className="grid gap-2 px-4 py-3 lg:px-5">
                  <div {...trig("pin")}>
                    <Segmented<string>
                      label={t("scenario.pinTitle", "Pin a scenario")}
                      value={pinValue}
                      isDisabled={locked}
                      onChange={(v) => askPin(v === AUTO ? null : v)}
                      options={[
                        { id: AUTO, label: t("scenario.autoOption", "Auto") },
                        ...(cfg?.scenarios ?? []).map((s) => ({ id: s.id, label: s.name })),
                      ]}
                    />
                  </div>
                  <p className="nd-aux">
                    {t("scenario.pinHelp", "Pinning holds one scenario regardless of what the device sees. It survives a reboot — use it if detection is misbehaving.")}
                  </p>
                </div>
              </Group>
              {confirmHere("pin")}
            </section>

            {/* ── home networks ── */}
            <section>
              <GroupTitle>{t("scenario.homeTitle", "Networks that mean “at home”")}</GroupTitle>
              <p className="nd-aux -mt-1 mb-3 px-1">
                {t("scenario.homeDesc", "Seeing any one of these turns the device's own Wi-Fi off so your phone moves to the house network.")}
              </p>
              <div className={`nd-group${sc.stale ? " nd-stale" : ""}`}>
                {entries.length === 0 ? (
                  <div className="nd-row text-nd-t2">
                    {t("scenario.noEntries", "Nothing listed — the device will never decide it is at home.")}{" "}
                    {t("scenario.noEntriesNext", "Scan below and add your home network.")}
                  </div>
                ) : (
                  entries.map((e) => (
                    <Row
                      key={`${e.ssid}|${e.bssid ?? ""}`}
                      label={<span className="nd-mono">{e.ssid}</span>}
                      sub={e.bssid ? <span className="nd-mono">{e.bssid}</span> : t("scenario.anyRadio", "any radio with this name")}
                      control={
                        <span {...trig("home")}>
                          <Button
                            variant="ghost"
                            iconOnly
                            isDisabled={locked}
                            aria-label={t("scenario.removeAria", "Remove {{ssid}}", { ssid: e.ssid })}
                            onPress={() => askRemoveEntry(e)}
                          >
                            <Trash size={20} weight="bold" aria-hidden />
                          </Button>
                        </span>
                      }
                    />
                  ))
                )}
              </div>
              {nameless.length > 0 && (
                <div className="mt-3 rounded-nd-card bg-nd-washW px-4 py-3">
                  <StatusMark tone="warn">
                    {t(
                      "scenario.noBssidWarning",
                      "{{count}} entry matches on name alone. Anyone can broadcast that name and your Wi-Fi would switch off — pick the network from a scan instead, which records its hardware address too.",
                      { count: nameless.length },
                    )}
                  </StatusMark>
                </div>
              )}
              {confirmHere("home")}
            </section>

            {/* ── nearby (scan) ── */}
            <section>
              <GroupTitle>{t("scenario.nearbyTitle", "Nearby networks")}</GroupTitle>
              <p className="nd-aux -mt-1 mb-3 px-1">
                {t("scenario.nearbyDesc", "Adding from a scan records the hardware address, so a network merely using the same name will not fool it.")}
              </p>
              <div className="mb-3 flex flex-wrap items-center gap-3">
                <Button
                  variant="secondary"
                  onPress={() => setScanAsk((o) => !o)}
                  isDisabled={scanOp.busy || busy}
                  pending={scanOp.busy}
                  {...scanInline.triggerProps}
                >
                  <ArrowClockwise size={20} weight="bold" aria-hidden />
                  {scanOp.busy ? t("scenario.scanning", "Scanning…") : t("scenario.scan", "Scan")}
                </Button>
                <span className="nd-aux">{t("scenario.scanWhat", "Uses the radios for a few seconds.")}</span>
              </div>
              <ConfirmInline
                id={scanInline.id}
                open={scanAsk}
                actionLabel={t("scenario.scanAction", "scan now")}
                consequence={t(
                  "scenario.scanConsequence",
                  "The device scans for nearby Wi-Fi, usually 5–30 seconds. If no radio is free it adds a temporary scan interface and removes it afterwards. Devices on the U60's Wi-Fi may slow down briefly.",
                )}
                onCancel={cancelScan}
                onConfirm={() => {
                  setScanAsk(false);
                  scanOp.start();
                  scanOp.confirm();
                }}
              />
              <div className="nd-group mt-3">
                {scanOp.phase === "failed" || scanOp.phase === "unknown" ? (
                  <div className="nd-row flex-wrap" role="alert">
                    <span className="flex-1">
                      <StatusMark tone="bad">
                        {scanOp.phase === "unknown"
                          ? t("scenario.scanNoReply", "The scan got no answer in time. Try again in a moment.")
                          : t("scenario.scanFailed", "Scan failed: {{e}}", { e: scanOp.error ?? "" })}
                      </StatusMark>
                    </span>
                  </div>
                ) : null}
                {scanned === null ? (
                  scanOp.busy ? (
                    <div className="nd-row text-nd-t2" role="status">{t("scenario.scanning", "Scanning…")}</div>
                  ) : (
                    <div className="nd-row text-nd-t2">{t("scenario.tapScan", "Scan to list what the device can see.")}</div>
                  )
                ) : pickable.length === 0 ? (
                  <div className="nd-row text-nd-t2">
                    {scanned.length === 0
                      ? t("scenario.noNetworks", "Nothing found.")
                      : t("scenario.allListed", "Everything nearby is already listed.")}
                  </div>
                ) : (
                  <div className="max-h-[480px] overflow-y-auto">
                    {pickable.map((n) => (
                      <Row
                        key={`${n.bssid}|${n.ssid}`}
                        label={<span className="nd-mono">{n.ssid}</span>}
                        sub={<span className="nd-mono">{n.bssid}</span>}
                        value={`${n.signal} dBm`}
                        mono
                        control={
                          <span {...trig("nearby")}>
                            <Button
                              variant="ghost"
                              iconOnly
                              isDisabled={locked}
                              aria-label={t("scenario.addAria", "Add {{ssid}}", { ssid: n.ssid })}
                              onPress={() => askAddEntry({ ssid: n.ssid, bssid: n.bssid })}
                            >
                              <Plus size={20} weight="bold" aria-hidden />
                            </Button>
                          </span>
                        }
                      />
                    ))}
                  </div>
                )}
              </div>
              {listErr && (
                <p className="mt-2 px-1" role="alert">
                  <StatusMark tone="bad">{listErr}</StatusMark>
                </p>
              )}
              {confirmHere("nearby")}
            </section>

            {/* ── abroad ── */}
            <section>
              <GroupTitle>{t("scenario.abroadTitle", "Abroad")}</GroupTitle>
              <p className="nd-aux -mt-1 mb-3 px-1">
                {t(
                  "scenario.abroadDesc",
                  "Decided by the SIM in the slot, not by where the signal comes from: with a foreign SIM or eSIM profile, CHILL's main group goes direct. A home SIM roaming abroad changes nothing.",
                )}
              </p>
              <div className={`nd-group${sc.stale ? " nd-stale" : ""}`}>
                {abroad.length === 0 ? (
                  <div className="nd-row text-nd-t2">{t("scenario.noAbroad", "Not set up — a foreign SIM is treated as away.")}</div>
                ) : (
                  abroad.map((s) => (
                    <Row
                      key={s.id}
                      label={s.name}
                      value={s.detect.type === "mcc" ? s.detect.mccs.join(" ") : t("scenario.otherMcc", "any foreign SIM")}
                      mono={s.detect.type === "mcc"}
                    />
                  ))
                )}
                {(data.pending_restore ?? []).map((p) => (
                  <Row key={p.key} label={t("scenario.willRestore", "Restores on return:")} value={restoreText(t, p.key, p.body)} />
                ))}
              </div>
              {abroad.length < SHIPPED_ABROAD && (
                <div className="mt-3">
                  <span {...trig("abroad")}>
                    <Button variant="secondary" isDisabled={locked} onPress={askAddAbroad}>
                      <Plus size={20} weight="bold" aria-hidden />
                      {t("scenario.addAbroad", "Add abroad scenario")}
                    </Button>
                  </span>
                </div>
              )}
              <p className="nd-aux mt-3 px-1">
                {t(
                  "scenario.abroadNote",
                  "Only the “🚀 节点选择” group is set to DIRECT, once, on arrival; groups that do not offer DIRECT (such as AI) keep their node. Changing it by hand afterwards sticks. When you are back on a home SIM the node from before the trip is restored. If CHILL is off, both steps are skipped and retried later.",
                )}
              </p>
              {confirmHere("abroad")}
            </section>

            {/* ── what each scenario does ── */}
            <section>
              <GroupTitle>{t("scenario.doesTitle", "What each scenario does")}</GroupTitle>
              <p className="nd-aux -mt-1 mb-3 px-1">
                {t(
                  "scenario.doesDesc",
                  "Applied once on entering. A changed CHILL node is put back on leaving. Away is where every failure ends up, so its Wi-Fi always stays on.",
                )}
              </p>
              {!chill.data?.region?.options?.length && (
                <p className="nd-aux mb-3 px-1">
                  {t("scenario.nodeFallback", "CHILL is not listing its nodes right now, so the choices below are the built-in regions.")}
                </p>
              )}
              <div className={`nd-group nd-list${sc.stale ? " nd-stale" : ""}`}>
                {cfg?.scenarios.map((s) => {
                  const wifi = wifiOf(s);
                  const fallback = s.detect.type === "fallback";
                  const node = nodeOf(s);
                  const opts = [...new Set([...nodeOptions, ...(node ? [node] : [])])];
                  return (
                    <div key={s.id} className="nd-list">
                      <div className="nd-row">
                        <span className="nd-row__text">
                          <span className="nd-row__label">{s.name}</span>
                        </span>
                      </div>
                      <Row
                        label="Wi-Fi"
                        value={fallback ? t("scenario.alwaysOn", "always on") : undefined}
                        control={
                          fallback ? undefined : (
                            <span {...trig("does")}>
                              <Switch
                                label={t("scenario.wifiFor", "Wi-Fi in {{name}}", { name: s.name })}
                                isSelected={wifi !== false}
                                isDisabled={locked}
                                onChange={(on) => askWifi(s, on)}
                              />
                            </span>
                          )
                        }
                      />
                      <label className="nd-row">
                        <span className="nd-row__text">
                          <span className="nd-row__label">{t("scenario.chillNode", "CHILL node")}</span>
                        </span>
                        <select
                          className="nd-field w-auto max-w-[60%]"
                          aria-label={t("scenario.nodeFor", "CHILL node in {{name}}", { name: s.name })}
                          value={node}
                          disabled={locked}
                          onChange={(e) => askNode(s, e.target.value)}
                        >
                          <option value="">{t("scenario.nodeUnchanged", "leave as is")}</option>
                          {opts.map((o) => (
                            <option key={o} value={o}>
                              {nodeLabel(o)}
                            </option>
                          ))}
                        </select>
                      </label>
                    </div>
                  );
                })}
              </div>
              {confirmHere("does")}
            </section>

            {/* ── timing ── */}
            <section>
              <GroupTitle>{t("scenario.timingTitle", "How eagerly it reacts")}</GroupTitle>
              <p className="nd-aux -mt-1 mb-3 px-1">
                {t("scenario.timingDesc", "Each scan briefly occupies the radios, so scanning often costs a little responsiveness for anything connected.")}
              </p>
              {form ? (
                <div className={`nd-group${sc.stale ? " nd-stale" : ""}`}>
                  {(
                    [
                      ["scan_interval_away_charging_secs", t("scenario.pCharging", "Scan interval on charger (s)"), "numeric"],
                      ["scan_interval_away_battery_secs", t("scenario.pBattery", "Scan interval on battery (s)"), "numeric"],
                      ["scan_interval_home_secs", t("scenario.pHome", "Scan interval at home (s)"), "numeric"],
                      ["enter_hits", t("scenario.pEnter", "Confirmations to enter"), "numeric"],
                      ["exit_misses", t("scenario.pExit", "Confirmations to leave"), "numeric"],
                      // Negative number: a numeric keypad has no minus sign on iOS.
                      ["min_rssi_dbm", t("scenario.pRssi", "Signal floor without a hardware address (dBm)"), "text"],
                    ] as [keyof ScenarioParams, string, "numeric" | "text"][]
                  ).map(([key, label, mode]) => (
                    <label key={key} className="nd-row">
                      <span className="nd-row__text">
                        <span className="nd-row__label">{label}</span>
                      </span>
                      <input
                        className="nd-field nd-mono w-28 text-right"
                        inputMode={mode}
                        value={form[key]}
                        disabled={locked}
                        onChange={(e) => setForm({ ...form, [key]: e.target.value })}
                      />
                    </label>
                  ))}
                </div>
              ) : (
                <div className="nd-group">
                  <div className="nd-row"><span className="nd-skel" style={{ width: "16ch" }} /></div>
                </div>
              )}
              <div className="mt-3 flex flex-wrap items-center gap-3">
                <span {...trig("timing")}>
                  <Button onPress={askParams} isDisabled={locked || !form} pending={busy && shownAt === "timing"}>
                    {t("scenario.save", "Save")}
                  </Button>
                </span>
              </div>
              {formErr && (
                <p className="mt-2 px-1" role="alert">
                  <StatusMark tone="bad">{formErr}</StatusMark>
                </p>
              )}
              <p className="nd-aux mt-3 px-1">
                {t("scenario.timingNote", "At home the device's own Wi-Fi is already off, so scanning there is free. Away from home it is not.")}
              </p>
              {confirmHere("timing")}
            </section>
          </>
        )}

        {/* ── not set up ── */}
        {data && !configured && (
          <section>
            <Group title={t("scenario.notSetUpTitle", "Not set up yet")}>
              <div className="grid gap-3 p-4 lg:p-5">
                <p className="nd-body text-nd-t2">
                  {t(
                    "scenario.notSetUpDesc",
                    "Create the two default scenarios — at home and away. Nothing happens until you add a home network, so this is safe to do now.",
                  )}
                </p>
                <div>
                  <span {...trig("setup")}>
                    <Button isDisabled={locked} onPress={askCreateDefaults}>
                      {t("scenario.createDefaults", "Create defaults")}
                    </Button>
                  </span>
                </div>
              </div>
            </Group>
            {confirmHere("setup")}
          </section>
        )}

        {/* ── log ── */}
        <ConsoleBand label={t("scenario.logTitle", "What it did")}>
          <div className="mb-3 flex flex-wrap items-center gap-2 font-[family-name:var(--nd-font)]">
            <h2 className="nd-console__title flex-1">{t("scenario.logTitle", "What it did")}</h2>
            <button
              type="button"
              className="nd-btn bg-nd-console-card text-nd-console-t1"
              onClick={() => log.mutate()}
            >
              <ArrowClockwise size={20} weight="bold" aria-hidden />
              {t("scenario.refreshLog", "Refresh log")}
            </button>
          </div>
          {log.error && !log.data && (
            <p className="nd-console__muted mb-2">{t("scenario.logFailed", "Couldn't read the log: {{e}}", { e: log.error.message ?? "" })}</p>
          )}
          <pre tabIndex={0} className="max-h-72 overflow-auto whitespace-pre-wrap break-words">
            {log.data?.log ? log.data.log : <span className="nd-console__muted">{log.data ? t("scenario.noLog", "Nothing yet.") : t("scenario.logLoading", "Reading the log…")}</span>}
          </pre>
        </ConsoleBand>
      </div>

      {pending?.tier === 3 && (
        <ConfirmDialog
          open
          onOpenChange={(o) => !o && setPending(null)}
          title={pending.dialog?.title ?? pending.action}
          what={pending.consequence}
          downtime={pending.dialog?.downtime}
          recovery={pending.dialog?.recovery}
          actionLabel={t("nd.confirmAction", "Confirm: {{action}}", { action: pending.action })}
          cutsUplink
          onConfirm={go}
        />
      )}
    </>
  );
}

/** One `pending_restore[]` entry in words. Keys: scenario.rs CHILL_ON_KEY,
 *  CHILL_EXIT_KEY, and main-group region restores (`body.member`). */
function restoreText(t: TFunction, key: string, body: Record<string, unknown> | null) {
  if (key === "chill-on-after-abroad") {
    return t("scenario.chillOnWhenHome", "CHILL was switched off abroad; it is switched back on when you return.");
  }
  if (key === "chill-exit-after-abroad") {
    return body?.state === "proxy"
      ? t("scenario.exitWhenHome", "Exit back to “Proxy”")
      : t("scenario.exitWhenHomeOther", "Exit back to {{x}}", { x: String(body?.state ?? "—") });
  }
  const m = body?.member;
  if (typeof m === "string") return m === "DIRECT" ? t("scenario.direct", "Direct") : m;
  return "—";
}
