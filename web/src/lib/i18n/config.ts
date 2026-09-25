import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import { en } from "./en";
import { zh } from "./zh";
import { deepMerge, ND_ZH } from "./nd-zh";

export const SUPPORTED = ["en", "zh"] as const;
export type Lang = (typeof SUPPORTED)[number];
export const LANG_STORAGE_KEY = "u60_lang";

// Initialize synchronously with bundled resources. We deliberately pin lng to
// "en" here so the static-export prerender AND the first client render agree
// (no hydration mismatch); the provider switches to the detected/saved
// language in a mount effect. useSuspense:false keeps render synchronous.
if (!i18n.isInitialized) {
  i18n.use(initReactI18next).init({
    resources: {
      en: { translation: en },
      zh: { translation: deepMerge(zh as never, ...ND_ZH) },
    },
    lng: "en",
    fallbackLng: "en",
    supportedLngs: SUPPORTED as unknown as string[],
    interpolation: { escapeValue: false },
    react: { useSuspense: false },
  });
}

/** Detect the initial language: saved choice → browser → English. */
export function detectLang(): Lang {
  if (typeof window === "undefined") return "en";
  try {
    const saved = window.localStorage.getItem(LANG_STORAGE_KEY);
    if (saved === "en" || saved === "zh") return saved;
  } catch {
    /* ignore */
  }
  const nav = (navigator.language || "").toLowerCase();
  return nav.startsWith("zh") ? "zh" : "en";
}

export function setLang(lng: Lang) {
  i18n.changeLanguage(lng);
  try {
    window.localStorage.setItem(LANG_STORAGE_KEY, lng);
  } catch {
    /* ignore */
  }
  if (typeof document !== "undefined") document.documentElement.lang = lng;
}

export default i18n;
