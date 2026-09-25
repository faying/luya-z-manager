// Shared e2e fixtures.
//
//   mock      one mock agent (scripts/mock-agent/server.ts) per Playwright
//             worker, on port 9300 + parallelIndex. Per worker because the
//             mock only honours the most recent login's token and keeps
//             writes in memory: a shared mock would let parallel tests log
//             each other out and see each other's writes.
//   freshMock a mock of its own for one test (flow specs that write state).
//   ready     a page with agent URL, token, language and theme already in
//             localStorage, collecting page errors / console errors and every
//             request the app sends to the mock.
import { test as base, expect, type Page, type Request } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { resolve } from "node:path";

const WEB_DIR = resolve(__dirname, "../../..");

export type Mock = {
  port: number;
  url: string;
  /** Log in over HTTP (not through the UI); invalidates any older token. */
  login(): Promise<string>;
  /** Set runtime scenarios, e.g. "weak,missing"; "normal" resets. */
  scenario(set: string): Promise<void>;
};

async function startMock(port: number): Promise<{ mock: Mock; proc: ChildProcess }> {
  const proc = spawn(process.execPath, ["scripts/mock-agent/server.ts"], {
    cwd: WEB_DIR,
    env: { ...process.env, MOCK_PORT: String(port), MOCK_QUIET: "1" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let out = "";
  proc.stdout?.on("data", (d) => (out += String(d)));
  proc.stderr?.on("data", (d) => (out += String(d)));
  const url = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + 15_000;
  for (;;) {
    try {
      const r = await fetch(`${url}/__mock/scenario`);
      if (r.ok) break;
    } catch {
      /* not up yet */
    }
    if (proc.exitCode !== null || Date.now() > deadline) {
      proc.kill();
      throw new Error(`mock agent on :${port} did not start:\n${out}`);
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  const mock: Mock = {
    port,
    url,
    async login() {
      const r = await fetch(`${url}/api/auth/login`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ password: "x" }),
      });
      const j = (await r.json()) as { data: { token: string } };
      return j.data.token;
    },
    async scenario(set: string) {
      const r = await fetch(`${url}/__mock/scenario?set=${encodeURIComponent(set)}`);
      if (!r.ok) throw new Error(`scenario ${set}: ${r.status}`);
    },
  };
  return { mock, proc };
}

function stop(proc: ChildProcess): Promise<void> {
  return new Promise((resolve) => {
    if (proc.exitCode !== null) return resolve();
    proc.once("exit", () => resolve());
    proc.kill();
  });
}

export type Setup = {
  /** Put a valid token in localStorage (default true). */
  authed?: boolean;
  lang?: "zh" | "en";
  /** Stored theme; omitted = none stored (follows the system). */
  theme?: "light" | "dark";
};

export type Ready = {
  page: Page;
  mock: Mock;
  /** Uncaught page errors and console.error messages seen so far. */
  errors: string[];
  /** Every request the page sent to the mock agent (not /__mock). */
  requests: Request[];
  /** Writes (non-GET, non-OPTIONS) to the agent since `since` (index into requests). */
  writes(since?: number): { method: string; path: string; body: string | null }[];
  /** Load `path` of the static export and wait for the shell to settle. */
  goto(path: string): Promise<void>;
};

type Fixtures = {
  setup: Setup;
  freshMock: boolean;
  mock: Mock;
  ready: Ready;
};

type WorkerFixtures = { workerMock: Mock };

export const test = base.extend<Fixtures, WorkerFixtures>({
  setup: [{ authed: true, lang: "zh" }, { option: true }],
  freshMock: [false, { option: true }],

  workerMock: [
    async ({}, provide, workerInfo) => {
      const { mock, proc } = await startMock(9300 + workerInfo.parallelIndex);
      await provide(mock);
      await stop(proc);
    },
    { scope: "worker" },
  ],

  mock: async ({ workerMock, freshMock }, provide, testInfo) => {
    if (!freshMock) {
      await workerMock.scenario("normal");
      await provide(workerMock);
      return;
    }
    // Ports 9400+ for per-test mocks; one test at a time per worker.
    const { mock, proc } = await startMock(9400 + testInfo.parallelIndex);
    await provide(mock);
    await stop(proc);
  },

  ready: async ({ page, mock, setup }, provide) => {
    const token = setup.authed === false ? null : await mock.login();
    // Seed localStorage once per tab (sessionStorage flag), so a reload keeps
    // whatever the app itself stored since (theme choice, cleared token…).
    await page.addInitScript(
      ({ url, token, lang, theme }) => {
        if (sessionStorage.getItem("__e2e_seeded")) return;
        sessionStorage.setItem("__e2e_seeded", "1");
        localStorage.setItem("u60.agent_url", url);
        if (token) localStorage.setItem("u60.token", token);
        localStorage.setItem("u60_lang", lang);
        if (theme) localStorage.setItem("u60.theme", theme);
      },
      { url: mock.url, token, lang: setup.lang ?? "zh", theme: setup.theme ?? null },
    );

    const errors: string[] = [];
    const requests: Request[] = [];
    page.on("pageerror", (e) => errors.push(`pageerror: ${e.message}`));
    page.on("console", (m) => {
      if (m.type() === "error") errors.push(`console.error: ${m.text()}`);
    });
    page.on("request", (r) => {
      const u = new URL(r.url());
      if (u.port === String(mock.port) && !u.pathname.startsWith("/__mock/")) requests.push(r);
    });

    const ready: Ready = {
      page,
      mock,
      errors,
      requests,
      writes(since = 0) {
        return requests
          .slice(since)
          .filter((r) => r.method() !== "GET" && r.method() !== "OPTIONS")
          .map((r) => ({ method: r.method(), path: new URL(r.url()).pathname, body: r.postData() }));
      },
      async goto(path: string) {
        await page.goto(path);
        if (setup.authed === false || path.startsWith("/login")) {
          await expect(page.locator("h1").first()).toBeVisible();
        } else {
          await expect(page.locator("h1").first()).toBeVisible();
        }
        // Let the first round of reads land.
        await page.waitForLoadState("networkidle", { timeout: 3000 }).catch(() => {});
      },
    };
    await provide(ready);
  },
});

export { expect };
