// Text the agent words itself (alerts, health, scenarios, operators, exits,
// errors) comes in two languages: the Chinese field as before and an English
// one next to it (docs/designs/ui-english.md §2). The English field is
// missing on an older agent and on rows nobody has worded yet; then the
// Chinese one is shown, never a blank.
//
// Field names are not always `<field>_en` (operator `name` → `operator_en`,
// scenario names → `names_en[id]`), so the base helper takes the two values.
import { useTranslation } from "react-i18next";
import type { Lang } from "./config";

type Text = string | null | undefined;

/** `en` when the page is in English and the agent gave one, else `zh`. */
export function pick(zh: Text, en: Text, lang: Lang): string {
  if (lang === "en" && en) return en;
  return zh ?? "";
}

/** `pick(obj[field], obj[field + "_en"])` — for the fields that follow the naming. */
export function pickLang<T extends object>(obj: T | null | undefined, field: keyof T & string, lang: Lang): string {
  if (!obj) return "";
  const o = obj as Record<string, unknown>;
  const s = (v: unknown) => (typeof v === "string" ? v : null);
  return pick(s(o[field]), s(o[`${field}_en`]), lang);
}

/** The page language as a `Lang` (anything but "zh" is English, like LangSwitch). */
export function useLang(): Lang {
  const { i18n } = useTranslation();
  return i18n.resolvedLanguage === "zh" ? "zh" : "en";
}
