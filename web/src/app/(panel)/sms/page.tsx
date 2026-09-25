"use client";
// SMS inbox (new design). List page: status first (unread count, storage
// use), then the store filter and the actions on the selection, then the
// list. A row opens the message in a dialog.
//
// Reads: POST /api/sms/list is a read served over POST — kept. It is read
// once per store (mem_store "device" → NV, "sim" → SIM; the agent maps any
// other string to NV, so the old "All" filter only ever showed the device
// store). "All" is now both lists merged. GET /api/sms/capacity for storage.
//
// Writes (controls-inventory §/sms, design §3.1):
//   标为已读 (N)       tier 1  POST /api/sms/read {id:"1;2;", tag:0}   readback: those rows have tag "0"
//   open unread row   tier 1  same, one id, own op (so opening B while A's
//                             mark-read is in flight is queued, not dropped)
//   删除 (N)           tier 3  POST /api/sms/delete {id:"1;2;"}       readback: those rows are gone
//                             (was window.confirm)
//
// Fixed vs the old page (schemas/sms.ts):
// - `tag` is a string ("0" read, "1" unread, "2" sent, "3" failed, "4"
//   draft). The page compared `tag !== 0`, so every row looked unread and
//   sent messages got mark-read requests. Unread is now tag "1" only, and
//   only unread ids are sent to /api/sms/read.
// - Capacity: the page read sim_used/sim_total/device_used/device_total/
//   used/total, none of which the firmware sends, so the cards never showed.
//   Now per store: used = sms_<store>_{rev,send,draftbox}_total over
//   sms_<store>_total (B27 has no sms_simused_total, and sms_nvused_total is
//   unreliable there; `*used_total` is only a fallback — lib/sms.ts
//   storeUsage). The total is their sum.
// - "All" and "Device" showed the same list (see above).
import { useEffect, useRef, useState, type ReactNode } from "react";
import { useRouter } from "next/navigation";
import { useTranslation } from "react-i18next";
import { Modal } from "@heroui/react";
import { ArrowClockwise, EnvelopeOpen, NotePencil, Trash, X } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp } from "@/lib/api/writeOp";
import type { SmsCapacity, SmsList, SmsMessage } from "@/lib/api/schemas/sms";
import { decodeSms, formatSmsDate, storeUsage } from "@/lib/sms";
import {
  Button,
  ConfirmDialog,
  Freshness,
  OpResult,
  Segmented,
  StatusBlock,
  StatusMark,
  useToast,
  type Tone,
} from "@/components/nd";

type Filter = "all" | "sim" | "device";
type Store = "device" | "sim";

const LIST_BODY = (store: Store) => ({ page: 0, data_per_page: 500, mem_store: store, tags: 10, order_by: "order by id desc" });
const listKey = (store: Store) => `/api/sms/list?store=${store}`;
const POLL = 10000;

const isUnread = (m: SmsMessage) => String(m.tag) === "1";
const isRead = (m: SmsMessage) => String(m.tag) === "0";
const storeOf = (m: SmsMessage): Store => (m.mem_store === "sim" ? "sim" : "device");

type T = (k: string, d: string, o?: Record<string, unknown>) => string;

/** Word for a non-received tag (sent / failed / draft); null for read/unread. */
function tagWord(t: T, m: SmsMessage): { tone: Tone; text: string } | null {
  switch (String(m.tag)) {
    case "1":
      return { tone: "ok", text: t("sms.unreadWord", "Unread") };
    case "2":
      return { tone: "neutral", text: t("sms.sentWord", "Sent") };
    case "3":
      return { tone: "bad", text: t("sms.sendFailedWord", "Send failed") };
    case "4":
      return { tone: "neutral", text: t("sms.draftWord", "Draft") };
    default:
      return null;
  }
}

export default function SMSPage() {
  const { t } = useTranslation();
  const router = useRouter();
  const toast = useToast();

  const storeLabel = (s: Store | Filter) =>
    s === "all" ? t("sms.storeAll", "All") : s === "sim" ? t("sms.storeSim", "SIM") : t("sms.storeDevice", "Device");

  const [filter, setFilter] = useState<Filter>("all");
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [view, setView] = useState<SmsMessage | null>(null);

  const devApi = useApi<SmsList>(listKey("device"), { method: "POST", body: LIST_BODY("device"), refreshInterval: POLL });
  const simApi = useApi<SmsList>(listKey("sim"), { method: "POST", body: LIST_BODY("sim"), refreshInterval: POLL });
  const capApi = useApi<SmsCapacity>("/api/sms/capacity", { refreshInterval: POLL });

  const devMsgs = devApi.data?.messages;
  const simMsgs = simApi.data?.messages;
  const all: SmsMessage[] = (() => {
    const seen = new Set<number>();
    const out: SmsMessage[] = [];
    for (const m of [...(devMsgs ?? []), ...(simMsgs ?? [])]) {
      if (seen.has(m.id)) continue;
      seen.add(m.id);
      out.push(m);
    }
    return out.sort((a, b) => b.id - a.id);
  })();
  const messages = filter === "all" ? all : all.filter((m) => storeOf(m) === filter);
  const byId = new Map(all.map((m) => [m.id, m]));

  // Selection only counts rows that are still listed.
  const selectedMsgs = messages.filter((m) => selected.has(m.id));
  const selectedUnread = selectedMsgs.filter(isUnread);

  // ── readback: both stores, fresh, pushed into the SWR cache ──
  async function readAll(): Promise<SmsMessage[]> {
    const [d, s] = await Promise.all([
      apiFetch<SmsList>("/api/sms/list", { method: "POST", body: LIST_BODY("device") }),
      apiFetch<SmsList>("/api/sms/list", { method: "POST", body: LIST_BODY("sim") }),
    ]);
    await Promise.all([devApi.mutate(d, { revalidate: false }), simApi.mutate(s, { revalidate: false })]);
    void capApi.mutate();
    return [...(d?.messages ?? []), ...(s?.messages ?? [])];
  }
  const idString = (ids: number[]) => ids.join(";") + ";";

  // ── mark read (tier 1) ──
  const readIdsRef = useRef<number[]>([]);
  const readOp = useWriteOp({
    tier: 1,
    steps: [
      {
        label: t("sms.stepMarkRead", "Mark as read"),
        run: () => apiFetch("/api/sms/read", { method: "POST", body: { id: idString(readIdsRef.current), tag: 0 } }),
      },
    ],
    verify: async () => {
      const ids = new Set(readIdsRef.current);
      const rows = (await readAll()).filter((m) => ids.has(m.id));
      return rows.every(isRead);
    },
  });
  function markSelectedRead() {
    const ids = selectedUnread.map((m) => m.id);
    if (!ids.length || readOp.busy) return;
    readIdsRef.current = ids;
    readOp.start();
  }
  const readPhase = readOp.phase;
  const readCount = readIdsRef.current.length;
  useEffect(() => {
    if (readPhase === "applied" || readPhase === "accepted") {
      toast.show("ok", t("sms.markedRead", "Marked {{n}} as read", { n: readCount }));
      setSelected(new Set());
    }
    // toast/t are stable enough; only react to the phase change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [readPhase]);

  // ── auto mark-read when an unread message is opened (tier 1, queued) ──
  const autoQueue = useRef<Set<number>>(new Set());
  const autoIds = useRef<number[]>([]);
  const autoOp = useWriteOp({
    tier: 1,
    steps: [
      {
        label: t("sms.stepMarkRead", "Mark as read"),
        run: () => {
          // A re-send after login finds the queue empty: send the same ids again.
          if (autoQueue.current.size > 0) {
            autoIds.current = [...autoQueue.current];
            autoQueue.current.clear();
          }
          return apiFetch("/api/sms/read", { method: "POST", body: { id: idString(autoIds.current), tag: 0 } });
        },
      },
    ],
    verify: async () => {
      const ids = new Set(autoIds.current);
      return (await readAll()).filter((m) => ids.has(m.id)).every(isRead);
    },
  });
  const autoPhase = autoOp.phase;
  const autoBusy = autoOp.busy;
  useEffect(() => {
    if (autoPhase === "failed" || autoPhase === "unknown" || autoPhase === "partial") {
      toast.show("bad", t("sms.autoReadFailed", "Couldn't mark the message as read: {{e}}. Use “Mark read” to try again.", { e: autoOp.error ?? "" }));
    }
    // A message opened while the last request was in flight: send it now.
    if (!autoBusy && autoQueue.current.size > 0) {
      autoOp.reset();
      autoOp.start();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoPhase, autoBusy]);

  function openMsg(m: SmsMessage) {
    setView(m);
    if (!isUnread(m)) return;
    autoQueue.current.add(m.id);
    if (!autoOp.busy) autoOp.start();
  }

  // ── delete (tier 3) ──
  const delIdsRef = useRef<number[]>([]);
  const [askDelete, setAskDelete] = useState(false);
  const delOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("sms.stepDelete", "Delete messages"),
        run: () => apiFetch("/api/sms/delete", { method: "POST", body: { id: idString(delIdsRef.current) } }),
      },
    ],
    verify: async () => {
      const ids = new Set(delIdsRef.current);
      const gone = !(await readAll()).some((m) => ids.has(m.id));
      if (gone) setSelected(new Set());
      return gone;
    },
  });
  function openDelete() {
    if (!selectedMsgs.length || delOp.busy) return;
    delIdsRef.current = selectedMsgs.map((m) => m.id);
    setAskDelete(true);
  }
  function confirmDelete() {
    setAskDelete(false);
    delOp.start();
    delOp.confirm();
  }
  const delSenders = (() => {
    const nums = [...new Set(delIdsRef.current.map((id) => byId.get(id)?.number || t("sms.unknown", "Unknown")))];
    return nums.length > 3 ? `${nums.slice(0, 3).join("、")} …` : nums.join("、");
  })();

  const busy = readOp.busy || delOp.busy;

  // ── selection ──
  function toggle(id: number) {
    setSelected((prev) => {
      const n = new Set(prev);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  }
  const allSelected = messages.length > 0 && selectedMsgs.length === messages.length;
  function toggleAll() {
    setSelected(allSelected ? new Set() : new Set(messages.map((m) => m.id)));
  }

  // ── status ──
  const cap = capApi.data;
  const sim = storeUsage(cap, "sim");
  const dev = storeUsage(cap, "nv");
  const total = sim && dev ? { used: sim.used + dev.used, total: sim.total + dev.total } : null;
  const frac = (f: { used: number; total: number } | null) => (f ? `${f.used} / ${f.total}` : "—");
  const full = [sim && { name: storeLabel("sim"), ...sim }, dev && { name: storeLabel("device"), ...dev }].find(
    (x) => x && x.used >= x.total * 0.9,
  );

  const anyList = devMsgs !== undefined || simMsgs !== undefined;
  const bothFailed = !anyList && !!devApi.error && !!simApi.error;
  const unread = all.filter(isUnread).length;
  const stale = (devApi.data !== undefined && devApi.stale) || (simApi.data !== undefined && simApi.stale);
  const lastOkAt = Math.min(devApi.lastOkAt ?? Infinity, simApi.lastOkAt ?? Infinity);

  let tone: Tone = "neutral";
  let state: ReactNode = t("sms.loadingNd", "Reading messages…");
  let reason: ReactNode = null;
  if (bothFailed) {
    tone = "bad";
    state = t("sms.unreadable", "Can't read messages");
    reason = devApi.error?.message ?? simApi.error?.message;
  } else if (anyList) {
    if (full) {
      tone = "warn";
      state = t("sms.storeAlmostFull", "{{store}} message storage almost full", { store: full.name });
      reason = t("sms.storeFullReason", "{{used}} of {{total}} used. When it is full new messages can't be received; delete some old ones.", {
        used: full.used,
        total: full.total,
      });
    } else {
      tone = "ok";
      state = unread > 0 ? t("sms.unreadN", "{{n}} unread", { n: unread }) : t("sms.noUnread", "No unread messages");
    }
  }
  if (stale && anyList) tone = "stale";

  const capacityLine = (
    <span>
      {t("sms.simUsed", "SIM Used")} <span className="nd-mono">{frac(sim)}</span>
      {" · "}
      {t("sms.deviceUsed", "Device Used")} <span className="nd-mono">{frac(dev)}</span>
      {" · "}
      {t("sms.totalUsed", "Total Used")} <span className="nd-mono">{frac(total)}</span>
      {" · "}
      {t("sms.unread", "Unread")} <span className="nd-mono">{anyList ? unread : "—"}</span>
    </span>
  );

  function refreshLists() {
    void devApi.mutate();
    void simApi.mutate();
    void capApi.mutate();
  }

  const storeErr = (s: Store) => {
    const api = s === "sim" ? simApi : devApi;
    if (!api.error || (filter !== "all" && filter !== s)) return null;
    return (
      <div className="nd-row flex-wrap">
        <span className="flex-1 text-nd-t2">
          {t("sms.storeUnreadable", "Couldn't read the {{store}} messages: {{e}}", { store: storeLabel(s), e: api.error.message })}
        </span>
        <Button variant="secondary" size="sm" onPress={() => api.mutate()}>
          {t("common.retry", "Retry")}
        </Button>
      </div>
    );
  };

  const listLoading = filter === "all" ? !anyList && !bothFailed : (filter === "sim" ? simMsgs : devMsgs) === undefined && !(filter === "sim" ? simApi : devApi).error;

  return (
    <>
      <div className="mb-4 mt-2 flex flex-wrap items-center gap-3">
        <h1 className="nd-title flex-1">{t("sms.title", "SMS Inbox")}</h1>
        <Button onPress={() => router.push("/sms/compose")}>
          <NotePencil size={20} weight="bold" aria-hidden />
          {t("sms.compose", "Compose")}
        </Button>
      </div>

      <div className="grid max-w-[960px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={
            <>
              {reason && <span className="block">{reason}</span>}
              {capacityLine}
            </>
          }
          meta={stale ? <Freshness stale lastOkAt={Number.isFinite(lastOkAt) ? lastOkAt : null} what={t("sms.listWord", "List")} /> : undefined}
          actions={
            bothFailed ? (
              <Button variant="secondary" size="sm" onPress={refreshLists}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        <section aria-labelledby="sms-list-title">
          <h2 id="sms-list-title" className="sr-only">
            {t("sms.listTitle", "Messages")}
          </h2>
          {/* toolbar */}
          <div className="mb-3 flex flex-wrap items-center gap-2">
            <Segmented<Filter>
              label={t("sms.storeFilter", "Storage")}
              value={filter}
              onChange={(v) => {
                setFilter(v);
                setSelected(new Set());
              }}
              options={(["all", "sim", "device"] as const).map((id) => ({ id, label: storeLabel(id) }))}
            />
            <Button variant="ghost" iconOnly aria-label={t("sms.refresh", "Refresh messages")} onPress={refreshLists}>
              <ArrowClockwise size={20} weight="bold" aria-hidden />
            </Button>
            <div className="ml-auto flex flex-wrap items-center gap-2">
              {selectedMsgs.length > 0 && (
                <>
                  <Button variant="secondary" size="sm" onPress={markSelectedRead} isDisabled={busy || selectedUnread.length === 0} pending={readOp.busy}>
                    <EnvelopeOpen size={20} weight="bold" aria-hidden />
                    {t("sms.markRead", "Mark Read ({{count}})", { count: selectedUnread.length })}
                  </Button>
                  <Button variant="danger" size="sm" onPress={openDelete} isDisabled={busy} pending={delOp.busy}>
                    <Trash size={20} weight="bold" aria-hidden />
                    {t("sms.deleteN", "Delete ({{count}})", { count: selectedMsgs.length })}
                  </Button>
                </>
              )}
            </div>
          </div>
          {(readOp.phase !== "idle" && readOp.phase !== "applied" && readOp.phase !== "accepted") && (
            <div className="mb-3 px-1">
              <OpResult op={readOp} />
            </div>
          )}
          {delOp.phase !== "idle" && (
            <div className="mb-3 px-1">
              <OpResult op={delOp} />
            </div>
          )}

          <div className={`nd-group${stale ? " nd-stale" : ""}`}>
            {messages.length > 0 && (
              <div className="nd-row gap-2 py-0">
                <label className="flex min-h-[44px] cursor-pointer items-center gap-2">
                  <span className="flex h-11 w-11 shrink-0 items-center justify-center">
                    <input type="checkbox" className="h-5 w-5" checked={allSelected} onChange={toggleAll} />
                  </span>
                  <span className="nd-aux">
                    {t("sms.selectAll", "Select all")}
                    {selectedMsgs.length > 0 && ` · ${t("sms.selectedN", "{{n}} selected", { n: selectedMsgs.length })}`}
                  </span>
                </label>
              </div>
            )}
            {storeErr("device")}
            {storeErr("sim")}
            {listLoading ? (
              [0, 1, 2].map((i) => (
                <div key={i} className="nd-row nd-row--two" aria-hidden>
                  <span className="nd-skel" style={{ width: `${18 - i * 3}ch` }} />
                </div>
              ))
            ) : bothFailed ? null : messages.length === 0 ? (
              <div className="nd-row text-nd-t2">
                {filter === "sim"
                  ? t("sms.emptySim", "No messages on the SIM card.")
                  : filter === "device"
                    ? t("sms.emptyDevice", "No messages in device storage.")
                    : t("sms.emptyAll", "No messages yet. New ones show up here within 10 seconds.")}
              </div>
            ) : (
              messages.map((m) => {
                const from = m.number || t("sms.unknown", "Unknown");
                const unreadRow = isUnread(m);
                const word = tagWord(t, m);
                return (
                  <div key={m.id} className="nd-row nd-row--two items-start gap-2 py-2">
                    <label className="flex h-11 w-11 shrink-0 cursor-pointer items-center justify-center">
                      <input
                        type="checkbox"
                        className="h-5 w-5"
                        checked={selected.has(m.id)}
                        onChange={() => toggle(m.id)}
                        aria-label={t("sms.selectFrom", "Select the message from {{from}}", { from })}
                      />
                    </label>
                    <button
                      type="button"
                      className="grid min-w-0 flex-1 gap-0.5 rounded-nd-field py-1 text-start"
                      onClick={() => openMsg(m)}
                      aria-label={t("sms.openFrom", "Open the message from {{from}}", { from })}
                    >
                      <span className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5">
                        <span className={`nd-mono ${unreadRow ? "font-medium text-nd-t1" : "text-nd-t1"}`}>{from}</span>
                        {word && (
                          <span className="text-[14px]">
                            <StatusMark tone={word.tone}>{word.text}</StatusMark>
                          </span>
                        )}
                        <span className="nd-aux ml-auto">
                          <span className="nd-mono">{formatSmsDate(m.date) || "—"}</span>
                          {" · "}
                          {storeLabel(storeOf(m))}
                        </span>
                      </span>
                      <span className={`line-clamp-2 break-words text-[15px] leading-[22px] ${unreadRow ? "font-semibold text-nd-t1" : "text-nd-t2"}`}>
                        {decodeSms(m.content)}
                      </span>
                    </button>
                  </div>
                );
              })
            )}
          </div>
        </section>
      </div>

      {/* message detail */}
      <Modal.Backdrop isOpen={view !== null} onOpenChange={(o) => !o && setView(null)}>
        <Modal.Container placement="center">
          <Modal.Dialog className="nd nd-dialog">
            {view && (
              <>
                <Modal.Header className="flex items-start gap-3">
                  <div className="min-w-0 flex-1">
                    <Modal.Heading className="nd-title break-all">
                      <span className="sr-only">{t("sms.from", "From")} </span>
                      <span className="nd-mono">{view.number || t("sms.unknown", "Unknown")}</span>
                    </Modal.Heading>
                    <p className="nd-aux mt-1">
                      <span className="nd-mono">{formatSmsDate(view.date) || "—"}</span>
                      {" · "}
                      {storeLabel(storeOf(view))}
                      {" · "}
                      {(() => {
                        // The dialog's copy is from before the open; show the live tag.
                        const live = byId.get(view.id) ?? view;
                        const w = tagWord(t, live);
                        return isUnread(live) && autoOp.busy ? (
                          <StatusMark tone="neutral">{t("sms.markingRead", "Marking as read…")}</StatusMark>
                        ) : w ? (
                          <StatusMark tone={w.tone}>{w.text}</StatusMark>
                        ) : (
                          <StatusMark tone="ok">{t("sms.read", "Read")}</StatusMark>
                        );
                      })()}
                    </p>
                  </div>
                  <Button variant="ghost" iconOnly slot="close" aria-label={t("sms.close", "Close")}>
                    <X size={20} weight="bold" aria-hidden />
                  </Button>
                </Modal.Header>
                <Modal.Body>
                  <p className="nd-body whitespace-pre-wrap break-words">{decodeSms(view.content)}</p>
                </Modal.Body>
              </>
            )}
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>

      <ConfirmDialog
        open={askDelete}
        onOpenChange={(o) => !o && setAskDelete(false)}
        title={t("sms.confirmDeleteTitle", "Delete {{n}} message(s)?", { n: delIdsRef.current.length })}
        what={t("sms.confirmDeleteWhat", "{{n}} message(s) from {{from}} are deleted. This can't be undone.", {
          n: delIdsRef.current.length,
          from: delSenders,
        })}
        actionLabel={t("sms.deleteN", "Delete ({{count}})", { count: delIdsRef.current.length })}
        danger
        onConfirm={confirmDelete}
      />
    </>
  );
}
