"use client";
// USB (new design). Look page: USB state first, then the two controls, then
// the charger detail.
//
// Writes (controls-inventory §/usb, design §3.1):
//   power bank switch  tier 2, PUT /api/usb/powerbank {state} (key unconfirmed
//                      on the firmware; kept as the old page sent it),
//                      readback GET /api/device/charger otg_powerbank_state
//   USB mode           tier 3 (was a window.confirm; DEBUG = opening ADB),
//                      PUT /api/usb/mode {mode}, readback GET /api/usb/status
//                      mode. A computer on USB (RNDIS) loses its link, so
//                      wait for the agent before reading back.
// Unknown is not "off": fields the device doesn't send show "—".
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { BatteryCharging, Plug, Usb } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp } from "@/lib/api/writeOp";
import type { ChargerInfo, UsbStatus } from "@/lib/api/schemas/device";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  Row,
  Segmented,
  StatusBlock,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const USB_MODES = ["debug", "mtp", "rndis"] as const;
type UsbMode = (typeof USB_MODES)[number];

/** 1/"1"/true → true, 0/"0"/false → false, anything else → null (unknown). */
function tri(v: unknown): boolean | null {
  if (v === undefined || v === null || v === "") return null;
  if (typeof v === "boolean") return v;
  if (typeof v === "number") return v !== 0;
  const s = String(v).toLowerCase();
  if (s === "1" || s === "true" || s === "yes") return true;
  if (s === "0" || s === "false" || s === "no") return false;
  return null;
}

export default function UsbPage() {
  const { t } = useTranslation();
  const usbApi = useApi<UsbStatus>("/api/usb/status", { refreshInterval: 3000 });
  const chgApi = useApi<ChargerInfo>("/api/device/charger", { refreshInterval: 5000 });
  const usb = usbApi.data;
  const chg = chgApi.data;

  const noCc = usb?.typec_cc === "no_cc";
  const cable = tri(usb?.connect) ?? (noCc ? false : null);
  const mode = typeof usb?.mode === "string" && usb.mode ? usb.mode : null;
  const knownMode = mode && (USB_MODES as readonly string[]).includes(mode) ? (mode as UsbMode) : null;
  const powerbank = tri(chg?.otg_powerbank_state);
  const rj45 = tri(usb?.usb2rj45);
  const chgConnect = tri(chg?.charger_connect);
  const direct = chg?.direct_power_supply_mode;

  // ── power bank (tier 2) ──
  const [pbAsk, setPbAsk] = useState<boolean | null>(null);
  const pbWant = useRef(false);
  const pbInline = useConfirmInline(pbAsk !== null);
  const pbOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("usb.powerbankMode", "Powerbank Mode"),
        run: () => apiFetch("/api/usb/powerbank", { method: "PUT", body: { state: pbWant.current ? 1 : 0 } }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<ChargerInfo>("/api/device/charger");
      await chgApi.mutate(d, { revalidate: false });
      return tri(d?.otg_powerbank_state) === pbWant.current;
    },
  });

  // ── USB mode (tier 3) ──
  const [modeAsk, setModeAsk] = useState<UsbMode | null>(null);
  const modeWant = useRef<UsbMode>("mtp");
  const modeRecovery = t(
    "usb.modeRecovery",
    "If a computer on the USB cable lost its connection, unplug and replug the cable. To undo, pick the previous mode ({{mode}}) here from a device on the U60's Wi-Fi.",
    { mode: mode ? mode.toUpperCase() : "—" }
  );
  const modeOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("usb.usbMode", "USB Mode"),
        run: () => apiFetch("/api/usb/mode", { method: "PUT", body: { mode: modeWant.current } }),
      },
    ],
    waitDevice: { expectedSec: 30, recovery: modeRecovery },
    verify: async () => {
      const d = await apiFetch<UsbStatus>("/api/usb/status");
      await usbApi.mutate(d, { revalidate: false });
      return d?.mode === modeWant.current;
    },
  });

  const busy = pbOp.busy || modeOp.busy;

  function goPb() {
    if (pbAsk === null) return;
    pbWant.current = pbAsk;
    pbOp.start();
    pbOp.confirm();
    setPbAsk(null);
  }
  function goMode() {
    if (!modeAsk) return;
    modeWant.current = modeAsk;
    setModeAsk(null);
    modeOp.start();
    modeOp.confirm();
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: string = t("usb.loading", "Reading USB…");
  let reason: string | null = null;
  if (!usb && usbApi.error) {
    tone = "bad";
    state = t("usb.unreadable", "Can't read the USB state");
    reason = usbApi.error.message;
  } else if (usb) {
    const modeText = mode ? mode.toUpperCase() : "—";
    if (cable === true) {
      tone = "ok";
      state = t("usb.stAttached", "USB cable attached · {{mode}}", { mode: modeText });
    } else if (cable === false) {
      state = t("usb.stDetached", "No USB cable · mode {{mode}}", { mode: modeText });
    } else {
      tone = "warn";
      state = t("usb.stUnknown", "USB link state unknown · mode {{mode}}", { mode: modeText });
      reason = t("usb.stUnknownReason", "The device didn't report whether a cable is attached.");
    }
    if (powerbank === true) reason = [reason, t("usb.pbOnNote", "Power bank is on: the U60 is charging whatever is plugged in.")].filter(Boolean).join(" ");
    if (usbApi.stale) tone = "stale";
  }

  const directMeaning =
    direct === "enable"
      ? t("usb.directEnable", "charging stopped")
      : direct === "disable"
        ? t("usb.directDisable", "charging allowed")
        : null;
  const dash = "—";

  const modeLabel = (m: UsbMode) => m.toUpperCase();
  const modeWhat: Record<UsbMode, string> = {
    debug: t(
      "usb.whatDebug",
      "USB switches to ADB debugging. Anyone who can plug into the USB port gets a full root shell on the device. Only do this if you have the device with you and trust where it is."
    ),
    mtp: t("usb.whatMtp", "USB switches to file transfer (MTP). A computer on the cable stops using the U60 as a network adapter."),
    rndis: t("usb.whatRndis", "USB switches to a network adapter (RNDIS): a computer on the cable gets its connection through the U60."),
  };

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">USB</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("usb.desc", "USB connection status and mode control.")}</p>

      <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        <div className="grid content-start gap-6">
          <StatusBlock
            tone={tone}
            state={state}
            reason={reason}
            meta={usb && usbApi.stale ? <Freshness stale lastOkAt={usbApi.lastOkAt} /> : undefined}
            actions={
              usbApi.error ? (
                <Button variant="secondary" size="sm" onPress={() => usbApi.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              ) : undefined
            }
          />

          {/* ── USB mode ── */}
          <section aria-labelledby="usb-mode">
            <GroupTitle id="usb-mode">{t("usb.usbMode", "USB Mode")}</GroupTitle>
            <div className={`nd-group grid gap-3 p-4 lg:p-5${usbApi.stale ? " nd-stale" : ""}`}>
              <p className="nd-body text-nd-t2">
                {t("usb.usbModeHint", "Select the USB connection mode. Changing mode will reconnect the USB interface.")}
              </p>
              <Segmented<UsbMode>
                label={t("usb.usbMode", "USB Mode")}
                value={knownMode}
                isDisabled={!usb || usbApi.stale || busy}
                onChange={(m) => setModeAsk(m)}
                options={USB_MODES.map((m) => ({ id: m, label: modeLabel(m) }))}
              />
              {mode && !knownMode && (
                <span className="nd-aux">
                  {t("usb.otherMode", "Current mode: ")}
                  <span className="nd-mono">{mode}</span>
                </span>
              )}
              <OpResult op={modeOp} />
            </div>
          </section>

          {/* ── power bank ── */}
          <section aria-labelledby="usb-pb">
            <GroupTitle id="usb-pb">{t("usb.powerbankMode", "Powerbank Mode")}</GroupTitle>
            <div className={`nd-group${chgApi.stale ? " nd-stale" : ""}`}>
              <Row
                icon={BatteryCharging}
                label={t("usb.powerbankMode", "Powerbank Mode")}
                sub={
                  powerbank === null && chg
                    ? t("usb.pbUnknown", "The device didn't report the power bank state.")
                    : t("usb.powerbankHint", "When enabled, the device acts as a USB power bank and charges connected devices.")
                }
                value={powerbank === null ? dash : undefined}
                control={
                  <span {...pbInline.triggerProps}>
                    <Switch
                      label={t("usb.powerbankMode", "Powerbank Mode")}
                      isSelected={powerbank === true}
                      isDisabled={!chg || powerbank === null || chgApi.stale || busy}
                      onChange={(on) => setPbAsk(on)}
                    />
                  </span>
                }
              />
            </div>
            <ConfirmInline
              id={pbInline.id}
              open={pbAsk !== null}
              actionLabel={pbAsk ? t("usb.pbOnAction", "turn the power bank on") : t("usb.pbOffAction", "turn the power bank off")}
              consequence={
                pbAsk
                  ? t("usb.cPbOn", "The U60 starts charging whatever is plugged into its USB port, from its own battery.")
                  : t("usb.cPbOff", "The U60 stops charging the device on its USB port.")
              }
              onCancel={() => setPbAsk(null)}
              onConfirm={goPb}
            />
            <div className="mt-2 px-1">
              <OpResult op={pbOp} />
            </div>
          </section>
        </div>

        {/* ── detail ── */}
        <div className="grid content-start gap-6">
          <section aria-labelledby="usb-link">
            <GroupTitle id="usb-link">{t("usb.linkTitle", "USB port")}</GroupTitle>
            <div className={`nd-group${usbApi.stale ? " nd-stale" : ""}`}>
              <Row
                icon={Usb}
                label={t("usb.cable", "Cable")}
                value={!usb ? dash : cable === null ? dash : cable ? t("usb.attached", "Attached") : t("usb.detached", "Detached")}
              />
              <Row label={t("usb.usbCCc", "USB-C CC")} value={usb?.typec_cc || dash} mono />
              <Row label={t("usb.mode", "Mode")} value={mode ? mode.toUpperCase() : dash} mono />
              <Row
                label={t("usb.rj45", "USB-to-Ethernet adapter")}
                value={rj45 === null ? dash : rj45 ? t("usb.present", "Present") : t("usb.absent", "None")}
              />
            </div>
          </section>

          <section aria-labelledby="usb-chg">
            <GroupTitle id="usb-chg">{t("usb.chargerTitle", "Charger")}</GroupTitle>
            <div className={`nd-group${chgApi.stale ? " nd-stale" : ""}`}>
              <Row
                icon={Plug}
                label={t("usb.chargerConnected", "Charger connected")}
                value={chgConnect === null ? dash : chgConnect ? t("usb.yes", "Yes") : t("usb.no", "No")}
              />
              <Row label={t("usb.chargerType", "Charger Type")} value={chg?.charger_type != null ? String(chg.charger_type) : dash} mono />
              <Row label={t("usb.chargeStatus", "Charge status (raw)")} value={chg?.charge_status != null ? String(chg.charge_status) : dash} mono />
              <Row
                label={t("usb.directPowerLabel", "Direct power")}
                sub={directMeaning ?? undefined}
                value={direct || dash}
                mono
              />
            </div>
            {chgApi.error && (
              <div className="mt-2 flex flex-wrap items-center gap-3 px-1" role="alert">
                <span className="nd-aux">{t("usb.chgReadFailed", "Couldn't read the charger: {{e}}", { e: chgApi.error.message })}</span>
                <Button variant="secondary" size="sm" onPress={() => chgApi.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              </div>
            )}
            {chg && chgApi.stale && (
              <p className="nd-aux mt-2 px-1">
                <Freshness stale lastOkAt={chgApi.lastOkAt} />
              </p>
            )}
          </section>
        </div>
      </div>

      <ConfirmDialog
        open={modeAsk !== null}
        onOpenChange={(o) => !o && setModeAsk(null)}
        title={t("usb.confirmModeTitle", "Switch USB to {{mode}}?", { mode: modeAsk ? modeLabel(modeAsk) : "" })}
        what={modeAsk ? modeWhat[modeAsk] : ""}
        downtime={t("usb.modeDowntime", "The USB link drops and comes back in a few seconds. Wi-Fi and the mobile connection are not affected.")}
        recovery={modeRecovery}
        actionLabel={t("usb.confirmModeAction", "Switch to {{mode}}", { mode: modeAsk ? modeLabel(modeAsk) : "" })}
        danger={modeAsk === "debug"}
        cutsUplink
        onConfirm={goMode}
      />
    </>
  );
}
