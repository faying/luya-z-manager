"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useTranslation } from "react-i18next";
import { ArrowLeft, Send } from "lucide-react";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input } from "@/components/admin/Button";

function toUCS2Hex(text: string): string {
  return Array.from(text)
    .map((ch) => ch.charCodeAt(0).toString(16).padStart(4, "0"))
    .join("");
}

function needsUCS2(text: string): boolean {
  for (const ch of text) {
    if (ch.charCodeAt(0) > 127) return true;
  }
  return false;
}

function getSMSTime(): string {
  const now = new Date();
  const pad = (n: number, l = 2) => String(n).padStart(l, "0");
  return (
    `${now.getFullYear()}` +
    `${pad(now.getMonth() + 1)}` +
    `${pad(now.getDate())}` +
    `${pad(now.getHours())}` +
    `${pad(now.getMinutes())}` +
    `${pad(now.getSeconds())}`
  );
}

export default function ComposePage() {
  const { t } = useTranslation();
  const router = useRouter();
  const [to, setTo] = useState("");
  const [body, setBody] = useState("");
  const [sending, setSending] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const charCount = body.length;
  const ucs2 = needsUCS2(body);
  // UCS2: max 70 chars per segment; ASCII: 160
  const segmentSize = ucs2 ? 70 : 160;
  const segments = body.length === 0 ? 0 : Math.ceil(body.length / segmentSize);

  async function handleSend() {
    const toTrimmed = to.trim();
    const bodyTrimmed = body.trim();
    if (!toTrimmed) { setErr(t("smscompose.errEnterPhone", "Please enter a phone number.")); return; }
    if (!bodyTrimmed) { setErr(t("smscompose.errEmptyMessage", "Message cannot be empty.")); return; }

    const encodeType = ucs2 ? "ucs2" : "gsm7";
    const encodedBody = ucs2 ? toUCS2Hex(bodyTrimmed) : bodyTrimmed;

    setSending(true);
    setErr(null);
    try {
      await apiFetch("/api/sms/send", {
        method: "POST",
        body: {
          number: toTrimmed,
          sms_time: getSMSTime(),
          message_body: encodedBody,
          id: "-1",
          encode_type: encodeType,
        },
      });
      router.push("/sms/");
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : String(e));
      setSending(false);
    }
  }

  return (
    <>
      <PageHeader
        title={t("smscompose.title", "Compose SMS")}
        description={t("smscompose.desc", "Send a new SMS message.")}
        actions={
          <Button variant="ghost" size="sm" onClick={() => router.push("/sms/")}>
            <ArrowLeft size={14} /> {t("smscompose.back", "Back")}
          </Button>
        }
      />

      <SectionCard className="max-w-lg">
        {err && <div className="mb-4"><ErrorBanner message={err} /></div>}

        <div className="space-y-4">
          <div>
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("smscompose.toLabel", "To (phone number)")}</label>
            <Input
              type="tel"
              placeholder="+1234567890"
              value={to}
              onChange={(e) => setTo(e.target.value)}
            />
          </div>

          <div>
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("smscompose.messageLabel", "Message")}</label>
            <textarea
              className="h-32 w-full rounded-md border border-border bg-bg-input px-3 py-2 text-sm outline-none transition focus:border-border-focus resize-none"
              placeholder={t("smscompose.messagePlaceholder", "Type your message…")}
              value={body}
              onChange={(e) => setBody(e.target.value)}
            />
            <div className="mt-1 flex items-center justify-between text-xs text-text-dim">
              <span>{segments !== 1
                ? t("smscompose.charsSegmentsPlural", "{{chars}} chars · {{segments}} segments", { chars: charCount, segments })
                : t("smscompose.charsSegments", "{{chars}} chars · {{segments}} segment", { chars: charCount, segments })}</span>
              {ucs2 && <span className="text-warning">{t("smscompose.ucs2Encoding", "Unicode (UCS2) encoding")}</span>}
            </div>
          </div>

          <div className="flex gap-2">
            <Button
              onClick={handleSend}
              loading={sending}
              disabled={!to.trim() || !body.trim()}
            >
              <Send size={14} /> {t("smscompose.send", "Send")}
            </Button>
            <Button variant="outline" onClick={() => router.push("/sms/")} disabled={sending}>
              {t("smscompose.cancel", "Cancel")}
            </Button>
          </div>
        </div>
      </SectionCard>
    </>
  );
}
