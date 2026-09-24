// Shapes and wording for /api/alerts. Events are written on the device by the
// process supervisor and the Wi-Fi watchdog (u60-guard), which is also the
// only thing that sends alert SMS — see docs/RELIABILITY.md.

import type { TFunction } from "i18next";

export interface AlertEvent {
  seq: number;
  /** Unix seconds, or null when the device clock was not set yet. */
  time: number | null;
  /** Seconds since boot when it happened. */
  uptime: number;
  kind: string;
  text: string;
  unread: boolean;
}

export interface SmsRecord {
  time: number;
  seq: number;
  kind: string;
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

export function kindLabel(t: TFunction, kind: string): string {
  switch (kind) {
    case "agent-crash":
      return t("alerts.kindAgentCrash", "Admin backend (zte-agent) exited unexpectedly");
    case "agent-silent":
      return t("alerts.kindAgentSilent", "Admin backend (zte-agent) stopped responding");
    case "agent-hung":
      return t("alerts.kindAgentHung", "Admin backend was stuck and was restarted");
    case "devui-crash":
      return t("alerts.kindDevuiCrash", "Touch screen UI exited unexpectedly");
    case "devui-gave-up":
      return t("alerts.kindDevuiGaveUp", "Touch screen UI kept failing; stock UI is on screen (long-press the bottom-right corner to retry)");
    case "devui-theme-paused":
      return t("alerts.kindDevuiThemePaused", "Automatic light/dark switching on the touch screen paused until reboot");
    case "datad-crash":
      return t("alerts.kindDatadCrash", "Data service (zwrt-datad) exited unexpectedly");
    case "wifi-takeover":
      return t("alerts.kindWifiTakeover", "Wi-Fi watchdog turned Wi-Fi back on");
    case "wifi-restore-failed":
      return t("alerts.kindWifiRestoreFailed", "Wi-Fi watchdog could not turn Wi-Fi on");
    case "sms-test":
      return t("alerts.kindSmsTest", "Test SMS");
    case "sms-failed":
      return t("alerts.kindSmsFailed", "Alert SMS could not be sent");
    default:
      return kind;
  }
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
