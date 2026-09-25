"use client";
// VPN passthrough (new design). Status first (which passthroughs are on),
// the note on what passthrough is, then the three switches.
//
// Writes (controls-inventory §/router/vpn, design §3.1): each switch is its
// own PUT /api/router/vpn {<field>: "0"|"1"} (ubus router_set_alg_switch,
// passed through) — tier 2 locally, tier 3 over Tailscale — read back from
// GET /api/router/vpn. The GET shape comes from the old page and is
// unconfirmed on the device: a field the device doesn't return shows as
// unknown ("—"), not as off; its switch stays usable.
// B27 (recorded 2026-09-25) returns only {alg_sip_enable}: none of the three
// passthrough keys. The page then says the firmware doesn't report them
// (neutral, not an error) and shows SIP ALG read-only — the agent's PUT would
// forward an alg_sip_enable change to router_set_alg_switch, but whether the
// firmware honours that key is unknown, so no write is offered for it.
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp } from "@/lib/api/writeOp";
import type { RouterVpn } from "@/lib/api/schemas/router";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

type Field = "l2tp_passthrough" | "pptp_passthrough" | "ipsec_passthrough";
const FIELDS: Field[] = ["l2tp_passthrough", "pptp_passthrough", "ipsec_passthrough"];
const PROTO: Record<Field, string> = { l2tp_passthrough: "L2TP", pptp_passthrough: "PPTP", ipsec_passthrough: "IPSec" };

/** "1" / "0" as the device reports it, or null when it isn't reported. */
function read(d: RouterVpn | undefined, f: Field): boolean | null {
  // Passthrough of ubus JSON: accept 1/"1"/true in case the shape differs.
  const v = String((d as Record<string, unknown> | undefined)?.[f] ?? "");
  if (v === "1" || v === "true") return true;
  if (v === "0" || v === "false") return false;
  return null;
}

interface Pending {
  tier: 2 | 3;
  field: Field;
  on: boolean;
}

/** SIP ALG as reported (B27), or null. */
function readSip(d: RouterVpn | undefined): boolean | null {
  const v = String(d?.alg_sip_enable ?? "");
  if (v === "1" || v === "true") return true;
  if (v === "0" || v === "false") return false;
  return null;
}

export default function VpnPage() {
  const { t } = useTranslation();
  const vpn = useApi<RouterVpn>("/api/router/vpn");
  const data = vpn.data;

  const [pending, setPending] = useState<Pending | null>(null);
  const wantRef = useRef<{ field: Field; on: boolean } | null>(null);
  const [shownFor, setShownFor] = useState<{ field: Field; on: boolean } | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);

  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: [
      {
        label: t("vpn.stepSwitch", "Passthrough"),
        run: () => {
          const w = wantRef.current!;
          return apiFetch("/api/router/vpn", { method: "PUT", body: { [w.field]: w.on ? "1" : "0" } });
        },
      },
    ],
    verify: async () => {
      const d = await apiFetch<RouterVpn>("/api/router/vpn");
      await vpn.mutate(d, { revalidate: false });
      const w = wantRef.current;
      return w !== null && read(d, w.field) === w.on;
    },
  });
  const busy = op.busy;
  const locked = !data || vpn.stale || busy;

  const labels: Record<Field, { label: string; desc: string }> = {
    l2tp_passthrough: { label: t("vpn.l2tpLabel", "L2TP Passthrough"), desc: t("vpn.l2tpDesc", "Layer 2 Tunneling Protocol") },
    pptp_passthrough: { label: t("vpn.pptpLabel", "PPTP Passthrough"), desc: t("vpn.pptpDesc", "Point-to-Point Tunneling Protocol") },
    ipsec_passthrough: { label: t("vpn.ipsecLabel", "IPSec Passthrough"), desc: t("vpn.ipsecDesc", "Internet Protocol Security") },
  };

  function consequence(p: { field: Field; on: boolean }) {
    const proto = PROTO[p.field];
    return p.on
      ? t("vpn.cOn", "Devices on the U60 can open {{proto}} VPN connections to servers outside.", { proto })
      : t("vpn.cOff", "{{proto}} VPN connections from devices on the U60 stop working; open ones drop.", { proto });
  }
  const action = (p: { field: Field; on: boolean }) =>
    p.on
      ? t("vpn.turnOn", "turn {{proto}} passthrough on", { proto: PROTO[p.field] })
      : t("vpn.turnOff", "turn {{proto}} passthrough off", { proto: PROTO[p.field] });

  function ask(field: Field, on: boolean) {
    if (busy) return;
    setPending({ tier: isRemoteAccess() ? 3 : 2, field, on });
  }

  function go() {
    if (!pending) return;
    wantRef.current = { field: pending.field, on: pending.on };
    setShownFor(wantRef.current);
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: string = t("vpn.loading", "Reading VPN passthrough…");
  let reason: string | null = null;
  if (!data && vpn.error) {
    tone = "bad";
    state = t("vpn.unreadable", "Can't read VPN passthrough");
    reason = vpn.error.message;
  } else if (data) {
    const on = FIELDS.filter((f) => read(data, f) === true).map((f) => PROTO[f]);
    const unknown = FIELDS.filter((f) => read(data, f) === null).map((f) => PROTO[f]);
    const sip = readSip(data);
    if (unknown.length === FIELDS.length && sip !== null) {
      state = t("vpn.statusNotReported", "This firmware doesn't report VPN passthrough");
      reason = t(
        "vpn.statusOnlySip",
        "It only reports SIP ALG ({{sip}}, shown below). The L2TP / PPTP / IPSec switches still send a change, but the page can't read it back to confirm it.",
        { sip: sip ? t("common.on", "On") : t("common.off", "Off") }
      );
    } else if (unknown.length === FIELDS.length) {
      tone = "warn";
      state = t("vpn.statusUnknown", "Passthrough state unknown");
      reason = t("vpn.statusUnknownReason", "The device answered without any passthrough fields.");
    } else if (on.length === 0) {
      state = t("vpn.statusAllOff", "All passthrough off");
      reason = t("vpn.statusAllOffReason", "VPN clients behind the U60 may fail to connect.");
    } else {
      tone = "ok";
      state = t("vpn.statusOn", "On: {{list}}", { list: on.join(" · ") });
      reason = unknown.length ? t("vpn.statusSomeUnknown", "Not reported: {{list}}", { list: unknown.join(" · ") }) : null;
    }
    if (vpn.stale) tone = "stale";
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("vpn.title", "VPN Passthrough")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">
        {t("vpn.desc", "Allow VPN clients behind NAT to initiate tunnels to external servers.")}
      </p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={data && vpn.stale ? <Freshness stale lastOkAt={vpn.lastOkAt} /> : undefined}
          actions={
            !data && vpn.error ? (
              <Button variant="secondary" size="sm" onPress={() => vpn.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        <section>
          <GroupTitle>{t("vpn.settingsTitle", "Passthrough Settings")}</GroupTitle>
          <p className="nd-aux -mt-1 mb-3 px-1">
            {t("vpn.infoNote", "Passthrough lets a client behind NAT initiate a VPN connection to an outside server; it does not run a VPN server on the router.")}
          </p>
          <div className={`nd-group${vpn.stale ? " nd-stale" : ""}`}>
            {!data
              ? FIELDS.map((f) => (
                  <div key={f} className="nd-row">
                    <span className="nd-skel" style={{ width: "12ch" }} />
                  </div>
                ))
              : FIELDS.map((f) => {
                  const v = read(data, f);
                  return (
                    <Row
                      key={f}
                      label={labels[f].label}
                      sub={v === null ? `${labels[f].desc} · ${t("vpn.notReportedNoConfirm", "not reported by the device — a change can't be confirmed")}` : labels[f].desc}
                      value={v === null ? "—" : undefined}
                      control={
                        <span {...(pending?.tier === 2 && pending.field === f ? inline.triggerProps : {})}>
                          <Switch
                            label={labels[f].label}
                            isSelected={v === true}
                            isDisabled={locked}
                            onChange={(on) => ask(f, on)}
                          />
                        </span>
                      }
                    />
                  );
                })}
            {data && readSip(data) !== null && (
              <Row
                label={t("vpn.sipLabel", "SIP ALG")}
                sub={t("vpn.sipDesc", "Rewrites SIP (VoIP) signalling through NAT · read-only here")}
                value={readSip(data) ? t("common.on", "On") : t("common.off", "Off")}
              />
            )}
          </div>
          {pending?.tier === 2 && (
            <ConfirmInline
              id={inline.id}
              open
              actionLabel={action(pending)}
              consequence={consequence(pending)}
              onCancel={() => setPending(null)}
              onConfirm={go}
            />
          )}
          {vpn.stale && data && (
            <p className="nd-aux mt-2 px-1">
              <Freshness stale lastOkAt={vpn.lastOkAt} what={t("wifi.settingsWord", "Settings")} />
              {t("wifi.refreshToEdit", " — refresh before changing anything.")}{" "}
              <Button variant="secondary" size="sm" onPress={() => vpn.mutate()}>
                {t("wifi.refresh", "Refresh")}
              </Button>
            </p>
          )}
          {shownFor && op.phase !== "idle" && op.phase !== "confirming" && (
            <div className="mt-2 grid gap-1 px-1">
              <span className="nd-aux">{action(shownFor)}</span>
              <OpResult op={op} />
            </div>
          )}
        </section>
      </div>

      {pending?.tier === 3 && (
        <ConfirmDialog
          open
          onOpenChange={(o) => !o && setPending(null)}
          title={t("vpn.confirmTitle", "Change {{proto}} passthrough?", { proto: PROTO[pending.field] })}
          what={consequence(pending)}
          downtime={t("vpn.downtime", "Usually none; the firewall rules reload in a moment.")}
          recovery={t("vpn.recovery", "Flip the switch back here. If this page stops loading over Tailscale, someone at the device can change it from a device on its Wi-Fi.")}
          actionLabel={t("nd.confirmAction", "Confirm: {{action}}", { action: action(pending) })}
          cutsUplink
          onConfirm={go}
        />
      )}
    </>
  );
}
