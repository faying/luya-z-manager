// Prints the inventory summary (found / renamed / interaction-only / missing)
// after a run that included inventory.spec.ts, and writes it to
// test-results/inventory-summary.md.
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

type RowResult = {
  route: string;
  line: number;
  control: string;
  status: string;
  how?: string;
  detail?: string;
};

const STATUSES = ["found", "renamed", "interaction ✓", "interaction", "missing", "NOT FOUND", "interaction ✗", "missing-but-found"] as const;

export default function globalTeardown() {
  const dir = resolve(__dirname, "../../../test-results/inventory");
  if (!existsSync(dir)) return;
  const all: RowResult[] = readdirSync(dir)
    .filter((f) => f.endsWith(".json"))
    .flatMap((f) => JSON.parse(readFileSync(resolve(dir, f), "utf8")) as RowResult[]);
  if (!all.length) return;
  const routes = [...new Set(all.map((r) => r.route))].sort();
  const n = (rs: RowResult[], s: string) => rs.filter((r) => r.status === s).length;

  const lines: string[] = [];
  lines.push("# Controls inventory check", "");
  lines.push(`| route | ${STATUSES.join(" | ")} |`, `|---|${STATUSES.map(() => "---:").join("|")}|`);
  for (const route of routes) {
    const rs = all.filter((r) => r.route === route);
    lines.push(`| ${route} | ${STATUSES.map((s) => n(rs, s) || "").join(" | ")} |`);
  }
  lines.push(`| **total (${all.length})** | ${STATUSES.map((s) => `**${n(all, s)}**`).join(" | ")} |`, "");

  const list = (title: string, s: string) => {
    const rs = all.filter((r) => r.status === s);
    if (!rs.length) return;
    lines.push(`## ${title}`, "");
    for (const r of rs) lines.push(`- ${r.route} (L${r.line}) ${r.control} — ${r.detail ?? ""}${r.how ? ` (found as ${r.how})` : ""}`);
    lines.push("");
  };
  list("MISSING (known, marked 〔缺失〕)", "missing");
  list("NOT FOUND (unmarked — fix the page or the inventory)", "NOT FOUND");
  list("Marked missing but found again", "missing-but-found");
  list("Interaction steps ran but the control did not appear", "interaction ✗");
  list("Interaction-only (not checked)", "interaction");
  list("Interaction-only (verified by performing the steps)", "interaction ✓");
  const ren = all.filter((r) => (r as RowResult & { renamed?: boolean }).renamed);
  if (ren.length) {
    lines.push(`## Rows with a new name (〔现名〕, ${ren.length}, any status)`, "");
    for (const r of ren) lines.push(`- ${r.route} (L${r.line}) ${r.control} — ${r.status}${r.how ? `: ${r.how}` : ""}`);
    lines.push("");
  }

  const text = lines.join("\n");
  writeFileSync(resolve(__dirname, "../../../test-results/inventory-summary.md"), text);
  const head = text.split("\n## Interaction-only (not checked)")[0];
  console.log(`\n${head}\n(full list incl. interaction-only rows: test-results/inventory-summary.md)\n`);
}
