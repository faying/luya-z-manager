// Contrast table for the new-design tokens (src/app/newdesign.css).
// Run: node scripts/contrast.ts > docs/contrast.md
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../src/app/newdesign.css", import.meta.url), "utf8");

function block(selector: string): Record<string, string> {
  const i = css.indexOf(selector);
  const body = css.slice(css.indexOf("{", i) + 1, css.indexOf("}", i));
  const out: Record<string, string> = {};
  for (const m of body.matchAll(/--nd-([\w-]+):\s*(#[0-9a-f]{6})/gi)) out[m[1]] = m[2];
  return out;
}
const themes = { light: block(':root,\n[data-theme="light"]'), dark: block('[data-theme="dark"] {') };

function lum(hex: string): number {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const l = c.map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2];
}
function ratio(a: string, b: string): number {
  const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

type Pair = [fg: string, bg: string, min: number, use: string];
const pairs: Pair[] = [
  ...["t1", "t2", "t3"].flatMap((f) =>
    ["bg", "card", "track", "wash", "washW", "washB", "accS"].map((b): Pair => [f, b, 4.5, "文字"]),
  ),
  ...["okT", "warnT", "badT", "accT"].flatMap((f) =>
    ["bg", "card", "wash", "washW", "washB", "accS", "track"].map((b): Pair => [f, b, 4.5, "状态词 / 链接"]),
  ),
  ...["fillBlue", "fillGreen", "fillOrange", "fillRed"].map((b): Pair => ["onFill", b, 4.5, "按钮白字"]),
  ["onPrimary", "primary", 4.5, "主按钮 / 选中项文字"],
  ["onConfirm", "confirm", 4.5, "确认按钮文字"],
  ["primary", "bg", 3, "主按钮 / 开关（非文字）"],
  ["primary", "card", 3, "主按钮 / 开关（非文字）"],
  ...["okMark", "warnMark", "badMark"].flatMap((f) =>
    ["wash", "washW", "washB", "track"].map((b): Pair => [f, b, 3, "色块里的状态符号（非文字）"]),
  ),
  ["focus", "bg", 3, "焦点环（非文字）"],
  ["focus", "card", 3, "焦点环（非文字）"],
  ...["blkCharts", "blkNetwork", "blkWifi", "blkServices", "blkMessages", "blkSystem"].flatMap((b) =>
    ["t1", "t2"].map((f): Pair => [f, b, 4.5, "分类色块文字（色块内 t3 映射为 t2）"]),
  ),
  ...["blkCharts", "blkNetwork", "blkWifi", "blkServices", "blkMessages", "blkSystem"].map((b): Pair => ["okMark", b, 3, "色块里的状态符号（非文字）"]),
  ["onBand", "band", 4.5, "深绿带文字"],
  ["bandAccent", "band", 3, "深绿带状态符号"],
  ["console-t1", "console-bg", 4.5, "深色带文字"],
  ["console-t2", "console-bg", 4.5, "深色带文字"],
  ["console-t1", "console-card", 4.5, "深色带文字"],
  ["console-t2", "console-card", 4.5, "深色带文字"],
  ...["okMark", "warnMark", "badMark", "accMark"].flatMap((f) =>
    ["card", "bg"].map((b): Pair => [f, b, 3, "状态点 / 开关轨道 / 图表线（非文字）"]),
  ),
  ["t3", "card", 3, "输入框边界（非文字）"],
];

const lines = [
  "# 新设计对比度表",
  "",
  "由 `node scripts/contrast.ts` 从 `src/app/newdesign.css` 生成，不要手改。文字 ≥ 4.5:1；非文字元素（WCAG 1.4.11）≥ 3:1。",
  "",
];
let fails = 0;
for (const [name, t] of Object.entries(themes)) {
  const light = themes.light;
  lines.push(`## ${name === "light" ? "浅色" : "深色"}`, "", "| 前景 | 背景 | 对比度 | 要求 | 结果 | 用途 |", "|---|---|---:|---:|---|---|");
  for (const [f, b, min, use] of pairs) {
    const fg = t[f] ?? light[f];
    const bg = t[b] ?? light[b];
    if (!fg || !bg) continue;
    const r = ratio(fg, bg);
    const ok = r >= min;
    if (!ok) fails++;
    lines.push(`| ${f} \`${fg}\` | ${b} \`${bg}\` | ${r.toFixed(2)} | ${min} | ${ok ? "通过" : "**不通过**"} | ${use} |`);
  }
  lines.push("");
}
lines.push(fails ? `**${fails} 项不通过。**` : "全部通过。");
console.log(lines.join("\n"));
process.exitCode = fails ? 1 : 0;
