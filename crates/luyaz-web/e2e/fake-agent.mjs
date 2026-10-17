// A stand-in for the agent on one port: /api/* and /__mock/* from the typed mock agent
// (e2e/mock-agent), anything else 404 (the old Next.js pages it used to serve from web/out are
// gone, X13). From anywhere:
//   node crates/luyaz-web/e2e/fake-agent.mjs 19090
import http from "node:http";
import { spawn } from "node:child_process";

const PORT = Number(process.argv[2] ?? 19090);
const MOCK = PORT + 2;
const SERVER = new URL("./mock-agent/server.ts", import.meta.url).pathname;
const kids = [
  spawn(process.execPath, [SERVER], { env: { ...process.env, MOCK_PORT: String(MOCK), MOCK_QUIET: "1" }, stdio: "inherit" }),
];
process.on("exit", () => kids.forEach((k) => k.kill()));
process.on("SIGINT", () => process.exit(0));
process.on("SIGTERM", () => process.exit(0));

// /api/usb/status gets `mode_settable` as the agent adds it (false: the firmware has no setter,
// as on CN B31; absent: could not be checked, like the typed mock). POST /__lw/usb?settable=
// true|false|absent picks it (default absent).
let settable = "absent";

// /router/esim (e2e/router-esim.mjs): GET /__lw/esim = how many POSTs reached /api/esim/* (path only,
// never a body); POST /__lw/esim/reset = a fresh mock agent (the card and its 300 s switch cooldown
// live in the mock's memory; its tokens too, so log in again after).
const esimPosts = {};
const devicePosts = {};
const mockUp = () => new Promise((ok) => {
  const t = setInterval(() => http.get({ host: "127.0.0.1", port: MOCK, path: "/__mock/state" }, (r) => { r.resume(); clearInterval(t); ok(); }).on("error", () => {}), 200);
});

// /sms/forward (e2e/sms-forward.mjs): GET /__lw/fwd-config = the stored forwarding config, asked
// with the Bearer luyaz-web last sent (the mock honours only its newest token; tabs may not read
// the config raw once the page is migrated, proxy.rs secret_read).
let lastAuth;
let holdExit = false;
let slowExit = 0;


function handler(req, res) {
  if (req.headers.authorization) lastAuth = req.headers.authorization;
  if (req.url === "/__lw/fwd-config") {
    const up = http.get({ host: "127.0.0.1", port: MOCK, path: "/api/sms/forward/config", headers: lastAuth ? { authorization: lastAuth } : {} }, (r) => {
      res.writeHead(r.statusCode, { "content-type": "application/json" });
      r.pipe(res);
    });
    up.on("error", () => { res.writeHead(502); res.end(); });
    return;
  }
  // /router/scenario (e2e/scenario.mjs): GET /__lw/scene = the engine state as stored (asked with the
  // Bearer luyaz-web last sent); PUT /__lw/scene = replace the config behind the page's back
  // (an empty scenarios list: the "not set up" card; no abroad scenario: "Add abroad scenario").
  if (req.url === "/__lw/scene") {
    const up = http.request({ host: "127.0.0.1", port: MOCK, path: "/api/scenario", method: req.method === "PUT" ? "PUT" : "GET", headers: { ...(lastAuth ? { authorization: lastAuth } : {}), "content-type": "application/json" } }, (r) => {
      res.writeHead(r.statusCode, { "content-type": "application/json" });
      r.pipe(res);
    });
    up.on("error", () => { res.writeHead(502); res.end(); });
    return req.pipe(up);
  }
  if (req.url === "/__lw/esim") {
    res.writeHead(200, { "content-type": "application/json" });
    return res.end(JSON.stringify(esimPosts));
  }
  if (req.url === "/__lw/esim/reset") {
    for (const k of Object.keys(esimPosts)) delete esimPosts[k];
    kids[0].once("exit", () => {
      kids[0] = spawn(process.execPath, [SERVER], { env: { ...process.env, MOCK_PORT: String(MOCK), MOCK_QUIET: "1" }, stdio: "inherit" });
      mockUp().then(() => { res.writeHead(204); res.end(); });
    });
    return void kids[0].kill();
  }
  if (req.method === "POST" && req.url.startsWith("/api/esim/")) esimPosts[req.url] = (esimPosts[req.url] ?? 0) + 1;
  // /router/device (e2e/device.mjs): GET /__lw/device = how many POSTs reached reboot / poweroff /
  // factory-reset (path only, never a body); POST /__lw/device/reset zeroes them.
  if (req.url === "/__lw/device/reset") {
    for (const k of Object.keys(devicePosts)) delete devicePosts[k];
    res.writeHead(204);
    return res.end();
  }
  if (req.url === "/__lw/device") {
    res.writeHead(200, { "content-type": "application/json" });
    return res.end(JSON.stringify(devicePosts));
  }
  if (req.method === "POST" && /^\/api\/device\/(reboot|poweroff|factory-reset)$/.test(req.url)) devicePosts[req.url] = (devicePosts[req.url] ?? 0) + 1;
  if (req.url.startsWith("/__lw/usb")) {
    settable = new URL(req.url, "http://x").searchParams.get("settable") ?? "absent";
    res.writeHead(204);
    return res.end();
  }
  if (req.method === "GET" && req.url.startsWith("/api/usb/status") && settable !== "absent") {
    const up = http.request({ host: "127.0.0.1", port: MOCK, path: req.url, method: "GET", headers: req.headers }, (r) => {
      let body = "";
      r.setEncoding("utf8");
      r.on("data", (c) => (body += c));
      r.on("end", () => {
        try {
          const v = JSON.parse(body);
          if (v?.data && typeof v.data === "object") v.data.mode_settable = settable === "true";
          body = JSON.stringify(v);
        } catch { /* not JSON: as it is */ }
        const h = { ...r.headers };
        delete h["content-length"];
        delete h["content-encoding"];
        res.writeHead(r.statusCode, h);
        res.end(body);
      });
    });
    up.on("error", () => { res.writeHead(502); res.end(); });
    return req.pipe(up);
  }
  // / (e2e/home.mjs): POST /__lw/home?hold=1 makes GET /api/services/chill keep saying exit "proxy"
  // (the exit write is accepted but the readback never shows it); ?hold=0 undoes it.
  if (req.url.startsWith("/__lw/home")) {
    const q = new URL(req.url, "http://x").searchParams;
    if (q.has("hold")) holdExit = q.get("hold") === "1";
    // ?slow=<ms>: the exit PUT answers that much later, so the pending state is there to be seen
    if (q.has("slow")) slowExit = Number(q.get("slow")) || 0;
    res.writeHead(204);
    return res.end();
  }
  if (slowExit && req.method === "PUT" && req.url.split("?")[0] === "/api/services/chill/exit" && !req.__slowed) {
    req.__slowed = true;
    req.pause();
    return void setTimeout(() => { req.resume(); handler(req, res); }, slowExit);
  }
  if (holdExit && req.method === "GET" && req.url.split("?")[0] === "/api/services/chill") {
    const up = http.request({ host: "127.0.0.1", port: MOCK, path: req.url, method: "GET", headers: req.headers }, (r) => {
      let body = "";
      r.setEncoding("utf8");
      r.on("data", (c) => (body += c));
      r.on("end", () => {
        try { const v = JSON.parse(body); if (v?.data?.exit) v.data.exit = "proxy"; body = JSON.stringify(v); } catch { /* as it is */ }
        const h = { ...r.headers };
        delete h["content-length"];
        delete h["content-encoding"];
        res.writeHead(r.statusCode, h);
        res.end(body);
      });
    });
    up.on("error", () => { res.writeHead(502); res.end(); });
    return req.pipe(up);
  }
  const api = req.url.startsWith("/api/") || req.url.startsWith("/__mock/");
  if (!api) { res.writeHead(404); return res.end(); }
  const up = http.request({ host: "127.0.0.1", port: MOCK, path: req.url, method: req.method, headers: req.headers }, (r) => {
    res.writeHead(r.statusCode, r.headers);
    r.pipe(res);
  });
  up.on("error", () => { res.writeHead(502); res.end(); });
  req.pipe(up);
}

http.createServer(handler).listen(PORT, "0.0.0.0", () => console.log(`[fake-agent] :${PORT} (mock :${MOCK})`));
