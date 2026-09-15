"use client";

import useSWR, { SWRConfiguration, SWRResponse } from "swr";
import { apiFetch, FetchOptions } from "@/lib/api/client";

export function useApi<T = unknown>(
  path: string | null,
  opts: FetchOptions & SWRConfiguration = {}
): SWRResponse<T> {
  const { method, body, raw, noAuth, signal, ...swrOpts } = opts;
  return useSWR<T>(
    path,
    (p) => apiFetch<T>(p, { method, body, raw, noAuth, signal }),
    swrOpts
  );
}
