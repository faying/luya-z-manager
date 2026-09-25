import { describe, expect, it } from "vitest";
import { badgeText, differsFromHome, logoSlug, logoSrc, normalizePlmn } from "@/lib/operatorLogo";

describe("operator logo", () => {
  it("normalizes two- and three-digit MNCs", () => {
    expect(normalizePlmn("460", "1")).toBe("460-001");
    expect(normalizePlmn("460", "01")).toBe("460-001");
    expect(normalizePlmn("310", "260")).toBe("310-260");
    expect(normalizePlmn("46", "01")).toBeNull();
    expect(normalizePlmn(null, "01")).toBeNull();
  });
  it("looks up slugs", () => {
    expect(logoSlug("460", "00")).toBe("china-mobile");
    expect(logoSlug("454", "12")).toBe("cmhk");
    expect(logoSlug("999", "99")).toBeNull();
  });
  it("uses the base path and only files that exist", () => {
    expect(logoSrc("csl", "/admin", ["csl"])).toBe("/admin/operator-logos/csl.svg");
    expect(logoSrc("csl", "", [])).toBeNull();
    expect(logoSrc(null, "", ["csl"])).toBeNull();
  });
  it("badges with the first character", () => {
    expect(badgeText("中国移动")).toBe("中");
    expect(badgeText("docomo")).toBe("D");
    expect(badgeText("")).toBe("?");
  });
  it("compares serving and home by PLMN", () => {
    const cm = { mcc: "460", mnc: "00", name: "中国移动" };
    expect(differsFromHome({ mcc: "460", mnc: "000", name: "CMCC" }, cm)).toBe(false);
    expect(differsFromHome({ mcc: "454", mnc: "12", name: "CMHK" }, cm)).toBe(true);
    expect(differsFromHome(null, cm)).toBe(false);
  });
});
