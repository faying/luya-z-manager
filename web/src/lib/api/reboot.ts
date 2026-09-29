// Reboot as a write with a real readback: the device's uptime must have gone
// back down. One failed 3-s probe used to count as "went down", so a busy
// device that answered again a moment later read as "rebooted" while it had
// not restarted yet (9-26 audit A10).
import { apiFetch } from "./client";

async function uptime(): Promise<number | null> {
  try {
    const r = await apiFetch<{ uptime?: number }>("/api/device/system", { timeoutMs: 5000 });
    return typeof r?.uptime === "number" ? r.uptime : null;
  } catch {
    return null;
  }
}

/** Steps + verify for useWriteOp (use with waitDevice.expectDown). verify
 *  runs once the agent answers again and keeps reading uptime for up to 2
 *  minutes: an agent that answers with the old uptime has not restarted yet. */
// Module-level on purpose: useWriteOp reads its config afresh on every render,
// so a per-call closure would give the step and verify different variables.
let before: number | null = null;

export function rebootWrite(label: string) {
  return {
    steps: [
      {
        label,
        run: async () => {
          before = await uptime();
          return apiFetch("/api/device/reboot", { method: "POST" });
        },
      },
    ],
    verify: async () => {
      if (before === null) return true;   // could not read it before: nothing to compare
      const until = Date.now() + 120_000;
      for (;;) {
        const now = await uptime();
        if (now !== null && now < before) return true;
        if (Date.now() > until) return false;
        await new Promise((r) => setTimeout(r, 3000));
      }
    },
  };
}
