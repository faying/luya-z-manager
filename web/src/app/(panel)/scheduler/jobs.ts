// Helpers shared by /scheduler and /router/schedule (new design).
import type { TFunction } from "i18next";
import { fmtDevice } from "@/lib/deviceClock";
import type { SchedulerAction, SchedulerSchedule } from "@/lib/api/schemas/system";

// Index = the agent's day number: 0 = Monday … 6 = Sunday (scheduler.rs:88,
// inventory §9 1a1fc45). Keep this order.
export const DAYS_EN = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const DAY_KEYS = ["dayMon", "dayTue", "dayWed", "dayThu", "dayFri", "daySat", "daySun"];

export function dayLabel(t: TFunction, i: number): string {
  return t(`scheduler.${DAY_KEYS[i]}`, DAYS_EN[i]);
}

export function fmtSchedule(t: TFunction, s: SchedulerSchedule): string {
  if (s.type === "once") {
    return s.at ? t("scheduler.onceAtTime", "Once at {{time}}", { time: fmtDevice(s.at) }) : t("scheduler.once", "Once");
  }
  const days = s.days ?? [];
  const dayStr =
    days.length === 0 || days.length === 7 ? t("scheduler.daily", "daily") : days.map((d) => dayLabel(t, d)).join(", ");
  return `${s.time ?? "?"} — ${dayStr}`;
}

/**
 * Writes that are tier 3 when done by hand (design §3.1). A job that sends
 * one of these later is just as consequential, so creating, editing or
 * switching on such a job is tier 3 too. Prefix match on the path; GET is
 * never tier 3.
 */
const TIER3_PREFIXES = [
  "/api/device/reboot",
  "/api/device/factory-reset",
  "/api/modem/network-mode",
  "/api/modem/airplane",
  "/api/cell/band/",
  "/api/cell/lock/",
  "/api/cell/stc/reset",
  "/api/router/apn/",
  "/api/esim/switch",
  "/api/esim/delete",
  "/api/usb/mode",
  "/api/system/kill",
  "/api/sim/pin/",
  "/api/sim/unlock",
  "/api/at/send",
  "/api/sms/delete",
  "/api/sms/forward/log/clear",
];

export function isTier3Action(a: Pick<SchedulerAction, "method" | "path">): boolean {
  if (a.method.toUpperCase() === "GET") return false;
  const p = a.path.trim().split("?")[0];
  if (p === "/api/sms/forward/rules" && a.method.toUpperCase() === "DELETE") return true;
  return TIER3_PREFIXES.some((x) => p.startsWith(x));
}

export const isRebootJob = (a: SchedulerAction) => a.method.toUpperCase() === "POST" && a.path.trim() === "/api/device/reboot";
