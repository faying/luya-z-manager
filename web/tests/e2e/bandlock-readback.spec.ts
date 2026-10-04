// Band lock reads back what the firmware keeps (E4 T9c): GET
// /api/cell/band/lock after a lock or a reset; the reset texts say cell
// locks go too (D22).
import { test, expect } from "./support/fixtures";

test.use({ freshMock: true, viewport: { width: 1440, height: 1000 }, setup: { authed: true, lang: "zh" } });

test("LTE lock and reset read back as applied; the locked set is shown", async ({ ready, page }) => {
  await ready.goto("/bandlock/");
  await expect(page.getByText("没锁（全部频段）")).toBeVisible();
  const lte = page.getByRole("region", { name: /LTE/ });
  await lte.getByRole("button", { name: "B3", exact: true }).click();
  await lte.getByRole("button", { name: /应用 LTE/ }).click();
  await page.getByRole("dialog").getByRole("button", { name: /锁定 LTE/ }).click();
  await expect(lte.getByText("已生效")).toBeVisible({ timeout: 20000 });
  await expect(page.getByText(/LTE B3$/)).toBeVisible();

  await page.getByRole("button", { name: "重置 / 全部解锁" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("频段和小区锁一起恢复");
  await dialog.getByRole("button", { name: "重置 / 全部解锁" }).click();
  await expect(page.getByRole("region", { name: "自动选频" }).getByText("已生效")).toBeVisible({ timeout: 20000 });
  await expect(page.getByText("没锁（全部频段）")).toBeVisible();
});

test("cell lock reset says band locks go too", async ({ ready, page }) => {
  await ready.goto("/router/celllock/");
  await page.getByRole("button", { name: "全部解锁", exact: true }).first().click();
  await expect(page.getByRole("dialog")).toContainText("频段和小区锁一起恢复");
});
