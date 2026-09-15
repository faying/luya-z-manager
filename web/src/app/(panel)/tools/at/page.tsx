"use client";

import { useState, useRef, useEffect, useCallback } from "react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { Terminal, Send, ChevronUp, ChevronDown } from "lucide-react";
import { useTranslation } from "react-i18next";

type TFunc = (key: string, defaultValue: string, options?: Record<string, unknown>) => string;

interface PortInfo {
  port: string;
  available: boolean;
}

interface ATResponse {
  command: string;
  response: string;
  port: string;
  elapsed_ms: number;
}

interface HistoryEntry {
  command: string;
  response: string;
  elapsed_ms: number;
  ts: Date;
}

const DANGEROUS_PATTERN = /CFUN=0|\+CRESET|&F|\+NVWR|\+QPOWD|\+COPS=/i;

function getDangerReason(cmd: string, t: TFunc): string {
  if (/CFUN=0/i.test(cmd)) return t("atterm.dangerCfun0", "CFUN=0 will shut down the modem radio");
  if (/\+CRESET/i.test(cmd)) return t("atterm.dangerCreset", "+CRESET will reset the modem");
  if (/&F/i.test(cmd)) return t("atterm.dangerFactory", "&F will factory-reset modem settings");
  if (/\+NVWR/i.test(cmd)) return t("atterm.dangerNvwr", "+NVWR writes to non-volatile memory");
  if (/\+QPOWD/i.test(cmd)) return t("atterm.dangerQpowd", "+QPOWD will power down the modem");
  if (/\+COPS=/i.test(cmd)) return t("atterm.dangerCops", "+COPS= will change network operator registration");
  return t("atterm.dangerGeneric", "This command may be dangerous");
}

export default function ATTerminalPage() {
  const { t } = useTranslation();
  const { data: portInfo } = useApi<PortInfo>("/api/at/port");
  const [command, setCommand] = useState("");
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [historyIdx, setHistoryIdx] = useState(-1);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastElapsed, setLastElapsed] = useState<number | null>(null);
  const [inlineError, setInlineError] = useState<string | null>(null);
  const responseRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Scroll response area to bottom on new history
  useEffect(() => {
    if (responseRef.current) {
      responseRef.current.scrollTop = responseRef.current.scrollHeight;
    }
  }, [history]);

  const sendCommand = useCallback(async () => {
    const cmd = command.trim();
    if (!cmd) return;

    // Validate starts with AT
    if (!/^at/i.test(cmd)) {
      setInlineError(t("atterm.errorMustStartWithAt", "Command must start with AT"));
      return;
    }
    setInlineError(null);

    // Confirm dangerous
    if (DANGEROUS_PATTERN.test(cmd)) {
      const reason = getDangerReason(cmd, t);
      if (!window.confirm(t("atterm.confirmDangerous", "Warning: {{reason}}\n\nAre you sure you want to send: {{cmd}}", { reason, cmd }))) {
        return;
      }
    }

    setSending(true);
    setError(null);
    try {
      const result = await apiFetch<ATResponse>("/api/at/send", {
        method: "POST",
        body: { command: cmd, timeout: 3 },
      });
      setLastElapsed(result.elapsed_ms);
      setHistory((prev) => [
        ...prev,
        { command: result.command, response: result.response, elapsed_ms: result.elapsed_ms, ts: new Date() },
      ]);
      setCommand("");
      setHistoryIdx(-1);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
    } finally {
      setSending(false);
    }
  }, [command, t]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      sendCommand();
      return;
    }
    const cmds = history.map((h) => h.command);
    if (e.key === "ArrowUp") {
      e.preventDefault();
      const nextIdx = historyIdx < cmds.length - 1 ? historyIdx + 1 : historyIdx;
      setHistoryIdx(nextIdx);
      if (cmds.length > 0) {
        setCommand(cmds[cmds.length - 1 - nextIdx] ?? "");
      }
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      const nextIdx = historyIdx > 0 ? historyIdx - 1 : -1;
      setHistoryIdx(nextIdx);
      if (nextIdx === -1) {
        setCommand("");
      } else if (cmds.length > 0) {
        setCommand(cmds[cmds.length - 1 - nextIdx] ?? "");
      }
    }
  };

  return (
    <>
      <PageHeader
        title={t("atterm.title", "AT Terminal")}
        description={t("atterm.desc", "Send AT commands directly to the modem.")}
        actions={
          portInfo ? (
            <span
              className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium ${
                portInfo.available
                  ? "bg-success/15 text-success"
                  : "bg-error/15 text-error"
              }`}
            >
              <span
                className={`h-1.5 w-1.5 rounded-full ${portInfo.available ? "bg-success" : "bg-error"}`}
              />
              {portInfo.port} — {portInfo.available ? t("atterm.available", "available") : t("atterm.unavailable", "unavailable")}
            </span>
          ) : null
        }
      />

      {error && <ErrorBanner message={error} />}

      <div className="mt-4 grid gap-4 lg:grid-cols-2">
        {/* Left: input + history */}
        <SectionCard title={t("atterm.commandCard", "Command")}>
          <div className="flex flex-col gap-2">
            <div className="flex gap-2">
              <input
                ref={inputRef}
                value={command}
                onChange={(e) => {
                  setCommand(e.target.value);
                  setInlineError(null);
                }}
                onKeyDown={handleKeyDown}
                placeholder="AT+CGMR"
                className="h-9 w-full rounded-md border border-border bg-bg-input px-3 font-mono text-sm outline-none transition focus:border-accent"
                disabled={sending}
              />
              <Button onClick={sendCommand} loading={sending} disabled={sending || !command.trim()}>
                <Send className="h-4 w-4" />
                {t("atterm.send", "Send")}
              </Button>
            </div>
            {inlineError && <p className="text-xs text-error">{inlineError}</p>}
            <p className="text-xs text-text-dim">
              {t("atterm.recallHintPrefix", "Use")} <ChevronUp className="inline h-3 w-3" />/<ChevronDown className="inline h-3 w-3" /> {t("atterm.recallHintSuffix", "arrows to recall history.")}
            </p>

            {history.length > 0 && (
              <div className="mt-2">
                <p className="mb-1 text-xs font-medium text-text-dim uppercase tracking-wider">{t("atterm.history", "History")}</p>
                <div className="flex flex-col gap-1 max-h-48 overflow-y-auto">
                  {[...history].reverse().map((h, i) => (
                    <button
                      key={i}
                      onClick={() => {
                        setCommand(h.command);
                        setInlineError(null);
                        inputRef.current?.focus();
                      }}
                      className="rounded px-2 py-1 text-left text-xs font-mono text-text-dim hover:bg-bg-elevated transition"
                    >
                      {h.command}
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>
        </SectionCard>

        {/* Right: response */}
        <SectionCard title={t("atterm.responseCard", "Response")}>
          <div
            ref={responseRef}
            className="min-h-[200px] max-h-[400px] overflow-y-auto rounded-md bg-bg-elevated p-3 font-mono text-xs"
          >
            {history.length === 0 ? (
              <div className="flex h-full items-center justify-center text-text-dim">
                <Terminal className="mr-2 h-4 w-4" />
                {t("atterm.emptyResponse", "No commands sent yet")}
              </div>
            ) : (
              history.map((h, i) => (
                <div key={i} className="mb-3 last:mb-0">
                  <div className="text-accent">
                    &gt; {h.command}
                  </div>
                  <div className="whitespace-pre-wrap text-text">{h.response}</div>
                  <div className="mt-0.5 text-text-dim text-[10px]">{h.elapsed_ms}ms</div>
                </div>
              ))
            )}
          </div>
          {lastElapsed !== null && history.length > 0 && (
            <p className="mt-2 text-xs text-text-dim">
              {t("atterm.lastResponse", "Last response in {{ms}}ms", { ms: lastElapsed })}
            </p>
          )}
        </SectionCard>
      </div>
    </>
  );
}
