"use client";
// Operator logo with a first-letter badge fallback (missing file, load error,
// unknown PLMN). See lib/operatorLogo.ts.
import { useState } from "react";
import { badgeText, logoSlug, logoSrc } from "@/lib/operatorLogo";

const BASE = process.env.NEXT_PUBLIC_BASE_PATH ?? "";

export function OperatorLogo({
  mcc,
  mnc,
  name,
  size = 20,
}: {
  mcc: string | null | undefined;
  mnc: string | null | undefined;
  name: string | null | undefined;
  size?: number;
}) {
  const src = logoSrc(logoSlug(mcc, mnc), BASE);
  const [failed, setFailed] = useState<string | null>(null);
  if (src && failed !== src) {
    return (
      // White chip: many marks are black/dark and vanish in dark mode. Wide
      // wordmarks (SmarTone, SoftBank) keep their aspect up to 4× the height.
      <span
        className="inline-flex shrink-0 items-center justify-center align-middle"
        style={{ height: size, maxWidth: size * 4, padding: "1px 3px", borderRadius: 4, background: "#fff" }}
      >
        {/* eslint-disable-next-line @next/next/no-img-element -- static export, plain file */}
        <img src={src} alt="" style={{ height: size - 2, width: "auto", maxWidth: size * 4 - 6 }} onError={() => setFailed(src)} />
      </span>
    );
  }
  return (
    <span
      aria-hidden
      className="inline-flex shrink-0 items-center justify-center rounded-full align-middle text-nd-t1"
      style={{ width: size, height: size, fontSize: size * 0.55, lineHeight: 1, background: "var(--nd-fill)" }}
    >
      {badgeText(name)}
    </span>
  );
}
