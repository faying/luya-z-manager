"use client";

// Scenario engine — pick which networks mean "home", and see what the device
// decided.
//
// Deliberately NOT a generic action editor. The actions a scenario runs are
// fixed in the config the device ships with; exposing an arbitrary
// method/path/body builder here would turn "walking into a room" into a trigger
// for any API on the box. This page edits a fixed set of things only: which
// networks count as home, how eagerly the device reacts, whether the shipped
// abroad scenario is present, and per scenario two fixed switches — Wi-Fi on
// or off, and which node CHILL's main group uses.

import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { fmtDevice } from "@/lib/deviceClock";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner, Status } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { Plus, Trash2, RefreshCw, ShieldAlert, Pin, PinOff } from "lucide-react";

interface SsidEntry {
  ssid: string;
  bssid?: string;
}

interface Params {
  scan_interval_home_secs: number;
  scan_interval_away_battery_secs: number;
  scan_interval_away_charging_secs: number;
  enter_hits: number;
  exit_misses: number;
  min_rssi_dbm: number;
}

interface Scenario {
  id: string;
  name: string;
  detect:
    | { type: "ssid"; entries: SsidEntry[] }
    | { type: "mcc"; mccs: string[] }
    | { type: "abroad" }
    | { type: "fallback" };
  actions?: ScenarioAction[];
  inhibit_sleep?: boolean;
}

// Mirrors ScenarioAction in zte-agent/src/scenario.rs. Only the fields this
// page builds are typed; anything else on an action is carried through as-is.
interface ScenarioAction {
  method: string;
  path: string;
  body?: Record<string, unknown>;
  snapshot?: string[];
  verify_field?: { path: string; pointer: string; equals: unknown };
  best_effort?: boolean;
  restore_on_exit?: { read_path: string; read_pointer: string; field: string };
  [k: string]: unknown;
}

interface Config {
  version: number;
  home_mcc?: string;
  params: Params;
  scenarios: Scenario[];
}

interface ScenarioData {
  enabled: boolean;
  pin: string | null;
  guard_takeover?: boolean;
  current: string;
  candidate: string;
  hits: number;
  misses: number;
  last_switch: number | null;
  last_scan: number;
  last_error: string | null;
  config: Config;
  sim_mcc: string | null;
  pending_restore?: { key: string; body: { member?: string } | null; saved_at: number }[];
}

interface ScanNet {
  ssid: string;
  bssid: string;
  signal: number;
}

const HOME_ID = "home";

const WIFI_PATH = "/api/wifi/radio";
const REGION_PATH = "/api/services/chill/regions";
// Must match CHILL_MAIN_GROUP in scenario.rs and scripts/chill/template.yaml.
const CHILL_GROUP = "🚀 节点选择";
// Used when CHILL is not running and cannot list its own members.
const CHILL_FALLBACK_OPTIONS = ["DIRECT", "🇹🇼 台湾", "🇯🇵 日本", "🇸🇬 新加坡", "🇺🇸 美国"];

function wifiOf(s: Scenario): boolean | null {
  const a = s.actions?.find((x) => x.path === WIFI_PATH);
  return a ? a.body?.ap_2g !== false : null;
}

function isMainGroup(a: ScenarioAction) {
  return a.path === REGION_PATH && a.body?.group === CHILL_GROUP;
}

function nodeOf(s: Scenario): string {
  const a = s.actions?.find(isMainGroup);
  return typeof a?.body?.member === "string" ? a.body.member : "";
}

// Wi-Fi goes first: arriving from home means the APs are down, and anything
// after it may need the network.
function withWifi(s: Scenario, on: boolean): Scenario {
  const rest = (s.actions ?? []).filter((a) => a.path !== WIFI_PATH);
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
function withNode(s: Scenario, member: string): Scenario {
  const rest = (s.actions ?? []).filter((a) => !isMainGroup(a));
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
// `abroad_scenarios()` in zte-agent/src/scenario.rs ships exactly one.
const SHIPPED_ABROAD = 1;

function isAbroad(s: Scenario) {
  return s.detect.type === "mcc" || s.detect.type === "abroad";
}

function hasEntries(s: Scenario): s is Scenario & {
  detect: { type: "ssid"; entries: SsidEntry[] };
} {
  return s.detect.type === "ssid";
}

function sameEntry(a: SsidEntry, b: SsidEntry) {
  return (
    a.ssid === b.ssid &&
    (a.bssid ?? "").toLowerCase() === (b.bssid ?? "").toLowerCase()
  );
}

function when(ts: number | null | undefined) {
  if (!ts) return "—";
  return fmtDevice(ts);
}

export default function ScenarioPage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<ScenarioData>("/api/scenario", {
    refreshInterval: 10000,
  });
  const { data: chill } = useApi<{ region?: { options?: string[] } }>("/api/services/chill");
  const { data: logData, mutate: mutateLog } = useApi<{ log: string }>(
    "/api/scenario/log",
    { refreshInterval: 15000 },
  );

  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState<ScanNet[] | null>(null);

  // Timing fields are seeded from the device once, then edited locally so a
  // background refresh cannot yank a half-typed number out from under you.
  const [form, setForm] = useState<Record<keyof Params, string> | null>(null);
  useEffect(() => {
    if (data?.config?.params && form === null) {
      const p = data.config.params;
      setForm({
        scan_interval_home_secs: String(p.scan_interval_home_secs),
        scan_interval_away_battery_secs: String(p.scan_interval_away_battery_secs),
        scan_interval_away_charging_secs: String(p.scan_interval_away_charging_secs),
        enter_hits: String(p.enter_hits),
        exit_misses: String(p.exit_misses),
        min_rssi_dbm: String(p.min_rssi_dbm),
      });
    }
  }, [data, form]);

  const cfg = data?.config;
  const home = cfg?.scenarios.find((s) => s.id === HOME_ID);
  const entries = useMemo(
    () => (home && hasEntries(home) ? home.detect.entries : []),
    [home],
  );
  const configured = (cfg?.scenarios.length ?? 0) > 0;

  const currentName =
    cfg?.scenarios.find((s) => s.id === data?.current)?.name ?? data?.current ?? "—";

  function flash(text: string, err = false) {
    setMsg({ text, err });
    if (!err) setTimeout(() => setMsg(null), 3000);
  }

  async function save(next: Config) {
    setBusy(true);
    try {
      await apiFetch("/api/scenario", { method: "PUT", body: JSON.stringify(next) });
      await mutate();
      flash(t("scenario.saved", "Saved"));
      return true;
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
      return false;
    } finally {
      setBusy(false);
    }
  }

  function withEntries(next: SsidEntry[]): Config | null {
    if (!cfg || !home) return null;
    return {
      ...cfg,
      scenarios: cfg.scenarios.map((s) =>
        s.id === HOME_ID ? { ...s, detect: { type: "ssid", entries: next } } : s,
      ),
    };
  }

  async function addEntry(e: SsidEntry) {
    if (!e.ssid.trim()) return;
    if (entries.some((x) => sameEntry(x, e))) {
      flash(t("scenario.alreadyListed", "Already in the list"), true);
      return;
    }
    const next = withEntries([...entries, { ssid: e.ssid.trim(), bssid: e.bssid }]);
    if (next) await save(next);
  }

  async function removeEntry(e: SsidEntry) {
    const next = withEntries(entries.filter((x) => !sameEntry(x, e)));
    if (next) await save(next);
  }

  async function saveParams() {
    if (!cfg || !form) return;
    const num = (v: string) => Number(v);
    const p: Params = {
      scan_interval_home_secs: num(form.scan_interval_home_secs),
      scan_interval_away_battery_secs: num(form.scan_interval_away_battery_secs),
      scan_interval_away_charging_secs: num(form.scan_interval_away_charging_secs),
      enter_hits: num(form.enter_hits),
      exit_misses: num(form.exit_misses),
      min_rssi_dbm: num(form.min_rssi_dbm),
    };
    if (Object.values(p).some((v) => !Number.isFinite(v))) {
      flash(t("scenario.badNumber", "Every field must be a number"), true);
      return;
    }
    if (p.enter_hits < 1 || p.exit_misses < 1) {
      flash(t("scenario.minOne", "Confirmations must be at least 1"), true);
      return;
    }
    await save({ ...cfg, params: p });
  }

  async function initialise() {
    setBusy(true);
    try {
      const tpl = await apiFetch<Config>("/api/scenario/template");
      await apiFetch("/api/scenario", { method: "PUT", body: JSON.stringify(tpl) });
      await mutate();
      flash(t("scenario.initialised", "Created the default scenarios — now add your home network"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  // Append whichever shipped abroad scenarios are missing. Never rewrites what
  // is already there — the home networks in particular took effort to pick.
  async function addAbroad() {
    if (!cfg) return;
    setBusy(true);
    try {
      const tpl = await apiFetch<Config>("/api/scenario/template");
      const have = new Set(cfg.scenarios.map((s) => s.id));
      const missing = tpl.scenarios.filter((s) => isAbroad(s) && !have.has(s.id));
      await apiFetch("/api/scenario", {
        method: "PUT",
        body: JSON.stringify({ ...cfg, scenarios: [...cfg.scenarios, ...missing] }),
      });
      await mutate();
      flash(t("scenario.abroadAdded", "Abroad scenario added"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function setEnabled(on: boolean) {
    setBusy(true);
    try {
      await apiFetch("/api/scenario/enabled", {
        method: "PUT",
        body: JSON.stringify({ enabled: on }),
      });
      await mutate();
      await mutateLog();
      flash(on ? t("scenario.enabled", "Engine on") : t("scenario.disabledOk", "Engine off — Wi-Fi restored"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function setPin(id: string | null) {
    setBusy(true);
    try {
      await apiFetch("/api/scenario/pin", {
        method: "POST",
        body: JSON.stringify({ id }),
      });
      await mutate();
      flash(id ? t("scenario.pinned", "Pinned") : t("scenario.unpinned", "Pin cleared"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function scan() {
    setScanning(true);
    try {
      const r = await apiFetch<{ networks: ScanNet[] }>("/api/scenario/scan");
      setScanned(r.networks);
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setScanning(false);
    }
  }

  // Hide what is already listed, and collapse the noise: one row per
  // (ssid, bssid) is what actually matters for matching.
  const pickable = useMemo(() => {
    if (!scanned) return [];
    return scanned.filter((n) => !entries.some((e) => sameEntry(e, n)));
  }, [scanned, entries]);

  const nameless = entries.filter((e) => !e.bssid);
  const abroad = cfg?.scenarios.filter(isAbroad) ?? [];
  const nodeOptions = chill?.region?.options?.length ? chill.region.options : CHILL_FALLBACK_OPTIONS;

  function editScenario(next: Scenario) {
    if (!cfg) return;
    save({ ...cfg, scenarios: cfg.scenarios.map((s) => (s.id === next.id ? next : s)) });
  }

  if (error) return <ErrorBanner message={error.message} onRetry={() => mutate()} />;

  return (
    <div>
      <PageHeader
        title={t("scenario.title", "Scenarios")}
        description={t(
          "scenario.desc",
          "The device watches for networks you name here and reconfigures itself when it recognises where it is.",
        )}
        actions={
          data && (
            <Toggle
              checked={data.enabled}
              onChange={setEnabled}
              disabled={busy}
              label={t("scenario.engineOn", "Engine on")}
            />
          )
        }
      />

      {msg && (
        <div
          className={`mb-4 rounded-md border px-3 py-2 text-sm ${
            msg.err
              ? "border-error/40 bg-error/10 text-error"
              : "border-success/40 bg-success/10 text-success"
          }`}
        >
          {msg.text}
        </div>
      )}

      {!configured ? (
        <SectionCard
          title={t("scenario.notSetUpTitle", "Not set up yet")}
          description={t(
            "scenario.notSetUpDesc",
            "Create the two default scenarios — at home and away. Nothing happens until you add a home network, so this is safe to do now.",
          )}
        >
          <Button onClick={initialise} loading={busy}>
            {t("scenario.createDefaults", "Create defaults")}
          </Button>
        </SectionCard>
      ) : (
        <div className="grid gap-4">
          {/* Where the device thinks it is */}
          <SectionCard title={t("scenario.statusTitle", "Right now")}>
            <dl className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm sm:grid-cols-3 lg:grid-cols-5">
              <div>
                <dt className="text-xs text-text-dim">{t("scenario.current", "Scenario")}</dt>
                <dd className="mt-0.5 font-medium">
                  <Status tone={data?.current === HOME_ID ? "accent" : "success"}>
                    {currentName}
                  </Status>
                </dd>
              </div>
              <div>
                <dt className="text-xs text-text-dim">{t("scenario.lastSwitch", "Last change")}</dt>
                <dd className="mt-0.5 tabular-nums">{when(data?.last_switch)}</dd>
              </div>
              <div>
                <dt className="text-xs text-text-dim">{t("scenario.lastScan", "Last scan")}</dt>
                <dd className="mt-0.5 tabular-nums">{when(data?.last_scan)}</dd>
              </div>
              <div>
                <dt className="text-xs text-text-dim">{t("scenario.simMcc", "SIM country code")}</dt>
                <dd className="mt-0.5 tabular-nums">
                  {data?.sim_mcc ?? <span className="text-text-dim">{t("scenario.simUnknown", "unknown")}</span>}
                </dd>
              </div>
              <div>
                <dt className="text-xs text-text-dim">{t("scenario.pin", "Pinned to")}</dt>
                <dd className="mt-0.5">
                  {data?.pin ? (
                    <button
                      onClick={() => setPin(null)}
                      disabled={busy}
                      className="inline-flex items-center gap-1 text-accent underline underline-offset-2 hover:text-accent-hover"
                    >
                      <PinOff size={12} /> {data.pin}
                    </button>
                  ) : (
                    <span className="text-text-dim">{t("scenario.notPinned", "auto")}</span>
                  )}
                </dd>
              </div>
            </dl>

            {data?.candidate && data.candidate !== data.current && (
              <p className="mt-3 text-xs text-text-dim">
                {t("scenario.pending", "Seeing {{name}} — {{hits}} of {{need}} confirmations so far.", {
                  name: cfg?.scenarios.find((s) => s.id === data.candidate)?.name ?? data.candidate,
                  hits: data.hits,
                  need: cfg?.params.enter_hits ?? 2,
                })}
              </p>
            )}
            {data?.guard_takeover && (
              <p className="mt-3 text-xs text-warning">
                {t(
                  "scenario.guardTakeover",
                  "The Wi-Fi watchdog took over while the agent was not responding. Staying in away; the pin is paused until 10 minutes of steady running.",
                )}
              </p>
            )}
            {data?.last_error && (
              <p className="mt-3 text-xs text-warning">{data.last_error}</p>
            )}

            <div className="mt-4 flex flex-wrap gap-2">
              {cfg?.scenarios.map((s) => (
                <Button
                  key={s.id}
                  size="sm"
                  variant={data?.pin === s.id ? "primary" : "outline"}
                  disabled={busy}
                  onClick={() => setPin(data?.pin === s.id ? null : s.id)}
                >
                  <Pin size={12} /> {s.name}
                </Button>
              ))}
            </div>
            <p className="mt-2 text-xs text-text-dim">
              {t(
                "scenario.pinHelp",
                "Pinning holds one scenario regardless of what the device sees. It survives a reboot — use it if detection is misbehaving.",
              )}
            </p>
          </SectionCard>

          {/* Home networks */}
          <SectionCard
            title={t("scenario.homeTitle", "Networks that mean “at home”")}
            description={t(
              "scenario.homeDesc",
              "Seeing any one of these turns the device's own Wi-Fi off so your phone moves to the house network.",
            )}
          >
            {entries.length === 0 ? (
              <p className="text-sm text-text-dim">
                {t("scenario.noEntries", "Nothing listed — the device will never decide it is at home.")}
              </p>
            ) : (
              <div className="space-y-1">
                {entries.map((e) => (
                  <div
                    key={`${e.ssid}|${e.bssid ?? ""}`}
                    className="flex items-center gap-3 border-b border-border/50 py-2 last:border-0"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="truncate font-mono text-sm">{e.ssid}</div>
                      <div className="mt-0.5 font-mono text-xs text-text-dim">
                        {e.bssid ?? t("scenario.anyRadio", "any radio with this name")}
                      </div>
                    </div>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => removeEntry(e)}
                      disabled={busy}
                      aria-label={t("scenario.removeAria", "Remove {{ssid}}", { ssid: e.ssid })}
                      className="shrink-0 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
                    >
                      <Trash2 size={13} className="text-error" />
                    </Button>
                  </div>
                ))}
              </div>
            )}

            {nameless.length > 0 && (
              <div className="mt-3 flex items-start gap-2 rounded-md border border-warning/30 bg-warning/[0.06] px-3 py-2 text-xs text-warning">
                <ShieldAlert size={14} className="mt-0.5 shrink-0" />
                <span>
                  {t(
                    "scenario.noBssidWarning",
                    "{{count}} entry matches on name alone. Anyone can broadcast that name and your Wi-Fi would switch off — pick the network from a scan instead, which records its hardware address too.",
                    { count: nameless.length },
                  )}
                </span>
              </div>
            )}
          </SectionCard>

          {/* Pick from a scan */}
          <SectionCard
            title={t("scenario.nearbyTitle", "Nearby networks")}
            description={t(
              "scenario.nearbyDesc",
              "Adding from a scan records the hardware address, so a network merely using the same name will not fool it.",
            )}
            actions={
              <Button size="sm" variant="outline" onClick={scan} disabled={scanning}>
                <RefreshCw size={13} className={scanning ? "animate-spin" : ""} />
                {scanning ? t("scenario.scanning", "Scanning…") : t("scenario.scan", "Scan")}
              </Button>
            }
          >
            {scanned === null ? (
              <p className="text-sm text-text-dim">
                {t("scenario.tapScan", "Scan to list what the device can see.")}
              </p>
            ) : pickable.length === 0 ? (
              <p className="text-sm text-text-dim">
                {scanned.length === 0
                  ? t("scenario.noNetworks", "Nothing found.")
                  : t("scenario.allListed", "Everything nearby is already listed.")}
              </p>
            ) : (
              <div className="max-h-96 space-y-1 overflow-y-auto">
                {pickable.map((n) => (
                  <div
                    key={n.bssid}
                    className="flex items-center gap-3 border-b border-border/50 py-2 last:border-0"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="truncate font-mono text-sm">{n.ssid}</div>
                      <div className="mt-0.5 font-mono text-xs text-text-dim">{n.bssid}</div>
                    </div>
                    <span className="shrink-0 tabular-nums text-xs text-text-dim">
                      {n.signal} dBm
                    </span>
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => addEntry({ ssid: n.ssid, bssid: n.bssid })}
                      disabled={busy}
                      aria-label={t("scenario.addAria", "Add {{ssid}}", { ssid: n.ssid })}
                      className="shrink-0 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
                    >
                      <Plus size={14} />
                    </Button>
                  </div>
                ))}
              </div>
            )}
          </SectionCard>

          {/* Abroad */}
          <SectionCard
            title={t("scenario.abroadTitle", "Abroad")}
            description={t(
              "scenario.abroadDesc",
              "Decided by the SIM in the slot, not by where the signal comes from: with a foreign SIM or eSIM profile, CHILL's main group goes direct. A home SIM roaming abroad changes nothing.",
            )}
            actions={
              abroad.length < SHIPPED_ABROAD && (
                <Button size="sm" variant="outline" onClick={addAbroad} disabled={busy}>
                  <Plus size={13} />
                  {t("scenario.addAbroad", "Add abroad scenario")}
                </Button>
              )
            }
          >
            {abroad.length === 0 ? (
              <p className="text-sm text-text-dim">
                {t("scenario.noAbroad", "Not set up — a foreign SIM is treated as away.")}
              </p>
            ) : (
              <div className="space-y-1">
                {abroad.map((s) => (
                  <div
                    key={s.id}
                    className="flex items-center gap-3 border-b border-border/50 py-2 last:border-0"
                  >
                    <div className="min-w-0 flex-1 text-sm">{s.name}</div>
                    <div className="shrink-0 font-mono text-xs text-text-dim">
                      {s.detect.type === "mcc"
                        ? s.detect.mccs.join(" ")
                        : t("scenario.otherMcc", "any foreign SIM")}
                    </div>
                  </div>
                ))}
              </div>
            )}
            <p className="mt-3 text-xs text-text-dim">
              {t(
                "scenario.abroadNote",
                "Only the “🚀 节点选择” group is set to DIRECT, once, on arrival; groups that do not offer DIRECT (such as AI) keep their node. Changing it by hand afterwards sticks. When you are back on a home SIM the node from before the trip is restored. If CHILL is off, both steps are skipped and retried later.",
              )}
            </p>
            {(data?.pending_restore ?? []).map((p) => (
              <p key={p.key} className="mt-2 text-xs">
                <span className="text-text-dim">{t("scenario.willRestore", "Restores on return:")}</span>{" "}
                <span className="font-medium">{p.body?.member ?? "—"}</span>
              </p>
            ))}
          </SectionCard>

          {/* What each scenario does */}
          <SectionCard
            title={t("scenario.doesTitle", "What each scenario does")}
            description={t(
              "scenario.doesDesc",
              "Applied once on entering. A changed CHILL node is put back on leaving. Away is where every failure ends up, so its Wi-Fi always stays on.",
            )}
          >
            <div className="space-y-1">
              {cfg?.scenarios.map((s) => {
                const wifi = wifiOf(s);
                const fallback = s.detect.type === "fallback";
                const node = nodeOf(s);
                return (
                  <div
                    key={s.id}
                    className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-border/50 py-2 last:border-0"
                  >
                    <div className="min-w-[5rem] flex-1 text-sm font-medium">{s.name}</div>
                    <div className="flex items-center gap-2 text-xs text-text-dim">
                      Wi-Fi
                      {fallback ? (
                        <span className="text-text">{t("scenario.alwaysOn", "always on")}</span>
                      ) : (
                        <Toggle
                          checked={wifi !== false}
                          onChange={(on) => editScenario(withWifi(s, on))}
                          disabled={busy}
                          label={
                            wifi === false ? t("scenario.wifiOff", "off") : t("scenario.wifiOn", "on")
                          }
                        />
                      )}
                    </div>
                    <label className="flex items-center gap-2 text-xs text-text-dim">
                      CHILL
                      <select
                        className="h-9 rounded-lg border border-border bg-bg-card px-2 text-[13px] text-text outline-none transition focus:border-accent focus:ring-2 focus:ring-accent/20"
                        value={node}
                        disabled={busy}
                        onChange={(e) => editScenario(withNode(s, e.target.value))}
                      >
                        <option value="">{t("scenario.nodeUnchanged", "leave as is")}</option>
                        {[...new Set([...nodeOptions, ...(node ? [node] : [])])].map((o) => (
                          <option key={o} value={o}>
                            {o === "DIRECT" ? t("scenario.direct", "Direct") : o}
                          </option>
                        ))}
                      </select>
                    </label>
                  </div>
                );
              })}
            </div>
          </SectionCard>

          {/* Timing */}
          {form && (
            <SectionCard
              title={t("scenario.timingTitle", "How eagerly it reacts")}
              description={t(
                "scenario.timingDesc",
                "Each scan briefly occupies the radios, so scanning often costs a little responsiveness for anything connected.",
              )}
              actions={
                <Button size="sm" onClick={saveParams} loading={busy}>
                  {t("scenario.save", "Save")}
                </Button>
              }
            >
              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                {(
                  [
                    ["scan_interval_away_charging_secs", t("scenario.pCharging", "Scan interval on charger (s)")],
                    ["scan_interval_away_battery_secs", t("scenario.pBattery", "Scan interval on battery (s)")],
                    ["scan_interval_home_secs", t("scenario.pHome", "Scan interval at home (s)")],
                    ["enter_hits", t("scenario.pEnter", "Confirmations to enter")],
                    ["exit_misses", t("scenario.pExit", "Confirmations to leave")],
                    ["min_rssi_dbm", t("scenario.pRssi", "Signal floor without a hardware address (dBm)")],
                  ] as [keyof Params, string][]
                ).map(([key, label]) => (
                  <label key={key} className="block">
                    <span className="mb-1 block text-xs text-text-dim">{label}</span>
                    <Input
                      value={form[key]}
                      inputMode="numeric"
                      onChange={(e) => setForm({ ...form, [key]: e.target.value })}
                    />
                  </label>
                ))}
              </div>
              <p className="mt-3 text-xs text-text-dim">
                {t(
                  "scenario.timingNote",
                  "At home the device's own Wi-Fi is already off, so scanning there is free. Away from home it is not.",
                )}
              </p>
            </SectionCard>
          )}

          {/* Log */}
          <SectionCard
            title={t("scenario.logTitle", "What it did")}
            actions={
              <Button size="sm" variant="outline" onClick={() => mutateLog()}>
                <RefreshCw size={13} />
              </Button>
            }
          >
            {logData?.log ? (
              <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words font-mono text-xs leading-relaxed text-text-dim">
                {logData.log}
              </pre>
            ) : (
              <p className="text-sm text-text-dim">{t("scenario.noLog", "Nothing yet.")}</p>
            )}
          </SectionCard>
        </div>
      )}
    </div>
  );
}
