"use client";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { MagnifyingGlass } from "@phosphor-icons/react";
import { HubGroups, HubTitle } from "@/components/nd/shell/Hub";
import { StatusMark, type Tone } from "@/components/nd";
import { ROUTES, searchRoutes } from "@/lib/routes";
import { usePublicStatus } from "@/lib/publicStatus";

// The six most used functions, as touch-style tiles (design doc §4, 5A).
const TILES = ["/router/wifi", "/services/tailscale", "/router/esim", "/sms", "/bandlock"];

export default function FunctionsHub() {
  const { t } = useTranslation();
  const router = useRouter();
  const { data } = usePublicStatus();
  const [q, setQ] = useState("");

  const state = useMemo(() => {
    const s: Record<string, { tone: Tone; text: ReactNode } | undefined> = {};
    if (!data) return s;
    s["/router/wifi"] = data.wifi?.on ? { tone: "ok", text: t("nd.on", "On") } : { tone: "neutral", text: t("nd.off", "Off") };
    const ts = data.services?.tailscale;
    s["/services/tailscale"] = ts?.running
      ? { tone: "ok", text: t("nd.online", "Online") }
      : ts?.installed
        ? { tone: "warn", text: t("nd.stopped", "Stopped") }
        : { tone: "neutral", text: t("nd.notInstalled", "Not installed") };
    const n = data.sms?.unread ?? 0;
    s["/sms"] = n > 0 ? { tone: "ok", text: t("nd.unread", "{{n}} unread", { n }) } : undefined;
    return s;
  }, [data, t]);

  const hits = q.trim() ? searchRoutes(q, (r) => t(r.tKey, r.fallback)) : [];

  return (
    <>
      <HubTitle>{t("anchor.functions", "Functions")}</HubTitle>
      <form
        role="search"
        className="mb-4"
        onSubmit={(e) => {
          e.preventDefault();
          if (hits[0]) router.push(hits[0].href);
        }}
      >
        <label className="flex min-h-11 items-center gap-3 rounded-nd-field bg-nd-card shadow-[inset_0_0_0_1px_var(--nd-hair)] px-4">
          <MagnifyingGlass size={20} weight="bold" className="text-nd-t3" aria-hidden />
          <input
            type="search"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            aria-label={t("nd.searchPages", "Search pages and settings")}
            placeholder={t("nd.searchPlaceholder", "Search: band lock, APN, SMS…")}
            className="h-11 min-w-0 flex-1 bg-transparent text-[16px] outline-none placeholder:text-nd-t3"
          />
        </label>
        {q.trim() && (
          <ul className="nd-group mt-2" aria-label={t("nd.results", "Results")}>
            {hits.length === 0 ? (
              <li className="nd-row text-nd-t2">{t("nd.noResults", "Nothing found for “{{q}}” · try: band lock, APN, SMS", { q })}</li>
            ) : (
              hits.slice(0, 8).map((r) => (
                <li key={r.href}>
                  <Link href={r.href} className="nd-row">
                    <r.icon size={20} weight="bold" className="nd-row__icon" aria-hidden />
                    <span className="nd-row__label flex-1">{t(r.tKey, r.fallback)}</span>
                  </Link>
                </li>
              ))
            )}
          </ul>
        )}
      </form>

      <section aria-label={t("nd.common", "Common")} className="grid grid-cols-2 gap-3 lg:grid-cols-3">
        {TILES.map((href) => {
          const r = ROUTES.find((x) => x.href === href)!;
          const st = state[href];
          return (
            <Link key={href} href={href} className="nd-module flex min-h-[96px] flex-col justify-between">
              <span className="nd-module__head flex items-center gap-2">
                <r.icon size={24} weight="fill" aria-hidden />
                {t(r.tKey, r.fallback)}
              </span>
              <span className="min-h-5 text-[14px] leading-5">{st && <StatusMark tone={st.tone}>{st.text}</StatusMark>}</span>
            </Link>
          );
        })}
      </section>

      <div className="mt-2">
        <HubGroups anchor="functions" />
      </div>
    </>
  );
}
