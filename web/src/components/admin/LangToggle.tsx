"use client";

import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { setLang, type Lang } from "@/lib/i18n/config";

/** Segmented 中 / EN language switch for the header. */
export function LangToggle() {
  const { i18n } = useTranslation();
  const cur: Lang = i18n.resolvedLanguage === "zh" ? "zh" : "en";
  const opts: { lng: Lang; label: string }[] = [
    { lng: "zh", label: "中" },
    { lng: "en", label: "EN" },
  ];
  return (
    <div
      role="group"
      aria-label="Language"
      className="flex items-center rounded-md border border-border p-0.5"
    >
      {opts.map((o) => {
        const active = cur === o.lng;
        return (
          <button
            key={o.lng}
            type="button"
            onClick={() => setLang(o.lng)}
            aria-pressed={active}
            className={cn(
              "rounded-[5px] px-1.5 py-0.5 text-[11px] font-semibold leading-none transition-colors",
              active ? "bg-accent-soft text-accent" : "text-text-dim hover:text-text"
            )}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
