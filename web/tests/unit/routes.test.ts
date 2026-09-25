import { describe, expect, it } from "vitest";
import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { anchorFor, ROUTES, routeFor, searchRoutes } from "@/lib/routes";

const title = (r: { fallback: string }) => r.fallback;

describe("route table", () => {
  it("lists every panel page except /login", () => {
    const root = join(__dirname, "../../src/app/(panel)");
    const pages: string[] = [];
    const walk = (dir: string, rel: string) => {
      for (const n of readdirSync(dir)) {
        const p = join(dir, n);
        if (statSync(p).isDirectory()) walk(p, `${rel}/${n}`);
        else if (n === "page.tsx") pages.push(rel || "/");
      }
    };
    walk(root, "");
    const hubs = ["/charts", "/functions", "/system"];
    const expected = pages.filter((p) => p !== "/login" && !hubs.includes(p)).sort();
    expect(ROUTES.map((r) => r.href).sort()).toEqual(expected);
  });

  it("matches the longest prefix", () => {
    expect(routeFor("/sms/forward/")?.href).toBe("/sms/forward");
    expect(routeFor("/sms")?.href).toBe("/sms");
    expect(anchorFor("/functions/")).toBe("functions");
    expect(anchorFor("/router/device")).toBe("system");
  });
});

describe("search", () => {
  // Success criteria in the design doc: these must put the page first.
  const must: [string, string][] = [
    ["锁频", "/bandlock"], ["suopin", "/bandlock"], ["band", "/bandlock"], ["bandlock", "/bandlock"],
    ["APN", "/router/apn"], ["接入点", "/router/apn"],
    ["锁小区", "/router/celllock"], ["celllock", "/router/celllock"],
    ["eSIM", "/router/esim"],
    ["短信", "/sms"], ["sms", "/sms"],
    ["CHILL", "/services/chill"], ["代理", "/services/chill"], ["节点", "/services/chill"],
  ];
  it.each(must)("%s → %s first", (q, href) => {
    expect(searchRoutes(q, title)[0]?.href).toBe(href);
  });
  it("returns nothing for an empty query", () => {
    expect(searchRoutes("  ", title)).toEqual([]);
  });
});
