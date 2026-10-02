// English mode (docs/designs/ui-english.md T7, DT9; playwright project "en").
//
// Every exported route, in English, on a 375×812 phone and on a desktop:
//   - no Chinese on the page except what is the user's or the network's own
//     text (SMS, names, SSIDs, proxy groups…), listed in RAW below
//   - the agent's English really shows (POSITIVE), so an empty page can't pass
//   - at 375: no sideways scroll, and no button or segment label on two lines
import { test, expect } from "./support/fixtures";
import { exportedRoutes, OUTSIDE_SHELL } from "./support/routes";

const SIZES = [
  { name: "375", width: 375, height: 812 },
  { name: "1440", width: 1440, height: 1000 },
] as const;

/** Han, CJK punctuation and full-width forms (so a stray "（" is caught too). */
const CJK = /[　-〿㐀-鿿豈-﫿＀-￯]+/g;

/**
 * Text that stays as it is in English mode: it is not ours to translate.
 * Checked per line of each text node: a line with Chinese passes when all of
 * it is part of one entry (pages may show a cut-off preview) or matches
 * RAW_LINES, or when no Chinese is left once whole entries are taken out of it
 * ("In use: 台湾旅行"). So "中国台湾 台北市 · 中华电信" fails even though an
 * SMS here mentions 中国台湾. Everything comes from the mock agent's data.
 */
const RAW: string[] = [
  // SMS bodies and the forwarding rule (sms.ts), USSD/STK menus (telephony.ts)
  "【中華電信】歡迎您來到台灣！您已登錄中華電信網路，漫遊服務已開通。撥打 800 可查詢漫遊資費，祝您旅途愉快。",
  "【漫游提醒】尊敬的客户，您已抵达中国台湾地区。境外流量包 25元/天，达 3GB 后限速，当日有效。退订请回复 TD。",
  "【星海銀行】您的驗證碼為 482913，5 分鐘內有效。本行不會以任何方式向您索取驗證碼，請勿告知他人。",
  "【星海銀行】您的驗證碼為 663015，用於綁定新裝置，5 分鐘內有效。",
  "到机场了吗？我们在出境大厅 3 号门等你，车停在 P2 🙂",
  "验证码 → 手机推送",
  "验证码",
  "驗證碼",
  // the network's broadcast name and the APN profile named after it (network.ts, router.ts)
  "中華電信",
  // eSIM nicknames (esim.ts), scheduler job names (system.ts)
  "台湾旅行",
  "国内主号",
  "夜间省电",
  "每周一凌晨重启",
];

/** Log lines, shown as the device wrote them (names inside are the user's). */
const RAW_LINES: RegExp[] = [
  /^\d{4}-\d\d-\d\dT[\d:]+ (entering scenario "[^"]+" \(\w+\)|saved "[^"]+" to restore on leaving)$/,
];

/** Agent wording that must show in English, per route. */
const POSITIVE: Record<string, string[]> = {
  "/": ["Chunghwa Telecom", "Taiwan", "Abroad"],
  "/health/": ["ZTE auto-update", "Supervised by procd"],
  "/alerts/": ["Admin backend (zte-agent) exited unexpectedly"],
  "/router/scenario/": ["Abroad"],
};

function strayChinese(lines: string[]): string[] {
  const hasCjk = new RegExp(CJK.source);
  const byLength = [...RAW].sort((a, b) => b.length - a.length);
  return lines.filter((line) => {
    if (!hasCjk.test(line)) return false;
    const t = line.replace(/…$/, "");
    // all of it is (a cut-off piece of) the user's text, or a log line
    if (RAW.some((r) => r.includes(t)) || RAW_LINES.some((re) => re.test(t))) return false;
    // our sentence around the user's text ("In use: 台湾旅行"): only the whole entries come out
    const rest = byLength.reduce((x, r) => x.split(r).join(" "), t);
    return hasCjk.test(rest);
  });
}

for (const route of exportedRoutes()) {
  for (const size of SIZES) {
    test.describe(`${route} @${size.name} en`, () => {
      test.use({
        viewport: { width: size.width, height: size.height },
        setup: { authed: !OUTSIDE_SHELL.has(route), lang: "en", theme: "light" },
      });
      test("english", async ({ ready, page }) => {
        await ready.goto(route);
        await expect(page.locator("html")).toHaveAttribute("lang", "en");
        for (const s of POSITIVE[route] ?? []) await expect(page.getByText(s, { exact: false }).first()).toBeAttached();

        // Lines of the visible text nodes, minus text marked as Chinese on purpose (the 中 of the language switch).
        const nodes = await page.evaluate(() => {
          const out: string[] = [];
          const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
          for (let n = walk.nextNode(); n; n = walk.nextNode()) {
            const el = n.parentElement;
            if (!el || el.closest("[lang^=zh], script, style") || !el.checkVisibility()) continue;
            for (const line of (n.textContent ?? "").split("\n")) {
              const t = line.replace(/\s+/g, " ").trim();
              if (t) out.push(t);
            }
          }
          return out;
        });
        expect(strayChinese(nodes), "Chinese on an English page").toEqual([]);

        if (size.width === 375) {
          const { sw, iw } = await page.evaluate(() => ({ sw: document.documentElement.scrollWidth, iw: window.innerWidth }));
          expect(sw, `horizontal scroll: scrollWidth ${sw} > innerWidth ${iw}`).toBeLessThanOrEqual(iw);
          // Buttons and segment labels stay on one line (design review decision 12).
          const wrapped = await page.evaluate(() => {
            const bad: string[] = [];
            for (const el of document.querySelectorAll<HTMLElement>("button, [role=button], .nd-seg__item")) {
              if (!el.offsetParent || !el.textContent?.trim()) continue;
              // A whole row that is a button (an SMS in the list) has its own lines by design.
              const blocky = [...el.querySelectorAll<HTMLElement>("*")].some((d) =>
                /^(block|flex|grid|list-item)$/.test(getComputedStyle(d).display)
              );
              if (blocky) continue;
              // Line boxes of the text only (an icon beside it sits at its own height).
              const rects: DOMRect[] = [];
              const walk = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
              for (let n = walk.nextNode(); n; n = walk.nextNode()) {
                if (!n.textContent?.trim()) continue;
                const r = document.createRange();
                r.selectNodeContents(n);
                rects.push(...[...r.getClientRects()].filter((x) => x.width > 0));
              }
              rects.sort((a, b) => a.top - b.top);
              let lines = 0;
              let bottom = -Infinity;
              for (const x of rects) {
                if (x.top >= bottom - 2) lines++;
                bottom = Math.max(bottom, x.bottom);
              }
              if (lines > 1) bad.push(el.textContent.trim().slice(0, 40));
            }
            return bad;
          });
          expect(wrapped, "buttons or segments wrapped onto two lines").toEqual([]);
        }
        expect(ready.errors, "page errors / console errors").toEqual([]);
      });
    });
  }
}
