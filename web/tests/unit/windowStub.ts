import { vi } from "vitest";

/** Minimal browser globals for lib/api in the node environment. */
export function stubWindow(hostname = "10.0.66.1", origin = "http://10.0.66.1:9090") {
  const store = new Map<string, string>();
  const localStorage = {
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    setItem: (k: string, v: string) => void store.set(k, String(v)),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
  };
  const win = Object.assign(new EventTarget(), { localStorage, location: { hostname, origin } });
  vi.stubGlobal("window", win);
  return win;
}

export interface Deferred<T> {
  promise: Promise<T>;
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
}

export function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

export function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

/** A fetch that never answers but rejects when its signal aborts. */
export function hangingFetch() {
  return vi.fn((_url: string, init?: RequestInit) => {
    return new Promise<Response>((_res, rej) => {
      const s = init?.signal;
      if (s?.aborted) return rej(new DOMException("aborted", "AbortError"));
      s?.addEventListener("abort", () => rej(new DOMException("aborted", "AbortError")));
    });
  });
}
