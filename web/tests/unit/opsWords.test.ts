// English words of the write-op layer on the web (E4 T14, DD7): the same rules
// as the touch screen's lang_test.c and docs/ui-glossary.md §13 — no Chinese,
// no "Please", and short status lines without a full stop. Full sentences
// (the *Detail / *Why / *Recovery / remoteDrop lines) may end with one.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((n) => {
    const p = join(dir, n);
    return statSync(p).isDirectory() ? files(p) : /\.tsx?$/.test(n) ? [p] : [];
  });
}

const DEFAULTS = new Map<string, string>();
for (const f of files(join(__dirname, "../../src"))) {
  for (const m of readFileSync(f, "utf8").matchAll(/t\("((?:ops|changes)\.[A-Za-z]+)",\s*"([^"]*)"/g)) {
    DEFAULTS.set(m[1], m[2]);
  }
}
const SENTENCE = /(Detail|Why|Recovery|remoteDrop)$/;

describe("write-op English words", () => {
  it("finds the strings", () => {
    expect(DEFAULTS.size).toBeGreaterThan(25);
  });
  for (const [key, en] of DEFAULTS) {
    it(key, () => {
      expect(en).not.toMatch(/[　-鿿＀-￯]/);
      expect(en).not.toMatch(/\bplease\b/i);
      if (!SENTENCE.test(key)) expect(en).not.toMatch(/\.$/);
    });
  }
});
