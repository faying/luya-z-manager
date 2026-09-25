"use client";
// Mobile network (settings page, design doc §3.1 / §5.1). Status first
// (connected / data off / airplane), then the switches, manual carrier
// selection, and device control (reboot) last.
//
// Writes (controls-inventory §/router/mobile-network):
//   mobile data, roaming   tier 2 locally, tier 3 over Tailscale (turning
//                          either off can cut the uplink the page is using);
//                          readback /api/modem/data enable / roam_enable
//   airplane on / off      tier 3, cutsUplink, waitDevice 30 s; readback
//                          /api/modem/status operate_mode. "Off" is
//                          POST /api/modem/online (AT+CFUN=1), retried once
//                          after 3 s as before.
//   carrier scan           tier 2; POST /api/netinfo/scan, then poll
//                          /api/netinfo `scan` (3 s × 80). The agent runs the
//                          search (≈110 s measured, mobile data down) and
//                          parses the modem's "status,name,plmn,rat;" string.
//   register (row Select)  tier 3 (manual network selection), cutsUplink,
//                          waitDevice 30 s; the agent guards it and goes back
//                          to automatic if it does not take; readback polls
//                          /api/modem/register/guard (3 s × 40)
//   back to automatic      tier 2; POST /api/modem/netselect/auto (AT+COPS=0,
//                          retried by the agent); readback polls the guard
//   reboot                 tier 3, waitDevice 90 s (expect the agent to go away)
// Long jobs run inside the write op, so they keep polling while the tab is
// hidden; they stop when the page is left.
import { connectFamilies, connectKind } from "@/lib/connectState";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Airplane, ArrowClockwise, CellSignalFull, Globe, MagnifyingGlass, Power } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp } from "@/lib/api/writeOp";
import type { ModemData, ModemDataSetBody, ModemScanOperator, ModemStatus } from "@/lib/api/schemas/modem";
import type { NetInfo, NetInfoGuard } from "@/lib/api/schemas/network";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const DATA = "/api/modem/data";
const STATUS = "/api/modem/status";
const SCAN_TRIES = 80;
const REGISTER_TRIES = 40;
const POLL_MS = 2000;
const JOB_POLL_MS = 3000;

/** AT+COPS access technology → generation (measured: 2, 7, 11). */
function ratName(rat?: string): string {
  switch (rat) {
    case "11": case "12": case "13": case "10": return "5G";
    case "7": case "8": case "9": return "4G";
    case "2": case "3": case "4": case "5": case "6": return "3G";
    case "0": case "1": return "2G";
    default: return rat ?? "—";
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The firmware may send 1/0 as numbers or strings; `!!"0"` would read as on. */
function flag(v: unknown): boolean {
  return v === true || v === 1 || v === "1";
}

type Ask =
  | { kind: "data"; next: boolean; tier: 2 | 3 }
  | { kind: "roam"; next: boolean; tier: 2 | 3 }
  | { kind: "air"; next: boolean }
  | { kind: "scan" }
  | { kind: "register"; op: ModemScanOperator }
  | { kind: "auto" }
  | { kind: "reboot" };

export default function MobileNetworkPage() {
  const { t } = useTranslation();
  const md = useApi<ModemData>(DATA, { refreshInterval: 5000 });
  const ms = useApi<ModemStatus>(STATUS, { refreshInterval: 5000 });
  const data = md.data;
  const status = ms.data;

  const airplaneOn = status?.operate_mode !== undefined && status.operate_mode !== "ONLINE";
  const dataOn = flag(data?.enable);
  const roamOn = flag(data?.roam_enable);
  const connectStatus = data?.connect_status ?? "";
  const ck = connectKind(connectStatus);
  const fams = connectFamilies(connectStatus);

  // Loops started by a write op stop when the page is left.
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const checkAlive = () => {
    if (!alive.current) throw new Error("page closed");
  };

  const [ask, setAsk] = useState<Ask | null>(null);
  const askRef = useRef<Ask | null>(null);
  const inline = useConfirmInline(ask?.kind === "scan" || ask?.kind === "auto" || ((ask?.kind === "data" || ask?.kind === "roam") && ask.tier === 2));

  // ── mobile data / roaming (PUT /api/modem/data carries both) ──
  const baseBody = (): ModemDataSetBody => ({
    cid: 1,
    connect_mode: typeof data?.connect_mode === "number" ? data.connect_mode : Number(data?.connect_mode ?? 1) || 1,
    roam_enable: roamOn ? 1 : 0,
    enable: dataOn ? 1 : 0,
  });
  const readBack = async (check: (d: ModemData) => boolean) => {
    for (let i = 0; i < 3; i++) {
      if (i > 0) await sleep(POLL_MS);
      checkAlive();
      const d = await apiFetch<ModemData>(DATA);
      if (check(d)) {
        await md.mutate(d, { revalidate: false });
        return true;
      }
    }
    return false;
  };

  const dataOp = useWriteOp({
    tier: ask?.kind === "data" ? ask.tier : 2,
    steps: [
      {
        label: t("mobilenet.mobileData", "Mobile Data"),
        run: () => {
          const a = askRef.current;
          const on = a?.kind === "data" && a.next;
          const body: ModemDataSetBody = { ...baseBody(), enable: on ? 1 : 0 };
          if (on) body.connect_mode = 1;
          else body.connect_status = "disconnected";
          return apiFetch(DATA, { method: "PUT", body });
        },
      },
    ],
    verify: () => {
      const a = askRef.current;
      const on = a?.kind === "data" && a.next;
      return readBack((d) => flag(d.enable) === on);
    },
  });

  const roamOp = useWriteOp({
    tier: ask?.kind === "roam" ? ask.tier : 2,
    steps: [
      {
        label: t("mobilenet.roaming", "Roaming"),
        run: () => {
          const a = askRef.current;
          const on = a?.kind === "roam" && a.next;
          return apiFetch(DATA, { method: "PUT", body: { ...baseBody(), roam_enable: on ? 1 : 0 } satisfies ModemDataSetBody });
        },
      },
    ],
    verify: () => {
      const a = askRef.current;
      const on = a?.kind === "roam" && a.next;
      return readBack((d) => flag(d.roam_enable) === on);
    },
  });

  // ── airplane ──
  const airRecovery = t(
    "mobilenet.airRecovery",
    "Turn airplane mode off on this page. If the modem doesn't come back, reboot the device (Device control below, or hold the power button)."
  );
  const airOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("mobilenet.airplaneMode", "Airplane Mode"),
        run: async () => {
          const a = askRef.current;
          if (a?.kind === "air" && a.next) {
            return apiFetch(`/api/modem/airplane`, { method: "POST", body: { operate_mode: "LPM" } });
          }
          // AT+CFUN=1 waits 8 s on the device; retry once after 3 s (as before).
          try {
            return await apiFetch(`/api/modem/online`, { method: "POST", timeoutMs: 20000 });
          } catch {
            await sleep(3000);
            checkAlive();
            return apiFetch(`/api/modem/online`, { method: "POST", timeoutMs: 20000 });
          }
        },
      },
    ],
    verify: async () => {
      const a = askRef.current;
      const wantOn = a?.kind === "air" && a.next;
      for (let i = 0; i < 5; i++) {
        if (i > 0) await sleep(POLL_MS);
        checkAlive();
        const s = await apiFetch<ModemStatus>(STATUS);
        const on = s.operate_mode !== undefined && s.operate_mode !== "ONLINE";
        if (on === wantOn) {
          await ms.mutate(s, { revalidate: false });
          return true;
        }
      }
      return false;
    },
    waitDevice: { expectedSec: 30, recovery: airRecovery },
  });

  // ── carrier scan ──
  const [operators, setOperators] = useState<ModemScanOperator[] | null>(null);
  const scanOp = useWriteOp({
    tier: 2,
    steps: [
      { label: t("mobilenet.stepScanStart", "Start scan"), run: () => apiFetch(`/api/netinfo/scan`, { method: "POST" }) },
      {
        label: t("mobilenet.stepScanWait", "Wait for the scan"),
        run: async () => {
          for (let i = 0; i < SCAN_TRIES; i++) {
            await sleep(JOB_POLL_MS);
            checkAlive();
            const scan = (await apiFetch<NetInfo>(`/api/netinfo`)).scan;
            if (scan?.state === "done") {
              setOperators(
                scan.operators.map((o) => ({ m_mcc_mnc: o.plmn, m_oper_name: o.name, m_rat: o.rat, m_status: o.status }))
              );
              return;
            }
            if (scan?.state === "error") throw new Error(scan.error ?? t("mobilenet.scanFailed", "The scan failed"));
          }
          throw new Error(t("mobilenet.scanTimedOut", "Carrier scan timed out"));
        },
      },
    ],
  });

  /** Poll the guard until it settles. */
  async function waitGuard(done: (g: NetInfoGuard) => boolean): Promise<NetInfoGuard | null> {
    for (let i = 0; i < REGISTER_TRIES; i++) {
      await sleep(JOB_POLL_MS);
      checkAlive();
      const g = await apiFetch<NetInfoGuard>(`/api/modem/register/guard`);
      if (done(g)) return g;
    }
    return null;
  }

  // ── register to a carrier ──
  const [regMsg, setRegMsg] = useState<{ tone: Tone; text: string } | null>(null);
  const [regSent, setRegSent] = useState<ModemScanOperator | null>(null);
  const regRecovery = t(
    "mobilenet.regRecovery",
    "If it does not take, the device goes back to automatic selection by itself within a minute. You can also press Back to Automatic below."
  );
  const regOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("mobilenet.stepRegister", "Register"),
        run: () => {
          const a = askRef.current;
          const op = a?.kind === "register" ? a.op : {};
          return apiFetch(`/api/modem/register`, { method: "POST", body: { m_mcc_mnc: op.m_mcc_mnc, m_rat: op.m_rat } });
        },
      },
    ],
    verify: async () => {
      const a = askRef.current;
      const name = a?.kind === "register" ? a.op.m_oper_name ?? a.op.m_mcc_mnc ?? "" : "";
      const g = await waitGuard((g) => g.phase === "ok" || g.phase === "reverted");
      if (g?.phase === "ok") {
        setRegMsg({ tone: "ok", text: t("mobilenet.registeredTo", "Registered to {{name}}", { name }) });
        return true;
      }
      if (g?.phase === "reverted") {
        setRegMsg({
          tone: "bad",
          text: t("mobilenet.regReverted", "Could not register ({{why}}); back to automatic selection", { why: g.reason || "—" }),
        });
        return false;
      }
      setRegMsg({ tone: "warn", text: t("mobilenet.registrationTimedOut", "Registration timed out") });
      return false;
    },
    waitDevice: { expectedSec: 30, recovery: regRecovery },
  });

  // ── back to automatic selection ──
  const autoOp = useWriteOp({
    tier: 2,
    steps: [{ label: t("mobilenet.backToAuto", "Back to Automatic"), run: () => apiFetch(`/api/modem/netselect/auto`, { method: "POST" }) }],
    verify: async () => {
      const g = await waitGuard((g) => g.phase === "reverted" || g.phase === "ok");
      if (g?.phase === "reverted") {
        setRegMsg({ tone: "ok", text: g.reason && g.reason !== "手动恢复自动" ? g.reason : t("mobilenet.autoDone", "Back to automatic selection") });
        return true;
      }
      return false;
    },
  });

  // ── reboot ──
  const rebootOp = useWriteOp({
    tier: 3,
    steps: [{ label: t("mobilenet.reboot", "Reboot"), run: () => apiFetch(`/api/device/reboot`, { method: "POST" }) }],
    waitDevice: {
      expectedSec: 90,
      expectDown: true,
      recovery: t("mobilenet.rebootRecovery", "Wait another minute, then reconnect to the U60's Wi-Fi. If the screen stays dark, hold the power button to start it."),
    },
  });

  const radioBusy = dataOp.busy || roamOp.busy || airOp.busy || regOp.busy;
  const locked = !data || md.stale || radioBusy;
  const airLocked = !status || ms.stale || radioBusy;

  function open(a: Ask) {
    askRef.current = a;
    setAsk(a);
  }
  function close() {
    setAsk(null);
  }
  function go() {
    const a = askRef.current;
    if (!a) return;
    const op =
      a.kind === "data" ? dataOp : a.kind === "roam" ? roamOp : a.kind === "air" ? airOp : a.kind === "scan" ? scanOp : a.kind === "register" ? regOp : a.kind === "auto" ? autoOp : rebootOp;
    if (a.kind === "scan") setOperators(null);
    if (a.kind === "register") {
      setRegMsg(null);
      setRegSent(a.op);
    }
    setAsk(null);
    op.start();
    op.confirm();
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("mobilenet.loading", "Reading the mobile connection…");
  let reason: ReactNode = null;
  if (!data && !status && (md.error || ms.error)) {
    tone = "bad";
    state = t("mobilenet.unreadable", "Can't read the mobile connection");
    reason = (md.error ?? ms.error)?.message;
  } else if (airplaneOn) {
    tone = "bad";
    state = t("mobilenet.stAirplane", "Airplane mode on");
    reason = t("mobilenet.stAirplaneReason", "The cellular radio is off. Turn airplane mode off below.");
  } else if (data && !dataOn) {
    tone = "bad";
    state = t("mobilenet.stDataOff", "Mobile data off");
    reason = t("mobilenet.stDataOffReason", "Devices on the U60 have no internet. Turn mobile data on below.");
  } else if (data && ck === "connected") {
    tone = "ok";
    state = t("mobilenet.stConnected", "Connected");
    reason = [fams, roamOn ? t("mobilenet.stRoamingAllowed", "Data roaming is allowed.") : null].filter(Boolean).join(" · ") || null;
  } else if (data && ck === "connecting") {
    tone = "neutral";
    state = t("mobilenet.stConnecting", "Connecting…");
  } else if (data) {
    tone = "warn";
    state = t("mobilenet.stNotConnected", "Not connected");
    reason = connectStatus
      ? t("mobilenet.stNotConnectedReason", "The modem reports “{{s}}”. Check the signal and the APN.", { s: connectStatus })
      : t("mobilenet.stNoStatus", "The modem doesn't report a connection state.");
  }
  if ((data && md.stale) || (status && ms.stale)) tone = "stale";

  const connTone: Tone = ck === "connected" ? "ok" : ck === "connecting" || ck === "unknown" ? "neutral" : "bad";
  const connWord =
    ck === "connected"
      ? [t("mobilenet.stConnected", "Connected"), fams].filter(Boolean).join(" · ")
      : ck === "connecting"
        ? t("mobilenet.stConnecting", "Connecting…")
        : connectStatus;
  const remote = isRemoteAccess();
  const dataTier = (): 2 | 3 => (remote ? 3 : 2);

  const confirmInlineFor = (kinds: Ask["kind"][]) =>
    ask && kinds.includes(ask.kind) && (ask.kind === "scan" || ask.kind === "auto" || ((ask.kind === "data" || ask.kind === "roam") && ask.tier === 2)) ? (
      <ConfirmInline
        id={inline.id}
        open
        actionLabel={inlineAction(ask)}
        consequence={inlineConsequence(ask)}
        onCancel={close}
        onConfirm={go}
      />
    ) : null;

  function inlineAction(a: Ask): string {
    if (a.kind === "data") return a.next ? t("mobilenet.dataOnAction", "turn mobile data on") : t("mobilenet.dataOffAction", "turn mobile data off");
    if (a.kind === "roam") return a.next ? t("mobilenet.roamOnAction", "allow roaming") : t("mobilenet.roamOffAction", "turn roaming off");
    if (a.kind === "auto") return t("mobilenet.backToAuto", "Back to Automatic");
    return t("mobilenet.scanForCarriers", "Scan for Carriers");
  }
  function inlineConsequence(a: Ask): string {
    if (a.kind === "data")
      return a.next
        ? t("mobilenet.dataOnConsequence", "The U60 connects to the mobile network again.")
        : t("mobilenet.dataOffConsequence", "Devices on the U60 lose internet until mobile data is turned back on. Wi-Fi and this page keep working.");
    if (a.kind === "roam")
      return a.next
        ? t("mobilenet.roamOnConsequence", "The U60 may use partner networks abroad; your carrier may charge roaming fees.")
        : t("mobilenet.roamOffConsequence", "If the U60 is roaming right now, it loses its mobile connection.");
    if (a.kind === "auto")
      return t("mobilenet.autoConsequence", "The modem picks the network by itself again. Mobile data may drop for about half a minute while it re-registers.");
    return t("mobilenet.scanConsequence", "The modem searches every carrier nearby. It takes about two minutes, and mobile data is off while it searches.");
  }

  const regTarget = ask?.kind === "register" ? ask.op : null;
  const regName = regTarget ? regTarget.m_oper_name ?? regTarget.m_mcc_mnc ?? "—" : "";
  const airNext = ask?.kind === "air" ? ask.next : !airplaneOn;
  const dlgData = ask && (ask.kind === "data" || ask.kind === "roam") && ask.tier === 3 ? ask : null;

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("mobilenet.title", "Mobile Network")}</h1>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={
            (data && md.stale) || (status && ms.stale) ? (
              <Freshness stale lastOkAt={md.lastOkAt ?? ms.lastOkAt} />
            ) : undefined
          }
          actions={
            md.error || ms.error ? (
              <Button
                variant="secondary"
                size="sm"
                onPress={() => {
                  void md.mutate();
                  void ms.mutate();
                }}
              >
                <ArrowClockwise size={18} weight="bold" aria-hidden />
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── connection ── */}
        <section>
          <Group title={t("mobilenet.connection", "Connection")} stale={md.stale}>
            <Row
              icon={CellSignalFull}
              label={t("mobilenet.status", "Status")}
              value={data ? <StatusMark tone={connTone}>{connWord || "—"}</StatusMark> : <span className="nd-skel inline-block w-20" />}
            />
            <Row
              icon={Globe}
              label={t("mobilenet.mobileData", "Mobile Data")}
              sub={data ? (dataOn ? t("common.on", "On") : t("common.off", "Off")) : undefined}
              control={
                data ? (
                  <span {...(ask?.kind === "data" && ask.tier === 2 ? inline.triggerProps : {})}>
                    <Switch
                      label={t("mobilenet.mobileData", "Mobile Data")}
                      isSelected={dataOn}
                      isDisabled={locked}
                      onChange={(next) => open({ kind: "data", next, tier: dataTier() })}
                    />
                  </span>
                ) : (
                  <span className="nd-skel inline-block w-11" />
                )
              }
            />
            <Row
              icon={Globe}
              label={t("mobilenet.roaming", "Roaming")}
              sub={data ? (roamOn ? t("common.on", "On") : t("common.off", "Off")) : undefined}
              control={
                data ? (
                  <span {...(ask?.kind === "roam" && ask.tier === 2 ? inline.triggerProps : {})}>
                    <Switch
                      label={t("mobilenet.roaming", "Roaming")}
                      isSelected={roamOn}
                      isDisabled={locked}
                      onChange={(next) => open({ kind: "roam", next, tier: dataTier() })}
                    />
                  </span>
                ) : (
                  <span className="nd-skel inline-block w-11" />
                )
              }
            />
          </Group>
          {confirmInlineFor(["data", "roam"])}
          {data && md.stale && (
            <p className="nd-aux mt-1 px-1">
              <Freshness stale lastOkAt={md.lastOkAt} what={t("mobilenet.settingsWord", "Settings")} />
              {t("mobilenet.refreshToEdit", " — refresh before changing anything.")}
            </p>
          )}
          {!data && md.error && (
            <p role="alert" className="nd-aux mt-1 px-1 text-nd-badT">
              {t("mobilenet.dataErr", "Couldn't read mobile data settings: {{e}}", { e: md.error.message ?? "" })}
            </p>
          )}
          <div className="mt-2 grid gap-1 px-1">
            <OpResult op={dataOp} />
            <OpResult op={roamOp} />
          </div>
        </section>

        {/* ── airplane ── */}
        <section>
          <Group title={t("mobilenet.airplaneMode", "Airplane Mode")} stale={ms.stale}>
            <Row
              icon={Airplane}
              label={t("mobilenet.airplaneMode", "Airplane Mode")}
              sub={t("mobilenet.airplaneHint", "Note: If modem doesn't recover after disabling airplane mode, reboot via Device Control.")}
              control={
                status ? (
                  <Switch
                    label={t("mobilenet.airplaneMode", "Airplane Mode")}
                    isSelected={airplaneOn}
                    isDisabled={airLocked}
                    onChange={(next) => open({ kind: "air", next })}
                  />
                ) : (
                  <span className="nd-skel inline-block w-11" />
                )
              }
            />
          </Group>
          {!status && ms.error && (
            <p role="alert" className="nd-aux mt-1 px-1 text-nd-badT">
              {t("mobilenet.statusErr", "Couldn't read the radio state: {{e}}", { e: ms.error.message ?? "" })}
            </p>
          )}
          <div className="mt-2 grid gap-1 px-1">
            <OpResult op={airOp} />
          </div>
        </section>

        {/* ── manual carrier selection ── */}
        <section aria-labelledby="mn-scan">
          <GroupTitle id="mn-scan">{t("mobilenet.manualCarrierScan", "Manual Carrier Scan")}</GroupTitle>
          <div className="flex flex-wrap items-center gap-3">
            <span {...(ask?.kind === "scan" ? inline.triggerProps : {})}>
              <Button
                variant="secondary"
                onPress={() => (ask?.kind === "scan" ? close() : open({ kind: "scan" }))}
                isDisabled={scanOp.busy || regOp.busy}
                pending={scanOp.busy}
              >
                <MagnifyingGlass size={20} weight="bold" aria-hidden />
                {scanOp.busy ? t("mobilenet.scanning", "Scanning…") : t("mobilenet.scanForCarriers", "Scan for Carriers")}
              </Button>
            </span>
            {airplaneOn && <span className="nd-aux">{t("mobilenet.scanNeedsRadio", "Airplane mode is on: the scan will find nothing until it is turned off.")}</span>}
          </div>
          {confirmInlineFor(["scan"])}
          <div className="mt-2 grid gap-1 px-1">
            {scanOp.busy ? (
              <p className="nd-aux" role="status">
                {t("mobilenet.scanTakesTime", "About two minutes; mobile data is off while it searches…")}
              </p>
            ) : scanOp.phase === "accepted" ? null : (
              <OpResult op={scanOp} />
            )}
          </div>

          {operators !== null && (
            <div className="mt-3">
              {operators.length === 0 ? (
                <div className="nd-group">
                  <p className="nd-body p-4 text-nd-t2 lg:p-5" role="status">
                    {t("mobilenet.noCarriers", "No carriers found · check the antenna position and scan again.")}
                  </p>
                </div>
              ) : (
                <div className="nd-group">
                  <p className="sr-only" role="status">
                    {t("mobilenet.nFound", "{{n}} carriers found", { n: operators.length })}
                  </p>
                  {operators.map((op, i) => {
                    const name = op.m_oper_name ?? op.m_mcc_mnc ?? "—";
                    return (
                      <Row
                        key={`${op.m_mcc_mnc ?? ""}-${op.m_rat ?? ""}-${i}`}
                        label={op.m_oper_name ?? "—"}
                        sub={
                          <>
                            <span className="nd-mono">{op.m_mcc_mnc ?? "—"}</span>
                            {" · "}
                            {ratName(op.m_rat)}
                            {op.m_status === "2" && <> · {t("mobilenet.opCurrent", "current")}</>}
                            {op.m_status === "3" && <> · {t("mobilenet.opForbidden", "forbidden")}</>}
                          </>
                        }
                        control={
                          <Button
                            size="sm"
                            variant="secondary"
                            onPress={() => open({ kind: "register", op })}
                            isDisabled={regOp.busy || airOp.busy}
                            pending={regOp.busy && regSent === op}
                            aria-label={t("mobilenet.registerTo", "Register to {{name}}", { name })}
                          >
                            {t("mobilenet.select", "Select")}
                          </Button>
                        }
                      />
                    );
                  })}
                </div>
              )}
            </div>
          )}
          <div className="mt-3 nd-group">
            <Row
              label={t("mobilenet.backToAuto", "Back to Automatic")}
              sub={t("mobilenet.backToAutoSub", "Let the modem pick the network again")}
              control={
                <span {...(ask?.kind === "auto" ? inline.triggerProps : {})}>
                  <Button
                    size="sm"
                    variant="secondary"
                    onPress={() => (ask?.kind === "auto" ? close() : open({ kind: "auto" }))}
                    isDisabled={regOp.busy || autoOp.busy || scanOp.busy}
                    pending={autoOp.busy}
                  >
                    {t("mobilenet.backToAutoBtn", "Back to Auto")}
                  </Button>
                </span>
              }
            />
          </div>
          {confirmInlineFor(["auto"])}
          <div className="mt-2 grid gap-1 px-1">
            <OpResult op={autoOp} />
            <OpResult op={regOp} />
            {regMsg && !regOp.busy && (
              <p role={regMsg.tone === "ok" ? "status" : "alert"}>
                <StatusMark tone={regMsg.tone}>{regMsg.text}</StatusMark>
              </p>
            )}
          </div>
        </section>

        {/* ── device control ── */}
        <section>
          <Group title={t("mobilenet.deviceControl", "Device Control")}>
            <Row
              icon={Power}
              label={t("mobilenet.rebootRouter", "Reboot Router")}
              sub={t("mobilenet.rebootHint", "Use this if modem fails to recover from airplane mode.")}
              control={
                <Button variant="secondary" size="sm" onPress={() => open({ kind: "reboot" })} isDisabled={rebootOp.busy} pending={rebootOp.busy}>
                  {t("mobilenet.reboot", "Reboot")}
                </Button>
              }
            />
          </Group>
          <div className="mt-2 grid gap-1 px-1">
            <OpResult op={rebootOp} />
          </div>
        </section>
      </div>

      {/* ── tier-3 dialogs ── */}
      <ConfirmDialog
        open={!!dlgData}
        onOpenChange={(o) => !o && close()}
        title={
          !dlgData
            ? ""
            : dlgData.kind === "data"
              ? dlgData.next
                ? t("mobilenet.dataOnTitle", "Turn mobile data on?")
                : t("mobilenet.dataOffTitle", "Turn mobile data off?")
              : dlgData.next
                ? t("mobilenet.roamOnTitle", "Allow data roaming?")
                : t("mobilenet.roamOffTitle", "Turn data roaming off?")
        }
        what={dlgData ? inlineConsequence(dlgData) : ""}
        downtime={
          dlgData && !dlgData.next
            ? t("mobilenet.dataOffDowntime", "Until it is turned back on. Over Tailscale this page goes away with it.")
            : undefined
        }
        recovery={
          dlgData && !dlgData.next
            ? t("mobilenet.dataOffRecovery", "Someone on the U60's Wi-Fi opens this page and turns it back on, or uses the device's touchscreen.")
            : undefined
        }
        actionLabel={
          !dlgData
            ? ""
            : dlgData.kind === "data"
              ? dlgData.next
                ? t("mobilenet.dataOnBtn", "Turn mobile data on")
                : t("mobilenet.dataOffBtn", "Turn mobile data off")
              : dlgData.next
                ? t("mobilenet.roamOnBtn", "Allow roaming")
                : t("mobilenet.roamOffBtn", "Turn roaming off")
        }
        cutsUplink={!!dlgData && !dlgData.next}
        onConfirm={go}
      />
      <ConfirmDialog
        open={ask?.kind === "air"}
        onOpenChange={(o) => !o && close()}
        title={airNext ? t("mobilenet.airOnTitle", "Turn airplane mode on?") : t("mobilenet.airOffTitle", "Turn airplane mode off?")}
        what={
          airNext
            ? t("mobilenet.confirmAirplane", "Enable airplane mode? This will turn off the cellular radio.")
            : t("mobilenet.airOffWhat", "The cellular radio comes back on and the modem registers to the network again.")
        }
        downtime={
          airNext
            ? t("mobilenet.airOnDowntime", "No mobile connection until airplane mode is turned off.")
            : t("mobilenet.airOffDowntime", "About 30 seconds until the modem is back on the network.")
        }
        recovery={airRecovery}
        actionLabel={airNext ? t("mobilenet.airOnAction", "Turn airplane mode on") : t("mobilenet.airOffAction", "Turn airplane mode off")}
        cutsUplink
        onConfirm={go}
      />
      <ConfirmDialog
        open={ask?.kind === "register"}
        onOpenChange={(o) => !o && close()}
        title={t("mobilenet.regTitle", "Register to {{name}}?", { name: regName })}
        what={t("mobilenet.regWhat", "The modem leaves automatic network selection and registers only to {{name}} ({{plmn}}).", {
          name: regName,
          plmn: regTarget?.m_mcc_mnc ?? "—",
        })}
        downtime={t("mobilenet.regDowntime", "The mobile connection drops for about 30 seconds while the modem registers. If it is refused, it stays off.")}
        recovery={regRecovery}
        actionLabel={t("mobilenet.regAction", "Register")}
        cutsUplink
        onConfirm={go}
      />
      <ConfirmDialog
        open={ask?.kind === "reboot"}
        onOpenChange={(o) => !o && close()}
        title={t("mobilenet.rebootTitle", "Reboot the U60?")}
        what={t("mobilenet.confirmReboot", "Reboot the router now?")}
        downtime={t("mobilenet.rebootDowntime", "About 90 seconds: Wi-Fi, the mobile connection and this page all go away.")}
        recovery={t("mobilenet.rebootRecovery", "Wait another minute, then reconnect to the U60's Wi-Fi. If the screen stays dark, hold the power button to start it.")}
        actionLabel={t("mobilenet.reboot", "Reboot")}
        cutsUplink
        onConfirm={go}
      />
    </>
  );
}
