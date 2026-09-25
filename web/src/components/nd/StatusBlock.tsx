import type { ReactNode } from "react";
import { StatusMark } from "./StatusMark";
import type { Tone } from "./tone";

/**
 * Full-width state band: state · reason · next step · freshness.
 * The state word is the loudest thing in it; when something is wrong
 * the reason outranks any number on the page.
 */
export function StatusBlock({
  tone,
  state,
  reason,
  meta,
  actions,
  as: Tag = "section",
  labelledBy,
}: {
  tone: Tone;
  state: ReactNode;
  reason?: ReactNode;
  meta?: ReactNode;
  actions?: ReactNode;
  as?: "section" | "div";
  labelledBy?: string;
}) {
  return (
    <Tag
      className={`nd-status nd-status--${tone}${tone === "stale" ? " nd-stale" : ""}`}
      aria-labelledby={labelledBy}
    >
      <div className="nd-status__main">
        <div className="nd-status__state" role="status" aria-live="polite">
          <StatusMark tone={tone}>{state}</StatusMark>
        </div>
        {reason && <div className="nd-status__reason">{reason}</div>}
        {meta && <div className="nd-status__meta">{meta}</div>}
      </div>
      {actions && <div className="nd-status__actions">{actions}</div>}
    </Tag>
  );
}
