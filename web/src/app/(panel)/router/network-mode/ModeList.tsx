"use client";
// Single choice among rows that carry a label, a one-line description and
// an "in use" mark. ChoiceGrid only takes bare strings, so this is a local
// radio list: one Group card, 60px rows, ✓ on the selected one.
import { Radio, RadioGroup } from "react-aria-components";
import { CheckCircle, Circle } from "@phosphor-icons/react";
import { StatusMark } from "@/components/nd";

export interface ModeOption {
  value: string;
  label: string;
  desc: string;
}

export function ModeList({
  label,
  options,
  value,
  current,
  currentLabel,
  onChange,
  isDisabled,
}: {
  label: string;
  options: ModeOption[];
  value: string | null;
  current: string | undefined;
  currentLabel: string;
  onChange: (v: string) => void;
  isDisabled?: boolean;
}) {
  return (
    <RadioGroup
      aria-label={label}
      value={value}
      onChange={onChange}
      isDisabled={isDisabled}
      className="nd-group"
    >
      {options.map((o) => (
        <Radio
          key={o.value}
          value={o.value}
          aria-label={o.label}
          className={({ isSelected, isDisabled: dis, isFocusVisible }) =>
            [
              "nd-row nd-row--two cursor-pointer",
              isSelected ? "bg-nd-accS" : "",
              dis ? "cursor-not-allowed opacity-60" : "",
              isFocusVisible ? "outline-2 outline-offset-[-2px] outline-[var(--nd-focus)]" : "",
            ].join(" ")
          }
        >
          {({ isSelected }) => (
            <>
              {isSelected ? (
                <CheckCircle size={22} weight="fill" className="shrink-0 text-nd-t1" aria-hidden />
              ) : (
                <Circle size={22} weight="bold" className="shrink-0 text-nd-t3" aria-hidden />
              )}
              <span className="nd-row__text">
                <span className={`nd-row__label${isSelected ? " font-semibold" : ""}`}>{o.label}</span>
                <span className="nd-row__sub block">{o.desc}</span>
              </span>
              {current === o.value && (
                <span className="nd-aux shrink-0">
                  <StatusMark tone="ok">{currentLabel}</StatusMark>
                </span>
              )}
            </>
          )}
        </Radio>
      ))}
    </RadioGroup>
  );
}
