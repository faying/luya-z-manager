"use client";
// The change in progress, on every page (E4 T9, write-op-layer.md DD4,
// DD15, DD16): one ink strip like the alert bar, "Checking · 1:42 ›" while it
// runs, then the result ("▲ No data · back to Auto ›") until someone presses
// "Got it" here or on the touch screen. Links to the item's page, where the
// full progress and the buttons are; not shown on that page itself.
// Only phase changes are announced: the countdown is aria-hidden. A failed
// revert is role=alert. Also says when datad is stuck (writes are off) and
// when the agent dropped while a change was running (result unknown).
// With nothing running or to report: the one-time "auto revert is on" notice
// (DD18) with "Got it", which datad records for both sides.
import { useState } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { errorText } from "@/lib/api/types";
import { useOps } from "@/lib/hooks/useOps";
import { ITEM_ROUTE, fmtClock, markTone, noteNoticeAcked, opNotice, opText } from "@/lib/ops";
import { useLang } from "@/lib/i18n/pick";
import { Button } from "../Button";
import { TONE_SYMBOL } from "../tone";

export function OpBanner() {
  const { t } = useTranslation();
  const lang = useLang();
  const pathname = (usePathname() ?? "").replace(/\/$/, "");
  const ops = useOps();
  const [acking, setAcking] = useState(false);
  const [ackErr, setAckErr] = useState<string | null>(null);

  if (ops.disconnected) {
    return (
      <div role="status" aria-live="polite" className="nd-announce" data-testid="op-banner">
        <p className="nd-announce__text">
          <span aria-hidden="true" className="nd-announce__sym">▲</span>
          <span className="font-semibold">{t("ops.disconnected", "Disconnected · result unknown")}</span>
          <span className="nd-announce__detail">{t("ops.disconnectedDetail", "Nothing is resent. The result shows here once the device answers again.")}</span>
        </p>
      </div>
    );
  }
  if (ops.stuck) {
    return (
      <div role="status" aria-live="polite" className="nd-announce" data-testid="op-banner">
        <p className="nd-announce__text">
          <span aria-hidden="true" className="nd-announce__sym nd-announce__sym--bad">■</span>
          <span className="font-semibold">{t("ops.stuck", "Data service not responding")}</span>
          <span className="nd-announce__detail">{t("ops.stuckDetail", "Settings can't be changed for now.")}</span>
        </p>
      </div>
    );
  }

  const show = ops.show;
  if (!show) {
    if (!opNotice(ops.data?.op)) return null;
    const ack = async () => {
      setAcking(true);
      setAckErr(null);
      try {
        await apiFetch("/api/ops/act", { method: "POST", body: { act: "notice_ack" } });
        noteNoticeAcked();
      } catch (e) {
        setAckErr(errorText(e, lang));
      } finally {
        setAcking(false);
        ops.refresh();
      }
    };
    return (
      <div role="status" aria-live="polite" className="nd-announce" data-testid="op-notice">
        <p className="nd-announce__text">
          <span aria-hidden="true" className="nd-announce__sym nd-announce__sym--neutral">●</span>
          <span className="nd-announce__wrap font-semibold">
            {t("ops.noticeRollbackOn", "Auto revert on: reverts a mode change that can't connect")}
          </span>
          {ackErr && <span className="nd-announce__detail">{ackErr}</span>}
        </p>
        <div className="flex shrink-0 items-center">
          <Button variant="ghost" size="sm" className="nd-announce__btn" onPress={ack} pending={acking}>
            {t("ops.ack", "Got it")}
          </Button>
        </div>
      </div>
    );
  }
  const v = show.v;
  const href = ITEM_ROUTE[v.item] ?? "/changes";
  if (pathname === href) return null;

  const live = show.kind === "live";
  const tone = live ? "neutral" : markTone(v.mark, v.stay);
  const old = opText(v, "old", lang);
  const target = opText(v, "target", lang);
  const detail = [opText(v, "what", lang), old && target ? `${old} → ${target}` : "", opText(v, "source", lang)]
    .filter(Boolean)
    .join(" · ");

  return (
    <div
      role={show.kind === "alert" ? "alert" : "status"}
      aria-live={show.kind === "alert" ? "assertive" : "polite"}
      className="nd-announce"
      data-testid="op-banner"
    >
      <p className="nd-announce__text">
        <span aria-hidden="true" className={`nd-announce__sym nd-announce__sym--${tone}`}>
          {live ? "●" : TONE_SYMBOL[tone]}
        </span>
        <span className="font-semibold">
          {opText(v, "say", lang)}
          {live && v.remaining_ms !== null && (
            <span aria-hidden="true" className={ops.frozen ? "nd-announce__frozen" : undefined}>
              {" · "}
              {fmtClock(ops.remaining)}
            </span>
          )}
        </span>
        {detail && <span className="nd-announce__detail">{detail}</span>}
      </p>
      <div className="flex shrink-0 items-center">
        <Link href={href} className="nd-btn nd-btn--sm nd-announce__btn nd-announce__view">
          {t("ops.details", "Details")} ›
        </Link>
      </div>
    </div>
  );
}
