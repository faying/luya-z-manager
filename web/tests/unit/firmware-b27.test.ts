// Shapes recorded from a real U60 Pro (MU5250) on firmware B27, 2026-09-25.
// Values here are made up; only the shapes match the device.
import { describe, expect, it } from "vitest";
import { ApiError } from "@/lib/api/types";
import { isUnsupported } from "@/lib/api/unsupported";
import { storeUsage } from "@/lib/sms";
import { servingCellId, carriers } from "@/lib/home";
import { normalizeFilterRules, normalizePortForward } from "@/app/(panel)/router/firewall/normalize";

describe("isUnsupported", () => {
  it("is true only for 503 + Method not found", () => {
    const msg = "ubus call zwrt_router.api router_get_qos_switch failed: Command failed: ubus call zwrt_router.api router_get_qos_switch {} (Method not found)";
    expect(isUnsupported(new ApiError(msg, 503))).toBe(true);
    expect(isUnsupported(new ApiError("ubus call x y failed: Command failed: Invalid argument", 503))).toBe(false);
    expect(isUnsupported(new ApiError(msg, 500))).toBe(false);
    expect(isUnsupported(new Error(msg))).toBe(false);
    expect(isUnsupported(undefined)).toBe(false);
  });
});

describe("storeUsage", () => {
  it("adds rev + send + draftbox and ignores the unreliable nvused_total", () => {
    const cap = {
      sms_nv_total: 100,
      sms_nvused_total: 0,
      sms_nv_rev_total: 5,
      sms_nv_send_total: 1,
      sms_nv_draftbox_total: 1,
      sms_sim_total: 40,
      sms_sim_rev_total: 3,
      sms_sim_send_total: 0,
      sms_sim_draftbox_total: 0,
    };
    expect(storeUsage(cap, "nv")).toEqual({ used: 7, total: 100 });
    expect(storeUsage(cap, "sim")).toEqual({ used: 3, total: 40 });
  });
  it("falls back to *used_total without per-box keys, null without a total", () => {
    expect(storeUsage({ sms_sim_total: 30, sms_simused_total: 2 }, "sim")).toEqual({ used: 2, total: 30 });
    expect(storeUsage({ sms_sim_total: 30 }, "sim")).toBeNull();
    expect(storeUsage({ sms_sim_rev_total: 2 }, "sim")).toBeNull();
    expect(storeUsage(undefined, "nv")).toBeNull();
  });
});

describe("firewall rule lists", () => {
  it("treats {} as no rules and keeps arrays / {rule_list} working", () => {
    expect(normalizePortForward({})).toEqual([]);
    expect(normalizeFilterRules({})).toEqual([]);
    expect(normalizePortForward([{ id: "1", name: "a" }])).toHaveLength(1);
    expect(normalizePortForward({ rule_list: [{ id: "1" }] })).toHaveLength(1);
    expect(normalizePortForward({ some_name: [{ id: "1" }] })).toHaveLength(1);
    expect(normalizePortForward({ a: "x" })).toBeNull();
    expect(normalizePortForward("nope")).toBeNull();
  });
});

describe("servingCellId", () => {
  it("uses the NR id in SA when LTE fields are absent", () => {
    const sig = { network_type: "SA", nr5g_rsrp: -90, nr5g_pci: 12, nr5g_cell_id: 1234567, rmcc: 1, rmnc: 1 };
    const cs = carriers(sig);
    expect(cs[0]).toMatchObject({ kind: "nr", pci: 12 });
    expect(servingCellId(sig, cs[0])).toBe(1234567);
  });
  it("uses the LTE id when LTE serves", () => {
    const sig = { network_type: "LTE", lte_rsrp: -100, lte_pci: 7, cell_id: 555, nr5g_cell_id: 0 };
    const cs = carriers(sig);
    expect(servingCellId(sig, cs[0])).toBe(555);
  });
});

import { normalizeUpnp, upnpBody } from "../../src/app/(panel)/router/firewall/normalize";

describe("UPnP on B27 (router_get_upnp / router_set_upnp_switch)", () => {
  const b27 = { enabled: "0", enable_upnp: "0", notify_interval: "60", ttl: "", enable_natpmp: "0" };
  it("reads enable_upnp, falls back to upnp_switch", () => {
    expect(normalizeUpnp(b27)).toBe(false);
    expect(normalizeUpnp({ ...b27, enable_upnp: "1" })).toBe(true);
    expect(normalizeUpnp({ upnp_switch: "1" })).toBe(true);
    expect(normalizeUpnp({})).toBeUndefined();
  });
  it("writes integers, keeps the device's other values, drops an empty ttl", () => {
    expect(upnpBody(b27, true)).toEqual({ enable_upnp: 1, notify_interval: 60, natpmp: 0 });
    expect(upnpBody({ ...b27, ttl: "4" }, false)).toEqual({ enable_upnp: 0, notify_interval: 60, ttl: 4, natpmp: 0 });
  });
});
