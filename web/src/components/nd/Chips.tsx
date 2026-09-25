"use client";
import { ToggleButton, ToggleButtonGroup, type Key } from "react-aria-components";

/** Multi-select option chips (44 high, radius 8). Selected = the ink primary surface. */
export function Chips({
  label,
  options,
  value,
  onChange,
  isDisabled,
}: {
  label: string;
  options: string[];
  value: Set<string>;
  onChange: (v: Set<string>) => void;
  isDisabled?: boolean;
}) {
  return (
    <ToggleButtonGroup
      aria-label={label}
      selectionMode="multiple"
      selectedKeys={value}
      onSelectionChange={(keys: Set<Key>) => onChange(new Set([...keys].map(String)))}
      isDisabled={isDisabled}
      className="nd-chips"
    >
      {options.map((o) => (
        <ToggleButton key={o} id={o} className="nd-chip">
          {o}
        </ToggleButton>
      ))}
    </ToggleButtonGroup>
  );
}
