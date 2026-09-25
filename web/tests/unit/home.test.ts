import { describe, expect, it } from "vitest";
import { bytes, carrierCounts, carriers, cpuTempC, mbps, parseCa, sigState, totalBandwidth } from "@/lib/home";

describe("parseCa", () => {
  it("parses 11-field records and marks the -140 floor inactive", () => {
    const cs = parseCa("1,671,0,78,627264,100,0,-95,-11,18,-60;2,12,0,41,504990,60,0,-140,-20,0,-100", "nr");
    expect(cs).toHaveLength(2);
    expect(cs[0]).toMatchObject({ band: "78", pci: 671, arfcn: 627264, bw: 100, active: true });
    expect(cs[1].active).toBe(false);
  });
  it("skips legacy 5-field records and empty strings", () => {
    expect(parseCa("12,3,1,1300,20", "lte")).toEqual([]);
    expect(parseCa("", "lte")).toEqual([]);
    expect(parseCa(undefined, "nr")).toEqual([]);
  });
});

describe("carriers", () => {
  it("puts the serving NR cell first, then nrca and lteca without de-duplicating", () => {
    const cs = carriers({
      network_type: "SA",
      nr5g_rsrp: -95,
      nr5g_action_band: "n78",
      nr5g_bandwidth: "100",
      nr5g_pci: 671,
      nr5g_snr: "18.5",
      nrca: "1,671,0,78,627264,100,0,-140,-20,0,-100;2,12,0,41,504990,40,0,-100,-12,10,-70",
      lteca: "1,300,0,3,1300,20,0,-99,-10,12,-70",
    });
    expect(cs.map((c) => [c.kind, c.band, c.serving])).toEqual([
      ["nr", "78", true],
      ["nr", "78", false],
      ["nr", "41", false],
      ["lte", "3", false],
    ]);
    expect(totalBandwidth(cs)).toBe(260);
    expect(carrierCounts(cs)).toEqual({ nr: 3, lte: 1 });
    expect(cs[0].sinr).toBe(18.5);
  });
  it("has no bandwidth without carriers", () => {
    expect(totalBandwidth(carriers({ network_type: "NO_SERVICE" }))).toBeNull();
  });
});

describe("sigState (same rules as the touchscreen)", () => {
  const base = { everValid: true, valid: true, bars: 4, sinr: 10 };
  it.each([
    [{ ...base, everValid: false }, "loading"],
    [{ ...base, valid: false }, "stale"],
    [{ ...base, simState: "sim absent" }, "nosim"],
    [{ ...base, bars: 0 }, "none"],
    [{ ...base, bars: null }, "none"],
    [{ ...base, bars: 2 }, "weak"],
    [{ ...base, sinr: -1 }, "weak"],
    [{ ...base, simState: "sim ready" }, "good"],
    [base, "good"],
  ] as const)("%o → %s", (args, want) => {
    expect(sigState(args)).toBe(want);
  });
});

describe("formatting", () => {
  it("converts bytes/s to Mbps", () => {
    expect(mbps(39_000_000)).toBe("312");
    expect(mbps(5_262_500)).toBe("42.1");
    expect(mbps(null)).toBeNull();
  });
  it("formats byte counts", () => {
    expect(bytes(1_234_567_890)).toBe("1.23 GB");
    expect(bytes("512")).toBe("512 B");
    expect(bytes(null)).toBeNull();
  });
  it("reads cpuss_temp in °C or milli-°C", () => {
    expect(cpuTempC({ cpuss_temp: 47000 })).toBe(47);
    expect(cpuTempC({ cpuss_temp: 52 })).toBe(52);
    expect(cpuTempC({})).toBeNull();
  });
});
