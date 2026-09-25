export type ApiOk<T> = { ok: true; data?: T };
export type ApiErr = { ok: false; error: string };
export type ApiResp<T> = ApiOk<T> | ApiErr;

export class ApiError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
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
 *  check (e.g. Tailscale `error` set, CHILL running with no groups) — R9. */
export class InvalidDataError extends Error {
  reason: string;
  constructor(reason: string) {
    super(reason);
    this.name = "InvalidDataError";
    this.reason = reason;
  }
}
