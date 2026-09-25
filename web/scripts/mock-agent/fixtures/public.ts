// GET /api/public/status — public.rs:29. Unauthenticated summary for the
// login screen, the header status strip, the dashboard services card and
// lib/deviceClock.ts. Built from the same nwinfo payload as
// /api/network/signal (fixtures/network.ts) plus cross-area shared state.

import type { Ctx, Route } from "../lib.ts";
import { ok } from "../lib.ts";
import { shared } from "../shared.ts";
import { buildNetinfo, wwanConnected, DEVICE_UTC_OFFSET } from "./network.ts";
import type { PublicStatus } from "../../../src/lib/api/schemas/public.ts";

/** Wi-Fi SSID (main2g_ssid). No shared field; keep in step with fixtures/wifi.ts if it differs. */
const SSID = "U60-Pro";
/** Tailscale IPv4 of this node (first column of `tailscale status`). */
const TS_IP = "100.101.7.23";
/** Last scenario switch: 2026-09-22 18:40 on the device clock (local time labelled UTC). */
const LAST_SWITCH = Date.UTC(2026, 8, 22, 18, 40, 12) / 1000;

function publicStatus(ctx: Ctx): PublicStatus {
  const net = buildNetinfo(ctx);
  const nettype = net.network_type ?? "";
  const operator = net.network_provider_fullname || net.network_provider || "";
  const barN = parseInt(net.signalbar ?? "", 10);
  const rsrp = (nettype === "LTE" ? net.lte_rsrp : net.nr5g_rsrp) ?? 0;

  return {
    network: {
      connected: wwanConnected(ctx),
      type: nettype,
      operator,
      bar: Number.isFinite(barN) ? barN : -1,
      rsrp: Number.isInteger(rsrp) ? rsrp : Math.trunc(rsrp),
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
      home_mode: {
        present: shared.homeMode.present,
        enabled: shared.homeMode.present && shared.homeMode.enabled,
        mode: shared.homeMode.mode,
      },
    },
    scenario: {
      configured: true,
      enabled: true,
      current: "abroad",
      name: "国外",
      wifi_off: false,
      abroad: true,
      auto_direct: false,
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
