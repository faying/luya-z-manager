"use client";
// Telemetry blocker (new design). Status first (how many of the known ZTE
// telemetry domains are blocked), then quick block, add, and the list.
//
// Writes (controls-inventory §/router/telemetry): every change is a PUT
// /api/router/domain-filter {action:"add"|"delete", domain} — tier 2
// locally, tier 3 over Tailscale (treated like firewall rules), read back
// from GET /api/router/domain-filter.
// 「拦截已知遥测」 sends one PUT per known domain not yet blocked. The old page
// swallowed a failed PUT and still reported success; now each domain is a
// step (R10): a failure shows which were blocked and which weren't, and
// "send the rest" only sends the missing ones. Readback: all four listed.
//
// The GET shape is unconfirmed (ubus passthrough): the old page read
// `blocked_domains[]`; the iOS app parses `rule_list[].domain`. Both are
// accepted, and B27's `{}` (recorded 2026-09-25, nothing blocked) is an empty
// list; any other shape counts as unreadable, not empty.
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Prohibit, Trash } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp, type WriteStep } from "@/lib/api/writeOp";
import type { RouterDomainFilter } from "@/lib/api/schemas/router";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  StatusBlock,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const KNOWN_TELEMETRY_DOMAINS = ["iot.zte.com.cn", "ztems.com", "zte.com.cn", "ztemt.com.cn"];

/** Blocked domains from either reply shape, or null when neither is there. */
function domainsOf(d: unknown): string[] | null {
  if (!d || typeof d !== "object") return null;
  const o = d as Record<string, unknown>;
  if (Array.isArray(o.blocked_domains)) return o.blocked_domains.filter((x): x is string => typeof x === "string");
  if (Array.isArray(o.rule_list)) {
    return o.rule_list
      .map((r) => (r && typeof r === "object" ? (r as Record<string, unknown>).domain : undefined))
      .filter((x): x is string => typeof x === "string" && x !== "");
  }
  if (Object.keys(o).length === 0) return [];
  return null;
}

type Section = "quick" | "add" | "list";

interface Pending {
  section: Section;
  tier: 2 | 3;
  action: string;
  title: string;
  consequence: string;
  steps: WriteStep[];
  check: (list: string[]) => boolean;
  onApplied?: () => void;
  /** Row that opened it (list section). */
  domain?: string;
}

const put = (action: "add" | "delete", domain: string) =>
  apiFetch("/api/router/domain-filter", { method: "PUT", body: { action, domain } });

export default function TelemetryPage() {
  const { t } = useTranslation();
  const api = useApi<RouterDomainFilter>("/api/router/domain-filter");
  const list = api.data === undefined ? undefined : domainsOf(api.data);
  const [newDomain, setNewDomain] = useState("");
  const [addErr, setAddErr] = useState<string | null>(null);

  const [pending, setPending] = useState<Pending | null>(null);
  const [shownAt, setShownAt] = useState<Section | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);
  const [lastAction, setLastAction] = useState<string | null>(null);
  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: pending?.steps ?? [],
    verify: pending
      ? async () => {
          const d = await apiFetch<RouterDomainFilter>("/api/router/domain-filter");
          await api.mutate(d, { revalidate: false });
          const l = domainsOf(d);
          const ok = !!l && pending.check(l);
          if (ok) pending.onApplied?.();
          return ok;
        }
      : undefined,
  });
  const busy = op.busy;
  const locked = !list || api.stale || busy;

  function ask(p: Omit<Pending, "tier">) {
    if (busy) return;
    setPending({ ...p, tier: isRemoteAccess() ? 3 : 2 });
  }
  function go() {
    if (!pending) return;
    setLastAction(pending.action);
    setShownAt(pending.section);
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }
  const trig = (s: Section, domain?: string) =>
    pending?.section === s && pending.tier === 2 && pending.domain === domain ? inline.triggerProps : {};

  const missing = list ? KNOWN_TELEMETRY_DOMAINS.filter((d) => !list.includes(d)) : [];

  function askQuick() {
    if (!list || missing.length === 0) return;
    const todo = [...missing];
    ask({
      section: "quick",
      action: t("telemetry.quickAction", "block {{n}} telemetry domain(s)", { n: todo.length }),
      title: t("telemetry.quickTitle", "Block the known ZTE telemetry domains?"),
      consequence: t("telemetry.cQuick", "Devices behind the U60 can no longer reach {{list}}.", { list: todo.join(", ") }),
      steps: todo.map((d) => ({ label: d, run: () => put("add", d) })),
      check: (l) => KNOWN_TELEMETRY_DOMAINS.every((d) => l.includes(d)),
    });
  }

  function askAdd(e?: FormEvent) {
    e?.preventDefault();
    const d = newDomain.trim().toLowerCase();
    if (!d) {
      setAddErr(t("telemetry.enterDomain", "Enter a domain name"));
      return;
    }
    if (!/^[^\s/:@]+\.[^\s/:@.]+$/.test(d)) {
      setAddErr(t("telemetry.badDomain", "That doesn't look like a domain name, e.g. example.com."));
      return;
    }
    if (list?.includes(d)) {
      setAddErr(t("telemetry.alreadyBlocked", "{{domain}} is already blocked.", { domain: d }));
      return;
    }
    setAddErr(null);
    ask({
      section: "add",
      action: t("telemetry.addAction", "block {{domain}}", { domain: d }),
      title: t("telemetry.addTitle", "Block {{domain}}?", { domain: d }),
      consequence: t("telemetry.cAdd", "Devices behind the U60 can no longer reach {{domain}}.", { domain: d }),
      steps: [{ label: d, run: () => put("add", d) }],
      check: (l) => l.includes(d),
      onApplied: () => setNewDomain(""),
    });
  }

  function askRemove(d: string) {
    ask({
      section: "list",
      domain: d,
      action: t("telemetry.removeAction", "unblock {{domain}}", { domain: d }),
      title: t("telemetry.confirmRemoveNd", "Remove “{{domain}}”?", { domain: d }),
      consequence: t("telemetry.cRemove", "Devices behind the U60 can reach {{domain}} again.", { domain: d }),
      steps: [{ label: d, run: () => put("delete", d) }],
      check: (l) => !l.includes(d),
    });
  }

  function confirmHere(s: Section, domain?: string) {
    return (
      <>
        {pending?.section === s && pending.tier === 2 && pending.domain === domain && (
          <ConfirmInline
            id={inline.id}
            open
            actionLabel={pending.action}
            consequence={pending.consequence}
            onCancel={() => setPending(null)}
            onConfirm={go}
          />
        )}
        {domain === undefined && shownAt === s && op.phase !== "idle" && op.phase !== "confirming" && (
          <div className="mt-2 px-1">
            <OpResult op={op} />
          </div>
        )}
      </>
    );
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: string = t("telemetry.loading", "Reading the block list…");
  let reason: string | null = null;
  if (api.data === undefined && api.error) {
    tone = "bad";
    state = t("telemetry.unreadable", "Can't read the block list");
    reason = api.error.message;
  } else if (api.data !== undefined && !list) {
    tone = "warn";
    state = t("telemetry.unknownShape", "Block list not readable");
    reason = t("telemetry.unknownShapeReason", "The device answered, but without a list this page recognises. Changes are disabled until it can be read.");
  } else if (list) {
    const n = KNOWN_TELEMETRY_DOMAINS.length - missing.length;
    tone = missing.length === 0 ? "ok" : "warn";
    state =
      missing.length === 0
        ? t("telemetry.stAll", "ZTE telemetry blocked")
        : t("telemetry.stSome", "{{n}} of {{total}} known telemetry domains blocked", { n, total: KNOWN_TELEMETRY_DOMAINS.length });
    reason =
      missing.length === 0
        ? t("telemetry.stAllReason", "All {{total}} known domains are on the block list.", { total: KNOWN_TELEMETRY_DOMAINS.length })
        : t("telemetry.stMissing", "Not blocked: {{list}}", { list: missing.join(", ") });
    if (api.stale) tone = "stale";
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("telemetry.title", "Telemetry Blocker")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("telemetry.desc", "Block ZTE telemetry and other unwanted domains.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={list && api.stale ? <Freshness stale lastOkAt={api.lastOkAt} what={t("schedreboot.listWord", "List")} /> : undefined}
          actions={
            api.error || (api.data !== undefined && !list) ? (
              <Button variant="secondary" size="sm" onPress={() => api.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── quick block ── */}
        <section aria-labelledby="tl-quick">
          <GroupTitle id="tl-quick">{t("telemetry.quickBlock", "Quick Block")}</GroupTitle>
          <div className="nd-group grid gap-3 p-4 lg:p-5">
            <p className="nd-body text-nd-t2">
              {t("telemetry.knownTelemetry", "Known ZTE telemetry:")}{" "}
              {KNOWN_TELEMETRY_DOMAINS.map((d, i) => (
                <span key={d}>
                  {i > 0 && ", "}
                  <span className="nd-mono">{d}</span>
                </span>
              ))}
            </p>
            <div className="flex flex-wrap items-center gap-3">
              <span {...trig("quick")}>
                <Button variant="secondary" onPress={askQuick} isDisabled={locked || missing.length === 0}>
                  <Prohibit size={20} weight="bold" aria-hidden />
                  {t("telemetry.blockKnownTelemetry", "Block Known Telemetry")}
                </Button>
              </span>
              {list && missing.length === 0 && (
                <span className="nd-aux">{t("telemetry.allAlreadyBlocked", "All telemetry domains already blocked")}</span>
              )}
            </div>
          </div>
          {confirmHere("quick")}
        </section>

        {/* ── add ── */}
        <section aria-labelledby="tl-add">
          <GroupTitle id="tl-add">{t("telemetry.addDomain", "Add Domain")}</GroupTitle>
          <form className="nd-group grid gap-2 p-4 lg:p-5" onSubmit={askAdd} noValidate>
            <label htmlFor="tl-domain" className="nd-row__label">
              {t("telemetry.domainLabel", "Domain")}
            </label>
            <div className="flex flex-wrap gap-3">
              <input
                id="tl-domain"
                className="nd-field nd-mono min-w-0 flex-1"
                value={newDomain}
                placeholder="example.com"
                disabled={locked}
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
                aria-invalid={addErr ? true : undefined}
                aria-describedby={addErr ? "tl-domain-err" : undefined}
                onChange={(e) => {
                  setNewDomain(e.target.value);
                  setAddErr(null);
                }}
              />
              <span {...trig("add")}>
                <Button type="submit" isDisabled={locked}>
                  {t("telemetry.add", "Add")}
                </Button>
              </span>
            </div>
            {addErr && (
              <span id="tl-domain-err" className="nd-aux text-nd-badT" role="alert">
                {addErr}
              </span>
            )}
          </form>
          {confirmHere("add")}
        </section>

        {/* ── list ── */}
        <section aria-labelledby="tl-list">
          <GroupTitle id="tl-list">{t("telemetry.blockedDomains", "Blocked Domains")}</GroupTitle>
          <div className={`nd-group${api.stale ? " nd-stale" : ""}`}>
            {list === undefined ? (
              [0, 1, 2].map((i) => (
                <div key={i} className="nd-row">
                  <span className="nd-skel" style={{ width: "16ch" }} />
                </div>
              ))
            ) : list === null ? (
              <div className="nd-row">
                <span className="nd-body text-nd-t2">{t("telemetry.listUnknown", "The list can't be shown: the device's reply has no domain list.")}</span>
              </div>
            ) : list.length === 0 ? (
              <div className="nd-row">
                <span className="nd-body text-nd-t2">
                  {t("telemetry.noDomainsBlockedNd", "No domains blocked. Use “Block Known Telemetry” or add one above.")}
                </span>
              </div>
            ) : (
              list.map((d) => (
                <div key={d} className="border-b border-[var(--nd-sep)] last:border-0">
                  <div className="nd-row">
                    <span className="nd-row__text">
                      <span className="nd-row__label nd-mono break-all">{d}</span>
                      {KNOWN_TELEMETRY_DOMAINS.includes(d) && (
                        <span className="nd-row__sub block">{t("telemetry.knownTag", "Known ZTE telemetry")}</span>
                      )}
                    </span>
                    <span {...trig("list", d)}>
                      <Button
                        variant="ghost"
                        iconOnly
                        aria-label={t("telemetry.removeDomainNamed", "Remove {{domain}}", { domain: d })}
                        onPress={() => askRemove(d)}
                        isDisabled={locked}
                      >
                        <Trash size={20} weight="bold" aria-hidden />
                      </Button>
                    </span>
                  </div>
                  {pending?.section === "list" && pending.domain === d && <div className="px-4 pb-3">{confirmHere("list", d)}</div>}
                </div>
              ))
            )}
          </div>
          {shownAt === "list" && op.phase !== "idle" && op.phase !== "confirming" && (
            <div className="mt-2 grid gap-1 px-1">
              {lastAction && <span className="nd-aux">{lastAction}</span>}
              <OpResult op={op} />
            </div>
          )}
          {api.stale && list && (
            <p className="nd-aux mt-2 px-1">
              <Freshness stale lastOkAt={api.lastOkAt} what={t("schedreboot.listWord", "List")} />
              {t("wifi.refreshToEdit", " — refresh before changing anything.")}{" "}
              <Button variant="secondary" size="sm" onPress={() => api.mutate()}>
                {t("wifi.refresh", "Refresh")}
              </Button>
            </p>
          )}
        </section>
      </div>

      {pending?.tier === 3 && (
        <ConfirmDialog
          open
          onOpenChange={(o) => !o && setPending(null)}
          title={pending.title}
          what={pending.consequence}
          downtime={t("telemetry.downtime", "None; the filter reloads in a moment.")}
          recovery={t("telemetry.recovery", "Undo it on this page. If this page stops loading over Tailscale, someone on the U60's Wi-Fi can change it.")}
          actionLabel={t("nd.confirmAction", "Confirm: {{action}}", { action: pending.action })}
          cutsUplink
          onConfirm={go}
        />
      )}
    </>
  );
}
