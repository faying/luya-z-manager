// Words for signal readings, shared by the home and signal pages so both
// (and the touchscreen) name the same levels the same way.
export type T = (k: string, d: string, o?: Record<string, unknown>) => string;

export function carrierSummary(t: T, c: { nr: number; lte: number }) {
  if (c.nr && c.lte) return t("home.ccBoth", "{{lte}} LTE + {{nr}} NR carriers", { lte: c.lte, nr: c.nr });
  if (c.nr) return t("home.ccNr", "{{n}} NR carriers", { n: c.nr });
  if (c.lte) return t("home.ccLte", "{{n}} LTE carriers", { n: c.lte });
  return t("home.noCarriers", "No active carriers");
}

export function radioName(type?: string) {
  const u = (type ?? "").toUpperCase();
  if (u === "SA") return "5G SA";
  if (u === "NSA") return "5G NSA";
  if (u === "LTE") return "4G LTE";
  return type || null;
}

export function rsrpWord(t: T, v: number | null) {
  if (v == null) return t("home.noSignalWord", "No signal");
  if (v >= -100) return t("home.good", "Good");
  if (v >= -110) return t("home.fair", "Fair");
  return t("home.weak", "Weak");
}

export function sinrWord(t: T, v: number | null) {
  if (v == null) return t("home.noSignalWord", "No signal");
  if (v >= 13) return t("home.good", "Good");
  if (v >= 0) return t("home.fair", "Fair");
  return t("home.weak", "Weak");
}


export function rsrqWord(t: T, v: number | null) {
  if (v == null) return t("home.noSignalWord", "No signal");
  if (v >= -10) return t("home.good", "Good");
  if (v >= -15) return t("home.fair", "Fair");
  return t("home.weak", "Weak");
}
