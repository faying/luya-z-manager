// APN: with two or more carrier candidates (scenario apn-candidates, as on the
// owner's device 2026-10-03: ctiot dialled first, ctnet second), one can be
// used for this SIM only (netinfo.rs apn_use → apn_pick.rs).
import { test, expect } from "./support/fixtures";

test.use({ freshMock: true, viewport: { width: 1440, height: 1000 } });

// The write waits for the re-dial (NET_WAIT_SEC = 30) before reading back.
test.setTimeout(120_000);

test.beforeEach(async ({ mock }) => {
  await mock.scenario("apn-candidates");
});

test("the IoT candidate is tagged, and 'use for this SIM' sends the candidate id and reads it back", async ({ ready, page }) => {
  await ready.goto("/router/apn/");
  await expect(page.getByText("物联网", { exact: true })).toBeVisible();
  await expect(page.getByText(/只给这张卡用：换别的卡会回到「自动」/)).toBeVisible();

  const since = ready.requests.length;
  await page.getByRole("button", { name: "只给这张卡用 ctnet" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("只有这张卡用它");
  await dialog.getByRole("button", { name: "只给这张卡用" }).click();
  await expect
    .poll(() => ready.writes(since).filter((w) => w.method === "POST" && w.path === "/api/netinfo/apn").length, { timeout: 10_000 })
    .toBe(1);
  const body = JSON.parse(ready.writes(since).find((w) => w.path === "/api/netinfo/apn")?.body ?? "{}");
  expect(body).toEqual({ id: "auto109600" });
  await expect(page.getByText("本卡在用")).toBeVisible({ timeout: 60_000 });
  // The picked candidate no longer offers the button; the other one still does.
  await expect(page.getByRole("button", { name: "只给这张卡用 ctnet" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "只给这张卡用 ctiot" })).toBeVisible();
});

test("one candidate only: no pick button, read-only note as before", async ({ ready, page, mock }) => {
  await mock.scenario("normal");
  await ready.goto("/router/apn/");
  await expect(page.getByText("由运营商提供 —— 只读。")).toBeVisible();
  await expect(page.getByRole("button", { name: /^只给这张卡用/ })).toHaveCount(0);
});
