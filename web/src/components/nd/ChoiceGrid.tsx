"use client";
// Single choice among cards that are themselves buttons (regions,
// AI exit): the one legal "card grid" on a page (design doc §6, 15A).
// Phones 2 columns, ≥1024 4 columns. Selected = accS wash + accT + ✓.
import { ToggleButton, ToggleButtonGroup, type Key } from "react-aria-components";
import { CheckCircle } from "@phosphor-icons/react";

export function ChoiceGrid({
  label,
  options,
  value,
  onChange,
  isDisabled,
}: {
  label: string;
  options: string[];
  value: string | null | undefined;
  onChange: (v: string) => void;
  isDisabled?: boolean;
}) {
  return (
    <ToggleButtonGroup
      aria-label={label}
      selectionMode="single"
      disallowEmptySelection
      selectedKeys={value ? [value] : []}
      onSelectionChange={(keys: Set<Key>) => {
        const [k] = [...keys];
        if (k !== undefined && k !== value) onChange(String(k));
      }}
      isDisabled={isDisabled}
      className="nd-choices"
    >
      {options.map((o) => (
        <ToggleButton key={o} id={o} className="nd-choice">
          {({ isSelected }) => (
            <>
              <span className="min-w-0 flex-1 truncate text-start">{o}</span>
              {isSelected && <CheckCircle size={20} weight="fill" aria-hidden />}
            </>
          )}
        </ToggleButton>
      ))}
    </ToggleButtonGroup>
  );
}
