"use client";
import { useTranslation } from "react-i18next";
import { HubGroups, HubTitle } from "@/components/nd/shell/Hub";

export default function ChartsHub() {
  const { t } = useTranslation();
  return (
    <>
      <HubTitle>{t("anchor.charts", "Charts")}</HubTitle>
      <HubGroups anchor="charts" />
    </>
  );
}
