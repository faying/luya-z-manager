import Link from "next/link";
import { CaretRight } from "@phosphor-icons/react";
import type { ReactNode } from "react";

/**
 * Whole-card target (scenario, Tailscale, function tiles): the second
 * legal use of a card. Cards that contain their own controls (the home
 * CHILL module) must not use href — put the link on the header instead.
 */
export function ModuleCard({
  href,
  title,
  children,
  headerHref,
  stale = false,
  className = "",
}: {
  href?: string;
  title: ReactNode;
  children?: ReactNode;
  /** Link only the header row (for cards that hold controls). */
  headerHref?: string;
  stale?: boolean;
  className?: string;
}) {
  const head = headerHref ? (
    <Link href={headerHref} className="nd-module__head hover:underline underline-offset-4">
      <span>{title}</span>
      <CaretRight size={16} weight="bold" className="nd-row__chev" aria-hidden />
    </Link>
  ) : (
    <div className="nd-module__head">
      <span>{title}</span>
      {href && <CaretRight size={16} weight="bold" className="nd-row__chev" aria-hidden />}
    </div>
  );
  const inner = (
    <>
      {head}
      {children && <div className={stale ? "nd-stale" : undefined}>{children}</div>}
    </>
  );
  if (href) {
    return (
      <Link href={href} className={`nd-module ${className}`}>
        {inner}
      </Link>
    );
  }
  return <section className={`nd-module ${className}`}>{inner}</section>;
}
