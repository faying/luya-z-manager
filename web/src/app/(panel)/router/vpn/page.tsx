"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Toggle } from "@/components/admin/Button";

interface VpnConfig {
  l2tp_passthrough?: string;
  pptp_passthrough?: string;
  ipsec_passthrough?: string;
}

export default function VpnPage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<VpnConfig>("/api/router/vpn");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);

  const l2tp = data?.l2tp_passthrough === "1";
  const pptp = data?.pptp_passthrough === "1";
  const ipsec = data?.ipsec_passthrough === "1";

  async function setPassthrough(field: keyof VpnConfig, enabled: boolean) {
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/router/vpn", { method: "PUT", body: { [field]: enabled ? "1" : "0" } });
      const proto = field.replace("_passthrough", "").toUpperCase();
      setMsg({
        text: enabled
          ? t("vpn.passthroughEnabled", "{{proto}} passthrough enabled", { proto })
          : t("vpn.passthroughDisabled", "{{proto}} passthrough disabled", { proto }),
        err: false,
      });
      mutate();
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : String(e), err: true });
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <PageHeader title={t("vpn.title", "VPN Passthrough")} description={t("vpn.desc", "Allow VPN clients behind NAT to initiate tunnels to external servers.")} />

      {error && <ErrorBanner message={error instanceof ApiError ? error.message : String(error)} />}
      {msg && (
        <div className={`mb-4 rounded-md px-3 py-2 text-sm ${msg.err ? "border border-error/40 bg-error/10 text-error" : "border border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      <div className="mb-4 rounded-md border border-border bg-bg-elevated px-4 py-3 text-xs text-text-dim">
        {t("vpn.infoNote", "Passthrough lets a client behind NAT initiate a VPN connection to an outside server; it does not run a VPN server on the router.")}
      </div>

      <SectionCard title={t("vpn.settingsTitle", "Passthrough Settings")}>
        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <div>
              <p className="text-sm font-medium">{t("vpn.l2tpLabel", "L2TP Passthrough")}</p>
              <p className="mt-0.5 text-xs text-text-dim">{t("vpn.l2tpDesc", "Layer 2 Tunneling Protocol")}</p>
            </div>
            <Toggle checked={l2tp} onChange={(v) => setPassthrough("l2tp_passthrough", v)} disabled={busy} />
          </div>
          <div className="flex items-center justify-between border-t border-border/60 pt-4">
            <div>
              <p className="text-sm font-medium">{t("vpn.pptpLabel", "PPTP Passthrough")}</p>
              <p className="mt-0.5 text-xs text-text-dim">{t("vpn.pptpDesc", "Point-to-Point Tunneling Protocol")}</p>
            </div>
            <Toggle checked={pptp} onChange={(v) => setPassthrough("pptp_passthrough", v)} disabled={busy} />
          </div>
          <div className="flex items-center justify-between border-t border-border/60 pt-4">
            <div>
              <p className="text-sm font-medium">{t("vpn.ipsecLabel", "IPSec Passthrough")}</p>
              <p className="mt-0.5 text-xs text-text-dim">{t("vpn.ipsecDesc", "Internet Protocol Security")}</p>
            </div>
            <Toggle checked={ipsec} onChange={(v) => setPassthrough("ipsec_passthrough", v)} disabled={busy} />
          </div>
        </div>
      </SectionCard>
    </>
  );
}
