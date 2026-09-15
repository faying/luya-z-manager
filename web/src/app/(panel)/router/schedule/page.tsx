"use client";

import Link from "next/link";
import { PageHeader, SectionCard } from "@/components/admin/StatCard";
import { Calendar } from "lucide-react";
import { useTranslation } from "react-i18next";

// NOTE: /api/modem/schedule-reboot does not exist (404).
// Recurring reboot scheduling is done via /api/scheduler/jobs (POST with action POST /api/device/reboot).
// The Scheduler page at /admin/scheduler handles this workflow.

export default function ScheduleRebootPage() {
  const { t } = useTranslation();
  return (
    <>
      <PageHeader
        title={t("schedreboot.title", "Schedule Reboot")}
        description={t("schedreboot.desc", "Set up recurring reboots for your router.")}
      />

      <SectionCard title={t("schedreboot.cardTitle", "Use the Scheduler")}>
        <div className="flex flex-col gap-4 py-2">
          <p className="text-sm text-text-dim">
            {t("schedreboot.intro1", "Recurring scheduled reboots are managed through the Scheduler. The Scheduler creates cron jobs that call")}{" "}
            <code className="rounded bg-bg-elevated px-1 font-mono text-xs">POST /api/device/reboot</code>{" "}
            {t("schedreboot.intro2", "on your chosen schedule.")}
          </p>
          <p className="text-sm text-text-dim">
            {t("schedreboot.gotoBefore", "Go to the")} <strong>Scheduler</strong> {t("schedreboot.gotoAfter", "page to create or manage reboot jobs.")}
          </p>
          <div>
            <Link
              href="/scheduler"
              className="inline-flex items-center gap-2 rounded-lg border border-accent bg-accent/10 px-4 py-2 text-sm font-medium text-accent transition hover:bg-accent/20"
            >
              <Calendar className="h-4 w-4" />
              {t("schedreboot.openScheduler", "Open Scheduler")}
            </Link>
          </div>
        </div>
      </SectionCard>
    </>
  );
}
