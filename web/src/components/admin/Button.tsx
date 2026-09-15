import { ButtonHTMLAttributes, forwardRef } from "react";
import { cn } from "@/lib/utils";

type Variant = "primary" | "ghost" | "danger" | "outline";
type Size = "sm" | "md";

const VARIANTS: Record<Variant, string> = {
  primary:
    "bg-accent text-white shadow-sm hover:bg-accent-hover active:scale-[0.98] disabled:opacity-50",
  ghost:
    "text-text-dim hover:bg-bg hover:text-text active:scale-[0.98] disabled:opacity-50",
  outline:
    "border border-border bg-bg-card text-text hover:border-accent hover:text-accent active:scale-[0.98] disabled:opacity-50",
  danger:
    "bg-error text-white shadow-sm hover:bg-error/90 active:scale-[0.98] disabled:opacity-50",
};

const SIZES: Record<Size, string> = {
  sm: "h-8 px-3 text-[12px] rounded-lg",
  md: "h-9 px-4 text-[13px] rounded-lg",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
  loading?: boolean;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "primary", size = "md", loading, className, children, disabled, ...rest },
  ref
) {
  return (
    <button
      ref={ref}
      disabled={disabled || loading}
      className={cn(
        "inline-flex items-center justify-center gap-1.5 font-medium transition-[background-color,color,border-color,transform] duration-150",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40 focus-visible:ring-offset-1 focus-visible:ring-offset-bg",
        VARIANTS[variant],
        SIZES[size],
        className
      )}
      {...rest}
    >
      {loading ? <Spinner /> : children}
    </button>
  );
});

function Spinner() {
  return (
    <span
      aria-hidden
      className="inline-block h-3 w-3 animate-[spin_0.7s_linear_infinite] rounded-full border-2 border-current border-r-transparent opacity-70"
    />
  );
}

export function Input({ className, ...rest }: React.InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      className={cn(
        "h-9 w-full rounded-lg border border-border bg-bg-card px-3 text-[13px] text-text placeholder:text-text-dim/70 outline-none transition",
        "focus:border-accent focus:ring-2 focus:ring-accent/20",
        className
      )}
      {...rest}
    />
  );
}

export function Textarea({ className, ...rest }: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={cn(
        "w-full rounded-md border border-border bg-bg px-3 py-2 text-[13px] text-text placeholder:text-text-dim/70 outline-none transition",
        "focus:border-accent focus:ring-2 focus:ring-accent/15",
        className
      )}
      {...rest}
    />
  );
}

export function Toggle({
  checked,
  onChange,
  disabled,
  label,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  label?: string;
}) {
  return (
    <label className={cn("inline-flex cursor-pointer items-center gap-2", disabled && "cursor-not-allowed opacity-50")}>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        disabled={disabled}
        onClick={() => !disabled && onChange(!checked)}
        className={cn(
          // 20x36 visual track with a >=44x24 invisible hit area via ::before so
          // the touch target meets WCAG 2.5.8 (AA) without growing the visuals.
          "relative h-5 w-9 rounded-full border transition-colors duration-150",
          "before:absolute before:left-1/2 before:top-1/2 before:h-6 before:w-11 before:-translate-x-1/2 before:-translate-y-1/2 before:content-['']",
          // Off state needs a clearly grey track + outline so the white knob is
          // visible (white-on-near-white was ~1.05:1). On = ink-blue.
          checked
            ? "border-accent bg-accent"
            : "border-[color:var(--admin-text-disabled)] bg-[color:var(--admin-text-disabled)]",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/40 focus-visible:ring-offset-1 focus-visible:ring-offset-bg"
        )}
      >
        <span
          className={cn(
            // White knob with a hairline ring + shadow so its edge reads on any
            // track. Transform (GPU); inner width 34 (36 - 2*border), travel = 34-16-1-1 = 16px.
            "absolute left-[1px] top-[1px] h-4 w-4 rounded-full bg-white shadow ring-1 ring-black/15 transition-transform duration-150",
            checked ? "translate-x-[16px]" : "translate-x-0"
          )}
        />
      </button>
      {label && <span className="text-[13px] text-text">{label}</span>}
    </label>
  );
}
