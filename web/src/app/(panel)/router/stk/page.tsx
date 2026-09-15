"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronLeft, Send, MessageSquare, LayoutList, XCircle, RotateCcw } from "lucide-react";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input } from "@/components/admin/Button";

// ─── Types ───────────────────────────────────────────────────────────────────

interface USSDResponse {
  response: string;
  status: string;
  session_active: boolean;
}

interface STKMenuItem {
  id: string | number;
  label: string;
}

interface STKMenu {
  title: string;
  items: STKMenuItem[];
}

type Tab = "ussd" | "stk";

// ─── USSD Section ─────────────────────────────────────────────────────────────

function USSDSection() {
  const { t } = useTranslation();
  const [code, setCode] = useState("");
  const [reply, setReply] = useState("");
  const [response, setResponse] = useState<USSDResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function sendUSSD() {
    const trimmed = code.trim();
    if (!trimmed) { setErr(t("stk.errEnterCode", "Enter a USSD code.")); return; }
    setLoading(true);
    setErr(null);
    try {
      const data = await apiFetch<USSDResponse>("/api/ussd/send", {
        method: "POST",
        body: { code: trimmed },
      });
      setResponse(data);
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }

  async function respondUSSD() {
    const trimmed = reply.trim();
    if (!trimmed) return;
    setLoading(true);
    setErr(null);
    try {
      const data = await apiFetch<USSDResponse>("/api/ussd/respond", {
        method: "POST",
        body: { reply: trimmed },
      });
      setResponse(data);
      setReply("");
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }

  async function cancelUSSD() {
    setLoading(true);
    setErr(null);
    try {
      await apiFetch("/api/ussd/cancel", { method: "POST", body: {} });
      setResponse(null);
      setReply("");
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <SectionCard title="USSD" className="max-w-lg">
      {err && <div className="mb-3"><ErrorBanner message={err} /></div>}

      <div className="space-y-3">
        <div className="flex gap-2">
          <Input
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="*#100#"
            className="font-mono"
            onKeyDown={(e) => { if (e.key === "Enter") sendUSSD(); }}
          />
          <Button onClick={sendUSSD} loading={loading} disabled={!code.trim()}>
            <Send size={14} /> {t("stk.send", "Send")}
          </Button>
        </div>

        {response && (
          <div className="rounded-lg border border-border bg-bg-elevated p-4 space-y-2">
            <pre className="whitespace-pre-wrap text-sm font-mono">{response.response}</pre>
            <div className="flex items-center gap-2 text-xs text-text-dim">
              <span>{t("stk.status", "Status")}: <span className="font-mono">{response.status}</span></span>
              {response.session_active && (
                <span className="rounded-full bg-success/20 px-2 py-0.5 text-success text-xs">
                  {t("stk.sessionActive", "Session active")}
                </span>
              )}
            </div>
          </div>
        )}

        {response?.session_active && (
          <div className="flex gap-2">
            <Input
              value={reply}
              onChange={(e) => setReply(e.target.value)}
              placeholder={t("stk.enterReply", "Enter reply…")}
              className="font-mono"
              onKeyDown={(e) => { if (e.key === "Enter") respondUSSD(); }}
            />
            <Button onClick={respondUSSD} loading={loading} disabled={!reply.trim()}>
              {t("stk.reply", "Reply")}
            </Button>
            <Button variant="danger" size="md" onClick={cancelUSSD} disabled={loading}>
              <XCircle size={14} />
            </Button>
          </div>
        )}

        {response && !response.session_active && (
          <Button variant="outline" size="sm" onClick={() => { setResponse(null); setCode(""); }}>
            <RotateCcw size={13} /> {t("stk.newQuery", "New Query")}
          </Button>
        )}
      </div>
    </SectionCard>
  );
}

// ─── STK Section ──────────────────────────────────────────────────────────────

function STKSection() {
  const { t } = useTranslation();
  const [menu, setMenu] = useState<STKMenu | null>(null);
  const [menuStack, setMenuStack] = useState<STKMenu[]>([]);
  const [loading, setLoading] = useState(false);
  const [message, setMessage] = useState<{ text: string; isErr: boolean } | null>(null);
  const [notSupported, setNotSupported] = useState(false);

  async function loadMenu() {
    setLoading(true);
    setMessage(null);
    setNotSupported(false);
    try {
      const data = await apiFetch<Record<string, unknown>>("/api/stk/menu");
      if (data["supported"] === false) {
        setNotSupported(true);
      } else {
        const parsed = parseSTKMenu(data);
        setMenu(parsed);
        setMenuStack([]);
      }
    } catch (e) {
      setMessage({ text: e instanceof ApiError ? e.message : String(e), isErr: true });
    } finally {
      setLoading(false);
    }
  }

  async function selectItem(item: STKMenuItem) {
    setLoading(true);
    setMessage(null);
    try {
      const data = await apiFetch<Record<string, unknown>>("/api/stk/select", {
        method: "POST",
        body: { item_id: item.id },
      });
      if (data["supported"] === false) {
        setNotSupported(true);
      } else {
        const responseType = (data["type"] as string) ?? "";
        if (responseType === "menu") {
          const subMenu = parseSTKMenu(data);
          if (subMenu.items.length > 0 && menu) {
            setMenuStack((s) => [...s, menu]);
            setMenu(subMenu);
          }
        } else {
          const rawData = (data["data"] as string) ?? t("stk.noResponse", "No response");
          setMessage({ text: rawData, isErr: false });
        }
      }
    } catch (e) {
      setMessage({ text: e instanceof ApiError ? e.message : String(e), isErr: true });
    } finally {
      setLoading(false);
    }
  }

  function goBack() {
    setMenuStack((stack) => {
      const prev = [...stack];
      const parent = prev.pop();
      if (parent) setMenu(parent);
      return prev;
    });
  }

  function parseSTKMenu(data: Record<string, unknown>): STKMenu {
    const title = (data["title"] as string) ?? t("stk.menu", "Menu");
    const rawItems = (data["items"] as Array<Record<string, unknown>>) ?? [];
    const items: STKMenuItem[] = rawItems.map((item) => ({
      id: (item["id"] as string | number) ?? 0,
      label: (item["label"] as string) ?? String(item["id"]),
    }));
    return { title, items };
  }

  const breadcrumbs = [...menuStack.map((m) => m.title), menu?.title].filter(Boolean);

  return (
    <SectionCard title={t("stk.stkTitle", "SIM Toolkit (STK)")}>
      {!menu && !notSupported && (
        <div className="py-4 text-center space-y-3">
          <p className="text-sm text-text-dim">{t("stk.loadHint", "Load the SIM Toolkit menu to navigate carrier services.")}</p>
          <Button onClick={loadMenu} loading={loading}>
            <LayoutList size={14} /> {t("stk.loadMenu", "Load STK Menu")}
          </Button>
        </div>
      )}

      {notSupported && (
        <div className="py-4 text-center text-sm text-text-dim">{t("stk.notSupported", "STK is not supported by this SIM.")}</div>
      )}

      {message && (
        <div className="mb-3">
          {message.isErr ? (
            <ErrorBanner message={message.text} />
          ) : (
            <div className="rounded-lg border border-border bg-bg-elevated p-4">
              <pre className="whitespace-pre-wrap text-sm font-mono">{message.text}</pre>
            </div>
          )}
        </div>
      )}

      {menu && (
        <div className="space-y-3">
          {/* Breadcrumb */}
          {breadcrumbs.length > 1 && (
            <div className="flex items-center gap-1 text-xs text-text-dim overflow-x-auto">
              {breadcrumbs.map((crumb, i) => (
                <span key={i} className="flex items-center gap-1 shrink-0">
                  {i > 0 && <span>/</span>}
                  <span className={i === breadcrumbs.length - 1 ? "text-text font-medium" : ""}>{crumb}</span>
                </span>
              ))}
            </div>
          )}

          <div className="flex items-center justify-between">
            <h4 className="font-semibold">{menu.title}</h4>
            <div className="flex items-center gap-2">
              {menuStack.length > 0 && (
                <Button variant="outline" size="sm" onClick={goBack} disabled={loading}>
                  <ChevronLeft size={13} /> {t("stk.back", "Back")}
                </Button>
              )}
              <Button variant="ghost" size="sm" onClick={loadMenu} disabled={loading}>
                <RotateCcw size={13} />
              </Button>
            </div>
          </div>

          <div className="divide-y divide-border/50 rounded-lg border border-border">
            {menu.items.length === 0 ? (
              <div className="py-4 text-center text-sm text-text-dim">{t("stk.noItems", "No items.")}</div>
            ) : (
              menu.items.map((item) => (
                <button
                  key={String(item.id)}
                  onClick={() => selectItem(item)}
                  disabled={loading}
                  className="flex w-full items-center justify-between px-4 py-3 text-left text-sm transition hover:bg-bg-elevated disabled:opacity-50 first:rounded-t-lg last:rounded-b-lg"
                >
                  <span>{item.label}</span>
                  <span className="text-text-dim">›</span>
                </button>
              ))
            )}
          </div>
        </div>
      )}
    </SectionCard>
  );
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function STKPage() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("ussd");

  return (
    <>
      <PageHeader
        title={t("stk.title", "USSD & SIM Toolkit")}
        description={t("stk.desc", "Send USSD codes and navigate your SIM's STK menu.")}
      />

      <div className="mb-5 flex gap-1 rounded-lg border border-border bg-bg-card p-1 w-fit">
        <button
          onClick={() => setTab("ussd")}
          className={`flex items-center gap-1.5 rounded-md px-4 py-1.5 text-sm font-medium transition ${
            tab === "ussd" ? "bg-accent text-white" : "text-text-dim hover:text-text"
          }`}
        >
          <MessageSquare size={14} /> USSD
        </button>
        <button
          onClick={() => setTab("stk")}
          className={`flex items-center gap-1.5 rounded-md px-4 py-1.5 text-sm font-medium transition ${
            tab === "stk" ? "bg-accent text-white" : "text-text-dim hover:text-text"
          }`}
        >
          <LayoutList size={14} /> {t("stk.stkMenuTab", "STK Menu")}
        </button>
      </div>

      {tab === "ussd" && <USSDSection />}
      {tab === "stk" && <STKSection />}
    </>
  );
}
