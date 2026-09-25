"use client";
import { useTranslation } from "react-i18next";
import type { UseWriteOp } from "@/lib/api/writeOp";
import { Button } from "./Button";
import { StatusMark } from "./StatusMark";

/** Inline result of one write op (the design's write lifecycle, §5.1). */
export function OpResult({ op }: { op: UseWriteOp }) {
  const { t } = useTranslation();
  switch (op.phase) {
    case "submitting":
    case "verifying":
      return <p className="nd-aux" role="status">{t("nd.submitting", "Sending…")}</p>;
    case "waitDevice":
      return (
        <p className="nd-body" role="status">
          <StatusMark tone="neutral">
            {t("nd.waitingDevice", "Waiting for the device to come back · about {{s}} s", { s: op.remainingSec ?? 0 })}
          </StatusMark>
        </p>
      );
    case "accepted":
    case "applied":
      return (
        <p role="status">
          <StatusMark tone="ok">{op.phase === "applied" ? t("nd.applied", "Applied") : t("nd.accepted", "Accepted by the device")}</StatusMark>
        </p>
      );
    case "partial":
      return (
        <div role="alert" className="grid gap-2">
          <StatusMark tone="warn">
            {t("nd.partial", "Partly applied: {{done}} done, {{pending}} not confirmed", { done: op.done.join(", "), pending: op.notDone.join(", ") })}
          </StatusMark>
          <div>
            <Button variant="confirm" size="sm" onPress={op.resubmit}>
              {t("nd.resubmitRest", "Send the rest: {{steps}}", { steps: op.notDone.join(", ") })}
            </Button>
          </div>
        </div>
      );
    case "unknown":
      return (
        <p role="alert">
          <StatusMark tone="warn">{t("nd.unknown", "Connection dropped; not yet confirmed whether it took effect")}</StatusMark>
        </p>
      );
    case "failed":
      return (
        <p role="alert">
          <StatusMark tone="bad">
            {op.errorKind === "mismatch"
              ? t("nd.mismatch", "Not applied: the device still shows the old value")
              : t("nd.failed", "Not applied: {{e}}", { e: op.error ?? "" })}
          </StatusMark>
        </p>
      );
    case "resubmitReady":
      return (
        <div role="alert" className="grid gap-2">
          <span className="nd-aux">{t("nd.resubmitAfterLogin", "Signed in again. The change was not sent yet.")}</span>
          <div>
            <Button variant="confirm" size="sm" onPress={op.resubmit}>
              {t("nd.resubmit", "Send again")}
            </Button>
          </div>
        </div>
      );
    case "waitTimeout":
      return (
        <p role="alert" className="grid gap-1">
          <StatusMark tone="bad">{t("nd.waitTimeout", "The device has not come back")}</StatusMark>
          {op.recovery && <span className="nd-aux">{op.recovery}</span>}
        </p>
      );
    default:
      return null;
  }
}
