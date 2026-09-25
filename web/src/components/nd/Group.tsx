import Link from "next/link";
import { CaretRight, type Icon } from "@phosphor-icons/react";
import type { ReactNode } from "react";

/** Group title: 13/18 t2, more space above than below. */
export function GroupTitle({ children, id }: { children: ReactNode; id?: string }) {
  return (
    <h2 className="nd-group-title" id={id}>
      {children}
    </h2>
  );
}

/** One card holding a set of like rows (the first legal use of a card). */
export function Group({
  title,
  children,
  className = "",
  stale = false,
}: {
  title?: ReactNode;
  children: ReactNode;
  className?: string;
  stale?: boolean;
}) {
  return (
    <section className={className}>
      {title && <GroupTitle>{title}</GroupTitle>}
      <div className={`nd-group${stale ? " nd-stale" : ""}`}>{children}</div>
    </section>
  );
}

type RowProps = {
  icon?: Icon;
  label: ReactNode;
  sub?: ReactNode;
  value?: ReactNode;
  /** Right-side control (switch, segmented, button). */
  control?: ReactNode;
  href?: string;
  onPress?: () => void;
  mono?: boolean;
};

/** 48px row (60 with a sub line). Links and buttons get the chevron. */
export function Row({ icon: I, label, sub, value, control, href, onPress, mono }: RowProps) {
  const body = (
    <>
      {I && <I size={20} weight="bold" className="nd-row__icon" aria-hidden />}
      <span className="nd-row__text">
        <span className="nd-row__label">{label}</span>
        {sub && <span className="nd-row__sub block">{sub}</span>}
      </span>
      {value !== undefined && <span className={`nd-row__value${mono ? " nd-mono" : ""}`}>{value}</span>}
      {control}
      {(href || onPress) && <CaretRight size={16} weight="bold" className="nd-row__chev" aria-hidden />}
    </>
  );
  const cls = `nd-row${sub ? " nd-row--two" : ""}`;
  if (href) {
    return (
      <Link href={href} className={cls}>
        {body}
      </Link>
    );
  }
  if (onPress) {
    return (
      <button type="button" className={cls} onClick={onPress}>
        {body}
      </button>
    );
  }
  return <div className={cls}>{body}</div>;
}
