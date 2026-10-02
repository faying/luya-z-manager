export type ApiOk<T> = { ok: true; data?: T };
export type ApiErr = {
  ok: false;
  error: string;
  error_en?: string | null;
  /** Set when another job holds the device (deep_diag.rs `refusal`: "diagnose"). */
  busy?: string | null;
  /** About how long until it is free (with `busy`). */
  retry_after_s?: number | null;
};
export type ApiResp<T> = ApiOk<T> | ApiErr;

export class ApiError extends Error {
  status: number;
  /** The agent's `error_en`, when it gave one. `message` stays the agent's
   *  own `error`, so code that tests it keeps working in either language;
   *  pages show `errorText(e, lang)` instead. */
  messageEn?: string;
  /** What holds the device when the agent refused because it is busy
   *  (409 `busy`, e.g. "diagnose": 「正在诊断，约 N 秒后再试」). The message
   *  already says it all, so pages show it as it is. */
  busy?: string;
  retryAfterS?: number;
  constructor(message: string, status: number, messageEn?: string | null, busy?: string | null, retryAfterS?: number | null) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    if (messageEn) this.messageEn = messageEn;
    if (busy) this.busy = busy;
    if (typeof retryAfterS === "number") this.retryAfterS = retryAfterS;
  }
}

/** What to show for a failure: the English the agent gave in English mode,
 *  else the message. */
export function errorText(e: unknown, lang: "zh" | "en"): string {
  if (lang === "en" && e instanceof ApiError && e.messageEn) return e.messageEn;
  return e instanceof Error ? e.message : String(e);
}

export class UnauthorizedError extends ApiError {
  /** True when the token this request was sent with is no longer the current
   *  token (a newer login happened while it was in flight). A stale 401 does
   *  not clear the session and does not fire UNAUTHORIZED_EVENT (R11). */
  staleToken: boolean;
  constructor(message = "unauthorized", staleToken = false) {
    super(message, 401);
    this.name = "UnauthorizedError";
    this.staleToken = staleToken;
  }
}

/** The request got no answer within its time limit (R7). Status 0, like a
 *  network error: the device may or may not have acted on it. */
export class TimeoutError extends ApiError {
  timeoutMs: number;
  constructor(timeoutMs: number, message = `timeout after ${timeoutMs} ms`) {
    super(message, 0);
    this.name = "TimeoutError";
    this.timeoutMs = timeoutMs;
  }
}

/** HTTP and envelope said ok, but the payload fails the endpoint's `isValid`
 *  check (e.g. Tailscale `error` set, a service running with no data) — R9. */
export class InvalidDataError extends Error {
  reason: string;
  constructor(reason: string) {
    super(reason);
    this.name = "InvalidDataError";
    this.reason = reason;
  }
}
