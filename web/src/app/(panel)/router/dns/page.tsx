"use client";
// DNS / DoH (new design). Settings page: status first (what resolves DNS now),
// then the mode + servers form, then the DoH proxy numbers and its cache.
//
// Writes (controls-inventory §/router/dns, design §3.1):
//   Apply — tier 2 (inline confirm; owner's call 9-24, it restarts dnsmasq),
//   tier 3 over Tailscale (a bad upstream can cut the remote session). It is
//   always TWO requests, tracked as steps (R10):
//     auto / manual:  ① PUT /api/router/dns   ② POST /api/doh/disable
//     DoH:            ① PUT /api/doh/config {upstream_url}   ② POST /api/doh/enable
//   Steps ② start/stop the agent's DoH proxy and rewrite + restart dnsmasq.
//   Readback: /api/router/dns and
//   /api/doh/status (both handlers drop dnsmasq errors, so only the readback
//   says it worked).
//   DoH already running + changed upstream: the config patch takes effect at
//   once, but /api/doh/enable answers 500 "already running". That exact reply
//   counts as step ② done once /api/doh/status says the proxy is running; the
//   readback then decides.
//   Clear cache — tier 2 (inline confirm). No readback: live queries refill the
//   cache straight away, so `cache_entries == 0` can't prove anything; the
//   result reads "accepted by the device".
//
// Fixed vs the old page: the cache list reads `domain` (agent never sends
// `name`, doh/mod.rs:106); the form no longer freezes on the first load.
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Trash } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp, type WriteStep } from "@/lib/api/writeOp";
import type { DohCacheEntry, DohStatus, RouterDns } from "@/lib/api/schemas/router";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  Segmented,
  StatusBlock,
  StatusMark,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

type DnsMode = "auto" | "manual" | "doh";

interface Draft {
  mode: DnsMode;
  primary: string;
  secondary: string;
  url: string;
}

function isValidIPv4(s: string): boolean {
  const parts = s.split(".");
  if (parts.length !== 4) return false;
  return parts.every((p) => /^\d+$/.test(p) && Number(p) >= 0 && Number(p) <= 255);
}

const DEFAULT_UPSTREAM = "https://cloudflare-dns.com/dns-query";

export default function DnsPage() {
  const { t } = useTranslation();
  const dns = useApi<RouterDns>("/api/router/dns", { refreshInterval: 30000 });
  const doh = useApi<DohStatus>("/api/doh/status", { refreshInterval: 10000 });
  const dohOn = doh.data?.config.enabled === true;
  const cache = useApi<DohCacheEntry[]>(dohOn ? "/api/doh/cache" : null, { refreshInterval: 15000 });

  const loaded = !!dns.data && !!doh.data;
  const stale = dns.stale || doh.stale;

  // ── form: a draft once edited, the device's values until then ──
  const [draft, setDraft] = useState<Draft | null>(null);
  const [formErr, setFormErr] = useState<string | null>(null);
  const fromDevice: Draft | null = loaded
    ? {
        mode: dohOn ? "doh" : dns.data?.dns_mode === "manual" ? "manual" : "auto",
        primary: dns.data?.prefer_dns_manual ?? "",
        secondary: dns.data?.standby_dns_manual ?? "",
        url: doh.data?.config.upstream_url ?? "",
      }
    : null;
  const form = draft ?? fromDevice;
  const dirty =
    !!draft &&
    (!fromDevice ||
      draft.mode !== fromDevice.mode ||
      draft.primary !== fromDevice.primary ||
      draft.secondary !== fromDevice.secondary ||
      draft.url !== fromDevice.url);
  const edit = (patch: Partial<Draft>) => {
    if (!form) return;
    setDraft({ ...form, ...patch });
    setFormErr(null);
  };

  const primary = form?.primary.trim() ?? "";
  const secondary = form?.secondary.trim() ?? "";
  const url = form?.url.trim() ?? "";
  const mode = form?.mode ?? "auto";

  const disableDoh: WriteStep = {
    label: t("dns.stepDohOff", "Turn DoH off"),
    run: () => apiFetch("/api/doh/disable", { method: "POST", body: {} }),
  };
  const steps: WriteStep[] =
    mode === "doh"
      ? [
          {
            label: t("dns.stepUpstream", "DoH upstream"),
            run: () => apiFetch("/api/doh/config", { method: "PUT", body: { upstream_url: url } }),
          },
          {
            label: t("dns.stepDohOn", "Turn DoH on"),
            run: async () => {
              try {
                await apiFetch("/api/doh/enable", { method: "POST", body: {} });
              } catch (e) {
                // Proxy already up (e.g. only the upstream changed): the agent
                // refuses to start it twice. Count it as done if it is running.
                if (e instanceof ApiError && e.status === 500 && /already running/i.test(e.message)) {
                  const st = await apiFetch<DohStatus>("/api/doh/status");
                  if (st.running) return;
                }
                throw e;
              }
            },
          },
        ]
      : [
          {
            label: t("dns.stepServers", "DNS servers"),
            run: () =>
              apiFetch("/api/router/dns", {
                method: "PUT",
                body:
                  mode === "manual"
                    ? { dns_mode: "manual", prefer_dns_manual: primary, standby_dns_manual: secondary }
                    : { dns_mode: "auto", prefer_dns_manual: "", standby_dns_manual: "" },
              }),
          },
          disableDoh,
        ];

  const [ask, setAsk] = useState<null | 2 | 3>(null);
  const applyInline = useConfirmInline(ask === 2);

  const op = useWriteOp({
    tier: ask ?? 2,
    steps,
    verify: async () => {
      const [d, s] = await Promise.all([apiFetch<RouterDns>("/api/router/dns"), apiFetch<DohStatus>("/api/doh/status")]);
      await Promise.all([dns.mutate(d, { revalidate: false }), doh.mutate(s, { revalidate: false })]);
      if (mode === "doh") return s.running && s.config.enabled && s.config.upstream_url === url;
      if (s.config.enabled) return false;
      if (mode === "auto") return d.dns_mode === "auto";
      return d.dns_mode === "manual" && (d.prefer_dns_manual ?? "") === primary && (d.standby_dns_manual ?? "") === secondary;
    },
  });

  const locked = !loaded || stale || op.busy;

  function apply() {
    if (!form || locked) return;
    if (mode === "manual") {
      if (!primary) return setFormErr(t("dns.primaryEmpty", "Primary DNS cannot be empty"));
      if (!isValidIPv4(primary)) return setFormErr(t("dns.invalidPrimary", "Invalid primary DNS address"));
      if (secondary && !isValidIPv4(secondary)) return setFormErr(t("dns.invalidSecondary", "Invalid secondary DNS address"));
    }
    if (mode === "doh") {
      if (!url) return setFormErr(t("dns.upstreamEmpty", "Upstream URL cannot be empty"));
      if (!/^https:\/\/\S+$/i.test(url)) return setFormErr(t("dns.upstreamHttps", "The upstream must be an https:// address"));
    }
    setFormErr(null);
    setAsk(isRemoteAccess() ? 3 : 2);
  }

  function go() {
    op.start();
    op.confirm();
    setTimeout(() => setAsk(null), 0);
  }

  const applyConsequence =
    mode === "doh"
      ? t("dns.applyWhatDoh", "Apply saves the upstream, then starts the DoH proxy. DNS restarts; names may not resolve for a few seconds.")
      : t("dns.applyWhatPlain", "Apply saves the servers, then turns the DoH proxy off. DNS restarts; names may not resolve for a few seconds.");

  // ── clear cache (tier 2) ──
  const [askClear, setAskClear] = useState(false);
  const clearInline = useConfirmInline(askClear);
  const clearOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("dns.clearCache", "Clear Cache"),
        run: async () => {
          await apiFetch("/api/doh/cache/clear", { method: "POST", body: {} });
          void cache.mutate();
          void doh.mutate();
        },
      },
    ],
  });

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("dns.loading", "Reading DNS settings…");
  let reason: ReactNode = null;
  const loadErr = (!dns.data && dns.error) || (!doh.data && doh.error);
  if (loadErr) {
    tone = "bad";
    state = !dns.data ? t("dns.unreadable", "Can't read DNS settings") : t("dns.dohUnreadable", "Can't read DoH status");
    reason = loadErr.message;
  } else if (loaded && dohOn && doh.data?.running) {
    tone = "ok";
    state = t("dns.stDoh", "DNS over HTTPS");
    reason = <span className="nd-mono break-all">{doh.data.config.upstream_url}</span>;
  } else if (loaded && dohOn) {
    tone = "warn";
    state = t("dns.stDohDown", "DoH is on but the proxy is not running");
    reason = t("dns.stDohDownDesc", "Devices may fail to resolve names. Press Apply again, or switch to Auto.");
  } else if (loaded && dns.data?.dns_mode === "manual") {
    tone = "ok";
    state = t("dns.stManual", "Manual DNS");
    reason = <span className="nd-mono">{[dns.data.prefer_dns_manual, dns.data.standby_dns_manual].filter(Boolean).join(" · ") || "—"}</span>;
  } else if (loaded) {
    tone = "ok";
    state = t("dns.stAuto", "Automatic (from the operator)");
    const ops = [dns.data?.prefer_dns_auto, dns.data?.standby_dns_auto].filter(Boolean).join(" · ");
    reason = ops ? <span className="nd-mono">{ops}</span> : null;
  }
  if (loaded && stale) tone = "stale";

  const st = doh.data?.stats;
  const entries = cache.data ?? [];

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("dns.title", "DNS Settings")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("dns.desc", "Configure DNS resolution mode for the router.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={loaded && stale ? <Freshness stale lastOkAt={dns.stale ? dns.lastOkAt : doh.lastOkAt} what={t("dns.settingsWord", "Settings")} /> : undefined}
          actions={
            loadErr ? (
              <Button
                variant="secondary"
                size="sm"
                onPress={() => {
                  void dns.mutate();
                  void doh.mutate();
                }}
              >
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── mode + servers ── */}
        <section aria-labelledby="dns-mode-title">
          <GroupTitle id="dns-mode-title">{t("dns.dnsMode", "DNS Mode")}</GroupTitle>
          <div className={`nd-group grid gap-4 p-4 lg:p-5${stale ? " nd-stale" : ""}`}>
            {!form ? (
              <span className="nd-skel" style={{ width: "24ch" }} />
            ) : (
              <Segmented<DnsMode>
                label={t("dns.dnsMode", "DNS Mode")}
                block
                value={form.mode}
                isDisabled={locked}
                onChange={(m) => edit({ mode: m })}
                options={[
                  { id: "auto", label: t("dns.modeAuto", "Auto (ISP-assigned)") },
                  { id: "manual", label: t("dns.modeManual", "Manual") },
                  { id: "doh", label: t("dns.modeDoh", "DNS-over-HTTPS (DoH)") },
                ]}
              />
            )}

            {form?.mode === "auto" && (
              <p className="nd-aux">
                {t("dns.autoHint", "Uses the DNS servers the operator hands out.")}
                {(dns.data?.prefer_dns_auto || dns.data?.standby_dns_auto) && (
                  <>
                    {" "}
                    {t("dns.operatorDnsNow", "Now:")}{" "}
                    <span className="nd-mono">{[dns.data?.prefer_dns_auto, dns.data?.standby_dns_auto].filter(Boolean).join(" · ")}</span>
                  </>
                )}
              </p>
            )}

            {form?.mode === "manual" && (
              <div className="grid gap-3">
                <label className="grid gap-1">
                  <span className="nd-aux">{t("dns.primaryDnsLabel", "Primary DNS")}</span>
                  <input
                    className="nd-field nd-mono"
                    inputMode="decimal"
                    autoComplete="off"
                    value={form.primary}
                    onChange={(e) => edit({ primary: e.target.value })}
                    placeholder={t("dns.primaryPlaceholder", "e.g. 1.1.1.1")}
                    disabled={locked}
                  />
                </label>
                <label className="grid gap-1">
                  <span className="nd-aux">{t("dns.secondaryDns", "Secondary DNS (optional)")}</span>
                  <input
                    className="nd-field nd-mono"
                    inputMode="decimal"
                    autoComplete="off"
                    value={form.secondary}
                    onChange={(e) => edit({ secondary: e.target.value })}
                    placeholder={t("dns.secondaryPlaceholder", "e.g. 1.0.0.1")}
                    disabled={locked}
                  />
                </label>
              </div>
            )}

            {form?.mode === "doh" && (
              <label className="grid gap-1">
                <span className="nd-aux">{t("dns.upstreamUrl", "Upstream URL")}</span>
                <input
                  className="nd-field nd-mono"
                  type="url"
                  autoComplete="off"
                  autoCapitalize="off"
                  spellCheck={false}
                  value={form.url}
                  onChange={(e) => edit({ url: e.target.value })}
                  placeholder={DEFAULT_UPSTREAM}
                  disabled={locked}
                />
              </label>
            )}

            {form && (
              <p className="nd-aux">
                {form.mode === "doh"
                  ? t("dns.applyWhatDoh", "Apply saves the upstream, then starts the DoH proxy. DNS restarts; names may not resolve for a few seconds.")
                  : t("dns.applyWhatPlain", "Apply saves the servers, then turns the DoH proxy off. DNS restarts; names may not resolve for a few seconds.")}
              </p>
            )}

            {formErr && (
              <p role="alert" className="nd-error">
                {formErr}
              </p>
            )}

            <div className="flex flex-wrap items-center gap-3">
              <Button onPress={apply} isDisabled={locked || !form} pending={op.busy} {...(ask === 2 ? applyInline.triggerProps : {})}>
                {t("dns.apply", "Apply")}
              </Button>
              {dirty && !op.busy && (
                <Button
                  variant="ghost"
                  onPress={() => {
                    setDraft(null);
                    setFormErr(null);
                  }}
                >
                  {t("dns.discard", "Discard changes")}
                </Button>
              )}
            </div>
            {ask === 2 && (
              <ConfirmInline
                id={applyInline.id}
                open
                actionLabel={t("dns.apply", "Apply")}
                consequence={applyConsequence}
                onCancel={() => setAsk(null)}
                onConfirm={go}
              />
            )}
            <OpResult op={op} />
          </div>
          {loaded && stale && (
            <p className="nd-aux mt-1 px-1">
              <Freshness stale lastOkAt={dns.stale ? dns.lastOkAt : doh.lastOkAt} what={t("dns.settingsWord", "Settings")} />
              {t("dns.refreshToEdit", " — refresh before changing anything.")}
            </p>
          )}
        </section>

        {/* ── DoH proxy numbers ── */}
        {doh.data && (form?.mode === "doh" || dohOn) && (
          <Group title={t("dns.dohProxy", "DoH proxy")} stale={doh.stale}>
            <Row
              label={t("dns.proxyState", "Proxy")}
              value={
                <StatusMark tone={doh.data.running ? "ok" : dohOn ? "warn" : "neutral"}>
                  {doh.data.running ? t("common.running", "Running") : t("common.stopped", "Stopped")}
                </StatusMark>
              }
            />
            <Row label={t("dns.queriesTotal", "Queries")} value={st?.queries_total ?? "—"} mono />
            <Row label={t("dns.cacheEntriesRow", "Cache entries")} value={st?.cache_entries ?? "—"} mono />
            <Row
              label={t("dns.cacheHitsMisses", "Cache hits / misses")}
              value={st ? `${st.cache_hits} / ${st.cache_misses}` : "—"}
              mono
            />
            <Row label={t("dns.listenAddr", "Listens on")} value={doh.data.config.listen_addr || "—"} mono />
          </Group>
        )}

        {/* ── DoH cache ── */}
        {dohOn && (
          <section aria-labelledby="doh-cache-title">
            <div className="flex flex-wrap items-center gap-2">
              <div className="flex-1">
                <GroupTitle id="doh-cache-title">{t("dns.dohCache", "DoH Cache")}</GroupTitle>
              </div>
              <span className="nd-aux">{cache.data ? t("dns.entriesCount", "{{count}} entries", { count: entries.length }) : null}</span>
              <Button
                variant="secondary"
                size="sm"
                onPress={() => setAskClear((o) => !o)}
                isDisabled={clearOp.busy || !cache.data || entries.length === 0}
                pending={clearOp.busy}
                {...clearInline.triggerProps}
              >
                <Trash size={18} weight="bold" aria-hidden />
                {t("dns.clearCache", "Clear Cache")}
              </Button>
            </div>
            <ConfirmInline
              id={clearInline.id}
              open={askClear}
              actionLabel={t("dns.clearCache", "Clear Cache")}
              consequence={t("dns.clearConsequence", "Every cached answer is dropped; the next lookups go to the upstream again and are a little slower.")}
              onCancel={() => setAskClear(false)}
              onConfirm={() => {
                setAskClear(false);
                clearOp.start();
                clearOp.confirm();
              }}
            />
            <div className="mb-2 px-1">
              <OpResult op={clearOp} />
            </div>
            <div className={`nd-group max-h-[480px] overflow-y-auto${cache.stale ? " nd-stale" : ""}`}>
              {!cache.data && !cache.error ? (
                <div className="nd-row">
                  <span className="nd-skel" style={{ width: "16ch" }} />
                </div>
              ) : cache.error && !cache.data ? (
                <div className="nd-row flex-wrap">
                  <span className="flex-1 text-nd-t2">{t("dns.cacheUnreadable", "Couldn't read the cache: {{msg}}", { msg: cache.error.message })}</span>
                  <Button variant="secondary" size="sm" onPress={() => cache.mutate()}>
                    {t("common.retry", "Retry")}
                  </Button>
                </div>
              ) : entries.length === 0 ? (
                <div className="nd-row text-nd-t2">{t("dns.cacheEmpty", "The cache is empty. It fills as devices look up names.")}</div>
              ) : (
                entries.map((e, i) => (
                  <Row
                    key={`${e.domain}-${e.type_id}-${i}`}
                    label={<span className="nd-mono break-all">{e.domain || "—"}</span>}
                    sub={e.type || "—"}
                    value={t("dns.ttlValue", "TTL {{s}} s", { s: e.ttl })}
                    mono
                  />
                ))
              )}
            </div>
          </section>
        )}
      </div>

      <ConfirmDialog
        open={ask === 3}
        onOpenChange={(o) => !o && setAsk(null)}
        title={t("dns.confirmTitle", "Change how DNS resolves?")}
        what={applyConsequence}
        downtime={t("dns.downtime", "Name lookups pause for a few seconds while DNS restarts.")}
        recovery={t("dns.recovery", "If sites stop loading, reopen this page by its IP address and switch back to Auto.")}
        actionLabel={t("dns.apply", "Apply")}
        cutsUplink
        onConfirm={go}
      />
    </>
  );
}
