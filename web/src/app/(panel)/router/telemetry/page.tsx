"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input } from "@/components/admin/Button";
import { Trash2, ShieldOff } from "lucide-react";

// Actual shape: { action_response: object, blocked_domains: string[] }
interface DomainFilterConfig {
  action_response?: Record<string, unknown>;
  blocked_domains?: string[];
}

const KNOWN_TELEMETRY_DOMAINS = ["iot.zte.com.cn", "ztems.com", "zte.com.cn", "ztemt.com.cn"];

export default function TelemetryPage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<DomainFilterConfig>("/api/router/domain-filter");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const [newDomain, setNewDomain] = useState("");
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  const blockedDomains = data?.blocked_domains ?? [];

  function showMsg(text: string, err: boolean) {
    setMsg({ text, err });
  }

  async function addDomain(domain: string) {
    const trimmed = domain.trim();
    if (!trimmed) { showMsg(t("telemetry.enterDomain", "Enter a domain name"), true); return; }
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/router/domain-filter", { method: "PUT", body: { action: "add", domain: trimmed } });
      showMsg(t("telemetry.added", "Added {{domain}}", { domain: trimmed }), false);
      setNewDomain("");
      mutate();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function deleteDomain(domain: string) {
    setDeleteTarget(null);
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/router/domain-filter", { method: "PUT", body: { action: "delete", domain } });
      showMsg(t("telemetry.removed", "Removed {{domain}}", { domain }), false);
      mutate();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function blockAllTelemetry() {
    setBusy(true);
    setMsg(null);
    const existing = new Set(blockedDomains);
    let added = 0;
    for (const domain of KNOWN_TELEMETRY_DOMAINS) {
      if (existing.has(domain)) continue;
      try {
        await apiFetch("/api/router/domain-filter", { method: "PUT", body: { action: "add", domain } });
        added++;
      } catch {
        // continue with remaining
      }
    }
    showMsg(added > 0 ? t("telemetry.blockedCount", "Blocked {{count}} telemetry domain", { count: added }) : t("telemetry.allAlreadyBlocked", "All telemetry domains already blocked"), false);
    mutate();
    setBusy(false);
  }

  return (
    <>
      <PageHeader title={t("telemetry.title", "Telemetry Blocker")} description={t("telemetry.desc", "Block ZTE telemetry and other unwanted domains.")} />

      {error && <ErrorBanner message={error instanceof ApiError ? error.message : String(error)} />}
      {msg && (
        <div className={`mb-4 rounded-md px-3 py-2 text-sm ${msg.err ? "border border-error/40 bg-error/10 text-error" : "border border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      <SectionCard title={t("telemetry.quickBlock", "Quick Block")} className="mb-4">
        <p className="mb-3 text-xs text-text-dim">
          {t("telemetry.knownTelemetry", "Known ZTE telemetry:")} {KNOWN_TELEMETRY_DOMAINS.join(", ")}
        </p>
        <Button variant="outline" onClick={blockAllTelemetry} loading={busy} disabled={busy}>
          <ShieldOff className="h-4 w-4" />
          {t("telemetry.blockKnownTelemetry", "Block Known Telemetry")}
        </Button>
      </SectionCard>

      <SectionCard title={t("telemetry.addDomain", "Add Domain")} className="mb-4">
        <div className="flex gap-2">
          <Input
            value={newDomain}
            onChange={(e) => setNewDomain(e.target.value)}
            placeholder="example.com"
            onKeyDown={(e) => { if (e.key === "Enter") addDomain(newDomain); }}
            className="max-w-sm"
          />
          <Button onClick={() => addDomain(newDomain)} loading={busy} disabled={busy}>{t("telemetry.add", "Add")}</Button>
        </div>
      </SectionCard>

      <SectionCard title={t("telemetry.blockedDomains", "Blocked Domains")}>
        {blockedDomains.length > 0 ? (
          <div className="space-y-0">
            {blockedDomains.map((domain) => (
              <div key={domain} className="flex items-center justify-between border-b border-border/60 py-2 last:border-0">
                <span className="font-mono text-sm">{domain}</span>
                <button
                  onClick={() => setDeleteTarget(domain)}
                  className="text-text-dim hover:text-error transition"
                  aria-label={t("telemetry.removeDomain", "Remove domain")}
                >
                  <Trash2 className="h-4 w-4" />
                </button>
              </div>
            ))}
          </div>
        ) : (
          <p className="text-sm text-text-dim">{t("telemetry.noDomainsBlocked", "No domains blocked.")}</p>
        )}

        {deleteTarget && (
          <div className="mt-3 rounded-md border border-error/40 bg-error/10 p-3">
            <p className="mb-2 text-sm text-error">{t("telemetry.confirmRemove", 'Remove "{{domain}}"?', { domain: deleteTarget })}</p>
            <div className="flex gap-2">
              <Button variant="danger" size="sm" onClick={() => deleteDomain(deleteTarget)} loading={busy}>{t("telemetry.remove", "Remove")}</Button>
              <Button variant="ghost" size="sm" onClick={() => setDeleteTarget(null)}>{t("telemetry.cancel", "Cancel")}</Button>
            </div>
          </div>
        )}
      </SectionCard>
    </>
  );
}
