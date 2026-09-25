// /api/public/status as the shell and hub pages read it (no login needed;
// the same SWR key everywhere, so it is fetched once per 10 s).
import { useEffect, useSyncExternalStore } from "react";
import { useApi } from "@/lib/hooks/useApi";

export interface PublicStatus {
  network?: { connected?: boolean; type?: string; rsrp?: number };
  wifi?: { on?: boolean };
  services?: {
    tailscale?: { running?: boolean; installed?: boolean; node?: string | null };
  };
  sms?: { unread?: number };
  clock?: { utc_offset?: number };
  device?: { model?: string; name?: string };
}

export function usePublicStatus() {
  return useApi<PublicStatus>("/api/public/status", { refreshInterval: 10000 });
}

const DEVICE_KEY = "u60.device_label";
export const DEFAULT_DEVICE_LABEL = "U60 Pro · MU5250";

const noSubscribe = () => () => {};
function readCachedLabel(): string | null {
  try {
    return window.localStorage.getItem(DEVICE_KEY);
  } catch {
    return null; // private mode may throw
  }
}

/**
 * "U60 Pro · MU5250" from the device's own identity (public.rs `device`), so
 * one build names another model correctly. Works before login; the last value
 * is kept in localStorage so the fallback does not flash on every load.
 */
export function useDeviceLabel(): string {
  const { data } = useApi<PublicStatus>("/api/public/status", { refreshInterval: 10000, noAuth: true });
  const name = data?.device?.name?.trim() || "";
  const model = data?.device?.model?.trim() || "";
  const fresh = name && model && name !== model ? `${name} · ${model}` : name || model;
  const cached = useSyncExternalStore(noSubscribe, readCachedLabel, () => null);

  useEffect(() => {
    if (!fresh) return;
    try {
      window.localStorage.setItem(DEVICE_KEY, fresh);
    } catch {
      /* private mode may throw */
    }
  }, [fresh]);

  return fresh || cached || DEFAULT_DEVICE_LABEL;
}
