"use client";

import { useState, useRef } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";

interface ProgressData {
  progress?: number | string;
  [key: string]: unknown;
}

export default function SignalDetectPage() {
  const { t } = useTranslation();
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState(0);
  const [results, setResults] = useState<Record<string, string>[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsErr, setMsgIsErr] = useState(false);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  function showMsg(text: string, isErr: boolean) {
    setMsg(text);
    setMsgIsErr(isErr);
  }

  function stopPolling() {
    if (pollRef.current) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }

  async function fetchResults() {
    try {
      const data = await apiFetch<Record<string, unknown>>("/api/cell/signal-detect/results");
      if (data && typeof data === "object") {
        const rows = flattenResults(data);
        setResults(rows);
      }
    } catch { /* results may not be available */ }
  }

  function flattenResults(data: Record<string, unknown>): Record<string, string>[] {
    // Try common shapes
    const arr = data.cells ?? data.results ?? data.list;
    if (Array.isArray(arr)) {
      return arr.map((item) =>
        Object.fromEntries(Object.entries(item as Record<string, unknown>).map(([k, v]) => [k, String(v)]))
      );
    }
    // Flat object: single result row
    return [Object.fromEntries(Object.entries(data).map(([k, v]) => [k, String(v)]))];
  }

  function startPolling() {
    stopPolling();
    pollRef.current = setInterval(async () => {
      try {
        const data = await apiFetch<ProgressData>("/api/cell/signal-detect/progress");
        const p = parseFloat(String(data?.progress ?? "0"));
        setProgress(isNaN(p) ? 0 : p);
        if (p >= 100) {
          stopPolling();
          setRunning(false);
          showMsg(t("sigdetect.detectionComplete", "Detection complete"), false);
          await fetchResults();
        }
      } catch { /* continue polling */ }
    }, 2000);
  }

  async function startDetection() {
    setBusy(true);
    setMsg(null);
    setResults(null);
    setProgress(0);
    try {
      await apiFetch("/api/cell/signal-detect/start", { method: "POST" });
      setRunning(true);
      showMsg(t("sigdetect.detectionStarted", "Detection started"), false);
      startPolling();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function stopDetection() {
    stopPolling();
    setBusy(true);
    try {
      await apiFetch("/api/cell/signal-detect/stop", { method: "POST" });
      setRunning(false);
      showMsg(t("sigdetect.detectionStopped", "Detection stopped"), false);
      await fetchResults();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  const resultKeys = results && results.length > 0 ? Object.keys(results[0]) : [];

  return (
    <>
      <PageHeader title={t("sigdetect.title", "Signal Detection")} description={t("sigdetect.desc", "Scan and analyze cell signal quality.")} />

      {msg && (
        <div className="mb-3">
          {msgIsErr ? (
            <ErrorBanner message={msg} />
          ) : (
            <div className="rounded-md border border-success/40 bg-success/10 px-3 py-2 text-sm text-success">{msg}</div>
          )}
        </div>
      )}

      <SectionCard className="mb-4">
        <div className="flex items-center gap-3">
          {!running ? (
            <Button loading={busy} onClick={startDetection}>
              {t("sigdetect.startDetection", "Start Detection")}
            </Button>
          ) : (
            <Button variant="danger" loading={busy} onClick={stopDetection}>
              {t("sigdetect.stopDetection", "Stop Detection")}
            </Button>
          )}
          {running && (
            <span className="text-sm text-text-dim">
              {t("sigdetect.progress", "Progress:")} <span className="font-mono font-semibold text-text">{progress.toFixed(0)}%</span>
            </span>
          )}
        </div>

        {running && (
          <div className="mt-4">
            <div className="h-2 w-full overflow-hidden rounded-full bg-bg-elevated">
              <div
                className="h-2 rounded-full bg-accent transition-all duration-500"
                style={{ width: `${Math.min(100, progress)}%` }}
              />
            </div>
          </div>
        )}
      </SectionCard>

      {results && (
        <SectionCard title={t("sigdetect.detectionResults", "Detection Results")}>
          {results.length === 0 ? (
            <p className="text-sm text-text-dim">{t("sigdetect.noResults", "No results available.")}</p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-xs">
                <thead>
                  <tr className="border-b border-border text-left text-text-dim">
                    {resultKeys.map((k) => (
                      <th key={k} className="pb-2 pr-3 font-medium">{k}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {results.map((row, i) => (
                    <tr key={i} className="border-b border-border/50 last:border-0 hover:bg-bg-elevated/50">
                      {resultKeys.map((k) => (
                        <td key={k} className="py-1.5 pr-3 font-mono">{row[k] ?? "—"}</td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </SectionCard>
      )}
    </>
  );
}
