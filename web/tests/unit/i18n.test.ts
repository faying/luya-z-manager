import { describe, expect, it } from "vitest";
import { deepMerge } from "@/lib/i18n/nd-zh";

describe("nd-zh deepMerge", () => {
  it("adds keys inside an existing namespace without dropping the others", () => {
    const out = deepMerge({ a: { x: "1", y: "2" }, b: "3" }, { a: { y: "20", z: "30" } });
    expect(out).toEqual({ a: { x: "1", y: "20", z: "30" }, b: "3" });
  });
});
