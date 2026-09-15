"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useTranslation } from "react-i18next";
import { Trash2, MailOpen, RefreshCw, PenSquare, X } from "lucide-react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, StatCard, ErrorBanner, Status } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { decodeSms, formatSmsDate } from "@/lib/sms";
import { useSWRConfig } from "swr";

type MemStore = "all" | "sim" | "device";

interface SMSMessage {
  id: number;
  number: string;
  content: string;
  date: string;
  tag: number;
  mem_store?: string;
}

interface SMSListResp {
  messages: SMSMessage[];
  total: number;
}

interface SMSCapacity {
  sim_used?: number;
  sim_total?: number;
  device_used?: number;
  device_total?: number;
  used?: number;
  total?: number;
}

export default function SMSPage() {
  const { t } = useTranslation();
  const router = useRouter();
  const { mutate } = useSWRConfig();
  const storeLabel: Record<MemStore, string> = {
    all: t("sms.storeAll", "All"),
    sim: t("sms.storeSim", "SIM"),
    device: t("sms.storeDevice", "Device"),
  };
  const [filter, setFilter] = useState<MemStore>("all");
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [viewMsg, setViewMsg] = useState<SMSMessage | null>(null);

  const listKey = `/api/sms/list?store=${filter}`;

  const { data: listData, isLoading: listLoading } = useApi<SMSListResp>(listKey, {
    method: "POST",
    body: { page: 0, data_per_page: 500, mem_store: filter, tags: 10, order_by: "order by id desc" },
    refreshInterval: 10000,
  });

  const { data: cap } = useApi<SMSCapacity>("/api/sms/capacity", { refreshInterval: 10000 });

  const messages = listData?.messages ?? [];

  function toggleSelect(id: number) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleAll() {
    if (selected.size === messages.length) {
      setSelected(new Set());
    } else {
      setSelected(new Set(messages.map((m) => m.id)));
    }
  }

  async function doAction(action: "delete" | "read", ids: number[]) {
    if (!ids.length) return;
    const idStr = ids.join(";") + ";";
    setBusy(true);
    setErr(null);
    try {
      if (action === "delete") {
        await apiFetch("/api/sms/delete", { method: "POST", body: { id: idStr } });
      } else {
        await apiFetch("/api/sms/read", { method: "POST", body: { id: idStr, tag: 0 } });
      }
      setSelected(new Set());
      mutate(listKey);
      mutate("/api/sms/capacity");
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  // Open the detail popup and, if the message is unread, mark it read in the
  // background (no page spinner, keeps any current selection intact).
  function openMsg(msg: SMSMessage) {
    setViewMsg(msg);
    if (msg.tag !== 0) {
      apiFetch("/api/sms/read", { method: "POST", body: { id: `${msg.id};`, tag: 0 } })
        .then(() => { mutate(listKey); mutate("/api/sms/capacity"); })
        .catch(() => { /* non-fatal: detail is still shown */ });
    }
  }

  const selectedIds = Array.from(selected);
  const unreadCount = messages.filter((m) => m.tag !== 0).length;

  return (
    <>
      <PageHeader
        title={t("sms.title", "SMS Inbox")}
        description={t("sms.desc", "View, manage and compose messages.")}
        actions={
          <Button onClick={() => router.push("/sms/compose")} size="md">
            <PenSquare size={15} /> {t("sms.compose", "Compose")}
          </Button>
        }
      />

      {/* Capacity */}
      <div className="mb-4 grid grid-cols-2 gap-3 sm:grid-cols-4">
        {cap?.sim_total != null && (
          <>
            <StatCard label={t("sms.simUsed", "SIM Used")} value={`${cap.sim_used ?? "—"} / ${cap.sim_total}`} />
          </>
        )}
        {cap?.device_total != null && (
          <StatCard label={t("sms.deviceUsed", "Device Used")} value={`${cap.device_used ?? "—"} / ${cap.device_total}`} />
        )}
        {cap?.total != null && (
          <StatCard label={t("sms.totalUsed", "Total Used")} value={`${cap.used ?? "—"} / ${cap.total}`} />
        )}
        <StatCard label={t("sms.unread", "Unread")} value={unreadCount} />
      </div>

      {err && <div className="mb-3"><ErrorBanner message={err} /></div>}

      <SectionCard>
        {/* Toolbar */}
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <div className="flex gap-1 rounded-lg border border-border p-0.5">
            {(["all", "sim", "device"] as MemStore[]).map((s) => (
              <button
                key={s}
                onClick={() => { setFilter(s); setSelected(new Set()); }}
                className={`rounded-md px-3 py-1 text-xs font-medium transition ${
                  filter === s ? "bg-accent text-white" : "text-text-dim hover:text-text"
                }`}
              >
                {storeLabel[s]}
              </button>
            ))}
          </div>

          <div className="ml-auto flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => mutate(listKey)}
              disabled={listLoading}
            >
              <RefreshCw size={13} />
            </Button>
            {selectedIds.length > 0 && (
              <>
                <Button
                  variant="outline"
                  size="sm"
                  loading={busy}
                  onClick={() => doAction("read", selectedIds)}
                >
                  <MailOpen size={13} /> {t("sms.markRead", "Mark Read ({{count}})", { count: selectedIds.length })}
                </Button>
                <Button
                  variant="danger"
                  size="sm"
                  loading={busy}
                  onClick={() => {
                    if (!confirm(t("sms.confirmDelete", "Delete {{count}} message(s)?", { count: selectedIds.length }))) return;
                    doAction("delete", selectedIds);
                  }}
                >
                  <Trash2 size={13} /> {t("sms.deleteN", "Delete ({{count}})", { count: selectedIds.length })}
                </Button>
              </>
            )}
          </div>
        </div>

        {/* Table */}
        {listLoading && messages.length === 0 ? (
          <div className="py-10 text-center text-sm text-text-dim">{t("sms.loading", "Loading…")}</div>
        ) : messages.length === 0 ? (
          <div className="py-10 text-center text-sm text-text-dim">{t("sms.noMessages", "No messages.")}</div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs text-text-dim">
                  <th className="pb-2 pr-3 font-medium">
                    <input
                      type="checkbox"
                      className="accent-accent"
                      checked={selected.size === messages.length && messages.length > 0}
                      onChange={toggleAll}
                    />
                  </th>
                  <th className="pb-2 pr-3 font-medium">{t("sms.from", "From")}</th>
                  <th className="pb-2 pr-3 font-medium">{t("sms.message", "Message")}</th>
                  <th className="pb-2 pr-3 font-medium">{t("sms.date", "Date")}</th>
                  <th className="pb-2 font-medium">{t("sms.store", "Store")}</th>
                </tr>
              </thead>
              <tbody>
                {messages.map((msg) => {
                  const isUnread = msg.tag !== 0;
                  return (
                    <tr
                      key={msg.id}
                      className="border-b border-border/50 last:border-0 hover:bg-bg-elevated/50"
                    >
                      <td className="py-2 pr-3">
                        <input
                          type="checkbox"
                          className="accent-accent"
                          checked={selected.has(msg.id)}
                          onChange={() => toggleSelect(msg.id)}
                        />
                      </td>
                      <td className="cursor-pointer py-2 pr-3" onClick={() => openMsg(msg)}>
                        <span className={`font-mono text-xs underline-offset-2 hover:underline ${isUnread ? "font-semibold text-accent" : "text-text"}`}>
                          {msg.number || t("sms.unknown", "Unknown")}
                        </span>
                      </td>
                      <td className="max-w-xs cursor-pointer py-2 pr-3" onClick={() => openMsg(msg)}>
                        <span className={`line-clamp-2 ${isUnread ? "font-medium text-text" : "text-text-dim"}`}>
                          {decodeSms(msg.content)}
                        </span>
                      </td>
                      <td className="whitespace-nowrap py-2 pr-3 font-mono text-xs text-text-dim">{formatSmsDate(msg.date)}</td>
                      <td className="py-2 text-xs text-text-dim">{msg.mem_store ?? "—"}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </SectionCard>

      {/* Message detail — opened from a row; auto-marks unread as read. */}
      {viewMsg && (
        <div
          className="fixed inset-0 z-[60] flex items-center justify-center bg-text/40 p-4 backdrop-blur-sm"
          onClick={() => setViewMsg(null)}
        >
          <div
            className="max-h-[calc(100vh-2rem)] w-full max-w-md overflow-y-auto rounded-2xl border border-border bg-bg-card p-6 shadow-lg"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="text-[10px] font-medium uppercase tracking-[0.12em] text-text-dim">
                  {t("sms.from", "From")}
                </div>
                <div className="mt-0.5 break-all font-mono text-[15px] font-semibold text-text">
                  {viewMsg.number || t("sms.unknown", "Unknown")}
                </div>
              </div>
              <button
                type="button"
                onClick={() => setViewMsg(null)}
                aria-label={t("sms.close", "Close")}
                className="-mr-1 -mt-1 shrink-0 rounded-md p-1 text-text-dim transition-colors hover:bg-bg hover:text-text"
              >
                <X size={16} />
              </button>
            </div>

            <div className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-text-dim">
              <span className="font-mono">{formatSmsDate(viewMsg.date)}</span>
              {viewMsg.mem_store && <><span className="opacity-50">·</span><span>{viewMsg.mem_store}</span></>}
              <Status tone="success">{t("sms.read", "Read")}</Status>
            </div>

            <div className="mt-4 whitespace-pre-wrap break-words rounded-lg border border-border/60 bg-bg-elevated/40 p-3 text-sm leading-relaxed text-text">
              {decodeSms(viewMsg.content)}
            </div>
          </div>
        </div>
      )}
    </>
  );
}
