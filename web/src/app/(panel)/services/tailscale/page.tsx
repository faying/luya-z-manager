"use client";
// Tailscale (read-only: the agent only has GETs here, and there is no
// "turn off" control today — don't add one without the tier-2/remote-3 rule).
//
//   status block (state · reason · freshness · retry)
//   login link when tailscaled asks for authentication
//   ≥1024: this node + peers | mesh + exit node
//   daemon log (ConsoleBand)
//
// /api/services/tailscale answers 200 with `error` when `tailscale status`
// fails; tailscaleValid turns that into a failure (R9) so the last good data
// stays on screen, greyed, with the reason.
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { tailscaleValid } from "@/lib/api/freshness";
import type { TailscalePeer, TailscaleLog, TailscaleStatus } from "@/lib/api/schemas/services";
import { Button, ConsoleBand, Freshness, Group, GroupTitle, Help, Row, StatusBlock, StatusMark, type Tone } from "@/components/nd";

const REFRESH_INTERVAL = 5000;
const LOG_LINES = 200;
const PEER_CAP = 32; // services.rs sends at most 32 peers

type TFn = (key: string, def: string, opts?: Record<string, unknown>) => string;

export default function TailscalePage() {
  const { t } = useTranslation();
  const ts = useApi<TailscaleStatus>("/api/services/tailscale", {
    refreshInterval: REFRESH_INTERVAL,
    isValid: tailscaleValid,
  });
  const s = ts.data;
  const st = stateOf(s, !!ts.error, ts.stale, t);

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">Tailscale</h1>
        <Button variant="ghost" iconOnly onPress={() => ts.mutate()} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>
      <p className="nd-aux -mt-2 mb-4 px-1">
        {t("ts.desc", "Mesh VPN running on the router. Read-only — manage peers from the Tailscale admin console.")}
      </p>

      <div className="grid gap-6">
        <StatusBlock
          tone={st.tone}
          state={st.state}
          reason={
            ts.invalidReason ? (
              <span role="alert">{t("ts.errorReason", "Tailscale: {{e}}", { e: ts.invalidReason })}</span>
            ) : ts.error && !s ? (
              t("ts.loadFailed", "Couldn't read the status: {{msg}} · retry", { msg: ts.error.message ?? "" })
            ) : (
              st.reason
            )
          }
          meta={
            <>
              {s?.installed && s.self?.dns_name && !ts.stale && <span className="nd-mono">{s.self.dns_name}</span>}
              <Freshness stale={ts.stale} lastOkAt={ts.lastOkAt} />
            </>
          }
          actions={
            (ts.error || ts.stale) && (
              <Button variant="secondary" size="sm" onPress={() => ts.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            )
          }
        />

        {s?.auth_url && (
          <section aria-labelledby="ts-login">
            <GroupTitle id="ts-login">{t("ts.loginRequired", "Login required")}</GroupTitle>
            <div className="nd-group p-4 lg:p-5">
              <p className="nd-body">{t("ts.loginDesc", "Tailscale needs authentication. Open this URL on a logged-in device:")}</p>
              <a
                href={s.auth_url}
                target="_blank"
                rel="noreferrer"
                className="nd-mono mt-2 inline-flex min-h-11 items-center break-all text-nd-accT underline underline-offset-4"
              >
                {s.auth_url}
              </a>
            </div>
          </section>
        )}

        {s?.installed && (
          <div className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
            <div className="grid content-start gap-6">
              <NodeGroup s={s} stale={ts.stale} />
              <PeersGroup s={s} stale={ts.stale} />
            </div>
            <div className="grid content-start gap-6">
              <MeshGroup s={s} stale={ts.stale} />
            </div>
          </div>
        )}

        <LogBand />
      </div>
    </>
  );
}

function stateOf(
  s: TailscaleStatus | undefined,
  error: boolean,
  stale: boolean,
  t: TFn,
): { tone: Tone; state: string; reason?: string } {
  if (!s) {
    if (error) return { tone: "bad", state: t("home.tsUnreadable", "Can't read Tailscale status") };
    return { tone: "neutral", state: t("ts.stLoading", "Loading") };
  }
  let r: { tone: Tone; state: string; reason?: string };
  if (!s.installed)
    r = {
      tone: "neutral",
      state: t("ts.notInstalledTitle", "Tailscale isn't installed"),
      reason: t("ts.notInstalledDesc", "No tailscaled binary was found in /data/tailscale/. Run the toolkit installer's Tailscale module on the device, then refresh this page."),
    };
  else if (!s.running) r = { tone: "bad", state: t("ts.stStopped", "Stopped"), reason: t("ts.stoppedReason", "tailscaled isn't running on the device.") };
  else if (s.auth_url) r = { tone: "warn", state: t("ts.stAuthRequired", "Auth required"), reason: t("ts.authReason", "Open the login link below to connect this router.") };
  else if (s.backend_state === "Running")
    r = {
      tone: "ok",
      state: t("ts.stRunning", "Running"),
      reason: t("ts.peersSummary", "{{total}} total · {{online}} online", { total: s.peer_count ?? "—", online: s.peer_online ?? "—" }),
    };
  else if (s.backend_state === "NeedsLogin") r = { tone: "warn", state: t("ts.stNeedsLogin", "Needs login") };
  else if (s.backend_state === "Stopped") r = { tone: "bad", state: t("ts.stStopped", "Stopped") };
  else r = { tone: "neutral", state: s.backend_state ?? t("ts.stUnknown", "Unknown") };
  return stale ? { ...r, tone: "stale" } : r;
}

function NodeGroup({ s, stale }: { s: TailscaleStatus; stale: boolean }) {
  const { t } = useTranslation();
  const ips = s.self?.ips ?? [];
  const v4 = ips.find((ip) => !ip.includes(":"));
  const v6 = ips.find((ip) => ip.includes(":"));
  const text = (v: string | null | undefined) => (v == null || v === "" ? "—" : v);
  return (
    <section>
      <GroupTitle>{t("ts.thisNode", "This node")}</GroupTitle>
      <p className="nd-aux -mt-1 mb-3 px-1">{t("ts.thisNodeDesc", "How the router appears on your Tailscale network.")}</p>
      <Group stale={stale}>
        <Row label={t("ts.hostname", "Hostname")} value={text(s.self?.hostname)} mono />
        <Row label={t("ts.tsName", "Tailscale name")} value={text(s.self?.dns_name)} mono />
        <Row
          label={<>{t("ts.state", "State")} <Help label={t("ts.state", "State")} text={t("ts.helpState", "Connection state of the Tailscale daemon.")} /></>}
          value={text(s.backend_state)}
        />
        <Row label="IP (v4)" value={text(v4)} mono />
        <Row label="IP (v6)" value={text(v6)} mono />
        <Row
          label={<>{t("ts.derp", "DERP relay")} <Help label={t("ts.derp", "DERP relay")} text={t("ts.helpDerp", "The Tailscale relay region used when a direct peer-to-peer link isn't possible.")} /></>}
          value={text(s.self?.relay)}
          mono
        />
        <Row label={t("ts.version", "Version")} value={text(s.version)} mono />
        <Row
          label={<>{t("ts.exitAvail", "Exit-node available")} <Help label={t("ts.exitAvail", "Exit-node available")} text={t("ts.helpExit", "Whether other devices can route their internet through this router.")} /></>}
          value={s.self ? (s.self.exit_node_option ? t("common.yes", "Yes") : t("common.no", "No")) : "—"}
        />
      </Group>
    </section>
  );
}

function MeshGroup({ s, stale }: { s: TailscaleStatus; stale: boolean }) {
  const { t } = useTranslation();
  const ex = s.exit_node;
  return (
    <>
      <Group title={t("ts.mesh", "Mesh")} stale={stale}>
        <Row label={t("ts.peers", "Peers")} value={s.peer_count ?? "—"} />
        <Row label={t("ts.online", "Online")} value={s.peer_online ?? "—"} />
      </Group>
      <Group title={t("ts.exitInUse", "Exit node in use")} stale={stale}>
        {ex ? (
          <Row
            label={ex.hostname || "—"}
            sub={ex.ips?.[0] ? <span className="nd-mono">{ex.ips[0]}</span> : undefined}
            value={
              <StatusMark tone={ex.online ? "ok" : "neutral"}>
                {ex.online ? t("services.online", "Online") : t("services.offline", "Offline")}
              </StatusMark>
            }
          />
        ) : (
          <Row label={t("ts.noExit", "No exit node selected.")} />
        )}
      </Group>
    </>
  );
}

function PeersGroup({ s, stale }: { s: TailscaleStatus; stale: boolean }) {
  const { t } = useTranslation();
  const peers = s.peers ?? [];
  const capped = peers.length >= PEER_CAP && (s.peer_count ?? 0) > PEER_CAP;
  return (
    <section>
      <GroupTitle>{t("ts.peersTitle", "Peers")}</GroupTitle>
      <p className="nd-aux -mt-1 mb-3 px-1">
        {t("ts.peersSummary", "{{total}} total · {{online}} online", { total: s.peer_count ?? "—", online: s.peer_online ?? "—" })}
        {capped && <> · {t("ts.peersCapped", "showing the first {{n}}", { n: PEER_CAP })}</>}
      </p>
      <Group stale={stale}>
        {peers.length === 0 ? (
          <Row label={t("ts.noPeers", "No other devices on this tailnet yet")} sub={t("ts.noPeersNext", "Sign in to Tailscale on another device and it will show up here.")} />
        ) : (
          peers.map((p, i) => <PeerRow key={p.id ?? `${p.hostname}-${i}`} p={p} />)
        )}
      </Group>
    </section>
  );
}

function PeerRow({ p }: { p: TailscalePeer }) {
  const { t } = useTranslation();
  const when = fmtRelative(p.last_handshake, t);
  const parts: ReactNode[] = [];
  if (p.ips?.[0]) parts.push(<span key="ip" className="nd-mono">{p.ips[0]}</span>);
  if (p.os) parts.push(<span key="os">{p.os}</span>);
  parts.push(
    <span key="bytes" className="tabular-nums">
      ↑{fmtBytes(p.tx_bytes)} ↓{fmtBytes(p.rx_bytes)}
    </span>,
  );
  if (when) parts.push(<span key="hs">{t("ts.handshake", "handshake {{when}}", { when })}</span>);
  return (
    <Row
      label={
        <>
          {p.hostname || "—"}
          {p.exit_node && <span className="nd-aux ms-2 font-medium text-nd-t2">{t("ts.exitNodeChip", "exit node")}</span>}
        </>
      }
      sub={parts.map((x, i) => (
        <span key={i}>
          {i > 0 && " · "}
          {x}
        </span>
      ))}
      value={
        <StatusMark tone={p.online ? "ok" : "neutral"}>
          {p.online ? t("services.online", "Online") : t("services.offline", "Offline")}
        </StatusMark>
      }
    />
  );
}

function LogBand() {
  const { t } = useTranslation();
  const [paused, setPaused] = useState(false);
  const log = useApi<TailscaleLog>(`/api/services/tailscale/log?lines=${LOG_LINES}`, { refreshInterval: paused ? 0 : 4000 });
  const lines = log.data?.lines ?? [];
  return (
    <ConsoleBand label={t("ts.daemonLog", "Daemon log")}>
      <div className="mb-3 flex flex-wrap items-center gap-2 font-[family-name:var(--nd-font)]">
        <span className="nd-console__title flex-1">
          {t("ts.daemonLog", "Daemon log")} <span className="nd-console__muted text-[13px] font-semibold">/data/tailscaled.log</span>
        </span>
        <button
          type="button"
          className="nd-btn nd-btn--sm bg-nd-console-card text-nd-console-t1"
          aria-pressed={paused}
          onClick={() => setPaused((p) => !p)}
        >
          {paused ? t("services.resume", "Resume") : t("services.pause", "Pause")}
        </button>
        <button
          type="button"
          className="nd-btn nd-btn--sm bg-nd-console-card text-nd-console-t1"
          aria-label={t("ts.refreshLog", "Refresh log")}
          onClick={() => log.mutate()}
        >
          {t("common.refresh", "Refresh")}
        </button>
      </div>
      {log.error && !log.data && (
        <p className="nd-console__muted mb-2" role="alert">
          {t("ts.logFailed", "Couldn't read the log: {{msg}}", { msg: log.error.message ?? "" })}
        </p>
      )}
      <pre tabIndex={0} className="max-h-[420px] overflow-auto whitespace-pre-wrap break-words">
        {lines.length ? lines.join("\n") : <span className="nd-console__muted">{t("services.noLogs", "No log lines yet.")}</span>}
      </pre>
    </ConsoleBand>
  );
}

/** Unknown counters show "—", never 0. */
function fmtBytes(n?: number | null): string {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function fmtRelative(iso: string | null | undefined, t: TFn): string | null {
  if (!iso || iso.startsWith("0001-01-01")) return null;
  const parsed = Date.parse(iso);
  if (Number.isNaN(parsed)) return iso;
  // tailscaled here sometimes stamps local time with a "Z" suffix (the
  // device clock is local time labelled UTC), so a timestamp can look up to
  // a zone's worth in the future: treat that as "just now".
  const diff = (Date.now() - parsed) / 1000;
  if (diff < 0) {
    if (diff > -12 * 3600) return t("services.justNow", "just now");
    return null;
  }
  if (diff < 60) return t("services.agoSec", "{{n}}s ago", { n: Math.floor(diff) });
  if (diff < 3600) return t("services.agoMin", "{{n}}m ago", { n: Math.floor(diff / 60) });
  if (diff < 86400) return t("services.agoHour", "{{n}}h ago", { n: Math.floor(diff / 3600) });
  return t("services.agoDay", "{{n}}d ago", { n: Math.floor(diff / 86400) });
}
