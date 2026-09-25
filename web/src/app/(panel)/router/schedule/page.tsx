"use client";
// Scheduled reboot (new design). There is no reboot-schedule endpoint
// (/api/modem/schedule-reboot does not exist): a scheduled reboot is a
// scheduler job that sends POST /api/device/reboot. This page explains that,
// shows the reboot jobs that exist (read-only), and links to /scheduler,
// where creating or switching on such a job is a tier-3 confirm.
import Link from "next/link";
import { useTranslation } from "react-i18next";
import { CalendarBlank, CaretRight } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import type { SchedulerJob } from "@/lib/api/schemas/system";
import { Button, Freshness, GroupTitle, Row, StatusBlock, StatusMark, type Tone } from "@/components/nd";
import { fmtSchedule, isRebootJob } from "../../scheduler/jobs";

export default function ScheduleRebootPage() {
  const { t } = useTranslation();
  const jobs = useApi<SchedulerJob[]>("/api/scheduler/jobs");
  const reboots = Array.isArray(jobs.data) ? jobs.data.filter((j) => isRebootJob(j.action)) : null;
  const on = reboots?.filter((j) => j.enabled) ?? [];

  let tone: Tone = "neutral";
  let state: string = t("schedreboot.loading", "Reading scheduled jobs…");
  let reason: string | null = null;
  if (!reboots && jobs.error) {
    tone = "bad";
    state = t("schedreboot.unreadable", "Can't read the scheduled jobs");
    reason = jobs.error.message;
  } else if (reboots) {
    if (on.length > 0) {
      tone = "ok";
      state = t("schedreboot.stOn", "Scheduled reboot on");
      reason = on.map((j) => fmtSchedule(t, j.schedule)).join(" · ");
    } else {
      state = t("schedreboot.stOff", "No scheduled reboot");
      reason =
        reboots.length > 0
          ? t("schedreboot.stOffSome", "{{n}} reboot job(s) exist but are switched off.", { n: reboots.length })
          : t("schedreboot.stNone", "Create one in the scheduler if you want the device to restart on a timetable.");
    }
    if (jobs.stale) tone = "stale";
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("schedreboot.title", "Schedule Reboot")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("schedreboot.desc", "Set up recurring reboots for your router.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={reboots && jobs.stale ? <Freshness stale lastOkAt={jobs.lastOkAt} what={t("schedreboot.listWord", "List")} /> : undefined}
          actions={
            jobs.error ? (
              <Button variant="secondary" size="sm" onPress={() => jobs.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {reboots && reboots.length > 0 && (
          <section aria-labelledby="sr-jobs">
            <GroupTitle id="sr-jobs">{t("schedreboot.jobsTitle", "Reboot jobs")}</GroupTitle>
            <div className={`nd-group${jobs.stale ? " nd-stale" : ""}`}>
              {reboots.map((j) => (
                <Row
                  key={j.id}
                  label={j.name || t("schedreboot.unnamed", "(no name)")}
                  sub={fmtSchedule(t, j.schedule)}
                  value={
                    <StatusMark tone={j.enabled ? "ok" : "neutral"}>
                      {j.enabled ? t("nd.on", "On") : t("nd.off", "Off")}
                    </StatusMark>
                  }
                  href="/scheduler"
                />
              ))}
            </div>
          </section>
        )}

        <section aria-labelledby="sr-how">
          <GroupTitle id="sr-how">{t("schedreboot.cardTitle", "Use the Scheduler")}</GroupTitle>
          <div className="nd-group grid gap-3 p-4 lg:p-5">
            <p className="nd-body text-nd-t2">
              {t(
                "schedreboot.intro1",
                "Recurring scheduled reboots are managed through the Scheduler. The Scheduler creates cron jobs that call"
              )}{" "}
              <code className="nd-mono">POST /api/device/reboot</code>
              {t("schedreboot.intro2", " on your chosen schedule.")}
            </p>
            <p className="nd-body text-nd-t2">
              {t("schedreboot.gotoBefore", "Go to the")} <strong>{t("scheduler.title", "Scheduler")}</strong>{" "}
              {t("schedreboot.gotoAfter", "page to create or manage reboot jobs.")}
            </p>
            <div>
              <Link href="/scheduler" className="nd-btn nd-btn--primary">
                <CalendarBlank size={20} weight="bold" aria-hidden />
                {t("schedreboot.openScheduler", "Open Scheduler")}
                <CaretRight size={16} weight="bold" aria-hidden />
              </Link>
            </div>
          </div>
        </section>
      </div>
    </>
  );
}
