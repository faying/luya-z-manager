// Status semantics shared with the touchscreen (docs/DESIGN.md §2):
// normal ● okT, warning ▲ warnT, stopped ■ badT, neutral ● t3.
// "stale" = data that stopped updating; it reads as neutral.
export type Tone = "ok" | "warn" | "bad" | "neutral" | "stale";

export const TONE_SYMBOL: Record<Tone, string> = {
  ok: "●",
  warn: "▲",
  bad: "■",
  neutral: "●",
  stale: "●",
};
