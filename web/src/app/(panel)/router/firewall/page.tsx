"use client";
// Firewall (new design). Settings page: status first (firewall on/off, level,
// DMZ exposure), then the switches, DMZ, port forwarding (list + add form) and
// the read-only filter rules.
//
// Writes (controls-inventory §/router/firewall, design §3.1): every one is
// "二·远程三" — inline confirm (tier 2) on the LAN, a dialog (tier 3, remote
// banner, recovery names the LAN address) when reached over Tailscale; the
// tier is decided at the moment of the press. One write op serves them all
// (they never overlap). Each is read back through normalize.ts, the same code
// the page renders with:
//   firewall / level / NAT / port-forward switch / DMZ → GET /api/router/firewall
//   UPnP → GET /api/router/firewall/upnp
//   add rule → a row with the same name, WAN port, LAN IP, LAN port appears
//   delete rule → the rule id is gone
// Request bodies are unchanged from the old page.
//
// Fixed vs the old page: key names (page vs iOS/write-style) are both read;
// the rule lists accept a bare array or {rule_list}; unknown values show "—"
// and lock the control instead of showing "off" / defaulting the level to
// "medium"; a list in an unrecognised shape says so instead of "no rules".
// B27: the lists come back as `{}` when empty (= no rules), and UPnP answers
// 503 "Method not found" — shown as "not available on this firmware", no
// switch, no retry.
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Plus, Trash, X } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp } from "@/lib/api/writeOp";
import type { RouterFirewall, RouterLan, RouterUpnp } from "@/lib/api/schemas/router";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  Segmented,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";
import {
  normalizeFilterRules,
  normalizeFirewall,
  normalizePortForward,
  normalizeUpnp,
  type FilterRule,
  type Level,
  type PfRule, upnpBody } from "./normalize";

type Section = "switches" | "dmz" | "pfAdd" | "pfList";

interface Pending {
  section: Section;
  tier: 2 | 3;
  /** Short verb phrase: the confirm button and the dialog action. */
  action: string;
  title: string;
  consequence: ReactNode;
  label: string;
  method: "PUT" | "POST";
  path: string;
  body: Record<string, string | number>;
  check: () => Promise<boolean>;
  onApplied?: () => void;
}

function isValidIPv4(s: string): boolean {
  const parts = s.split(".");
  if (parts.length !== 4) return false;
  return parts.every((p) => /^\d+$/.test(p) && Number(p) >= 0 && Number(p) <= 255);
}
const isPort = (s: string) => /^\d{1,5}(-\d{1,5})?$/.test(s) && s.split("-").every((p) => Number(p) >= 1 && Number(p) <= 65535);

const EMPTY_PF = { name: "", protocol: "TCP", wan: "", lanIp: "", lanPort: "", enabled: true };

type T = (k: string, d: string, o?: Record<string, unknown>) => string;

function levelLabel(t: T, l: Level) {
  return l === "low" ? t("firewall.levelLow", "low") : l === "medium" ? t("firewall.levelMedium", "medium") : t("firewall.levelHigh", "high");
}

export default function FirewallPage() {
  const { t } = useTranslation();
  const fwApi = useApi<RouterFirewall>("/api/router/firewall", { refreshInterval: 30000 });
  const upnpApi = useApi<RouterUpnp>("/api/router/firewall/upnp", { refreshInterval: 30000 });
  const pfApi = useApi<unknown>("/api/router/firewall/port-forward", { refreshInterval: 30000 });
  const filterApi = useApi<unknown>("/api/router/firewall/filter-rules", { refreshInterval: 60000 });

  const fw = normalizeFirewall(fwApi.data);
  const upnp = normalizeUpnp(upnpApi.data);
  const pfRules = pfApi.data === undefined ? undefined : normalizePortForward(pfApi.data);
  const filterRules = filterApi.data === undefined ? undefined : normalizeFilterRules(filterApi.data);

  // Remote is checked when a change is asked for; the LAN address is only
  // fetched then, for the dialog's recovery text.
  const [remote, setRemote] = useState(false);
  const lanApi = useApi<RouterLan>(remote ? "/api/router/lan" : null);
  const lanAddr = lanApi.data?.lan_ipaddr || null;

  // ── one write op for every change ──
  const [pending, setPending] = useState<Pending | null>(null);
  const [shownAt, setShownAt] = useState<Section | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);
  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: pending
      ? [{ label: pending.label, run: () => apiFetch(pending.path, { method: pending.method, body: pending.body }) }]
      : [],
    verify: pending
      ? async () => {
          const ok = await pending.check();
          if (ok) pending.onApplied?.();
          return ok;
        }
      : undefined,
  });
  const busy = op.busy;

  const readFw = async () => {
    const d = await apiFetch<RouterFirewall>("/api/router/firewall");
    await fwApi.mutate(d, { revalidate: false });
    return normalizeFirewall(d);
  };
  const readPf = async () => {
    const d = await apiFetch<unknown>("/api/router/firewall/port-forward");
    await pfApi.mutate(d, { revalidate: false });
    return normalizePortForward(d);
  };

  function ask(p: Omit<Pending, "tier">) {
    if (busy) return;
    const r = isRemoteAccess();
    setRemote(r);
    setPending({ ...p, tier: r ? 3 : 2 });
  }
  function go() {
    if (!pending) return;
    setShownAt(pending.section);
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }
  const trig = (section: Section) => (pending?.section === section && pending.tier === 2 ? inline.triggerProps : {});

  const fwLocked = !fwApi.data || fwApi.stale || busy;
  // Adding needs a readable, fresh list: the only proof an add worked is the new row.
  const pfListable = !!pfRules && !pfApi.stale;

  // ── switches ──
  function askSwitch(kind: "firewall" | "nat" | "pf", next: boolean) {
    const name =
      kind === "firewall" ? t("firewall.firewall", "Firewall") : kind === "nat" ? "NAT" : t("firewall.portForwarding", "Port Forwarding");
    const consequence = {
      firewall: next
        ? t("firewall.fwOnConsequence", "The firewall starts filtering traffic from the internet again at the selected level.")
        : t("firewall.fwOffConsequence", "Traffic from the internet is no longer filtered. Devices on the LAN are more exposed until you turn it back on."),
      nat: next
        ? t("firewall.natOnConsequence", "LAN devices share the mobile connection's address again.")
        : t("firewall.natOffConsequence", "LAN devices lose internet access through address translation; most setups stop working until NAT is back on."),
      pf: next
        ? t("firewall.pfOnConsequence", "The port forwarding rules below start forwarding traffic from the internet to LAN devices.")
        : t("firewall.pfOffConsequence", "All port forwarding rules stop; services behind them become unreachable from outside."),
    }[kind];
    const [path, key] =
      kind === "firewall"
        ? ["/api/router/firewall/switch", "firewall_switch"]
        : kind === "nat"
          ? ["/api/router/firewall/nat", "nat_switch"]
          : ["/api/router/firewall/port-forward/switch", "port_forward_switch"];
    const action = next ? t("firewall.turnOnX", "turn {{x}} on", { x: name }) : t("firewall.turnOffX", "turn {{x}} off", { x: name });
    ask({
      section: "switches",
      action,
      title: next ? t("firewall.turnOnXTitle", "Turn {{x}} on?", { x: name }) : t("firewall.turnOffXTitle", "Turn {{x}} off?", { x: name }),
      consequence,
      label: name,
      method: "PUT",
      path,
      body: { [key]: next ? "1" : "0" },
      check: async () => {
        const f = await readFw();
        return (kind === "firewall" ? f.enabled : kind === "nat" ? f.nat : f.portForward) === next;
      },
    });
  }
  function askLevel(level: Level) {
    ask({
      section: "switches",
      action: t("firewall.setLevelAction", "set the level to {{level}}", { level: levelLabel(t, level) }),
      title: t("firewall.setLevelTitle", "Set the firewall level to {{level}}?", { level: levelLabel(t, level) }),
      consequence: t("firewall.levelConsequence", "The firewall applies the {{level}} rule set; some connections from outside may start or stop working.", {
        level: levelLabel(t, level),
      }),
      label: t("firewall.firewallLevel", "Firewall Level"),
      method: "PUT",
      path: "/api/router/firewall/level",
      body: { firewall_level: level },
      check: async () => (await readFw()).level === level,
    });
  }
  function askUpnp(next: boolean) {
    ask({
      section: "switches",
      action: next ? t("firewall.turnOnX", "turn {{x}} on", { x: "UPnP" }) : t("firewall.turnOffX", "turn {{x}} off", { x: "UPnP" }),
      title: next ? t("firewall.turnOnXTitle", "Turn {{x}} on?", { x: "UPnP" }) : t("firewall.turnOffXTitle", "Turn {{x}} off?", { x: "UPnP" }),
      consequence: next
        ? t("firewall.upnpOnConsequence", "Apps on the LAN can open ports to the internet by themselves.")
        : t("firewall.upnpOffConsequence", "Apps can no longer open ports by themselves; games and calls may fall back to relays."),
      label: "UPnP",
      method: "PUT",
      path: "/api/router/firewall/upnp",
      body: upnpBody(upnpApi.data, next),
      check: async () => {
        const d = await apiFetch<RouterUpnp>("/api/router/firewall/upnp");
        await upnpApi.mutate(d, { revalidate: false });
        return normalizeUpnp(d) === next;
      },
    });
  }

  // ── DMZ (draft until Apply) ──
  const [dmzDraft, setDmzDraft] = useState<{ on: boolean; ip: string } | null>(null);
  const dmz = dmzDraft ?? (fw.dmz !== undefined ? { on: fw.dmz, ip: fw.dmzIp ?? "" } : null);
  const [dmzErr, setDmzErr] = useState<string | null>(null);
  function askDmz() {
    if (!dmz) return;
    const ip = dmz.ip.trim();
    if (dmz.on && !isValidIPv4(ip)) {
      setDmzErr(t("firewall.msgInvalidDmzIp", "Invalid DMZ host IP"));
      return;
    }
    setDmzErr(null);
    const on = dmz.on;
    ask({
      section: "dmz",
      action: on ? t("firewall.enableDmz", "Enable DMZ") : t("firewall.disableDmz", "Turn DMZ off"),
      title: on ? t("firewall.enableDmzTitle", "Expose {{ip}} through DMZ?", { ip }) : t("firewall.disableDmzTitle", "Turn DMZ off?"),
      consequence: on
        ? t("firewall.dmzOnConsequence", "Every connection from the internet that no other rule handles goes to {{ip}}, bypassing the firewall. Only do this for a device that protects itself.", { ip })
        : t("firewall.dmzOffConsequence", "The DMZ host stops receiving unsolicited traffic from the internet."),
      label: "DMZ",
      method: "PUT",
      path: "/api/router/firewall/dmz",
      body: { dmz_enabled: on ? "1" : "0", dmz_ip: ip },
      check: async () => {
        const f = await readFw();
        return f.dmz === on && (!on || (f.dmzIp ?? "") === ip);
      },
      onApplied: () => setDmzDraft(null),
    });
  }

  // ── port forwarding: add ──
  const [showAdd, setShowAdd] = useState(false);
  const [pf, setPf] = useState(EMPTY_PF);
  const [pfErr, setPfErr] = useState<string | null>(null);
  function askAdd() {
    const f = { ...pf, name: pf.name.trim(), wan: pf.wan.trim(), lanIp: pf.lanIp.trim(), lanPort: pf.lanPort.trim() };
    if (!f.name || !f.wan || !f.lanIp || !f.lanPort) return setPfErr(t("firewall.msgAllFieldsRequired", "All port forward fields required"));
    if (!isValidIPv4(f.lanIp)) return setPfErr(t("firewall.msgInvalidLanIp", "Invalid LAN IP"));
    if (!isPort(f.wan) || !isPort(f.lanPort)) return setPfErr(t("firewall.msgInvalidPort", "Ports are numbers from 1 to 65535 (or a range like 8000-8010)"));
    setPfErr(null);
    const same = (r: PfRule) => r.name === f.name && r.wanPort === f.wan && r.lanIp === f.lanIp && r.lanPort === f.lanPort;
    // A rule identical to an existing one must still show up as a NEW row.
    const before = pfRules?.filter(same).length ?? 0;
    ask({
      section: "pfAdd",
      action: t("firewall.addRuleAction", "add rule {{name}}", { name: f.name }),
      title: t("firewall.addRuleTitle", "Add port forwarding rule “{{name}}”?", { name: f.name }),
      consequence: t("firewall.addConsequence", "Traffic from the internet to {{proto}} port {{wan}} goes to {{ip}}:{{port}}.", {
        proto: f.protocol,
        wan: f.wan,
        ip: f.lanIp,
        port: f.lanPort,
      }),
      label: t("firewall.saveRule", "Save Rule"),
      method: "POST",
      path: "/api/router/firewall/port-forward",
      body: { action: "add", name: f.name, protocol: f.protocol, wan_port: f.wan, lan_ip: f.lanIp, lan_port: f.lanPort, enabled: f.enabled ? "1" : "0" },
      check: async () => {
        const l = await readPf();
        return !!l && l.filter(same).length > before;
      },
      onApplied: () => {
        setPf(EMPTY_PF);
        setShowAdd(false);
      },
    });
  }

  // ── port forwarding: delete ──
  function askDelete(r: PfRule) {
    const id = r.id;
    if (!id) return;
    const name = r.name || id;
    ask({
      section: "pfList",
      action: t("firewall.deleteRuleNamed", "Delete rule {{name}}", { name }),
      title: t("firewall.deleteRuleTitle", "Delete rule “{{name}}”?", { name }),
      consequence: t("firewall.deleteConsequence", "Traffic to {{wan}} stops reaching {{ip}}. To undo, add the rule again.", {
        wan: r.wanPort ?? "—",
        ip: r.lanIp ?? "—",
      }),
      label: t("firewall.delete", "Delete"),
      method: "POST",
      path: "/api/router/firewall/port-forward",
      body: { action: "delete", id },
      check: async () => {
        const l = await readPf();
        return !!l && !l.some((x) => x.id === id && x.name === r.name && x.wanPort === r.wanPort && x.lanIp === r.lanIp && x.lanPort === r.lanPort);
      },
    });
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("firewall.loading", "Reading firewall settings…");
  let reason: ReactNode = null;
  if (!fwApi.data && fwApi.error) {
    tone = "bad";
    state = t("firewall.unreadable", "Can't read firewall settings");
    reason = fwApi.error.message;
  } else if (fwApi.data && fw.enabled === undefined) {
    state = t("firewall.stUnknown", "Firewall state unknown");
    reason = t("firewall.stUnknownDesc", "The device's reply has no on/off value this page recognises.");
  } else if (fw.enabled === false) {
    tone = "warn";
    state = t("firewall.stOff", "Firewall off");
    reason = t("firewall.stOffDesc", "Traffic from the internet is not filtered.");
  } else if (fw.enabled === true && fw.dmz === true) {
    tone = "warn";
    state = t("firewall.stOnDmz", "Firewall on · DMZ exposes a device");
    reason = t("firewall.stDmzDesc", "{{ip}} receives all unsolicited traffic from the internet.", { ip: fw.dmzIp || "—" });
  } else if (fw.enabled === true) {
    tone = "ok";
    state = t("firewall.stOn", "Firewall on");
    reason = fw.level ? t("firewall.current", "Current: {{level}}", { level: levelLabel(t, fw.level) }) : null;
  }
  if (fwApi.data && fwApi.stale) tone = "stale";

  const recovery = lanAddr
    ? t("firewall.recoveryLan", "Join the U60's Wi-Fi or plug into its LAN, open http://{{ip}}:9090 and undo the change here.", { ip: lanAddr })
    : t("firewall.recoveryLanGeneric", "Join the U60's Wi-Fi or plug into its LAN, open this page at the device's LAN address (port 9090) and undo the change here.");

  function confirmHere(section: Section) {
    return (
      <>
        {pending?.section === section && pending.tier === 2 && (
          <ConfirmInline
            id={inline.id}
            open
            actionLabel={pending.action}
            consequence={pending.consequence}
            onCancel={() => setPending(null)}
            onConfirm={go}
          />
        )}
        {shownAt === section && (
          <div className="mt-2 px-1">
            <OpResult op={op} />
          </div>
        )}
      </>
    );
  }

  const unknownDash = <span className="nd-aux">—</span>;
  const switchCtl = (value: boolean | undefined, label: string, onChange: (v: boolean) => void, locked: boolean, section: Section = "switches") =>
    value === undefined ? (
      unknownDash
    ) : (
      <span {...trig(section)}>
        <Switch label={label} isSelected={value} isDisabled={locked} onChange={onChange} />
      </span>
    );

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("firewall.title", "Firewall")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("firewall.desc", "Manage firewall, NAT, DMZ, UPnP and port forwarding.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={fwApi.data && fwApi.stale ? <Freshness stale lastOkAt={fwApi.lastOkAt} what={t("firewall.settingsWord", "Settings")} /> : undefined}
          actions={
            !fwApi.data && fwApi.error ? (
              <Button variant="secondary" size="sm" onPress={() => fwApi.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── switches ── */}
        <section>
          <Group title={t("firewall.switches", "Switches")} stale={fwApi.stale}>
            {!fwApi.data ? (
              <div className="nd-row">
                <span className="nd-skel" style={{ width: "16ch" }} />
              </div>
            ) : (
              <>
                <Row
                  label={t("firewall.firewall", "Firewall")}
                  control={switchCtl(fw.enabled, t("firewall.firewall", "Firewall"), (v) => askSwitch("firewall", v), fwLocked)}
                />
                <div className="nd-row flex-wrap gap-y-2">
                  <span className="nd-row__text">
                    <span className="nd-row__label">{t("firewall.firewallLevel", "Firewall Level")}</span>
                    <span className="nd-row__sub block">
                      {t("firewall.current", "Current: {{level}}", {
                        level: fw.level ? levelLabel(t, fw.level) : fw.levelRaw ?? "—",
                      })}
                    </span>
                  </span>
                  <span {...trig("switches")}>
                    <Segmented<Level>
                      label={t("firewall.firewallLevel", "Firewall Level")}
                      value={fw.level ?? null}
                      isDisabled={fwLocked || fw.level === undefined}
                      onChange={askLevel}
                      options={(["low", "medium", "high"] as const).map((l) => ({ id: l, label: levelLabel(t, l) }))}
                    />
                  </span>
                </div>
                <Row label="NAT" control={switchCtl(fw.nat, "NAT", (v) => askSwitch("nat", v), fwLocked)} />
                <Row
                  label="UPnP"
                  sub={
                    upnpApi.error && !upnpApi.data
                      ? upnpApi.unsupported
                        ? t("firewall.upnpUnsupported", "Not available on this firmware: the device doesn't answer the UPnP request, so it can't be read or changed here.")
                        : t("firewall.upnpUnreadable", "Couldn't read UPnP: {{msg}}", { msg: upnpApi.error.message })
                      : undefined
                  }
                  control={
                    !upnpApi.data && !upnpApi.error ? (
                      <span className="nd-skel" style={{ width: 44 }} />
                    ) : (
                      switchCtl(upnp, "UPnP", askUpnp, !upnpApi.data || upnpApi.stale || busy)
                    )
                  }
                />
                <Row
                  label={t("firewall.portForwarding", "Port Forwarding")}
                  control={switchCtl(fw.portForward, t("firewall.portForwarding", "Port Forwarding"), (v) => askSwitch("pf", v), fwLocked)}
                />
                {fw.wanPing !== undefined && (
                  <Row label={t("firewall.wanPing", "Answer ping from the internet")} value={fw.wanPing ? t("common.on", "On") : t("common.off", "Off")} />
                )}
                {fw.wanPingFilter !== undefined && (
                  <Row label={t("firewall.wanPingFilter", "Block ping from the internet")} value={fw.wanPingFilter ? t("common.on", "On") : t("common.off", "Off")} />
                )}
                {fw.remoteWeb !== undefined && (
                  <Row label={t("firewall.remoteWeb", "Remote web management")} value={fw.remoteWeb ? t("common.on", "On") : t("common.off", "Off")} />
                )}
              </>
            )}
          </Group>
          {confirmHere("switches")}
          {fwApi.stale && fwApi.data && (
            <p className="nd-aux mt-1 px-1">
              <Freshness stale lastOkAt={fwApi.lastOkAt} what={t("firewall.settingsWord", "Settings")} />
              {t("firewall.refreshToEdit", " — refresh before changing anything.")}
            </p>
          )}
        </section>

        {/* ── DMZ ── */}
        <section aria-labelledby="dmz-title">
          <GroupTitle id="dmz-title">DMZ</GroupTitle>
          <div className={`nd-group${fwApi.stale ? " nd-stale" : ""}`}>
            {!fwApi.data ? (
              <div className="nd-row">
                <span className="nd-skel" style={{ width: "16ch" }} />
              </div>
            ) : !dmz ? (
              <div className="nd-row text-nd-t2">{t("firewall.dmzUnknown", "The device's reply has no DMZ value this page recognises.")}</div>
            ) : (
              <>
                <Row
                  label="DMZ"
                  sub={dmz.on ? t("firewall.enabled", "Enabled") : t("firewall.disabled", "Disabled")}
                  control={
                    <Switch
                      label="DMZ"
                      isSelected={dmz.on}
                      isDisabled={fwLocked}
                      onChange={(v) => {
                        setDmzDraft({ ...dmz, on: v });
                        setDmzErr(null);
                      }}
                    />
                  }
                />
                <div className="nd-row flex-wrap gap-3">
                  <label className="grid min-w-0 flex-1 gap-1">
                    <span className="nd-aux">{t("firewall.dmzHostIp", "DMZ Host IP")}</span>
                    <input
                      className="nd-field nd-mono"
                      inputMode="decimal"
                      autoComplete="off"
                      value={dmz.ip}
                      placeholder={lanAddr ? lanAddr.replace(/\.\d+$/, ".x") : "192.168.0.x"}
                      disabled={fwLocked || !dmz.on}
                      onChange={(e) => {
                        setDmzDraft({ ...dmz, ip: e.target.value });
                        setDmzErr(null);
                      }}
                    />
                  </label>
                  <span {...trig("dmz")} className="self-end">
                    <Button variant="secondary" onPress={askDmz} isDisabled={fwLocked}>
                      {t("firewall.apply", "Apply")}
                    </Button>
                  </span>
                </div>
              </>
            )}
          </div>
          {dmzErr && (
            <p role="alert" className="mt-2 px-1 nd-error">
              {dmzErr}
            </p>
          )}
          <p className="nd-aux mt-2 px-1">{t("firewall.dmzNote", "Exposing a host via DMZ bypasses firewall protections. Changes apply when you press Apply.")}</p>
          {confirmHere("dmz")}
        </section>

        {/* ── port forwarding ── */}
        <section aria-labelledby="pf-title">
          <div className="flex flex-wrap items-center gap-2">
            <div className="flex-1">
              <GroupTitle id="pf-title">{t("firewall.portForwarding", "Port Forwarding")}</GroupTitle>
            </div>
            {pfRules && <span className="nd-aux">{t("firewall.rulesCount", "{{n}} rules", { n: pfRules.length })}</span>}
            <Button
              variant="secondary"
              size="sm"
              aria-expanded={showAdd}
              isDisabled={!showAdd && !pfListable}
              onPress={() => setShowAdd((v) => !v)}
            >
              {showAdd ? <X size={18} weight="bold" aria-hidden /> : <Plus size={18} weight="bold" aria-hidden />}
              {showAdd ? t("firewall.cancel", "Cancel") : t("firewall.addRule", "Add Rule")}
            </Button>
          </div>

          {showAdd && (
            <div className="nd-group mb-3 grid gap-3 p-4 lg:p-5">
              <div className="grid gap-3 sm:grid-cols-2">
                <label className="grid gap-1">
                  <span className="nd-aux">{t("firewall.name", "Name")}</span>
                  <input
                    className="nd-field"
                    value={pf.name}
                    onChange={(e) => setPf({ ...pf, name: e.target.value })}
                    placeholder={t("firewall.ruleNamePlaceholder", "Rule name")}
                    disabled={busy}
                  />
                </label>
                <label className="grid gap-1">
                  <span className="nd-aux">{t("firewall.protocol", "Protocol")}</span>
                  <select className="nd-field" value={pf.protocol} onChange={(e) => setPf({ ...pf, protocol: e.target.value })} disabled={busy}>
                    <option value="TCP">TCP</option>
                    <option value="UDP">UDP</option>
                    <option value="Both">{t("firewall.both", "Both")}</option>
                  </select>
                </label>
                <label className="grid gap-1">
                  <span className="nd-aux">{t("firewall.wanPort", "WAN Port")}</span>
                  <input className="nd-field nd-mono" inputMode="numeric" value={pf.wan} onChange={(e) => setPf({ ...pf, wan: e.target.value })} placeholder="8080" disabled={busy} />
                </label>
                <label className="grid gap-1">
                  <span className="nd-aux">{t("firewall.lanIp", "LAN IP")}</span>
                  <input
                    className="nd-field nd-mono"
                    inputMode="decimal"
                    value={pf.lanIp}
                    onChange={(e) => setPf({ ...pf, lanIp: e.target.value })}
                    placeholder={lanAddr ? lanAddr.replace(/\.\d+$/, ".x") : "192.168.0.x"}
                    disabled={busy}
                  />
                </label>
                <label className="grid gap-1">
                  <span className="nd-aux">{t("firewall.lanPort", "LAN Port")}</span>
                  <input className="nd-field nd-mono" inputMode="numeric" value={pf.lanPort} onChange={(e) => setPf({ ...pf, lanPort: e.target.value })} placeholder="80" disabled={busy} />
                </label>
                <div className="flex items-end gap-3">
                  <Switch label={t("firewall.enabled", "Enabled")} isSelected={pf.enabled} onChange={(v) => setPf({ ...pf, enabled: v })} isDisabled={busy} />
                  <span className="nd-body pb-3" aria-hidden>
                    {t("firewall.enabled", "Enabled")}
                  </span>
                </div>
              </div>
              {pfErr && (
                <p role="alert" className="nd-error">
                  {pfErr}
                </p>
              )}
              <div>
                <span {...trig("pfAdd")}>
                  <Button onPress={askAdd} isDisabled={busy || !pfListable}>
                    {t("firewall.saveRule", "Save Rule")}
                  </Button>
                </span>
              </div>
            </div>
          )}
          {confirmHere("pfAdd")}

          <div className={`nd-group${pfApi.stale ? " nd-stale" : ""}`}>
            {pfRules === undefined && !pfApi.error ? (
              <div className="nd-row">
                <span className="nd-skel" style={{ width: "20ch" }} />
              </div>
            ) : pfApi.error && pfRules === undefined ? (
              <div className="nd-row flex-wrap">
                <span className="flex-1 text-nd-t2">{t("firewall.pfUnreadable", "Couldn't read the rules: {{msg}}", { msg: pfApi.error.message })}</span>
                <Button variant="secondary" size="sm" onPress={() => pfApi.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              </div>
            ) : pfRules === null ? (
              <div className="nd-row text-nd-t2">{t("firewall.listShapeUnknown", "The device answered in a format this page doesn't recognise, so the rules can't be listed.")}</div>
            ) : pfRules && pfRules.length === 0 ? (
              <div className="nd-row text-nd-t2">
                {t("firewall.noPortForwardRules", "No port forwarding rules.")} {t("firewall.noRulesNext", "Use “Add Rule” above to create one.")}
              </div>
            ) : (
              pfRules?.map((r, i) => (
                <Row
                  key={r.id ?? `i${i}`}
                  label={r.name || "—"}
                  sub={
                    <span className="nd-mono">
                      {`${r.protocol ?? "—"} · WAN ${r.wanPort ?? "—"} → ${r.lanIp ?? "—"}:${r.lanPort ?? "—"}`}
                      {r.enabled === false ? ` · ${t("common.off", "Off")}` : ""}
                    </span>
                  }
                  control={
                    <span {...(pending?.body.id === r.id ? trig("pfList") : {})}>
                      <Button
                        variant="ghost"
                        iconOnly
                        isDisabled={busy || !r.id || pfApi.stale}
                        aria-label={t("firewall.deleteRuleNamed", "Delete rule {{name}}", { name: r.name || r.id || "" })}
                        onPress={() => askDelete(r)}
                      >
                        <Trash size={20} weight="bold" aria-hidden />
                      </Button>
                    </span>
                  }
                />
              ))
            )}
          </div>
          {confirmHere("pfList")}
        </section>

        {/* ── filter rules (read-only) ── */}
        <section aria-labelledby="filter-title">
          <GroupTitle id="filter-title">{t("firewall.filterRulesTitle", "Filter Rules (read-only)")}</GroupTitle>
          <div className={`nd-group${filterApi.stale ? " nd-stale" : ""}`}>
            {filterRules === undefined && !filterApi.error ? (
              <div className="nd-row">
                <span className="nd-skel" style={{ width: "20ch" }} />
              </div>
            ) : filterApi.error && filterRules === undefined ? (
              <div className="nd-row flex-wrap">
                <span className="flex-1 text-nd-t2">{t("firewall.filterUnreadable", "Couldn't read the filter rules: {{msg}}", { msg: filterApi.error.message })}</span>
                <Button variant="secondary" size="sm" onPress={() => filterApi.mutate()}>
                  {t("common.retry", "Retry")}
                </Button>
              </div>
            ) : filterRules === null ? (
              <div className="nd-row text-nd-t2">{t("firewall.listShapeUnknown", "The device answered in a format this page doesn't recognise, so the rules can't be listed.")}</div>
            ) : filterRules && filterRules.length === 0 ? (
              <div className="nd-row text-nd-t2">{t("firewall.noFilterRules", "No filter rules configured.")}</div>
            ) : (
              filterRules?.map((r, i) => <FilterRow key={r.id ?? `i${i}`} r={r} t={t} />)
            )}
          </div>
        </section>
      </div>

      <ConfirmDialog
        open={pending !== null && pending.tier === 3}
        onOpenChange={(o) => !o && setPending(null)}
        title={pending?.title ?? ""}
        what={pending?.consequence}
        recovery={recovery}
        actionLabel={pending?.action ?? ""}
        cutsUplink
        onConfirm={go}
      />
    </>
  );
}

function FilterRow({ r, t }: { r: FilterRule; t: T }) {
  const parts: ReactNode[] = [];
  const v = (x: ReactNode) => <span className="nd-mono">{x}</span>;
  parts.push(v(r.protocol ?? "—"));
  if (r.srcMac) parts.push(<>MAC {v(r.srcMac)}</>);
  parts.push(<>{t("firewall.srcIp", "Src IP")} {v(`${r.srcIp || "—"}${r.srcPort ? `:${r.srcPort}` : ""}`)}</>);
  parts.push(<>{t("firewall.dstIp", "Dst IP")} {v(r.dstIp || "—")}</>);
  parts.push(<>{t("firewall.dstPort", "Dst Port")} {v(r.dstPort || "—")}</>);
  if (r.action && r.enabled !== undefined) parts.push(r.enabled ? t("common.on", "On") : t("common.off", "Off"));
  return (
    <Row
      label={r.name || "—"}
      sub={parts.map((p, i) => (
        <span key={i}>
          {i > 0 && " · "}
          {p}
        </span>
      ))}
      value={
        r.action ? (
          <span>
            {t("firewall.action", "Action")} {r.action}
          </span>
        ) : r.enabled !== undefined ? (
          <StatusMark tone={r.enabled ? "ok" : "neutral"}>{r.enabled ? t("common.on", "On") : t("common.off", "Off")}</StatusMark>
        ) : (
          "—"
        )
      }
    />
  );
}
