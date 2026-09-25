"use client";
// USSD & SIM Toolkit (new design). Tool page with two tabs. Nothing is read
// on entry; every request talks AT commands to the modem.
//
// Writes (controls-inventory §/router/stk, design §3.1) — all tier 2 inline
// confirms; none has a readback (the reply is the result):
//   发送 USSD      POST /api/ussd/send {code}      carrier services may be charged
//   回复           POST /api/ussd/respond {reply}
//   结束会话        POST /api/ussd/cancel {}
//   加载 / 重新加载  GET  /api/stk/menu             AT+CUSATD=1 etc. change the modem's STK state
//   菜单项          POST /api/stk/select {item_id}  may trigger a service on the SIM
// Time limits: /api/stk/menu runs up to ~18 s of AT waits (telephony.rs:
// 258-496), select ~13 s, USSD send/respond 8 s + setup — all get 30 s
// instead of the 8 s / 15 s defaults.
//
// Fixed vs the old page:
// - /api/ussd/respond answers 200 even when the modem replied ERROR (status
//   -1, response "…ERROR…", telephony.rs:430-441); that is now a failure.
// - USSD `status` is a number (0 done, 1 waiting for a reply, 2 ended by the
//   network, -1 no USSD reply); the page typed it as a string and printed
//   it raw. Shown as words now.
// - STK `reason`, `diagnostics` and `source` (sent, never shown) are shown.
// - A sub-menu with no items used to leave the page unchanged; it opens and
//   says it is empty, with Back.
// - The agent silently drops everything but digits and * # +; input with
//   other characters is refused with a message instead.
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, ArrowCounterClockwise, CaretLeft, ChatText, ListBullets, PaperPlaneRight, X } from "@phosphor-icons/react";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { useWriteOp, type WriteStep } from "@/lib/api/writeOp";
import type { StkItem, StkMenu, StkSelectResult, UssdResponse } from "@/lib/api/schemas/telephony";
import { Button, ConfirmInline, ConsoleBand, Group, OpResult, Row, StatusMark, useConfirmInline, type Tone } from "@/components/nd";
import { NdTabs } from "@/components/nd";

type TabId = "ussd" | "stk";
type T = (k: string, d: string, o?: Record<string, unknown>) => string;
const AT_TIMEOUT = 30000;
const USSD_CHARS = /^[0-9*#+]+$/;

interface Pending {
  key: string;
  action: string;
  consequence: ReactNode;
  step: WriteStep;
}

/** One op per panel; the pending request is captured when start() runs. */
function usePendingOp() {
  const [pending, setPending] = useState<Pending | null>(null);
  const inline = useConfirmInline(pending !== null);
  const op = useWriteOp({ tier: 2, steps: pending ? [pending.step] : [] });
  function go() {
    if (!pending) return;
    op.start();
    op.confirm();
    setPending(null);
  }
  const trig = (key: string) => (pending?.key === key ? inline.triggerProps : {});
  const confirm = (
    <ConfirmInline
      id={inline.id}
      open={pending !== null}
      actionLabel={pending?.action ?? ""}
      consequence={pending?.consequence}
      onCancel={() => setPending(null)}
      onConfirm={go}
    />
  );
  // "Accepted" adds nothing next to the reply itself; show every other phase.
  const result = op.phase !== "idle" && op.phase !== "accepted" ? <OpResult op={op} /> : null;
  return { op, pending, ask: (p: Pending) => !op.busy && setPending(p), cancel: () => setPending(null), trig, confirm, result };
}

function ussdStatus(t: T, s: number): { tone: Tone; text: string } {
  switch (s) {
    case 0:
      return { tone: "ok", text: t("stk.ussdDone", "Done") };
    case 1:
      return { tone: "ok", text: t("stk.ussdWaiting", "Waiting for your reply") };
    case 2:
      return { tone: "neutral", text: t("stk.ussdEnded", "Ended by the network") };
    case -1:
      return { tone: "warn", text: t("stk.ussdNoReply", "No USSD reply from the network") };
    default:
      return { tone: "neutral", text: t("stk.ussdStatusN", "Status {{s}}", { s }) };
  }
}

// ─── USSD ──────────────────────────────────────────────────────────────────

function USSDPanel() {
  const { t } = useTranslation();
  const codeId = useId();
  const replyId = useId();
  const [code, setCode] = useState("");
  const [reply, setReply] = useState("");
  const [response, setResponse] = useState<UssdResponse | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const p = usePendingOp();
  const busy = p.op.busy;

  function askSend() {
    const c = code.trim();
    if (!c) return setErr(t("stk.errEnterCode", "Enter a USSD code."));
    if (!USSD_CHARS.test(c)) return setErr(t("stk.errUssdChars", "Only digits and * # + can be sent."));
    setErr(null);
    p.ask({
      key: "send",
      action: t("stk.sendAction", "send {{code}}", { code: c }),
      consequence: t("stk.sendC", "The device sends {{code}} to your carrier. Some codes subscribe to or cancel services, and the carrier may charge for them.", { code: c }),
      step: {
        label: t("stk.send", "Send"),
        run: async () => {
          const d = await apiFetch<UssdResponse>("/api/ussd/send", { method: "POST", body: { code: c }, timeoutMs: AT_TIMEOUT });
          setResponse(d);
          return d;
        },
      },
    });
  }

  function askRespond() {
    const r = reply.trim();
    if (!r) return;
    if (!USSD_CHARS.test(r)) return setErr(t("stk.errUssdChars", "Only digits and * # + can be sent."));
    setErr(null);
    p.ask({
      key: "respond",
      action: t("stk.replyAction", "reply {{r}}", { r }),
      consequence: t("stk.replyC", "The device answers the carrier's menu with {{r}}. The carrier may charge for what it selects.", { r }),
      step: {
        label: t("stk.reply", "Reply"),
        run: async () => {
          const d = await apiFetch<UssdResponse>("/api/ussd/respond", { method: "POST", body: { reply: r }, timeoutMs: AT_TIMEOUT });
          // The agent passes an ERROR from the modem through as a reply.
          if (d.status === -1 && /ERROR/i.test(d.response)) {
            throw new ApiError(t("stk.respondError", "the modem answered ERROR; the session has probably ended"), 502);
          }
          setResponse(d);
          setReply("");
          return d;
        },
      },
    });
  }

  function askCancel() {
    p.ask({
      key: "cancel",
      action: t("stk.endSession", "End USSD session"),
      consequence: t("stk.cancelC", "The device ends this USSD session. To continue later, send the code again."),
      step: {
        label: t("stk.endSession", "End USSD session"),
        run: async () => {
          const d = await apiFetch("/api/ussd/cancel", { method: "POST", body: {} });
          setResponse(null);
          setReply("");
          return d;
        },
      },
    });
  }

  const st = response ? ussdStatus(t, response.status) : null;

  return (
    <section aria-labelledby="ussd-title" className="grid max-w-[720px] gap-4">
      <h2 id="ussd-title" className="sr-only">
        USSD
      </h2>
      <Group>
        <div className="nd-row flex-col items-stretch gap-2 py-3">
          <label htmlFor={codeId} className="nd-row__label">
            {t("stk.ussdCode", "USSD code")}
          </label>
          <div className="flex flex-wrap gap-2">
            <input
              id={codeId}
              className="nd-field nd-mono min-w-0 flex-1"
              inputMode="tel"
              autoComplete="off"
              placeholder="*#100#"
              value={code}
              disabled={busy}
              aria-invalid={err && !response?.session_active ? true : undefined}
              onChange={(e) => {
                setCode(e.target.value);
                setErr(null);
                if (p.pending?.key === "send") p.cancel();
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") askSend();
              }}
            />
            <span {...p.trig("send")}>
              <Button onPress={askSend} isDisabled={busy || !code.trim()} pending={busy}>
                <PaperPlaneRight size={20} weight="bold" aria-hidden />
                {t("stk.send", "Send")}
              </Button>
            </span>
          </div>
          <span className="nd-aux">{t("stk.ussdHint", "Digits and * # + only. The reply from your carrier appears below.")}</span>
        </div>
      </Group>

      {err && (
        <p role="alert" className="px-1 nd-error">
          {err}
        </p>
      )}
      {p.confirm}
      {p.result && <div className="px-1">{p.result}</div>}

      <section aria-labelledby="ussd-reply-title" aria-live="polite">
        <h3 id="ussd-reply-title" className="nd-group-title">
          {t("stk.replyTitle", "Reply")}
        </h3>
        <Group>
          {!response ? (
            <div className="nd-row text-nd-t2">{t("stk.noResultYet", "No result yet. Send a code to see the carrier's reply.")}</div>
          ) : (
            <>
              <div className="nd-row flex-col items-stretch gap-2 py-3">
                <p className="nd-body whitespace-pre-wrap break-words">{response.response || "—"}</p>
                <p className="nd-aux flex flex-wrap gap-x-3">
                  {st && <StatusMark tone={st.tone}>{st.text}</StatusMark>}
                  {response.session_active && <span>{t("stk.sessionActive", "Session active")}</span>}
                </p>
              </div>
              {response.session_active ? (
                <div className="nd-row flex-col items-stretch gap-2 py-3">
                  <label htmlFor={replyId} className="nd-row__label">
                    {t("stk.ussdReply", "USSD reply")}
                  </label>
                  <div className="flex flex-wrap gap-2">
                    <input
                      id={replyId}
                      className="nd-field nd-mono min-w-0 flex-1"
                      inputMode="tel"
                      autoComplete="off"
                      placeholder={t("stk.enterReply", "Enter reply…")}
                      value={reply}
                      disabled={busy}
                      onChange={(e) => {
                        setReply(e.target.value);
                        setErr(null);
                        if (p.pending?.key === "respond") p.cancel();
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") askRespond();
                      }}
                    />
                    <span {...p.trig("respond")}>
                      <Button onPress={askRespond} isDisabled={busy || !reply.trim()}>
                        {t("stk.reply", "Reply")}
                      </Button>
                    </span>
                    <span {...p.trig("cancel")}>
                      <Button variant="secondary" onPress={askCancel} isDisabled={busy} aria-label={t("stk.endSession", "End USSD session")}>
                        <X size={20} weight="bold" aria-hidden />
                        {t("stk.endShort", "End")}
                      </Button>
                    </span>
                  </div>
                </div>
              ) : (
                <div className="nd-row">
                  <Button
                    variant="secondary"
                    size="sm"
                    isDisabled={busy}
                    onPress={() => {
                      setResponse(null);
                      setCode("");
                      p.op.reset();
                    }}
                  >
                    <ArrowCounterClockwise size={18} weight="bold" aria-hidden />
                    {t("stk.newQuery", "New Query")}
                  </Button>
                </div>
              )}
            </>
          )}
        </Group>
      </section>
    </section>
  );
}

// ─── STK ───────────────────────────────────────────────────────────────────

interface Menu {
  title: string;
  items: StkItem[];
}

/** "+CUSATE: 0\"text\"OK" → "text"; null when there is no quoted text. */
function quoted(s: string): string | null {
  const m = s.match(/"([^"]+)"/);
  return m ? m[1] : null;
}

function STKPanel() {
  const { t } = useTranslation();
  const [menu, setMenu] = useState<Menu | null>(null);
  const [stack, setStack] = useState<Menu[]>([]);
  const [info, setInfo] = useState<StkMenu | null>(null);
  const [notSupported, setNotSupported] = useState<string | null>(null);
  const [display, setDisplay] = useState<string | null>(null);
  const p = usePendingOp();
  const busy = p.op.busy;
  // Read inside run(): the menu at the moment the item was chosen.
  const menuRef = useRef<Menu | null>(null);
  useEffect(() => {
    menuRef.current = menu;
  }, [menu]);

  function askLoad(key: "load" | "reload") {
    p.ask({
      key,
      action: key === "load" ? t("stk.loadMenu", "Load STK Menu") : t("stk.reloadMenu", "Reload menu"),
      consequence: t("stk.loadC", "The device asks the SIM card for its menu (AT+CUSATD, AT+STGI; up to about 20 s). This switches the modem's SIM Toolkit handling on; it does not order anything."),
      step: {
        label: t("stk.loadMenu", "Load STK Menu"),
        run: async () => {
          const d = await apiFetch<StkMenu>("/api/stk/menu", { timeoutMs: AT_TIMEOUT });
          setInfo(d);
          setDisplay(null);
          if (d.supported === false) {
            setNotSupported(d.reason ?? "");
            setMenu(null);
          } else {
            setNotSupported(null);
            setMenu({ title: d.title || t("stk.menu", "Menu"), items: d.items ?? [] });
          }
          setStack([]);
          return d;
        },
      },
    });
  }

  function askSelect(item: StkItem) {
    p.ask({
      key: `item-${item.id}`,
      action: t("stk.selectAction", "open {{label}}", { label: item.label }),
      consequence: t("stk.selectC", "The device selects “{{label}}” on the SIM card. Some entries send an SMS or subscribe to a service, and the carrier may charge for it.", {
        label: item.label,
      }),
      step: {
        label: item.label,
        run: async () => {
          const d = await apiFetch<StkSelectResult>("/api/stk/select", { method: "POST", body: { item_id: Number(item.id) }, timeoutMs: AT_TIMEOUT });
          if ("supported" in d && d.supported === false) {
            setNotSupported(d.reason ?? "");
            setMenu(null);
            setStack([]);
          } else if ("type" in d && d.type === "menu") {
            const parent = menuRef.current;
            if (parent) setStack((s) => [...s, parent]);
            setMenu({ title: d.title || t("stk.menu", "Menu"), items: d.items ?? [] });
            setDisplay(null);
          } else if ("type" in d && d.type === "display") {
            setDisplay(d.data || "");
          }
          return d;
        },
      },
    });
  }

  function goBack() {
    const prev = [...stack];
    const parent = prev.pop();
    if (parent) setMenu(parent);
    setStack(prev);
    setDisplay(null);
    p.cancel();
  }

  const crumbs = [...stack.map((m) => m.title), menu?.title].filter(Boolean) as string[];
  const shown = display !== null ? quoted(display) : null;

  const diag = info && (info.reason || info.source || (info.diagnostics?.length ?? 0) > 0) && (
    <ConsoleBand label={t("stk.diagnostics", "STK diagnostics")}>
      <pre className="max-h-[260px] overflow-auto whitespace-pre-wrap break-words">
        {[
          info.source ? `source: ${info.source}` : null,
          info.reason ? `reason: ${info.reason}` : null,
          ...(info.diagnostics ?? []),
        ]
          .filter(Boolean)
          .join("\n")}
      </pre>
    </ConsoleBand>
  );

  return (
    <section aria-labelledby="stk-title" className="grid max-w-[720px] gap-4">
      <h2 id="stk-title" className="sr-only">
        {t("stk.stkTitle", "SIM Toolkit (STK)")}
      </h2>

      {!menu && notSupported === null && (
        <Group>
          <div className="nd-row flex-col items-stretch gap-3 py-4">
            <p className="nd-body text-nd-t2">{t("stk.loadHint", "Load the SIM Toolkit menu to navigate carrier services.")}</p>
            <div>
              <span {...p.trig("load")}>
                <Button onPress={() => askLoad("load")} isDisabled={busy} pending={busy}>
                  <ListBullets size={20} weight="bold" aria-hidden />
                  {t("stk.loadMenu", "Load STK Menu")}
                </Button>
              </span>
            </div>
          </div>
        </Group>
      )}

      {notSupported !== null && (
        <Group>
          <div className="nd-row flex-wrap gap-3">
            <span className="flex-1">
              <StatusMark tone="neutral">{t("stk.notSupported", "STK is not supported by this SIM.")}</StatusMark>
            </span>
            <span {...p.trig("load")}>
              <Button variant="secondary" size="sm" onPress={() => askLoad("load")} isDisabled={busy}>
                {t("stk.tryAgain", "Try again")}
              </Button>
            </span>
          </div>
        </Group>
      )}

      {menu && (
        <div className="grid gap-2">
          {crumbs.length > 1 && (
            <nav aria-label={t("stk.breadcrumb", "Menu path")} className="nd-aux flex flex-wrap items-center gap-1 px-1">
              {crumbs.map((c, i) => (
                <span key={i} className="flex items-center gap-1">
                  {i > 0 && <span aria-hidden>/</span>}
                  <span className={i === crumbs.length - 1 ? "font-semibold text-nd-t1" : ""} aria-current={i === crumbs.length - 1 ? "page" : undefined}>
                    {c}
                  </span>
                </span>
              ))}
            </nav>
          )}
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="nd-body flex-1 font-semibold">{menu.title}</h3>
            {stack.length > 0 && (
              <Button variant="secondary" size="sm" onPress={goBack} isDisabled={busy}>
                <CaretLeft size={18} weight="bold" aria-hidden />
                {t("stk.back", "Back")}
              </Button>
            )}
            <span {...p.trig("reload")}>
              <Button variant="ghost" iconOnly onPress={() => askLoad("reload")} isDisabled={busy} aria-label={t("stk.reloadMenu", "Reload menu")}>
                <ArrowClockwise size={20} weight="bold" aria-hidden />
              </Button>
            </span>
          </div>
          <Group>
            {menu.items.length === 0 ? (
              <div className="nd-row text-nd-t2">
                {t("stk.noItems", "No items.")}
                {info?.reason && stack.length === 0 ? ` ${t("stk.noItemsReason", "The SIM has no menu waiting; try Reload in a moment.")}` : ""}
              </div>
            ) : (
              menu.items.map((item) => (
                <Row key={String(item.id)} label={item.label} onPress={busy ? undefined : () => askSelect(item)} />
              ))
            )}
          </Group>
        </div>
      )}

      {p.confirm}
      {p.result && <div className="px-1">{p.result}</div>}

      {display !== null && (
        <section aria-labelledby="stk-display-title" aria-live="polite" className="grid gap-2">
          <h3 id="stk-display-title" className="nd-group-title">
            {t("stk.simReply", "SIM reply")}
          </h3>
          <Group>
            <div className="nd-row">
              <p className="nd-body whitespace-pre-wrap break-words">{shown ?? (display ? display : t("stk.noResponse", "No response"))}</p>
            </div>
          </Group>
          {shown !== null && (
            <ConsoleBand label={t("stk.rawReply", "Raw modem reply")}>
              <pre className="whitespace-pre-wrap break-words">{display}</pre>
            </ConsoleBand>
          )}
        </section>
      )}

      {diag}
    </section>
  );
}

// ─── page ──────────────────────────────────────────────────────────────────

export default function STKPage() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<TabId>("ussd");
  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("stk.title", "USSD & SIM Toolkit")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("stk.desc", "Send USSD codes and navigate your SIM's STK menu.")}</p>
      <NdTabs<TabId>
        label={t("stk.tabsLabel", "USSD or STK")}
        value={tab}
        onChange={setTab}
        tabs={[
          {
            id: "ussd",
            label: (
              <>
                <ChatText size={18} weight={tab === "ussd" ? "fill" : "bold"} aria-hidden /> USSD
              </>
            ),
            content: <USSDPanel />,
          },
          {
            id: "stk",
            label: (
              <>
                <ListBullets size={18} weight={tab === "stk" ? "fill" : "bold"} aria-hidden /> {t("stk.stkMenuTab", "STK Menu")}
              </>
            ),
            content: <STKPanel />,
          },
        ]}
      />
    </>
  );
}
