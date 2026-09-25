"use client";
// AT terminal (tool page, new design). Order: port status → command →
// response (ConsoleBand, dark in both themes) → history.
//
//   Send, normal command     tier 1 (caller's rule: runs directly)   POST /api/at/send
//   Send, dangerous command  tier 3 ConfirmDialog with the command   POST /api/at/send
// No readback (R6): the modem's reply is the result, shown in the band.
//
// Dangerous = the old page's six patterns. at_terminal.rs strips ' ` $ ; | &
// before sending, so the check runs on the command as it will actually be
// sent (the old page only checked the raw text: `AT+CF;UN=0` slipped past).
//
// GET /api/at/port makes the agent send AT probes to the candidate ports,
// so it is read once on entry and when the user presses "Check again" —
// never polled, not on focus/reconnect. Each /api/at/send reply carries the
// port too and refreshes the cached value without another probe.
import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { ArrowClockwise, PaperPlaneRight } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp, type Tier } from "@/lib/api/writeOp";
import type { AtPort, AtSendBody, AtSendResult } from "@/lib/api/schemas/tools";
import { Button, ConfirmDialog, ConsoleBand, OpResult, StatusBlock, type Tone } from "@/components/nd";

interface HistoryEntry {
  id: number;
  command: string;
  response: string;
  elapsed_ms: number;
}

/** Same filter as at_terminal.rs:36-39. */
function sanitize(cmd: string): string {
  return cmd.replace(/['`$;|&]/g, "");
}

const DANGEROUS_PATTERN = /CFUN=0|\+CRESET|&F|\+NVWR|\+QPOWD|\+COPS=/i;

interface Danger {
  reason: string;
  recovery: string;
  /** Filled in by send(): the command as it will be sent. */
  command?: string;
}

/** Old page's list, checked against the raw input and the sent form. */
function dangerOf(raw: string, sent: string, t: TFunction): Danger | null {
  const hit = (re: RegExp) => re.test(raw) || re.test(sent);
  if (!DANGEROUS_PATTERN.test(raw) && !DANGEROUS_PATTERN.test(sent)) return null;
  if (hit(/CFUN=0/i))
    return {
      reason: t("atterm.dangerCfun0", "CFUN=0 will shut down the modem radio"),
      recovery: t("atterm.recoverCfun0", "Send AT+CFUN=1 to turn the radio back on, or restart the device."),
    };
  if (hit(/\+CRESET/i))
    return {
      reason: t("atterm.dangerCreset", "+CRESET will reset the modem"),
      recovery: t("atterm.recoverCreset", "The modem comes back by itself after about a minute. If it doesn't, restart the device."),
    };
  if (hit(/&F/i))
    return {
      reason: t("atterm.dangerFactory", "&F will factory-reset modem settings"),
      recovery: t("atterm.recoverFactory", "Modem settings changed with AT commands are lost and can't be restored from this page."),
    };
  if (hit(/\+NVWR/i))
    return {
      reason: t("atterm.dangerNvwr", "+NVWR writes to non-volatile memory"),
      recovery: t("atterm.recoverNvwr", "The old value can't be restored from this page. A wrong value can stop the modem from working until it is written back."),
    };
  if (hit(/\+QPOWD/i))
    return {
      reason: t("atterm.dangerQpowd", "+QPOWD will power down the modem"),
      recovery: t("atterm.recoverQpowd", "Restart the device to power the modem back up."),
    };
  if (hit(/\+COPS=/i))
    return {
      reason: t("atterm.dangerCops", "+COPS= will change network operator registration"),
      recovery: t("atterm.recoverCops", "Send AT+COPS=0 to go back to automatic operator selection."),
    };
  return {
    reason: t("atterm.dangerGeneric", "This command may be dangerous"),
    recovery: t("atterm.recoverGeneric", "Restart the device if the modem stops responding."),
  };
}

export default function ATTerminalPage() {
  const { t } = useTranslation();
  const port = useApi<AtPort>("/api/at/port", {
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
    revalidateIfStale: false,
  });

  const [command, setCommand] = useState("");
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [historyIdx, setHistoryIdx] = useState(-1);
  const [inlineError, setInlineError] = useState<string | null>(null);
  const [lastElapsed, setLastElapsed] = useState<number | null>(null);
  const [danger, setDanger] = useState<Danger | null>(null);
  const nextId = useRef(1);
  const responseRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // What start() sends; set right before start() (the op snapshots its config then).
  const sendRef = useRef<{ command: string; tier: Tier }>({ command: "", tier: 1 });

  // Getters: start() snapshots the config synchronously after send() set the
  // ref, before any re-render, so tier and label must be read lazily.
  const op = useWriteOp({
    get tier() {
      return sendRef.current.tier;
    },
    get steps() {
      return [
      {
        label: t("atterm.sendStep", "Send {{cmd}}", { cmd: sendRef.current.command }),
        run: async () => {
          const body: AtSendBody = { command: sendRef.current.command, timeout: 3 };
          const r = await apiFetch<AtSendResult>("/api/at/send", { method: "POST", body });
          setLastElapsed(r.elapsed_ms);
          setHistory((prev) => [
            ...prev,
            { id: nextId.current++, command: r.command, response: r.response, elapsed_ms: r.elapsed_ms },
          ]);
          setCommand((cur) => (cur.trim() === sendRef.current.command || sanitize(cur.trim()) === r.command ? "" : cur));
          setHistoryIdx(-1);
          if (r.port) void port.mutate({ port: r.port, available: true }, { revalidate: false });
          return r;
        },
      },
      ];
    },
  });

  // Scroll the response band to the newest reply.
  useEffect(() => {
    const el = responseRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [history]);

  const send = useCallback(() => {
    const raw = command.trim();
    if (!raw || op.busy) return;
    if (!/^at/i.test(raw)) {
      setInlineError(t("atterm.errorMustStartWithAt", "Command must start with AT"));
      return;
    }
    const sent = sanitize(raw);
    if (!sent) {
      setInlineError(t("atterm.errorEmptyAfterFilter", "Nothing is left after removing ' ` $ ; | &."));
      return;
    }
    setInlineError(null);
    const d = dangerOf(raw, sent, t);
    sendRef.current = { command: sent, tier: d ? 3 : 1 };
    if (d) {
      setDanger({ ...d, command: sent });
      return;
    }
    op.start();
  }, [command, op, t]);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      send();
      return;
    }
    const cmds = history.map((h) => h.command);
    if (e.key === "ArrowUp") {
      e.preventDefault();
      const nextIdx = historyIdx < cmds.length - 1 ? historyIdx + 1 : historyIdx;
      setHistoryIdx(nextIdx);
      if (cmds.length > 0) setCommand(cmds[cmds.length - 1 - nextIdx] ?? "");
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      const nextIdx = historyIdx > 0 ? historyIdx - 1 : -1;
      setHistoryIdx(nextIdx);
      setCommand(nextIdx === -1 ? "" : cmds[cmds.length - 1 - nextIdx] ?? "");
    }
  };

  // ── port status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("atterm.portReading", "Looking for the AT port…");
  let reason: ReactNode = null;
  if (port.data) {
    if (port.data.available && port.data.port) {
      tone = "ok";
      state = t("atterm.portAvailable", "AT port available");
      reason = <span className="nd-mono">{port.data.port}</span>;
    } else {
      tone = "bad";
      state = t("atterm.portNone", "No AT port answered");
      reason = t("atterm.portNoneReason", "None of the modem's AT ports replied OK. Commands will fail until one does. Check again in a moment.");
    }
  } else if (port.error) {
    tone = "warn";
    state = t("atterm.portUnread", "Couldn't check the AT port");
    reason = port.error instanceof Error ? port.error.message : String(port.error);
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("atterm.title", "AT Terminal")}</h1>

      <div className="grid max-w-[960px] gap-6">
        <p className="nd-body -mt-2 text-nd-t2">{t("atterm.desc", "Send AT commands directly to the modem.")}</p>

        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          actions={
            <Button variant="secondary" onPress={() => void port.mutate()} pending={port.isValidating}>
              <ArrowClockwise size={20} weight="bold" aria-hidden />
              {t("atterm.checkPort", "Check again")}
            </Button>
          }
        />

        {/* ── command + response ── */}
        <section aria-labelledby="at-cmd" className="grid gap-3">
          <h2 id="at-cmd" className="nd-group-title">
            {t("atterm.commandCard", "Command")}
          </h2>
          <ConsoleBand label={t("atterm.responseCard", "Response")}>
            <div
              ref={responseRef}
              className="max-h-[420px] min-h-[200px] overflow-y-auto"
              role="log"
              aria-live="polite"
              aria-label={t("atterm.responseCard", "Response")}
            >
              {history.length === 0 ? (
                <p className="nd-console__muted">{t("atterm.emptyResponse", "No commands sent yet")}</p>
              ) : (
                history.map((h) => (
                  <div key={h.id} className="mb-3 last:mb-0">
                    <div>&gt; {h.command}</div>
                    <div className="whitespace-pre-wrap break-all">{h.response.trim() || t("atterm.noReply", "(no reply)")}</div>
                    <div className="nd-console__muted text-[12px]">{h.elapsed_ms} ms</div>
                  </div>
                ))
              )}
            </div>
            <label className="mt-3 flex items-center gap-2 border-t border-nd-console-t2 pt-3">
              <span className="nd-console__muted" aria-hidden>
                &gt;
              </span>
              <span className="sr-only">{t("atterm.inputLabel", "AT command")}</span>
              <input
                ref={inputRef}
                value={command}
                onChange={(e) => {
                  setCommand(e.target.value);
                  setInlineError(null);
                }}
                onKeyDown={onKeyDown}
                placeholder="AT+CGMR"
                aria-describedby="at-hint"
                aria-invalid={inlineError ? true : undefined}
                autoComplete="off"
                autoCapitalize="characters"
                spellCheck={false}
                disabled={op.busy}
                className="min-h-[44px] w-full rounded-nd-field bg-transparent px-2 text-[16px] outline-hidden focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-nd-accT text-inherit"
              />
            </label>
          </ConsoleBand>

          <div className="flex flex-wrap items-center gap-3">
            <Button onPress={send} pending={op.busy} isDisabled={op.busy || !command.trim()}>
              <PaperPlaneRight size={20} weight="bold" aria-hidden />
              {t("atterm.send", "Send")}
            </Button>
            {lastElapsed !== null && history.length > 0 && (
              <span className="nd-aux">{t("atterm.lastResponse", "Last response in {{ms}}ms", { ms: lastElapsed })}</span>
            )}
          </div>
          <p id="at-hint" className="nd-aux">
            {t("atterm.recallHintNd", "Enter sends. ↑ / ↓ recall earlier commands.")}
          </p>
          {inlineError && (
            <p role="alert" className="text-nd-badT">
              {inlineError}
            </p>
          )}
          {op.phase !== "accepted" && op.phase !== "applied" && <OpResult op={op} />}
        </section>

        {/* ── history ── */}
        <section aria-labelledby="at-history">
          <h2 id="at-history" className="nd-group-title">
            {t("atterm.history", "History")}
          </h2>
          {history.length === 0 ? (
            <p className="nd-aux">{t("atterm.historyEmpty", "Commands you send here are listed for this visit; press one to put it back in the box.")}</p>
          ) : (
            <ul className="flex flex-wrap gap-2">
              {[...history].reverse().map((h) => (
                <li key={h.id}>
                  <Button
                    variant="secondary"
                    className="nd-mono"
                    aria-label={t("atterm.reuse", "Put {{cmd}} back in the box", { cmd: h.command })}
                    onPress={() => {
                      setCommand(h.command);
                      setInlineError(null);
                      inputRef.current?.focus();
                    }}
                  >
                    {h.command}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>
      </div>

      <ConfirmDialog
        open={danger !== null}
        onOpenChange={(o) => !o && setDanger(null)}
        title={t("atterm.confirmTitle", "Send a risky AT command?")}
        what={
          <>
            <span className="block">{danger?.reason}</span>
            <span className="nd-mono mt-2 block break-all">{danger?.command}</span>
          </>
        }
        downtime={t("atterm.confirmDowntime", "Depends on the command: the mobile connection may drop until the modem is back.")}
        recovery={danger?.recovery}
        actionLabel={t("atterm.confirmSend", "Send it")}
        cutsUplink
        danger
        onConfirm={() => {
          setDanger(null);
          op.start();
          op.confirm();
        }}
      />
    </>
  );
}
