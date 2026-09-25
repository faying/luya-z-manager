// Tiny static server for the `next build` export in web/out (e2e only).
// Usage: node tests/e2e/support/serve-out.mjs [port]   (default 4391, 127.0.0.1)
// Mirrors how zte-agent serves the admin UI: /a/b/ → out/a/b/index.html,
// /a/b → out/a/b.html or out/a/b/index.html, unknown → out/404.html (404).
import http from "node:http";
import { createReadStream, existsSync, statSync } from "node:fs";
import { extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(fileURLToPath(new URL("../../../out", import.meta.url)));
const PORT = Number(process.argv[2] ?? process.env.E2E_WEB_PORT ?? 4391);
const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".txt": "text/plain; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".ttf": "font/ttf",
  ".webmanifest": "application/manifest+json",
};

if (!existsSync(join(ROOT, "index.html"))) {
  console.error(`[serve-out] ${ROOT}/index.html not found — run \`npm run build\` first`);
  process.exit(1);
}

function isFile(p) {
  try {
    return statSync(p).isFile();
  } catch {
    return false;
  }
}

function resolvePath(urlPath) {
  const rel = normalize(decodeURIComponent(urlPath)).replace(/^([/\\])+/, "");
  const abs = join(ROOT, rel);
  if (abs !== ROOT && !abs.startsWith(ROOT + sep)) return null;
  for (const c of [abs, join(abs, "index.html"), abs + ".html"]) if (isFile(c)) return c;
  return null;
}

http
  .createServer((req, res) => {
    const path = new URL(req.url ?? "/", "http://x").pathname;
    let file = resolvePath(path);
    let status = 200;
    if (!file) {
      file = join(ROOT, "404.html");
      status = 404;
    }
    res.writeHead(status, { "Content-Type": TYPES[extname(file)] ?? "application/octet-stream", "Cache-Control": "no-store" });
    createReadStream(file).pipe(res);
  })
  .listen(PORT, "127.0.0.1", () => console.log(`[serve-out] ${ROOT} on http://127.0.0.1:${PORT}`));
