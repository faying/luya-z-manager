"use client";
// Enable ADB (tool page, new design). Order: current USB mode → the
// security warning → the one action.
//
//   Enable ADB debug USB mode   tier 3   PUT /api/usb/mode {mode:"debug"}
//
// usb.rs:13 passes the body to `zwrt_bsp.usb set` and returns the firmware
// reply (ubus passthrough). Readback: /api/usb/status `mode` — but that
// field's presence on this firmware is unconfirmed, so the readback is used
// only when the status read before the change actually carried a `mode`;
// otherwise the result is "accepted by the device" (R6).
//
// New here: the old page never read the USB mode. It is read once on entry
// (and on Refresh / after the change) — not polled.
import { useRef, useState, type ReactNode } from "react";
import Link from "next/link";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, Terminal, Warning } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp } from "@/lib/api/writeOp";
import type { UsbModeSet, UsbStatus } from "@/lib/api/schemas/device";
import { Button, ConfirmDialog, OpResult, StatusBlock, StatusMark, type Tone } from "@/components/nd";

function modeName(mode: string): string {
  return mode.toUpperCase();
}

export default function EnableADBPage() {
  const { t } = useTranslation();
  const usb = useApi<UsbStatus>("/api/usb/status", { revalidateOnFocus: false });
  const mode = typeof usb.data?.mode === "string" && usb.data.mode ? usb.data.mode : null;

  const [open, setOpen] = useState(false);
  // Whether a readback is possible, fixed on confirm (before start()).
  const canVerify = useRef(false);

  const op = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("adb.cardTitle", "Enable ADB Debug USB Mode"),
        run: async () => {
          const r = await apiFetch("/api/usb/mode", { method: "PUT", body: { mode: "debug" } satisfies UsbModeSet });
          // No readback possible: re-read once so the status shows what the device reports now.
          if (!canVerify.current) void usb.mutate();
          return r;
        },
      },
    ],
    get verify() {
      if (!canVerify.current) return undefined;
      // The firmware may lag the change a moment: up to 3 reads, 2 s apart.
      return async () => {
        for (let i = 0; i < 3; i++) {
          const d = await apiFetch<UsbStatus>("/api/usb/status");
          await usb.mutate(d, { revalidate: false });
          if (d.mode === "debug") return true;
          if (i < 2) await new Promise((r) => setTimeout(r, 2000));
        }
        return false;
      };
    },
  });
  const done = op.phase === "accepted" || op.phase === "applied";

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("adb.reading", "Reading USB mode…");
  let reason: ReactNode = null;
  if (usb.data) {
    if (mode === "debug") {
      tone = "warn";
      state = t("adb.isOn", "ADB is on");
      reason = t("adb.isOnReason", "USB is in debug mode: anyone with the cable has a root shell. Switch USB to another mode when you're done.");
    } else if (mode) {
      tone = "neutral";
      state = t("adb.isOff", "ADB is off");
      reason = t("adb.currentMode", "USB mode: {{mode}}", { mode: modeName(mode) });
    } else {
      tone = "neutral";
      state = t("adb.modeUnknown", "USB mode not reported");
      reason = t("adb.modeUnknownReason", "The device didn't say which USB mode is active.");
    }
  } else if (usb.error) {
    tone = "warn";
    state = t("adb.unread", "Couldn't read the USB mode");
    reason = usb.error instanceof Error ? usb.error.message : String(usb.error);
  }

  const isOn = mode === "debug" || done;

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">{t("adb.title", "Enable ADB")}</h1>
        <Button variant="ghost" iconOnly onPress={() => void usb.mutate()} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>

      <div className="grid max-w-[720px] gap-6">
        <p className="nd-body -mt-2 text-nd-t2">{t("adb.desc", "Switch USB to Android Debug Bridge mode.")}</p>

        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          actions={
            usb.error ? (
              <Button variant="secondary" onPress={() => void usb.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        <section aria-labelledby="adb-action" className="nd-group grid gap-4 p-4 lg:p-5">
          <h2 id="adb-action" className="font-semibold">
            {t("adb.cardTitle", "Enable ADB Debug USB Mode")}
          </h2>
          <p className="nd-body text-nd-t2">
            {t("adb.cardDesc", "Switches the USB port to ADB debug mode, exposing a full shell over USB.")}
          </p>
          <p className="nd-body flex items-start gap-2">
            <Warning size={20} weight="bold" className="mt-0.5 shrink-0 text-nd-warnT" aria-hidden />
            <span>
              {t(
                "adb.warning",
                "This exposes ADB over USB. Anyone with physical access to the USB port will have root shell access. Disable when not in use."
              )}
            </span>
          </p>
          <div>
            <Button variant={isOn ? "secondary" : "primary"} onPress={() => setOpen(true)} isDisabled={op.busy} pending={op.busy}>
              <Terminal size={20} weight="bold" aria-hidden />
              {isOn ? t("adb.reEnable", "Re-enable ADB") : t("adb.cardTitle", "Enable ADB Debug USB Mode")}
            </Button>
          </div>
          {done ? (
            <p role="status">
              <StatusMark tone="ok">
                {t("adb.successPrefix", "ADB debug mode enabled. Connect via USB and run")} <code className="nd-mono">adb devices</code>
                {t("adb.successSuffix", ".")}
              </StatusMark>
            </p>
          ) : (
            <OpResult op={op} />
          )}
          <p className="nd-aux">
            {t("adb.offHint", "To turn ADB off, pick another mode on the")}{" "}
            <Link href="/usb" className="text-nd-accT underline">
              {t("adb.usbPage", "USB mode page")}
            </Link>
            {t("adb.offHintEnd", ".")}
          </p>
        </section>
      </div>

      <ConfirmDialog
        open={open}
        onOpenChange={(o) => !o && setOpen(false)}
        title={t("adb.confirmTitle", "Enable ADB debug USB mode?")}
        what={
          <>
            <span className="block">
              {t(
                "adb.confirmWhat",
                "This will expose ADB over USB, allowing full shell access to the device. Only do this if you have physical access and trust your environment."
              )}
            </span>
            <span className="mt-2 block">
              {t(
                "adb.warning",
                "This exposes ADB over USB. Anyone with physical access to the USB port will have root shell access. Disable when not in use."
              )}
            </span>
          </>
        }
        downtime={t("adb.confirmDowntime", "If you are connected to this page over the USB cable (USB network), that connection drops. Wi-Fi and the mobile connection are not affected.")}
        recovery={t("adb.confirmRecovery", "Connect over Wi-Fi and switch USB back to another mode on the USB mode page.")}
        actionLabel={t("adb.confirmAction", "Enable ADB")}
        cutsUplink
        danger
        onConfirm={() => {
          canVerify.current = mode !== null;
          setOpen(false);
          op.start();
          op.confirm();
        }}
      />
    </>
  );
}
