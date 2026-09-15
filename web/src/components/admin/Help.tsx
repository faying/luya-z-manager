"use client";

import { useEffect, useRef, useState } from "react";
import { cn } from "@/lib/utils";

/**
 * Inline help affordance — a small "?" that reveals a plain-language
 * explanation of a technical label (PCI, EARFCN, TUN, APN, …).
 *
 * Works on touch *and* pointer: tap/click toggles a popover, and the
 * native `title` + `aria-label` cover hover and screen readers. The
 * popover is `position: fixed` (positioned from the trigger's rect) so
 * it escapes the `overflow-hidden` on the surrounding cards instead of
 * being clipped.
 */
export function Help({ text, className }: { text: string; className?: string }) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    const onDown = (e: MouseEvent) => {
      if (btnRef.current && !btnRef.current.contains(e.target as Node)) setOpen(false);
    };
    // capture scroll on any ancestor; reposition would drift, so just close
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    window.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => {
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onDown);
    };
  }, [open]);

  const toggle = () => {
    const el = btnRef.current;
    if (!el) return;
    if (open) {
      setOpen(false);
      return;
    }
    const r = el.getBoundingClientRect();
    const maxW = 256;
    let left = r.left;
    left = Math.min(left, window.innerWidth - maxW - 8);
    left = Math.max(8, left);
    setPos({ top: r.bottom + 6, left });
    setOpen(true);
  };

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        title={text}
        aria-label={text}
        aria-expanded={open}
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          toggle();
        }}
        className={cn(
          "ml-1 inline-flex h-3.5 w-3.5 shrink-0 cursor-help items-center justify-center rounded-full border border-border align-middle text-[9px] font-semibold leading-none text-text-dim/80 transition-colors hover:border-accent hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/30",
          open && "border-accent text-accent",
          className
        )}
      >
        ?
      </button>
      {open && pos && (
        <span
          role="tooltip"
          style={{ position: "fixed", top: pos.top, left: pos.left, maxWidth: 256 }}
          className="z-[80] block w-max rounded-lg border border-border bg-bg-card px-2.5 py-1.5 text-[11.5px] font-normal normal-case leading-snug tracking-normal text-text shadow-md"
        >
          {text}
        </span>
      )}
    </>
  );
}
