// "Last 5 minutes" (audit C, 10-04): the touch screen draws speed / CPU /
// memory / battery from its own samples; the agent keeps no such history, so
// the web samples while the page is open. Module-level so leaving the page
// and coming back in the same tab keeps what was gathered.
export interface TrendSample {
  /** Date.now() of the sample. */
  t: number;
  /** Mbps, mobile link. */
  down: number | null;
  up: number | null;
  /** % */
  cpu: number | null;
  mem: number | null;
  bat: number | null;
  /** W into (+) or out of (−) the battery. */
  watt: number | null;
}

export const TREND_WINDOW_MS = 5 * 60_000;
export const TREND_EVERY_MS = 3000;

let samples: TrendSample[] = [];

/** Add one sample; drop what's older than the window (and anything after a long gap). */
export function pushSample(s: TrendSample): TrendSample[] {
  const last = samples[samples.length - 1];
  // a gap of more than a minute (tab asleep): start over rather than draw a straight line across it
  if (last && s.t - last.t > 60_000) samples = [];
  samples = [...samples.filter((x) => s.t - x.t <= TREND_WINDOW_MS), s];
  return samples;
}

export function trendSamples(): TrendSample[] {
  return samples;
}

export function resetTrend(): void {
  samples = [];
}
