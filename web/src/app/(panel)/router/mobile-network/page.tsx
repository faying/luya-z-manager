"use client";

import { useState, useCallback } from "react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Toggle } from "@/components/admin/Button";
import { useSWRConfig } from "swr";
import { Radio, AlertTriangle } from "lucide-react";
import { useTranslation } from "react-i18next";

interface ModemData {
  cid?: number;
  connect_mode?: number;
  roam_enable?: number;
  enable?: number;
  connect_status?: string;
  roll_connect_status?: string;
}

interface ModemStatus {
  operate_mode?: string;
}

interface ScanOperator {
  m_mcc_mnc?: string;
  m_oper_name?: string;
  m_rat?: string;
  m_status?: string;
}

interface ScanStatus {
  status?: string;
}

interface ScanResults {
  operators?: ScanOperator[];
}

export default function MobileNetworkPage() {
  const { t } = useTranslation();
  const { mutate } = useSWRConfig();

  const { data: modemData, error: dataErr } = useApi<ModemData>("/api/modem/data", {
    refreshInterval: 5000,
  });
  const { data: modemStatus } = useApi<ModemStatus>("/api/modem/status", {
    refreshInterval: 5000,
  });

  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsError, setMsgIsError] = useState(false);

  // Derived state
  const airplaneOn = modemStatus?.operate_mode !== undefined && modemStatus.operate_mode !== "ONLINE";
  const dataEnabled = !!(modemData?.enable);
  const roamEnabled = !!(modemData?.roam_enable);
  const connectStatus = modemData?.connect_status ?? "";

  // Carrier scan state
  const [isScanning, setIsScanning] = useState(false);
  const [operators, setOperators] = useState<ScanOperator[]>([]);
  const [registering, setRegistering] = useState<string | null>(null);
  const [registerResult, setRegisterResult] = useState<string | null>(null);

  const showMsg = useCallback((text: string, isError: boolean) => {
    setMsg(text);
    setMsgIsError(isError);
  }, []);

  // Mobile data toggle
  const handleDataToggle = async (enabled: boolean) => {
    setSaving(true);
    try {
      const body: Record<string, unknown> = {
        cid: 1,
        connect_mode: enabled ? 1 : (modemData?.connect_mode ?? 1),
        roam_enable: modemData?.roam_enable ?? 0,
        enable: enabled ? 1 : 0,
      };
      if (!enabled) body.connect_status = "disconnected";
      await apiFetch("/api/modem/data", { method: "PUT", body });
      await mutate("/api/modem/data");
      showMsg(
        enabled
          ? t("mobilenet.mobileDataEnabled", "Mobile data enabled")
          : t("mobilenet.mobileDataDisabled", "Mobile data disabled"),
        false
      );
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setSaving(false);
    }
  };

  // Roaming toggle
  const handleRoamToggle = async (enabled: boolean) => {
    setSaving(true);
    try {
      await apiFetch("/api/modem/data", {
        method: "PUT",
        body: {
          cid: 1,
          connect_mode: modemData?.connect_mode ?? 1,
          roam_enable: enabled ? 1 : 0,
          enable: modemData?.enable ?? 0,
        },
      });
      await mutate("/api/modem/data");
      showMsg(
        enabled
          ? t("mobilenet.roamingEnabled", "Roaming enabled")
          : t("mobilenet.roamingDisabled", "Roaming disabled"),
        false
      );
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setSaving(false);
    }
  };

  // Airplane mode toggle
  const handleAirplaneToggle = async (enabled: boolean) => {
    if (enabled) {
      if (!window.confirm(t("mobilenet.confirmAirplane", "Enable airplane mode? This will turn off the cellular radio."))) return;
    }
    setSaving(true);
    try {
      if (enabled) {
        await apiFetch("/api/modem/airplane", { method: "POST", body: { operate_mode: "LPM" } });
        showMsg(t("mobilenet.airplaneEnabled", "Airplane mode enabled — cellular radio off"), false);
      } else {
        try {
          await apiFetch("/api/modem/online", { method: "POST" });
        } catch {
          // retry once
          await new Promise((r) => setTimeout(r, 3000));
          await apiFetch("/api/modem/online", { method: "POST" });
        }
        showMsg(
          t("mobilenet.airplaneDisabled", "Airplane mode disabled — If modem doesn't recover, reboot via Device Control."),
          false
        );
      }
      await mutate("/api/modem/status");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setSaving(false);
    }
  };

  // Carrier scan
  const handleScan = async () => {
    setIsScanning(true);
    setOperators([]);
    setRegisterResult(null);
    try {
      await apiFetch("/api/modem/scan", { method: "POST" });
      for (let i = 0; i < 30; i++) {
        await new Promise((r) => setTimeout(r, 2000));
        const statusData = await apiFetch<ScanStatus>("/api/modem/scan/status");
        const st = statusData.status ?? "";
        if (st === "done" || st === "complete" || st === "2") {
          const resultsData = await apiFetch<ScanResults>("/api/modem/scan/results");
          setOperators(resultsData.operators ?? []);
          setIsScanning(false);
          return;
        }
      }
      showMsg(t("mobilenet.scanTimedOut", "Carrier scan timed out"), true);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setIsScanning(false);
    }
  };

  // Register to a carrier
  const handleRegister = async (op: ScanOperator) => {
    const key = op.m_mcc_mnc ?? "";
    setRegistering(key);
    setRegisterResult(null);
    try {
      await apiFetch("/api/modem/register", {
        method: "POST",
        body: { m_mcc_mnc: op.m_mcc_mnc, m_rat: op.m_rat },
      });
      for (let i = 0; i < 15; i++) {
        await new Promise((r) => setTimeout(r, 2000));
        const res = await apiFetch<{ result?: string }>("/api/modem/register/result");
        const result = res.result ?? "";
        if (result === "success" || result === "1") {
          setRegisterResult(t("mobilenet.registeredTo", "Registered to {{name}}", { name: op.m_oper_name ?? op.m_mcc_mnc }));
          setRegistering(null);
          return;
        } else if (result === "fail" || result === "0") {
          setRegisterResult(t("mobilenet.registrationFailed", "Registration failed"));
          setRegistering(null);
          return;
        }
      }
      setRegisterResult(t("mobilenet.registrationTimedOut", "Registration timed out"));
    } catch (e) {
      setRegisterResult(e instanceof ApiError ? e.message : String(e));
    } finally {
      setRegistering(null);
    }
  };

  // Reboot
  const handleReboot = async () => {
    if (!window.confirm(t("mobilenet.confirmReboot", "Reboot the router now?"))) return;
    try {
      await apiFetch("/api/device/reboot", { method: "POST" });
      showMsg(t("mobilenet.rebooting", "Router is rebooting…"), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    }
  };

  return (
    <>
      <PageHeader
        title={t("mobilenet.title", "Mobile Network")}
        description={t("mobilenet.desc", "Manage cellular data, roaming, airplane mode, and carrier registration.")}
      />

      {dataErr && <ErrorBanner message={String(dataErr)} />}
      {msg && (
        <div
          className={`mb-4 rounded-md border px-3 py-2 text-sm ${
            msgIsError
              ? "border-error/40 bg-error/10 text-error"
              : "border-success/40 bg-success/10 text-success"
          }`}
        >
          {msg}
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        {/* Status + Toggles */}
        <SectionCard title={t("mobilenet.connection", "Connection")}>
          <div className="space-y-4">
            <Row label={t("mobilenet.status", "Status")}>
              <span
                className={`rounded-full px-2 py-0.5 text-xs font-medium ${
                  connectStatus.includes("connected")
                    ? "bg-success/15 text-success"
                    : "bg-text-dim/10 text-text-dim"
                }`}
              >
                {connectStatus || "—"}
              </span>
            </Row>
            <Row label={t("mobilenet.mobileData", "Mobile Data")}>
              <Toggle
                checked={dataEnabled}
                onChange={handleDataToggle}
                disabled={saving}
              />
            </Row>
            <Row label={t("mobilenet.roaming", "Roaming")}>
              <Toggle
                checked={roamEnabled}
                onChange={handleRoamToggle}
                disabled={saving}
              />
            </Row>
          </div>
        </SectionCard>

        {/* Airplane Mode */}
        <SectionCard title={t("mobilenet.airplaneMode", "Airplane Mode")}>
          <div className="space-y-3">
            <Row label={t("mobilenet.airplaneMode", "Airplane Mode")}>
              <Toggle
                checked={airplaneOn}
                onChange={handleAirplaneToggle}
                disabled={saving}
              />
            </Row>
            {!airplaneOn && (
              <div className="flex items-start gap-2 rounded-md border border-warning/40 bg-warning/10 p-3 text-xs text-warning">
                <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                <span>
                  {t("mobilenet.airplaneHint", "Note: If modem doesn't recover after disabling airplane mode, reboot via Device Control.")}
                </span>
              </div>
            )}
          </div>
        </SectionCard>
      </div>

      {/* Carrier Scan */}
      <SectionCard title={t("mobilenet.manualCarrierScan", "Manual Carrier Scan")} className="mt-4">
        <div className="mb-3 flex items-center gap-3">
          <Button
            onClick={handleScan}
            loading={isScanning}
            disabled={isScanning}
            variant="outline"
          >
            <Radio className="h-4 w-4" />
            {isScanning ? t("mobilenet.scanning", "Scanning…") : t("mobilenet.scanForCarriers", "Scan for Carriers")}
          </Button>
          {isScanning && (
            <span className="text-sm text-text-dim">
              {t("mobilenet.scanTakesTime", "This may take up to 60 seconds…")}
            </span>
          )}
        </div>

        {registerResult && (
          <div className="mb-3 rounded-md border border-border px-3 py-2 text-sm">
            {registerResult}
          </div>
        )}

        {operators.length > 0 && (
          <div className="overflow-hidden rounded-md border border-border">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border bg-bg-elevated text-xs uppercase tracking-wider text-text-dim">
                  <th className="px-3 py-2 text-left">{t("mobilenet.carrier", "Carrier")}</th>
                  <th className="px-3 py-2 text-left">MCC/MNC</th>
                  <th className="px-3 py-2 text-left">RAT</th>
                  <th className="px-3 py-2 text-left">{t("mobilenet.status", "Status")}</th>
                  <th className="px-3 py-2 text-left"></th>
                </tr>
              </thead>
              <tbody>
                {operators.map((op) => {
                  const key = (op.m_mcc_mnc ?? "") + (op.m_rat ?? "");
                  return (
                    <tr key={key} className="border-b border-border/60 last:border-0">
                      <td className="px-3 py-2 font-medium">{op.m_oper_name ?? "—"}</td>
                      <td className="px-3 py-2 font-mono text-xs">{op.m_mcc_mnc ?? "—"}</td>
                      <td className="px-3 py-2">{op.m_rat ?? "—"}</td>
                      <td className="px-3 py-2 text-text-dim">{op.m_status ?? "—"}</td>
                      <td className="px-3 py-2">
                        <Button
                          size="sm"
                          variant="outline"
                          loading={registering === (op.m_mcc_mnc ?? "")}
                          disabled={!!registering}
                          onClick={() => handleRegister(op)}
                        >
                          {t("mobilenet.select", "Select")}
                        </Button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </SectionCard>

      {/* Reboot */}
      <SectionCard title={t("mobilenet.deviceControl", "Device Control")} className="mt-4">
        <div className="flex items-center justify-between">
          <div>
            <p className="text-sm font-medium">{t("mobilenet.rebootRouter", "Reboot Router")}</p>
            <p className="mt-0.5 text-xs text-text-dim">
              {t("mobilenet.rebootHint", "Use this if modem fails to recover from airplane mode.")}
            </p>
          </div>
          <Button variant="danger" onClick={handleReboot}>
            {t("mobilenet.reboot", "Reboot")}
          </Button>
        </div>
      </SectionCard>
    </>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-sm text-text-dim">{label}</span>
      {children}
    </div>
  );
}
