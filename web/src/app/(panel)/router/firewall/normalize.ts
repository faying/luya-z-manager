// Firewall response normalizer. Every firewall GET is a ubus passthrough whose
// real shape is unconfirmed (schemas/router.ts): the old web page read one set
// of key names, the iOS app reads another, and the rule lists may be a bare
// array or wrapped in {rule_list: [...]}. The page renders AND reads back
// through these functions, so what is shown and what counts as "applied"
// always agree.
//
// Keys read (page-style first, then the iOS / write-style name):
//   enabled        firewall_enable   | firewall_switch
//   level          level             | firewall_level
//   nat            nat_enable        | nat_switch
//   dmz            dmz_enable        | dmz_enabled
//   dmzIp          dmz_ip            | dmz_hostname
//   portForward    portforward_enable| port_forward_switch
//   wanPing        wan_ping_enable   (page)
//   wanPingFilter  wan_ping_filter   (iOS; the opposite sense, so kept apart)
//   remoteWeb      remote_web_access_enable
//   upnp           upnp_switch
// Rule lists: bare array | {rule_list: [...]} (iOS) | an object with exactly
// one array-valued key (whatever the firmware calls it) | {} — B27 answers
// `{}` when there are no rules (recorded 2026-09-25; the non-empty shape is
// still unconfirmed). Anything else is "shape not recognised" (not "no rules").
// Filter rules: dst_ip | dest_ip, dst_port | dest_port, plus src_mac,
// src_port, enabled when present.

import type { RouterFirewall, RouterUpnp } from "@/lib/api/schemas/router";

type Obj = Record<string, unknown>;

const isObj = (v: unknown): v is Obj => !!v && typeof v === "object" && !Array.isArray(v);

/** "1"/"0", true/false, 1/0, "on"/"off" → boolean; missing or other → undefined (unknown). */
export function tri(v: unknown): boolean | undefined {
  if (v === true || v === 1) return true;
  if (v === false || v === 0) return false;
  if (typeof v === "string") {
    const s = v.trim().toLowerCase();
    if (s === "1" || s === "true" || s === "on" || s === "yes") return true;
    if (s === "0" || s === "false" || s === "off" || s === "no") return false;
  }
  return undefined;
}

function pick(o: Obj | undefined, ...keys: string[]): unknown {
  if (!o) return undefined;
  for (const k of keys) if (o[k] !== undefined && o[k] !== null) return o[k];
  return undefined;
}

function str(v: unknown): string | undefined {
  if (typeof v === "string") return v;
  if (typeof v === "number") return String(v);
  return undefined;
}

export type Level = "low" | "medium" | "high";

export interface FirewallState {
  enabled?: boolean;
  level?: Level;
  /** Raw level value when it isn't one of low/medium/high. */
  levelRaw?: string;
  nat?: boolean;
  dmz?: boolean;
  dmzIp?: string;
  portForward?: boolean;
  wanPing?: boolean;
  wanPingFilter?: boolean;
  remoteWeb?: boolean;
}

export function normalizeFirewall(d: RouterFirewall | undefined): FirewallState {
  const o = isObj(d) ? d : undefined;
  const lv = str(pick(o, "level", "firewall_level"))?.trim().toLowerCase();
  const level = lv === "low" || lv === "medium" || lv === "high" ? lv : undefined;
  return {
    enabled: tri(pick(o, "firewall_enable", "firewall_switch")),
    level,
    levelRaw: level ? undefined : lv || undefined,
    nat: tri(pick(o, "nat_enable", "nat_switch")),
    dmz: tri(pick(o, "dmz_enable", "dmz_enabled")),
    dmzIp: str(pick(o, "dmz_ip", "dmz_hostname")),
    portForward: tri(pick(o, "portforward_enable", "port_forward_switch")),
    wanPing: tri(pick(o, "wan_ping_enable")),
    wanPingFilter: tri(pick(o, "wan_ping_filter")),
    remoteWeb: tri(pick(o, "remote_web_access_enable")),
  };
}

/** B27 `router_get_upnp` answers enable_upnp; older builds were assumed to say upnp_switch. */
export function normalizeUpnp(d: RouterUpnp | undefined): boolean | undefined {
  const o = isObj(d) ? d : undefined;
  return tri(pick(o, "enable_upnp")) ?? tri(pick(o, "upnp_switch"));
}

/** Body for PUT /api/router/firewall/upnp → router_set_upnp_switch, which takes
 *  integers {enable_upnp, notify_interval, ttl, natpmp}. Keep the device's
 *  other values; leave out anything it didn't report (ttl is "" on B27). */
export function upnpBody(d: RouterUpnp | undefined, on: boolean): Record<string, number> {
  const o = isObj(d) ? d : {};
  const num = (v: unknown) => (v === "" || v == null || !Number.isFinite(Number(v)) ? undefined : Number(v));
  const body: Record<string, number> = { enable_upnp: on ? 1 : 0 };
  const interval = num(o.notify_interval);
  const ttl = num(o.ttl);
  const natpmp = num(o.enable_natpmp);
  if (interval !== undefined) body.notify_interval = interval;
  if (ttl !== undefined) body.ttl = ttl;
  if (natpmp !== undefined) body.natpmp = natpmp;
  return body;
}

/** Array of rule objects, or null when the shape isn't recognised. */
export function ruleList(d: unknown): Obj[] | null {
  if (Array.isArray(d)) return d.filter(isObj);
  if (isObj(d)) {
    const v = d.rule_list;
    if (Array.isArray(v)) return v.filter(isObj);
    const keys = Object.keys(d);
    if (keys.length === 0) return [];
    const arrays = keys.filter((k) => Array.isArray(d[k]));
    if (arrays.length === 1) return (d[arrays[0]] as unknown[]).filter(isObj);
  }
  return null;
}

export interface PfRule {
  /** Only a real id from the device; rules without one can't be deleted. */
  id?: string;
  name?: string;
  protocol?: string;
  wanPort?: string;
  lanIp?: string;
  lanPort?: string;
  enabled?: boolean;
}

export function normalizePortForward(d: unknown): PfRule[] | null {
  const l = ruleList(d);
  if (!l) return null;
  return l.map((r) => ({
    id: str(r.id),
    name: str(r.name),
    protocol: str(r.protocol),
    wanPort: str(r.wan_port),
    lanIp: str(r.lan_ip),
    lanPort: str(r.lan_port),
    enabled: tri(r.enabled),
  }));
}

export interface FilterRule {
  id?: string;
  name?: string;
  protocol?: string;
  srcMac?: string;
  srcIp?: string;
  srcPort?: string;
  dstIp?: string;
  dstPort?: string;
  action?: string;
  enabled?: boolean;
}

export function normalizeFilterRules(d: unknown): FilterRule[] | null {
  const l = ruleList(d);
  if (!l) return null;
  return l.map((r) => ({
    id: str(r.id),
    name: str(r.name),
    protocol: str(r.protocol),
    srcMac: str(r.src_mac),
    srcIp: str(r.src_ip),
    srcPort: str(r.src_port),
    dstIp: str(pick(r, "dst_ip", "dest_ip")),
    dstPort: str(pick(r, "dst_port", "dest_port")),
    action: str(r.action),
    enabled: tri(r.enabled),
  }));
}
