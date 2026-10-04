"use client";
// The item page's status block while a change runs and after it ends
// (E4 T9, write-op-layer.md DD12, DD13, DD9, DD16, DD17). Five layers:
// the sentence → old → new · source → three steps → countdown → buttons.
// All words but the button consequences are datad's (STATE_V2.md §12).
//
// - Running: "Revert to X" (primary, left) and "Keep Y" (right), each a
//   tier-2 inline confirm; only one open at a time. When the device decides
//   first (time's up, it reverted), the open confirm is dropped.
// - Ended, sticky: "Got it" (datad only records it; both sides hide it).
// - Revert failed (DD9): what it is now, the last good value, "Retry revert
//   to X" and "Restart device", both confirmed. not_applied (DD17):
//   the hint and "Restart device".
// The countdown is not in the live region (StatusBlock only announces the
// sentence); when reads stop for 20 s it stops and goes grey (DD12).
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { errorText } from "@/lib/api/types";
import { rebootWrite } from "@/lib/api/reboot";
import { useWriteOp } from "@/lib/api/writeOp";
import { useOps } from "@/lib/hooks/useOps";
import { pick, useLang } from "@/lib/i18n/pick";
import { fillNext, fmtClock, markTone, noteAcked, noteMine, opText, type OpView } from "@/lib/ops";
import { Button } from "./Button";
import { ConfirmInline, useConfirmInline } from "./ConfirmInline";
import { OpResult } from "./OpResult";
import { StatusBlock } from "./StatusBlock";

type Pending = "revert" | "keep" | "retry" | null;

/** The write that sets an item back to a raw value (DD9 retry). */
const RETRY_PARAMS: Record<string, (v: string) => Record<string, string>> = {
  "network.set_mode": (v) => ({ mode: v }),
};

export function OpStatus({ item }: { item: string }) {
  const { t } = useTranslation();
  const lang = useLang();
  const ops = useOps();
  const show = ops.show && ops.show.v.item === item ? ops.show : null;
  const v: OpView | null = show?.v ?? null;

  const [pending, setPending] = useState<Pending>(null);
  const [sending, setSending] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const confirm = useConfirmInline(pending !== null);

  // the device decides first: a confirm the state no longer allows is dropped
  const opId = v?.op_id;
  const canRevert = !!v?.can_revert;
  const canKeep = !!v?.can_keep;
  useEffect(() => {
    if ((pending === "revert" && !canRevert) || (pending === "keep" && !canKeep)) setPending(null);
  }, [pending, canRevert, canKeep]);
  useEffect(() => {
    setPending(null);
    setErr(null);
  }, [opId]);

  const reboot = useWriteOp({
    tier: 2,
    ...rebootWrite(t("ops.reboot", "Restart device")),
    waitDevice: { expectedSec: 90, expectDown: true, recovery: t("ops.rebootRecovery", "If the page doesn't come back in a few minutes, check the device's touch screen.") },
  });

  if (!v || !show) return reboot.phase !== "idle" ? <OpResult op={reboot} /> : null;

  const live = show.kind === "live";
  const tone = live ? "neutral" : markTone(v.mark, v.stay);
  const old = opText(v, "old", lang);
  const target = opText(v, "target", lang);
  const back = opText(v, "rollback_to", lang);
  const reason = [old && target ? `${old} → ${target}` : "", opText(v, "source", lang)].filter(Boolean).join(" · ");
  const note = opText(v, "note", lang);
  const next = live
    ? ops.frozen
      ? t("ops.frozen", "Stopped at {{t}} · waiting for the device", { t: fmtClock(ops.remaining) })
      : fillNext(opText(v, "next", lang), ops.remaining)
    : null;

  async function act(kind: "revert" | "keep" | "ack") {
    setSending(true);
    setErr(null);
    try {
      await apiFetch("/api/ops/act", { method: "POST", body: { act: kind, op_id: v!.op_id } });
      if (kind === "ack") noteAcked(v!.op_id);
      setPending(null);
    } catch (e) {
      setErr(errorText(e, lang));
    } finally {
      setSending(false);
      ops.refresh();
    }
  }

  async function retry() {
    const mk = RETRY_PARAMS[v!.action];
    if (!mk || !v!.rollback_to) return;
    setSending(true);
    setErr(null);
    const op_id = `web-retry-${Date.now()}`;
    noteMine(op_id);
    try {
      await apiFetch("/api/ops/write", { method: "POST", body: { op_id, request: { action: v!.action, params: mk(v!.rollback_to) } } });
      setPending(null);
    } catch (e) {
      setErr(errorText(e, lang));
    } finally {
      setSending(false);
      ops.refresh();
    }
  }

  const consequence: Record<Exclude<Pending, null>, string> = {
    revert: t("ops.revertWhat", "Back to {{x}} now; offline ~30 s while it re-registers", { x: back }),
    keep: t("ops.keepWhat", "No auto revert; unconfirmed, change it back yourself if no data"),
    retry: t("ops.retryWhat", "Send the change back to {{x}} again", { x: back }),
  };
  const confirmLabel: Record<Exclude<Pending, null>, string> = {
    revert: pick(v.revert_label_zh, v.revert_label_en, lang),
    keep: pick(v.keep_label_zh, v.keep_label_en, lang),
    retry: t("ops.retry", "Retry revert to {{x}}", { x: back }),
  };
  const open = (p: Pending) => () => {
    setErr(null);
    setPending(pending === p ? null : p);
  };

  const failed = v.phase === "rollback_failed";
  const notApplied = v.phase === "not_applied";
  const sticky = show.kind === "sticky" || show.kind === "alert";
  const rebootBusy = reboot.phase !== "idle" && reboot.phase !== "failed";

  return (
    <div className="nd-opstatus grid gap-2" data-testid="op-status">
      <StatusBlock
        tone={tone}
        state={opText(v, "say", lang)}
        reason={
          <>
            {reason}
            {note && <span className="block">{note}</span>}
            {failed && (
              <span className="block">
                {t("ops.nowAndLastGood", "Now {{now}} · last good {{last}}", {
                  now: opText(v, "readback", lang) || t("ops.unknownNow", "current setting unknown"),
                  last: back,
                })}
              </span>
            )}
            {notApplied && <span className="block">{t("ops.notAppliedHint", "Common after a modem crash; restart, then try again")}</span>}
          </>
        }
        meta={
          <>
            {v.steps?.length > 0 && (
              <ul className="nd-opsteps" aria-label={t("ops.progress", "Progress")}>
                {v.steps.map((s) => (
                  <li key={s.key} className={s.done ? "is-done" : undefined}>
                    <span aria-hidden="true">{s.done ? "✓" : "…"}</span> {lang === "en" ? s.en : s.zh}
                    <span className="sr-only">{s.done ? t("ops.stepDone", " done") : t("ops.stepWaiting", " waiting")}</span>
                  </li>
                ))}
              </ul>
            )}
            {next && (
              <p aria-hidden="true" className={ops.frozen ? "nd-opnext nd-stale" : "nd-opnext"} data-testid="op-next">
                {next}
              </p>
            )}
          </>
        }
      />
      <div className="nd-opbtns">
        {live && v.can_revert && (
          <span {...(pending === "revert" ? confirm.triggerProps : {})}>
            <Button onPress={open("revert")} isDisabled={sending || ops.stuck}>
              {confirmLabel.revert}
            </Button>
          </span>
        )}
        {live && v.can_keep && (
          <span {...(pending === "keep" ? confirm.triggerProps : {})}>
            <Button variant="secondary" onPress={open("keep")} isDisabled={sending || ops.stuck}>
              {confirmLabel.keep}
            </Button>
          </span>
        )}
        {failed && RETRY_PARAMS[v.action] && v.rollback_to && (
          <span {...(pending === "retry" ? confirm.triggerProps : {})}>
            <Button onPress={open("retry")} isDisabled={sending || ops.stuck}>
              {confirmLabel.retry}
            </Button>
          </span>
        )}
        {(failed || notApplied) && (
          <Button variant="secondary" onPress={() => { setPending(null); reboot.start(); }} isDisabled={rebootBusy}>
            {t("ops.reboot", "Restart device")}
          </Button>
        )}
        {sticky && (
          <Button variant="secondary" onPress={() => act("ack")} pending={sending && pending === null}>
            {t("ops.ack", "Got it")}
          </Button>
        )}
      </div>
      {pending && (
        <ConfirmInline
          id={confirm.id}
          open
          consequence={consequence[pending]}
          actionLabel={confirmLabel[pending]}
          pending={sending}
          onConfirm={() => (pending === "retry" ? retry() : act(pending))}
          onCancel={() => setPending(null)}
        />
      )}
      {reboot.phase === "confirming" && (
        <ConfirmInline
          id={`${confirm.id}-reboot`}
          open
          consequence={t("ops.rebootWhat", "Restarts the device; offline 1–2 min")}
          actionLabel={t("ops.reboot", "Restart device")}
          onConfirm={() => reboot.confirm()}
          onCancel={() => reboot.cancel()}
        />
      )}
      {reboot.phase !== "idle" && reboot.phase !== "confirming" && <OpResult op={reboot} />}
      {err && (
        <p className="nd-aux" role="alert">
          {err}
        </p>
      )}
    </div>
  );
}
