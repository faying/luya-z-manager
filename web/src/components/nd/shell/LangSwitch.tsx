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
        { id: "zh", label: "中" },
        { id: "en", label: "EN" },
      ]}
    />
  );
}
