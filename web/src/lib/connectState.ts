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
