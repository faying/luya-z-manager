"use client";

import { useState, useEffect } from "react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { useSWRConfig } from "swr";
import { useTranslation } from "react-i18next";

interface StcStatus {
  enabled?: boolean | string | number;
}

interface StcParams {
  lte_collect_timer?: string;
  nrsa_collect_timer?: string;
  lte_whitelist_max?: string;
  nrsa_whitelist_max?: string;
}

function isTruthy(v?: boolean | string | number): boolean {
  if (v === undefined || v === null) return false;
  if (typeof v === "boolean") return v;
  if (typeof v === "number") return v !== 0;
  return v === "1" || v === "true" || v === "enabled";
}

export default function StcPage() {
  const { t } = useTranslation();
  const { mutate } = useSWRConfig();
  // /api/cell/stc/* returns 503 currently — gracefully handle missing data
  const { data: status, isLoading: statusLoading, error: statusErr } = useApi<StcStatus>("/api/cell/stc/status");
  const { data: params, isLoading: paramsLoading, error: paramsErr } = useApi<StcParams>("/api/cell/stc/params");
  const serviceDown = statusErr != null || paramsErr != null;

  const [lteTimer, setLteTimer] = useState("");
  const [nrsaTimer, setNrsaTimer] = useState("");
  const [lteMax, setLteMax] = useState("");
  const [nrsaMax, setNrsaMax] = useState("");

  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsErr, setMsgIsErr] = useState(false);

  useEffect(() => {
    if (params) {
      setLteTimer(params.lte_collect_timer ?? "");
      setNrsaTimer(params.nrsa_collect_timer ?? "");
      setLteMax(params.lte_whitelist_max ?? "");
      setNrsaMax(params.nrsa_whitelist_max ?? "");
    }
  }, [params]);

  function showMsg(text: string, isErr: boolean) {
    setMsg(text);
    setMsgIsErr(isErr);
  }

  const enabled = isTruthy(status?.enabled);

  async function toggleEnable(next: boolean) {
    setBusy(true);
    try {
      await apiFetch(next ? "/api/cell/stc/enable" : "/api/cell/stc/disable", { method: "POST" });
      showMsg(next ? t("stc.enabledMsg", "STC enabled") : t("stc.disabledMsg", "STC disabled"), false);
      mutate("/api/cell/stc/status");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function applyParams() {
    setBusy(true);
    try {
      await apiFetch("/api/cell/stc/params", {
        method: "PUT",
        body: {
          lte_collect_timer: lteTimer,
          nrsa_collect_timer: nrsaTimer,
          lte_whitelist_max: lteMax,
          nrsa_whitelist_max: nrsaMax,
        },
      });
      showMsg(t("stc.paramsUpdated", "STC parameters updated"), false);
      mutate("/api/cell/stc/params");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function resetWhitelist() {
    if (!confirm(t("stc.confirmReset", "Reset STC whitelist? This cannot be undone."))) return;
    setBusy(true);
    try {
      await apiFetch("/api/cell/stc/reset", { method: "POST" });
      showMsg(t("stc.whitelistReset", "STC whitelist reset"), false);
      mutate("/api/cell/stc/status");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  const loading = statusLoading || paramsLoading;
  void serviceDown; // referenced below

  return (
    <>
      <PageHeader
        title="STC"
        description={t("stc.desc", "Smart Traffic Control — whitelist and timer configuration.")}
        actions={
          <Button variant="danger" size="sm" loading={busy} onClick={resetWhitelist}>
            {t("stc.resetWhitelist", "Reset Whitelist")}
          </Button>
        }
      />

      {(statusErr || paramsErr) && (
        <div className="mb-4 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          {t("stc.serviceUnavailable", "STC service is currently unavailable (503). Controls are disabled.")}
        </div>
      )}

      {msg && (
        <div className="mb-3">
          {msgIsErr ? (
            <ErrorBanner message={msg} />
          ) : (
            <div className="rounded-md border border-success/40 bg-success/10 px-3 py-2 text-sm text-success">{msg}</div>
          )}
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        <SectionCard title={t("stc.statusTitle", "Status")}>
          {loading ? (
            <p className="text-sm text-text-dim">{t("stc.loading", "Loading…")}</p>
          ) : (
            <div className="space-y-4">
              <div className="flex items-center justify-between">
                <span className="text-sm">{t("stc.stcEnabled", "STC Enabled")}</span>
                <Toggle checked={enabled} onChange={toggleEnable} disabled={busy || serviceDown} />
              </div>
              <div className="flex justify-between border-t border-border/60 pt-3 text-sm">
                <span className="text-text-dim">{t("stc.currentState", "Current State")}</span>
                <span className={`font-medium ${enabled ? "text-success" : "text-text-dim"}`}>
                  {enabled ? t("stc.enabled", "Enabled") : t("stc.disabled", "Disabled")}
                </span>
              </div>
            </div>
          )}
        </SectionCard>

        <SectionCard title={t("stc.parametersTitle", "Parameters")}>
          <div className="space-y-3">
            <div className="grid grid-cols-2 gap-3">
              <div>
                <label className="mb-1 block text-xs text-text-dim">{t("stc.lteCollectTimer", "LTE Collect Timer")}</label>
                <Input
                  value={lteTimer}
                  onChange={(e) => setLteTimer(e.target.value)}
                  placeholder={t("stc.seconds", "seconds")}
                  disabled={loading}
                />
              </div>
              <div>
                <label className="mb-1 block text-xs text-text-dim">{t("stc.nrsaCollectTimer", "NR-SA Collect Timer")}</label>
                <Input
                  value={nrsaTimer}
                  onChange={(e) => setNrsaTimer(e.target.value)}
                  placeholder={t("stc.seconds", "seconds")}
                  disabled={loading}
                />
              </div>
              <div>
                <label className="mb-1 block text-xs text-text-dim">{t("stc.lteWhitelistMax", "LTE Whitelist Max")}</label>
                <Input
                  value={lteMax}
                  onChange={(e) => setLteMax(e.target.value)}
                  placeholder={t("stc.count", "count")}
                  disabled={loading}
                />
              </div>
              <div>
                <label className="mb-1 block text-xs text-text-dim">{t("stc.nrsaWhitelistMax", "NR-SA Whitelist Max")}</label>
                <Input
                  value={nrsaMax}
                  onChange={(e) => setNrsaMax(e.target.value)}
                  placeholder={t("stc.count", "count")}
                  disabled={loading}
                />
              </div>
            </div>
            <Button size="sm" loading={busy} onClick={applyParams} disabled={loading || serviceDown}>
              {t("stc.applyParameters", "Apply Parameters")}
            </Button>
          </div>
        </SectionCard>
      </div>
    </>
  );
}
