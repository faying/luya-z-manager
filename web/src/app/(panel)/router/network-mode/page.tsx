"use client";
// Network mode (settings page, design doc §3.1 / §5.1). Status first (the
// mode the device reports now), then the six choices, then Apply.
//
// Apply = tier 3 (network selection): dialog with what happens, ~30 s
// without a mobile connection and how to go back; then wait for the agent
// (waitDevice 30 s) and read back /api/network/signal `net_select`
// (polled 5 × 2 s like the old page). `net_select` is the radio-mode
// preference (measured on B27: "WL_AND_5G"); `net_select_mode` is something
// else — automatic vs manual network selection (auto_select/manual_select).
// The six choices, their order and names follow ZTE's own web UI for this
// model (/usr/zte_web/web/js/config/ufi/U60Pro/config.js AUTO_MODES:
// 5G/4G/3G, 5G NSA = LTE_AND_5G, 5G SA = Only_5G, 4G/3G, 4G Only, 3G Only);
// the firmware knows 14 (zte_topsw_nwinfo strings, see OTHER). "TCHGWL_5G"
// (every RAT + 5G) was read on B27 after a manual register and counts as Auto.
import { useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp } from "@/lib/api/writeOp";
import type { NetworkSignal } from "@/lib/api/schemas/network";
import type { ModemNetworkModeBody } from "@/lib/api/schemas/modem";
import { Button, ConfirmDialog, Freshness, GroupTitle, OpResult, StatusBlock, type Tone } from "@/components/nd";
import { ModeList, type ModeOption } from "./ModeList";

const SIGNAL = "/api/network/signal";
const VERIFY_TRIES = 5;
const VERIFY_EVERY_MS = 2000;

const OPTIONS: { value: string; label: string; desc: string; lk: string; dk: string }[] = [
  { value: "WL_AND_5G", label: "5G/4G/3G (Auto)", desc: "5G, 4G and 3G; the modem picks the best one", lk: "netmode.autoLabel", dk: "netmode.autoDesc" },
  { value: "LTE_AND_5G", label: "5G NSA only", desc: "5G carried by a 4G anchor (NSA), never SA. Connected, it shows both a 4G and a 5G carrier; where there is no NSA it stays on 4G.", lk: "netmode.nsaLabel", dk: "netmode.nsaDesc" },
  { value: "Only_5G", label: "5G SA only", desc: "Standalone 5G only. Many countries have no SA yet, and roaming SIMs often can't use it: abroad this can mean no signal.", lk: "netmode.nr5gLabel", dk: "netmode.nr5gDesc" },
  { value: "WCDMA_AND_LTE", label: "4G/3G", desc: "No 5G: LTE, falling back to WCDMA", lk: "netmode.mixLabel", dk: "netmode.mixDesc" },
  { value: "Only_LTE", label: "4G only", desc: "LTE only", lk: "netmode.lteLabel", dk: "netmode.lteDesc" },
  { value: "Only_WCDMA", label: "3G only", desc: "WCDMA only. Many countries have switched 3G off: there this means no signal.", lk: "netmode.wcdmaLabel", dk: "netmode.wcdmaDesc" },
];
const AUTO = "WL_AND_5G";
const isAuto = (v: string | undefined | null) => v === AUTO || v === "TCHGWL_5G";
/** Modes that leave the device with no signal in much of the world (it has no 2G to fall back on). */
const RISKY: Record<string, { text: string; k: string }> = {
  Only_5G: {
    k: "netmode.riskSa",
    text: "Many countries have no 5G SA yet, and roaming SIMs often can't use it where it exists. Abroad, pick Auto unless you know this network has SA for your SIM.",
  },
  Only_WCDMA: {
    k: "netmode.risk3g",
    text: "3G has been switched off in many countries (the US, much of Europe, Japan and others), and this device has no 2G. There, this mode means no mobile network at all.",
  },
};
/** Firmware values that are not among the six choices, in words. */
const OTHER: Record<string, { label: string; k: string }> = {
  TCHGWL_5G: { label: "Auto (every network type)", k: "netmode.allLabel" },
  WL_AND_NSA: { label: "5G NSA + 4G + 3G", k: "netmode.wlNsaLabel" },
  "4G_AND_5G": { label: "4G + 5G", k: "netmode.lte5gLabel" },
  GSM_AND_LTE: { label: "4G + 2G", k: "netmode.gsmLteLabel" },
  TDSCDMA_AND_LTE: { label: "4G + TD-SCDMA", k: "netmode.tdLteLabel" },
  Only_GSM_WCDMA: { label: "3G + 2G", k: "netmode.gsmWcdmaLabel" },
  Only_TDSCDMA: { label: "TD-SCDMA only", k: "netmode.tdLabel" },
  Only_GSM: { label: "2G only", k: "netmode.gsmLabel" },
};

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export default function NetworkModePage() {
  const { t } = useTranslation();
  const sig = useApi<NetworkSignal>(SIGNAL, { refreshInterval: 5000 });
  const data = sig.data;
  const reported = data?.net_select || undefined;
  // "TCHGWL_5G" is Auto too: mark the Auto choice as the active one.
  const current = isAuto(reported) ? AUTO : reported;

  const options: ModeOption[] = OPTIONS.map((o) => ({ value: o.value, label: t(o.lk, o.label), desc: t(o.dk, o.desc) }));
  const labelOf = (v: string | undefined | null) => {
    if (!v) return "—";
    const o = options.find((x) => x.value === v);
    if (o) return o.label;
    return OTHER[v] ? t(OTHER[v].k, OTHER[v].label) : v;
  };

  // Draft: null = follow what the device reports.
  const [draft, setDraft] = useState<string | null>(null);
  const selected = draft ?? current ?? null;
  const [dialog, setDialog] = useState(false);
  const wantRef = useRef<string | null>(null);
  const [lastSeen, setLastSeen] = useState<string | undefined>(undefined);

  const recovery = t("netmode.recovery", "Come back to this page and choose “Auto”. If the page can't be reached, set the network mode back on the device's touchscreen.");

  const op = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("netmode.stepSet", "Set network mode"),
        run: () =>
          apiFetch(`/api/modem/network-mode`, {
            method: "PUT",
            body: { net_select: wantRef.current ?? "" } satisfies ModemNetworkModeBody,
          }),
      },
    ],
    // Snapshotted at start(): without the field there is nothing to compare.
    verify: current !== undefined
      ? async () => {
          for (let i = 0; i < VERIFY_TRIES; i++) {
            if (i > 0) await sleep(VERIFY_EVERY_MS);
            const fresh = await apiFetch<NetworkSignal>(SIGNAL);
            setLastSeen(fresh.net_select);
            if (fresh.net_select === wantRef.current || (isAuto(wantRef.current) && isAuto(fresh.net_select))) {
              await sig.mutate(fresh, { revalidate: false });
              setDraft(null);
              return true;
            }
          }
          return false;
        }
      : undefined,
    waitDevice: { expectedSec: 30, recovery },
  });

  const locked = !data || sig.stale || op.busy;
  const dirty = selected !== null && selected !== current;

  function apply() {
    setDialog(false);
    wantRef.current = selected;
    op.start();
    op.confirm();
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("netmode.loading", "Reading the network mode…");
  let reason: ReactNode = null;
  if (!data && sig.error) {
    tone = "bad";
    state = t("netmode.unreadable", "Can't read the network mode");
    reason = sig.error.message;
  } else if (data && current === undefined) {
    state = t("netmode.notReported", "The device doesn't report its network mode");
    reason = t("netmode.notReportedReason", "You can still choose a mode below; the result can't be read back.");
  } else if (data) {
    tone = current === AUTO ? "ok" : "warn";
    state = t("netmode.current", "Current: {{mode}}", { mode: labelOf(reported) });
    reason = current === AUTO ? null : t("netmode.fixedReason", "The modem only uses the networks this mode allows.");
  }
  // Separate from the radio mode: the operator itself was picked by hand.
  const manualOperator = data?.net_select_mode === "manual_select";
  if (data && sig.stale) tone = "stale";

  const want = selected;
  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("netmode.title", "Network Mode")}</h1>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={
            data && sig.stale ? (
              <Freshness stale lastOkAt={sig.lastOkAt} />
            ) : manualOperator ? (
              t("netmode.manualOperator", "The operator is chosen by hand (Mobile Network page); this mode only limits which radio it uses.")
            ) : undefined
          }
          actions={
            !data && sig.error ? (
              <Button variant="secondary" size="sm" onPress={() => sig.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        <section aria-labelledby="nm-select">
          <GroupTitle id="nm-select">{t("netmode.selectMode", "Select Mode")}</GroupTitle>
          <p className="nd-aux -mt-1 mb-3 px-1">{t("netmode.desc", "Select preferred radio access technology (RAT).")}</p>
          <ModeList
            label={t("netmode.selectMode", "Select Mode")}
            options={options}
            value={selected}
            current={current}
            currentLabel={t("netmode.active", "Active")}
            onChange={setDraft}
            isDisabled={locked}
          />
          {data && sig.stale && (
            <p className="nd-aux mt-2 px-1">
              <Freshness stale lastOkAt={sig.lastOkAt} what={t("netmode.settingsWord", "Settings")} />
              {t("netmode.refreshToEdit", " — refresh before changing anything.")}
            </p>
          )}
          <div className="mt-4 flex flex-wrap items-center justify-end gap-3">
            {dirty && !op.busy && (
              <Button variant="secondary" onPress={() => setDraft(null)}>
                {t("common.cancel", "Cancel")}
              </Button>
            )}
            <Button onPress={() => setDialog(true)} isDisabled={locked || !dirty} pending={op.busy}>
              {t("common.apply", "Apply")}
            </Button>
          </div>
          <div className="mt-2 grid gap-1 px-1">
            <OpResult op={op} />
            {op.phase === "failed" && op.errorKind === "mismatch" && (
              <p className="nd-aux">
                {t("netmode.stillReports", "The device still reports {{mode}}. It may still be switching; check again in a minute.", {
                  mode: labelOf(lastSeen),
                })}
              </p>
            )}
          </div>
        </section>
      </div>

      <ConfirmDialog
        open={dialog}
        onOpenChange={setDialog}
        title={t("netmode.confirmTitle", "Switch to {{mode}}?", { mode: labelOf(want) })}
        what={
          <>
            {want === AUTO
              ? t("netmode.confirmWhatAuto", "The modem picks the best available network again.")
              : t("netmode.confirmWhat", "The modem only uses {{mode}}. If that network isn't available here, there is no mobile connection.", { mode: labelOf(want) })}
            {want && RISKY[want] && <strong className="mt-2 block">{t(RISKY[want].k, RISKY[want].text)}</strong>}
          </>
        }
        downtime={t("netmode.downtime", "The mobile connection drops for about 30 seconds while the modem re-attaches.")}
        recovery={recovery}
        actionLabel={t("netmode.confirmAction", "Switch network mode")}
        cutsUplink
        onConfirm={apply}
      />
    </>
  );
}
