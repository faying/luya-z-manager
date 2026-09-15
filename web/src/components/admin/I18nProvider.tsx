"use client";

import { useEffect, useState } from "react";
import { I18nextProvider } from "react-i18next";
import i18n, { detectLang, setLang } from "@/lib/i18n/config";

/**
 * Wraps the app in the i18next context and, once mounted on the client,
 * switches from the deterministic "en" prerender language to the saved /
 * browser-detected language. Doing the switch in an effect (not at init)
 * avoids a hydration mismatch with the static export.
 */
export function I18nProvider({ children }: { children: React.ReactNode }) {
  const [, force] = useState(0);
  useEffect(() => {
    const target = detectLang();
    if (target !== i18n.language) setLang(target);
    else if (typeof document !== "undefined") document.documentElement.lang = target;
    const onChange = () => force((n) => n + 1);
    i18n.on("languageChanged", onChange);
    return () => i18n.off("languageChanged", onChange);
  }, []);
  return <I18nextProvider i18n={i18n}>{children}</I18nextProvider>;
}
