"use client";
// Device control (new design). Settings page: battery state first, then
// charge limit, power modes, then reboot / factory reset.
//
// Writes (controls-inventory §/router/device, design §3.1):
//   charge limit 「应用」  tier 2, PUT charge-control, readback GET charge-control
//   power-save switch     tier 2, PUT power-save (body kept exactly as the old
//                          page sent it: deviceInfoList as an array — which
//                          form the firmware accepts is unconfirmed), readback
//                          POST power-save (a read served over POST)
//   fast-boot switch      tier 2, PUT fast-boot, readback GET fast-boot
//   reboot                tier 3, POST reboot, wait for the device (≈90 s,
//                          must go down first); no readback → 「设备已接受」
//   factory reset         tier 3, type-to-confirm, POST factory-reset. The
//                          reset wipes zte-agent itself, so the device never
//                          answers this page again: the wait ends in its
//                          timeout, whose recovery text is what the user reads.
// Switches no longer flip optimistically: they show the device value until
// the readback confirms the change.
//
// Deliberately absent: any ZTE firmware update (FOTA) control.
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, BatteryCharging, Lightning, Warning } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp } from "@/lib/api/writeOp";
import type { ChargeControl, FastBoot, PowerSave } from "@/lib/api/schemas/device";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

interface Draft {
  enabled: boolean;
  limit: number;
  hyst: number;
}

const PS_READ = { deviceInfoList: ["power_saver_mode"] };

type Inline = null | { k: "charge" } | { k: "ps"; on: boolean } | { k: "fb"; on: boolean };

/** "1"/"0" → boolean; anything else (missing key, odd shape) → null. */
function flag(v: unknown): boolean | null {
  const s = v === undefined || v === null ? "" : String(v);
  if (s === "1") return true;
  if (s === "0") return false;
  return null;
}

function Slider({
  id,
  label,
  value,
  min,
  max,
  onChange,
  disabled,
}: {
  id: string;
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (v: number) => void;
  disabled?: boolean;
}) {
  return (
    <div className="nd-row flex-col items-stretch gap-2">
      <span className="flex items-baseline justify-between">
        <label htmlFor={id} className="nd-row__label">
          {label}
        </label>
        <span className="nd-mono nd-body" aria-hidden>
          {value}%
        </span>
      </span>
      <input
        id={id}
        type="range"
        min={min}
        max={max}
        value={value}
        disabled={disabled}
        aria-valuetext={`${value}%`}
        onChange={(e) => onChange(parseInt(e.target.value, 10))}
        className="h-11 w-full"
      />
      <span className="nd-aux flex justify-between" aria-hidden>
        <span>{min}%</span>
        <span>{max}%</span>
      </span>
    </div>
  );
}

export default function DevicePage() {
  const { t } = useTranslation();
  const charge = useApi<ChargeControl>("/api/device/charge-control");
  const fast = useApi<FastBoot>("/api/device/fast-boot");
  const ps = useApi<PowerSave>("/api/device/power-save", { method: "POST", body: PS_READ });

  const cd = charge.data;
  const psOn = ps.data ? flag(ps.data.power_saver_mode) : null;
  const fbOn = fast.data ? flag(fast.data.fast_boot) : null;

  // ── charge-limit draft (null = follow the device) ──
  const [draft, setDraft] = useState<Draft | null>(null);
  const fromDevice: Draft | null = cd
    ? { enabled: !!cd.charge_limit_enabled, limit: cd.charge_limit ?? 80, hyst: cd.hysteresis ?? 5 }
    : null;
  const cur = draft ?? fromDevice;
  const dirty =
    !!draft && !!fromDevice && (draft.enabled !== fromDevice.enabled || draft.limit !== fromDevice.limit || draft.hyst !== fromDevice.hyst);
  const edit = (p: Partial<Draft>) => cur && setDraft({ ...cur, ...p });

  const sentRef = useRef<Draft | null>(null);
  const chargeOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("devctl.chargeLimitTitle", "Charge Limit"),
        run: () => {
          const d = sentRef.current!;
          return apiFetch("/api/device/charge-control", {
            method: "PUT",
            body: { charge_limit_enabled: d.enabled, charge_limit: d.limit, hysteresis: d.hyst },
          });
        },
      },
    ],
    verify: async () => {
      const d = await apiFetch<ChargeControl>("/api/device/charge-control");
      await charge.mutate(d, { revalidate: false });
      const want = sentRef.current;
      if (!want) return false;
      const ok =
        d.charge_limit_enabled === want.enabled &&
        (!want.enabled || (d.charge_limit === want.limit && d.hysteresis === want.hyst));
      if (ok) setDraft(null);
      return ok;
    },
  });

  // ── power-save / fast-boot switches ──
  const psWant = useRef(false);
  const psOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("devctl.powerSaveMode", "Power-save mode"),
        run: () =>
          apiFetch("/api/device/power-save", {
            method: "PUT",
            // Unchanged from the old page (array form); unconfirmed on the device.
            body: { deviceInfoList: [{ power_saver_mode: psWant.current ? "1" : "0" }] },
          }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<PowerSave>("/api/device/power-save", { method: "POST", body: PS_READ });
      await ps.mutate(d, { revalidate: false });
      return flag(d?.power_saver_mode) === psWant.current;
    },
  });
  const fbWant = useRef(false);
  const fbOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("devctl.fastBoot", "Fast boot"),
        run: () => apiFetch("/api/device/fast-boot", { method: "PUT", body: { fast_boot: fbWant.current ? "1" : "0" } }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<FastBoot>("/api/device/fast-boot");
      await fast.mutate(d, { revalidate: false });
      return flag(d?.fast_boot) === fbWant.current;
    },
  });

  // ── reboot / factory reset ──
  const rebootRecovery = t(
    "devctl.rebootRecovery",
    "Wait a little longer, then reload this page. If it still doesn't load, check on the touchscreen that the device has started, and join its Wi-Fi again."
  );
  const rebootOp = useWriteOp({
    tier: 3,
    steps: [{ label: t("devctl.rebootTitle", "Reboot"), run: () => apiFetch("/api/device/reboot", { method: "POST" }) }],
    waitDevice: { expectedSec: 90, expectDown: true, recovery: rebootRecovery },
  });
  const resetRecovery = t(
    "devctl.resetRecovery",
    "This page will not come back: the reset removes this manager. The device returns with its factory Wi-Fi name, password and address (printed on its label). Join that Wi-Fi and run the install kit again to get this project's software back."
  );
  const resetOp = useWriteOp({
    tier: 3,
    steps: [{ label: t("devctl.factoryResetTitle", "Factory Reset"), run: () => apiFetch("/api/device/factory-reset", { method: "POST" }) }],
    waitDevice: { expectedSec: 90, timeoutSec: 300, expectDown: true, recovery: resetRecovery },
  });

  const [inline, setInline] = useState<Inline>(null);
  const inlineCtl = useConfirmInline(inline !== null);
  const inlineK = inline?.k ?? null;
  const [dialog, setDialog] = useState<null | "reboot" | "reset">(null);
  const anyBusy = chargeOp.busy || psOp.busy || fbOp.busy || rebootOp.busy || resetOp.busy;

  function askCharge() {
    if (!cur || anyBusy) return;
    setInline({ k: "charge" });
  }
  function goCharge() {
    sentRef.current = cur;
    chargeOp.start();
    chargeOp.confirm();
    setInline(null);
  }
  function askPs(on: boolean) {
    if (anyBusy) return;
    setInline({ k: "ps", on });
  }
  function goPs() {
    if (inline?.k !== "ps") return;
    psWant.current = inline.on;
    psOp.start();
    psOp.confirm();
    setInline(null);
  }
  function askFb(on: boolean) {
    if (anyBusy) return;
    setInline({ k: "fb", on });
  }
  function goFb() {
    if (inline?.k !== "fb") return;
    fbWant.current = inline.on;
    fbOp.start();
    fbOp.confirm();
    setInline(null);
  }
  function goDialog() {
    const which = dialog;
    setDialog(null);
    const op = which === "reboot" ? rebootOp : resetOp;
    op.start();
    op.confirm();
  }
  const trig = (k: "charge" | "ps" | "fb") => (inlineK === k ? inlineCtl.triggerProps : {});

  // ── status ──
  const capKnown = !!cd && !(cd.capacity === 0 && !cd.battery_status);
  // sysfs power_supply status words, shown in the page language.
  const statusWord = (s: string) =>
    ({
      Charging: t("devctl.bsCharging", "Charging"),
      Discharging: t("devctl.bsDischarging", "Discharging"),
      "Not charging": t("devctl.bsNotCharging", "Not charging"),
      Full: t("devctl.bsFull", "Full"),
    })[s] ?? s;
  const capText = capKnown ? `${cd!.capacity}%` : "—";
  let tone: Tone = "neutral";
  let state: string = t("devctl.loading", "Reading battery…");
  let reason: string | null = null;
  if (!cd && charge.error) {
    tone = "bad";
    state = t("devctl.unreadable", "Can't read the battery state");
    reason = charge.error.message;
  } else if (cd) {
    if (cd.charging_stopped) {
      tone = "warn";
      state = t("devctl.stStopped", "Battery {{cap}} · charging stopped", { cap: capText });
      reason = cd.charge_limit_enabled
        ? t("devctl.stStoppedLimit", "Charging is currently stopped (hardware-enforced). The limit resumes charging below {{low}}%.", {
            low: Math.max(0, (cd.charge_limit ?? 0) - (cd.hysteresis ?? 0)),
          })
        : t("devctl.chargingStopped", "Charging is currently stopped (hardware-enforced).");
    } else {
      tone = capKnown ? "ok" : "warn";
      state = capKnown
        ? t("devctl.stBattery", "Battery {{cap}}", { cap: capText })
        : t("devctl.stNoBattery", "Battery level not readable");
      reason = [
        cd.battery_status ? t("devctl.stStatus", "Status: {{s}}", { s: statusWord(cd.battery_status) }) : null,
        cd.charge_limit_enabled
          ? t("devctl.stLimitOn", "Charge limit {{limit}}%", { limit: cd.charge_limit })
          : t("devctl.stLimitOff", "No charge limit"),
      ]
        .filter(Boolean)
        .join(" · ");
    }
    if (charge.stale) tone = "stale";
  }

  const chargeLocked = !cur || charge.stale || anyBusy;

  const skeletonRow = (w: string) => (
    <div className="nd-row">
      <span className="nd-skel" style={{ width: w }} />
    </div>
  );

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("devctl.title", "Device Control")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("devctl.desc", "Charge limit, power modes, and reboot.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={cd && charge.stale ? <Freshness stale lastOkAt={charge.lastOkAt} /> : undefined}
          actions={
            charge.error ? (
              <Button variant="secondary" size="sm" onPress={() => charge.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── charge limit ── */}
        <section aria-labelledby="dc-charge">
          <GroupTitle id="dc-charge">{t("devctl.chargeLimitTitle", "Charge Limit")}</GroupTitle>
          <div className={`nd-group${charge.stale ? " nd-stale" : ""}`}>
            {!cur ? (
              <>
                {skeletonRow("14ch")}
                {skeletonRow("10ch")}
              </>
            ) : (
              <>
                <Row
                  icon={BatteryCharging}
                  label={t("devctl.enableChargeLimit", "Enable charge limit")}
                  sub={t("devctl.limitSub", "Stops charging at the limit; resumes when the battery drops by the hysteresis.")}
                  control={
                    <Switch
                      label={t("devctl.enableChargeLimit", "Enable charge limit")}
                      isSelected={cur.enabled}
                      isDisabled={chargeLocked}
                      onChange={(on) => edit({ enabled: on })}
                    />
                  }
                />
                {cur.enabled && (
                  <>
                    <Slider
                      id="dc-limit"
                      label={t("devctl.chargeLimitPct", "Charge limit %")}
                      value={cur.limit}
                      min={50}
                      max={100}
                      disabled={chargeLocked}
                      onChange={(v) => edit({ limit: v })}
                    />
                    {/* The agent accepts 1–20 (the old slider stopped at 10). */}
                    <Slider
                      id="dc-hyst"
                      label={t("devctl.hysteresisPct", "Hysteresis %")}
                      value={cur.hyst}
                      min={1}
                      max={20}
                      disabled={chargeLocked}
                      onChange={(v) => edit({ hyst: v })}
                    />
                  </>
                )}
                <Row
                  label={t("devctl.currentCapacityLabel", "Current capacity")}
                  value={cd?.battery_status ? `${capText} · ${statusWord(cd.battery_status)}` : capText}
                />
              </>
            )}
          </div>
          <div className="mt-3 flex flex-wrap items-center gap-3 px-1">
            <span {...trig("charge")}>
              <Button onPress={askCharge} isDisabled={chargeLocked} pending={chargeOp.busy}>
                {t("devctl.apply", "Apply")}
              </Button>
            </span>
            {dirty && <span className="nd-aux">{t("devctl.unsaved", "Not applied yet")}</span>}
          </div>
          {inlineK === "charge" && cur && (
            <ConfirmInline
              id={inlineCtl.id}
              open
              actionLabel={t("devctl.applyAction", "apply the charge limit")}
              consequence={
                cur.enabled
                  ? t("devctl.cChargeOn", "Charging stops at {{limit}}% and starts again below {{low}}%.", {
                      limit: cur.limit,
                      low: cur.limit - cur.hyst,
                    })
                  : t("devctl.cChargeOff", "The limit is removed; the battery charges to 100%.")
              }
              onCancel={() => setInline(null)}
              onConfirm={goCharge}
            />
          )}
          <div className="mt-2 px-1">
            <OpResult op={chargeOp} />
          </div>
          {charge.stale && cd && (
            <p className="nd-aux mt-2 px-1">
              <Freshness stale lastOkAt={charge.lastOkAt} what={t("wifi.settingsWord", "Settings")} />
              {t("wifi.refreshToEdit", " — refresh before changing anything.")}{" "}
              <Button variant="secondary" size="sm" onPress={() => charge.mutate()}>
                {t("wifi.refresh", "Refresh")}
              </Button>
            </p>
          )}
        </section>

        {/* ── power modes ── */}
        <section aria-labelledby="dc-power">
          <GroupTitle id="dc-power">{t("devctl.powerModesTitle", "Power Modes")}</GroupTitle>
          <div className="nd-group">
            <Row
              icon={Lightning}
              label={t("devctl.powerSaveMode", "Power-save mode")}
              sub={
                !ps.data && !ps.error
                  ? t("devctl.reading", "Reading…")
                  : psOn === null
                    ? t("devctl.psUnknown", "The device didn't report this setting.")
                    : undefined
              }
              value={psOn === null ? "—" : undefined}
              control={
                <span {...trig("ps")}>
                  <Switch
                    label={t("devctl.powerSaveMode", "Power-save mode")}
                    isSelected={psOn === true}
                    isDisabled={psOn === null || ps.stale || anyBusy}
                    onChange={askPs}
                  />
                </span>
              }
            />
            <Row
              icon={ArrowClockwise}
              label={t("devctl.fastBoot", "Fast boot")}
              sub={
                !fast.data && !fast.error
                  ? t("devctl.reading", "Reading…")
                  : fbOn === null
                    ? t("devctl.fbUnknown", "The device didn't report this setting.")
                    : t("devctl.fastBootHint", "Skips some init steps on reboot")
              }
              value={fbOn === null ? "—" : undefined}
              control={
                <span {...trig("fb")}>
                  <Switch
                    label={t("devctl.fastBoot", "Fast boot")}
                    isSelected={fbOn === true}
                    isDisabled={fbOn === null || fast.stale || anyBusy}
                    onChange={askFb}
                  />
                </span>
              }
            />
          </div>
          {(ps.error || fast.error) && (
            <div className="mt-2 flex flex-wrap items-center gap-3 px-1" role="alert">
              <span className="nd-aux">
                {ps.error
                  ? t("devctl.psReadFailed", "Couldn't read power-save mode: {{e}}", { e: ps.error.message })
                  : t("devctl.fbReadFailed", "Couldn't read fast boot: {{e}}", { e: fast.error!.message })}
              </span>
              <Button
                variant="secondary"
                size="sm"
                onPress={() => {
                  if (ps.error) void ps.mutate();
                  if (fast.error) void fast.mutate();
                }}
              >
                {t("common.retry", "Retry")}
              </Button>
            </div>
          )}
          {inline?.k === "ps" && (
            <ConfirmInline
              id={inlineCtl.id}
              open
              actionLabel={
                inline.on ? t("devctl.psOnAction", "turn power-save on") : t("devctl.psOffAction", "turn power-save off")
              }
              consequence={
                inline.on
                  ? t("devctl.cPsOn", "The device switches to the firmware's power-save mode.")
                  : t("devctl.cPsOff", "The device leaves power-save mode.")
              }
              onCancel={() => setInline(null)}
              onConfirm={goPs}
            />
          )}
          {inline?.k === "fb" && (
            <ConfirmInline
              id={inlineCtl.id}
              open
              actionLabel={
                inline.on ? t("devctl.fbOnAction", "turn fast boot on") : t("devctl.fbOffAction", "turn fast boot off")
              }
              consequence={
                inline.on
                  ? t("devctl.cFbOn", "The next start skips some init steps. Takes effect at the next reboot.")
                  : t("devctl.cFbOff", "The next start runs the full init. Takes effect at the next reboot.")
              }
              onCancel={() => setInline(null)}
              onConfirm={goFb}
            />
          )}
          <div className="mt-2 grid gap-1 px-1">
            {psOp.phase !== "idle" && psOp.phase !== "confirming" && (
              <>
                <span className="nd-aux">{t("devctl.powerSaveMode", "Power-save mode")}</span>
                <OpResult op={psOp} />
              </>
            )}
            {fbOp.phase !== "idle" && fbOp.phase !== "confirming" && (
              <>
                <span className="nd-aux">{t("devctl.fastBoot", "Fast boot")}</span>
                <OpResult op={fbOp} />
              </>
            )}
          </div>
        </section>

        {/* ── reboot / factory reset ── */}
        <section aria-labelledby="dc-restart">
          <GroupTitle id="dc-restart">{t("devctl.restartTitle", "Restart and reset")}</GroupTitle>
          <div className="nd-group">
            <div className="nd-row nd-row--two flex-wrap">
              <ArrowClockwise size={20} weight="bold" className="nd-row__icon" aria-hidden />
              <span className="nd-row__text">
                <span className="nd-row__label">{t("devctl.rebootTitle", "Reboot")}</span>
                <span className="nd-row__sub block">{t("devctl.rebootWarning", "Router will reboot and be temporarily unreachable.")}</span>
              </span>
              <Button variant="secondary" onPress={() => setDialog("reboot")} isDisabled={anyBusy}>
                {t("devctl.rebootDevice", "Reboot Device")}
              </Button>
            </div>
            <div className="nd-row nd-row--two flex-wrap">
              <Warning size={20} weight="bold" className="nd-row__icon" aria-hidden />
              <span className="nd-row__text">
                <span className="nd-row__label">{t("devctl.factoryResetTitle", "Factory Reset")}</span>
                <span className="nd-row__sub block">
                  {t("devctl.resetSub", "Erases every setting and this project's software. Needs the install kit again.")}
                </span>
              </span>
              <Button variant="danger" onPress={() => setDialog("reset")} isDisabled={anyBusy}>
                {t("devctl.factoryReset", "Factory Reset")}
              </Button>
            </div>
          </div>
          <div className="mt-2 grid gap-1 px-1">
            {rebootOp.phase !== "idle" && rebootOp.phase !== "confirming" && <OpResult op={rebootOp} />}
            {resetOp.phase !== "idle" && resetOp.phase !== "confirming" && <OpResult op={resetOp} />}
          </div>
        </section>
      </div>

      <ConfirmDialog
        open={dialog === "reboot"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("devctl.confirmRebootTitle", "Reboot the device?")}
        what={t("devctl.confirmRebootWhat", "The U60 restarts. Wi-Fi, the mobile connection and this page drop until it is back.")}
        downtime={t("devctl.rebootDowntime", "About 90 seconds. This page waits and reconnects by itself; you may need to sign in again.")}
        recovery={rebootRecovery}
        actionLabel={t("devctl.confirmReboot", "Confirm Reboot")}
        cutsUplink
        onConfirm={goDialog}
      />
      <ConfirmDialog
        open={dialog === "reset"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("devctl.confirmResetTitle", "Erase everything and restore factory settings?")}
        what={t(
          "devctl.confirmResetWhat",
          "Every setting is erased — Wi-Fi, APN, locks, SMS forwarding — and so is this project's software: this manager, the touchscreen interface and Tailscale. This cannot be undone."
        )}
        downtime={t("devctl.resetDowntime", "The device restarts; this page does not come back.")}
        recovery={resetRecovery}
        actionLabel={t("devctl.resetNow", "Reset Now")}
        typeToConfirm={t("devctl.resetWord", "factory reset")}
        danger
        cutsUplink
        onConfirm={goDialog}
      />
    </>
  );
}
