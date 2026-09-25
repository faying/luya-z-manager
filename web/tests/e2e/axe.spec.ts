// axe-core on every exported route at 390 px, light and dark.
// Fails on serious / critical violations (WCAG 2.x A/AA + best practice rules
// that axe itself rates serious or critical).
import AxeBuilder from "@axe-core/playwright";
import { test, expect } from "./support/fixtures";
import { exportedRoutes, OUTSIDE_SHELL } from "./support/routes";

const THEMES = ["light", "dark"] as const;

/** Known, reported, not fixed here: fixing them changes the visual design
 *  (the green status word would need another colour on the family blocks).
 *  Matched on rule + exact colour pair, so any other contrast failure still
 *  fails. Remove an entry once the tokens are changed.
 *  - okT #146c34 on blkServices #cdb9f6 (home services card status): 3.68
 *  - okT #146c34 on blkCharts #bcd4ff (charts-family status meta "实时"): 4.34 */
const KNOWN_CONTRAST: { fg: string; bg: string }[] = [
  { fg: "#146c34", bg: "#cdb9f6" },
  { fg: "#146c34", bg: "#bcd4ff" },
];

type NodeData = { fgColor?: string; bgColor?: string };
function isKnown(ruleId: string, data: NodeData | undefined): boolean {
  if (ruleId !== "color-contrast" || !data) return false;
  return KNOWN_CONTRAST.some((k) => k.fg === data.fgColor && k.bg === data.bgColor);
}

for (const route of exportedRoutes()) {
  for (const theme of THEMES) {
    test.describe(`${route} ${theme}`, () => {
      test.use({
        viewport: { width: 390, height: 844 },
        setup: { authed: !OUTSIDE_SHELL.has(route), lang: "zh", theme },
      });
      test("axe", async ({ ready, page }) => {
        await ready.goto(route);
        // Entry animations finish before colours are sampled.
        await page.waitForTimeout(400);
        const res = await new AxeBuilder({ page }).analyze();
        const bad = res.violations.filter((v) => v.impact === "serious" || v.impact === "critical");
        // One line per failing node, so the diff reads as a list of problems.
        const known: string[] = [];
        const problems = bad.flatMap((v) =>
          v.nodes.flatMap((n) => {
            const data = n.any.find((c) => c.id === "color-contrast")?.data as NodeData | undefined;
            if (isKnown(v.id, data)) {
              known.push(`${v.id} ${data?.fgColor} on ${data?.bgColor} · ${n.target.join(" ")}`);
              return [];
            }
            const why = (n.failureSummary ?? "").split("\n").slice(1).join(" ").trim();
            return [`${v.impact} ${v.id} · ${n.target.join(" ")} · ${why}`];
          }),
        );
        for (const k of known) test.info().annotations.push({ type: "known-axe", description: k });
        expect(problems).toEqual([]);
      });
    });
  }
}
