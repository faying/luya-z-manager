"use client";
// eSIM (new design). Removable eUICC card (eSTK.me / 5ber) managed through
// lpac on the device. Settings page: status first (the profile in use, or
// what the card is busy with), then the profiles and their actions, adding a
// profile, pending notifications, and the chip details last.
//
// Writes (controls-inventory §/router/esim, design §3.1):
//   switch     tier 3   POST /api/esim/switch → job. The agent enables the
//                       profile, resets the modem and waits up to ~30 s for the
//                       new identity; if it doesn't converge the job ends
//                       "rebooting" and the device reboots 2 s later
//                       (esim.rs:436-449). Only then does the op wait ~90 s for
//                       the device (expectDown). Readback: the profile reads
//                       "enabled" — the job saying "done" is not proof.
//                       429 = the card's cooldown after the last switch attempt
//                       (eSTK.me answers catBusy otherwise); its message stays.
//   delete     tier 3   POST /api/esim/delete → job; type-to-confirm. Readback: gone.
//   download   tier 2   POST /api/esim/download → job. Readback: one more profile.
//   nickname   tier 2   POST /api/esim/nickname. Readback: profileNickname.
//   send notifications  tier 2   POST /api/esim/notifications/process → job.
//                       Readback: the list is empty.
// Jobs are polled on /api/esim/job (no card access) inside the op, so they
// keep going while the tab is in the background.
//
// Every other read runs lpac against the card: profiles every 10 s,
// notifications every 30 s, chip info only on load / refresh — same as the
// old page, and none of them poll in the background. While a job runs the
// agent answers profiles with `busy:true, profiles:null`; the last list is
// kept on screen.
import { useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, Check, DownloadSimple, PaperPlaneTilt, PencilSimple, Power, Trash } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { TimeoutError } from "@/lib/api/types";
import { useWriteOp, type WaitDeviceOptions, type WriteOpConfig } from "@/lib/api/writeOp";
import type { EsimJob, EsimJobStarted, EsimNotifications, EsimProfile, EsimProfiles, EsimStatus } from "@/lib/api/schemas/esim";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  useConfirmInline,
  type Tone,
} from "@/components/nd";

const PROFILE_POLL = 10000;
const NOTIF_POLL = 30000;

/** Start a job and poll /api/esim/job (no card access) until it ends. */
async function runJob(path: string, body: unknown, everyMs: number, limitMs: number): Promise<EsimJob> {
  const started = await apiFetch<EsimJobStarted>(path, { method: "POST", body });
  const until = Date.now() + limitMs;
  for (;;) {
    await new Promise((r) => setTimeout(r, everyMs));
    const j = await apiFetch<EsimJob>("/api/esim/job");
    if (j.id === started.job_id && j.status === "done") return j;
    if (j.id === started.job_id && j.status === "error") throw new Error(j.message || "job failed");
    if (Date.now() > until) throw new TimeoutError(limitMs);
  }
}

/** Profiles as the card reports them; a busy card (job running) is not an answer yet. */
async function readProfiles(): Promise<EsimProfile[]> {
  const d = await apiFetch<EsimProfiles>("/api/esim/profiles");
  if (d.busy || !d.profiles) throw new Error("card busy");
  return d.profiles;
}

const nameOf = (p: EsimProfile) => p.profileNickname || p.serviceProviderName || p.profileName || p.iccid;

function fmtBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}

type Inline = null | "nick" | "download" | "notif";

export default function EsimPage() {
  const { t } = useTranslation();
  const chip = useApi<EsimStatus>("/api/esim/status");
  const installed = chip.data?.installed === true;
  const prof = useApi<EsimProfiles>(installed ? "/api/esim/profiles" : null, { refreshInterval: PROFILE_POLL });
  const notif = useApi<EsimNotifications>(installed ? "/api/esim/notifications" : null, { refreshInterval: NOTIF_POLL });

  // Keep the last real list while the card is busy (profiles:null).
  const fresh = prof.data?.profiles ?? null;
  const [kept, setKept] = useState<EsimProfile[] | null>(null);
  if (fresh && fresh !== kept) setKept(fresh);
  const list = fresh ?? kept;
  const cardBusy = prof.data?.busy === true;
  const active = list?.find((p) => p.profileState === "enabled") ?? null;

  const saveList = async (profiles: EsimProfile[]) => {
    await prof.mutate({ installed: true, busy: false, profiles }, { revalidate: false });
  };

  // ── switch (tier 3; may reboot) ──
  const [target, setTarget] = useState<EsimProfile | null>(null);
  const [dialog, setDialog] = useState<null | "switch" | "delete">(null);
  // Set by the switch step from the finished job, read by the controller
  // after the step resolves (it reads `waitDevice` lazily): a reboot only
  // becomes known once the job ends.
  const rebootRef = useRef(false);
  const switchRecovery = t(
    "esim.switchRecovery",
    "Give the device a few minutes to finish restarting. If the new profile has no service, switch back to “{{name}}” here or on the touchscreen's eSIM page.",
    { name: active ? nameOf(active) : "—" }
  );
  // The old page waited without a limit; give a slow boot plenty of room.
  const rebootWait: WaitDeviceOptions = { expectedSec: 90, timeoutSec: 300, expectDown: true, recovery: switchRecovery };
  const switchCfg: WriteOpConfig = {
    tier: 3,
    steps: [
      {
        label: t("esim.switch", "Switch"),
        run: async () => {
          const j = await runJob("/api/esim/switch", { iccid: target?.iccid }, 1500, 120000);
          rebootRef.current = j.rebooting;
        },
      },
    ],
    get waitDevice() {
      return rebootRef.current ? rebootWait : undefined;
    },
    verify: async () => {
      const ps = await readProfiles();
      await saveList(ps);
      return ps.find((p) => p.iccid === target?.iccid)?.profileState === "enabled";
    },
    // If the job poll itself drops because the reboot started first, keep
    // re-checking long enough to cover the reboot.
    reconfirmSec: 300,
  };
  const switchOp = useWriteOp(switchCfg);

  // ── delete (tier 3, type to confirm) ──
  const deleteOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("common.delete", "Delete"),
        run: () => runJob("/api/esim/delete", { iccid: target?.iccid }, 1500, 120000),
      },
    ],
    verify: async () => {
      const ps = await readProfiles();
      await saveList(ps);
      return !ps.some((p) => p.iccid === target?.iccid);
    },
  });

  // ── tier-2 actions (inline confirm) ──
  const [inline, setInline] = useState<Inline>(null);
  const inlineCtl = useConfirmInline(inline !== null);
  const cancelInline = () => setInline(null);

  // nickname
  const [edit, setEdit] = useState<{ iccid: string; nickname: string } | null>(null);
  const [nickReq, setNickReq] = useState<{ iccid: string; nickname: string; name: string } | null>(null);
  const nickOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("esim.rename", "Rename"),
        run: () => apiFetch("/api/esim/nickname", { method: "POST", body: { iccid: nickReq?.iccid, nickname: nickReq?.nickname } }),
      },
    ],
    verify: async () => {
      const ps = await readProfiles();
      await saveList(ps);
      return (ps.find((p) => p.iccid === nickReq?.iccid)?.profileNickname ?? "") === nickReq?.nickname;
    },
  });
  function askNickname(p: EsimProfile) {
    if (!edit || edit.iccid !== p.iccid) return;
    const nick = edit.nickname.trim();
    if (nick === (p.profileNickname ?? "")) {
      setEdit(null);
      return;
    }
    setNickReq({ iccid: p.iccid, nickname: nick, name: nameOf(p) });
    setInline("nick");
  }

  // download
  const [code, setCode] = useState("");
  const [confCode, setConfCode] = useState("");
  const [codeErr, setCodeErr] = useState<string | null>(null);
  const [dlReq, setDlReq] = useState<{ code: string; confirmation_code?: string; before: number } | null>(null);
  const downloadOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("esim.download", "Download"),
        // Downloads can take minutes (SM-DP+ over the mobile link).
        run: () => runJob("/api/esim/download", { code: dlReq?.code, confirmation_code: dlReq?.confirmation_code }, 1500, 600000),
      },
    ],
    verify: async () => {
      const ps = await readProfiles();
      await saveList(ps);
      return ps.length > (dlReq?.before ?? 0);
    },
  });
  function askDownload() {
    setCodeErr(null);
    const c = code.trim();
    if (!/^LPA:1\$/i.test(c) && !/^1\$/.test(c)) {
      setCodeErr(t("esim.badCode", "Enter an activation code like LPA:1$smdp.example.com$XXXX-XXXX"));
      return;
    }
    setDlReq({ code: c, confirmation_code: confCode.trim() || undefined, before: list?.length ?? 0 });
    setInline("download");
  }

  // notifications
  const notifList = notif.data ?? [];
  const notifOp = useWriteOp({
    tier: 2,
    steps: [
      {
        label: t("esim.processAll", "Send all"),
        run: () => runJob("/api/esim/notifications/process", {}, 2000, 120000),
      },
    ],
    verify: async () => {
      const d = await apiFetch<EsimNotifications>("/api/esim/notifications");
      if (d === null) throw new Error("card busy");
      await notif.mutate(d, { revalidate: false });
      return d.length === 0;
    },
  });

  function goInline() {
    const which = inline;
    setInline(null);
    if (which === "nick") {
      nickOp.start();
      nickOp.confirm();
    } else if (which === "download") {
      downloadOp.start();
      downloadOp.confirm();
    } else if (which === "notif") {
      notifOp.start();
      notifOp.confirm();
    }
  }
  function goDialog() {
    const which = dialog;
    setDialog(null);
    if (which === "switch") {
      rebootRef.current = false;
      switchOp.start();
      switchOp.confirm();
    } else if (which === "delete") {
      deleteOp.start();
      deleteOp.confirm();
    }
  }

  // Clear the download inputs once the card took the profile.
  const [clearedRun, setClearedRun] = useState(0);
  if ((downloadOp.phase === "applied" || downloadOp.phase === "accepted") && downloadOp.runId !== clearedRun) {
    setClearedRun(downloadOp.runId);
    setCode("");
    setConfCode("");
  }

  // Leave the rename box once the card shows the new nickname; keep the text otherwise.
  const [nickClosed, setNickClosed] = useState(0);
  if (nickOp.phase === "applied" && nickOp.runId !== nickClosed) {
    setNickClosed(nickOp.runId);
    setEdit(null);
  }

  const anyBusy = switchOp.busy || deleteOp.busy || nickOp.busy || downloadOp.busy || notifOp.busy;
  const busy = anyBusy || cardBusy;
  const locked = !list || prof.stale || busy;
  const rebooting = switchOp.phase === "waitDevice";

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("esim.loadingNd", "Reading the eUICC card…");
  let reason: ReactNode = null;
  let meta: ReactNode;
  const chipErr = !chip.data && chip.error;
  if (chipErr) {
    tone = "bad";
    state = t("esim.unreadable", "Can't read the eUICC card");
    reason = t("esim.loadFailed", "Couldn't read the eUICC: {{msg}}", { msg: chip.error?.message ?? "unknown" });
  } else if (chip.data && !installed) {
    state = t("esim.notInstalledTitle", "lpac isn't deployed on the device");
    reason = t(
      "esim.notInstalledDesc",
      "The eSIM manager needs the lpac bundle under /data/esim. Run scripts/esim/build-esim-bundle.sh from the repo, then refresh."
    );
  } else if (rebooting) {
    state = t("esim.rebooting", "Rebooting…");
    reason = t("esim.rebootBannerNd", "The device is rebooting onto the new profile. This page reconnects by itself (about 90 s, up to 5 min).");
  } else if (busy) {
    state = switchOp.busy
      ? t("esim.switching", "Switching…")
      : deleteOp.busy
        ? t("esim.deleting", "Deleting…")
        : downloadOp.busy
          ? t("esim.downloading", "Downloading profile…")
          : t("esim.working", "Working…");
    reason = t("esim.busyReason", "The card handles one operation at a time; the other actions wait until this one ends.");
  } else if (installed && !list && prof.error) {
    tone = "bad";
    state = t("esim.profilesUnreadable", "Can't read the profiles");
    reason = t("esim.profilesLoadFailed", "Couldn't load profiles: {{msg}}", { msg: prof.error.message });
  } else if (list) {
    if (active) {
      tone = "ok";
      state = t("esim.inUse", "In use: {{name}}", { name: nameOf(active) });
      reason = [active.serviceProviderName, active.profileName].filter(Boolean).join(" · ");
      meta = <span className="nd-mono">{active.iccid}</span>;
    } else {
      tone = "warn";
      state = t("esim.noneActive", "No profile is enabled");
      reason = list.length
        ? t("esim.noneActiveNext", "Switch to one of the profiles below to get mobile service.")
        : t("esim.noProfiles", "No profiles on the card yet — add one below.");
    }
  }
  if (list && prof.stale && !busy) {
    tone = "stale";
    meta = (
      <>
        <Freshness stale lastOkAt={prof.lastOkAt} what={t("esim.listWord", "List")} />
        {t("esim.refreshToEdit", " — refresh before changing anything.")}
      </>
    );
  }

  const refresh = () => {
    void chip.mutate();
    if (installed) void prof.mutate();
  };

  // 429 cooldown (eSTK.me catBusy) and 409 (another job) get a plain explanation.
  const switchErr = switchOp.phase === "failed" ? switchOp.error ?? "" : "";
  const cooldown = /cooldown/i.test(switchErr) ? switchErr.match(/wait (\d+)s/) : null;
  const jobClash = (e?: string) => /in progress/i.test(e ?? "");

  const tgtName = target ? nameOf(target) : "";
  const deleteWord = t("esim.deleteWord", "delete");

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">{t("esim.title", "eSIM")}</h1>
        <Button variant="ghost" iconOnly onPress={refresh} isDisabled={chip.isLoading} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>

      <div className="grid max-w-[720px] gap-6">
        <p className="nd-body -mt-2 text-nd-t2">
          {t("esim.desc", "Manage profiles on a removable eUICC card (eSTK.me / 5ber and similar) in the SIM slot.")}
        </p>

        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={meta}
          actions={
            chipErr || (installed && !list && prof.error) || (list && prof.stale && !busy) ? (
              <Button variant="secondary" size="sm" onPress={refresh}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* The switch result stays mounted whatever the card reads (reboot). */}
        {switchOp.phase !== "idle" && switchOp.phase !== "confirming" && (
          <div className="grid gap-2 px-1">
            <OpResult op={switchOp} />
            {cooldown && (
              <p className="nd-aux" role="note">
                {t(
                  "esim.cooldownNd",
                  "The card needs a pause after each switch attempt (eSTK.me cards answer “catBusy” otherwise). Try again in {{s}} s.",
                  { s: cooldown[1] }
                )}
              </p>
            )}
            {jobClash(switchErr) && <p className="nd-aux">{t("esim.jobClash", "Another eSIM operation is still running. Wait for it to finish, then try again.")}</p>}
          </div>
        )}

        {installed && (
          <>
            {/* ── profiles ── */}
            <section>
              <GroupTitle>{t("esim.profiles", "Profiles")}</GroupTitle>
              <p className="nd-aux -mt-1 mb-3 px-1">
                {t(
                  "esim.profilesDescNd",
                  "Switching re-attaches the modem on the new profile: usually about 40 s without a reboot. If the new profile doesn't take, the device reboots (about 90 s)."
                )}
              </p>
              <div className={`nd-group${prof.stale ? " nd-stale" : ""}`}>
                {!list ? (
                  prof.error ? (
                    <div className="nd-row text-nd-t2">{t("esim.profilesUnreadable", "Can't read the profiles")}</div>
                  ) : (
                    <>
                      <div className="nd-row"><span className="nd-skel" style={{ width: "14ch" }} /></div>
                      <div className="nd-row"><span className="nd-skel" style={{ width: "10ch" }} /></div>
                    </>
                  )
                ) : list.length === 0 ? (
                  <div className="nd-row text-nd-t2">{t("esim.noProfiles", "No profiles on the card yet — add one below.")}</div>
                ) : (
                  list.map((p) => {
                    const on = p.profileState === "enabled";
                    const editing = edit?.iccid === p.iccid;
                    const name = nameOf(p);
                    return (
                      <Row
                        key={p.iccid}
                        label={
                          editing ? (
                            <input
                              className="nd-field"
                              value={edit.nickname}
                              aria-label={t("esim.nicknameFor", "Nickname for {{name}}", { name })}
                              placeholder={p.serviceProviderName}
                              maxLength={64}
                              autoFocus
                              onChange={(e) => setEdit({ iccid: p.iccid, nickname: e.target.value })}
                              onKeyDown={(e) => {
                                if (e.key === "Enter") askNickname(p);
                                if (e.key === "Escape") setEdit(null);
                              }}
                            />
                          ) : (
                            <span className="inline-flex flex-wrap items-center gap-x-3">
                              <span>{name}</span>
                              {on && <StatusMark tone="ok">{t("esim.active", "Active")}</StatusMark>}
                            </span>
                          )
                        }
                        sub={
                          <>
                            {[p.serviceProviderName, p.profileName].filter(Boolean).join(" · ")}
                            {" · "}
                            <span className="nd-mono">{p.iccid}</span>
                          </>
                        }
                        control={
                          <span className="flex shrink-0 items-center gap-1">
                            {editing ? (
                              <span {...(inline === "nick" ? inlineCtl.triggerProps : {})}>
                                <Button variant="secondary" size="sm" isDisabled={locked} onPress={() => askNickname(p)}>
                                  <Check size={20} weight="bold" aria-hidden />
                                  {t("common.save", "Save")}
                                </Button>
                              </span>
                            ) : (
                              <Button
                                variant="ghost"
                                iconOnly
                                isDisabled={locked}
                                aria-label={t("esim.renameAria", "Rename {{name}}", { name })}
                                onPress={() => setEdit({ iccid: p.iccid, nickname: p.profileNickname ?? "" })}
                              >
                                <PencilSimple size={20} weight="bold" aria-hidden />
                              </Button>
                            )}
                            {!on && (
                              <>
                                <Button
                                  variant="secondary"
                                  size="sm"
                                  isDisabled={locked}
                                  aria-label={t("esim.switchAria", "Switch to {{name}}", { name })}
                                  onPress={() => {
                                    setTarget(p);
                                    setDialog("switch");
                                  }}
                                >
                                  <Power size={20} weight="bold" aria-hidden />
                                  {t("esim.switch", "Switch")}
                                </Button>
                                <Button
                                  variant="ghost"
                                  iconOnly
                                  isDisabled={locked}
                                  aria-label={t("esim.deleteAria", "Delete {{name}}", { name })}
                                  onPress={() => {
                                    setTarget(p);
                                    setDialog("delete");
                                  }}
                                >
                                  <Trash size={20} weight="bold" aria-hidden />
                                </Button>
                              </>
                            )}
                          </span>
                        }
                      />
                    );
                  })
                )}
              </div>
              {inline === "nick" && nickReq && (
                <ConfirmInline
                  id={inlineCtl.id}
                  open
                  actionLabel={t("esim.renameAction", "rename")}
                  consequence={
                    nickReq.nickname
                      ? t("esim.renameConsequence", "“{{name}}” is shown as “{{nick}}” here and on the touchscreen. The card itself doesn't change.", {
                          name: nickReq.name,
                          nick: nickReq.nickname,
                        })
                      : t("esim.clearNickConsequence", "The nickname of “{{name}}” is cleared; the carrier's name is shown instead.", { name: nickReq.name })
                  }
                  onCancel={cancelInline}
                  onConfirm={goInline}
                />
              )}
              <div className="mt-2 grid gap-2 px-1">
                <OpResult op={deleteOp} />
                {jobClash(deleteOp.error) && deleteOp.phase === "failed" && (
                  <p className="nd-aux">{t("esim.jobClash", "Another eSIM operation is still running. Wait for it to finish, then try again.")}</p>
                )}
                <OpResult op={nickOp} />
              </div>
            </section>

            {/* ── add a profile ── */}
            <DownloadSection
              code={code}
              setCode={setCode}
              confCode={confCode}
              setConfCode={setConfCode}
              err={codeErr}
              locked={locked}
              pending={downloadOp.busy}
              onAsk={askDownload}
              triggerProps={inline === "download" ? inlineCtl.triggerProps : {}}
              confirm={
                inline === "download" && dlReq ? (
                  <ConfirmInline
                    id={inlineCtl.id}
                    open
                    actionLabel={t("esim.downloadAction", "download the profile")}
                    consequence={t(
                      "esim.downloadConsequence",
                      "The device fetches the profile from the carrier's server over the mobile link and stores it on the card (uses card space; can take a few minutes). It is not enabled — switch to it when ready."
                    )}
                    onCancel={cancelInline}
                    onConfirm={goInline}
                  />
                ) : null
              }
              result={
                <>
                  <OpResult op={downloadOp} />
                  {jobClash(downloadOp.error) && downloadOp.phase === "failed" && (
                    <p className="nd-aux">{t("esim.jobClash", "Another eSIM operation is still running. Wait for it to finish, then try again.")}</p>
                  )}
                </>
              }
            />

            {/* ── pending notifications (hidden when there are none) ── */}
            {(notifList.length > 0 || notifOp.phase !== "idle") && (
              <section>
                <div className="flex items-end gap-2">
                  <div className="flex-1">
                    <GroupTitle>{t("esim.notifications", "Pending notifications")}</GroupTitle>
                  </div>
                  <span className="mb-2" {...(inline === "notif" ? inlineCtl.triggerProps : {})}>
                    <Button
                      variant="secondary"
                      size="sm"
                      isDisabled={busy || notifList.length === 0}
                      pending={notifOp.busy}
                      onPress={() => setInline((i) => (i === "notif" ? null : "notif"))}
                    >
                      <PaperPlaneTilt size={20} weight="bold" aria-hidden />
                      {t("esim.processAll", "Send all")}
                    </Button>
                  </span>
                </div>
                <p className="nd-aux -mt-1 mb-3 px-1">
                  {t("esim.notificationsDesc", "Delivery receipts the card owes the carriers' servers after profile operations.")}
                </p>
                {inline === "notif" && (
                  <ConfirmInline
                    id={inlineCtl.id}
                    open
                    actionLabel={t("esim.notifAction", "send the notifications")}
                    consequence={t(
                      "esim.notifConsequence",
                      "The device sends {{n}} receipt(s) to the carriers' servers over the mobile link and removes them from the card once delivered.",
                      { n: notifList.length }
                    )}
                    onCancel={cancelInline}
                    onConfirm={goInline}
                  />
                )}
                {notifList.length > 0 ? (
                  <Group stale={notif.stale}>
                    {notifList.map((n) => (
                      <Row
                        key={n.seqNumber}
                        label={`#${n.seqNumber} · ${n.profileManagementOperation}`}
                        sub={
                          <>
                            <span className="nd-mono">{n.notificationAddress}</span>
                            {" · "}
                            <span className="nd-mono">{n.iccid}</span>
                          </>
                        }
                      />
                    ))}
                  </Group>
                ) : (
                  <p className="nd-body px-1 text-nd-t2">{t("esim.noNotifications", "Nothing pending.")}</p>
                )}
                <div className="mt-2 px-1">
                  <OpResult op={notifOp} />
                </div>
              </section>
            )}
          </>
        )}

        {/* ── chip details ── */}
        {chip.data?.installed && (
          <Group title={t("esim.chipTitle", "Card")} stale={chip.stale}>
            <Row label="EID" value={<span className="break-all">{chip.data.eid || "—"}</span>} mono />
            <Row
              label={t("esim.freeSpace", "Free space")}
              sub={t("esim.freeSpaceHint", "for new profiles")}
              value={chip.data.free_nvm != null ? fmtBytes(chip.data.free_nvm) : "—"}
              mono
            />
            <Row label="SGP.22" sub={t("esim.specVersion", "Spec version")} value={chip.data.sgp22_version || "—"} mono />
            <Row label={t("esim.chipFw", "Chip firmware")} value={chip.data.firmware_version || "—"} mono />
            <Row label={t("esim.rootSmds", "Root SM-DS")} value={chip.data.root_smds || "—"} mono />
            <Row label={t("esim.defaultSmdp", "Default SM-DP+")} value={chip.data.default_smdp || "—"} mono />
          </Group>
        )}
      </div>

      <ConfirmDialog
        open={dialog === "switch"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("esim.confirmSwitchTitle", "Switch to “{{name}}”?", { name: tgtName })}
        what={t(
          "esim.confirmSwitchWhat",
          "The card enables “{{name}}” and the modem re-attaches on it{{from}}. Mobile data stops until it is registered.",
          { name: tgtName, from: active ? t("esim.fromProfile", " (instead of “{{name}}”)", { name: nameOf(active) }) : "" }
        )}
        downtime={t(
          "esim.switchDowntime",
          "Usually about 40 s without a reboot. If the new profile doesn't take within about 30 s, the device reboots to finish (about 90 s more). The card then needs a 5-minute pause before the next switch."
        )}
        recovery={switchRecovery}
        actionLabel={t("esim.switch", "Switch")}
        cutsUplink
        onConfirm={goDialog}
      />
      <ConfirmDialog
        open={dialog === "delete"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("esim.confirmDeleteTitle", "Delete “{{name}}” from the card?", { name: tgtName })}
        what={t("esim.confirmDeleteWhat", "The profile is erased from the eUICC. This can't be undone: getting it back needs a new activation code from the carrier.")}
        recovery={t("esim.deleteRecovery", "None on the device. Ask the carrier for a new profile if you need it again.")}
        actionLabel={t("common.delete", "Delete")}
        typeToConfirm={deleteWord}
        danger
        onConfirm={goDialog}
      />
    </>
  );
}

function DownloadSection({
  code,
  setCode,
  confCode,
  setConfCode,
  err,
  locked,
  pending,
  onAsk,
  triggerProps,
  confirm,
  result,
}: {
  code: string;
  setCode: (v: string) => void;
  confCode: string;
  setConfCode: (v: string) => void;
  err: string | null;
  locked: boolean;
  pending: boolean;
  onAsk: () => void;
  triggerProps: Record<string, unknown>;
  confirm: ReactNode;
  result: ReactNode;
}) {
  const { t } = useTranslation();
  const id = useId();
  return (
    <section aria-labelledby={`${id}-t`}>
      <h2 id={`${id}-t`} className="nd-group-title">
        {t("esim.addProfile", "Add profile")}
      </h2>
      <div className="nd-group grid gap-4 p-4 lg:p-5">
        <div className="grid gap-1">
          <label htmlFor={`${id}-code`} className="font-semibold">
            {t("esim.codeLabel", "Activation code")}
          </label>
          <input
            id={`${id}-code`}
            className="nd-field nd-mono"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="LPA:1$smdp.example.com$XXXX-XXXX-XXXX"
            autoComplete="off"
            spellCheck={false}
            disabled={locked}
          />
        </div>
        <div className="grid gap-1">
          <label htmlFor={`${id}-conf`} className="font-semibold">
            {t("esim.confCode", "Confirmation code (optional)")}
          </label>
          <input
            id={`${id}-conf`}
            className="nd-field"
            value={confCode}
            onChange={(e) => setConfCode(e.target.value)}
            autoComplete="off"
            disabled={locked}
          />
        </div>
        <p className="nd-aux">
          {t(
            "esim.addHint",
            "Paste the activation code from your carrier (the text under the QR code). The profile is downloaded to the card but not enabled — switch to it when ready."
          )}
        </p>
        {err && (
          <p role="alert">
            <StatusMark tone="bad">{err}</StatusMark>
          </p>
        )}
        <div>
          <span {...triggerProps}>
            <Button onPress={onAsk} isDisabled={locked || !code.trim()} pending={pending}>
              <DownloadSimple size={20} weight="bold" aria-hidden />
              {t("esim.download", "Download")}
            </Button>
          </span>
        </div>
        {confirm}
        {result}
      </div>
    </section>
  );
}
