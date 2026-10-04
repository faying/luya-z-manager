// Audit C, first batch (10-04): the 4G anchor of a 5G NSA link, serving RSSI, PLMN.
import { describe, expect, it } from "vitest";
import { lteAnchor, plmn, servingRssi } from "@/lib/home";
import type { NetworkSignal } from "@/lib/api/schemas/network";

const nsa = {
  network_type: "NSA",
  wan_active_band: "B3",
  wan_active_channel: 1650,
  lte_pci: 211,
  lte_rsrp: -96,
  lte_rsrq: -11,
  lte_snr: "12.5",
  lte_rssi: -70,
  nr5g_rsrp: -101,
  nr5g_rssi: -75,
  rssi: -60,
  rmcc: 460,
  rmnc: 1,
} as NetworkSignal;

describe("audit C cell helpers", () => {
  it("the LTE anchor only in NSA", () => {
    expect(lteAnchor(nsa)).toEqual({ band: "3", pci: 211, earfcn: 1650, rsrp: -96, rsrq: -11, sinr: 12.5, rssi: -70 });
    expect(lteAnchor({ ...nsa, network_type: "SA" })).toBeNull();
    expect(lteAnchor({ ...nsa, lte_rsrp: undefined })).toBeNull();
    // an NR band in the summary is not the anchor's band
    expect(lteAnchor({ ...nsa, wan_active_band: "n78" })?.band).toBeNull();
  });
  it("serving RSSI follows the serving RAT, else the summary", () => {
    expect(servingRssi(nsa, "nr")).toBe(-75);
    expect(servingRssi(nsa, "lte")).toBe(-70);
    expect(servingRssi({ ...nsa, nr5g_rssi: undefined }, "nr")).toBe(-60);
    expect(servingRssi(undefined, "nr")).toBeNull();
  });
  it("PLMN puts the MNC's leading zero back", () => {
    expect(plmn(nsa)).toBe("460-01");
    expect(plmn({ ...nsa, rmcc: "466", rmnc: "92" })).toBe("466-92");
    expect(plmn({ ...nsa, rmcc: "", rmnc: "" })).toBeNull();
    expect(plmn({ ...nsa, rmcc: 0 })).toBeNull();
  });
});
