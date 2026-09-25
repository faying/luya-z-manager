// Routes of the static export: every directory under web/out that holds an
// index.html (the root included), minus Next internals and the dev kit.
import { readdirSync, statSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";


export const OUT_DIR = resolve(__dirname, "../../../out");

const SKIP = new Set(["_next", "_not-found", "dev-kit"]);

export function exportedRoutes(): string[] {
  if (!existsSync(join(OUT_DIR, "index.html"))) {
    throw new Error("web/out is missing — run `npm run build` first (or `npm run test:e2e`, which builds).");
  }
  const found: string[] = [];
  const walk = (dir: string, url: string) => {
    if (existsSync(join(dir, "index.html"))) found.push(url);
    for (const name of readdirSync(dir)) {
      if (SKIP.has(name) || name.startsWith("__next")) continue;
      const p = join(dir, name);
      if (statSync(p).isDirectory()) walk(p, `${url}${name}/`);
    }
  };
  walk(OUT_DIR, "/");
  return found.sort();
}

/** Routes rendered outside the app shell (no <main> from Shell, no login needed). */
export const OUTSIDE_SHELL = new Set(["/login/", "/404/"]);
