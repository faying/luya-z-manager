"use client";

import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { useSWRConfig } from "swr";
import { Signal } from "lucide-react";

interface NetworkSignal {
  net_select_mode?: string;
}

const NET_SELECT_OPTIONS: { label: string; value: string; desc: string; lk: string; dk: string }[] = [
  { label: "Auto", value: "auto_select", desc: "Let the device pick the best available network", lk: "netmode.autoLabel", dk: "netmode.autoDesc" },
  { label: "5G/NR Only", value: "5G_only", desc: "Use 5G New Radio only", lk: "netmode.nr5gLabel", dk: "netmode.nr5gDesc" },
  { label: "5G + 4G (NSA)", value: "5G_4G", desc: "5G Non-Standalone with LTE anchor", lk: "netmode.nsaLabel", dk: "netmode.nsaDesc" },
  { label: "4G LTE Only", value: "4G_only", desc: "Use LTE only", lk: "netmode.lteLabel", dk: "netmode.lteDesc" },
  { label: "3G WCDMA", value: "3G_only", desc: "Use WCDMA/UMTS only", lk: "netmode.wcdmaLabel", dk: "netmode.wcdmaDesc" },
  { label: "2G GSM", value: "2G_only", desc: "Use GSM only", lk: "netmode.gsmLabel", dk: "netmode.gsmDesc" },
];

export default function NetworkModePage() {
  const { t } = useTranslation();
  const { mutate } = useSWRConfig();
  const { data: signal, error } = useApi<NetworkSignal>("/api/network/signal", {
    refreshInterval: 5000,
  });

  const [selected, setSelected] = useState<string>("auto_select");
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsError, setMsgIsError] = useState(false);

  // Sync selection from server on load
  useEffect(() => {
    if (signal?.net_select_mode) {
      setSelected(signal.net_select_mode);
    }
  }, [signal?.net_select_mode]);

  const handleApply = async () => {
    setSaving(true);
    setMsg(null);
    try {
      await apiFetch("/api/modem/network-mode", {
        method: "PUT",
        body: { net_select: selected },
      });

      // Poll up to 5 times (10s) to confirm
      for (let i = 0; i < 5; i++) {
        await new Promise((r) => setTimeout(r, 2000));
        const fresh = await apiFetch<NetworkSignal>("/api/network/signal");
        if (fresh.net_select_mode === selected) {
          await mutate("/api/network/signal");
          setMsg(t("netmode.updated", "Network mode updated"));
          setMsgIsError(false);
          setSaving(false);
          return;
        }
      }
      setMsg(t("netmode.sent", "Mode sent — router may still be switching"));
      setMsgIsError(false);
    } catch (e) {
      setMsg(e instanceof ApiError ? e.message : String(e));
      setMsgIsError(true);
    } finally {
      setSaving(false);
    }
  };

  const currentValue = signal?.net_select_mode;
  const isDirty = selected !== currentValue;

  return (
    <>
      <PageHeader
        title={t("netmode.title", "Network Mode")}
        description={t("netmode.desc", "Select preferred radio access technology (RAT).")}
        actions={
          currentValue ? (
            <span className="flex items-center gap-1.5 rounded-full bg-bg-elevated px-3 py-1 text-xs text-text-dim">
              <Signal className="h-3 w-3" />
              {t("netmode.current", "Current: {{mode}}", {
                mode: (() => {
                  const o = NET_SELECT_OPTIONS.find((x) => x.value === currentValue);
                  return o ? t(o.lk, o.label) : currentValue;
                })(),
              })}
            </span>
          ) : undefined
        }
      />

      {error && <ErrorBanner message={String(error)} />}
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

      <SectionCard title={t("netmode.selectMode", "Select Mode")}>
        <div className="space-y-2">
          {NET_SELECT_OPTIONS.map((opt) => {
            const isSelected = selected === opt.value;
            const isCurrent = currentValue === opt.value;
            return (
              <label
                key={opt.value}
                className={`flex cursor-pointer items-center gap-3 rounded-lg border p-3 transition ${
                  isSelected
                    ? "border-accent bg-accent/5"
                    : "border-border bg-bg-card hover:border-border-focus"
                }`}
              >
                <input
                  type="radio"
                  name="net_select"
                  value={opt.value}
                  checked={isSelected}
                  onChange={() => setSelected(opt.value)}
                  className="accent-accent"
                />
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium">{t(opt.lk, opt.label)}</span>
                    {isCurrent && (
                      <span className="rounded-full bg-success/15 px-2 py-0.5 text-[10px] font-medium text-success">
                        {t("netmode.active", "Active")}
                      </span>
                    )}
                  </div>
                  <p className="mt-0.5 text-xs text-text-dim">{t(opt.dk, opt.desc)}</p>
                </div>
              </label>
            );
          })}
        </div>

        <div className="mt-4 flex items-center justify-end gap-2">
          <Button
            onClick={handleApply}
            loading={saving}
            disabled={saving || !isDirty}
          >
            {t("common.apply", "Apply")}
          </Button>
        </div>
      </SectionCard>
    </>
  );
}
