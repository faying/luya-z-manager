"use client";
import { Button as AriaButton, type ButtonProps as AriaButtonProps } from "react-aria-components";
import type { ReactNode, Ref } from "react";

export type ButtonVariant = "primary" | "secondary" | "confirm" | "danger" | "ghost";

/**
 * Pill button, 44 high. primary = the ink primary surface; secondary = white + hairline;
 * confirm = coral with ink text (the "yes, do it" of a tier-2 confirm);
 * danger = fillRed (irreversible). `pending` shows a spinner, keeps the
 * width and blocks repeat presses.
 */
export function Button({
  variant = "primary",
  size,
  iconOnly,
  pending,
  children,
  className = "",
  ref,
  ...rest
}: Omit<AriaButtonProps, "children" | "className"> & {
  ref?: Ref<HTMLButtonElement>;
  variant?: ButtonVariant;
  size?: "sm";
  iconOnly?: boolean;
  pending?: boolean;
  children: ReactNode;
  className?: string;
}) {
  const cls = ["nd-btn", `nd-btn--${variant}`, size && `nd-btn--${size}`, iconOnly && "nd-btn--icon", className]
    .filter(Boolean)
    .join(" ");
  return (
    <AriaButton {...rest} ref={ref} className={cls} isPending={pending} isDisabled={rest.isDisabled}>
      {pending && <span className="nd-spin" aria-hidden />}
      {children}
    </AriaButton>
  );
}
