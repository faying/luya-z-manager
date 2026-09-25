import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  estimate,
  estimateText,
  formatDuration,
  type EstimateInput,
  type Tr,
} from "@/lib/batteryEstimate";
import { zh as batteryZh } from "@/lib/i18n/nd-zh/battery";

const zhT: Tr = (key, _def, vars) => {
  const s = (batteryZh.battery as Record<string, string>)[key.replace("battery.", "")];
  return s.replace(/\{\{(\w+)\}\}/g, (_, k) => String(vars?.[k]));
};

const docsDir = resolve(__dirname, "../../../docs");
const raw = readFileSync(resolve(docsDir, "battery-estimate/fixtures.json"));
const fixtures = JSON.parse(raw.toString()) as {
  cases: { name: string; input: EstimateInput; expect: { kind: string; minutes: number | null } }[];
};

describe("battery estimate fixtures", () => {
  it("sha256 matches docs/battery-estimate.md", () => {
    const doc = readFileSync(resolve(docsDir, "battery-estimate.md"), "utf8");
    const recorded = /fixtures sha256: `([0-9a-f]{64})`/.exec(doc)?.[1];
    expect(createHash("sha256").update(raw).digest("hex")).toBe(recorded);
  });

  for (const c of fixtures.cases) {
    it(c.name, () => {
      expect(estimate(c.input)).toEqual(c.expect);
    });
  }
});

describe("estimate text", () => {
  it("formats durations", () => {
    expect(formatDuration(45, zhT)).toBe("45 分钟");
    expect(formatDuration(120, zhT)).toBe("2 小时");
    expect(formatDuration(198, zhT)).toBe("3 小时 18 分");
  });

  it("names the limit when charging to it", () => {
    expect(estimateText({ kind: "charging_eta", minutes: 198 }, 80, zhT)).toBe("约 3 小时 18 分充到 80%");
    expect(estimateText({ kind: "reached_target", minutes: null }, 100, zhT)).toBe("已充满");
    expect(estimateText({ kind: "paused_at_limit", minutes: null }, 80, zhT)).toBe("已到上限，暂停充电");
    expect(estimateText({ kind: "unknown", minutes: null }, 100, zhT)).toBe("—");
  });
});
