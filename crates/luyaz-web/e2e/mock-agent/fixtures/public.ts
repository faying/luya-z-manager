// GET /api/public/status — luyaz-core api/status.rs public_status. Summary that
// needs no login (top bar, touch screen, device clock). Built from the same nwinfo payload as
// /api/network/signal (fixtures/network.ts) plus cross-area shared state.

import type { Ctx, Route } from "../lib.ts";
import { ok } from "../lib.ts";
import { shared } from "../shared.ts";
import { buildNetinfo, wwanConnected, DEVICE_UTC_OFFSET } from "./network.ts";
import { mockFinding } from "./diagnose.ts";
import type { PublicStatus } from "../../../src/lib/api/schemas/public.ts";

/** Wi-Fi SSID (main2g_ssid). No shared field; keep in step with fixtures/wifi.ts if it differs. */
const SSID = "U60-Pro";
/** Tailscale IPv4 of this node (first column of `tailscale status`). */
const TS_IP = "100.101.7.23";
/** Last scenario switch: 2026-09-22 18:40 on the device clock (local time labelled UTC). */
const LAST_SWITCH = Date.UTC(2026, 8, 22, 18, 40, 12) / 1000;

/** CHILL's manual-first summary (luyaz-core engines/manual_first.rs) under scenario mf-alldown: the hand-picked node and the backup are both down. */
export const MF_ALLDOWN = {
  notice: "JP 03 和 🇯🇵 日本 备用都不通",
  notice_en: "JP 03 and the 🇯🇵 日本 fallback are both down",
  notice_at: Date.UTC(2026, 9, 2, 14, 20, 0) / 1000,
  notice_kind: "alldown" as const,
};

/** the core's home verdict as the netwatch engine passes it on (story.state). */
function verdict(ctx: Ctx): string {
  if (ctx.has("verdict-stall")) return "stall";
  if (ctx.has("verdict-crowd")) return "crowd";
  if (ctx.has("nosignal") || shared.airplane) return shared.airplane ? "airplane" : "nosvc";
  if (ctx.has("weak")) return "weak";
  return "ok";
}

function publicStatus(ctx: Ctx): PublicStatus {
  const net = buildNetinfo(ctx);
  const nettype = net.network_type ?? "";
  const operator = net.network_provider_fullname || net.network_provider || "";
  const barN = parseInt(net.signalbar ?? "", 10);
  const rsrp = (nettype === "LTE" ? net.lte_rsrp : net.nr5g_rsrp) ?? 0;
  const chillState = shared.chill.state;

  return {
    network: {
      connected: wwanConnected(ctx),
      type: nettype,
      operator,
      bar: Number.isFinite(barN) ? barN : -1,
      rsrp: Number.isInteger(rsrp) ? rsrp : Math.trunc(rsrp),
      verdict: verdict(ctx),
      finding: (() => {
        const f = mockFinding(ctx);
        return f && f.confidence === "sure"
          ? { kind: f.kind, layer: f.layer, symptom: f.symptom, symptom_en: f.symptom_en }
          : null;
      })(),
    },
    wifi: { on: shared.wifiOn, ssid: SSID },
    battery: { percent: 76, charging: true },
    sms: { unread: shared.sms.unread },
    services: {
      tailscale: {
        running: shared.tailscale.running,
        installed: true,
        node: shared.tailscale.running ? `${TS_IP} ${shared.tailscale.node}` : "",
      },
      chill: {
        state: chillState,
        reason: null,
        // Owner's switch (/data/chill/disabled absent): off only when stopped on purpose.
        on: chillState !== "stopped",
        ...(ctx.has("mf-alldown") && chillState === "running" ? MF_ALLDOWN : {}),
      },
    },
    scenario: {
      configured: true,
      enabled: true,
      current: "abroad",
      name: "国外",
      name_en: "Abroad",
      wifi_off: false,
      abroad: true,
      chill_on_when_home: false,
      auto_direct: shared.chill.exit === "direct",
      pin: null,
      guard_takeover: false,
      // firmware-b27 exercises a scenario that has never switched (null).
      last_switch: ctx.has("firmware-b27") ? null : LAST_SWITCH,
    },
    alerts: { unread: shared.alertsUnread },
    clock: { utc_offset: DEVICE_UTC_OFFSET },
    health: { checked: true, bad: 0, warn: 1 },
    device: { model: "MU5250", name: "U60 Pro" },
  };
}

export const routes: Route[] = [{ method: "GET", path: "/api/public/status", handler: (ctx) => ok(publicStatus(ctx)) }];
