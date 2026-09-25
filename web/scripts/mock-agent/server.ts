// Local mock of zte-agent (:9090) for developing the admin web without a
// device. Zero dependencies: Node's built-in http only. Run from web/:
//
//   node scripts/mock-agent/server.ts
//
// then in the browser: localStorage.setItem("u60.agent_url", "http://localhost:9199")
// See README.md for scenarios.
//
// Nothing in src/ imports this, so it never ends up in `npm run build`.

import http from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Ctx, Method, Reply, Route, Scenario } from "./lib.ts";
import { SCENARIOS, stripFields } from "./lib.ts";
import { shared } from "./shared.ts";
import { ALL_ROUTES } from "./routes.ts";

// ---------------------------------------------------------------- config

const envInt = (name: string, def: number): number => {
  const v = Number(process.env[name]);
  return Number.isFinite(v) && v > 0 ? v : def;
};

const PORT = envInt("MOCK_PORT", 9199);
const HOST = "127.0.0.1";
/** How long "stale" endpoints hang. Must exceed the client's planned 9 s timeout (R7). */
const STALE_MS = envInt("MOCK_STALE_MS", 12_000);
/** How long the device is unreachable after a reboot in "reboot-token". */
const REBOOT_MS = envInt("MOCK_REBOOT_MS", 20_000);
/** How long the second step hangs in "step2-timeout". */
const STEP2_MS = envInt("MOCK_STEP2_MS", 20_000);
/** A write arriving within this window after the previous write finished counts as step 2. */
const STEP_WINDOW_MS = envInt("MOCK_STEP_WINDOW_MS", 3_000);
/** Token lifetime (agent: 3600 s, auth.rs TOKEN_TTL_SECS). */
const TOKEN_TTL_MS = envInt("MOCK_TOKEN_TTL", 3600) * 1000;
const QUIET = process.env.MOCK_QUIET === "1";

/** Endpoints that hang in the "stale" scenario. */
const STALE_PATHS = new Set(["/api/network/signal", "/api/network/speed"]);

// ---------------------------------------------------------------- routes


const routeTable = new Map<string, Route>();
for (const r of ALL_ROUTES) {
  const key = `${r.method} ${r.path}`;
  if (routeTable.has(key)) {
    console.warn(`[mock] duplicate route ${key} — keeping the first one`);
    continue;
  }
  routeTable.set(key, r);
}

// ---------------------------------------------------------------- scenarios

function parseScenarios(raw: string | undefined | null): { set: Set<Scenario>; unknown: string[] } {
  const set = new Set<Scenario>();
  const unknown: string[] = [];
  for (const part of (raw ?? "").split(",")) {
    const name = part.trim();
    if (!name) continue;
    if ((SCENARIOS as readonly string[]).includes(name)) {
      if (name !== "normal") set.add(name as Scenario);
    } else {
      unknown.push(name);
    }
  }
  return { set, unknown };
}

const envScen = parseScenarios(process.env.MOCK_SCENARIO);
if (envScen.unknown.length) {
  console.warn(`[mock] ignoring unknown MOCK_SCENARIO entries: ${envScen.unknown.join(", ")}`);
}
let runtimeScenarios: Set<Scenario> = envScen.set;

const scenarioList = (s: ReadonlySet<Scenario>): string => (s.size ? [...s].join(",") : "normal");

// ---------------------------------------------------------------- auth

let tokenCounter = 0;
/** Only the most recent login's token is valid (a new login invalidates older ones). */
let currentToken: { value: string; expires: number } | null = null;

function tokenValid(req: IncomingMessage): boolean {
  const h = req.headers["authorization"];
  if (typeof h !== "string" || !h.startsWith("Bearer ")) return false;
  const tok = h.slice("Bearer ".length);
  return !!currentToken && tok === currentToken.value && Date.now() < currentToken.expires;
}

// ---------------------------------------------------------------- global timeline state

/** reboot-token: sockets are dropped until this time. */
let offlineUntil = 0;
/** step2-timeout bookkeeping. */
let lastWriteDoneAt = 0;
let lastWriteWasStep2 = false;

// ---------------------------------------------------------------- helpers

const ORIGIN_RE = /^http:\/\/(localhost|127\.0\.0\.1)(:\d+)?$/;

function corsHeaders(req: IncomingMessage): Record<string, string> {
  const origin = req.headers.origin;
  const h: Record<string, string> = { Vary: "Origin" };
  if (typeof origin === "string" && ORIGIN_RE.test(origin)) {
    h["Access-Control-Allow-Origin"] = origin;
    h["Access-Control-Allow-Headers"] = "Authorization, Content-Type, X-Mock-Scenario";
    h["Access-Control-Allow-Methods"] = "GET, POST, PUT, DELETE";
    h["Access-Control-Max-Age"] = "600";
  }
  return h;
}

function sendJson(req: IncomingMessage, res: ServerResponse, status: number, body: unknown): void {
  if (res.destroyed || res.writableEnded) return;
  const text = JSON.stringify(body);
  res.writeHead(status, {
    ...corsHeaders(req),
    "Content-Type": "application/json",
    "Content-Length": Buffer.byteLength(text),
  });
  res.end(text);
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    req.on("error", reject);
  });
}

function drop(req: IncomingMessage): void {
  req.socket.destroy();
}

function log(method: string, path: string, status: number | string, t0: number, scen: ReadonlySet<Scenario>): void {
  if (QUIET) return;
  const ms = Date.now() - t0;
  console.log(`${new Date().toISOString().slice(11, 19)} ${method.padEnd(6)} ${path} → ${status} ${ms}ms [${scenarioList(scen)}]`);
}

// ---------------------------------------------------------------- mock control

function handleControl(req: IncomingMessage, res: ServerResponse, url: URL): void {
  if (url.pathname === "/__mock/scenario") {
    const set = url.searchParams.get("set");
    if (set !== null) {
      const parsed = parseScenarios(set);
      if (parsed.unknown.length) {
        sendJson(req, res, 400, { ok: false, error: `unknown scenario: ${parsed.unknown.join(", ")}`, known: SCENARIOS });
        return;
      }
      runtimeScenarios = parsed.set;
      if (!runtimeScenarios.has("reboot-token")) offlineUntil = 0;
      console.log(`[mock] scenario → ${scenarioList(runtimeScenarios)}`);
    }
    sendJson(req, res, 200, { ok: true, data: { scenario: scenarioList(runtimeScenarios), known: SCENARIOS } });
    return;
  }
  if (url.pathname === "/__mock/state") {
    sendJson(req, res, 200, {
      ok: true,
      data: {
        scenario: scenarioList(runtimeScenarios),
        shared,
        offline_for_ms: Math.max(0, offlineUntil - Date.now()),
        token_issued: tokenCounter,
        routes: [...routeTable.keys()].sort(),
      },
    });
    return;
  }
  sendJson(req, res, 404, { ok: false, error: "not found" });
}

// ---------------------------------------------------------------- request handling

async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const t0 = Date.now();
  const url = new URL(req.url ?? "/", `http://${HOST}`);
  const path = url.pathname;
  const method = (req.method ?? "GET").toUpperCase();

  // Mock control stays reachable in every scenario (including "down"), so you
  // can always switch back without restarting.
  if (path.startsWith("/__mock/")) {
    handleControl(req, res, url);
    return;
  }

  const header = req.headers["x-mock-scenario"];
  const scen = typeof header === "string" && header.trim() ? parseScenarios(header).set : runtimeScenarios;

  // "down": the agent is unreachable.
  if (scen.has("down")) {
    log(method, path, "RESET(down)", t0, scen);
    drop(req);
    return;
  }
  // "reboot-token": device is rebooting.
  if (Date.now() < offlineUntil) {
    log(method, path, "RESET(rebooting)", t0, scen);
    drop(req);
    return;
  }

  if (method === "OPTIONS") {
    res.writeHead(204, corsHeaders(req));
    res.end();
    log(method, path, 204, t0, scen);
    return;
  }

  const raw = await readBody(req);
  let body: unknown = undefined;
  if (raw.trim()) {
    try {
      body = JSON.parse(raw);
    } catch {
      body = undefined;
    }
  }

  // Login (handlers.rs:51): 400 on bad JSON / missing field, 401 on wrong password.
  if (method === "POST" && path === "/api/auth/login") {
    if (raw.trim() && body === undefined) {
      sendJson(req, res, 400, { ok: false, error: "invalid JSON" });
      log(method, path, 400, t0, scen);
      return;
    }
    const pw = body && typeof body === "object" ? (body as Record<string, unknown>).password : undefined;
    if (typeof pw !== "string") {
      sendJson(req, res, 400, { ok: false, error: "missing 'password' field" });
      log(method, path, 400, t0, scen);
      return;
    }
    if (pw === "") {
      sendJson(req, res, 401, { ok: false, error: "invalid password" });
      log(method, path, 401, t0, scen);
      return;
    }
    tokenCounter++;
    currentToken = { value: `mock-token-${tokenCounter}`, expires: Date.now() + TOKEN_TTL_MS };
    sendJson(req, res, 200, { ok: true, data: { token: currentToken.value } });
    log(method, path, 200, t0, scen);
    return;
  }

  // Auth (server.rs:97-113): everything except login and public status.
  if (path !== "/api/public/status" && !tokenValid(req)) {
    sendJson(req, res, 401, { ok: false, error: "unauthorized" });
    log(method, path, 401, t0, scen);
    return;
  }

  const route = routeTable.get(`${method} ${path}`);
  if (!route) {
    sendJson(req, res, 404, { ok: false, error: "not found" });
    log(method, path, 404, t0, scen);
    return;
  }
  const kind = route.kind ?? (route.method === "GET" ? "read" : "write");

  if (kind === "write" && scen.has("cmdfail")) {
    sendJson(req, res, 500, { ok: false, error: "mock failure" });
    log(method, path, 500, t0, scen);
    return;
  }

  if (kind === "read" && scen.has("stale") && STALE_PATHS.has(path)) {
    await sleep(STALE_MS);
  }

  let isStep2 = false;
  if (kind === "write" && scen.has("step2-timeout")) {
    isStep2 = !lastWriteWasStep2 && lastWriteDoneAt > 0 && Date.now() - lastWriteDoneAt < STEP_WINDOW_MS;
    if (isStep2) await sleep(STEP2_MS);
  }

  const ctx: Ctx = {
    method: method as Method,
    path,
    query: url.searchParams,
    body,
    scenarios: scen,
    has: (s: Scenario) => scen.has(s),
    now: Date.now(),
  };

  let reply: Reply;
  try {
    reply = await route.handler(ctx);
  } catch (e) {
    console.error(`[mock] handler error ${method} ${path}:`, e);
    reply = { status: 500, error: `mock handler error: ${(e as Error).message}` };
  }
  if (reply.delayMs) await sleep(reply.delayMs);

  if (kind === "write") {
    lastWriteDoneAt = Date.now();
    lastWriteWasStep2 = isStep2;
  }

  let status: number;
  let out: unknown;
  if (reply.raw !== undefined) {
    status = reply.status ?? 200;
    out = reply.raw;
  } else if (reply.error !== undefined) {
    status = reply.status ?? 500;
    out = { ok: false, error: reply.error };
  } else {
    status = reply.status ?? 200;
    let data = reply.data;
    if (kind === "read" && scen.has("missing") && !route.ownMissing && data !== undefined) {
      data = stripFields(data);
    }
    out = data === undefined ? { ok: true } : { ok: true, data };
  }

  // reboot-token: the device goes away right after acknowledging the reboot,
  // and every token issued before it is gone when it comes back (auth.rs keeps
  // tokens in memory only — Decision ledger R1).
  if (method === "POST" && path === "/api/device/reboot" && status < 400 && scen.has("reboot-token")) {
    offlineUntil = Date.now() + REBOOT_MS;
    currentToken = null;
    console.log(`[mock] reboot: dropping connections for ${REBOOT_MS} ms; all tokens invalidated`);
  }

  sendJson(req, res, status, out);
  log(method, path, status, t0, scen);
}

const server = http.createServer((req, res) => {
  handle(req, res).catch((e) => {
    console.error("[mock] unexpected error:", e);
    sendJson(req, res, 500, { ok: false, error: "mock internal error" });
  });
});
server.on("clientError", (_err, socket) => socket.destroy());

server.listen(PORT, HOST, () => {
  console.log(`[mock] zte-agent mock on http://${HOST}:${PORT}  (${routeTable.size} routes, scenario: ${scenarioList(runtimeScenarios)})`);
  console.log(`[mock] browser: localStorage.setItem("u60.agent_url", "http://localhost:${PORT}")`);
});
