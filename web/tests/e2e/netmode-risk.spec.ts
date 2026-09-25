// Network mode: the modes that leave a travelling device with no signal
// (5G SA only, 3G only) say so in the confirmation; Auto does not.
import { test, expect } from "./support/fixtures";

test.use({ freshMock: true, viewport: { width: 1440, height: 1000 }, setup: { authed: true, lang: "zh" } });

test("3G only and 5G SA only warn about abroad, Auto does not", async ({ ready, page }) => {
  await ready.goto("/router/network-mode/");
  const risk3g = "很多国家已经关了 3G";
  const riskSa = "很多国家还没有 5G SA";

  await page.getByRole("radio", { name: "只用 3G" }).click({ force: true });
  await page.getByRole("button", { name: "应用" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText(risk3g);
  await dialog.screenshot({ path: "test-results/netmode-risk-3g.png" });
  await page.getByRole("dialog").getByRole("button", { name: "取消" }).click();
  await expect(page.getByRole("dialog")).toBeHidden();

  await page.getByRole("radio", { name: "只用 5G SA" }).click({ force: true });
  await page.getByRole("button", { name: "应用" }).click();
  await expect(page.getByRole("dialog")).toContainText(riskSa);
  await page.getByRole("dialog").getByRole("button", { name: "取消" }).click();
  await expect(page.getByRole("dialog")).toBeHidden();

  await page.getByRole("radio", { name: "只用 4G" }).click({ force: true });
  await page.getByRole("button", { name: "应用" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(page.getByRole("dialog")).not.toContainText(risk3g);
  await expect(page.getByRole("dialog")).not.toContainText(riskSa);
});
