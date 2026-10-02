// Shapes and wording for /api/alerts. Events are written on the device by the
// process supervisor and the Wi-Fi watchdog (u60-guard), which is also the
// only thing that sends alert SMS — see docs/RELIABILITY.md.

import type { TFunction } from "i18next";
import type { Lang } from "@/lib/i18n/config";
import { pick } from "@/lib/i18n/pick";

export interface AlertEvent {
  seq: number;
  /** Unix seconds, or null when the device clock was not set yet. */
  time: number | null;
  /** Seconds since boot when it happened. */
  uptime: number;
  kind: string;
  label?: string | null;
  label_en?: string | null;
  text: string;
  unread: boolean;
}

export interface SmsRecord {
  time: number;
  seq: number;
  kind: string;
  /** Agent 2026-10-02 and later. */
  label?: string | null;
  label_en?: string | null;
  result: string;
}

export interface AlertsData {
  events: AlertEvent[];
  unread: number;
  read: number;
  uptime_now: number | null;
  clock_set: boolean;
  sms: {
    configured: boolean;
    number: string | null;
    abroad_allowed: boolean;
    abroad_now: boolean;
    processed: number;
    sent_24h: number;
    recent: SmsRecord[];
  };
}

export const ALERTS_PATH = "/api/alerts";

/** Something with an alert kind and, from the agent, its wording. */
export interface Labelled {
  kind: string;
  /** alerts.rs kind_label — the touch screen shows the same words. */
  label?: string | null;
  label_en?: string | null;
}

/**
 * What an alert or alert-SMS record means, in the page language, as the agent
 * words it (L2 review R8: one copy, in the agent). An agent from before
 * 2026-10-02 sends no label on SMS records: borrow one from an event of the
 * same kind, else show the kind itself.
 */
export function alertLabel(r: Labelled, lang: Lang, events: readonly Labelled[] = []): string {
  const has = (x: Labelled) => !!(x.label || x.label_en);
  const src = has(r) ? r : events.find((e) => e.kind === r.kind && has(e));
  return src ? pick(src.label, src.label_en, lang) || r.kind : r.kind;
}

export function smsResultLabel(t: TFunction, result: string): string {
  switch (result) {
    case "sent":
      return t("alerts.smsResultSent", "Sent");
    case "failed":
      return t("alerts.smsResultFailed", "Failed");
    case "suppressed-rate":
      return t("alerts.smsResultRate", "Not sent — over the limit");
    case "suppressed-abroad":
      return t("alerts.smsResultAbroad", "Not sent — abroad");
    case "no-number":
      return t("alerts.smsResultNoNumber", "Not sent — no number set");
    default:
      return result;
  }
}

function pad(n: number): string {
  return n < 10 ? `0${n}` : `${n}`;
}

/** Device-local "MM-DD HH:mm", or "N min after boot" when the clock was not set.
 * UTC getters on purpose: device epochs carry local digits (lib/deviceClock.ts). */
export function eventTime(t: TFunction, e: Pick<AlertEvent, "time" | "uptime">): string {
  if (e.time != null) {
    const d = new Date(e.time * 1000);
    return `${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`;
  }
  return t("alerts.afterBoot", "{{m}} min after boot", { m: Math.floor(e.uptime / 60) });
}
