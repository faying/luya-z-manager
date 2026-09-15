import { cn } from "@/lib/utils";

/* ---------------------------------------------------------------
 * Editorial primitives — pages keep their existing imports
 * (`PageHeader`, `SectionCard`, `StatCard`, `ErrorBanner`) and
 * pick up the new admin design system automatically.
 * ------------------------------------------------------------- */

export function StatCard({
  label,
  value,
  unit,
  hint,
  className,
}: {
  label: string;
  value: string | number;
  unit?: string;
  hint?: string;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "admin-card flex flex-col gap-2 px-5 py-4",
        className
      )}
    >
      <div className="text-[10px] font-medium uppercase tracking-[0.12em] text-text-dim">
        {label}
      </div>
      <div className="flex items-baseline gap-1.5">
        <span
          data-numeric
          className="font-display text-[26px] font-semibold leading-none tracking-tight text-text"
        >
          {value}
        </span>
        {unit && (
          <span className="text-[13px] font-medium text-text-dim">{unit}</span>
        )}
      </div>
      {hint && <div className="text-[12px] text-text-dim">{hint}</div>}
    </div>
  );
}

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: string;
  actions?: React.ReactNode;
}) {
  return (
    <header className="mb-8 flex flex-col gap-3 border-b border-border pb-6 sm:flex-row sm:items-end sm:justify-between sm:gap-6">
      <div className="space-y-2">
        <h2 className="font-display text-[28px] font-semibold leading-[1.15] tracking-[-0.025em] text-text">
          {title}
        </h2>
        {description && (
          <p className="max-w-[60ch] text-[14px] leading-relaxed text-text-dim">
            {description}
          </p>
        )}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}

export function ErrorBanner({
  message,
  onRetry,
}: {
  message: string;
  onRetry?: () => void;
}) {
  return (
    <div
      role="alert"
      className="flex items-start gap-2.5 rounded-md border border-error/30 bg-error/[0.06] px-3.5 py-2.5 text-[13px] leading-relaxed text-error"
    >
      <span className="mt-1 inline-block h-1.5 w-1.5 shrink-0 rounded-full bg-error" />
      <span className="flex-1">{message}</span>
      {onRetry && (
        <button
          type="button"
          onClick={onRetry}
          className="-my-0.5 shrink-0 rounded-md px-2 py-0.5 text-[12px] font-medium text-error underline-offset-2 transition-colors hover:bg-error/10 hover:underline focus:outline-none focus-visible:ring-2 focus-visible:ring-error/30"
        >
          Retry
        </button>
      )}
    </div>
  );
}

export function SectionCard({
  title,
  description,
  actions,
  children,
  className,
}: {
  title?: string;
  description?: string;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("admin-card overflow-hidden", className)}>
      {(title || actions) && (
        <header className="flex items-start justify-between gap-3 border-b border-border/70 px-5 py-4">
          <div className="space-y-1">
            {title && (
              <h3 className="font-display text-[15px] font-semibold tracking-tight text-text">
                {title}
              </h3>
            )}
            {description && (
              <p className="text-[12.5px] text-text-dim">{description}</p>
            )}
          </div>
          {actions && <div className="flex items-center gap-2">{actions}</div>}
        </header>
      )}
      <div className="px-5 py-4">{children}</div>
    </section>
  );
}

/** Status — typographic, no colored pill. Replaces Badge.
 *  Status tokens are AA-contrast at the source (admin.css), so text + dot share one hue. */
export function Status({
  tone = "neutral",
  children,
}: {
  tone?: "neutral" | "success" | "warning" | "danger" | "accent";
  children: React.ReactNode;
}) {
  const colorClass = {
    neutral: "text-text-dim",
    success: "text-success",
    warning: "text-warning",
    danger: "text-error",
    accent: "text-accent",
  }[tone];
  return <span className={cn("admin-status", colorClass)}>{children}</span>;
}

/** MetaRow — interpunct-separated inline meta items. */
export function MetaRow({
  items,
  className,
}: {
  items: Array<React.ReactNode | null | undefined | false>;
  className?: string;
}) {
  const visible = items.filter(Boolean) as React.ReactNode[];
  return (
    <div className={cn("admin-meta flex flex-wrap items-center", className)}>
      {visible.map((item, i) => (
        <span key={i} className="inline-flex items-center">
          {i > 0 && <span className="admin-meta-sep">·</span>}
          {item}
        </span>
      ))}
    </div>
  );
}
