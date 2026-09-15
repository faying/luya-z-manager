"use client";

import { useState, useEffect, useRef } from "react";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, StatCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { Wifi, StopCircle, Play } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

interface SpeedServer {
  id: number;
  name: string;
  sponsor: string;
  country: string;
  host: string;
  url: string;
}

interface SpeedProgress {
  phase: "idle" | "latency" | "download" | "upload" | "complete" | "cancelled" | "error";
  progress: number;
  ping_ms?: number;
  jitter_ms?: number;
  download_mbps?: number;
  upload_mbps?: number;
  error?: string;
  live_speed_mbps?: number;
}

const phaseLabels = (t: TFunction): Record<string, string> => ({
  idle: t("speedtest.phaseIdle", "Idle"),
  latency: t("speedtest.phaseLatency", "Measuring latency…"),
  download: t("speedtest.phaseDownload", "Testing download…"),
  upload: t("speedtest.phaseUpload", "Testing upload…"),
  complete: t("speedtest.phaseComplete", "Complete"),
  cancelled: t("speedtest.phaseCancelled", "Cancelled"),
  error: t("speedtest.phaseError", "Error"),
});

const DONE_PHASES = new Set(["complete", "cancelled", "error"]);

export default function SpeedTestPage() {
  const { t } = useTranslation();
  const [servers, setServers] = useState<SpeedServer[]>([]);
  const [selectedServerId, setSelectedServerId] = useState<number | "auto">("auto");
  const [loadingServers, setLoadingServers] = useState(true);
  const [serverError, setServerError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [progress, setProgress] = useState<SpeedProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    apiFetch<SpeedServer[]>("/api/speedtest/servers")
      .then((data) => setServers(data ?? []))
      .catch((e) => setServerError(e instanceof ApiError ? e.message : String(e)))
      .finally(() => setLoadingServers(false));
  }, []);

  const stopPolling = () => {
    if (pollRef.current) {
      clearTimeout(pollRef.current);
      pollRef.current = null;
    }
  };

  const poll = async () => {
    try {
      const p = await apiFetch<SpeedProgress>("/api/speedtest/progress");
      setProgress(p);
      if (DONE_PHASES.has(p.phase)) {
        setRunning(false);
        setStopping(false);
        stopPolling();
      } else {
        pollRef.current = setTimeout(poll, 1000);
      }
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
      setRunning(false);
      setStopping(false);
      stopPolling();
    }
  };

  useEffect(() => {
    return () => stopPolling();
  }, []);

  const handleStart = async () => {
    setError(null);
    setProgress(null);
    setRunning(true);
    try {
      await apiFetch("/api/speedtest/start", {
        method: "POST",
        body: selectedServerId === "auto" ? {} : { server_id: selectedServerId },
      });
      pollRef.current = setTimeout(poll, 500);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
      setRunning(false);
    }
  };

  const handleStop = async () => {
    setStopping(true);
    try {
      await apiFetch("/api/speedtest/stop", { method: "POST", body: {} });
    } catch {
      // ignore
    }
  };

  const phase = progress?.phase ?? "idle";
  const isComplete = phase === "complete";
  const isError = phase === "error";
  // Agent reports progress as an integer percent (0–100), not a 0–1 fraction.
  const pct = Math.max(0, Math.min(100, Math.round(progress?.progress ?? 0)));

  return (
    <>
      <PageHeader
        title={t("speedtest.title", "Speed Test")}
        description={t("speedtest.desc", "Measure your WAN connection speed.")}
      />

      {error && <ErrorBanner message={error} />}

      <SectionCard title={t("speedtest.configuration", "Configuration")} className="mb-4">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-end">
          <div className="flex-1">
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("speedtest.server", "Server")}</label>
            <select
              className="h-9 w-full rounded-md border border-border bg-bg-input px-3 text-sm outline-none transition focus:border-accent"
              value={selectedServerId}
              onChange={(e) =>
                setSelectedServerId(e.target.value === "auto" ? "auto" : Number(e.target.value))
              }
              disabled={running || loadingServers}
            >
              <option value="auto">{t("speedtest.autoBestServer", "Auto (best server)")}</option>
              {servers.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.sponsor} — {s.name}, {s.country}
                </option>
              ))}
            </select>
            {serverError && <p className="mt-1 text-xs text-error">{serverError}</p>}
          </div>
          {running ? (
            <Button variant="danger" onClick={handleStop} loading={stopping} disabled={stopping}>
              <StopCircle className="h-4 w-4" />
              {t("speedtest.stop", "Stop")}
            </Button>
          ) : (
            <Button onClick={handleStart} disabled={loadingServers}>
              <Play className="h-4 w-4" />
              {t("speedtest.start", "Start")}
            </Button>
          )}
        </div>
      </SectionCard>

      {progress && (
        <SectionCard title={t("speedtest.progress", "Progress")} className="mb-4">
          <div className="mb-2 flex items-center justify-between text-sm">
            <span className="flex items-center gap-2 font-medium">
              <Wifi className="h-4 w-4 text-accent" />
              {phaseLabels(t)[phase] ?? phase}
            </span>
            <span className="text-text-dim">{pct}%</span>
          </div>
          <div className="h-2 w-full overflow-hidden rounded-full bg-bg-elevated">
            <div
              className={`h-full rounded-full transition-all duration-500 ${
                isError ? "bg-error" : isComplete ? "bg-success" : "bg-accent"
              }`}
              style={{ width: `${pct}%` }}
            />
          </div>
          {progress.live_speed_mbps != null && !DONE_PHASES.has(phase) && (
            <p className="mt-2 text-sm text-text-dim">
              {t("speedtest.live", "Live")}: <span className="font-mono text-text">{progress.live_speed_mbps.toFixed(1)} Mbps</span>
            </p>
          )}
          {isError && progress.error && (
            <p className="mt-2 text-sm text-error">{progress.error}</p>
          )}
        </SectionCard>
      )}

      {isComplete && progress && (
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
          <StatCard
            label={t("speedtest.ping", "Ping")}
            value={progress.ping_ms?.toFixed(1) ?? "—"}
            unit="ms"
          />
          <StatCard
            label={t("speedtest.jitter", "Jitter")}
            value={progress.jitter_ms?.toFixed(1) ?? "—"}
            unit="ms"
          />
          <StatCard
            label={t("speedtest.download", "Download")}
            value={progress.download_mbps?.toFixed(2) ?? "—"}
            unit="Mbps"
          />
          <StatCard
            label={t("speedtest.upload", "Upload")}
            value={progress.upload_mbps?.toFixed(2) ?? "—"}
            unit="Mbps"
          />
        </div>
      )}
    </>
  );
}
