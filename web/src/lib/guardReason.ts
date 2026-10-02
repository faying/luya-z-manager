// Why the manual-register guard went back to automatic (netinfo.rs Guard), for
// the mobile-network page.
import type { NetInfoGuard } from "@/lib/api/schemas/network";
import type { Lang } from "@/lib/i18n/config";
import { pick } from "@/lib/i18n/pick";

/**
 * The reason to show, or null for a plain "back to automatic on request"
 * (the page says that in its own words). Compares `reason_code`; an agent
 * from before 2026-10-01 sends none, so then its Chinese text.
 */
export function guardReason(g: Pick<NetInfoGuard, "reason" | "reason_code" | "reason_en">, lang: Lang): string | null {
  if (!g.reason) return null;
  const plain = g.reason_code ? g.reason_code === "manual_auto" : g.reason === "手动恢复自动";
  return plain ? null : pick(g.reason, g.reason_en, lang);
}
