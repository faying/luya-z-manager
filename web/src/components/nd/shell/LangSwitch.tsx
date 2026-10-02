"use client";
// 中 / EN switch (was admin/LangToggle), as a small segmented control.
import { useTranslation } from "react-i18next";
import { setLang, type Lang } from "@/lib/i18n/config";
import { Segmented } from "../Segmented";

export function LangSwitch() {
  const { i18n, t } = useTranslation();
  const cur: Lang = i18n.resolvedLanguage === "zh" ? "zh" : "en";
  return (
    <Segmented<Lang>
      label={t("nd.language", "Language")}
      value={cur}
      onChange={(l) => setLang(l)}
      options={[
        // Each language under its own name, in its own language (lang="zh" for screen readers and the English-page check).
        { id: "zh", label: <span lang="zh">中</span> },
        { id: "en", label: "EN" },
      ]}
    />
  );
}
