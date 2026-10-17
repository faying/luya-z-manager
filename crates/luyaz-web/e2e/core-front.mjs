// Stand-in for the core's (luyaz-core) /v2/api compat layer in front of `luyaz-web mock` (8d-2a: luyaz-web has no
// agent behind it, so every call the old checks sent to the typed mock agent now arrives as
// /v2/api/* on --upstream). /v2/api/* goes to the mock core first; where it says "not mine"
// (404/501 not_in_compat_yet), the call is answered by the fake agent (typed mock agent) at the
// same path without /v2, with a Bearer this front logs in for. Everything else (SSE, /v2/control,
// /mock/*) goes straight to the mock core. From source/manager:
//   node crates/luyaz-web/e2e/core-front.mjs <FRONT_PORT> <FAKE_AGENT_PORT> <MOCK_PORT>
// then luyaz-web serve --upstream http://host.docker.internal:<FRONT_PORT>.
import http from "node:http";

const [FRONT, AGENT, MOCK] = process.argv.slice(2).map(Number);
let token;
const call = (port, method, path, headers, body) => new Promise((ok, bad) => {
  const r = http.request({ host: "127.0.0.1", port, method, path, headers }, (res) => ok(res));
  r.on("error", bad);
  r.end(body);
});
const read = (res) => new Promise((ok) => { const c = []; res.on("data", (d) => c.push(d)); res.on("end", () => ok(Buffer.concat(c))); });
let logging;
// one login at a time: the typed mock honours only the newest token, so racing logins kill each other
const login = () => (logging ??= doLogin().finally(() => { logging = undefined; }));
async function doLogin() {
  const b = JSON.stringify({ password: "x" });
  const r = await call(AGENT, "POST", "/api/auth/login", { "content-type": "application/json", "content-length": Buffer.byteLength(b) }, b);
  token = JSON.parse((await read(r)).toString()).data.token;
}
let versionsChecked = Math.floor(Date.now() / 1000) - 3 * 3600;
http.createServer(async (req, res) => {
  const body = await read(req);
  const h = { ...req.headers, host: `127.0.0.1:${MOCK}` };
  try {
    // the core's /v2/versions (versions.rs), not in the mock core: answered here. Made up: the
    // touch UI has a newer release, the scripts are from before 1.0.0; a check makes it "now"
    if (req.url === "/v2/versions" || req.url === "/v2/versions/check") {
      if (req.url.endsWith("/check")) versionsChecked = Math.floor(Date.now() / 1000);
      const c = (name, installed, latest) => ({ name, installed, latest, update: latest !== installed });
      res.writeHead(200, { "content-type": "application/json" });
      return res.end(JSON.stringify({
        components: [c("core", "1.0.0", "1.0.0"), c("lweb", "1.0.0", "1.0.0"), c("sentinel", "1.0.0", "1.0.0"), c("touch", "1.0.0", "1.1.0"), c("scripts", null, "1.0.0")],
        update_available: true, checked_at: versionsChecked, ok_at: versionsChecked, error: null,
      }));
    }
    if (req.url.startsWith("/v2/api/")) {
      const m = await call(MOCK, req.method, req.url, h, body);
      const mb = await read(m);
      const notMine = [404, 501].includes(m.statusCode) && mb.toString().includes("not_in_compat_yet");
      if (!notMine) { res.writeHead(m.statusCode, m.headers); return res.end(mb); }
      const path = req.url.slice(3);
      for (let i = 0; i < 2; i++) {
        if (!token) await login();
        const used = token;
        const a = await call(AGENT, req.method, path, { ...req.headers, host: `127.0.0.1:${AGENT}`, authorization: `Bearer ${token}` }, body);
        if (a.statusCode === 401 && i === 0) { await read(a); if (token === used) token = undefined; continue; }
        const ab = await read(a);
        res.writeHead(a.statusCode, a.headers);
        return res.end(ab);
      }
    }
    const m = await call(MOCK, req.method, req.url, h, body);
    res.writeHead(m.statusCode, m.headers);
    m.pipe(res);
    res.on("close", () => m.destroy());
  } catch {
    res.writeHead(502); res.end();
  }
}).listen(FRONT, "0.0.0.0");
