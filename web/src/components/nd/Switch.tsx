"use client";
import { Switch as AriaSwitch } from "react-aria-components";

/**
 * 44×26 track in a 44×44 hit area. On/off reads from the thumb position
 * and the ✓ inside the track; the track colour only helps.
 * Needs an accessible name: pass `label` (visually hidden) when the row
 * label sits elsewhere.
 */
export function Switch({
  isSelected,
  onChange,
  isDisabled,
  label,
}: {
  isSelected: boolean;
  onChange: (on: boolean) => void;
  isDisabled?: boolean;
  label: string;
}) {
  return (
    <AriaSwitch className="nd-switch" isSelected={isSelected} onChange={onChange} isDisabled={isDisabled} aria-label={label}>
      <span className="nd-switch__track">
        <svg className="nd-switch__check" viewBox="0 0 12 12" aria-hidden="true">
          <path d="M2.5 6.2 5 8.6 9.5 3.6" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
        <span className="nd-switch__thumb" />
      </span>
    </AriaSwitch>
  );
}
