import { ApiError } from "./types";

/**
 * The agent answered 503 because the firmware has no such ubus method
 * ("… (Method not found)"). Retrying can't help: the method won't appear
 * until the firmware (or the agent's method name) changes. Seen on B27 for
 * router_get_upnp_switch, router_get_qos_switch and the STC whitelist reads.
 */
export function isUnsupported(e: unknown): boolean {
  return e instanceof ApiError && e.status === 503 && /method not found/i.test(e.message);
}
