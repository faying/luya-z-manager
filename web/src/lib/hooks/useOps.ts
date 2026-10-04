"use client";
// The transaction state for the bar and the item's page (E4 T9, DD4).
// One SWR key, so every component on the page shares one poll: every 2 s
// while a change is running, 5 s otherwise.
import { useEffect, useState } from "react";
import { useApi, type UseApiOptions } from "./useApi";
import {
  OPS_PATH,
  countdownFrozen,
  observeOps,
  opShow,
  remainingNow,
  type OpShow,
  type OpsState,
} from "@/lib/ops";

export interface UseOps {
  data: OpsState | undefined;
  show: OpShow;
  /** Countdown stopped: datad stuck, stale block, or no read for 20 s. */
  frozen: boolean;
  /** ms left on the running one, counted down here between reads. */
  remaining: number;
  /** datad's executor has not moved for 20 s: writes through it are off (DD8). */
  stuck: boolean;
  /** We lost the agent while a change was running: result unknown (DD8). */
  disconnected: boolean;
  supported: boolean;
  refresh: () => void;
}

// Module-level on purpose: the countdown re-renders every second, and new
// function identities would restart SWR's interval timer each time (it never fired).
const OPS_OPTS: UseApiOptions<OpsState> = {
  refreshInterval: (d?: OpsState) => (d?.op?.active ? 2000 : 5000),
  refreshWhenHidden: false,
  // keep trying every 3 s while the agent is gone (not SWR's growing backoff):
  // the result must show soon after it answers again (DD8)
  onErrorRetry: (_e, _k, _c, revalidate, { retryCount }) => {
    setTimeout(() => void revalidate({ retryCount }), 3000);
  },
};

export function useOps(): UseOps {
  const q = useApi<OpsState>(OPS_PATH, OPS_OPTS);
  const data = q.data;
  useEffect(() => {
    observeOps(data?.op);
  }, [data]);

  // tick once a second while something is on show (countdown, 3 s results)
  const [now, setNow] = useState(() => Date.now());
  const active = !!data?.op?.active || data?.op?.last?.stay === "brief";
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [active]);

  const show = opShow(data?.op, now);
  const frozen = countdownFrozen(data, q.lastOkAt, now) || q.stale;
  const remaining = show?.kind === "live" ? remainingNow(show.v, q.lastOkAt, frozen, now) : 0;
  return {
    data,
    show,
    frozen,
    remaining,
    stuck: data?.datad === "stuck" && !q.stale,
    disconnected: q.stale && !!data?.op?.active,
    supported: !!data?.supported,
    refresh: () => void q.mutate(),
  };
}
