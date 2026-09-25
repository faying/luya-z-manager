"use client";
import { ToggleButton, ToggleButtonGroup, type Key } from "react-aria-components";
import type { ReactNode } from "react";

export type SegOption<K extends string> = { id: K; label: ReactNode; isDisabled?: boolean };

/**
 * Single-choice segmented control: 36 visual, 44 hit. Selected segment is
 * an ink primary pill (a selection, not a status pill).
 * Long English labels wrap to a second line of segments instead of
 * shrinking the type.
 */
export function Segmented<K extends string>({
  label,
  options,
  value,
  onChange,
  isDisabled,
  block,
}: {
  label: string;
  options: SegOption<K>[];
  value: K | null;
  onChange: (v: K) => void;
  isDisabled?: boolean;
  block?: boolean;
}) {
  return (
    <div className={`nd-seg${block ? " nd-seg--block" : ""}`}>
      <ToggleButtonGroup
        aria-label={label}
        selectionMode="single"
        disallowEmptySelection
        isDisabled={isDisabled}
        selectedKeys={value ? [value] : []}
        onSelectionChange={(keys: Set<Key>) => {
          const [k] = [...keys];
          if (k !== undefined && k !== value) onChange(k as K);
        }}
        className="nd-seg__track"
      >
        {options.map((o) => (
          <ToggleButton key={o.id} id={o.id} isDisabled={o.isDisabled} className="nd-seg__item">
            {o.label}
          </ToggleButton>
        ))}
      </ToggleButtonGroup>
    </div>
  );
}
