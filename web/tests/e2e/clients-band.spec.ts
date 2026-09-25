// Clients: each Wi-Fi device says which band it is on (2.4 / 5 GHz), its
// Wi-Fi generation and link rate; a lease that isn't on Wi-Fi says so.
import { test, expect } from "./support/fixtures";

test.use({ viewport: { width: 1440, height: 1000 }, setup: { authed: true, lang: "zh" } });

test("clients show band, generation and link rate", async ({ ready, page }) => {
  await ready.goto("/clients/");
  const list = page.locator("main");
  await expect(list).toContainText("5 GHz · Wi-Fi 6 · 2402 Mbps · 信号很好");
  await expect(list).toContainText("2.4 GHz · Wi-Fi 4 · 144 Mbps · 信号一般");
  await expect(list).toContainText("没连 Wi-Fi（网线、USB，或已经离开）");
  await page.locator("main").screenshot({ path: "test-results/clients-band.png" });
});
