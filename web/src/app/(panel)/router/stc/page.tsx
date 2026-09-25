"use client";
// STC (new design). Behind the agent this is the firmware's whitelist cell
// lock (`nwinfo_stc_cell_lock_*`, `nwinfo_*_stc_white_list_*`): once on, the
// modem only camps on cells it has collected into the whitelist. Settings
// page: status first, then the switch, the collection parameters, and the
// whitelist reset.
//
// Writes (controls-inventory §/router/stc, design §3.1; all ubus
// passthroughs, cell.rs:92-129):
//   on / off         tier 3   POST /api/cell/stc/enable | disable   readback status.enabled, waits ~30 s
//   reset whitelist  tier 3   POST /api/cell/stc/reset   no readback (no whitelist read endpoint)
//   apply parameters tier 2   PUT  /api/cell/stc/params  readback each field
// The old page notes that /api/cell/stc/* answered 503 on this firmware; the
// page then says so and locks the switch and the parameters. The reset
// doesn't depend on anything shown, so (as before) it stays available —
// except when the reads fail with 503 "Method not found" (B27 has no
// nwinfo_get_stc_white_list_* methods): then the firmware is taken not to
// support STC at all, the page says so, every control (reset included) is
// locked and nothing is retried.
import { useCallback, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, ArrowCounterClockwise } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp } from "@/lib/api/writeOp";
import type { CellStcParams, CellStcStatus } from "@/lib/api/schemas/modem";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const NET_WAIT_SEC = 30;
const FIELDS = ["lte_collect_timer", "nrsa_collect_timer", "lte_whitelist_max", "nrsa_whitelist_max"] as const;
type Field = (typeof FIELDS)[number];
type Params = Record<Field, string>;

/** true / non-zero number / "1" / "true" / "enabled" / non-zero numeric string. */
function isOn(v: CellStcStatus["enabled"]): boolean {
  if (v === undefined || v === null) return false;
  if (typeof v === "boolean") return v;
  if (typeof v === "number") return v !== 0;
  const s = v.toLowerCase();
  if (/^\d+$/.test(s)) return Number(s) !== 0;
  return s === "true" || s === "enabled";
}

/** Read until `ok` holds (the firmware can lag a write by a moment). */
async function settle<T>(read: () => Promise<T>, ok: (d: T) => boolean, tries = 3, gapMs = 2000): Promise<boolean> {
  for (let i = 0; i < tries; i++) {
    if (ok(await read())) return true;
    if (i < tries - 1) await new Promise((r) => setTimeout(r, gapMs));
  }
  return false;
}

export default function StcPage() {
  const { t } = useTranslation();
  const status = useApi<CellStcStatus>("/api/cell/stc/status");
  const params = useApi<CellStcParams>("/api/cell/stc/params");
  const serviceDown = (!status.data && !!status.error) || (!params.data && !!params.error);
  const unsupported = (!status.data && status.unsupported) || (!params.data && params.unsupported);
  const enabled = status.data ? isOn(status.data.enabled) : null;

  const [dialog, setDialog] = useState<null | "toggle" | "reset">(null);
  const [want, setWant] = useState(false);
  const recovery = t("stc.recovery", "Turn STC off on this page. If the page can't be reached, connect to the U60's Wi-Fi and open it from there.");

  const toggleOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: want ? t("stc.turnOn", "Turn STC on") : t("stc.turnOff", "Turn STC off"),
        run: () => apiFetch(want ? "/api/cell/stc/enable" : "/api/cell/stc/disable", { method: "POST" }),
      },
    ],
    waitDevice: { expectedSec: NET_WAIT_SEC, recovery },
    verify: () =>
      settle(
        async () => {
          const d = await apiFetch<CellStcStatus>("/api/cell/stc/status");
          await status.mutate(d, { revalidate: false });
          return d;
        },
        (d) => isOn(d.enabled) === want
      ),
  });

  const resetOp = useWriteOp({
    tier: 3,
    steps: [{ label: t("stc.resetWhitelist", "Reset Whitelist"), run: () => apiFetch("/api/cell/stc/reset", { method: "POST" }) }],
    waitDevice: { expectedSec: NET_WAIT_SEC, recovery },
  });

  // ── parameters (draft over the device values) ──
  const [draft, setDraft] = useState<Params | null>(null);
  const base: Params | null = params.data
    ? {
        lte_collect_timer: params.data.lte_collect_timer ?? "",
        nrsa_collect_timer: params.data.nrsa_collect_timer ?? "",
        lte_whitelist_max: params.data.lte_whitelist_max ?? "",
        nrsa_whitelist_max: params.data.nrsa_whitelist_max ?? "",
      }
    : null;
  const shown = draft ?? base;
  const [sent, setSent] = useState<Params | null>(null);
  const [askParams, setAskParams] = useState(false);
  const paramsInline = useConfirmInline(askParams);
  const cancelParams = useCallback(() => setAskParams(false), []);
  const paramsOp = useWriteOp({
    tier: 2,
    steps: [{ label: t("stc.applyParameters", "Apply Parameters"), run: () => apiFetch("/api/cell/stc/params", { method: "PUT", body: sent }) }],
    verify: () =>
      settle(
        async () => {
          const d = await apiFetch<CellStcParams>("/api/cell/stc/params");
          await params.mutate(d, { revalidate: false });
          return d;
        },
        (d) => !!sent && FIELDS.every((f) => String(d[f] ?? "") === sent[f])
      ),
  });
  // Drop the draft once the device shows it.
  const [clearedRun, setClearedRun] = useState(0);
  if (paramsOp.phase === "applied" && paramsOp.runId !== clearedRun) {
    setClearedRun(paramsOp.runId);
    setDraft(null);
  }

  const busy = toggleOp.busy || resetOp.busy || paramsOp.busy;
  const stale = (!!status.data && status.stale) || (!!params.data && params.stale);

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("stc.loadingNd", "Reading STC…");
  let reason: ReactNode = null;
  if (unsupported) {
    tone = "neutral";
    state = t("stc.unsupported", "Not available on this firmware");
    reason = t(
      "stc.unsupportedDesc",
      "The device's firmware doesn't answer the STC whitelist requests (the methods aren't there), so STC can't be read or changed from this page. The controls below are locked."
    );
  } else if (serviceDown) {
    tone = "warn";
    state = t("stc.unavailable", "STC service unavailable");
    reason = t("stc.serviceUnavailable", "STC service is currently unavailable (503). Controls are disabled.");
  } else if (enabled !== null) {
    tone = enabled ? "ok" : "neutral";
    state = enabled ? t("stc.enabled", "Enabled") : t("stc.disabled", "Disabled");
    reason = enabled
      ? t("stc.onReason", "The modem only uses cells in the collected whitelist.")
      : t("stc.offReason", "The modem picks cells normally.");
  }
  if (stale && !serviceDown) tone = "stale";

  const refresh = () => {
    void status.mutate();
    void params.mutate();
  };

  const labels: Record<Field, { text: string; unit: string }> = {
    lte_collect_timer: { text: t("stc.lteCollectTimer", "LTE Collect Timer"), unit: t("stc.seconds", "seconds") },
    nrsa_collect_timer: { text: t("stc.nrsaCollectTimer", "NR-SA Collect Timer"), unit: t("stc.seconds", "seconds") },
    lte_whitelist_max: { text: t("stc.lteWhitelistMax", "LTE Whitelist Max"), unit: t("stc.count", "count") },
    nrsa_whitelist_max: { text: t("stc.nrsaWhitelistMax", "NR-SA Whitelist Max"), unit: t("stc.count", "count") },
  };
  const paramsLocked = !params.data || params.stale || busy;

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">STC</h1>
        <Button variant="ghost" iconOnly onPress={refresh} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>

      <div className="grid max-w-[720px] gap-6">
        <p className="nd-body -mt-2 text-nd-t2">
          {t("stc.descNd", "Whitelist cell lock: the modem collects the cells it sees into a whitelist, and with STC on it only uses those cells.")}
        </p>

        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={
            stale && !serviceDown ? (
              <>
                <Freshness stale lastOkAt={status.lastOkAt ?? params.lastOkAt} what={t("stc.settingsWord", "Settings")} />
                {t("stc.refreshToEdit", " — refresh before changing anything.")}
              </>
            ) : undefined
          }
          actions={
            !unsupported && (serviceDown || stale) ? (
              <Button variant="secondary" size="sm" onPress={refresh}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── on / off ── */}
        <section>
          <Group stale={status.stale}>
            <Row
              label={t("stc.switchLabel", "STC (whitelist cell lock)")}
              sub={
                unsupported
                  ? t("stc.notOnFirmware", "Not available on this firmware")
                  : enabled === null
                    ? undefined
                    : `${t("stc.currentState", "Current State")}: ${enabled ? t("stc.enabled", "Enabled") : t("stc.disabled", "Disabled")}`
              }
              value={unsupported ? "—" : undefined}
              control={
                enabled === null ? (
                  status.error ? undefined : <span className="nd-skel" />
                ) : (
                  <Switch
                    label="STC"
                    isSelected={toggleOp.busy ? want : enabled}
                    isDisabled={serviceDown || status.stale || busy}
                    onChange={(v) => {
                      setWant(v);
                      setDialog("toggle");
                    }}
                  />
                )
              }
            />
          </Group>
          <div className="mt-2 px-1">
            <OpResult op={toggleOp} />
          </div>
        </section>

        {/* ── parameters ── */}
        <section aria-labelledby="stc-params">
          <GroupTitle id="stc-params">{t("stc.parametersTitle", "Parameters")}</GroupTitle>
          <div className={`nd-group grid gap-4 p-4 lg:p-5${params.stale ? " nd-stale" : ""}`}>
            <div className="grid gap-4 sm:grid-cols-2">
              {FIELDS.map((f) => (
                <div key={f} className="grid gap-1">
                  <label htmlFor={`stc-${f}`} className="font-semibold">
                    {labels[f].text}
                  </label>
                  {shown ? (
                    <input
                      id={`stc-${f}`}
                      className="nd-field nd-mono"
                      inputMode="numeric"
                      value={shown[f]}
                      placeholder={labels[f].unit}
                      aria-describedby={`stc-${f}-unit`}
                      disabled={paramsLocked || askParams}
                      onChange={(e) => setDraft({ ...(shown as Params), [f]: e.target.value })}
                    />
                  ) : unsupported ? (
                    <input id={`stc-${f}`} className="nd-field nd-mono" value="" placeholder="—" aria-describedby={`stc-${f}-unit`} disabled readOnly />
                  ) : (
                    <span className="nd-field flex items-center">
                      <span className="nd-skel" />
                    </span>
                  )}
                  <span id={`stc-${f}-unit`} className="nd-aux">
                    {labels[f].unit}
                  </span>
                </div>
              ))}
            </div>
            <div>
              <Button
                onPress={() => {
                  // Fix what will be sent when the confirm opens; start() reads it later.
                  if (!askParams && shown) setSent({ ...shown });
                  setAskParams((o) => !o);
                }}
                isDisabled={paramsLocked || serviceDown || !shown}
                pending={paramsOp.busy}
                {...paramsInline.triggerProps}
              >
                {t("stc.applyParameters", "Apply Parameters")}
              </Button>
            </div>
            {askParams && sent && (
              <ConfirmInline
                id={paramsInline.id}
                open
                actionLabel={t("stc.applyParameters", "Apply Parameters")}
                consequence={t(
                  "stc.paramsConsequence",
                  "Whitelist collection uses the new timers and limits: LTE {{lt}} s / {{lm}} cells, NR-SA {{nt}} s / {{nm}} cells.",
                  { lt: sent.lte_collect_timer, lm: sent.lte_whitelist_max, nt: sent.nrsa_collect_timer, nm: sent.nrsa_whitelist_max }
                )}
                onCancel={cancelParams}
                onConfirm={() => {
                  setAskParams(false);
                  paramsOp.start();
                  paramsOp.confirm();
                }}
              />
            )}
            <OpResult op={paramsOp} />
          </div>
        </section>

        {/* ── whitelist reset ── */}
        <section aria-labelledby="stc-reset">
          <GroupTitle id="stc-reset">{t("stc.whitelistTitle", "Whitelist")}</GroupTitle>
          <div className="nd-group grid gap-3 p-4 lg:p-5">
            <p className="nd-body text-nd-t2">
              {t("stc.resetDesc", "Clear the collected cells. The modem starts collecting again from the cells it sees now.")}
            </p>
            <div>
              <Button variant="secondary" onPress={() => setDialog("reset")} isDisabled={busy || unsupported}>
                <ArrowCounterClockwise size={20} weight="bold" aria-hidden />
                {t("stc.resetWhitelist", "Reset Whitelist")}
              </Button>
            </div>
            <OpResult op={resetOp} />
          </div>
        </section>
      </div>

      <ConfirmDialog
        open={dialog === "toggle"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={want ? t("stc.confirmOnTitle", "Turn STC on?") : t("stc.confirmOffTitle", "Turn STC off?")}
        what={
          want
            ? t("stc.confirmOnWhat", "The modem only uses cells in the collected whitelist. If none of them is in reach, there is no mobile service until STC is off again.")
            : t("stc.confirmOffWhat", "The modem stops limiting itself to the whitelist and picks cells normally.")
        }
        downtime={t("stc.downtime", "The mobile connection may drop for about 30 seconds while the modem re-attaches.")}
        recovery={recovery}
        actionLabel={want ? t("stc.turnOn", "Turn STC on") : t("stc.turnOff", "Turn STC off")}
        cutsUplink
        onConfirm={() => {
          setDialog(null);
          toggleOp.start();
          toggleOp.confirm();
        }}
      />
      <ConfirmDialog
        open={dialog === "reset"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("stc.confirmResetTitle", "Reset the STC whitelist?")}
        what={t("stc.confirmReset", "Reset STC whitelist? This cannot be undone.")}
        downtime={t("stc.downtime", "The mobile connection may drop for about 30 seconds while the modem re-attaches.")}
        recovery={t("stc.resetRecovery", "The whitelist can't be restored; the modem collects a new one over time. Turn STC off if service is poor meanwhile.")}
        actionLabel={t("stc.resetWhitelist", "Reset Whitelist")}
        cutsUplink
        danger
        onConfirm={() => {
          setDialog(null);
          resetOp.start();
          resetOp.confirm();
        }}
      />
    </>
  );
}
