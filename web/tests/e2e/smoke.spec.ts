// Smoke: every exported route at phone and desktop size, light and dark.
//   - no uncaught page error, no console.error
//   - no horizontal scroll
//   - an <h1> exists
import { test, expect } from "./support/fixtures";
import { exportedRoutes, OUTSIDE_SHELL } from "./support/routes";

const SIZES = [
  { name: "390", width: 390, height: 844 },
  { name: "1440", width: 1440, height: 1000 },
] as const;
const THEMES = ["light", "dark"] as const;

for (const route of exportedRoutes()) {
  for (const size of SIZES) {
    for (const theme of THEMES) {
      test.describe(`${route} @${size.name} ${theme}`, () => {
        test.use({
          viewport: { width: size.width, height: size.height },
          setup: { authed: !OUTSIDE_SHELL.has(route), lang: "zh", theme },
        });
        test("smoke", async ({ ready, page }) => {
          await ready.goto(route);
          await expect(page).toHaveURL(new RegExp(`${route.replace(/\//g, "\\/")}$`));
          await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
          await expect(page.locator("h1").first()).toBeVisible();
          const { sw, iw } = await page.evaluate(() => ({
            sw: document.documentElement.scrollWidth,
            iw: window.innerWidth,
          }));
          expect(sw, `horizontal scroll: scrollWidth ${sw} > innerWidth ${iw}`).toBeLessThanOrEqual(iw);
          expect(ready.errors, "page errors / console errors").toEqual([]);
        });
      });
    }
  }
}
