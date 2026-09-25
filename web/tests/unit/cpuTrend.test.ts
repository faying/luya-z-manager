import { describe, expect, it } from "vitest";
import { pushPoint, sparkPoints } from "@/lib/cpuTrend";

describe("cpu trend", () => {
  it("keeps the newest points", () => {
    let l: number[] = [];
    for (let i = 0; i < 65; i++) l = pushPoint(l, i);
    expect(l).toHaveLength(60);
    expect(l[0]).toBe(5);
    expect(l[59]).toBe(64);
  });
  it("draws newest at the right edge and clamps", () => {
    expect(sparkPoints([0, 150], 10, 10, 3)).toBe("5.0,10.0 10.0,0.0");
    expect(sparkPoints([], 10, 10)).toBe("");
  });
});
