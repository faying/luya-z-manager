export type ApiOk<T> = { ok: true; data?: T };
export type ApiErr = { ok: false; error: string };
export type ApiResp<T> = ApiOk<T> | ApiErr;

export class ApiError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

export class UnauthorizedError extends ApiError {
  constructor(message = "unauthorized") {
    super(message, 401);
  }
}
