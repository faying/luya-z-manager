"use client";
// Process monitor (tool page, new design). Order: summary status → the
// processes that may be ended (Group rows with 44px End buttons) → the full
// list (ConsoleBand table; sort with a Segmented + direction button above it,
// so the band itself holds only text).
//
//   End one process   tier 3   POST /api/system/kill-bloat {pids:[pid]}
//   End all on list   tier 3   POST /api/system/kill-bloat {all:true}
// No readback (R6): procd may start a process again at once, so the reply's
// `killed` / `skipped` lists are the result, shown by name and PID.
//
// The agent only ends names on its list (system.rs BLOAT_DAEMONS). Since
// fb4aedc that list no longer holds any daemon from zte_topsw_daemon.conf
// (they are in SYNC_BARRIER_DAEMONS, which the agent never kills).
//
// /api/system/top returns the top 50 by CPU only; `bloat_count` counts all
// processes, and {all:true} scans all of /proc — so the button uses
// bloat_count, and the confirm lists the rows we have plus "N more".
import { useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, SortAscending, SortDescending, XCircle } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp } from "@/lib/api/writeOp";
import type { KillBloatResult, ProcessEntry, SystemTop } from "@/lib/api/schemas/system";
import {
  Button,
  ConfirmDialog,
  ConsoleBand,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  Segmented,
  StatusBlock,
  StatusMark,
  type Tone,
} from "@/components/nd";

type SortKey = "cpu_pct" | "rss_kb" | "name" | "pid";
type SortDir = "asc" | "desc";
type Target = { pid: number; name: string };

/** Mirror of system.rs BLOAT_DAEMONS (fb4aedc), shown in the "end all" confirm. */
const BLOAT_NAMES = [
  "zte_topsw_tr069",
  "zte_topsw_tr069_sub",
  "zte_mqtt_sdk_st",
  "zte_topsw_diag",
  "zte_topsw_samba",
  "zte_topsw_nfc",
  "zte_topsw_get_brand",
  "zte_topsw_jwxk_query",
  "zte-topsw-tunnel",
  "zte_dua",
];

function fmtKB(kb: number): string {
  if (!Number.isFinite(kb)) return "—";
  if (kb < 1024) return `${kb} KB`;
  return `${(kb / 1024).toFixed(1)} MB`;
}

const fmtTarget = (p: Target) => `${p.name} (${p.pid})`;

export default function ProcessesPage() {
  const { t } = useTranslation();
  const top = useApi<SystemTop>("/api/system/top", { refreshInterval: 3000 });
  const procs = top.data?.processes;

  const [sortKey, setSortKey] = useState<SortKey>("cpu_pct");
  const [sortDir, setSortDir] = useState<SortDir>("desc");
  const sorted = useMemo(() => {
    const list = [...(procs ?? [])];
    list.sort((a, b) => {
      const av = a[sortKey];
      const bv = b[sortKey];
      const c = typeof av === "string" && typeof bv === "string" ? av.localeCompare(bv) : (av as number) - (bv as number);
      return sortDir === "asc" ? c : -c;
    });
    return list;
  }, [procs, sortKey, sortDir]);

  const bloatRows = useMemo(() => (procs ?? []).filter((p) => p.is_bloat), [procs]);
  const bloatCount = top.data?.bloat_count ?? bloatRows.length;
  // cpu_pct is a delta since the previous call: all zero on the first sample.
  const cpuKnown = (procs ?? []).some((p) => p.cpu_pct > 0);

  // ── writes. The single target lives in a ref for `run` (the op snapshots
  // its config at start()); the dialogs show a frozen copy in state so the
  // 3 s poll doesn't change them while the user reads. ──
  const [dialog, setDialog] = useState<null | "one" | "all">(null);
  const oneRef = useRef<Target | null>(null);
  const [oneTarget, setOneTarget] = useState<Target | null>(null);
  const [allSnap, setAllSnap] = useState<{ rows: Target[]; more: number }>({ rows: [], more: 0 });
  const [oneResult, setOneResult] = useState<KillBloatResult | null>(null);
  const [allResult, setAllResult] = useState<KillBloatResult | null>(null);

  const killOne = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("procs.endOneStep", "Kill process"),
        run: async () => {
          const target = oneRef.current;
          if (!target) throw new Error("no process selected");
          setOneResult(null);
          const r = await apiFetch<KillBloatResult>("/api/system/kill-bloat", { method: "POST", body: { pids: [target.pid] } });
          setOneResult(r);
          void top.mutate();
          return r;
        },
      },
    ],
  });
  const killAll = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("procs.endAllStep", "Kill all listed processes"),
        run: async () => {
          setAllResult(null);
          const r = await apiFetch<KillBloatResult>("/api/system/kill-bloat", { method: "POST", body: { all: true } });
          setAllResult(r);
          void top.mutate();
          return r;
        },
      },
    ],
  });
  const busy = killOne.busy || killAll.busy;
  const locked = busy || !top.data || top.stale;

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("procs.reading", "Reading processes…");
  let reason: ReactNode = null;
  if (top.data) {
    tone = top.stale ? "stale" : "ok";
    state = t("procs.summary", "{{n}} processes running", { n: top.data.total_count });
    reason =
      bloatCount > 0
        ? t("procs.bloatSummary", "{{n}} of them can be killed: {{cpu}} CPU, {{mem}} memory.", {
            n: bloatCount,
            cpu: `${top.data.bloat_cpu_pct.toFixed(1)}%`,
            mem: fmtKB(top.data.bloat_rss_kb),
          })
        : t("procs.noBloat", "None of the processes on the agent's list is running.");
  } else if (top.error) {
    tone = "warn";
    state = t("procs.unread", "Couldn't read the process list");
    reason = top.error instanceof Error ? top.error.message : String(top.error);
  }

  const openOne = (p: ProcessEntry) => {
    oneRef.current = { pid: p.pid, name: p.name };
    setOneTarget(oneRef.current);
    setDialog("one");
  };
  const openAll = () => {
    const rows = bloatRows.map((p) => ({ pid: p.pid, name: p.name }));
    setAllSnap({ rows, more: Math.max(0, bloatCount - rows.length) });
    setDialog("all");
  };

  const sortOptions = [
    { id: "cpu_pct" as const, label: "CPU" },
    { id: "rss_kb" as const, label: t("procs.sortMemory", "Memory") },
    { id: "name" as const, label: t("procs.colName", "Name") },
    { id: "pid" as const, label: "PID" },
  ];

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">{t("procs.title", "Process Monitor")}</h1>
        <Button variant="ghost" iconOnly onPress={() => void top.mutate()} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>

      <div className="grid max-w-[960px] gap-6">
        <p className="nd-body -mt-2 text-nd-t2">{t("procs.desc", "Live list of running processes. Refreshes every 3 seconds.")}</p>

        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={top.data && top.stale ? <Freshness stale lastOkAt={top.lastOkAt} what={t("procs.listWord", "The list")} /> : undefined}
          actions={
            top.error ? (
              <Button variant="secondary" onPress={() => void top.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── processes that may be ended ── */}
        <section aria-labelledby="procs-bloat">
          <div className="flex flex-wrap items-end justify-between gap-2">
            <GroupTitle id="procs-bloat">{t("procs.bloatTitle", "Can be killed")}</GroupTitle>
            <Button variant="danger" onPress={openAll} isDisabled={locked || bloatCount === 0} pending={killAll.busy}>
              <XCircle size={20} weight="bold" aria-hidden />
              {t("procs.killAllBloat", "Kill All Bloat ({{count}})", { count: bloatCount })}
            </Button>
          </div>
          <p className="nd-aux mb-2 mt-1">
            {t("procs.bloatExplain", "Only processes on the agent's list can be ended here. Boot-critical firmware daemons are never on it. The system may start an ended process again.")}
          </p>
          {!top.data ? (
            top.error ? null : (
              <div className="nd-group p-4">
                <span className="nd-skel" />
              </div>
            )
          ) : bloatRows.length === 0 ? (
            <p className="nd-body text-nd-t2">
              {bloatCount > 0
                ? t("procs.bloatNotInTop", "{{n}} listed process(es) are running but not among the 50 busiest shown here. Use the button above to end them.", { n: bloatCount })
                : t("procs.bloatNone", "Nothing to kill right now.")}
            </p>
          ) : (
            <Group stale={top.stale}>
              {bloatRows.map((p) => (
                <Row
                  key={p.pid}
                  label={<span className="nd-mono">{p.name}</span>}
                  sub={
                    <span className="nd-mono">
                      PID {p.pid} · {cpuKnown ? `${p.cpu_pct.toFixed(1)}%` : "—"} · {fmtKB(p.rss_kb)}
                    </span>
                  }
                  control={
                    <Button
                      variant="secondary"
                      onPress={() => openOne(p)}
                      isDisabled={locked}
                      pending={killOne.busy && oneTarget?.pid === p.pid}
                      aria-label={t("procs.killAria", "Kill {{name}} ({{pid}})", { name: p.name, pid: p.pid })}
                    >
                      {t("procs.kill", "Kill")}
                    </Button>
                  }
                />
              ))}
            </Group>
          )}
          <div className="mt-2 grid gap-2 px-1">
            <KillOutcome op={killOne} result={oneResult} />
            <KillOutcome op={killAll} result={allResult} />
          </div>
        </section>

        {/* ── full list ── */}
        <section aria-labelledby="procs-all" className="grid gap-3">
          <GroupTitle id="procs-all">{t("procs.allTitle", "All processes (50 busiest)")}</GroupTitle>
          <div className="flex flex-wrap items-center gap-2">
            <Segmented label={t("procs.sortBy", "Sort by")} options={sortOptions} value={sortKey} onChange={(k) => { setSortKey(k); setSortDir(k === "name" || k === "pid" ? "asc" : "desc"); }} />
            <Button
              variant="ghost"
              iconOnly
              onPress={() => setSortDir((d) => (d === "asc" ? "desc" : "asc"))}
              aria-label={sortDir === "asc" ? t("procs.sortAsc", "Ascending; press for descending") : t("procs.sortDesc", "Descending; press for ascending")}
            >
              {sortDir === "asc" ? <SortAscending size={20} weight="bold" aria-hidden /> : <SortDescending size={20} weight="bold" aria-hidden />}
            </Button>
          </div>
          <ConsoleBand scrollable label={t("procs.allTitle", "All processes (50 busiest)")} className={`overflow-x-auto${top.stale ? " nd-stale" : ""}`}>
            {sorted.length === 0 ? (
              <p className="nd-console__muted">
                {procs === undefined
                  ? top.error
                    ? t("procs.unread", "Couldn't read the process list")
                    : t("procs.loading", "Loading…")
                  : t("procs.empty", "No processes found")}
              </p>
            ) : (
              <table className="w-full min-w-[520px] border-collapse text-left">
                <thead>
                  <tr className="nd-console__muted">
                    <th scope="col" className="py-1 pr-3 font-normal" aria-sort={sortKey === "name" ? (sortDir === "asc" ? "ascending" : "descending") : undefined}>
                      {t("procs.colName", "Name")}
                    </th>
                    <th scope="col" className="py-1 pr-3 text-right font-normal" aria-sort={sortKey === "pid" ? (sortDir === "asc" ? "ascending" : "descending") : undefined}>
                      PID
                    </th>
                    <th scope="col" className="py-1 pr-3 text-right font-normal" aria-sort={sortKey === "cpu_pct" ? (sortDir === "asc" ? "ascending" : "descending") : undefined}>
                      CPU%
                    </th>
                    <th scope="col" className="py-1 pr-3 text-right font-normal" aria-sort={sortKey === "rss_kb" ? (sortDir === "asc" ? "ascending" : "descending") : undefined}>
                      RSS
                    </th>
                    <th scope="col" className="py-1 pr-3 font-normal">
                      {t("procs.colState", "State")}
                    </th>
                    <th scope="col" className="py-1 font-normal">
                      {t("procs.colBloat", "Bloat?")}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {sorted.map((p) => (
                    <tr key={p.pid}>
                      <td className="py-0.5 pr-3">{p.name}</td>
                      <td className="py-0.5 pr-3 text-right">{p.pid}</td>
                      <td className="py-0.5 pr-3 text-right">{cpuKnown ? p.cpu_pct.toFixed(1) : "—"}</td>
                      <td className="py-0.5 pr-3 text-right">{fmtKB(p.rss_kb)}</td>
                      <td className="nd-console__muted py-0.5 pr-3">{stateLabel(p.state, t)}</td>
                      <td className="py-0.5">{p.is_bloat ? t("procs.bloatBadge", "bloat") : ""}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </ConsoleBand>
          {!cpuKnown && top.data && <p className="nd-aux">{t("procs.cpuFirst", "CPU % shows after the next refresh (it is measured between two readings).")}</p>}
        </section>
      </div>

      <ConfirmDialog
        open={dialog === "one"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("procs.confirmOneTitle", "Kill this process?")}
        what={
          <>
            <span className="block">{t("procs.confirmOneWhat", "Sends SIGKILL to:")}</span>
            <span className="nd-mono mt-1 block">{oneTarget ? fmtTarget(oneTarget) : "—"}</span>
          </>
        }
        downtime={t("procs.confirmDowntime", "Nothing goes offline; the process stops at once.")}
        recovery={t("procs.confirmRecovery", "The system may start it again by itself; otherwise restarting the device brings it back.")}
        actionLabel={t("procs.kill", "Kill")}
        danger
        onConfirm={() => {
          setDialog(null);
          killOne.start();
          killOne.confirm();
        }}
      />
      <ConfirmDialog
        open={dialog === "all"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("procs.confirmAllTitle", "Kill all {{count}} listed processes?", { count: bloatCount })}
        what={
          <>
            <span className="block">{t("procs.confirmAllWhat", "Sends SIGKILL to every running process on the agent's list:")}</span>
            <ul className="nd-mono mt-1">
              {allSnap.rows.map((r) => (
                <li key={r.pid}>{fmtTarget(r)}</li>
              ))}
            </ul>
            {allSnap.more > 0 && (
              <span className="mt-1 block">
                {t("procs.confirmAllMore", "…and {{n}} more not among the 50 busiest shown here.", { n: allSnap.more })}
              </span>
            )}
            <span className="nd-aux mt-2 block">
              {t("procs.confirmAllList", "Names on the list: {{names}}", { names: BLOAT_NAMES.join(", ") })}
            </span>
          </>
        }
        downtime={t("procs.confirmDowntime", "Nothing goes offline; the process stops at once.")}
        recovery={t("procs.confirmRecovery", "The system may start it again by itself; otherwise restarting the device brings it back.")}
        actionLabel={t("procs.endAllAction", "Kill them")}
        danger
        onConfirm={() => {
          setDialog(null);
          killAll.start();
          killAll.confirm();
        }}
      />
    </>
  );
}

function stateLabel(s: string, t: (k: string, d: string) => string): string {
  switch (s) {
    case "running":
      return t("procs.stRunning", "running");
    case "sleeping":
      return t("procs.stSleeping", "sleeping");
    case "disk":
      return t("procs.stDisk", "disk wait");
    case "zombie":
      return t("procs.stZombie", "zombie");
    case "stopped":
      return t("procs.stStopped", "stopped");
    default:
      return s || "—";
  }
}

/** The reply's lists (the old page showed "[object Object]"); OpResult for the rest. */
function KillOutcome({ op, result }: { op: ReturnType<typeof useWriteOp>; result: KillBloatResult | null }) {
  const { t } = useTranslation();
  if (op.phase !== "accepted" && op.phase !== "applied") return <OpResult op={op} />;
  if (!result) return null;
  const killed = result.killed ?? [];
  const skipped = result.skipped ?? [];
  return (
    <div role="status" className="grid gap-1">
      {killed.length > 0 ? (
        <StatusMark tone="ok">
          {t("procs.killedList", "Killed {{count}}: {{list}}", { count: killed.length, list: killed.map(fmtTarget).join(", ") })}
          {result.freed_rss_kb > 0 && ` · ${t("procs.freed", "freed {{mem}}", { mem: fmtKB(result.freed_rss_kb) })}`}
        </StatusMark>
      ) : (
        <StatusMark tone="warn">{t("procs.killedNone", "Nothing was killed.")}</StatusMark>
      )}
      {skipped.length > 0 && (
        <StatusMark tone="warn">
          {t("procs.skippedList", "Not killed (not on the agent's list or already gone): {{list}}", { list: skipped.map(fmtTarget).join(", ") })}
        </StatusMark>
      )}
    </div>
  );
}
