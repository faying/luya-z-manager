"use client";
// RSRP over the last ~2 minutes (design doc §6, 15A): our own SVG, no chart
// library. Fixed −130…−70 dBm scale so the line doesn't jump when the range
// narrows; threshold bands use the status washes (good ≥ −100, fair ≥ −110,
// weak below), the line uses the non-text accent mark, labels are t3.
import { useTranslation } from "react-i18next";

const W = 600;
const H = 160;
const TOP = -70;
const BOTTOM = -130;
const PAD_L = 40;
const PAD_R = 8;
const PAD_Y = 8;

const y = (v: number) => {
  const c = Math.max(BOTTOM, Math.min(TOP, v));
  return PAD_Y + ((TOP - c) / (TOP - BOTTOM)) * (H - PAD_Y * 2);
};

export function TrendChart({ samples, minSamples = 30, stale = false }: { samples: number[]; minSamples?: number; stale?: boolean }) {
  const { t } = useTranslation();
  if (samples.length < minSamples) {
    return (
      <div className="flex h-40 items-center justify-center rounded-nd-card bg-nd-card shadow-[inset_0_0_0_1px_var(--nd-hair)] text-[14px] text-nd-t2">
        {t("signal.collectingMinute", "Collecting — the curve appears in about a minute")}
      </div>
    );
  }
  const n = samples.length;
  const x = (i: number) => PAD_L + (i / (n - 1)) * (W - PAD_L - PAD_R);
  const line = samples.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const lo = Math.min(...samples);
  const hi = Math.max(...samples);
  const bands: [number, number, string][] = [
    [TOP, -100, "var(--nd-wash)"],
    [-100, -110, "var(--nd-washW)"],
    [-110, BOTTOM, "var(--nd-washB)"],
  ];
  return (
    <div className={`rounded-nd-card bg-nd-card shadow-[inset_0_0_0_1px_var(--nd-hair)] p-3 lg:p-4${stale ? " nd-stale" : ""}`}>
      <svg
        viewBox={`0 0 ${W} ${H}`}
        className="h-40 w-full"
        preserveAspectRatio="none"
        role="img"
        aria-label={t("signal.trendAria", "RSRP over the last 2 minutes, from {{lo}} to {{hi}} dBm", { lo, hi })}
      >
        {bands.map(([a, b, fill]) => (
          <rect key={a} x={PAD_L} y={y(a)} width={W - PAD_L - PAD_R} height={y(b) - y(a)} fill={fill} />
        ))}
        {[-80, -100, -110, -120].map((v) => (
          <text key={v} x={PAD_L - 6} y={y(v) + 4} textAnchor="end" fontSize={11} fill="var(--nd-t3)" style={{ fontVariantNumeric: "tabular-nums" }}>
            {v}
          </text>
        ))}
        <polyline
          points={line}
          fill="none"
          stroke={stale ? "var(--nd-t3)" : "var(--nd-accMark)"}
          strokeWidth={2}
          strokeLinejoin="round"
          strokeLinecap="round"
          vectorEffect="non-scaling-stroke"
        />
      </svg>
      <div className="mt-2 flex justify-between text-[12px] text-nd-t3">
        <span>{t("signal.twoMinAgo", "2 min ago")}</span>
        <span>{t("signal.now", "now")}</span>
      </div>
    </div>
  );
}
