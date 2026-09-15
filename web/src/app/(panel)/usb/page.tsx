"use client";

import { useState } from "react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, StatCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Toggle } from "@/components/admin/Button";
import { useSWRConfig } from "swr";
import { useTranslation } from "react-i18next";

interface UsbStatus {
  connect?: number;
  mode?: string;
  typec_cc?: string;
  usb2rj45?: number;
}

interface ChargerInfo {
  charger_type?: number;
  charge_status?: number;
  charger_connect?: number;
  direct_power_supply_mode?: string;
  otg_powerbank_state?: number;
}

const USB_MODES = ["debug", "mtp", "rndis"] as const;
type UsbMode = (typeof USB_MODES)[number];

function isTruthy(v?: string | number | boolean): boolean {
  if (v === undefined || v === null) return false;
  if (typeof v === "boolean") return v;
  if (typeof v === "number") return v !== 0;
  return v === "1" || v === "true" || v === "yes";
}

export default function UsbPage() {
  const { t } = useTranslation();
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  const { data: usb, isLoading } = useApi<UsbStatus>("/api/usb/status", {
    refreshInterval: 3000,
  });
  const { data: charger } = useApi<ChargerInfo>("/api/device/charger", {
    refreshInterval: 5000,
  });

  const cableAttached = isTruthy(usb?.connect);
  const powerbankActive = isTruthy(charger?.otg_powerbank_state);

  async function togglePowerbank(enabled: boolean) {
    setBusy(true);
    setErr(null);
    setMsg(null);
    try {
      await apiFetch("/api/usb/powerbank", {
        method: "PUT",
        body: { state: enabled ? 1 : 0 },
      });
      setMsg(enabled ? t("usb.powerbankEnabled", "Powerbank mode enabled") : t("usb.powerbankDisabled", "Powerbank mode disabled"));
      mutate("/api/usb/status");
      mutate("/api/device/charger");
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function changeMode(mode: UsbMode) {
    if (!confirm(t("usb.confirmChangeMode", 'Change USB mode to "{{mode}}"? This will disconnect the current session.', { mode }))) return;
    setBusy(true);
    setErr(null);
    setMsg(null);
    try {
      await apiFetch("/api/usb/mode", {
        method: "PUT",
        body: { mode },
      });
      setMsg(t("usb.modeChanged", "USB mode changed to {{mode}}", { mode }));
      mutate("/api/usb/status");
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <PageHeader title="USB" description={t("usb.desc", "USB connection status and mode control.")} />

      <div className="mb-4 grid grid-cols-2 gap-3 sm:grid-cols-4">
        <StatCard label={t("usb.cable", "Cable")} value={isLoading ? "…" : cableAttached ? t("usb.attached", "Attached") : t("usb.detached", "Detached")} />
        <StatCard label={t("usb.chargerType", "Charger Type")} value={charger?.charger_type != null ? String(charger.charger_type) : "—"} />
        <StatCard label={t("usb.usbCCc", "USB-C CC")} value={usb?.typec_cc ?? "—"} />
        <StatCard label={t("usb.mode", "Mode")} value={usb?.mode ?? "—"} />
      </div>

      {err && <div className="mb-3"><ErrorBanner message={err} /></div>}
      {msg && (
        <div className="mb-3 rounded-md border border-success/40 bg-success/10 px-3 py-2 text-sm text-success">
          {msg}
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        <SectionCard title={t("usb.powerbankMode", "Powerbank Mode")}>
          <p className="mb-4 text-sm text-text-dim">
            {t("usb.powerbankHint", "When enabled, the device acts as a USB power bank and charges connected devices.")}
          </p>
          {charger?.direct_power_supply_mode && (
            <div className="mb-3 text-sm">
              <span className="text-text-dim">{t("usb.directPower", "Direct power:")} </span>
              <span className="font-mono">{charger.direct_power_supply_mode}</span>
            </div>
          )}
          <Toggle
            checked={powerbankActive}
            onChange={togglePowerbank}
            disabled={busy}
            label={powerbankActive ? t("usb.powerbankOn", "Powerbank On") : t("usb.powerbankOff", "Powerbank Off")}
          />
        </SectionCard>

        <SectionCard title={t("usb.usbMode", "USB Mode")}>
          <p className="mb-4 text-sm text-text-dim">
            {t("usb.usbModeHint", "Select the USB connection mode. Changing mode will reconnect the USB interface.")}
          </p>
          <div className="flex flex-wrap gap-2">
            {USB_MODES.map((mode) => (
              <Button
                key={mode}
                variant={usb?.mode === mode ? "primary" : "outline"}
                size="sm"
                loading={busy}
                onClick={() => changeMode(mode)}
              >
                {mode.toUpperCase()}
              </Button>
            ))}
          </div>
        </SectionCard>
      </div>
    </>
  );
}
