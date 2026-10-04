"use client";
// Last 5 minutes (audit C, 10-04): mobile download/upload, CPU, memory and
// battery as small line charts, like the touch screen's 系统 tab. The agent
// keeps no history, so this page samples every 3 s while it is open
// (lib/liveTrend.ts keeps the samples for the tab); the note says so.
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import type { CpuUsage, MemInfo, NetworkSpeed } from "@/lib/api/schemas/network";
import type { SysfsBattery } from "@/lib/api/schemas/device";
import { MiniChart } from "@/components/nd";
import { TREND_EVERY_MS, TREND_WINDOW_MS, pushSample, trendSamples, type TrendSample } from "@/lib/liveTrend";

const OPTS = { refreshInterval: TREND_EVERY_MS } as const;

const mbit = (bps: number | null | undefined) => (bps == null || !Number.isFinite(bps) ? null : (bps * 8) / 1_000_000);

export default function TrendsPage() {
  const { t } = useTranslation();
  const speed = useApi<NetworkSpeed>("/api/network/speed", OPTS);
  const cpu = useApi<CpuUsage>("/api/cpu", OPTS);
  const mem = useApi<MemInfo>("/api/memory", OPTS);
  const bat = useApi<SysfsBattery>("/api/battery", OPTS);
  const [samples, setSamples] = useState<TrendSample[]>(() => trendSamples());

  // one sample per tick from whatever each endpoint last answered (fresh only)
  const latest = useRef({ speed, cpu, mem, bat });
  latest.current = { speed, cpu, mem, bat };
  useEffect(() => {
    const tick = () => {
      const { speed: s, cpu: c, mem: m, bat: b } = latest.current;
      const fresh = <T,>(q: { data?: T; stale: boolean }) => (q.stale ? undefined : q.data);
      const sp = fresh(s);
      const bt = fresh(b);
      const watt = bt && bt.voltage_uv && bt.current_ua != null ? (bt.voltage_uv / 1e6) * (bt.current_ua / 1e6) : null;
      const sample: TrendSample = {
        t: Date.now(),
        down: mbit(sp?.rx_speed),
        up: mbit(sp?.tx_speed),
        cpu: fresh(c)?.overall ?? null,
        mem: fresh(m)?.usage_pct ?? null,
        bat: bt?.capacity ?? null,
        watt,
      };
      if (Object.values(sample).filter((v) => v != null).length > 1) setSamples(pushSample(sample));
    };
    const id = setInterval(tick, TREND_EVERY_MS);
    return () => clearInterval(id);
  }, []);

  const times = samples.map((s) => s.t);
  const last = samples[samples.length - 1];
  const empty = t("trends.collecting", "Collecting — the lines appear after a few seconds");
  const foot: [string, string] = [t("trends.fiveMinAgo", "5 min ago"), t("signal.now", "now")];
  const fmt = (v: number | null | undefined, digits = 1) => (v == null ? "—" : v >= 100 ? v.toFixed(0) : v.toFixed(digits));
  const span = samples.length > 1 ? Math.round((times[times.length - 1] - times[0]) / 60_000) : 0;

  return (
    <>
      <h1 className="nd-title mb-2 mt-2">{t("nav.trends", "Last 5 Minutes")}</h1>
      <p className="nd-aux mb-4 px-1">
        {span >= 5
          ? t("trends.noteFull", "Sampled every 3 s by this page.")
          : t("trends.note", "Sampled every 3 s while this page is open; the device keeps no history, so the lines start when you open it.")}
      </p>
      <div className="grid max-w-[1100px] gap-4 lg:grid-cols-2">
        <MiniChart
          title={t("trends.speed", "Mobile speed")}
          now={t("trends.speedNow", "↓ {{d}} ↑ {{u}} Mbps", { d: fmt(last?.down), u: fmt(last?.up) })}
          label={t("trends.speedAria", "Mobile download (solid) and upload (dashed), last 5 minutes")}
          times={times}
          series={[
            { values: samples.map((s) => s.down), tone: "accent" },
            { values: samples.map((s) => s.up), tone: "muted" },
          ]}
          windowMs={TREND_WINDOW_MS}
          stale={speed.stale}
          empty={empty}
          foot={foot}
        />
        <MiniChart
          title="CPU"
          now={`${fmt(last?.cpu, 0)}%`}
          label={t("trends.cpuAria", "CPU use, last 5 minutes")}
          times={times}
          series={[{ values: samples.map((s) => s.cpu), tone: "accent" }]}
          max={100}
          windowMs={TREND_WINDOW_MS}
          stale={cpu.stale}
          empty={empty}
          foot={foot}
        />
        <MiniChart
          title={t("trends.memory", "Memory")}
          now={`${fmt(last?.mem, 0)}%`}
          label={t("trends.memAria", "Memory in use, last 5 minutes")}
          times={times}
          series={[{ values: samples.map((s) => s.mem), tone: "accent" }]}
          max={100}
          windowMs={TREND_WINDOW_MS}
          stale={mem.stale}
          empty={empty}
          foot={foot}
        />
        <MiniChart
          title={t("trends.battery", "Battery")}
          now={
            last?.bat == null
              ? "—"
              : last.watt == null
                ? `${last.bat}%`
                : t("trends.batNow", "{{p}}% · {{w}} W", { p: last.bat, w: (last.watt >= 0 ? "+" : "−") + Math.abs(last.watt).toFixed(1) })
          }
          label={t("trends.batAria", "Battery level, last 5 minutes")}
          times={times}
          series={[{ values: samples.map((s) => s.bat), tone: "accent" }]}
          max={100}
          windowMs={TREND_WINDOW_MS}
          stale={bat.stale}
          empty={empty}
          foot={foot}
        />
      </div>
    </>
  );
}
