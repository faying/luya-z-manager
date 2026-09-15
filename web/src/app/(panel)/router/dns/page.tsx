"use client";

import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input } from "@/components/admin/Button";

interface DnsConfig {
  dns_mode?: string;
  prefer_dns_manual?: string;
  standby_dns_manual?: string;
  prefer_dns_auto?: string;
  standby_dns_auto?: string;
}

interface DoHStatusConfig {
  enabled?: boolean;
  upstream_url?: string;
  cache_enabled?: boolean;
  cache_max_entries?: number;
  listen_addr?: string;
  timeout_ms?: number;
}

interface DoHStatusStats {
  cache_entries?: number;
  cache_hits?: number;
  cache_misses?: number;
  queries_total?: number;
}

interface DoHStatus {
  config?: DoHStatusConfig;
  running?: boolean;
  stats?: DoHStatusStats;
}

interface DoHCacheEntry {
  name?: string;
  type?: string;
  ttl?: number;
}

type DnsMode = "auto" | "manual" | "doh";

function isValidIPv4(s: string): boolean {
  const parts = s.split(".");
  if (parts.length !== 4) return false;
  return parts.every((p) => {
    const n = Number(p);
    return /^\d+$/.test(p) && n >= 0 && n <= 255;
  });
}

export default function DnsPage() {
  const { t } = useTranslation();
  const { data: dnsConfig, error: dnsErr, mutate: mutateDns } = useApi<DnsConfig>("/api/router/dns");
  const { data: dohStatus, error: dohErr, mutate: mutateDoh } = useApi<DoHStatus>("/api/doh/status");

  const [mode, setMode] = useState<DnsMode>("auto");
  const [primary, setPrimary] = useState("");
  const [secondary, setSecondary] = useState("");
  const [upstreamUrl, setUpstreamUrl] = useState("");
  const [cacheEntries, setCacheEntries] = useState<DoHCacheEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const [initDone, setInitDone] = useState(false);

  useEffect(() => {
    if (initDone || !dnsConfig) return;
    if (dohStatus?.config?.enabled) {
      setMode("doh");
    } else if (dnsConfig.dns_mode === "manual") {
      setMode("manual");
    } else {
      setMode("auto");
    }
    setPrimary(dnsConfig.prefer_dns_manual ?? "");
    setSecondary(dnsConfig.standby_dns_manual ?? "");
    if (dohStatus?.config?.upstream_url) setUpstreamUrl(dohStatus.config.upstream_url);
    setInitDone(true);
  }, [dnsConfig, dohStatus, initDone]);

  useEffect(() => {
    if (!dohStatus?.config?.enabled) return;
    apiFetch<DoHCacheEntry[]>("/api/doh/cache").then((d) => setCacheEntries(d ?? [])).catch(() => {});
  }, [dohStatus?.config?.enabled]);

  async function applyAuto() {
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/router/dns", { method: "PUT", body: { dns_mode: "auto", prefer_dns_manual: "", standby_dns_manual: "" } });
      await apiFetch("/api/doh/disable", { method: "POST", body: {} });
      setMsg({ text: t("dns.dnsSetAuto", "DNS set to Auto"), err: false });
      mutateDns();
      mutateDoh();
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : String(e), err: true });
    } finally {
      setBusy(false);
    }
  }

  async function applyManual() {
    if (!primary) return setMsg({ text: t("dns.primaryEmpty", "Primary DNS cannot be empty"), err: true });
    if (!isValidIPv4(primary)) return setMsg({ text: t("dns.invalidPrimary", "Invalid primary DNS address"), err: true });
    if (secondary && !isValidIPv4(secondary)) return setMsg({ text: t("dns.invalidSecondary", "Invalid secondary DNS address"), err: true });
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/router/dns", { method: "PUT", body: { dns_mode: "manual", prefer_dns_manual: primary, standby_dns_manual: secondary } });
      await apiFetch("/api/doh/disable", { method: "POST", body: {} });
      setMsg({ text: t("dns.dnsSetManual", "DNS set to Manual"), err: false });
      mutateDns();
      mutateDoh();
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : String(e), err: true });
    } finally {
      setBusy(false);
    }
  }

  async function applyDoH() {
    if (!upstreamUrl) return setMsg({ text: t("dns.upstreamEmpty", "Upstream URL cannot be empty"), err: true });
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/doh/config", { method: "PUT", body: { upstreams: [upstreamUrl] } });
      await apiFetch("/api/doh/enable", { method: "POST", body: {} });
      setMsg({ text: t("dns.dohEnabled", "DoH enabled"), err: false });
      mutateDns();
      mutateDoh();
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : String(e), err: true });
    } finally {
      setBusy(false);
    }
  }

  async function handleApply() {
    if (mode === "auto") await applyAuto();
    else if (mode === "manual") await applyManual();
    else await applyDoH();
  }

  async function clearCache() {
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/doh/cache/clear", { method: "POST", body: {} });
      setCacheEntries([]);
      setMsg({ text: t("dns.cacheCleared", "Cache cleared"), err: false });
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : String(e), err: true });
    } finally {
      setBusy(false);
    }
  }

  const loadErr = dnsErr || dohErr;

  return (
    <>
      <PageHeader title={t("dns.title", "DNS Settings")} description={t("dns.desc", "Configure DNS resolution mode for the router.")} />

      {loadErr && <ErrorBanner message={loadErr instanceof ApiError ? loadErr.message : String(loadErr)} />}
      {msg && (
        <div className={`mb-4 rounded-md px-3 py-2 text-sm ${msg.err ? "border border-error/40 bg-error/10 text-error" : "border border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      <SectionCard title={t("dns.dnsMode", "DNS Mode")} className="mb-4">
        <div className="flex flex-col gap-3">
          {(["auto", "manual", "doh"] as DnsMode[]).map((m) => (
            <label key={m} className="flex cursor-pointer items-center gap-3">
              <input
                type="radio"
                name="dns_mode"
                value={m}
                checked={mode === m}
                onChange={() => setMode(m)}
                className="h-4 w-4 accent-accent"
              />
              <span className="text-sm font-medium capitalize">{m === "doh" ? t("dns.modeDoh", "DNS-over-HTTPS (DoH)") : m === "auto" ? t("dns.modeAuto", "Auto (ISP-assigned)") : t("dns.modeManual", "Manual")}</span>
            </label>
          ))}
        </div>
      </SectionCard>

      {mode === "manual" && (
        <SectionCard title={t("dns.manualServers", "Manual DNS Servers")} className="mb-4">
          <div className="flex flex-col gap-3">
            <div>
              <label className="mb-1 block text-xs font-medium text-text-dim">{t("dns.primaryDns", "Primary DNS")}</label>
              <Input value={primary} onChange={(e) => setPrimary(e.target.value)} placeholder={t("dns.primaryPlaceholder", "e.g. 1.1.1.1")} />
            </div>
            <div>
              <label className="mb-1 block text-xs font-medium text-text-dim">{t("dns.secondaryDns", "Secondary DNS (optional)")}</label>
              <Input value={secondary} onChange={(e) => setSecondary(e.target.value)} placeholder={t("dns.secondaryPlaceholder", "e.g. 1.0.0.1")} />
            </div>
          </div>
        </SectionCard>
      )}

      {mode === "doh" && (
        <SectionCard title={t("dns.dohUpstream", "DoH Upstream")} className="mb-4">
          <div>
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("dns.upstreamUrl", "Upstream URL")}</label>
            <Input value={upstreamUrl} onChange={(e) => setUpstreamUrl(e.target.value)} placeholder="https://cloudflare-dns.com/dns-query" />
          </div>
          {dohStatus && (
            <div className="mt-3 flex flex-wrap gap-4 text-sm text-text-dim">
              <span>{t("dns.cacheEntriesLabel", "Cache entries:")} <span className="font-mono text-text">{dohStatus.stats?.cache_entries ?? "—"}</span></span>
              <span>{t("dns.queriesLabel", "Queries:")} <span className="font-mono text-text">{dohStatus.stats?.queries_total ?? "—"}</span></span>
            </div>
          )}
        </SectionCard>
      )}

      <div className="mb-4">
        <Button onClick={handleApply} loading={busy}>{t("dns.apply", "Apply")}</Button>
      </div>

      {mode === "doh" && dohStatus?.config?.enabled && (
        <SectionCard title={t("dns.dohCache", "DoH Cache")}>
          <div className="mb-3 flex items-center justify-between">
            <span className="text-sm text-text-dim">{t("dns.entriesCount", "{{count}} entries", { count: cacheEntries.length })}</span>
            <Button variant="outline" size="sm" onClick={clearCache} loading={busy}>{t("dns.clearCache", "Clear Cache")}</Button>
          </div>
          {cacheEntries.length > 0 && (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-border text-left text-xs text-text-dim">
                    <th className="pb-2 pr-4">{t("dns.colName", "Name")}</th>
                    <th className="pb-2 pr-4">{t("dns.colType", "Type")}</th>
                    <th className="pb-2">TTL</th>
                  </tr>
                </thead>
                <tbody>
                  {cacheEntries.map((e, i) => (
                    <tr key={i} className="border-b border-border/60 last:border-0">
                      <td className="py-1.5 pr-4 font-mono">{e.name ?? "—"}</td>
                      <td className="py-1.5 pr-4">{e.type ?? "—"}</td>
                      <td className="py-1.5">{e.ttl ?? "—"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </SectionCard>
      )}
    </>
  );
}
