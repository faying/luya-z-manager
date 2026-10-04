"use client";
// "Last changed 10-03 14:32 · Screen ›" under a setting (E4 T9b, DD5): who
// last wrote this item through datad and when (journal.list `owners`),
// linking to that change in the change log. Nothing when datad has no owner
// for it (never changed, or an older datad).
import Link from "next/link";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { useOps } from "@/lib/hooks/useOps";
import { useLang } from "@/lib/i18n/pick";
import { JOURNAL_PATH, shortWhen, sourceName, type Journal } from "@/lib/ops";

export function LastChange({ item }: { item: string }) {
  const { t } = useTranslation();
  const lang = useLang();
  // an older datad has no change log: don't ask
  const j = useApi<Journal>(useOps().supported ? JOURNAL_PATH : null, { refreshInterval: 30000 });
  const o = j.data?.owners?.[item];
  if (!o) return null;
  const href = o.op_id ? `/changes/?op=${encodeURIComponent(o.op_id)}` : "/changes/";
  return (
    <p className="nd-aux px-1" data-testid="last-change">
      <Link href={href} className="inline-flex min-h-11 items-center font-medium text-nd-accT underline-offset-4 hover:underline">
        {t("changes.lastChange", "Last changed {{when}} · {{source}}", { when: shortWhen(o.t), source: sourceName(o.source, lang) })} ›
      </Link>
    </p>
  );
}
