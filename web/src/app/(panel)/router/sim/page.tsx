"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input } from "@/components/admin/Button";
import { useSWRConfig } from "swr";
import { ShieldAlert, RefreshCw } from "lucide-react";

interface SIMInfo {
  sim_states?: string;
  modem_main_state?: string;
  pin_status?: string;
  sim_imsi?: string;
  sim_iccid?: string;
  pinnumber?: string;
  puknumber?: string;
}

interface LockTrials {
  available_trials?: string;
  ret?: number;
}

function Row({ k, v }: { k: string; v?: string }) {
  return (
    <div className="flex justify-between border-b border-border/60 py-1.5 text-sm last:border-0">
      <span className="text-text-dim">{k}</span>
      <span className="font-mono">{v ?? "—"}</span>
    </div>
  );
}

function PinInput({ label, value, onChange, placeholder, hint }: { label: string; value: string; onChange: (v: string) => void; placeholder?: string; hint?: string }) {
  return (
    <div>
      <label className="mb-1 block text-xs text-text-dim">{label}</label>
      <Input
        type="password"
        inputMode="numeric"
        pattern="[0-9]*"
        value={value}
        onChange={(e) => onChange(e.target.value.replace(/\D/g, ""))}
        placeholder={placeholder}
        maxLength={20}
      />
      {hint && <div className="mt-0.5 text-xs text-text-dim">{hint}</div>}
    </div>
  );
}

type ActionPanel = "verify-pin" | "verify-puk" | "change-pin" | "pin-mode" | "nck" | null;

export default function SIMPage() {
  const { t } = useTranslation();
  const { data: simInfo, error: simErr, mutate: mutateSim } = useApi<SIMInfo>("/api/sim/info");
  const { data: lockInfo, error: lockErr } = useApi<LockTrials>("/api/sim/lock-trials");
  const { mutate: globalMutate } = useSWRConfig();

  const [panel, setPanel] = useState<ActionPanel>(null);
  const [pinInput, setPinInput] = useState("");
  const [oldPin, setOldPin] = useState("");
  const [newPin, setNewPin] = useState("");
  const [pukInput, setPukInput] = useState("");
  const [nckInput, setNckInput] = useState("");
  const [pinModeEnable, setPinModeEnable] = useState(true);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);

  function showMsg(text: string, err = false) {
    setMsg({ text, err });
    setTimeout(() => setMsg(null), 5000);
  }

  async function refresh() {
    await mutateSim();
    globalMutate("/api/sim/info");
    globalMutate("/api/sim/lock-trials");
  }

  function openPanel(p: ActionPanel) {
    setPanel(p);
    setPinInput(""); setOldPin(""); setNewPin(""); setPukInput(""); setNckInput("");
    setMsg(null);
  }

  async function doVerifyPin() {
    if (pinInput.length < 4) return showMsg(t("sim.msgPinMin", "PIN must be at least 4 digits"), true);
    setSaving(true);
    try {
      await apiFetch("/api/sim/pin/verify", {
        method: "POST",
        body: { pin_num: pinInput, puk_num: "", pin_encode_flag: "0" },
      });
      showMsg(t("sim.msgPinVerified", "PIN verified"));
      setPanel(null);
      await refresh();
    } catch (e) { showMsg(e instanceof ApiError ? e.message : t("sim.failed", "Failed"), true); }
    finally { setSaving(false); }
  }

  async function doVerifyPuk() {
    if (pukInput.length < 8) return showMsg(t("sim.msgPukMin", "PUK must be at least 8 digits"), true);
    if (newPin.length < 4) return showMsg(t("sim.msgNewPinMin", "New PIN must be at least 4 digits"), true);
    setSaving(true);
    try {
      await apiFetch("/api/sim/pin/verify", {
        method: "POST",
        body: { pin_num: newPin, puk_num: pukInput, pin_encode_flag: "0" },
      });
      showMsg(t("sim.msgPukVerified", "PUK verified, new PIN set"));
      setPanel(null);
      await refresh();
    } catch (e) { showMsg(e instanceof ApiError ? e.message : t("sim.failed", "Failed"), true); }
    finally { setSaving(false); }
  }

  async function doChangePin() {
    if (oldPin.length < 4 || newPin.length < 4) return showMsg(t("sim.msgPinMin", "PIN must be at least 4 digits"), true);
    setSaving(true);
    try {
      await apiFetch("/api/sim/pin/change", {
        method: "POST",
        body: { pin_num: oldPin, new_pin_num: newPin, pin_encode_flag: "0" },
      });
      showMsg(t("sim.msgPinChanged", "PIN changed successfully"));
      setPanel(null);
    } catch (e) { showMsg(e instanceof ApiError ? e.message : t("sim.failed", "Failed"), true); }
    finally { setSaving(false); }
  }

  async function doPinMode() {
    if (pinInput.length < 4) return showMsg(t("sim.msgPinMin", "PIN must be at least 4 digits"), true);
    setSaving(true);
    try {
      await apiFetch("/api/sim/pin/mode", {
        method: "POST",
        body: { pin_num_m: pinInput, pin_mode: pinModeEnable ? "1" : "0", pin_encode_flag: "0" },
      });
      showMsg(pinModeEnable ? t("sim.msgPinLockEnabled", "PIN lock enabled") : t("sim.msgPinLockDisabled", "PIN lock disabled"));
      setPanel(null);
      await refresh();
    } catch (e) { showMsg(e instanceof ApiError ? e.message : t("sim.failed", "Failed"), true); }
    finally { setSaving(false); }
  }

  async function doNck() {
    if (!nckInput.trim()) return showMsg(t("sim.msgNckRequired", "Unlock code is required"), true);
    setSaving(true);
    try {
      await apiFetch("/api/sim/unlock", {
        method: "POST",
        body: { nck: nckInput },
      });
      showMsg(t("sim.msgSimUnlocked", "SIM unlocked successfully"));
      setPanel(null);
      await refresh();
    } catch (e) { showMsg(e instanceof ApiError ? e.message : t("sim.failed", "Failed"), true); }
    finally { setSaving(false); }
  }

  const pinEnabled = simInfo?.pin_status === "1";
  const pinLocked = ["wait pin", "modem_waitpin"].includes((simInfo?.sim_states ?? "").toLowerCase()) ||
    ["wait pin", "modem_waitpin"].includes((simInfo?.modem_main_state ?? "").toLowerCase());
  const pukLocked = ["wait puk", "modem_waitpuk"].includes((simInfo?.sim_states ?? "").toLowerCase()) ||
    ["wait puk", "modem_waitpuk"].includes((simInfo?.modem_main_state ?? "").toLowerCase());

  return (
    <>
      <PageHeader
        title="SIM / PIN"
        description={t("sim.desc", "SIM status and PIN management.")}
        actions={
          <Button variant="ghost" size="sm" onClick={refresh}>
            <RefreshCw size={13} /> {t("common.refresh", "Refresh")}
          </Button>
        }
      />

      {(simErr || lockErr) && <ErrorBanner message={(simErr ?? lockErr)!.message} />}
      {msg && (
        <div className={`mb-4 rounded-md border px-3 py-2 text-sm ${msg.err ? "border-error/40 bg-error/10 text-error" : "border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      {(pinLocked || pukLocked) && (
        <div className="mb-4 flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <ShieldAlert size={15} />
          {pukLocked ? t("sim.pukLocked", "SIM is PUK-locked. Enter PUK to unlock.") : t("sim.pinLocked", "SIM is PIN-locked. Enter PIN to unlock.")}
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        <SectionCard title={t("sim.simInfo", "SIM Info")}>
          <Row k={t("sim.status", "Status")} v={simInfo?.sim_states} />
          <Row k={t("sim.modemState", "Modem State")} v={simInfo?.modem_main_state} />
          <Row k={t("sim.pinLock", "PIN Lock")} v={simInfo?.pin_status === "1" ? t("common.enabled", "Enabled") : simInfo?.pin_status === "0" ? t("common.disabled", "Disabled") : simInfo?.pin_status} />
          <Row k="IMSI" v={simInfo?.sim_imsi} />
          <Row k="ICCID" v={simInfo?.sim_iccid} />
          {lockInfo && (
            <Row k={t("sim.availTrials", "Available trials")} v={lockInfo.available_trials} />
          )}
          {simInfo && (
            <>
              {simInfo.pinnumber && <Row k={t("sim.pinAttempts", "PIN attempts remaining")} v={simInfo.pinnumber} />}
              {simInfo.puknumber && <Row k={t("sim.pukAttempts", "PUK attempts remaining")} v={simInfo.puknumber} />}
            </>
          )}
        </SectionCard>

        <SectionCard title={t("sim.actions", "Actions")}>
          <div className="flex flex-wrap gap-2">
            {pinLocked && (
              <Button size="sm" onClick={() => openPanel("verify-pin")}>{t("sim.verifyPin", "Verify PIN")}</Button>
            )}
            {pukLocked && (
              <Button size="sm" onClick={() => openPanel("verify-puk")}>{t("sim.verifyPuk", "Verify PUK")}</Button>
            )}
            {!pinLocked && !pukLocked && (
              <>
                <Button size="sm" variant="outline" onClick={() => { setPinModeEnable(!pinEnabled); openPanel("pin-mode"); }}>
                  {pinEnabled ? t("sim.disablePinLock", "Disable PIN Lock") : t("sim.enablePinLock", "Enable PIN Lock")}
                </Button>
                <Button size="sm" variant="outline" onClick={() => openPanel("change-pin")}>{t("sim.changePin", "Change PIN")}</Button>
              </>
            )}
            <Button size="sm" variant="outline" onClick={() => openPanel("nck")}>
              <ShieldAlert size={13} /> {t("sim.networkUnlock", "Network Unlock")}
            </Button>
          </div>

          {panel === "verify-pin" && (
            <div className="mt-4 space-y-3 border-t border-border/60 pt-4">
              <p className="text-sm font-medium">{t("sim.verifyPin", "Verify PIN")}</p>
              <PinInput label={t("sim.pin", "PIN")} value={pinInput} onChange={setPinInput} placeholder={t("sim.digits48", "4-8 digits")} hint={t("sim.digits48", "4-8 digits")} />
              <div className="flex gap-2">
                <Button onClick={doVerifyPin} loading={saving}>{t("sim.submit", "Submit")}</Button>
                <Button variant="ghost" onClick={() => setPanel(null)}>{t("common.cancel", "Cancel")}</Button>
              </div>
            </div>
          )}

          {panel === "verify-puk" && (
            <div className="mt-4 space-y-3 border-t border-border/60 pt-4">
              <p className="text-sm font-medium">{t("sim.verifyPuk", "Verify PUK")}</p>
              <PinInput label={t("sim.puk", "PUK")} value={pukInput} onChange={setPukInput} placeholder={t("sim.digits8", "8+ digits")} hint={t("sim.digits8", "8+ digits")} />
              <PinInput label={t("sim.newPin", "New PIN")} value={newPin} onChange={setNewPin} placeholder={t("sim.digits48", "4-8 digits")} />
              <div className="flex gap-2">
                <Button onClick={doVerifyPuk} loading={saving}>{t("sim.submit", "Submit")}</Button>
                <Button variant="ghost" onClick={() => setPanel(null)}>{t("common.cancel", "Cancel")}</Button>
              </div>
            </div>
          )}

          {panel === "change-pin" && (
            <div className="mt-4 space-y-3 border-t border-border/60 pt-4">
              <p className="text-sm font-medium">{t("sim.changePin", "Change PIN")}</p>
              <PinInput label={t("sim.currentPin", "Current PIN")} value={oldPin} onChange={setOldPin} placeholder={t("sim.digits48", "4-8 digits")} />
              <PinInput label={t("sim.newPin", "New PIN")} value={newPin} onChange={setNewPin} placeholder={t("sim.digits48", "4-8 digits")} />
              <div className="flex gap-2">
                <Button onClick={doChangePin} loading={saving}>{t("sim.changePin", "Change PIN")}</Button>
                <Button variant="ghost" onClick={() => setPanel(null)}>{t("common.cancel", "Cancel")}</Button>
              </div>
            </div>
          )}

          {panel === "pin-mode" && (
            <div className="mt-4 space-y-3 border-t border-border/60 pt-4">
              <p className="text-sm font-medium">{pinModeEnable ? t("sim.enablePinTitle", "Enable PIN Lock") : t("sim.disablePinTitle", "Disable PIN Lock")}</p>
              <PinInput label={t("sim.confirmPin", "Confirm current PIN")} value={pinInput} onChange={setPinInput} placeholder={t("sim.digits48", "4-8 digits")} />
              <div className="flex gap-2">
                <Button onClick={doPinMode} loading={saving}>{t("common.confirm", "Confirm")}</Button>
                <Button variant="ghost" onClick={() => setPanel(null)}>{t("common.cancel", "Cancel")}</Button>
              </div>
            </div>
          )}

          {panel === "nck" && (
            <div className="mt-4 space-y-3 border-t border-border/60 pt-4">
              <p className="text-sm font-medium">{t("sim.nckTitle", "Network Unlock (NCK)")}</p>
              <div className="rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-xs text-warning">
                {t("sim.nckWarn", "This is irreversible. Only proceed if you have a valid unlock code.")}
              </div>
              <div>
                <label className="mb-1 block text-xs text-text-dim">{t("sim.nckLabel", "Unlock code (NCK)")}</label>
                <Input value={nckInput} onChange={(e) => setNckInput(e.target.value)} placeholder={t("sim.nckPlaceholder", "Unlock code")} />
              </div>
              <div className="flex gap-2">
                <Button variant="danger" onClick={doNck} loading={saving}>{t("sim.unlockSim", "Unlock SIM")}</Button>
                <Button variant="ghost" onClick={() => setPanel(null)}>{t("common.cancel", "Cancel")}</Button>
              </div>
            </div>
          )}
        </SectionCard>
      </div>
    </>
  );
}
