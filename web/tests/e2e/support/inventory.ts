// Parser for web/docs/controls-inventory.md (the pre-redesign control list)
// used by inventory.spec.ts (design doc E9).
//
// Every "#### 控件" table row has an accessible-name cell (column 2). The
// parser pulls role「name」 pairs out of it. A 「name」 without a role right
// before it takes the last role word seen in the cell, so
// `radio「自动」「手动」` and `button「高级」/「隐藏」` both work. Placeholders
// become patterns: `{名称}` → any text, a standalone `N` → digits.
//
// Markers added to the cell after the redesign (the original text stays as
// the old name):
//   〔现名：button「新名字」〕  renamed — only the pairs inside are checked
//   〔交互后：原因〕            only there after an interaction (tab, dialog,
//                              form, a selection…). If the reason spells out
//                              steps — `点 button「添加配置」` (click; several
//                              run in order), `填 textbox「名字」=值` (fill),
//                              `场景 down` (mock scenario before loading),
//                              `载入后场景 down` (switch after the page has
//                              data — e.g. "lost the connection");
//                              `{…}` in a step name matches any text — the
//                              check performs them and
//                              then looks for the name ("verified"); without
//                              steps the row is listed as unverified.
//   〔缺失：说明〕              known missing — listed; the check fails if it
//                              turns up again, so the doc gets updated
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export const INVENTORY_PATH = resolve(__dirname, "../../../docs/controls-inventory.md");

export const ROLES = [
  "button",
  "link",
  "textbox",
  "searchbox",
  "switch",
  "checkbox",
  "radio",
  "radiogroup",
  "combobox",
  "listbox",
  "option",
  "spinbutton",
  "slider",
  "group",
  "toolbar",
  "tab",
  "tablist",
  "tabpanel",
  "dialog",
  "region",
  "heading",
  "menuitem",
  "status",
  "alert",
  "progressbar",
  "table",
  "row",
  "navigation",
] as const;
export type Role = (typeof ROLES)[number];

export type NameSpec = { role: Role | null; name: string; pattern: RegExp | null };

export type Row = {
  /** Section heading, e.g. "/login 登录" or "全局（每个登录后页面都有）". */
  section: string;
  /** Route of the static export this row is checked on ("/" for 全局). */
  route: string;
  /** Line number in the markdown (1-based). */
  line: number;
  control: string;
  cell: string;
  names: NameSpec[];
  renamed: boolean;
  interaction: string | null;
  /** Steps parsed from the 〔交互后〕 text (empty = unverified). */
  steps: Step[];
  missing: string | null;
};

function parseSteps(text: string): Step[] {
  const steps: Step[] = [];
  const re = /点\s*([a-z]+)「([^」]+)」|填\s*([a-z]+)「([^」]+)」=([^\s，；〕]+)|(载入后)?场景\s+([a-z0-9,-]+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m[1] && ROLE_SET.has(m[1])) steps.push({ kind: "click", role: m[1] as Role, name: m[2], pattern: toPattern(m[2]) });
    else if (m[3] && ROLE_SET.has(m[3])) steps.push({ kind: "fill", role: m[3] as Role, name: m[4], pattern: toPattern(m[4]), value: m[5] });
    else if (m[7]) steps.push({ kind: m[6] ? "scenarioAfter" : "scenario", set: m[7] });
  }
  return steps;
}

export type Step =
  | { kind: "click"; role: Role; name: string; pattern: RegExp | null }
  | { kind: "fill"; role: Role; name: string; pattern: RegExp | null; value: string }
  | { kind: "scenario"; set: string }
  | { kind: "scenarioAfter"; set: string };

export type TableProblem = { line: number; expected: number; got: number; text: string };

/** Split a markdown table row into cells (pipes inside `code` don't split). */
export function splitRow(line: string): string[] {
  const cells: string[] = [];
  let cur = "";
  let code = false;
  const body = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  for (let i = 0; i < body.length; i++) {
    const ch = body[i];
    if (ch === "`") code = !code;
    if (ch === "\\" && body[i + 1] === "|") {
      cur += "|";
      i++;
      continue;
    }
    if (ch === "|" && !code) {
      cells.push(cur.trim());
      cur = "";
      continue;
    }
    cur += ch;
  }
  cells.push(cur.trim());
  return cells;
}

const ROLE_SET = new Set<string>(ROLES);
const MARK = /〔(现名|交互后|缺失)：([^〕]*)〕/g;

function toPattern(name: string): RegExp | null {
  const hasBrace = /\{[^}]+\}/.test(name);
  const hasN = /(^|[^A-Za-z])N(?![A-Za-z])/.test(name);
  if (!hasBrace && !hasN) return null;
  let src = "";
  const parts = name.split(/(\{[^}]+\}|(?<![A-Za-z])N(?![A-Za-z]))/);
  for (const p of parts) {
    if (!p) continue;
    if (/^\{[^}]+\}$/.test(p)) src += ".+?";
    else if (p === "N") src += "\\d+";
    else src += p.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }
  return new RegExp(`^${src}$`);
}

/** Drop （…） notes that sit outside 「…」 names. */
function stripNotes(text: string): string {
  let out = "";
  let inName = 0;
  let inNote = 0;
  for (const ch of text) {
    if (!inNote && ch === "「") inName++;
    if (!inNote && ch === "」") inName = Math.max(0, inName - 1);
    if (!inName && ch === "（") inNote++;
    if (!inNote) out += ch;
    if (!inName && ch === "）") inNote = Math.max(0, inNote - 1);
  }
  return out;
}

/** Pull role「name」 pairs out of free text. */
export function extractNames(text: string): NameSpec[] {
  const out: NameSpec[] = [];
  let role: Role | null = null;
  // Tokens: a role word, or a 「…」 name (optionally glued to a role word).
  const re = /([a-z]+)?「([^」]+)」|\b([a-z]+)\b/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m[2] !== undefined) {
      if (m[1] && ROLE_SET.has(m[1])) role = m[1] as Role;
      const name = m[2].trim();
      if (name) out.push({ role, name, pattern: toPattern(name) });
    } else if (m[3] && ROLE_SET.has(m[3])) {
      role = m[3] as Role;
    }
  }
  return out;
}

export function sectionRoute(section: string): string | null {
  if (section.startsWith("全局")) return "/";
  const m = /^(\/[^\s]*)/.exec(section);
  if (!m) return null;
  return m[1] === "/" ? "/" : `${m[1].replace(/\/$/, "")}/`;
}

export function parseInventory(md = readFileSync(INVENTORY_PATH, "utf8")): { rows: Row[]; problems: TableProblem[] } {
  const lines = md.split("\n");
  const rows: Row[] = [];
  const problems: TableProblem[] = [];
  let section = "";
  let inControls = false;
  let headerCells = 0;
  let sawSep = false;
  lines.forEach((raw, i) => {
    const line = raw.trimEnd();
    if (line.startsWith("## ")) {
      section = line.slice(3).trim();
      inControls = false;
      return;
    }
    if (line.startsWith("#### ")) {
      inControls = line.slice(5).trim() === "控件";
      headerCells = 0;
      sawSep = false;
      return;
    }
    if (line.startsWith("### ")) {
      inControls = false;
      return;
    }
    if (!inControls || !line.startsWith("|")) {
      if (inControls && headerCells && line.trim() === "") {
        headerCells = 0;
        sawSep = false;
      }
      return;
    }
    const cells = splitRow(line);
    if (!headerCells) {
      headerCells = cells.length;
      return;
    }
    if (!sawSep && /^\|[\s|:-]+\|$/.test(line)) {
      sawSep = true;
      return;
    }
    if (cells.length !== headerCells) problems.push({ line: i + 1, expected: headerCells, got: cells.length, text: line.slice(0, 120) });
    const route = sectionRoute(section);
    if (!route) return;
    const cell = cells[1] ?? "";
    let renamedText: string | null = null;
    let interaction: string | null = null;
    let missing: string | null = null;
    for (const mk of cell.matchAll(MARK)) {
      // Notes in （…） after the new name are prose, not names.
      if (mk[1] === "现名") renamedText = (renamedText ?? "") + stripNotes(mk[2]);
      if (mk[1] === "交互后") interaction = mk[2].trim();
      if (mk[1] === "缺失") missing = mk[2].trim();
    }
    const base = cell.replace(MARK, "");
    const names = renamedText !== null ? extractNames(renamedText) : extractNames(base);
    rows.push({
      section,
      route,
      line: i + 1,
      control: cells[0] ?? "",
      cell,
      names,
      renamed: renamedText !== null,
      interaction,
      steps: interaction ? parseSteps(interaction) : [],
      missing,
    });
  });
  return { rows, problems };
}
