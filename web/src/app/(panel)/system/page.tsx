"use client";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { HubGroups, HubTitle } from "@/components/nd/shell/Hub";
import { LangSwitch } from "@/components/nd/shell/LangSwitch";
import { Group, Row, Segmented } from "@/components/nd";
import { applyTheme, readThemeChoice, type ThemeChoice } from "@/lib/theme";

export default function SystemHub() {
  const { t } = useTranslation();
  const [theme, setTheme] = useState<ThemeChoice>(() => readThemeChoice());
  return (
    <>
      <HubTitle>{t("anchor.system", "System")}</HubTitle>
      {/* Interface settings apply to this browser only (tier 1, no confirm). */}
      <Group title={t("nd.interface", "Interface")}>
        <Row
          label={t("nd.appearance", "Appearance")}
          control={
            <Segmented<ThemeChoice>
              label={t("nd.appearance", "Appearance")}
              value={theme}
              onChange={(v) => {
                setTheme(v);
                applyTheme(v);
              }}
              options={[
                { id: "system", label: t("nd.themeSystem", "System") },
                { id: "light", label: t("nd.themeLight", "Light") },
                { id: "dark", label: t("nd.themeDark", "Dark") },
              ]}
            />
          }
        />
        <Row label={t("nd.language", "Language")} control={<LangSwitch />} />
      </Group>
      <HubGroups anchor="system" />
    </>
  );
}
