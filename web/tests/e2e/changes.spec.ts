// The change log (E4 T9b; write-op-layer.md DD5, DD10, DD11): datad's rows
// (mock fixtures/ops.ts, shaped like journal.list V2-41), detail, undo.
import { test, expect } from "./support/fixtures";

test.use({ freshMock: true, viewport: { width: 1440, height: 1000 }, setup: { authed: true, lang: "zh" } });

test("two lines a row; hidden rows left out; skipped runs folded", async ({ ready, page, mock }) => {
  await mock.ops("idle");
  await ready.goto("/changes/");
  const rows = page.locator(".nd-row");
  await expect(rows).toHaveCount(5);
  await expect(rows.nth(0)).toContainText("制式 · 自动 → 只用 4G");
  await expect(rows.nth(0)).toContainText("10-03 14:32 · 网页 · 已切到只用 4G");
  // the result already names the source: not repeated
  await expect(rows.nth(2)).toContainText("10-02 22:10 · 情景跳过 ×5（你手动改过）");
  await expect(page.getByText("op.ack")).toHaveCount(0);
  await expect(page.getByText("只显示最近 50 条")).toBeVisible();
});

test("undo is in the detail; network mode asks with the tier-3 dialog", async ({ ready, page, mock }) => {
  await mock.ops("idle");
  await ready.goto("/changes/");
  await page.locator(".nd-row").nth(0).click();
  const d = page.getByTestId("change-detail");
  await expect(d).toContainText("已切到只用 4G");
  await d.getByRole("button", { name: "撤销" }).click();
  expect((await mock.ops()).writes).toEqual([]);
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("撤回这次改动：自动 → 只用 4G");
  await dialog.getByRole("button", { name: "撤销" }).click();
  await expect.poll(async () => (await mock.ops()).writes).toEqual([
    expect.objectContaining({ action: "network.set_mode", undo: true, params: { mode: "WL_AND_5G" } }),
  ]);
  // back to the list
  await expect(page.getByTestId("change-detail")).toHaveCount(0);
});

test("a row that can't be undone says why, and sends nothing", async ({ ready, page, mock }) => {
  await mock.ops("idle");
  await ready.goto("/changes/");
  await page.locator(".nd-row").nth(4).click();
  const d = page.getByTestId("change-detail");
  await expect(d.getByRole("button", { name: "撤销" })).toBeDisabled();
  await expect(d).toContainText("之后又改过");
  // writes that aren't transactions have no undo at all
  await page.getByRole("button", { name: /改动记录/ }).click();
  await page.locator(".nd-row").nth(1).click();
  await expect(page.getByTestId("change-detail").getByRole("button")).toHaveCount(0);
  expect((await mock.ops()).writes).toEqual([]);
});

test("empty and unreadable (DD11)", async ({ ready, page, mock }) => {
  await mock.ops("journal-empty");
  await ready.goto("/changes/");
  await expect(page.getByText("还没有改动")).toBeVisible();
  await expect(page.getByText("触屏、网页、情景、定时任务改的设置都会记在这里。")).toBeVisible();
  await mock.ops("journal-down");
  await ready.goto("/changes/");
  await expect(page.getByText("读不到改动记录 · 数据服务没响应")).toBeVisible({ timeout: 20000 });
});

test("the settings page links its last change to the detail (DD5)", async ({ ready, page, mock }) => {
  await mock.ops("idle");
  await ready.goto("/router/network-mode/");
  const last = page.getByTestId("last-change");
  await expect(last).toContainText("上次改动 10-03 14:32 · 网页");
  await last.getByRole("link").click();
  await expect(page).toHaveURL(/\/changes\/\?op=web-20/);
  await expect(page.getByTestId("change-detail")).toContainText("已切到只用 4G");
});

test.describe("English", () => {
  test.use({ setup: { authed: true, lang: "en" } });
  test("rows and the transaction bar use datad's English", async ({ ready, page, mock }) => {
    await mock.ops("idle");
    await ready.goto("/changes/");
    await expect(page.locator(".nd-row").nth(0)).toContainText("Network mode · Auto → 4G only");
    await expect(page.locator(".nd-row").nth(0)).toContainText("10-03 14:32 · Web · Now 4G only");
    await expect(page.locator("main")).not.toContainText(/[一-鿿]/);
    await mock.ops("verifying");
    await ready.goto("/router/wifi/");
    await expect(page.getByTestId("op-banner")).toContainText("Checking");
    await expect(page.getByTestId("op-banner")).not.toContainText(/[一-鿿]/);
  });
});

test("a new change landing while a detail is open doesn't move the detail or its undo", async ({ ready, page, mock }) => {
  test.setTimeout(60000);
  await mock.ops("idle");
  await ready.goto("/changes/");
  await page.locator(".nd-row").nth(0).click();
  const d = page.getByTestId("change-detail");
  await expect(d).toContainText("已切到只用 4G");
  await mock.ops("journal-push");
  // the list refetches every 15 s; force it sooner by waiting one cycle
  await page.waitForTimeout(16000);
  await expect(d).toContainText("已切到只用 4G");
  await expect(d).not.toContainText("关掉漫游");
  await d.getByRole("button", { name: "撤销" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "撤销" }).click();
  await expect.poll(async () => (await mock.ops()).writes.map((w) => w.params)).toEqual([{ mode: "WL_AND_5G" }]);
});
