"use client";
import { useEffect, useId, useRef, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "./Button";

/**
 * Tier-2 confirm (design doc §3.1, 13A). First press on the trigger
 * opens this region below it: one sentence of consequence, then
 * "Confirm: <action>" (fillOrange) and "Cancel". No timer. Esc or a
 * press elsewhere closes it. Focus moves to Cancel on open; the trigger
 * carries aria-controls while open (use `triggerProps`). `triggerProps` may sit on the
 * button itself or on a plain wrapper (<span>) around it: ConfirmInline
 * puts aria-expanded on whichever element is the actual button (a wrapper
 * span may not carry aria-expanded, and a switch role doesn't allow it).
 * The confirm button sits in a different place from the trigger, so a
 * double tap cannot reach it.
 */
export function useConfirmInline(open: boolean) {
  const id = useId();
  // aria-controls only while the region exists (it must point at an id
  // that is in the DOM).
  const triggerProps: { "data-confirm-trigger": string; "aria-controls"?: string } = open
    ? { "data-confirm-trigger": id, "aria-controls": id }
    : { "data-confirm-trigger": id };
  return { id, triggerProps };
}

/** The button(s) a trigger wrapper stands for: the element itself if it is a
 *  button, otherwise the buttons inside it. */
function triggerButtons(id: string): HTMLElement[] {
  const out: HTMLElement[] = [];
  document.querySelectorAll<HTMLElement>(`[data-confirm-trigger="${CSS.escape(id)}"]`).forEach((el) => {
    const isButton = el.tagName === "BUTTON" || el.getAttribute("role") === "button";
    if (isButton) out.push(el);
    else el.querySelectorAll<HTMLElement>('button, [role="button"]').forEach((b) => out.push(b));
  });
  return out;
}

export function ConfirmInline({
  id,
  open,
  consequence,
  actionLabel,
  onConfirm,
  onCancel,
  pending,
}: {
  id: string;
  open: boolean;
  consequence: ReactNode;
  actionLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
  pending?: boolean;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  // Keep the latest onCancel without re-running the effect: callers pass
  // inline arrows, and a re-run would pull focus back to Cancel on every
  // re-render (e.g. each poll).
  const cancelCb = useRef(onCancel);
  useEffect(() => {
    cancelCb.current = onCancel;
  });

  // aria-expanded on the trigger button (see useConfirmInline). No deps:
  // one id can serve several triggers and the one carrying triggerProps may
  // change while `open` stays true, so re-apply after every render.
  useEffect(() => {
    const buttons = triggerButtons(id);
    for (const b of buttons) b.setAttribute("aria-expanded", open ? "true" : "false");
    return () => {
      for (const b of buttons) b.removeAttribute("aria-expanded");
    };
  });

  useEffect(() => {
    if (!open) return;
    cancelRef.current?.focus();
    const cancel = () => cancelCb.current();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && cancel();
    const onDown = (e: PointerEvent) => {
      const el = ref.current;
      if (!el || el.contains(e.target as Node)) return;
      // A press on the trigger toggles it itself; ignore it here.
      const triggers = document.querySelectorAll(`[data-confirm-trigger="${CSS.escape(id)}"]`);
      for (const tr of triggers) if (tr.contains(e.target as Node)) return;
      cancel();
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onDown);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("pointerdown", onDown);
    };
  }, [open, id]);

  if (!open) return null;
  return (
    <div ref={ref} id={id} role="group" aria-label={actionLabel} className="nd-confirm">
      <p className="nd-confirm__text" aria-live="polite">
        {consequence}
      </p>
      <div className="nd-confirm__actions">
        <Button variant="confirm" onPress={onConfirm} pending={pending}>
          {t("nd.confirmAction", "Confirm: {{action}}", { action: actionLabel })}
        </Button>
        <Button variant="secondary" onPress={onCancel} ref={cancelRef}>
          {t("common.cancel", "Cancel")}
        </Button>
      </div>
    </div>
  );
}
