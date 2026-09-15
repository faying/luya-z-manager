"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { Skull, ChevronUp, ChevronDown } from "lucide-react";

interface ProcessEntry {
  pid: number;
  name: string;
  rss_kb: number;
  vsz_kb?: number;
  cpu_pct: number;
  state?: string;
  is_bloat?: boolean;
}

// /api/system/top returns a summary object with the process list under `processes`.
interface TopData {
  processes: ProcessEntry[];
  total_count: number;
  bloat_count: number;
  bloat_cpu_pct: number;
  bloat_rss_kb: number;
}

interface KillResult {
  killed: number[];
}

type SortKey = "name" | "pid" | "cpu_pct" | "rss_kb";
type SortDir = "asc" | "desc";

function fmtKB(kb: number): string {
  if (!Number.isFinite(kb)) return "—";
  if (kb < 1024) return `${kb} KB`;
  return `${(kb / 1024).toFixed(1)} MB`;
}

export default function ProcessesPage() {
  const { t } = useTranslation();
  const { data, error: loadError, mutate } = useApi<TopData>("/api/system/top", {
    refreshInterval: 3000,
  });
  const procs = data?.processes;
  const [sortKey, setSortKey] = useState<SortKey>("cpu_pct");
  const [sortDir, setSortDir] = useState<SortDir>("desc");
  const [killing, setKilling] = useState<Set<number>>(new Set());
  const [killingAll, setKillingAll] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [successMsg, setSuccessMsg] = useState<string | null>(null);

  const handleSort = (key: SortKey) => {
    if (sortKey === key) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortKey(key);
      setSortDir("desc");
    }
  };

  const sorted = [...(procs ?? [])].sort((a, b) => {
    const av = a[sortKey];
    const bv = b[sortKey];
    if (typeof av === "string" && typeof bv === "string") {
      return sortDir === "asc" ? av.localeCompare(bv) : bv.localeCompare(av);
    }
    const an = av as number;
    const bn = bv as number;
    return sortDir === "asc" ? an - bn : bn - an;
  });

  const killProcess = async (pid: number) => {
    if (!window.confirm(t("procs.confirmKill", "Kill process PID {{pid}}?", { pid }))) return;
    setKilling((s) => new Set(s).add(pid));
    setActionError(null);
    setSuccessMsg(null);
    try {
      const result = await apiFetch<KillResult>("/api/system/kill-bloat", {
        method: "POST",
        body: { pids: [pid] },
      });
      setSuccessMsg(t("procs.killedPids", "Killed PID(s): {{pids}}", { pids: result.killed.join(", ") }));
      mutate();
    } catch (e) {
      setActionError(e instanceof ApiError ? e.message : String(e));
    } finally {
      setKilling((s) => {
        const next = new Set(s);
        next.delete(pid);
        return next;
      });
    }
  };

  const killAllBloat = async () => {
    const bloatCount = (procs ?? []).filter((p) => p.is_bloat).length;
    if (bloatCount === 0) return;
    if (!window.confirm(t("procs.confirmKillAll", "Kill all {{count}} bloat process(es)?", { count: bloatCount }))) return;
    setKillingAll(true);
    setActionError(null);
    setSuccessMsg(null);
    try {
      const result = await apiFetch<KillResult>("/api/system/kill-bloat", {
        method: "POST",
        body: { all: true },
      });
      setSuccessMsg(t("procs.killedCount", "Killed {{count}} process(es): {{pids}}", { count: result.killed.length, pids: result.killed.join(", ") }));
      mutate();
    } catch (e) {
      setActionError(e instanceof ApiError ? e.message : String(e));
    } finally {
      setKillingAll(false);
    }
  };

  const bloatCount = (procs ?? []).filter((p) => p.is_bloat).length;

  const SortIcon = ({ col }: { col: SortKey }) =>
    sortKey === col ? (
      sortDir === "asc" ? (
        <ChevronUp className="inline h-3 w-3" />
      ) : (
        <ChevronDown className="inline h-3 w-3" />
      )
    ) : null;

  const thCls = "px-3 py-2 text-left text-xs font-medium uppercase tracking-wider text-text-dim cursor-pointer select-none hover:text-text";

  return (
    <>
      <PageHeader
        title={t("procs.title", "Process Monitor")}
        description={t("procs.desc", "Live list of running processes. Refreshes every 3 seconds.")}
        actions={
          <Button
            variant="danger"
            size="sm"
            onClick={killAllBloat}
            loading={killingAll}
            disabled={killingAll || bloatCount === 0}
          >
            <Skull className="h-3.5 w-3.5" />
            {t("procs.killAllBloat", "Kill All Bloat ({{count}})", { count: bloatCount })}
          </Button>
        }
      />

      {loadError && <ErrorBanner message={(loadError as ApiError).message ?? String(loadError)} />}
      {actionError && <ErrorBanner message={actionError} />}
      {successMsg && (
        <div className="rounded-md border border-success/40 bg-success/10 px-3 py-2 text-sm text-success">
          {successMsg}
        </div>
      )}

      <SectionCard className="mt-4 overflow-x-auto p-0">
        <table className="w-full text-sm">
          <thead className="border-b border-border">
            <tr>
              <th className={thCls} onClick={() => handleSort("name")}>
                {t("procs.colName", "Name")} <SortIcon col="name" />
              </th>
              <th className={thCls} onClick={() => handleSort("pid")}>
                PID <SortIcon col="pid" />
              </th>
              <th className={thCls} onClick={() => handleSort("cpu_pct")}>
                CPU% <SortIcon col="cpu_pct" />
              </th>
              <th className={thCls} onClick={() => handleSort("rss_kb")}>
                RSS <SortIcon col="rss_kb" />
              </th>
              <th className="px-3 py-2 text-left text-xs font-medium uppercase tracking-wider text-text-dim">
                {t("procs.colBloat", "Bloat?")}
              </th>
              <th className="px-3 py-2" />
            </tr>
          </thead>
          <tbody className="divide-y divide-border/50">
            {sorted.length === 0 && (
              <tr>
                <td colSpan={6} className="px-3 py-6 text-center text-sm text-text-dim">
                  {procs === undefined ? t("procs.loading", "Loading…") : t("procs.empty", "No processes found")}
                </td>
              </tr>
            )}
            {sorted.map((p) => (
              <tr key={p.pid} className={p.is_bloat ? "bg-warning/5" : undefined}>
                <td className="px-3 py-2 font-mono font-medium">{p.name}</td>
                <td className="px-3 py-2 font-mono text-text-dim">{p.pid}</td>
                <td className="px-3 py-2 font-mono">{(Number.isFinite(p.cpu_pct) ? p.cpu_pct : 0).toFixed(1)}%</td>
                <td className="px-3 py-2 font-mono">{fmtKB(p.rss_kb)}</td>
                <td className="px-3 py-2">
                  {p.is_bloat && (
                    <span className="rounded-full bg-warning/20 px-2 py-0.5 text-xs text-warning font-medium">
                      {t("procs.bloatBadge", "bloat")}
                    </span>
                  )}
                </td>
                <td className="px-3 py-2 text-right">
                  {p.is_bloat && (
                    <Button
                      variant="danger"
                      size="sm"
                      onClick={() => killProcess(p.pid)}
                      loading={killing.has(p.pid)}
                      disabled={killing.has(p.pid) || killingAll}
                    >
                      {t("procs.kill", "Kill")}
                    </Button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </SectionCard>
    </>
  );
}
