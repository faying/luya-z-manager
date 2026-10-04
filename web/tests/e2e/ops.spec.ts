// The write-op layer on the web (E4 T9a; write-op-layer.md DD4, DD8, DD9,
// DD12, DD13, DD15, DD16). States come from the mock's presets
// (scripts/mock-agent/fixtures/ops.ts), shaped like datad's real view.
import { test, expect } from "./support/fixtures";

test.use({ freshMock: true, viewport: { width: 1440, height: 1000 }, setup: { authed: true, lang: "zh" } });

test("a change started on the touch screen shows as a bar on other pages, links to its page", async ({ ready, page, mock }) => {
  await mock.ops("verifying");
  await ready.goto("/router/wifi/");
  const bar = page.getByTestId("op-banner");
  await expect(bar).toContainText("正在确认");
  await expect(bar).toContainText("制式 · 自动 → 只用 5G SA · 触屏");
  await expect(bar).toHaveAttribute("role", "status");
  // the countdown is not announced: only the phase is in the live text
  await expect(bar.locator('[aria-hidden="true"]', { hasText: /\d:\d\d/ })).toHaveCount(1);
  await bar.getByRole("link", { name: /详情/ }).click();
  await expect(page).toHaveURL(/\/router\/network-mode\/?$/);
  // on its own page: no bar, the status block has everything
  await expect(page.getByTestId("op-banner")).toHaveCount(0);
  const block = page.getByTestId("op-status");
  await expect(block).toContainText("正在确认");
  await expect(block).toContainText("设置已生效");
  await expect(block.getByTestId("op-next")).toContainText(/\d:\d\d 后没通就退回到自动/);
  // the countdown line is outside the live region
  await expect(block.locator('[role="status"]')).not.toContainText(/\d:\d\d/);
});

test("revert and keep are two-step; one confirm open at a time", async ({ ready, page, mock }) => {
  await mock.ops("verifying");
  await ready.goto("/router/network-mode/");
  const block = page.getByTestId("op-status");
  await block.getByRole("button", { name: "退回自动" }).click();
  await expect(page.getByText("马上改回「自动」，会重新注册，断网几十秒")).toBeVisible();
  expect((await mock.ops()).acts).toEqual([]);
  // keep replaces the open revert confirm
  await block.getByRole("button", { name: "保留只用 5G SA" }).click();
  await expect(page.getByText("不再自动退回；还没确认通，没网要你自己改回去")).toBeVisible();
  await expect(page.getByText("马上改回「自动」")).toHaveCount(0);
  await page.getByRole("group").getByRole("button", { name: /保留只用 5G SA/ }).click();
  await expect.poll(async () => (await mock.ops()).acts).toEqual([{ act: "keep", op_id: "screen-12-3" }]);
  // result stays until Got it, which hides it on both sides
  await expect(block).toContainText("保留只用 5G SA · 没确认通");
  await block.getByRole("button", { name: "知道了" }).click();
  await expect.poll(async () => (await mock.ops()).acts.map((a) => a.act)).toEqual(["keep", "ack"]);
  await expect(page.getByTestId("op-status")).toHaveCount(0);
});

test("two tabs press revert: the second is refused and told, nothing is done twice", async ({ ready, page, mock, context }) => {
  await mock.ops("verifying");
  await ready.goto("/router/network-mode/");
  const other = await context.newPage();
  await other.goto(page.url());
  for (const p of [page, other]) {
    await p.bringToFront();
    await p.getByTestId("op-status").getByRole("button", { name: "退回自动" }).click();
  }
  await page.bringToFront();
  await page.getByRole("group").getByRole("button", { name: "退回自动" }).click();
  await expect.poll(async () => (await mock.ops()).acts.length).toBe(1);
  await other.bringToFront();
  const late = other.getByRole("group").getByRole("button", { name: "退回自动" });
  // its next read drops the confirm; if it is pressed first, datad refuses
  if (await late.isVisible()) await late.click().catch(() => {});
  await expect(other.getByRole("group")).toHaveCount(0, { timeout: 8000 });
  expect((await mock.ops()).acts).toEqual([{ act: "revert", op_id: "screen-12-3" }]);
});

test("revert failed: alert bar, retry and restart are confirmed (DD9)", async ({ ready, page, mock }) => {
  await mock.ops("rollback-failed");
  await ready.goto("/");
  const bar = page.getByTestId("op-banner");
  await expect(bar).toHaveAttribute("role", "alert");
  await expect(bar).toContainText("退回也没通");
  await ready.goto("/router/network-mode/");
  const block = page.getByTestId("op-status");
  await expect(block).toContainText("现在：当前设置未知 · 上次确认：自动");
  await block.getByRole("button", { name: "再试一次退回到自动" }).click();
  expect((await mock.ops()).writes).toEqual([]);
  await page.getByRole("group").getByRole("button", { name: "再试一次退回到自动" }).click();
  await expect.poll(async () => (await mock.ops()).writes.map((w) => w.params)).toEqual([{ mode: "WL_AND_5G" }]);
  await expect(block.getByRole("button", { name: "重启设备" })).toBeVisible();
});

test("datad stuck: the bar says so and Apply is off (DD8)", async ({ ready, page, mock }) => {
  await mock.ops("stuck");
  await ready.goto("/router/network-mode/");
  await expect(page.getByTestId("op-banner")).toContainText("数据服务没响应");
  await expect(page.getByText("数据服务没响应 · 暂时不能改设置")).toBeVisible();
  await expect(page.getByRole("radio", { name: "只用 4G" })).toBeDisabled();
});

test("a switch from the web hands over to the transaction; rollback off is said up front (DD6)", async ({ ready, page, mock }) => {
  await mock.ops("idle-rollback-off");
  await ready.goto("/router/network-mode/");
  await page.getByRole("radio", { name: "只用 4G" }).click({ force: true });
  await page.getByRole("button", { name: "应用" }).click();
  await expect(page.getByRole("dialog")).toContainText("自动退回没开：没通也会保持");
  await page.getByRole("dialog").getByRole("button", { name: "切换网络模式" }).click();
  const block = page.getByTestId("op-status");
  await expect(block).toContainText("正在确认");
  await expect(block.getByTestId("op-next")).toContainText("自动退回没开");
  // no readback of its own: no 「没有生效」 while datad is still checking
  await expect(page.getByText(/没有生效/)).toHaveCount(0);
});

test("auto revert just switched on: a one-time notice, Got it hides it on both sides (DD18)", async ({ ready, page, mock }) => {
  await mock.ops("notice");
  await ready.goto("/router/network-mode/");
  const note = page.getByTestId("op-notice");
  await expect(note).toContainText("自动退回已开：切模式没通会退回");
  await expect(note).toHaveAttribute("role", "status");
  // the confirm before a switch says it too
  await page.getByRole("radio", { name: "只用 4G" }).click({ force: true });
  await page.getByRole("button", { name: "应用" }).click();
  await expect(page.getByRole("dialog")).toContainText("自动退回已开：没通会退回");
  await page.keyboard.press("Escape");
  await note.getByRole("button", { name: "知道了" }).click();
  await expect.poll(async () => (await mock.ops()).acts).toEqual([{ act: "notice_ack" }]);
  await expect(page.getByTestId("op-notice")).toHaveCount(0);
});

test("the agent drops mid-change: result unknown, nothing resent (DD8)", async ({ ready, page, mock }) => {
  test.setTimeout(60000);
  await mock.ops("verifying");
  await ready.goto("/router/wifi/");
  await expect(page.getByTestId("op-banner")).toContainText("正在确认");
  await mock.scenario("down");
  await expect(page.getByTestId("op-banner")).toContainText("和设备断开了 · 操作结果未知", { timeout: 20000 });
  await mock.scenario("normal");
  await expect(page.getByTestId("op-banner")).toContainText("正在确认", { timeout: 15000 });
  expect((await mock.ops()).acts).toEqual([]);
});

test.describe("phone", () => {
  test.use({ viewport: { width: 390, height: 844 } });
  test("buttons are full width, ≥44 px and not under the tab bar (DD15)", async ({ ready, page, mock }) => {
    await mock.ops("verifying");
    await ready.goto("/router/network-mode/");
    const btn = page.getByTestId("op-status").getByRole("button", { name: "保留只用 5G SA" });
    await btn.scrollIntoViewIfNeeded();
    const box = (await btn.boundingBox())!;
    expect(box.height).toBeGreaterThanOrEqual(44);
    expect(box.width).toBeGreaterThan(300);
    const hit = await page.evaluate(([x, y]) => document.elementFromPoint(x, y)?.closest("button")?.textContent ?? "", [box.x + box.width / 2, box.y + box.height / 2]);
    expect(hit).toContain("保留只用 5G SA");
  });
});

test("the write errors after datad started the change: no 「没有生效」, the transaction shows (T8a rule)", async ({ ready, page, mock }) => {
  await mock.ops("idle-apply-fails");
  await ready.goto("/router/network-mode/");
  await page.getByRole("radio", { name: "只用 4G" }).click({ force: true });
  await page.getByRole("button", { name: "应用" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "切换网络模式" }).click();
  await expect(page.getByTestId("op-status")).toContainText("正在确认");
  await expect(page.getByText(/没有生效/)).toHaveCount(0);
});
