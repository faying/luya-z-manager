import type { ReactNode } from "react";
import { TONE_SYMBOL, type Tone } from "./tone";

/** Symbol + word. Colour is never the only cue. */
export function StatusMark({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span className={`nd-mark nd-mark--${tone}`}>
      <span className="nd-mark__sym" aria-hidden="true">{TONE_SYMBOL[tone]}</span>
      <span>{children}</span>
    </span>
  );
}
