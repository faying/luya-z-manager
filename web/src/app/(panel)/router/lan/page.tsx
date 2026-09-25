"use client";
// LAN / DHCP (new design). Status first (the U60's address and whether it
// hands out addresses), then the IP settings and the DHCP server, then one
// Apply — one PUT /api/router/lan with all six fields, as before.
//
// Writes (controls-inventory §/router/lan, design §3.1): tier 2 locally,
// tier 3 over Tailscale. The PUT goes straight to ubus router_set_lan_para
// (router.rs:82), so the result is read back from GET /api/router/lan. The
// LAN restarts: the page waits for the device (30 s). When the address
// changes and the page is reached over the LAN, the old address never
// answers again — the wait expects that, and the recovery names the new
// address (http://<new-ip>:9090) from the start.
//
// Agent vs page (schemas/router.ts): dhcp_start is the raw uci host number
// ("100"), dhcp_end is a full IP the agent computes, and dhcp_lease_time is
// raw uci (OpenWrt often stores "12h"). The old page demanded a whole number
// of seconds and blocked submit on "12h"; units are accepted now. Both
// range fields are sent back in the form they were read — which form
// router_set_lan_para expects is unconfirmed.
import { useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp } from "@/lib/api/writeOp";
import type { RouterLan, RouterLanSetBody } from "@/lib/api/schemas/router";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

type LanForm = RouterLanSetBody;
const KEYS: (keyof LanForm)[] = ["lan_ipaddr", "lan_netmask", "dhcp_enable", "dhcp_start", "dhcp_end", "dhcp_lease_time"];

function parseIPv4(s: string): number[] | null {
  const parts = s.trim().split(".");
  if (parts.length !== 4) return null;
  if (!parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) <= 255)) return null;
  return parts.map(Number);
}
const toInt = (o: number[]) => ((o[0] << 24) >>> 0) + (o[1] << 16) + (o[2] << 8) + o[3];

function isNetmask(s: string): boolean {
  const o = parseIPv4(s);
  if (!o) return false;
  const n = toInt(o);
  if (n === 0) return false;
  const inv = ~n >>> 0;
  return (inv & (inv + 1)) === 0; // contiguous ones
}

/** Host part of "100" or "10.0.66.100". */
function hostOf(s: string): string {
  const v = s.trim();
  if (/^\d{1,3}$/.test(v)) return String(Number(v));
  const o = parseIPv4(v);
  return o ? String(o[3]) : v;
}

/** OpenWrt lease time → seconds ("12h" → 43200); null if not a lease time. */
function leaseSeconds(s: string): number | null {
  const m = /^(\d+)([smhdw]?)$/.exec(s.trim());
  if (!m) return null;
  const mult = { "": 1, s: 1, m: 60, h: 3600, d: 86400, w: 604800 }[m[2] as "" | "s" | "m" | "h" | "d" | "w"];
  return Number(m[1]) * mult;
}

function seed(d: RouterLan): LanForm {
  return {
    lan_ipaddr: d.lan_ipaddr ?? "",
    lan_netmask: d.lan_netmask ?? "",
    dhcp_enable: d.dhcp_enable === "1" ? "1" : "0",
    dhcp_start: d.dhcp_start ?? "",
    dhcp_end: d.dhcp_end ?? "",
    dhcp_lease_time: d.dhcp_lease_time ?? "",
  };
}

function readbackMatches(sent: LanForm, d: RouterLan): boolean {
  if ((d.lan_ipaddr ?? "") !== sent.lan_ipaddr.trim()) return false;
  if ((d.lan_netmask ?? "") !== sent.lan_netmask.trim()) return false;
  if ((d.dhcp_enable === "1" ? "1" : "0") !== sent.dhcp_enable) return false;
  if (sent.dhcp_enable === "0") return true;
  if (hostOf(d.dhcp_start ?? "") !== hostOf(sent.dhcp_start)) return false;
  if (hostOf(d.dhcp_end ?? "") !== hostOf(sent.dhcp_end)) return false;
  return leaseSeconds(d.dhcp_lease_time ?? "") === leaseSeconds(sent.dhcp_lease_time);
}

const adminUrl = (ip: string) => `http://${ip.trim()}:9090`;

interface Pending {
  tier: 2 | 3;
  body: LanForm;
  consequence: ReactNode;
  recovery: string;
  newIp: string | null;
  expectDown: boolean;
}

export default function LanPage() {
  const { t } = useTranslation();
  const lan = useApi<RouterLan>("/api/router/lan");
  const data = lan.data;

  const [draft, setDraft] = useState<LanForm | null>(null);
  const base = data ? seed(data) : null;
  const form = draft ?? base;
  const changed = form && base ? KEYS.filter((k) => form[k] !== base[k]) : [];

  const [pending, setPending] = useState<Pending | null>(null);
  const [newIpNote, setNewIpNote] = useState<string | null>(null);
  const sentRef = useRef<LanForm | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);

  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: [
      {
        label: t("lan.stepSettings", "LAN settings"),
        run: () => apiFetch("/api/router/lan", { method: "PUT", body: sentRef.current }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<RouterLan>("/api/router/lan");
      await lan.mutate(d, { revalidate: false });
      const ok = sentRef.current !== null && readbackMatches(sentRef.current, d);
      if (ok) setDraft(null);
      return ok;
    },
    waitDevice: pending ? { expectedSec: 30, expectDown: pending.expectDown, recovery: pending.recovery } : undefined,
  });
  const busy = op.busy;
  const locked = !data || lan.stale || busy;

  function set<K extends keyof LanForm>(k: K, v: LanForm[K]) {
    if (!form) return;
    setDraft({ ...form, [k]: v });
  }

  // ── validation ──
  const errs: string[] = [];
  if (form) {
    const ip = parseIPv4(form.lan_ipaddr);
    const maskOk = isNetmask(form.lan_netmask);
    if (!ip) errs.push(t("lan.errInvalidIp", "Invalid LAN IP address"));
    if (!maskOk) errs.push(t("lan.errInvalidNetmask", "Invalid netmask"));
    if (form.dhcp_enable === "1") {
      const lease = leaseSeconds(form.dhcp_lease_time);
      if (lease === null || lease <= 0)
        errs.push(t("lan.errInvalidLeaseUnits", "Lease time must be a positive number of seconds, or a number with a unit such as 12h"));
      const endpoint = (s: string, which: string) => {
        const v = s.trim();
        if (/^\d{1,3}$/.test(v)) {
          if (Number(v) < 1 || Number(v) > 254) errs.push(t("lan.errRangeHost", "{{which}}: a host number is 1–254", { which }));
          return;
        }
        const o = parseIPv4(v);
        if (!o) {
          errs.push(t("lan.errRangeFormat", "{{which}}: enter a host number such as 100, or a full address", { which }));
          return;
        }
        if (ip && maskOk) {
          const m = toInt(parseIPv4(form.lan_netmask)!);
          if ((toInt(o) & m) >>> 0 !== (toInt(ip) & m) >>> 0)
            errs.push(t("lan.errRangeSubnet", "{{which}} {{addr}} is not in the LAN's network", { which, addr: v }));
        }
      };
      endpoint(form.dhcp_start, t("lan.startAddress", "Start Address"));
      endpoint(form.dhcp_end, t("lan.endAddress", "End Address"));
      if (!errs.length && Number(hostOf(form.dhcp_start)) > Number(hostOf(form.dhcp_end)))
        errs.push(t("lan.errRangeOrder", "The start address comes after the end address"));
    }
  }

  function describe(f: LanForm, b: LanForm) {
    const lines: string[] = [];
    const ipChanged = f.lan_ipaddr.trim() !== b.lan_ipaddr;
    const ip = f.lan_ipaddr.trim();
    if (ipChanged) lines.push(t("lan.cIp", "The U60's address becomes {{ip}}. This page then opens at {{url}}.", { ip, url: adminUrl(ip) }));
    if (f.lan_netmask !== b.lan_netmask) lines.push(t("lan.cMask", "The netmask becomes {{mask}}.", { mask: f.lan_netmask.trim() }));
    if (f.dhcp_enable !== b.dhcp_enable)
      lines.push(
        f.dhcp_enable === "0"
          ? t("lan.cDhcpOff", "DHCP goes off: devices that join get no address and need one set by hand.")
          : t("lan.cDhcpOn", "DHCP comes on."),
      );
    if (f.dhcp_enable === "1" && (f.dhcp_start !== b.dhcp_start || f.dhcp_end !== b.dhcp_end || f.dhcp_lease_time !== b.dhcp_lease_time))
      lines.push(
        t("lan.cRange", "Addresses handed out: {{start}} – {{end}}, lease {{lease}}.", {
          start: f.dhcp_start.trim(),
          end: f.dhcp_end.trim(),
          lease: f.dhcp_lease_time.trim(),
        }),
      );
    lines.push(t("lan.cRestart", "The LAN restarts: connected devices drop for a moment and fetch a new address."));
    const recovery = ipChanged
      ? t("lan.recoveryNewIp", "Open {{url}}. If it doesn't load, disconnect and reconnect your device's Wi-Fi (or replug the cable) so it gets an address in the new network.", { url: adminUrl(ip) })
      : t("lan.recoverySame", "Wait a moment and reload this page. If it still doesn't load, disconnect and reconnect your device's Wi-Fi (or replug the cable).");
    return {
      consequence: lines.join(" "),
      recovery,
      newIp: ipChanged ? ip : null,
      // Over the LAN the old address stops answering for good, so "back"
      // must not be taken from a probe that lands before the restart.
      expectDown: ipChanged && !isRemoteAccess(),
    };
  }

  function askApply() {
    if (!form || !base || busy || changed.length === 0 || errs.length > 0) return;
    const body: LanForm = { ...form, lan_ipaddr: form.lan_ipaddr.trim(), lan_netmask: form.lan_netmask.trim() };
    setPending({ tier: isRemoteAccess() ? 3 : 2, body, ...describe(form, base) });
  }

  function go() {
    if (!pending) return;
    sentRef.current = pending.body;
    setNewIpNote(pending.newIp);
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("lan.loading", "Reading the LAN settings…");
  let reason: ReactNode = null;
  if (!data && lan.error) {
    tone = "bad";
    state = t("lan.unreadable", "Can't read the LAN settings");
    reason = lan.error.message;
  } else if (data) {
    if (!data.lan_ipaddr) {
      tone = "warn";
      state = t("lan.noAddress", "LAN address unreadable");
      reason = t("lan.noAddressReason", "The device returned no LAN address. Reload before changing anything.");
    } else if (data.dhcp_enable === "1") {
      tone = "ok";
      state = t("lan.statusDhcpOn", "DHCP on");
      reason = (
        <>
          {t("lan.handsOut", "Hands out")} <span className="nd-mono">{data.dhcp_start || "—"}</span> –{" "}
          <span className="nd-mono">{data.dhcp_end || "—"}</span>
        </>
      );
    } else {
      state = t("lan.statusDhcpOff", "DHCP off");
      reason = t("lan.dhcpOffReason", "Devices need an address set by hand.");
    }
    if (lan.stale) tone = "stale";
  }

  const showNewIp = newIpNote !== null && ["waitDevice", "waitTimeout", "unknown", "verifying", "applied", "failed"].includes(op.phase);
  const skeleton = (
    <div className="nd-group">
      {[0, 1].map((i) => (
        <div key={i} className="nd-row">
          <span className="nd-skel" style={{ width: "10ch" }} />
        </div>
      ))}
    </div>
  );

  function field(k: "lan_ipaddr" | "lan_netmask" | "dhcp_start" | "dhcp_end" | "dhcp_lease_time", label: string, placeholder: string, sub?: string, disabled?: boolean) {
    if (!form) return null;
    const id = `lan-${k}`;
    return (
      <div className="nd-row flex-wrap">
        <span className="nd-row__text">
          <label htmlFor={id} className="nd-row__label">
            {label}
          </label>
          {sub && <span className="nd-row__sub block">{sub}</span>}
        </span>
        <input
          id={id}
          className="nd-field nd-mono sm:w-56"
          value={form[k]}
          placeholder={placeholder}
          disabled={locked || disabled}
          inputMode={k === "dhcp_lease_time" ? "text" : "decimal"}
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => set(k, e.target.value)}
        />
      </div>
    );
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("lan.title", "LAN Settings")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("lan.desc", "Configure the router's LAN IP and DHCP server.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={
            data ? (
              <span className="grid">
                <span>
                  {t("lan.u60At", "U60 at")} <span className="nd-mono">{data.lan_ipaddr || "—"}</span> /{" "}
                  <span className="nd-mono">{data.lan_netmask || "—"}</span>
                </span>
                {lan.stale && <Freshness stale lastOkAt={lan.lastOkAt} />}
              </span>
            ) : undefined
          }
          actions={
            !data && lan.error ? (
              <Button variant="secondary" size="sm" onPress={() => lan.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {lan.stale && data && (
          <p className="nd-aux px-1">
            <Freshness stale lastOkAt={lan.lastOkAt} what={t("wifi.settingsWord", "Settings")} />
            {t("wifi.refreshToEdit", " — refresh before changing anything.")}{" "}
            <Button variant="secondary" size="sm" onPress={() => lan.mutate()}>
              {t("wifi.refresh", "Refresh")}
            </Button>
          </p>
        )}

        <section>
          <GroupTitle>{t("lan.ipConfig", "IP Configuration")}</GroupTitle>
          {!form ? (
            skeleton
          ) : (
            <div className={`nd-group${lan.stale ? " nd-stale" : ""}`}>
              {field("lan_ipaddr", t("lan.lanIpAddress", "LAN IP Address"), "192.168.0.1")}
              {field("lan_netmask", t("lan.subnetMask", "Subnet Mask"), "255.255.255.0")}
            </div>
          )}
        </section>

        <section>
          <GroupTitle>{t("lan.dhcpServer", "DHCP Server")}</GroupTitle>
          {!form ? (
            skeleton
          ) : (
            <div className={`nd-group${lan.stale ? " nd-stale" : ""}`}>
              <Row
                label={t("lan.dhcpServer", "DHCP Server")}
                sub={form.dhcp_enable === "1" ? t("lan.enabled", "Enabled") : t("lan.disabled", "Disabled")}
                control={
                  <Switch
                    label={t("lan.dhcpServer", "DHCP Server")}
                    isSelected={form.dhcp_enable === "1"}
                    isDisabled={locked}
                    onChange={(on) => set("dhcp_enable", on ? "1" : "0")}
                  />
                }
              />
              {field("dhcp_start", t("lan.startAddress", "Start Address"), "100", t("lan.startSub", "Host number (e.g. 100) or full address"), form.dhcp_enable !== "1")}
              {field("dhcp_end", t("lan.endAddress", "End Address"), "192.168.0.149", undefined, form.dhcp_enable !== "1")}
              {field("dhcp_lease_time", t("lan.leaseTime", "Lease time"), "12h", t("lan.leaseSub", "Seconds, or with a unit: 30m, 12h, 1d"), form.dhcp_enable !== "1")}
            </div>
          )}
        </section>

        <section aria-labelledby="lan-apply-title">
          <GroupTitle id="lan-apply-title">{t("lan.apply", "Apply")}</GroupTitle>
          <div className="nd-group grid gap-3 p-4 lg:p-5">
            <p className="nd-aux" role="status">
              {!form
                ? t("lan.loading", "Reading the LAN settings…")
                : changed.length === 0
                  ? t("wifi.noChanges", "No changes yet. Change a setting above, then apply.")
                  : t("wifi.nChanges", "{{n}} change(s) not applied yet.", { n: changed.length })}
            </p>
            {changed.length > 0 && errs.length > 0 && (
              <ul className="grid gap-1" role="alert">
                {errs.map((e) => (
                  <li key={e}>
                    <StatusMark tone="bad">{e}</StatusMark>
                  </li>
                ))}
              </ul>
            )}
            <div className="flex flex-wrap items-center gap-3">
              <span {...(pending?.tier === 2 ? inline.triggerProps : {})}>
                <Button onPress={askApply} isDisabled={locked || changed.length === 0 || errs.length > 0} pending={busy}>
                  {t("lan.apply", "Apply")}
                </Button>
              </span>
              {changed.length > 0 && (
                <Button variant="secondary" isDisabled={busy} onPress={() => setDraft(null)}>
                  {t("wifi.discard", "Discard changes")}
                </Button>
              )}
            </div>
            {pending?.tier === 2 && (
              <ConfirmInline
                id={inline.id}
                open
                actionLabel={t("lan.applyAction", "apply the LAN settings")}
                consequence={pending.consequence}
                onCancel={() => setPending(null)}
                onConfirm={go}
              />
            )}
            <OpResult op={op} />
            {showNewIp && newIpNote && (
              <p className="nd-body" role="status">
                <StatusMark tone="warn">
                  {t("lan.newAddressNote", "The U60 is now at {{ip}}. Open", { ip: newIpNote })}{" "}
                  <a className="nd-mono text-nd-accT underline" href={adminUrl(newIpNote)}>
                    {adminUrl(newIpNote)}
                  </a>
                </StatusMark>
              </p>
            )}
          </div>
        </section>
      </div>

      {pending?.tier === 3 && (
        <ConfirmDialog
          open
          onOpenChange={(o) => !o && setPending(null)}
          title={t("lan.confirmTitle", "Apply the LAN settings?")}
          what={pending.consequence}
          downtime={t("lan.downtime", "About 30 seconds while the LAN restarts; connected devices drop.")}
          recovery={pending.recovery}
          actionLabel={t("nd.confirmAction", "Confirm: {{action}}", { action: t("lan.applyAction", "apply the LAN settings") })}
          cutsUplink
          onConfirm={go}
        />
      )}
    </>
  );
}
