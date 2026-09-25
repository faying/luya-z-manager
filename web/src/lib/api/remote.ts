// Remote (Tailscale) access detection — spec 3.1, C2.
//
// Remote when the host the API is reached through is any of:
//   - IPv4 in 100.64.0.0/10 (Tailscale CGNAT range)
//   - IPv6 in fd7a:115c:a1e0::/48 (Tailscale ULA range)
//   - a name ending in .ts.net (MagicDNS full name)
//   - a single-label name (MagicDNS short name), except "localhost"
// Known blind spot: through a Tailscale subnet route the browser sees the
// device's LAN address and this can't tell (TODO-1: agent `via_tailscale`).

import { getApiBase } from "./client";

function parseIPv4(h: string): number[] | null {
  const parts = h.split(".");
  if (parts.length !== 4) return null;
  const out: number[] = [];
  for (const p of parts) {
    if (!/^\d{1,3}$/.test(p)) return null;
    const n = Number(p);
    if (n > 255) return null;
    out.push(n);
  }
  return out;
}

/** Expand an IPv6 literal into 8 hextets, or null if it isn't one.
 *  Accepts a trailing embedded IPv4 and a zone id (dropped). */
function parseIPv6(h: string): number[] | null {
  let s = h.split("%")[0];
  if (!s.includes(":")) return null;
  // Embedded IPv4 tail (::ffff:1.2.3.4) → two hextets.
  const lastColon = s.lastIndexOf(":");
  const tail = s.slice(lastColon + 1);
  if (tail.includes(".")) {
    const v4 = parseIPv4(tail);
    if (!v4) return null;
    s = `${s.slice(0, lastColon + 1)}${((v4[0] << 8) | v4[1]).toString(16)}:${((v4[2] << 8) | v4[3]).toString(16)}`;
  }
  const halves = s.split("::");
  if (halves.length > 2) return null;
  const toGroups = (part: string) => (part === "" ? [] : part.split(":"));
  const head = toGroups(halves[0]);
  const rest = halves.length === 2 ? toGroups(halves[1]) : [];
  let groups: string[];
  if (halves.length === 2) {
    const fill = 8 - head.length - rest.length;
    if (fill < 1) return null;
    groups = [...head, ...Array<string>(fill).fill("0"), ...rest];
  } else {
    groups = head;
  }
  if (groups.length !== 8) return null;
  const out: number[] = [];
  for (const g of groups) {
    if (!/^[0-9a-f]{1,4}$/i.test(g)) return null;
    out.push(parseInt(g, 16));
  }
  return out;
}

export function isRemoteHost(hostname: string): boolean {
  let h = hostname.trim().toLowerCase();
  if (h.startsWith("[") && h.endsWith("]")) h = h.slice(1, -1);
  if (h.endsWith(".")) h = h.slice(0, -1);
  if (!h) return false;

  // IPv6 first: literals have no dots and would otherwise look single-label.
  if (h.includes(":")) {
    const v6 = parseIPv6(h);
    return !!v6 && v6[0] === 0xfd7a && v6[1] === 0x115c && v6[2] === 0xa1e0;
  }
  const v4 = parseIPv4(h);
  if (v4) return v4[0] === 100 && v4[1] >= 64 && v4[1] <= 127;
  if (/^[\d.]+$/.test(h)) return false; // malformed numeric, not a name
  if (h.endsWith(".ts.net")) return true;
  if (!h.includes(".")) return h !== "localhost";
  return false;
}

/** Whether this page talks to the agent over Tailscale. Uses the API host
 *  (getApiBase), not location.hostname — in dev they differ (C2). */
export function isRemoteAccess(): boolean {
  try {
    return isRemoteHost(new URL(getApiBase()).hostname);
  } catch {
    return false;
  }
}
