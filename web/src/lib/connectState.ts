// zwrt_data get_wwaniface `connect_status`. The firmware reports the address
// families in the word: "ipv4_ipv6_connected", "ipv4_connected",
// "ipv6_connected" (B27, 2026-09-25), older ones plain "connected".
export type ConnectKind = "connected" | "connecting" | "disconnected" | "unknown";

export function connectKind(s: string | null | undefined): ConnectKind {
  const v = (s ?? "").toLowerCase();
  if (!v) return "unknown";
  if (v.includes("disconnect")) return "disconnected";
  if (v.includes("connecting")) return "connecting";
  if (v.includes("connected")) return "connected";
  return "disconnected";
}

/** "IPv4 + IPv6" / "IPv4" / "IPv6" / null when the word names no family. */
export function connectFamilies(s: string | null | undefined): string | null {
  const v = (s ?? "").toLowerCase();
  const fam = [v.includes("ipv4") && "IPv4", v.includes("ipv6") && "IPv6"].filter(Boolean);
  return fam.length ? fam.join(" + ") : null;
}

/**
 * Mobile data switch from get_wwaniface. `enable` is the last value written:
 * 0 after boot while the auto dial is connected (E4 T12, B31, 2026-10-04), so
 * 0 counts as off only when the call is down. Same rule as datad's
 * data_switch() and the touch screen. null = cannot tell.
 */
export function dataSwitchOn(enable: unknown, status: string | null | undefined): boolean | null {
  if (enable === true || enable === 1 || enable === "1") return true;
  if (!(enable === false || enable === 0 || enable === "0")) return null;
  const k = connectKind(status);
  if (k === "unknown") return null;
  return k === "connected" || k === "connecting";
}
