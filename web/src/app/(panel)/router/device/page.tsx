"use client";

import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Toggle } from "@/components/admin/Button";
import { AlertTriangle, BatteryCharging, Zap, RotateCcw } from "lucide-react";

interface ChargeControl {
  charge_limit_enabled?: boolean;
  charge_limit?: number;
  hysteresis?: number;
  charging_stopped?: boolean;
  battery_status?: string;
  capacity?: number;
}

interface FastBoot {
  fast_boot?: string;
}

interface PowerSaveResp {
  power_saver_mode?: string;
}

function FieldRow({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5 border-b border-border/60 py-3 last:border-0 sm:flex-row sm:items-center sm:justify-between">
      <div>
        <div className="text-sm">{label}</div>
        {hint && <div className="text-xs text-text-dim">{hint}</div>}
      </div>
      <div className="sm:w-52">{children}</div>
    </div>
  );
}

function SliderField({ label, value, onChange, min, max }: { label: string; value: number; onChange: (v: number) => void; min: number; max: number }) {
  return (
    <div className="space-y-1">
      <div className="flex justify-between text-xs text-text-dim">
        <span>{label}</span>
        <span>{value}</span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        value={value}
        onChange={(e) => onChange(parseInt(e.target.value))}
        className="w-full accent-[var(--color-accent)]"
      />
      <div className="flex justify-between text-xs text-text-dim">
        <span>{min}</span>
        <span>{max}</span>
      </div>
    </div>
  );
}

export default function DevicePage() {
  const { t } = useTranslation();
  const { data: chargeData, error: chargeErr } = useApi<ChargeControl>("/api/device/charge-control");
  const { data: fastBootData, error: fastBootErr } = useApi<FastBoot>("/api/device/fast-boot");

  const [chargeLimitEnabled, setChargeLimitEnabled] = useState(false);
  const [chargeLimit, setChargeLimit] = useState(80);
  const [hysteresis, setHysteresis] = useState(5);
  const [fastBoot, setFastBoot] = useState(false);
  const [powerSave, setPowerSave] = useState(false);
  const [psLoaded, setPsLoaded] = useState(false);
  const [chargeLoaded, setChargeLoaded] = useState(false);
  const [fastBootLoaded, setFastBootLoaded] = useState(false);

  const [saving, setSaving] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const [confirmReboot, setConfirmReboot] = useState(false);
  const [confirmReset, setConfirmReset] = useState<"first" | "second" | null>(null);

  // Load charge control
  useEffect(() => {
    if (!chargeData) return;
    if (chargeData.charge_limit_enabled !== undefined) setChargeLimitEnabled(chargeData.charge_limit_enabled);
    if (chargeData.charge_limit !== undefined) setChargeLimit(chargeData.charge_limit);
    if (chargeData.hysteresis !== undefined) setHysteresis(chargeData.hysteresis);
    setChargeLoaded(true);
  }, [chargeData]);

  // Load fast boot
  useEffect(() => {
    if (!fastBootData) return;
    setFastBoot(fastBootData.fast_boot === "1");
    setFastBootLoaded(true);
  }, [fastBootData]);

  // Load power save
  useEffect(() => {
    let cancelled = false;
    apiFetch<PowerSaveResp>("/api/device/power-save", {
      method: "POST",
      body: { deviceInfoList: ["power_saver_mode"] },
    }).then((d) => {
      if (!cancelled && d?.power_saver_mode !== undefined) {
        setPowerSave(d.power_saver_mode === "1");
        setPsLoaded(true);
      }
    }).catch(() => {});
    return () => { cancelled = true; };
  }, []);

  function showMsg(text: string, err = false) {
    setMsg({ text, err });
    setTimeout(() => setMsg(null), 4000);
  }

  async function applyChargeLimit() {
    if (!chargeLoaded) return;
    setSaving("charge");
    try {
      await apiFetch("/api/device/charge-control", {
        method: "PUT",
        body: { charge_limit_enabled: chargeLimitEnabled, charge_limit: chargeLimit, hysteresis },
      });
      showMsg(chargeLimitEnabled ? t("devctl.chargeLimitSet", "Charge limit set to {{limit}}%", { limit: chargeLimit }) : t("devctl.chargeLimitDisabled", "Charge limit disabled"));
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : t("devctl.failed", "Failed"), true);
    } finally { setSaving(null); }
  }

  async function togglePowerSave(enabled: boolean) {
    if (!psLoaded) return;
    setPowerSave(enabled);
    try {
      await apiFetch("/api/device/power-save", {
        method: "PUT",
        body: { deviceInfoList: [{ power_saver_mode: enabled ? "1" : "0" }] },
      });
      showMsg(enabled ? t("devctl.powerSaveEnabled", "Power-save mode enabled") : t("devctl.powerSaveDisabled", "Power-save mode disabled"));
    } catch (e) {
      setPowerSave(!enabled);
      showMsg(e instanceof ApiError ? e.message : t("devctl.failed", "Failed"), true);
    }
  }

  async function toggleFastBoot(enabled: boolean) {
    if (!fastBootLoaded) return;
    setFastBoot(enabled);
    try {
      await apiFetch("/api/device/fast-boot", {
        method: "PUT",
        body: { fast_boot: enabled ? "1" : "0" },
      });
      showMsg(enabled ? t("devctl.fastBootEnabled", "Fast boot enabled") : t("devctl.fastBootDisabled", "Fast boot disabled"));
    } catch (e) {
      setFastBoot(!enabled);
      showMsg(e instanceof ApiError ? e.message : t("devctl.failed", "Failed"), true);
    }
  }

  async function doReboot() {
    setSaving("reboot");
    try {
      await apiFetch("/api/device/reboot", { method: "POST" });
      showMsg(t("devctl.rebooting", "Router is rebooting…"));
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : t("devctl.failed", "Failed"), true);
    } finally { setSaving(null); setConfirmReboot(false); }
  }

  async function doFactoryReset() {
    setSaving("reset");
    try {
      await apiFetch("/api/device/factory-reset", { method: "POST" });
      showMsg(t("devctl.factoryResetInitiated", "Factory reset initiated…"));
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : t("devctl.failed", "Failed"), true);
    } finally { setSaving(null); setConfirmReset(null); }
  }

  const anyErr = chargeErr || fastBootErr;

  return (
    <>
      <PageHeader title={t("devctl.title", "Device Control")} description={t("devctl.desc", "Charge limit, power modes, and reboot.")} />

      {anyErr && <ErrorBanner message={anyErr.message} />}
      {msg && (
        <div className={`mb-4 rounded-md border px-3 py-2 text-sm ${msg.err ? "border-error/40 bg-error/10 text-error" : "border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        {/* Charge Limit */}
        <SectionCard title={t("devctl.chargeLimitTitle", "Charge Limit")}>
          {chargeData?.charging_stopped && (
            <div className="mb-3 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-xs text-warning">
              {t("devctl.chargingStopped", "Charging is currently stopped (hardware-enforced).")}
            </div>
          )}
          <FieldRow label={t("devctl.enableChargeLimit", "Enable charge limit")}>
            <Toggle checked={chargeLimitEnabled} onChange={setChargeLimitEnabled} label={chargeLimitEnabled ? t("devctl.enabled", "Enabled") : t("devctl.disabled", "Disabled")} />
          </FieldRow>
          {chargeLimitEnabled && (
            <>
              <div className="py-3">
                <SliderField label={t("devctl.chargeLimitPct", "Charge limit %")} value={chargeLimit} onChange={setChargeLimit} min={50} max={100} />
              </div>
              <div className="py-3">
                <SliderField label={t("devctl.hysteresisPct", "Hysteresis %")} value={hysteresis} onChange={setHysteresis} min={1} max={10} />
              </div>
            </>
          )}
          <div className="mt-2 flex justify-end">
            <Button onClick={applyChargeLimit} loading={saving === "charge"} disabled={!chargeLoaded}>
              <BatteryCharging size={14} /> {t("devctl.apply", "Apply")}
            </Button>
          </div>
          {chargeData?.capacity !== undefined && (
            <p className="mt-2 text-xs text-text-dim">{t("devctl.currentCapacity", "Current capacity:")} {chargeData.capacity}%{chargeData.battery_status ? ` — ${chargeData.battery_status}` : ""}</p>
          )}
        </SectionCard>

        {/* Power Modes */}
        <SectionCard title={t("devctl.powerModesTitle", "Power Modes")}>
          <FieldRow label={t("devctl.powerSaveMode", "Power-save mode")}>
            <Toggle checked={powerSave} onChange={togglePowerSave} label={powerSave ? t("devctl.on", "On") : t("devctl.off", "Off")} disabled={!psLoaded} />
          </FieldRow>
          <FieldRow label={t("devctl.fastBoot", "Fast boot")} hint={t("devctl.fastBootHint", "Skips some init steps on reboot")}>
            <Toggle checked={fastBoot} onChange={toggleFastBoot} label={fastBoot ? t("devctl.on", "On") : t("devctl.off", "Off")} disabled={!fastBootLoaded} />
          </FieldRow>
        </SectionCard>

        {/* Reboot */}
        <SectionCard title={t("devctl.rebootTitle", "Reboot")}>
          {confirmReboot ? (
            <div className="space-y-3">
              <p className="text-sm text-warning">{t("devctl.rebootWarning", "Router will reboot and be temporarily unreachable.")}</p>
              <div className="flex gap-2">
                <Button onClick={doReboot} loading={saving === "reboot"}>
                  <RotateCcw size={14} /> {t("devctl.confirmReboot", "Confirm Reboot")}
                </Button>
                <Button variant="ghost" onClick={() => setConfirmReboot(false)}>{t("devctl.cancel", "Cancel")}</Button>
              </div>
            </div>
          ) : (
            <Button variant="outline" onClick={() => setConfirmReboot(true)}>
              <RotateCcw size={14} /> {t("devctl.rebootDevice", "Reboot Device")}
            </Button>
          )}
        </SectionCard>

        {/* Factory Reset */}
        <SectionCard title={t("devctl.factoryResetTitle", "Factory Reset")}>
          {confirmReset === null && (
            <Button variant="danger" onClick={() => setConfirmReset("first")}>
              <AlertTriangle size={14} /> {t("devctl.factoryReset", "Factory Reset")}
            </Button>
          )}
          {confirmReset === "first" && (
            <div className="space-y-3">
              <p className="text-sm text-error">{t("devctl.resetConfirm1", "This will erase all settings. Are you sure?")}</p>
              <div className="flex gap-2">
                <Button variant="danger" onClick={() => setConfirmReset("second")}>{t("devctl.yesImSure", "Yes, I'm sure")}</Button>
                <Button variant="ghost" onClick={() => setConfirmReset(null)}>{t("devctl.cancel", "Cancel")}</Button>
              </div>
            </div>
          )}
          {confirmReset === "second" && (
            <div className="space-y-3">
              <p className="text-sm text-error font-medium">{t("devctl.resetConfirm2", "Final confirmation — this cannot be undone.")}</p>
              <div className="flex gap-2">
                <Button variant="danger" onClick={doFactoryReset} loading={saving === "reset"}>
                  {t("devctl.resetNow", "Reset Now")}
                </Button>
                <Button variant="ghost" onClick={() => setConfirmReset(null)}>{t("devctl.cancel", "Cancel")}</Button>
              </div>
            </div>
          )}
        </SectionCard>
      </div>
    </>
  );
}
