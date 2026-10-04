"use client";
// A small line chart for "last 5 minutes" (audit C): our own SVG like
// TrendChart, up to two lines, x = time across the window, a fixed or
// auto-scaled y range. Lines use the accent mark (and t2 for the second).
import type { ReactNode } from "react";

const W = 600;
const H = 120;
const PAD_L = 0; // the labels sit in HTML to the left of the SVG
const PAD_R = 8;
const PAD_Y = 8;

export interface MiniSeries {
  values: (number | null)[];
  /** "accent" (first line) or "muted" (second). */
  tone: "accent" | "muted";
}

export function MiniChart({
  title,
  now,
  label,
  times,
  series,
  min = 0,
  max,
  windowMs,
  stale = false,
  empty,
  foot,
}: {
  title: ReactNode;
  /** The current value(s), right of the title. */
  now: ReactNode;
  /** Screen-reader description of the whole chart. */
  label: string;
  times: number[];
  series: MiniSeries[];
  min?: number;
  /** Fixed top; when absent the top is the largest value (at least `min + 1`). */
  max?: number;
  windowMs: number;
  stale?: boolean;
  empty: string;
  foot: [string, string];
}) {
  const vals = series.flatMap((s) => s.values).filter((v): v is number => v != null);
  const top = max ?? Math.max(min + 1, ...vals) * 1.1;
  const end = times[times.length - 1] ?? 0;
  const x = (t: number) => PAD_L + (1 - (end - t) / windowMs) * (W - PAD_L - PAD_R);
  const y = (v: number) => PAD_Y + ((top - Math.max(min, Math.min(top, v))) / (top - min)) * (H - PAD_Y * 2);
  const line = (s: MiniSeries) =>
    s.values
      .map((v, i) => (v == null ? null : `${x(times[i]).toFixed(1)},${y(v).toFixed(1)}`))
      .filter(Boolean)
      .join(" ");
  const ticks = [min, (min + top) / 2, top];
  return (
    <section className={`rounded-nd-card bg-nd-card shadow-[inset_0_0_0_1px_var(--nd-hair)] p-3 lg:p-4${stale ? " nd-stale" : ""}`}>
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <h2 className="text-[15px] font-semibold text-nd-t1">{title}</h2>
        <span className="text-[14px] tabular-nums text-nd-t2">{now}</span>
      </div>
      {vals.length < 2 ? (
        <div className="flex h-[120px] items-center justify-center text-[14px] text-nd-t2">{empty}</div>
      ) : (
        // tick labels are HTML: the SVG stretches to the card (preserveAspectRatio
        // none), which would squash any text drawn inside it on a phone
        <div className="relative h-[120px]">
          {ticks.map((v) => (
            <span
              key={v}
              className="absolute left-0 w-[34px] text-right text-[11px] leading-none tabular-nums text-nd-t3"
              style={{ top: `calc(${(y(v) / H) * 100}% - 5px)` }}
              aria-hidden
            >
              {v >= 10 ? Math.round(v) : Math.round(v * 10) / 10}
            </span>
          ))}
          <svg viewBox={`0 0 ${W} ${H}`} className="absolute inset-y-0 left-[40px] right-0 h-full w-[calc(100%-40px)]" preserveAspectRatio="none" role="img" aria-label={label}>
            {ticks.map((v) => (
              <line key={v} x1={PAD_L} x2={W - PAD_R} y1={y(v)} y2={y(v)} stroke="var(--nd-hair)" strokeWidth={1} vectorEffect="non-scaling-stroke" />
            ))}
            {series.map((s, i) => (
              <polyline
                key={i}
                points={line(s)}
                fill="none"
                stroke={stale ? "var(--nd-t3)" : s.tone === "accent" ? "var(--nd-accMark)" : "var(--nd-t2)"}
                strokeWidth={2}
                strokeDasharray={s.tone === "muted" ? "4 3" : undefined}
                strokeLinejoin="round"
                strokeLinecap="round"
                vectorEffect="non-scaling-stroke"
              />
            ))}
          </svg>
        </div>
      )}
      <div className="mt-2 flex justify-between text-[12px] text-nd-t3">
        <span>{foot[0]}</span>
        <span>{foot[1]}</span>
      </div>
    </section>
  );
}
