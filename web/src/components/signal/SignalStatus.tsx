"use client";
// Signal status block shared by home and /signal: state · reason · next
// step · freshness, plus the radio summary line (design doc §5, 3A).
import Link from "next/link";
import { useTranslation } from "react-i18next";
import type { ReactNode } from "react";
import type { NetworkSignal } from "@/lib/api/schemas/network";
import { Button, Freshness, StatusBlock, type Tone } from "@/components/nd";
import type { Carrier, SigState } from "@/lib/home";
import { carrierSummary, radioName } from "@/lib/signalWords";

export function SignalStatus({
  state, allDown, sig, serving, counts, bars, lastOkAt, speedStale, onRetry,
}: {
  state: SigState;
  allDown: boolean;
  sig: NetworkSignal | undefined;
  serving: Carrier | undefined;
  counts: { nr: number; lte: number };
  bars: number | null;
  lastOkAt: number | null;
  speedStale: boolean;
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  const summary = [
    radioName(sig?.network_type),
    sig?.network_provider_fullname || sig?.network_provider,
    counts.nr + counts.lte > 0 ? carrierSummary(t, counts) : null,
    bars != null && bars >= 0 ? t("home.bars", "{{n}}/5 bars", { n: bars }) : null,
  ].filter(Boolean).join(" · ");

  let tone: Tone = "neutral";
  let word: string;
  let reason: ReactNode = null;
  let actions: ReactNode = null;
  const retry = (
    <Button variant="secondary" size="sm" onPress={onRetry}>
      {t("home.retryNow", "Retry now")}
    </Button>
  );

  switch (state) {
    case "loading":
      word = t("home.connecting", "Connecting to the device…");
      break;
    case "stale":
      tone = allDown ? "bad" : "stale";
      word = allDown ? t("home.deviceDown", "Lost contact with the device") : t("home.sigStale", "Signal and speed are not updating");
      reason = (
        <>
          <Freshness stale lastOkAt={lastOkAt} />
          {allDown && <> · {t("home.retrying", "retrying")}</>}
        </>
      );
      actions = retry;
      break;
    case "nosim":
      tone = "bad";
      word = t("home.noSim", "No SIM card");
      reason = t("home.noSimNext", "Insert a SIM, or enable a profile in Functions → eSIM");
      break;
    case "none":
      tone = "warn";
      word = t("home.noService", "No network service");
      reason = (
        <Link href="/signal" className="font-medium text-nd-accT hover:underline underline-offset-4">
          {t("home.checkSignal", "Check signal ›")}
        </Link>
      );
      break;
    case "weak":
      tone = "warn";
      word = t("home.sigWeak", "Weak signal");
      reason = (
        <>
          {serving?.rsrp != null && <>RSRP {serving.rsrp} dBm · </>}
          <Link href="/bandlock" className="font-medium text-nd-accT hover:underline underline-offset-4">
            {t("home.toBandlock", "Band lock ›")}
          </Link>
        </>
      );
      break;
    default:
      tone = "ok";
      word = t("home.sigGood", "Signal good");
      if (speedStale) {
        reason = <Freshness stale lastOkAt={null} what={t("home.speed", "Speed")} />;
        actions = retry;
      }
  }

  return (
    <div>
      <StatusBlock tone={tone} state={word} reason={reason} meta={state !== "loading" ? summary : undefined} actions={actions} />
    </div>
  );
}

