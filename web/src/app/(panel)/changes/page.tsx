"use client";
// Change log (E4 T9b; write-op-layer.md DD5, DD10, DD11). Every setting
// changed through datad — from the touch screen, this page, a scene, a
// scheduled job — newest first, two lines each:
//   制式 · 自动 → 只用 4G
//   10-03 14:32 · 网页 · 已切到只用 4G
// All words are datad's (journal.list, STATE_V2.md V2-41); this page only
// lays them out. A row opens its detail; undo / redo lives there.
//
// Undo (DD10): only datad says whether a row can be undone (the latest change
// of that item, and not "nothing changed"); a row that can't shows why,
// greyed. Network mode cuts the mobile link → the tier-3 dialog (it says when
// you are connected remotely); anything else is tier 2. The request is sent
// as datad wrote it, with a fresh op_id so a resend never writes twice; then
// back to the list, and the bar at the top follows the new change.
//
// Empty: "No changes yet · …"; unreadable: "Can't read change log · data
// service not responding" with what was read before kept, greyed (DD11).
// An older datad has no change log: the page says so instead of asking.
// ?op=<op_id> opens a row's detail (the "last changed" link on settings pages).
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { errorText } from "@/lib/api/types";
import { useApi } from "@/lib/hooks/useApi";
import { useOps } from "@/lib/hooks/useOps";
import { pick, useLang } from "@/lib/i18n/pick";
import {
  CUTS_UPLINK,
  JOURNAL_LIMIT,
  JOURNAL_PATH,
  entryKey,
  entryLine2,
  markTone,
  noteMine,
  shortWhen,
  visibleEntries,
  type Journal,
  type JournalEntry,
} from "@/lib/ops";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  Row,
  StatusBlock,
  StatusMark,
  useConfirmInline,
} from "@/components/nd";

function rowLine1(e: JournalEntry, lang: "zh" | "en"): string {
  const what = pick(e.what_zh, e.what_en, lang);
  const change = pick(e.change_zh, e.change_en, lang);
  return change ? `${what} · ${change}` : what;
}

export default function ChangesPage() {
  const { t } = useTranslation();
  const lang = useLang();
  const ops = useOps();
  // an older datad (no write-op layer) has no change log: say so, don't ask
  const old = !!ops.data && !ops.supported && ops.data.datad === "up";
  const j = useApi<Journal>(ops.data && !old ? JOURNAL_PATH : null, { refreshInterval: 15000 });
  const rows = visibleEntries(j.data);
  // the open detail, by the entry's key: the list refetches newest-first, so a
  // position would point at another entry once a new change lands
  const [sel, setSel] = useState<{ key: string; snap: JournalEntry } | null>(null);

  // ?op=<op_id> from a settings page's "last changed" link
  const [wantOp, setWantOp] = useState<string | null>(null);
  useEffect(() => {
    setWantOp(new URLSearchParams(window.location.search).get("op"));
  }, []);
  useEffect(() => {
    if (!wantOp || !j.data) return;
    const e = rows.find((x) => x.op_id === wantOp);
    if (e) setSel({ key: entryKey(e), snap: e });
    setWantOp(null);
  }, [wantOp, j.data, rows]);

  const open = (e: JournalEntry | null) => {
    setSel(e ? { key: entryKey(e), snap: e } : null);
    const op = e?.op_id;
    const url = op ? `?op=${encodeURIComponent(op)}` : window.location.pathname;
    window.history.replaceState(null, "", url);
    window.scrollTo({ top: 0 });
  };

  const failed = !!j.error && !j.data;
  const stale = !!j.data && j.stale;

  if (sel) {
    // the current wording of that entry; gone from the latest 50 → what was
    // shown, with no undo (it can't be the latest change of its item any more)
    const cur = rows.find((e) => entryKey(e) === sel.key);
    const e = cur ?? { ...sel.snap, undo_view: null };
    return <Detail e={e} onBack={() => open(null)} stuck={ops.stuck} afterSend={() => { j.mutate(); ops.refresh(); open(null); }} />;
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("nav.changes", "Change log")}</h1>
      <div className="grid max-w-[720px] gap-6">
        {(failed || stale) && (
          <StatusBlock
            tone={failed ? "bad" : "stale"}
            state={t("changes.unreadable", "Can't read change log · data service not responding")}
            meta={stale ? <Freshness stale lastOkAt={j.lastOkAt} /> : undefined}
            actions={
              <Button variant="secondary" size="sm" onPress={() => j.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            }
          />
        )}
        {old && <StatusBlock tone="neutral" state={t("changes.oldDatad", "This data service has no change log yet")} reason={t("changes.oldDatadWhy", "It comes with the newer data service (zwrt-datad).")} />}
        {!old && !j.data && !j.error && <p className="nd-aux px-1">{t("changes.loading", "Reading the change log…")}</p>}
        {j.data && rows.length === 0 && (
          <StatusBlock tone="neutral" state={t("changes.empty", "No changes yet")} reason={t("changes.emptyWhy", "Settings changed from the touch screen, this page, scenes and scheduled jobs are all recorded here.")} />
        )}
        {rows.length > 0 && (
          <Group stale={stale}>
            {rows.map((e, i) => (
              <Row
                key={`${e.op_id ?? ""}-${i}`}
                label={<StatusMark tone={markTone(e.mark ?? null)}>{rowLine1(e, lang)}</StatusMark>}
                sub={entryLine2(e, lang)}
                onPress={() => open(e)}
              />
            ))}
          </Group>
        )}
        {j.data && rows.length > 0 && (
          <p className="nd-aux px-1">{t("changes.onlyLatest", "Latest {{n}} only", { n: JOURNAL_LIMIT })}</p>
        )}
      </div>
    </>
  );
}

function Detail({ e, onBack, afterSend, stuck }: { e: JournalEntry; onBack: () => void; afterSend: () => void; stuck: boolean }) {
  const { t } = useTranslation();
  const lang = useLang();
  const u = e.undo_view ?? null;
  const [asking, setAsking] = useState(false);
  const [sending, setSending] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const confirm = useConfirmInline(asking && !!u?.request && !CUTS_UPLINK.has(u.request.action));
  const label = u ? pick(u.label_zh, u.label_en, lang) : "";
  const why = u ? pick(u.why_zh, u.why_en, lang) : "";
  const tier3 = !!u?.request && CUTS_UPLINK.has(u.request.action);
  const what = pick(e.what_zh, e.what_en, lang);
  const change = pick(e.change_zh, e.change_en, lang);

  async function send() {
    if (!u?.request) return;
    setSending(true);
    setErr(null);
    const op_id = `web-undo-${Date.now()}`;
    noteMine(op_id);
    try {
      await apiFetch("/api/ops/write", { method: "POST", body: { op_id, request: u.request } });
      setAsking(false);
      afterSend();
    } catch (x) {
      setErr(errorText(x, lang));
    } finally {
      setSending(false);
    }
  }

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <Button variant="ghost" size="sm" onPress={onBack}>
          ‹ {t("nav.changes", "Change log")}
        </Button>
      </div>
      <div className="grid max-w-[720px] gap-6" data-testid="change-detail">
        <StatusBlock
          tone={markTone(e.mark ?? null)}
          state={pick(e.result_zh, e.result_en, lang)}
          reason={change ? `${what} · ${change}` : what}
          meta={[shortWhen(e.t), pick(e.source_zh, e.source_en, lang)].filter(Boolean).join(" · ")}
        />
        {u && (
          <section className="grid gap-2">
            <div className="nd-opbtns">
              <span {...(confirm.triggerProps)}>
                <Button
                  variant="secondary"
                  onPress={() => setAsking(true)}
                  isDisabled={!u.ok || !u.request || stuck || sending}
                >
                  {label}
                </Button>
              </span>
            </div>
            {!u.ok && why && <p className="nd-aux px-1">{why}</p>}
            {u.ok && stuck && <p className="nd-aux px-1">{t("ops.stuckReason", "Data service not responding · settings can't be changed for now")}</p>}
            {asking && !tier3 && (
              <ConfirmInline
                id={confirm.id}
                open
                consequence={change ? t("changes.undoWhat", "{{label}}: {{what}} · {{change}}", { label, what, change }) : label}
                actionLabel={label}
                pending={sending}
                onConfirm={send}
                onCancel={() => setAsking(false)}
              />
            )}
            {err && (
              <p className="nd-aux px-1" role="alert">
                {err}
              </p>
            )}
          </section>
        )}
      </div>
      {tier3 && (
        <ConfirmDialog
          open={asking}
          onOpenChange={setAsking}
          title={t("changes.undoTitle", "{{label}}: {{what}}?", { label, what })}
          what={change ? t("changes.undoChange", "This change is undone: {{change}}", { change }) : label}
          downtime={t("netmode.downtime", "The mobile connection drops for about 30 seconds while the modem re-attaches.")}
          recovery={t("netmode.recovery", "Come back to this page and choose “Auto”. If the page can't be reached, set the network mode back on the device's touchscreen.")}
          actionLabel={label}
          cutsUplink
          pending={sending}
          onConfirm={send}
        />
      )}
    </>
  );
}
