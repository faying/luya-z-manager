// Proxy in front of the fake agent: rewrites its China/Taiwan scenario into one synthetic story
// (SIM + network "Example Mobile" 001/01, home, not roaming). node rewrite-agent.mjs <LISTEN> <UPSTREAM> [BIND]
// web.sh runs two: before the fake agent, and before core-front (the mock core's own fixtures).
import http from "node:http";
const [LISTEN, AGENT] = process.argv.slice(2).map(Number);
const rules = [
  [/Chunghwa Telecom|China Unicom|China Mobile|中华电信|中華電信|中国联通|中国移动/g, "Example Mobile"],
  [/中国台湾 台北市|中国台湾|中国/g, "Exampleland"], [/\b(Taiwan|China)\b/g, "Exampleland"], [/"(TW|CN)"/g, '"ZZ"'],
  [/("mcc"\s*:\s*)"(460|466)"/g, '$1"001"'], [/("mnc"\s*:\s*)"(92|01|00)"/g, '$1"01"'],
  [/("mcc"\s*:\s*)(460|466)\b/g, "$11"], [/("mnc"\s*:\s*)(92)\b/g, "$11"],
  [/4669[2]|46001|46000/g, "00101"], [/460000000000001/g, "001010000000001"],
  [/89860000000000000001/g, "8900000000000000000"], [/203\.0\.113\.24/g, "198.51.100.23"],
  [/台湾旅行/g, "Travel eSIM"], [/国内主号/g, "Home Line"],
  [/🇹🇼/g, "🇯🇵"], [/台湾/g, "Japan"],
  [/"roaming"\s*:\s*true/g, '"roaming":false'],
  // phone numbers: +886 9000001xx → +1 555 01xx (the fictional range)
  [/\+8869000001(\d\d)/g, "+155501$1"], [/\+886900000000/g, "+15550100"],
];
http.createServer((req, res) => {
  const c = []; req.on("data", (d) => c.push(d)); req.on("end", () => {
    const r = http.request({ host: "127.0.0.1", port: AGENT, method: req.method, path: req.url, headers: { ...req.headers, host: `127.0.0.1:${AGENT}`, "accept-encoding": "identity" } }, (up) => {
      // JSON is rewritten whole; anything else (the SSE stream) passes through as it comes
      if (!/json/.test(up.headers["content-type"] ?? "")) {
        res.writeHead(up.statusCode, up.headers); up.pipe(res); res.on("close", () => up.destroy()); return;
      }
      const b = []; up.on("data", (d) => b.push(d)); up.on("end", () => {
        let s = Buffer.concat(b).toString();
        if (/json/.test(up.headers["content-type"] ?? "")) for (const [a, z] of rules) s = s.replace(a, z);
        // no CHILL on this device: netinfo has no CHILL exit (as the core reports without it)
        if (/\/api\/netinfo/.test(req.url)) try {
          const j = JSON.parse(s); const n = j.data ?? j;
          if (n && typeof n === "object") { n.proxy = null; n.chill_running = false; s = JSON.stringify(j); }
        } catch { /* not JSON: as it came */ }
        const h = { ...up.headers }; delete h["content-length"]; delete h["transfer-encoding"];
        res.writeHead(up.statusCode, h); res.end(s);
      });
    });
    r.on("error", () => { res.writeHead(502); res.end(); }); r.end(Buffer.concat(c));
  });
}).listen(LISTEN, process.argv[4] ?? "127.0.0.1");
