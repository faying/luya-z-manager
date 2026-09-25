import type { ReactNode } from "react";

/** Grid of measurement cells separated by hairlines, no card per cell. */
export function ReadoutWall({ children, label, cols = 4 }: { children: ReactNode; label: string; cols?: 3 | 4 }) {
  return (
    <section className={`nd-wall${cols === 3 ? " nd-wall--3" : ""}`} aria-label={label}>
      {children}
    </section>
  );
}

/**
 * One instrument reading: label, value + unit, one line of state text.
 * value === undefined → still loading (skeleton); null → no value ("—"),
 * never a fake 0 or -140. The number itself is never coloured.
 */
export function Readout({
  label,
  value,
  unit,
  sub,
  wide = false,
  full = false,
  hero = false,
  stale = false,
}: {
  label: ReactNode;
  value: ReactNode | null | undefined;
  unit?: ReactNode;
  sub?: ReactNode;
  wide?: boolean;
  /** Span the whole row. */
  full?: boolean;
  hero?: boolean;
  stale?: boolean;
}) {
  const cls = ["nd-readout", wide && "nd-readout--wide", full && "nd-readout--full", hero && "nd-readout--hero", stale && "nd-stale"]
    .filter(Boolean)
    .join(" ");
  return (
    <div className={cls}>
      <div className="nd-readout__label">{label}</div>
      <div className="nd-readout__value">
        {value === undefined ? (
          <span className="nd-skel" aria-label="…" />
        ) : value === null ? (
          <span className="nd-readout__empty">—</span>
        ) : (
          <>
            <span>{value}</span>
            {unit && <span className="nd-readout__unit">{unit}</span>}
          </>
        )}
      </div>
      {sub && <div className="nd-readout__sub">{sub}</div>}
    </div>
  );
}
