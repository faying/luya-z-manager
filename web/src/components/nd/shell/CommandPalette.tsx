"use client";
// ⌘K / Ctrl+K search over the route table (lib/routes.ts). Combobox
// pattern: the input keeps focus, arrows move the active option, Enter
// opens it. Opening moves focus into the dialog; closing returns it to
// whatever opened it (React Aria's modal does both).
import { Modal } from "@heroui/react";
import { useRouter } from "next/navigation";
import { useEffect, useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { MagnifyingGlass } from "@phosphor-icons/react";
import { ROUTES, searchRoutes, type RouteDef } from "@/lib/routes";

const SUGGESTED = ["/bandlock", "/router/wifi", "/router/esim", "/sms", "/router/apn", "/services/tailscale"];

export function CommandPalette({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const { t } = useTranslation();
  const router = useRouter();
  const id = useId();
  const [q, setQ] = useState("");
  const [active, setActive] = useState(0);
  const title = (r: RouteDef) => t(r.tKey, r.fallback);

  const results = useMemo(
    () => (q.trim() ? searchRoutes(q, title) : SUGGESTED.map((h) => ROUTES.find((r) => r.href === h)!)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [q, t],
  );
  useEffect(() => setActive(0), [q]);
  useEffect(() => {
    if (!open) setQ("");
  }, [open]);

  function go(r: RouteDef | undefined) {
    if (!r) return;
    onOpenChange(false);
    router.push(r.href);
  }

  return (
    <Modal.Backdrop isOpen={open} onOpenChange={onOpenChange}>
      <Modal.Container placement="top" className="sm:mt-[12vh]">
        <Modal.Dialog className="nd nd-dialog w-full max-w-[560px] p-0" aria-label={t("nd.search", "Search")}>
          <div className="flex items-center gap-3 border-b border-nd-sep px-4">
            <MagnifyingGlass size={20} weight="bold" className="text-nd-t3" aria-hidden />
            <input
              autoFocus
              role="combobox"
              aria-expanded
              aria-controls={`${id}-list`}
              aria-activedescendant={results[active] ? `${id}-${active}` : undefined}
              aria-autocomplete="list"
              aria-label={t("nd.searchPages", "Search pages and settings")}
              placeholder={t("nd.searchPlaceholder", "Search: band lock, APN, SMS…")}
              value={q}
              onChange={(e) => setQ(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "ArrowDown") { e.preventDefault(); setActive((a) => Math.min(a + 1, results.length - 1)); }
                else if (e.key === "ArrowUp") { e.preventDefault(); setActive((a) => Math.max(a - 1, 0)); }
                else if (e.key === "Enter") { e.preventDefault(); go(results[active]); }
              }}
              className="h-14 min-w-0 flex-1 bg-transparent text-[16px] text-nd-t1 outline-none placeholder:text-nd-t3"
            />
          </div>
          {results.length > 0 ? (
            <ul id={`${id}-list`} role="listbox" aria-label={t("nd.results", "Results")} className="max-h-[60dvh] overflow-y-auto p-2">
              {!q.trim() && <li role="presentation" className="nd-aux px-3 pb-1 pt-2">{t("nd.suggested", "Common")}</li>}
              {results.map((r, i) => {
                const I = r.icon;
                return (
                  <li
                    key={r.href}
                    id={`${id}-${i}`}
                    role="option"
                    aria-selected={i === active}
                    onMouseMove={() => setActive(i)}
                    onClick={() => go(r)}
                    className={`flex min-h-12 cursor-pointer items-center gap-3 rounded-nd-field px-3 ${i === active ? "bg-nd-accS text-nd-t1" : "text-nd-t1"}`}
                  >
                    <I size={20} weight={i === active ? "fill" : "bold"} aria-hidden />
                    <span className="flex-1 font-semibold">{title(r)}</span>
                    <span className="nd-mono text-[13px] text-nd-t3">{r.href}</span>
                  </li>
                );
              })}
            </ul>
          ) : (
            <p className="px-5 py-6 text-[15px] text-nd-t2" role="status">
              {t("nd.noResults", "Nothing found for “{{q}}” · try: band lock, APN, SMS", { q })}
            </p>
          )}
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  );
}
