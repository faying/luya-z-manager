// Operator logo lookup. Logos are trademarks: the SVGs live in
// public/operator-logos/<slug>.svg, are never published (tools/sync-public.sh
// deletes the directory and empties LOGO_FILES), and every miss falls back to
// a first-letter badge.

/** Slugs that have an SVG in public/operator-logos/. Add a slug here when you add its file. */
export const LOGO_FILES: readonly string[] = [];

/** "MCC-MNC" (3+3 digits) → slug. */
const PLMN_SLUG: Record<string, string> = {
  "460-000": "china-mobile", "460-002": "china-mobile", "460-004": "china-mobile", "460-007": "china-mobile", "460-008": "china-mobile",
  "460-001": "china-unicom", "460-006": "china-unicom", "460-009": "china-unicom",
  "460-003": "china-telecom", "460-005": "china-telecom", "460-011": "china-telecom",
  "460-015": "china-broadnet",
  "454-000": "csl", "454-002": "csl", "454-010": "csl", "454-018": "csl",
  "454-003": "three-hk", "454-004": "three-hk",
  "454-006": "smartone", "454-015": "smartone",
  "454-012": "cmhk", "454-013": "cmhk",
  "466-092": "chunghwa", "466-001": "fetnet", "466-097": "taiwan-mobile",
  "440-010": "docomo", "440-020": "softbank", "440-011": "rakuten",
  "440-050": "au", "440-051": "au", "440-052": "au", "440-053": "au", "440-054": "au",
  "450-005": "skt", "450-008": "kt", "450-006": "lguplus",
  "525-001": "singtel", "525-003": "m1", "525-005": "starhub",
  "310-260": "t-mobile-us", "310-410": "att", "311-480": "verizon",
};

/** "460","1" / "460","01" / "46001" style input → "460-001"; null if not digits. */
export function normalizePlmn(mcc: string | null | undefined, mnc: string | null | undefined): string | null {
  const c = (mcc ?? "").trim();
  const n = (mnc ?? "").trim();
  if (!/^\d{3}$/.test(c) || !/^\d{1,3}$/.test(n)) return null;
  return `${c}-${n.padStart(3, "0")}`;
}

export function logoSlug(mcc: string | null | undefined, mnc: string | null | undefined): string | null {
  const k = normalizePlmn(mcc, mnc);
  return k ? PLMN_SLUG[k] ?? null : null;
}

/** Image path for a slug that has a file, else null (use the badge). */
export function logoSrc(slug: string | null, basePath: string, files: readonly string[] = LOGO_FILES): string | null {
  return slug && files.includes(slug) ? `${basePath}/operator-logos/${slug}.svg` : null;
}

/** Badge text: first character (whole CJK char or first Latin letter, upper-cased). */
export function badgeText(name: string | null | undefined): string {
  const s = (name ?? "").trim();
  if (!s) return "?";
  return Array.from(s)[0].toUpperCase();
}

/** Serving and SIM home differ (by PLMN when both known, else by name). */
export function differsFromHome(
  serving: { mcc: string | null; mnc: string | null; name: string | null } | null | undefined,
  home: { mcc: string | null; mnc: string | null; name: string | null } | null | undefined,
): boolean {
  if (!serving || !home) return false;
  const a = normalizePlmn(serving.mcc, serving.mnc);
  const b = normalizePlmn(home.mcc, home.mnc);
  if (a && b) return a !== b;
  return !!serving.name && !!home.name && serving.name !== home.name;
}
