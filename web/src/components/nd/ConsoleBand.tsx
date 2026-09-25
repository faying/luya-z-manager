import type { ReactNode } from "react";

/** Dark band for terminal-like content only: AT, logs, process lists. */
/** Pass `scrollable` when the band itself scrolls (overflow-x-auto…): it
 *  becomes focusable so keyboard users can scroll it (WCAG 2.1.1). */
export function ConsoleBand({
  children,
  label,
  className = "",
  scrollable,
}: {
  children: ReactNode;
  label: string;
  className?: string;
  scrollable?: boolean;
}) {
  return (
    <section className={`nd-console ${className}`} aria-label={label} tabIndex={scrollable ? 0 : undefined}>
      {children}
    </section>
  );
}
