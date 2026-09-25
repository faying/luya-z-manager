"use client";
// SIM / PIN (new design). Settings page: status first (ready / PIN-locked /
// PUK-locked, with the attempts left), then the SIM details, then the
// actions. Each action opens a small form; submitting it opens a tier-3
// dialog, because every one of them can use up an attempt on the card
// (PIN → PUK lock; PUK → card locked for good; NCK → no more unlock tries).
// The dialogs quote the attempts left as the device reports them.
//
// Writes (controls-inventory §/router/sim; all ubus passthroughs that never
// check the firmware's result, sim.rs:20-62):
//   verify PIN   POST /api/sim/pin/verify   readback sim_states no longer "wait pin"
//   verify PUK   POST /api/sim/pin/verify   readback sim_states no longer "wait puk"
//   PIN lock     POST /api/sim/pin/mode     readback pin_status
//   change PIN   POST /api/sim/pin/change   no readback (the PIN can't be read);
//                a drop in `pinnumber` right after means the current PIN was wrong
//   NCK unlock   POST /api/sim/unlock       no readback
// After every write the SIM info and NCK trials are read again, whatever
// the outcome, so the attempts shown are current.
import { useEffect, useId, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, LockKey, LockKeyOpen, Password, ShieldWarning } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp, type WriteStep } from "@/lib/api/writeOp";
import type { SimInfo, SimLockTrials } from "@/lib/api/schemas/sim";
import {
  Button,
  ConfirmDialog,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  type Tone,
} from "@/components/nd";

type Panel = "verify-pin" | "verify-puk" | "change-pin" | "pin-mode" | "nck";

/** The action the dialog / op will send, fixed when the form is submitted. */
interface Action {
  kind: Panel;
  pin: string;
  oldPin: string;
  newPin: string;
  puk: string;
  nck: string;
  enable: boolean;
}

const PIN_STATES = ["wait pin", "modem_waitpin"];
const PUK_STATES = ["wait puk", "modem_waitpuk"];

function inState(info: SimInfo | undefined, states: string[]): boolean {
  return (
    states.includes((info?.sim_states ?? "").toLowerCase()) || states.includes((info?.modem_main_state ?? "").toLowerCase())
  );
}

/** Read until `ok` holds (the firmware can lag a write by a moment). */
async function settle<T>(read: () => Promise<T>, ok: (d: T) => boolean, tries = 3, gapMs = 2000): Promise<boolean> {
  for (let i = 0; i < tries; i++) {
    if (ok(await read())) return true;
    if (i < tries - 1) await new Promise((r) => setTimeout(r, gapMs));
  }
  return false;
}

const num = (v?: string) => (v !== undefined && v !== "" && /^\d+$/.test(v) ? Number(v) : null);

export default function SIMPage() {
  const { t } = useTranslation();
  const sim = useApi<SimInfo>("/api/sim/info");
  const trials = useApi<SimLockTrials>("/api/sim/lock-trials");
  const info = sim.data;

  const pinEnabled = info?.pin_status === "1";
  const pinLocked = inState(info, PIN_STATES);
  const pukLocked = inState(info, PUK_STATES);
  const pinLeft = num(info?.pinnumber);
  const pukLeft = num(info?.puknumber);
  const nckLeft = num(trials.data?.available_trials);

  const [panel, setPanel] = useState<Panel | null>(null);
  const [pinInput, setPinInput] = useState("");
  const [oldPin, setOldPin] = useState("");
  const [newPin, setNewPin] = useState("");
  const [pukInput, setPukInput] = useState("");
  const [nckInput, setNckInput] = useState("");
  const [formErr, setFormErr] = useState<string | null>(null);
  const [action, setAction] = useState<Action | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);

  const readInfo = async () => {
    const d = await apiFetch<SimInfo>("/api/sim/info");
    await sim.mutate(d, { revalidate: false });
    return d;
  };

  // ── one write op; the steps come from the submitted action ──
  const steps: WriteStep[] = [];
  let verify: (() => Promise<boolean>) | undefined;
  if (action) {
    const a = action;
    switch (a.kind) {
      case "verify-pin":
        steps.push({
          label: t("sim.verifyPin", "Verify PIN"),
          run: () => apiFetch("/api/sim/pin/verify", { method: "POST", body: { pin_num: a.pin, puk_num: "", pin_encode_flag: "0" } }),
        });
        verify = () => settle(readInfo, (d) => !inState(d, PIN_STATES));
        break;
      case "verify-puk":
        steps.push({
          label: t("sim.verifyPuk", "Verify PUK"),
          run: () => apiFetch("/api/sim/pin/verify", { method: "POST", body: { pin_num: a.newPin, puk_num: a.puk, pin_encode_flag: "0" } }),
        });
        verify = () => settle(readInfo, (d) => !inState(d, PUK_STATES));
        break;
      case "pin-mode":
        steps.push({
          label: a.enable ? t("sim.enablePinLock", "Enable PIN Lock") : t("sim.disablePinLock", "Disable PIN Lock"),
          run: () =>
            apiFetch("/api/sim/pin/mode", {
              method: "POST",
              body: { pin_num_m: a.pin, pin_mode: a.enable ? "1" : "0", pin_encode_flag: "0" },
            }),
        });
        verify = () => settle(readInfo, (d) => (d.pin_status === "1") === a.enable);
        break;
      case "change-pin": {
        const before = pinLeft;
        steps.push({
          label: t("sim.changePin", "Change PIN"),
          run: async () => {
            await apiFetch("/api/sim/pin/change", { method: "POST", body: { pin_num: a.oldPin, new_pin_num: a.newPin, pin_encode_flag: "0" } });
            // No readback for a PIN; a burnt attempt is the one visible sign it failed.
            const after = num((await readInfo()).pinnumber);
            if (before !== null && after !== null && after < before) {
              throw new Error(t("sim.wrongCurrentPin", "The current PIN was not accepted. {{n}} attempt(s) left.", { n: after }));
            }
          },
        });
        break;
      }
      case "nck":
        steps.push({
          label: t("sim.unlockSim", "Unlock SIM"),
          run: () => apiFetch("/api/sim/unlock", { method: "POST", body: { nck: a.nck } }),
        });
        break;
    }
  }
  const op = useWriteOp({ tier: 3, steps, verify });

  // Whatever the outcome, re-read the card so the attempts shown are current.
  const settled = !op.busy && op.phase !== "idle" && op.phase !== "confirming";
  const { mutate: mutateSim } = sim;
  const { mutate: mutateTrials } = trials;
  useEffect(() => {
    if (!settled) return;
    void mutateSim();
    void mutateTrials();
  }, [settled, op.runId, mutateSim, mutateTrials]);

  // Close the form once the device took it; keep it (and the input) otherwise.
  const [closedRun, setClosedRun] = useState(0);
  if ((op.phase === "applied" || op.phase === "accepted") && op.runId !== closedRun) {
    setClosedRun(op.runId);
    setPanel(null);
    setPinInput("");
    setOldPin("");
    setNewPin("");
    setPukInput("");
    setNckInput("");
  }

  const locked = !info || sim.stale || op.busy;

  function openPanel(p: Panel) {
    setPanel((cur) => (cur === p ? null : p));
    setPinInput("");
    setOldPin("");
    setNewPin("");
    setPukInput("");
    setNckInput("");
    setFormErr(null);
  }

  function submit(kind: Panel) {
    setFormErr(null);
    const a: Action = { kind, pin: pinInput, oldPin, newPin, puk: pukInput, nck: nckInput.trim(), enable: !pinEnabled };
    if (kind === "verify-pin" || kind === "pin-mode") {
      if (a.pin.length < 4) return setFormErr(t("sim.msgPinMin", "PIN must be at least 4 digits"));
    } else if (kind === "verify-puk") {
      if (a.puk.length < 8) return setFormErr(t("sim.msgPukMin", "PUK must be at least 8 digits"));
      if (a.newPin.length < 4) return setFormErr(t("sim.msgNewPinMin", "New PIN must be at least 4 digits"));
    } else if (kind === "change-pin") {
      if (a.oldPin.length < 4 || a.newPin.length < 4) return setFormErr(t("sim.msgPinMin", "PIN must be at least 4 digits"));
    } else if (kind === "nck") {
      if (!a.nck) return setFormErr(t("sim.msgNckRequired", "Unlock code is required"));
    }
    setAction(a);
    setDialogOpen(true);
  }

  function confirmAction() {
    setDialogOpen(false);
    op.start();
    op.confirm();
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("sim.loadingNd", "Reading the SIM…");
  let reason: ReactNode = null;
  const loadErr = !info && sim.error;
  if (loadErr) {
    tone = "bad";
    state = t("sim.unreadable", "Can't read the SIM");
    reason = sim.error?.message;
  } else if (pukLocked) {
    tone = "bad";
    state = t("sim.statePuk", "SIM locked by PUK");
    reason = t("sim.pukLocked", "SIM is PUK-locked. Enter PUK to unlock.");
  } else if (pinLocked) {
    tone = "bad";
    state = t("sim.statePin", "SIM locked by PIN");
    reason = t("sim.pinLocked", "SIM is PIN-locked. Enter PIN to unlock.");
  } else if (info) {
    const raw = (info.sim_states ?? "").toLowerCase();
    const ready = raw.includes("ready") || raw.includes("complete");
    tone = ready ? "ok" : "neutral";
    state = ready ? t("sim.stateReady", "SIM ready") : t("sim.stateOther", "SIM state: {{s}}", { s: info.sim_states || "—" });
    reason = pinEnabled
      ? t("sim.pinOnReason", "PIN lock is on: the PIN is asked for at every start-up.")
      : info.pin_status === "0"
        ? t("sim.pinOffReason", "PIN lock is off.")
        : null;
  }
  if (info && sim.stale) tone = "stale";
  const attempts =
    pukLocked && pukLeft !== null
      ? t("sim.pukLeft", "{{n}} PUK attempt(s) left", { n: pukLeft })
      : pinLocked && pinLeft !== null
        ? t("sim.pinLeft", "{{n}} PIN attempt(s) left", { n: pinLeft })
        : null;

  const refresh = () => {
    void sim.mutate();
    void trials.mutate();
  };

  const pinField = (id: string, labelText: string, value: string, set: (v: string) => void, hint?: string) => (
    <PinField id={id} label={labelText} value={value} onChange={set} hint={hint} disabled={op.busy} />
  );

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">SIM / PIN</h1>
        <Button variant="ghost" iconOnly onPress={refresh} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>

      <div className="grid max-w-[720px] gap-6">
        <p className="nd-body -mt-2 text-nd-t2">{t("sim.desc", "SIM status and PIN management.")}</p>

        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={
            info && sim.stale ? (
              <>
                <Freshness stale lastOkAt={sim.lastOkAt} what={t("sim.infoWord", "SIM details")} />
                {t("sim.refreshToEdit", " — refresh before changing anything.")}
              </>
            ) : (
              attempts ?? undefined
            )
          }
          actions={
            loadErr || (info && sim.stale) ? (
              <Button variant="secondary" size="sm" onPress={refresh}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── details ── */}
        <Group title={t("sim.simInfo", "SIM Info")} stale={sim.stale}>
          <Row label={t("sim.status", "Status")} value={info ? info.sim_states || "—" : <span className="nd-skel" />} mono />
          <Row label={t("sim.modemState", "Modem State")} value={info ? info.modem_main_state || "—" : <span className="nd-skel" />} mono />
          <Row
            label={t("sim.pinLock", "PIN Lock")}
            value={
              !info ? (
                <span className="nd-skel" />
              ) : info.pin_status === "1" ? (
                t("common.enabled", "Enabled")
              ) : info.pin_status === "0" ? (
                t("common.disabled", "Disabled")
              ) : (
                info.pin_status || "—"
              )
            }
          />
          <Row label="IMSI" value={info ? info.sim_imsi || "—" : <span className="nd-skel" />} mono />
          <Row label="ICCID" value={info ? info.sim_iccid || "—" : <span className="nd-skel" />} mono />
          {info?.pinnumber && <Row label={t("sim.pinAttempts", "PIN attempts remaining")} value={info.pinnumber} mono />}
          {info?.puknumber && <Row label={t("sim.pukAttempts", "PUK attempts remaining")} value={info.puknumber} mono />}
          <Row
            label={t("sim.nckTrials", "Network unlock (NCK) attempts left")}
            value={trials.data ? trials.data.available_trials || "—" : trials.error ? t("sim.unreadableShort", "Can't read") : <span className="nd-skel" />}
            mono
          />
        </Group>
        {trials.error && info && (
          <p className="nd-aux -mt-4 px-1">
            {t("sim.trialsErr", "NCK attempts couldn't be read: {{e}}", { e: trials.error.message })}
          </p>
        )}

        {/* ── actions ── */}
        <section>
          <GroupTitle>{t("sim.actions", "Actions")}</GroupTitle>
          <div className="flex flex-wrap gap-2">
            {pinLocked && (
              <Button onPress={() => openPanel("verify-pin")} isDisabled={locked} aria-expanded={panel === "verify-pin"}>
                <LockKeyOpen size={20} weight="bold" aria-hidden />
                {t("sim.verifyPin", "Verify PIN")}
              </Button>
            )}
            {pukLocked && (
              <Button onPress={() => openPanel("verify-puk")} isDisabled={locked} aria-expanded={panel === "verify-puk"}>
                <LockKeyOpen size={20} weight="bold" aria-hidden />
                {t("sim.verifyPuk", "Verify PUK")}
              </Button>
            )}
            {!pinLocked && !pukLocked && (
              <>
                <Button variant="secondary" onPress={() => openPanel("pin-mode")} isDisabled={locked} aria-expanded={panel === "pin-mode"}>
                  <LockKey size={20} weight="bold" aria-hidden />
                  {pinEnabled ? t("sim.disablePinLock", "Disable PIN Lock") : t("sim.enablePinLock", "Enable PIN Lock")}
                </Button>
                <Button variant="secondary" onPress={() => openPanel("change-pin")} isDisabled={locked} aria-expanded={panel === "change-pin"}>
                  <Password size={20} weight="bold" aria-hidden />
                  {t("sim.changePin", "Change PIN")}
                </Button>
              </>
            )}
            <Button variant="secondary" onPress={() => openPanel("nck")} isDisabled={locked} aria-expanded={panel === "nck"}>
              <ShieldWarning size={20} weight="bold" aria-hidden />
              {t("sim.networkUnlock", "Network Unlock")}
            </Button>
          </div>

          {panel && (
            <section aria-labelledby="sim-panel-title" className="nd-group mt-4 grid gap-4 p-4 lg:p-5">
              <h3 id="sim-panel-title" className="font-semibold">
                {panel === "verify-pin"
                  ? t("sim.verifyPin", "Verify PIN")
                  : panel === "verify-puk"
                    ? t("sim.verifyPuk", "Verify PUK")
                    : panel === "change-pin"
                      ? t("sim.changePin", "Change PIN")
                      : panel === "pin-mode"
                        ? pinEnabled
                          ? t("sim.disablePinTitle", "Disable PIN Lock")
                          : t("sim.enablePinTitle", "Enable PIN Lock")
                        : t("sim.nckTitle", "Network Unlock (NCK)")}
              </h3>
              {panel === "verify-pin" && pinField("pin", t("sim.pin", "PIN"), pinInput, setPinInput, t("sim.digits48", "4-8 digits"))}
              {panel === "verify-puk" && (
                <>
                  {pinField("puk", t("sim.puk", "PUK"), pukInput, setPukInput, t("sim.digits8", "8+ digits"))}
                  {pinField("newpin", t("sim.newPin", "New PIN"), newPin, setNewPin, t("sim.digits48", "4-8 digits"))}
                </>
              )}
              {panel === "change-pin" && (
                <>
                  {pinField("oldpin", t("sim.currentPin", "Current PIN"), oldPin, setOldPin, t("sim.digits48", "4-8 digits"))}
                  {pinField("newpin", t("sim.newPin", "New PIN"), newPin, setNewPin, t("sim.digits48", "4-8 digits"))}
                </>
              )}
              {panel === "pin-mode" && pinField("pinm", t("sim.confirmPin", "Confirm current PIN"), pinInput, setPinInput, t("sim.digits48", "4-8 digits"))}
              {panel === "nck" && (
                <>
                  <p>
                    <StatusMark tone="warn">{t("sim.nckWarn", "This is irreversible. Only proceed if you have a valid unlock code.")}</StatusMark>
                  </p>
                  <div className="grid gap-1">
                    <label htmlFor="sim-nck" className="font-semibold">
                      {t("sim.nckLabel", "Unlock code (NCK)")}
                    </label>
                    <input
                      id="sim-nck"
                      className="nd-field nd-mono"
                      value={nckInput}
                      autoComplete="off"
                      spellCheck={false}
                      disabled={op.busy}
                      placeholder={t("sim.nckPlaceholder", "Unlock code")}
                      onChange={(e) => setNckInput(e.target.value)}
                    />
                  </div>
                </>
              )}
              {formErr && (
                <p role="alert">
                  <StatusMark tone="bad">{formErr}</StatusMark>
                </p>
              )}
              <div className="flex flex-wrap gap-2">
                <Button variant={panel === "nck" || panel === "verify-puk" ? "danger" : "primary"} onPress={() => submit(panel)} isDisabled={locked} pending={op.busy}>
                  {panel === "change-pin"
                    ? t("sim.changePin", "Change PIN")
                    : panel === "pin-mode"
                      ? t("common.confirm", "Confirm")
                      : panel === "nck"
                        ? t("sim.unlockSim", "Unlock SIM")
                        : t("sim.submit", "Submit")}
                </Button>
                <Button variant="secondary" onPress={() => setPanel(null)} isDisabled={op.busy}>
                  {t("common.cancel", "Cancel")}
                </Button>
              </div>
            </section>
          )}

          <div className="mt-3 grid gap-2 px-1">
            <OpResult op={op} />
            {(op.phase === "failed" || op.phase === "unknown") && (pinLeft !== null || pukLeft !== null) && (
              <p className="nd-aux">
                {[
                  pinLeft !== null && t("sim.pinLeft", "{{n}} PIN attempt(s) left", { n: pinLeft }),
                  pukLeft !== null && t("sim.pukLeft", "{{n}} PUK attempt(s) left", { n: pukLeft }),
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </p>
            )}
          </div>
        </section>
      </div>

      <SimDialog
        action={action}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        onConfirm={confirmAction}
        pinLeft={pinLeft}
        pukLeft={pukLeft}
        nckLeft={nckLeft}
      />
    </>
  );
}

function PinField({
  id,
  label,
  value,
  onChange,
  hint,
  disabled,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (v: string) => void;
  hint?: string;
  disabled?: boolean;
}) {
  const uid = useId();
  const fid = `${uid}-${id}`;
  return (
    <div className="grid gap-1">
      <label htmlFor={fid} className="font-semibold">
        {label}
      </label>
      <input
        id={fid}
        className="nd-field nd-mono"
        type="password"
        inputMode="numeric"
        pattern="[0-9]*"
        autoComplete="off"
        maxLength={20}
        value={value}
        disabled={disabled}
        aria-describedby={hint ? `${fid}-hint` : undefined}
        onChange={(e) => onChange(e.target.value.replace(/\D/g, ""))}
      />
      {hint && (
        <span id={`${fid}-hint`} className="nd-aux">
          {hint}
        </span>
      )}
    </div>
  );
}

function SimDialog({
  action,
  open,
  onOpenChange,
  onConfirm,
  pinLeft,
  pukLeft,
  nckLeft,
}: {
  action: Action | null;
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onConfirm: () => void;
  pinLeft: number | null;
  pukLeft: number | null;
  nckLeft: number | null;
}) {
  const { t } = useTranslation();
  if (!action) return null;
  const left = (n: number | null, key: string, def: string) =>
    n === null ? t("sim.leftUnknown", "The device didn't report how many attempts are left.") : t(key, def, { n });
  const pinTries = left(pinLeft, "sim.pinTriesLeft", "{{n}} PIN attempt(s) left.");

  let title = "";
  let what: ReactNode = "";
  let recovery: ReactNode;
  let actionLabel = "";
  let danger = false;
  let cutsUplink = false;
  switch (action.kind) {
    case "verify-pin":
      title = t("sim.dlgVerifyPinTitle", "Send this PIN to the SIM?");
      what = (
        <>
          {t("sim.dlgVerifyPinWhat", "A wrong PIN uses up one attempt; when none are left the SIM locks and needs the PUK.")} <strong>{pinTries}</strong>
        </>
      );
      recovery = t("sim.dlgPinRecovery", "If the PIN attempts run out, unlock with the PUK from your carrier.");
      actionLabel = t("sim.verifyPin", "Verify PIN");
      break;
    case "verify-puk":
      title = t("sim.dlgVerifyPukTitle", "Send this PUK and set a new PIN?");
      what = (
        <>
          {t("sim.dlgVerifyPukWhat", "A wrong PUK uses up one attempt. When none are left the SIM is locked for good and has to be replaced by the carrier.")}{" "}
          <strong>{left(pukLeft, "sim.pukTriesLeft", "{{n}} PUK attempt(s) left.")}</strong>
        </>
      );
      recovery = t("sim.dlgPukRecovery", "Check the PUK with your carrier before trying again.");
      actionLabel = t("sim.verifyPuk", "Verify PUK");
      danger = true;
      break;
    case "change-pin":
      title = t("sim.dlgChangeTitle", "Change the SIM PIN?");
      what = (
        <>
          {t("sim.dlgChangeWhat", "The SIM's PIN changes to the new one. A wrong current PIN uses up one attempt.")} <strong>{pinTries}</strong>
        </>
      );
      recovery = t("sim.dlgChangeRecovery", "Note the new PIN. If it's forgotten, the PUK resets it.");
      actionLabel = t("sim.changePin", "Change PIN");
      break;
    case "pin-mode":
      title = action.enable ? t("sim.dlgPinOnTitle", "Turn the PIN lock on?") : t("sim.dlgPinOffTitle", "Turn the PIN lock off?");
      what = (
        <>
          {action.enable
            ? t("sim.dlgPinOnWhat", "From the next start-up the SIM stays locked until someone enters the PIN: no mobile data, and no remote access over Tailscale, until then. A wrong PIN uses up one attempt.")
            : t("sim.dlgPinOffWhat", "The SIM no longer asks for a PIN at start-up. A wrong PIN uses up one attempt.")}{" "}
          <strong>{pinTries}</strong>
        </>
      );
      recovery = action.enable
        ? t("sim.dlgPinOnRecovery", "After a restart, join the U60's Wi-Fi and enter the PIN on this page, or turn the PIN lock off here first.")
        : undefined;
      actionLabel = action.enable ? t("sim.enablePinLock", "Enable PIN Lock") : t("sim.disablePinLock", "Disable PIN Lock");
      cutsUplink = action.enable;
      break;
    case "nck":
      title = t("sim.dlgNckTitle", "Send this network unlock code?");
      what = (
        <>
          {t("sim.dlgNckWhat", "This can't be undone. A wrong code uses up one attempt; when none are left the device can't be unlocked with a code any more.")}{" "}
          <strong>{left(nckLeft, "sim.nckTriesLeft", "{{n}} unlock attempt(s) left.")}</strong>
        </>
      );
      recovery = t("sim.dlgNckRecovery", "Double-check the code with whoever issued it before sending.");
      actionLabel = t("sim.unlockSim", "Unlock SIM");
      danger = true;
      break;
  }
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      what={what}
      recovery={recovery}
      actionLabel={actionLabel}
      danger={danger}
      cutsUplink={cutsUplink}
      onConfirm={onConfirm}
    />
  );
}
