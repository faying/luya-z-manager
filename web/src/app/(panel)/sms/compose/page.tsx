"use client";
// Compose SMS (new design). Tool page: recipient, text, length, send.
//
// Send (controls-inventory §/sms/compose): tier 2 — it goes out over the
// mobile network and the carrier may charge per part, so the inline confirm
// names the recipient, the length and the number of parts. POST
// /api/sms/send; no readback exists (the outbox is not in the inbox list),
// so success is "accepted by the device" and the page returns to the inbox.
//
// Fixed vs the old page:
// - The agent passes the firmware reply through without looking at it
//   (sms.rs:49-58). The firmware answers {result:3} on success
//   (sms_forward.rs check_sms_send_result treats 3 or no result as sent);
//   any other result is now shown as a failure instead of returning to the
//   inbox as if it had been sent.
// - Part count: a message longer than one part is split into 153-character
//   (GSM 7-bit) or 67-character (UCS2) parts, not 160 / 70.
// - UCS2 encoding walks UTF-16 code units, so emoji (surrogate pairs) are
//   no longer cut to their first half.
// - The count and the confirm use the trimmed text that is actually sent.
import { useEffect, useId, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { useTranslation } from "react-i18next";
import { ArrowLeft, PaperPlaneRight } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { useWriteOp } from "@/lib/api/writeOp";
import type { SmsSendRequest, SmsWmsResult } from "@/lib/api/schemas/sms";
import { Button, ConfirmInline, OpResult, useConfirmInline, useToast } from "@/components/nd";

function toUCS2Hex(text: string): string {
  let out = "";
  for (let i = 0; i < text.length; i++) out += text.charCodeAt(i).toString(16).padStart(4, "0");
  return out;
}

function needsUCS2(text: string): boolean {
  for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) > 127) return true;
  return false;
}

/** Parts on the air: one part holds 160 / 70; split parts hold 153 / 67. */
function partsOf(len: number, ucs2: boolean): number {
  if (len === 0) return 0;
  const single = ucs2 ? 70 : 160;
  const multi = ucs2 ? 67 : 153;
  return len <= single ? 1 : Math.ceil(len / multi);
}

function getSMSTime(): string {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
}

/** Digits with an optional leading +; spaces, dashes and brackets are ignored. */
const cleanNumber = (s: string) => s.replace(/[\s\-()]/g, "");
const isNumber = (s: string) => /^\+?\d{3,20}$/.test(s);

export default function ComposePage() {
  const { t } = useTranslation();
  const router = useRouter();
  const toast = useToast();
  const toId = useId();
  const bodyId = useId();
  const countId = useId();

  const [to, setTo] = useState("");
  const [body, setBody] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const inline = useConfirmInline(asking);

  const number = cleanNumber(to);
  const text = body.trim();
  const ucs2 = needsUCS2(text);
  const chars = text.length;
  const parts = partsOf(chars, ucs2);

  // The op snapshots its config at start(): the request travels in a ref.
  const reqRef = useRef<SmsSendRequest | null>(null);
  const op = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("smscompose.stepSend", "Send SMS"),
        run: async () => {
          const r = await apiFetch<SmsWmsResult | null>("/api/sms/send", { method: "POST", body: reqRef.current });
          const code = r && typeof r === "object" ? r.result : undefined;
          if (code !== undefined && code !== null && String(code) !== "3") {
            throw new ApiError(t("smscompose.rejected", "the device did not send it (result {{code}})", { code: String(code) }), 502);
          }
          return r;
        },
      },
    ],
  });

  function ask() {
    if (op.busy) return;
    if (!number) return setErr(t("smscompose.errEnterPhone", "Please enter a phone number."));
    if (!isNumber(number)) return setErr(t("smscompose.errBadNumber", "Enter digits only, optionally starting with +, 3 to 20 digits."));
    if (!text) return setErr(t("smscompose.errEmptyMessage", "Message cannot be empty."));
    setErr(null);
    reqRef.current = {
      number,
      sms_time: getSMSTime(),
      message_body: ucs2 ? toUCS2Hex(text) : text,
      id: "-1",
      encode_type: ucs2 ? "ucs2" : "gsm7",
    };
    setAsking(true);
  }

  function send() {
    setAsking(false);
    op.start();
    op.confirm();
  }

  // Accepted: back to the inbox, as before, with a toast saying so.
  const phase = op.phase;
  useEffect(() => {
    if (phase !== "accepted") return;
    const sentTo = reqRef.current?.number ?? "";
    toast.show("ok", t("smscompose.accepted", "Accepted by the device: SMS to {{to}}", { to: sentTo }));
    router.push("/sms/");
    // Only the phase change matters here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase]);

  const locked = op.busy;

  return (
    <>
      <div className="mb-4 mt-2 flex flex-wrap items-center gap-3">
        <h1 className="nd-title flex-1">{t("smscompose.title", "Compose SMS")}</h1>
        <Button variant="ghost" onPress={() => router.push("/sms/")}>
          <ArrowLeft size={20} weight="bold" aria-hidden />
          {t("smscompose.back", "Back")}
        </Button>
      </div>

      <div className="grid max-w-[720px] gap-4">
        <p className="nd-body text-nd-t2">
          {t("smscompose.descNd", "Sent from the SIM in the device over the mobile network. Your carrier may charge for each part.")}
        </p>

        <div className="nd-group grid gap-4 p-4 lg:p-5">
          <label className="grid gap-1" htmlFor={toId}>
            <span className="nd-aux">{t("smscompose.toLabel", "To (phone number)")}</span>
            <input
              id={toId}
              type="tel"
              inputMode="tel"
              autoComplete="off"
              className="nd-field nd-mono"
              placeholder="+886912345678"
              value={to}
              disabled={locked}
              aria-invalid={!!err && (!number || !isNumber(number)) ? true : undefined}
              onChange={(e) => {
                setTo(e.target.value);
                setErr(null);
                setAsking(false);
              }}
            />
          </label>

          <label className="grid gap-1" htmlFor={bodyId}>
            <span className="nd-aux">{t("smscompose.messageLabel", "Message")}</span>
            <textarea
              id={bodyId}
              className="nd-field min-h-[160px] resize-y py-3 leading-6"
              placeholder={t("smscompose.messagePlaceholder", "Type your message…")}
              value={body}
              disabled={locked}
              aria-describedby={countId}
              onChange={(e) => {
                setBody(e.target.value);
                setErr(null);
                setAsking(false);
              }}
            />
          </label>
          <p id={countId} className="nd-aux flex flex-wrap justify-between gap-2" aria-live="polite">
            <span>
              {parts !== 1
                ? t("smscompose.charsSegmentsPlural", "{{chars}} chars · {{segments}} segments", { chars, segments: parts })
                : t("smscompose.charsSegments", "{{chars}} chars · {{segments}} segment", { chars, segments: parts })}
            </span>
            {ucs2 && <span>{t("smscompose.ucs2Encoding", "Unicode (UCS2) encoding")}</span>}
          </p>

          {err && (
            <p role="alert" className="nd-error">
              {err}
            </p>
          )}

          <div className="flex flex-wrap gap-2">
            <span {...inline.triggerProps}>
              <Button onPress={ask} isDisabled={locked || !to.trim() || !text} pending={locked}>
                <PaperPlaneRight size={20} weight="bold" aria-hidden />
                {t("smscompose.send", "Send")}
              </Button>
            </span>
            <Button variant="secondary" onPress={() => router.push("/sms/")} isDisabled={locked}>
              {t("smscompose.cancel", "Cancel")}
            </Button>
          </div>

          <ConfirmInline
            id={inline.id}
            open={asking}
            actionLabel={t("smscompose.sendAction", "send to {{to}}", { to: reqRef.current?.number ?? number })}
            consequence={t(
              "smscompose.sendConsequence",
              "Sends {{chars}} characters to {{to}} as {{parts}} SMS part(s) over the mobile network. Your carrier may charge for each part. A sent SMS can't be recalled.",
              { chars, to: reqRef.current?.number ?? number, parts },
            )}
            onCancel={() => setAsking(false)}
            onConfirm={send}
          />
          {op.phase !== "idle" && op.phase !== "accepted" && <OpResult op={op} />}
        </div>
      </div>
    </>
  );
}
