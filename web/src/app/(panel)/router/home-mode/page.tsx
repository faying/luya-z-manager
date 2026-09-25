"use client";
// Home Mode (new design). Superseded by Scenarios and not in the nav, but
// kept whole: status first (Wi-Fi off / on / paused), then the on/off switch,
// home SSIDs, nearby scan, detection settings, then the two logs.
//
// Writes (controls-inventory §/router/home-mode, design §3.1):
//   switch on   → tier 2 local / tier 3 remote (the device will then switch
//                 its own Wi-Fi off by itself — a Wi-Fi switch in effect)
//   switch off, add / remove / undo SSID, save settings, scan → tier 2
// PUT /api/homemode drops flag-file errors (homemode.rs:168-173), so every
// write is read back from GET /api/homemode.
// GET /api/homemode/scan may wake the 2.4 GHz radio (uci + reload) while
// home mode has Wi-Fi off: only on an explicit, confirmed press.
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, Plus, Trash } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp, type WriteStep } from "@/lib/api/writeOp";
import type { HomeMode, HomeModeBody, HomeModeLog, HomeModeScan } from "@/lib/api/schemas/wifi";
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
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

type Section = "switch" | "list" | "nearby" | "settings";

interface Pending {
  section: Section;
  action: string;
  consequence: ReactNode;
  tier: 2 | 3;
  body: HomeModeBody;
  check: (d: HomeMode) => boolean;
  /** After a removal: what "Undo" puts back. */
  undo?: { prev: string[]; ssid: string };
  /** Typed into the add box: clear it once sent. */
  clearsInput?: boolean;
}

const sameList = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

export default function HomeModePage() {
  const { t } = useTranslation();
  const hm = useApi<HomeMode>("/api/homemode", { refreshInterval: 15000 });
  const log = useApi<HomeModeLog>("/api/homemode/log", { refreshInterval: 10000 });
  const data = hm.data;
  const ssids = data?.ssids ?? [];

  // ── one write op for every change (they never overlap) ──
  const [pending, setPending] = useState<Pending | null>(null);
  const [newSsid, setNewSsid] = useState("");
  const [shownAt, setShownAt] = useState<Section | null>(null);
  const [undo, setUndo] = useState<{ prev: string[]; ssid: string } | null>(null);
  const [lastUndo, setLastUndo] = useState<{ prev: string[]; ssid: string } | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);
  const body = pending?.body;
  const check = pending?.check;
  const steps: WriteStep[] = body
    ? [{ label: t("homemode.save", "Save"), run: () => apiFetch("/api/homemode", { method: "PUT", body }) }]
    : [];
  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps,
    verify: check
      ? async () => {
          const d = await apiFetch<HomeMode>("/api/homemode");
          await hm.mutate(d, { revalidate: false });
          return check(d);
        }
      : undefined,
  });
  const busy = op.busy;
  const locked = !data || hm.stale || busy;

  // Offer "Undo" only once the removal is confirmed on the device.
  const { phase } = op;
  const mutateLog = log.mutate;
  useEffect(() => {
    if (phase === "applied") void mutateLog();
  }, [phase, mutateLog]);
  const showUndo = phase === "applied" && shownAt === "list" && lastUndo !== null;

  // Stable, so ConfirmInline doesn't re-focus Cancel on every poll re-render.
  const cancelPending = useCallback(() => {
    setPending(null);
    setUndo(null);
  }, []);

  function ask(p: Pending) {
    if (busy) return;
    setPending(p);
  }
  function go() {
    if (!pending) return;
    setShownAt(pending.section);
    setLastUndo(pending.undo ?? null);
    setUndo(null);
    if (pending.clearsInput) setNewSsid("");
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }
  const trig = (section: Section) =>
    pending?.section === section && pending.tier === 2 ? inline.triggerProps : {};

  // ── switch ──
  function askEnabled(next: boolean) {
    ask({
      section: "switch",
      tier: next && isRemoteAccess() ? 3 : 2,
      action: next ? t("homemode.turnOn", "turn Home Mode on") : t("homemode.turnOff", "pause Home Mode"),
      consequence: next
        ? t("homemode.onConsequence", "When a home SSID is nearby the U60 switches its own Wi-Fi off; devices connected to it drop off and move to your home router. Wi-Fi comes back when you leave.")
        : t("homemode.offConsequence", "Home Mode stops checking. If it had Wi-Fi off, Wi-Fi comes back on."),
      body: { enabled: next },
      check: (d) => d.enabled === next,
    });
  }

  // ── SSID list ──
  const [listErr, setListErr] = useState<string | null>(null);
  function askAdd(value: string, section: Section) {
    setListErr(null);
    const v = value.trim();
    if (!v) return;
    if (ssids.some((s) => s.toLowerCase() === v.toLowerCase())) {
      setListErr(t("homemode.alreadyInList", "“{{ssid}}” is already in the list", { ssid: v }));
      return;
    }
    const next = [...ssids, v];
    ask({
      section,
      tier: 2,
      action: t("homemode.addAria", "Add {{ssid}}", { ssid: v }),
      consequence: data?.using_default
        ? t("homemode.addConsequenceDefault", "“{{ssid}}” counts as home from now on. This saves your own list in place of the built-in defaults.", { ssid: v })
        : t("homemode.addConsequence", "“{{ssid}}” counts as home from now on: seeing it switches the U60's Wi-Fi off.", { ssid: v }),
      body: { ssids: next },
      check: (d) => sameList(d.ssids, next),
      clearsInput: section === "list",
    });
  }
  function askRemove(ssid: string) {
    const prev = ssids;
    const next = ssids.filter((s) => s !== ssid);
    ask({
      section: "list",
      tier: 2,
      action: t("homemode.removeAria", "Remove {{ssid}}", { ssid }),
      consequence: t("homemode.removeConsequence", "“{{ssid}}” no longer counts as home. You can undo this right after.", { ssid }),
      body: { ssids: next },
      check: (d) => sameList(d.ssids, next),
      undo: { prev, ssid },
    });
  }
  function askUndo() {
    if (!lastUndo) return;
    const { prev, ssid } = lastUndo;
    setUndo(lastUndo);
    ask({
      section: "list",
      tier: 2,
      action: t("homemode.undoAction", "put {{ssid}} back", { ssid }),
      consequence: t("homemode.undoConsequence", "“{{ssid}}” counts as home again.", { ssid }),
      body: { ssids: prev },
      check: (d) => sameList(d.ssids, prev),
    });
  }

  // ── detection settings ──
  const [draft, setDraft] = useState<{ every: string; misses: string } | null>(null);
  const cfg = draft ?? (data ? { every: String(data.check_every), misses: String(data.exit_misses) } : null);
  function askSettings() {
    if (!cfg) return;
    const every = Math.min(60, Math.max(1, parseInt(cfg.every, 10) || 0));
    const misses = Math.min(30, Math.max(1, parseInt(cfg.misses, 10) || 0));
    setDraft({ every: String(every), misses: String(misses) });
    ask({
      section: "settings",
      tier: 2,
      action: t("homemode.saveSettings", "save the detection settings"),
      consequence: t("homemode.settingsConsequence", "Rechecks every {{every}} min; Wi-Fi returns after {{misses}} missed recheck(s).", { every, misses }),
      body: { check_every: every, exit_misses: misses },
      check: (d) => d.check_every === every && d.exit_misses === misses,
    });
  }

  // ── scan ──
  const [scanAsk, setScanAsk] = useState(false);
  const cancelScan = useCallback(() => setScanAsk(false), []);
  const scanInline = useConfirmInline(scanAsk);
  const [scanned, setScanned] = useState<HomeModeScan | null>(null);
  const scanOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("homemode.scan", "Scan"),
        run: async () => {
          const r = await apiFetch<HomeModeScan>("/api/homemode/scan");
          setScanned({ networks: r.networks ?? [], woke_radio: !!r.woke_radio });
        },
      },
    ],
  });
  const pickable = (scanned?.networks ?? []).filter((n) => !ssids.some((s) => s.toLowerCase() === n.ssid.toLowerCase()));

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("homemode.loading", "Reading Home Mode…");
  let reason: ReactNode = null;
  if (!data && hm.error) {
    tone = "bad";
    state = t("homemode.unreadable", "Can't read Home Mode");
    reason = hm.error.message;
  } else if (data && !data.enabled) {
    state = t("homemode.statusPaused", "Paused");
    reason = t("homemode.pausedReason", "Wi-Fi stays on whatever networks are nearby.");
  } else if (data?.wifi_off) {
    tone = "warn";
    state = t("homemode.statusWifiOff", "Wi-Fi off — home nearby");
    reason = t("homemode.wifiOffReason", "A home network is nearby, so the U60's own Wi-Fi is off. It comes back when you leave.");
  } else if (data) {
    tone = "ok";
    state = t("homemode.statusWifiOn", "Wi-Fi on");
    reason = ssids.length === 0 ? t("homemode.noSsids", "No SSIDs configured — home mode will never trigger.") : null;
  }
  if (data && hm.stale) tone = "stale";

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
          <div className="mt-2 grid gap-2 px-1">
            <OpResult op={op} />
            {section === "list" && showUndo && !undo && (
              <div>
                <span {...trig("list")}>
                  <Button variant="secondary" size="sm" isDisabled={locked} onPress={askUndo}>
                    {t("homemode.undo", "Undo")}
                  </Button>
                </span>
              </div>
            )}
          </div>
        )}
      </>
    );
  }

  const every = parseInt(cfg?.every ?? "", 10) || 0;
  const misses = parseInt(cfg?.misses ?? "", 10) || 0;

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("homemode.title", "Home Mode")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">
        {t("homemode.desc", "When a known home network is nearby, the U60 turns its own Wi-Fi off so your devices fall back to your home router. Wi-Fi comes back when you leave.")}
      </p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={data && hm.stale ? <Freshness stale lastOkAt={hm.lastOkAt} /> : undefined}
          actions={
            !data && hm.error ? (
              <Button variant="secondary" size="sm" onPress={() => hm.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── superseded note + switch ── */}
        <section>
          <Group>
            <Row
              label={t("homemode.supersededLink", "Open Scenarios →")}
              sub={t("homemode.superseded", "Home Mode has been replaced by Scenarios; the two can't run together.")}
              href="/router/scenario"
            />
          </Group>
        </section>

        <section>
          <Group stale={hm.stale}>
            <Row
              label={t("homemode.title", "Home Mode")}
              sub={data ? (data.enabled ? t("homemode.on", "On") : t("homemode.off", "Off")) : undefined}
              control={
                data ? (
                  <span {...trig("switch")}>
                    <Switch
                      label={t("homemode.title", "Home Mode")}
                      isSelected={data.enabled}
                      isDisabled={locked}
                      onChange={askEnabled}
                    />
                  </span>
                ) : (
                  <span className="nd-skel" />
                )
              }
            />
          </Group>
          {confirmHere("switch")}
          {hm.stale && data && (
            <p className="nd-aux mt-1 px-1">
              <Freshness stale lastOkAt={hm.lastOkAt} what={t("homemode.settingsWord", "Settings")} />
              {t("homemode.refreshToEdit", " — refresh before changing anything.")}
            </p>
          )}
        </section>

        {/* ── home SSIDs ── */}
        <section>
          <GroupTitle>{t("homemode.homeSsidsTitle", "Home SSIDs")}</GroupTitle>
          <p className="nd-aux -mt-1 mb-3 px-1">
            {t("homemode.homeSsidsDesc", "If any of these networks is seen nearby, the U60 turns its Wi-Fi off.")}
          </p>
          {data?.using_default && (
            <p className="nd-aux mb-3 px-1">
              {t("homemode.usingDefaults", "Using built-in defaults. Adding or removing an SSID below saves your own list.")}
            </p>
          )}
          <div className="mb-3 flex gap-2">
            <input
              className="nd-field nd-mono flex-1"
              value={newSsid}
              onChange={(e) => setNewSsid(e.target.value)}
              placeholder={t("homemode.addSsidPlaceholder", "Add an SSID (exact name)")}
              aria-label={t("homemode.addSsidAria", "Add a home SSID")}
              disabled={locked}
              onKeyDown={(e) => {
                if (e.key === "Enter") askAdd(newSsid, "list");
              }}
            />
            <span {...trig("list")}>
              <Button onPress={() => askAdd(newSsid, "list")} isDisabled={locked || !newSsid.trim()}>
                <Plus size={20} weight="bold" aria-hidden />
                {t("homemode.add", "Add")}
              </Button>
            </span>
          </div>
          <div className={`nd-group${hm.stale ? " nd-stale" : ""}`}>
            {!data ? (
              <div className="nd-row"><span className="nd-skel" style={{ width: "12ch" }} /></div>
            ) : ssids.length === 0 ? (
              <div className="nd-row text-nd-t2">
                {t("homemode.noSsids", "No SSIDs configured — home mode will never trigger.")}{" "}
                {t("homemode.noSsidsNext", "Type one above or scan below.")}
              </div>
            ) : (
              ssids.map((s) => (
                <Row
                  key={s}
                  label={<span className="nd-mono">{s}</span>}
                  control={
                    <span {...trig("list")}>
                      <Button
                        variant="ghost"
                        iconOnly
                        isDisabled={locked}
                        aria-label={t("homemode.removeAria", "Remove {{ssid}}", { ssid: s })}
                        onPress={() => askRemove(s)}
                      >
                        <Trash size={20} weight="bold" aria-hidden />
                      </Button>
                    </span>
                  }
                />
              ))
            )}
          </div>
          {listErr && (
            <p className="mt-2 px-1" role="alert">
              <StatusMark tone="bad">{listErr}</StatusMark>
            </p>
          )}
          {confirmHere("list")}
        </section>

        {/* ── nearby (scan) ── */}
        <section>
          <GroupTitle>{t("homemode.nearbyTitle", "Nearby Networks")}</GroupTitle>
          <p className="nd-aux -mt-1 mb-3 px-1">{t("homemode.nearbyDesc", "Scan to add a network you can see right now.")}</p>
          <div className="mb-3 flex flex-wrap items-center gap-3">
            <Button
              variant="secondary"
              onPress={() => setScanAsk((o) => !o)}
              isDisabled={scanOp.busy || busy}
              pending={scanOp.busy}
              {...scanInline.triggerProps}
            >
              <ArrowClockwise size={20} weight="bold" aria-hidden />
              {scanOp.busy
                ? data?.wifi_off
                  ? t("homemode.wakingWifi", "Waking Wi-Fi…")
                  : t("homemode.scanning", "Scanning…")
                : t("homemode.scan", "Scan")}
            </Button>
            <span className="nd-aux">
              {data?.wifi_off
                ? t("homemode.scanWhatOff", "Wi-Fi is off: the scan wakes 2.4 GHz for a moment.")
                : t("homemode.scanWhat", "Uses the 2.4 GHz radio for a few seconds.")}
            </span>
          </div>
          <ConfirmInline
            id={scanInline.id}
            open={scanAsk}
            actionLabel={t("homemode.scanAction", "scan now")}
            consequence={
              data?.wifi_off
                ? t("homemode.scanConsequenceOff", "Home Mode has Wi-Fi off, so the device switches 2.4 GHz on just for this scan and off again afterwards (Wi-Fi reloads twice, about 15–60 seconds). Phones nearby may briefly see the U60's network.")
                : t("homemode.scanConsequence", "The device scans for nearby Wi-Fi on 2.4 GHz, a few seconds. Devices on the U60's Wi-Fi may slow down briefly.")
            }
            onCancel={cancelScan}
            onConfirm={() => {
              setScanAsk(false);
              scanOp.start();
              scanOp.confirm();
            }}
          />
          {scanOp.busy && data?.wifi_off && (
            <p className="nd-aux mt-2 px-1" role="status">
              {t("homemode.wakingRadio", "Waking the 2.4 GHz radio to scan — this takes a few seconds while Wi-Fi is off.")}
            </p>
          )}
          {scanned?.woke_radio && !scanOp.busy && (
            <p className="nd-aux mt-2 px-1">
              {t("homemode.wokeRadio", "2.4 GHz was woken for this scan and switched off again.")}
            </p>
          )}
          <div className="nd-group mt-3">
            {(scanOp.phase === "failed" || scanOp.phase === "unknown") && (
              <div className="nd-row" role="alert">
                <StatusMark tone="bad">
                  {scanOp.phase === "unknown"
                    ? t("homemode.scanNoReply", "The scan got no answer in time. Wi-Fi may still be settling; try again in a minute.")
                    : t("homemode.scanFailedWhy", "Scan failed: {{e}}", { e: scanOp.error ?? "" })}
                </StatusMark>
              </div>
            )}
            {scanned === null ? (
              <div className="nd-row text-nd-t2" role={scanOp.busy ? "status" : undefined}>
                {scanOp.busy ? t("homemode.scanning", "Scanning…") : t("homemode.tapScan", "Tap Scan to list nearby Wi-Fi networks.")}
              </div>
            ) : pickable.length === 0 ? (
              <div className="nd-row text-nd-t2">
                {scanned.networks.length === 0
                  ? t("homemode.noNetworks", "No networks found.")
                  : t("homemode.allInList", "All nearby networks are already in your list.")}
              </div>
            ) : (
              pickable.map((n) => (
                <Row
                  key={n.ssid}
                  label={<span className="nd-mono">{n.ssid}</span>}
                  value={`${n.signal} dBm`}
                  mono
                  control={
                    <span {...trig("nearby")}>
                      <Button
                        variant="ghost"
                        iconOnly
                        isDisabled={locked}
                        aria-label={t("homemode.addAria", "Add {{ssid}}", { ssid: n.ssid })}
                        onPress={() => askAdd(n.ssid, "nearby")}
                      >
                        <Plus size={20} weight="bold" aria-hidden />
                      </Button>
                    </span>
                  }
                />
              ))
            )}
          </div>
          {confirmHere("nearby")}
        </section>

        {/* ── detection settings ── */}
        <section>
          <GroupTitle>{t("homemode.detectionTitle", "Detection settings")}</GroupTitle>
          <p className="nd-aux -mt-1 mb-3 px-1">
            {t("homemode.detectionDesc", "How often the device rechecks whether you’re still home, and how long to wait before turning Wi-Fi back on after you leave.")}
          </p>
          <div className={`nd-group${hm.stale ? " nd-stale" : ""}`}>
            {cfg ? (
              <>
                <label className="nd-row">
                  <span className="nd-row__text">
                    <span className="nd-row__label">{t("homemode.recheckInterval", "Recheck interval")}</span>
                    <span className="nd-row__sub block">{t("homemode.minutes", "minutes")} · 1–60</span>
                  </span>
                  <input
                    className="nd-field nd-mono w-24 text-right"
                    type="number"
                    min={1}
                    max={60}
                    inputMode="numeric"
                    value={cfg.every}
                    disabled={locked}
                    onChange={(e) => setDraft({ ...cfg, every: e.target.value })}
                  />
                </label>
                <label className="nd-row">
                  <span className="nd-row__text">
                    <span className="nd-row__label">{t("homemode.missesLabel", "Misses before Wi-Fi returns")}</span>
                    <span className="nd-row__sub block">{t("homemode.rechecks", "rechecks")} · 1–30</span>
                  </span>
                  <input
                    className="nd-field nd-mono w-24 text-right"
                    type="number"
                    min={1}
                    max={30}
                    inputMode="numeric"
                    value={cfg.misses}
                    disabled={locked}
                    onChange={(e) => setDraft({ ...cfg, misses: e.target.value })}
                  />
                </label>
              </>
            ) : (
              <div className="nd-row"><span className="nd-skel" style={{ width: "16ch" }} /></div>
            )}
          </div>
          {cfg && (
            <p className="nd-aux mt-3 px-1">
              {t("homemode.returnLead", "After you leave, Wi-Fi comes back in about")}{" "}
              <span className="font-medium text-nd-t1">{t("homemode.minValue", "{{n}} min", { n: every * misses })}</span>{" "}
              {t("homemode.returnDetail", "({{misses}} missed recheck(s) × {{every}} min). Shorter intervals react faster but wake the 2.4 GHz radio more often.", { misses: cfg.misses, every: cfg.every })}
            </p>
          )}
          <div className="mt-3">
            <span {...trig("settings")}>
              <Button onPress={askSettings} isDisabled={locked || !cfg}>
                {t("homemode.save", "Save")}
              </Button>
            </span>
          </div>
          {confirmHere("settings")}
        </section>

        {/* ── logs ── */}
        <ConsoleBand label={t("homemode.switchEventsTitle", "Switch events")}>
          <div className="mb-3 flex flex-wrap items-center gap-2 font-[family-name:var(--nd-font)]">
            <span className="flex-1">
              <h2 className="nd-console__title">{t("homemode.switchEventsTitle", "Switch events")}</h2>
              <span className="nd-console__muted text-[13px]">
                {t("homemode.switchEventsDesc", "When Wi-Fi was actually turned off / back on — newest first.")}
              </span>
            </span>
            <button type="button" className="nd-btn bg-nd-console-card text-nd-console-t1" onClick={() => log.mutate()}>
              <ArrowClockwise size={20} weight="bold" aria-hidden />
              {t("homemode.refresh", "Refresh")}
            </button>
          </div>
          {log.error && !log.data && (
            <p className="nd-console__muted mb-2">{t("homemode.logFailed", "Couldn't read the log: {{e}}", { e: log.error.message ?? "" })}</p>
          )}
          <pre tabIndex={0} className="max-h-64 overflow-auto whitespace-pre-wrap break-words">
            {log.data?.events ? (
              newestFirst(log.data.events)
            ) : (
              <span className="nd-console__muted">
                {log.data ? t("homemode.noSwitchEvents", "No switch events yet.") : t("homemode.logLoading", "Reading the log…")}
              </span>
            )}
          </pre>
        </ConsoleBand>

        <ConsoleBand label={t("homemode.scanActivityTitle", "Scan activity")}>
          <div className="mb-3 font-[family-name:var(--nd-font)]">
            <h2 className="nd-console__title">{t("homemode.scanActivityTitle", "Scan activity")}</h2>
            <span className="nd-console__muted text-[13px]">
              {t("homemode.scanActivityDesc", "Periodic ~2-minute rechecks while at home — high volume, kept separate so it doesn’t bury the switch events.")}
            </span>
          </div>
          <pre tabIndex={0} className="max-h-48 overflow-auto whitespace-pre-wrap break-words nd-console__muted">
            {log.data?.scans
              ? newestFirst(log.data.scans)
              : log.data
                ? t("homemode.noScanActivity", "No scan activity yet.")
                : t("homemode.logLoading", "Reading the log…")}
          </pre>
        </ConsoleBand>
      </div>

      {pending?.tier === 3 && (
        <ConfirmDialog
          open
          onOpenChange={(o) => !o && setPending(null)}
          title={t("homemode.onTitle", "Turn Home Mode on?")}
          what={pending.consequence}
          downtime={t("homemode.onDowntime", "The U60's Wi-Fi stays off for as long as a home SSID is nearby.")}
          recovery={t("homemode.onRecovery", "Pause Home Mode here — over Tailscale the page stays reachable through the mobile connection — or walk out of range of the home network.")}
          actionLabel={t("nd.confirmAction", "Confirm: {{action}}", { action: pending.action })}
          cutsUplink
          onConfirm={go}
        />
      )}
    </>
  );
}

function newestFirst(text: string) {
  return text.split("\n").filter(Boolean).reverse().join("\n");
}
