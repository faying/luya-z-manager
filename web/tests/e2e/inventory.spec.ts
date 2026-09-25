// Inventory auto-check (design doc E9): every control listed in
// docs/controls-inventory.md must still be findable on its page by its
// accessible name, at 1440 or 390 wide. Rows marked 〔交互后：…〕 are only
// there after an interaction and are listed, not checked; rows marked
// 〔缺失：…〕 are known-missing and listed (the test fails if one turns up
// again, so the doc gets fixed). Anything else not found fails.
// Per-route results go to test-results/inventory/*.json; the global teardown
// (support/inventory-teardown.ts) prints the summary table and writes
// test-results/inventory-summary.md.
import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Page } from "@playwright/test";
import { test, expect } from "./support/fixtures";
import { exportedRoutes, OUTSIDE_SHELL } from "./support/routes";
import { parseInventory, ROLES, type NameSpec, type Row } from "./support/inventory";

export const INVENTORY_OUT = resolve(__dirname, "../../test-results/inventory");

const { rows, problems } = parseInventory();

// Recipes start scans, tests and USSD sessions on the mock: keep that state
// out of the worker mock other specs share.
test.use({ freshMock: true });

test("inventory tables are well-formed (no row lost a cell)", () => {
  expect(problems).toEqual([]);
  // 329: the QoS switch row went away on purpose (page read-only, 2026-09-25).
  expect(rows.length).toBeGreaterThanOrEqual(329);
});

test("every inventory route exists in the export", () => {
  const exported = new Set(exportedRoutes());
  expect([...new Set(rows.map((r) => r.route))].filter((r) => !exported.has(r))).toEqual([]);
});

type Hit = { how: string; role?: string; name: string };

const LABELLED = new Set(["textbox", "searchbox", "combobox", "spinbutton", "slider", "switch", "checkbox", "radio"]);

async function count(l: ReturnType<Page["getByRole"]>): Promise<number> {
  return l.count().catch(() => 0);
}

/** Strict: a named role must match that role (form controls may also match
 *  by label, e.g. password inputs have no textbox role). A name without a
 *  role may match any role or label. No plain-text fallback: 「开」 as text is
 *  not the switch that used to be called 「开」. */
async function findName(page: Page, n: NameSpec): Promise<Hit | null> {
  const name = n.pattern ?? n.name;
  const exact = n.pattern ? undefined : true;
  if (n.role) {
    if (await count(page.getByRole(n.role, { name, exact }))) return { how: "role", role: n.role, name: n.name };
    if (LABELLED.has(n.role) && (await count(page.getByLabel(name, { exact })))) return { how: "label", role: n.role, name: n.name };
    return null;
  }
  if (await count(page.getByLabel(name, { exact }))) return { how: "label", name: n.name };
  for (const r of ROLES) if (await count(page.getByRole(r, { name, exact }))) return { how: "role", role: r, name: n.name };
  return null;
}

/** For the failure message: where the name does turn up (another role, plain text). */
async function diagnose(page: Page, row: Row): Promise<string> {
  const seen: string[] = [];
  for (const n of row.names) {
    const name = n.pattern ?? n.name;
    const exact = n.pattern ? undefined : true;
    for (const r of ROLES) if (r !== n.role && (await count(page.getByRole(r, { name, exact })))) seen.push(`${r}「${n.name}」`);
    if (!seen.length && (await count(page.getByText(name, { exact })))) seen.push(`text「${n.name}」`);
  }
  return seen.length ? ` — on the page as ${[...new Set(seen)].join(" ")}` : "";
}

async function findRow(page: Page, row: Row): Promise<Hit | null> {
  for (const n of row.names) {
    const hit = await findName(page, n);
    if (hit) return hit;
  }
  return null;
}

export type RowResult = {
  route: string;
  line: number;
  control: string;
  cell: string;
  status: "found" | "renamed" | "interaction ✓" | "interaction" | "missing" | "NOT FOUND" | "interaction ✗" | "missing-but-found";
  renamed?: boolean;
  how?: string;
  detail?: string;
};

const byRoute = new Map<string, Row[]>();
for (const r of rows) byRoute.set(r.route, [...(byRoute.get(r.route) ?? []), r]);

for (const [route, list] of byRoute) {
  test.describe(`inventory ${route}`, () => {
    test.use({ setup: { authed: !OUTSIDE_SHELL.has(route), lang: "zh", theme: "light" }, viewport: { width: 1440, height: 1000 } });

    test("controls are findable", async ({ ready, page }) => {
      test.setTimeout(180_000);
      await ready.goto(route);
      await page.waitForTimeout(600);
      const checked = list.filter((r) => !r.interaction);
      const hits = new Map<number, Hit | null>();
      for (const r of checked) hits.set(r.line, await findRow(page, r));
      // Phone layout: bottom tabs, "more" menus and other phone-only controls.
      if ([...hits.values()].some((h) => !h)) {
        await page.setViewportSize({ width: 390, height: 844 });
        await page.waitForTimeout(600);
        for (const r of checked) if (!hits.get(r.line)) hits.set(r.line, await findRow(page, r));
      }

      const diag = new Map<number, string>();
      for (const r of checked) if (!hits.get(r.line)) diag.set(r.line, await diagnose(page, r));

      // Interaction rows with steps: fresh page, do the steps, then look.
      const after = new Map<number, { hit: Hit | null; error?: string }>();
      for (const r of list.filter((x) => x.interaction && x.steps.length)) {
        let error: string | undefined;
        try {
          await page.setViewportSize({ width: 1440, height: 1000 });
          const scen = r.steps.find((s) => s.kind === "scenario");
          await ready.mock.scenario(scen && scen.kind === "scenario" ? scen.set : "normal");
          await page.goto(route);
          await page.waitForTimeout(scen ? 2500 : 600);
          for (const s of r.steps) {
            if (s.kind === "scenario") continue;
            if (s.kind === "scenarioAfter") {
              await ready.mock.scenario(s.set);
              continue;
            }
            const target = page.getByRole(s.role, { name: s.pattern ?? s.name, exact: s.pattern ? undefined : true }).first();
            // react-aria switches/checkboxes keep the real <input> visually
            // hidden; a DOM click on it toggles it like a tap on the label.
            if (s.kind === "click" && (s.role === "switch" || s.role === "checkbox")) await target.dispatchEvent("click", undefined, { timeout: 5000 });
            else if (s.kind === "click") await target.click({ timeout: 5000 });
            else await target.fill(s.value, { timeout: 5000 });
            await page.waitForTimeout(400);
          }
        } catch (e) {
          error = String(e).split("\n")[0];
        }
        // Results of a scan / confirm can take a few seconds to land.
        let hit: Hit | null = null;
        const until = Date.now() + (error ? 0 : r.steps.some((s) => s.kind === "scenarioAfter") ? 25_000 : 10_000);
        do {
          hit = await findRow(page, r);
          if (hit || Date.now() > until) break;
          await page.waitForTimeout(500);
        } while (!hit);
        after.set(r.line, { hit, error });
        await page.keyboard.press("Escape").catch(() => {});
      }
      await ready.mock.scenario("normal");

      const results: RowResult[] = list.map((r) => {
        const base = { route, line: r.line, control: r.control, cell: r.cell, renamed: r.renamed };
        if (r.interaction && r.steps.length) {
          const a = after.get(r.line)!;
          if (a.hit) return { ...base, status: "interaction ✓", how: `${a.hit.how}${a.hit.role ? ` ${a.hit.role}` : ""}「${a.hit.name}」`, detail: r.interaction };
          return { ...base, status: "interaction ✗", detail: `${r.interaction}${a.error ? ` — ${a.error}` : ""}` };
        }
        if (r.interaction) return { ...base, status: "interaction", detail: r.interaction };
        const hit = hits.get(r.line) ?? null;
        const how = hit ? `${hit.how}${hit.role ? ` ${hit.role}` : ""}「${hit.name}」` : undefined;
        if (r.missing) return hit ? { ...base, status: "missing-but-found", how, detail: r.missing } : { ...base, status: "missing", detail: r.missing };
        if (!hit) return { ...base, status: "NOT FOUND", detail: (r.names.map((n) => `${n.role ?? "?"}「${n.name}」`).join(" ") || "(no name in cell)") + (diag.get(r.line) ?? "") };
        return { ...base, status: r.renamed ? "renamed" : "found", how };
      });
      mkdirSync(INVENTORY_OUT, { recursive: true });
      writeFileSync(resolve(INVENTORY_OUT, `${route.replace(/\//g, "_") || "_"}.json`), JSON.stringify(results, null, 2));

      const bad = results
        .filter((r) => r.status === "NOT FOUND" || r.status === "missing-but-found" || r.status === "interaction ✗")
        .map((r) => `L${r.line} [${r.status}] ${r.control} :: ${r.detail}${r.how ? ` (found as ${r.how})` : ""}`);
      expect(bad).toEqual([]);
    });
  });
}
