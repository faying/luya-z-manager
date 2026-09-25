"use client";
// Small "?" that opens a short explanation on press (never on hover, so it
// works the same with touch, mouse and keyboard). 44px hit area.
import { Button as AriaButton, Dialog, DialogTrigger, Popover } from "react-aria-components";
import { Question } from "@phosphor-icons/react";
import { useTranslation } from "react-i18next";

export function Help({ text, label }: { text: string; label?: string }) {
  const { t } = useTranslation();
  return (
    <DialogTrigger>
      <AriaButton
        aria-label={label ? t("nd.aboutX", "About {{x}}", { x: label }) : t("nd.about", "About")}
        className="-my-3 inline-flex h-11 w-11 items-center justify-center rounded-full text-nd-t3 hover:text-nd-t1"
      >
        <Question size={18} weight="bold" aria-hidden />
      </AriaButton>
      <Popover placement="top" offset={4} className="nd z-50 max-w-[300px] rounded-nd-field bg-nd-card p-3 text-[14px] leading-5 text-nd-t1 shadow-nd-float ring-1 ring-nd-hair">
        <Dialog className="outline-none">{text}</Dialog>
      </Popover>
    </DialogTrigger>
  );
}
