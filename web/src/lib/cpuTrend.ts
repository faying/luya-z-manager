// CPU trend for /tools/cpu: one point per /api/cpu reply, 60 points (5 min at 5 s).
export const TREND_POINTS = 60;

export function pushPoint(list: number[], v: number, max = TREND_POINTS): number[] {
  const out = [...list, v];
  return out.length > max ? out.slice(out.length - max) : out;
}

/** SVG polyline points for 0–100 values in a w×h box, newest at the right. */
export function sparkPoints(list: number[], w: number, h: number, max = TREND_POINTS): string {
  if (list.length === 0) return "";
  const step = w / (max - 1);
  const x0 = w - step * (list.length - 1);
  return list
    .map((v, i) => {
      const y = h - (Math.min(100, Math.max(0, v)) / 100) * h;
      return `${(x0 + i * step).toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
}
