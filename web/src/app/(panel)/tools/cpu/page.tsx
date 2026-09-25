"use client";
// CPU and memory (tool page, new design). Reads /api/cpu and /api/memory
// every 5 s while the tab is visible (SWR pauses hidden tabs); the trend is
// kept in this page only, cleared when the tab is hidden or the page is left.
// Not /api/system/top: that walks every process.
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, GroupTitle, Row, StatusBlock } from "@/components/nd";
import { useApi } from "@/lib/hooks/useApi";
import type { CpuUsage, MemInfo } from "@/lib/api/schemas/network";
import type { ValidResult } from "@/lib/api/freshness";
import { pushPoint, sparkPoints, TREND_POINTS } from "@/lib/cpuTrend";

function cpuValid(d: CpuUsage | null | undefined): ValidResult {
  if (!d || typeof d.overall !== "number" || !Array.isArray(d.cores)) return { ok: false, reason: "no CPU usage" };
  return true;
}

function memValid(d: MemInfo | null | undefined): ValidResult {
  if (!d || typeof d.total_kb !== "number" || typeof d.used_kb !== "number") return { ok: false, reason: "no memory info" };
  return true;
}

const W = 320;
const H = 64;

export default function CpuPage() {
  const { t } = useTranslation();
  const [trend, setTrend] = useState<number[]>([]);
  const cpu = useApi<CpuUsage>("/api/cpu", {
    refreshInterval: 5000,
    isValid: cpuValid,
    onSuccess: (d) => {
      if (cpuValid(d) === true) setTrend((l) => pushPoint(l, d.overall));
    },
  });
  const mem = useApi<MemInfo>("/api/memory", { refreshInterval: 5000, isValid: memValid });

  useEffect(() => {
    const onVis = () => {
      if (document.visibilityState === "hidden") setTrend([]);
    };
    document.addEventListener("visibilitychange", onVis);
    return () => document.removeEventListener("visibilitychange", onVis);
  }, []);

  const c = cpu.data;
  const m = mem.data;
  const mib = (kb: number) => `${Math.round(kb / 1024)} MiB`;
  const cores = (Array.isArray(c?.per_core) ? c.per_core : null) ?? c?.cores.map((u, id) => ({ id, online: true, usage: u, freq_mhz: null, max_mhz: null })) ?? [];

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("cpu.title", "CPU & Memory")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">
        {t("cpu.desc", "Sampled every 5 seconds while this page is open; the trend covers the last 5 minutes.")}
      </p>
      <div className="grid max-w-[720px] gap-6">
        {!c && cpu.error ? (
          <StatusBlock
            tone="bad"
            state={t("cpu.failed", "Can't read CPU usage")}
            reason={cpu.error.message}
            actions={
              <Button variant="secondary" size="sm" onPress={() => cpu.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            }
          />
        ) : (
          <StatusBlock
            tone={cpu.stale ? "stale" : "neutral"}
            state={c ? t("cpu.overall", "CPU {{pct}}%", { pct: c.overall.toFixed(1) }) : t("cpu.loading", "Reading…")}
            reason={
              m
                ? t("cpu.memLine", "Memory {{used}} of {{total}} used ({{pct}}%)", {
                    used: mib(m.used_kb),
                    total: mib(m.total_kb),
                    pct: Math.round(m.usage_pct),
                  })
                : null
            }
          />
        )}

        <section aria-labelledby="cpu-trend">
          <GroupTitle id="cpu-trend">{t("cpu.trend", "Last 5 minutes")}</GroupTitle>
          <div className="nd-group p-3">
            <svg viewBox={`0 0 ${W} ${H}`} width="100%" height={H} role="img" aria-label={t("cpu.trend", "Last 5 minutes")}>
              <line x1="0" y1={H / 2} x2={W} y2={H / 2} stroke="currentColor" strokeOpacity="0.12" />
              <polyline
                points={sparkPoints(trend, W, H, TREND_POINTS)}
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                className="text-nd-acc"
              />
            </svg>
          </div>
        </section>

        <section aria-labelledby="cpu-cores">
          <GroupTitle id="cpu-cores">{t("cpu.cores", "Cores")}</GroupTitle>
          <div className={`nd-group${cpu.stale ? " nd-stale" : ""}`}>
            {cores.map((k) => (
              <Row
                key={k.id}
                label={`CPU ${k.id}`}
                mono
                value={
                  !k.online
                    ? t("cpu.offline", "Offline")
                    : [
                        typeof k.usage !== "number" ? "—" : `${k.usage.toFixed(1)}%`,
                        k.freq_mhz == null ? null : k.max_mhz ? `${k.freq_mhz} / ${k.max_mhz} MHz` : `${k.freq_mhz} MHz`,
                      ]
                        .filter(Boolean)
                        .join(" · ")
                }
              />
            ))}
          </div>
        </section>

        {m && (
          <section aria-labelledby="cpu-mem">
            <GroupTitle id="cpu-mem">{t("cpu.memory", "Memory")}</GroupTitle>
            <div className={`nd-group${mem.stale ? " nd-stale" : ""}`}>
              <Row label={t("cpu.memUsed", "Used")} value={mib(m.used_kb)} mono />
              <Row label={t("cpu.memAvailable", "Available")} value={mib(m.available_kb)} mono />
              <Row label={t("cpu.memCached", "Cache and buffers")} value={mib(m.cached_kb + m.buffers_kb)} mono />
              <Row label={t("cpu.memTotal", "Total")} value={mib(m.total_kb)} mono />
            </div>
          </section>
        )}
      </div>
    </>
  );
}
