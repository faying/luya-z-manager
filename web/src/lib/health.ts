// The rows of the health page, from doctor.sh's checks (GET /api/health).
//
// doctor.sh --tsv puts the device manifest's verdict (id "manifest": what
// `u60 ship` put on the device vs what is there now, touch-ui docs/SHIP.md)
// last, so the existing rows keep their order for every reader. People read
// it first — doctor's own output prints it on top — so it moves to the top
// here; every other row stays in doctor's order.
import type { HealthCheck } from "@/lib/api/schemas/system";
import type { Lang } from "@/lib/i18n/config";
import { pickLang } from "@/lib/i18n/pick";

export type CheckTone = "ok" | "warn" | "bad";

export const LEVEL_TONE: Record<HealthCheck["level"], CheckTone> = { ok: "ok", warn: "warn", bad: "bad" };

export interface HealthRow {
  id: string;
  label: string;
  detail: string;
  tone: CheckTone;
}

export const MANIFEST_ID = "manifest";

export function healthRows(checks: HealthCheck[], lang: Lang = "zh"): HealthRow[] {
  const rows = checks.map((c) => ({
    id: c.id,
    label: pickLang(c, "label", lang),
    detail: pickLang(c, "detail", lang),
    tone: LEVEL_TONE[c.level],
  }));
  const m = rows.findIndex((r) => r.id === MANIFEST_ID);
  if (m <= 0) return rows;
  return [rows[m], ...rows.slice(0, m), ...rows.slice(m + 1)];
}
