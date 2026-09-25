"use client";

import { useEffect, useLayoutEffect, useRef, useSyncExternalStore } from "react";
import useSWR, { SWRConfiguration, SWRResponse } from "swr";
import { apiFetch, FetchOptions, LOGIN_EVENT } from "@/lib/api/client";
import { UnauthorizedError } from "@/lib/api/types";
import { isUnsupported } from "@/lib/api/unsupported";
import {
  FreshState,
  INITIAL_FRESH,
  Validator,
  freshNow,
  getFresh,
  isStale,
  subscribeFresh,
  validatingFetcher,
} from "@/lib/api/freshness";

// SWR already pauses polling while the tab is hidden (refreshWhenHidden
// defaults to false) and revalidates on focus (C1). Long jobs that must keep
// polling in the background pass `refreshWhenHidden: true`.

export type UseApiOptions<T> = FetchOptions &
  SWRConfiguration<T> & {
    /** R9: payload check. false / {ok:false,reason} counts as a failure and
     *  does not replace the last valid data. */
    isValid?: Validator<T>;
  };

export type UseApiResponse<T> = SWRResponse<T> & {
  /** §5 freshness rule for this endpoint. */
  stale: boolean;
  /** Browser ms of the last valid response this session. */
  lastOkAt: number | null;
  /** Why the latest response was rejected by `isValid`, if it was. */
  invalidReason?: string;
  /** The latest error is "firmware has no such method" (503 Method not found).
   *  Not retried; SWR also stops interval polling while the error stands. */
  unsupported: boolean;
};

const noopSubscribe = () => () => {};

/** A stale 401 (R11) means a newer login already happened, so the login event
 *  has fired before this reply arrived: re-send once right away with the new
 *  token instead of waiting for SWR's error retry. */
async function fetchOnce<T>(p: string, o: FetchOptions): Promise<T> {
  try {
    return await apiFetch<T>(p, o);
  } catch (e) {
    if (e instanceof UnauthorizedError && e.staleToken) return apiFetch<T>(p, o);
    throw e;
  }
}

export function useApi<T = unknown>(path: string | null, opts: UseApiOptions<T> = {}): UseApiResponse<T> {
  const { method, body, raw, noAuth, signal, timeoutMs, isValid, ...swrOpts } = opts;
  const swr = useSWR<T>(
    path,
    (p: string) =>
      validatingFetcher<T>(p, () => fetchOnce<T>(p, { method, body, raw, noAuth, signal, timeoutMs }), isValid)(),
    // "Method not found" never heals by retrying: no error-retry backoff for it.
    { shouldRetryOnError: (e: unknown) => !isUnsupported(e), ...swrOpts }
  );

  const ri = swrOpts.refreshInterval;
  const interval = typeof ri === "function" ? ri(swr.data) : ri ?? 0;

  const fresh: FreshState = useSyncExternalStore(
    path === null ? noopSubscribe : subscribeFresh,
    () => getFresh(path),
    () => INITIAL_FRESH
  );
  const stale = useSyncExternalStore(
    path === null ? noopSubscribe : subscribeFresh,
    () => isStale(getFresh(path), freshNow(), interval),
    () => false
  );

  // R2: reads that failed with 401 are re-sent after the next login.
  const errRef = useRef<unknown>(undefined);
  const mutateRef = useRef(swr.mutate);
  useLayoutEffect(() => {
    errRef.current = swr.error;
    mutateRef.current = swr.mutate;
  });
  useEffect(() => {
    if (path === null) return;
    const onLogin = () => {
      if (errRef.current instanceof UnauthorizedError) void mutateRef.current();
    };
    window.addEventListener(LOGIN_EVENT, onLogin);
    return () => window.removeEventListener(LOGIN_EVENT, onLogin);
  }, [path]);

  // Delegate through getters: SWR tracks which fields a component reads and
  // only re-renders for those; spreading would read (and subscribe to) all.
  const out = {} as UseApiResponse<T>;
  Object.defineProperties(out, {
    data: { get: () => swr.data, enumerable: true },
    error: { get: () => swr.error, enumerable: true },
    isLoading: { get: () => swr.isLoading, enumerable: true },
    isValidating: { get: () => swr.isValidating, enumerable: true },
    mutate: { value: swr.mutate, enumerable: true },
    stale: { value: stale, enumerable: true },
    lastOkAt: { value: fresh.lastOkAt, enumerable: true },
    invalidReason: { value: fresh.invalidReason, enumerable: true },
    unsupported: { get: () => isUnsupported(swr.error), enumerable: true },
  });
  return out;
}
