"use client";

import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Bell,
  Check,
  CreditCard,
  Download,
  Pencil,
  Power,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import {
  PageHeader,
  SectionCard,
  Status,
  MetaRow,
  ErrorBanner,
} from "@/components/admin/StatCard";
import { Button, Input } from "@/components/admin/Button";

interface ChipStatus {
  installed: boolean;
  eid?: string;
  default_smdp?: string | null;
  root_smds?: string | null;
  sgp22_version?: string;
  firmware_version?: string;
  free_nvm?: number;
}

interface EsimProfile {
  iccid: string;
  isdpAid: string;
  profileState: "enabled" | "disabled";
  profileNickname: string | null;
  serviceProviderName: string;
  profileName: string;
  profileClass: string;
}

interface ProfilesResp {
  installed: boolean;
  busy?: boolean;
  profiles: EsimProfile[] | null;
}

interface Notification {
  seqNumber: number;
  profileManagementOperation: string;
  notificationAddress: string;
  iccid: string;
}

interface JobResp {
  id: number;
  kind: string;
  status: "idle" | "running" | "done" | "error";
  message: string;
  iccid: string;
  rebooting: boolean;
}

const PROFILE_POLL = 10000;

export default function EsimPage() {
  const { t } = useTranslation();
  const { data: chip, error: chipError, mutate: mutateChip, isLoading } = useApi<ChipStatus>(
    "/api/esim/status"
  );

  return (
    <>
      <PageHeader
        title={t("esim.title", "eSIM")}
        description={t(
          "esim.desc",
          "Manage profiles on a removable eUICC card (eSTK.me / 5ber and similar) in the SIM slot."
        )}
        actions={
          <Button variant="outline" size="sm" onClick={() => mutateChip()} disabled={isLoading}>
            <RefreshCw size={12} />
            {t("common.refresh", "Refresh")}
          </Button>
        }
      />

      {chipError && (
        <ErrorBanner
          message={t("esim.loadFailed", "Couldn't read the eUICC: {{msg}}", {
            msg: chipError instanceof ApiError ? chipError.message : "unknown",
          })}
          onRetry={() => mutateChip()}
        />
      )}

      {chip && !chip.installed && (
        <SectionCard title={t("common.notInstalled", "Not installed")}>
          <div className="flex flex-col items-center gap-3 py-8 text-center">
            <span className="flex h-12 w-12 items-center justify-center rounded-full bg-accent-soft text-accent">
              <CreditCard size={22} />
            </span>
            <div className="space-y-1">
              <p className="text-sm font-medium text-text">
                {t("esim.notInstalledTitle", "lpac isn't deployed on the device")}
              </p>
              <p className="mx-auto max-w-[46ch] text-[13px] leading-relaxed text-text-dim">
                {t(
                  "esim.notInstalledDesc",
                  "The eSIM manager needs the lpac bundle under /data/esim. Run scripts/esim/build-esim-bundle.sh from the repo, then refresh."
                )}
              </p>
            </div>
          </div>
        </SectionCard>
      )}

      {chip?.installed && (
        <>
          <div className="admin-card overflow-hidden">
            <div className="grid grid-cols-2 gap-px bg-border md:grid-cols-4">
              <Vital label="EID" value={chip.eid ? `…${chip.eid.slice(-8)}` : "—"} hint={chip.eid} mono />
              <Vital
                label={t("esim.freeSpace", "Free space")}
                value={chip.free_nvm != null ? fmtBytes(chip.free_nvm) : "—"}
                hint={t("esim.freeSpaceHint", "for new profiles")}
              />
              <Vital label="SGP.22" value={chip.sgp22_version ?? "—"} hint={t("esim.specVersion", "Spec version")} />
              <Vital
                label={t("esim.chipFw", "Chip firmware")}
                value={chip.firmware_version ?? "—"}
                hint={chip.root_smds ?? undefined}
              />
            </div>
          </div>

          <ProfilesSection />
          <NotificationsSection />
        </>
      )}
    </>
  );
}

/** Profile list + switch / rename / delete / download. Switching reboots the
 *  device (the modem stack only picks up the new profile on boot), so it gets
 *  a strong confirm and a "rebooting" banner that polls until the agent is
 *  back. */
function ProfilesSection() {
  const { t } = useTranslation();
  const { data, mutate, error } = useApi<ProfilesResp>("/api/esim/profiles", {
    refreshInterval: PROFILE_POLL,
  });

  const [pendingJob, setPendingJob] = useState<number | null>(null);
  const [opLabel, setOpLabel] = useState("");
  const [rebooting, setRebooting] = useState(false);
  const { data: job } = useApi<JobResp>(pendingJob != null ? "/api/esim/job" : null, {
    refreshInterval: pendingJob != null ? 1500 : 0,
  });

  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const msgTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  function flash(text: string, err = false) {
    if (msgTimer.current) clearTimeout(msgTimer.current);
    setMsg({ text, err });
    if (!err) msgTimer.current = setTimeout(() => setMsg(null), 5000);
  }

  const [code, setCode] = useState("");
  const [confCode, setConfCode] = useState("");
  const [edit, setEdit] = useState<{ iccid: string; nickname: string } | null>(null);
  const [localBusy, setLocalBusy] = useState(false);

  useEffect(() => {
    if (pendingJob == null || !job || job.id !== pendingJob) return;
    if (job.status === "done") {
      setPendingJob(null);
      if (job.rebooting) {
        setRebooting(true);
      } else {
        flash(job.message || t("esim.opDone", "Done"));
        mutate();
      }
    } else if (job.status === "error") {
      flash(job.message || t("esim.opFailed", "Operation failed"), true);
      setPendingJob(null);
      mutate();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [job, pendingJob]);

  // While rebooting, ping the (unauthenticated) public status endpoint until
  // the agent answers again, then drop the banner and reload data.
  useEffect(() => {
    if (!rebooting) return;
    const timer = setInterval(async () => {
      try {
        await apiFetch("/api/public/status", { noAuth: true, raw: true });
        setRebooting(false);
        flash(t("esim.switchDone", "Switched — device is back online"));
        mutate();
      } catch {
        /* still down */
      }
    }, 4000);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rebooting]);

  const busy = pendingJob != null || localBusy || rebooting || (data?.busy ?? false);
  const profiles = data?.profiles ?? [];

  async function startJob(path: string, body: unknown, label: string) {
    if (busy) return;
    setOpLabel(label);
    try {
      const res = await apiFetch<{ job_id: number }>(path, { method: "POST", body });
      setPendingJob(res.job_id);
    } catch (e) {
      flash(e instanceof ApiError ? e.message : t("esim.opFailed", "Operation failed"), true);
    }
  }

  function switchTo(p: EsimProfile) {
    const name = p.profileNickname || p.serviceProviderName || p.iccid;
    if (
      !window.confirm(
        t(
          "esim.confirmSwitch",
          'Switch to "{{name}}"?\n\nThe modem re-attaches on the new profile — expect a brief data interruption (usually ~40s, no reboot).',
          { name }
        )
      )
    )
      return;
    startJob("/api/esim/switch", { iccid: p.iccid }, t("esim.switching", "Switching…"));
  }

  function download() {
    const c = code.trim();
    if (!/^LPA:1\$/i.test(c) && !/^1\$/.test(c)) {
      flash(t("esim.badCode", "Enter an activation code like LPA:1$smdp.example.com$XXXX-XXXX"), true);
      return;
    }
    startJob(
      "/api/esim/download",
      { code: c, confirmation_code: confCode.trim() || undefined },
      t("esim.downloading", "Downloading profile…")
    );
    setCode("");
    setConfCode("");
  }

  async function deleteProfile(p: EsimProfile) {
    if (busy) return;
    const name = p.profileNickname || p.serviceProviderName || p.iccid;
    if (
      !window.confirm(
        t("esim.confirmDelete", 'Permanently delete profile "{{name}}" from the card? This cannot be undone.', {
          name,
        })
      )
    )
      return;
    startJob("/api/esim/delete", { iccid: p.iccid }, t("esim.deleting", "Deleting…"));
  }

  async function saveNickname(p: EsimProfile) {
    if (!edit || edit.iccid !== p.iccid) return;
    const nick = edit.nickname.trim();
    setEdit(null);
    if (nick === (p.profileNickname ?? "")) return;
    setLocalBusy(true);
    try {
      await apiFetch("/api/esim/nickname", { method: "POST", body: { iccid: p.iccid, nickname: nick } });
      flash(t("esim.renamed", "Nickname saved"));
      mutate();
    } catch (e) {
      flash(e instanceof ApiError ? e.message : t("esim.opFailed", "Operation failed"), true);
    } finally {
      setLocalBusy(false);
    }
  }

  return (
    <SectionCard
      title={t("esim.profiles", "Profiles")}
      description={t("esim.profilesDesc", "Switching reboots the device so the modem attaches with the new profile.")}
      className="mt-6"
      actions={
        busy ? (
          <Status tone="accent">
            {rebooting ? t("esim.rebooting", "Rebooting…") : opLabel || t("esim.working", "Working…")}
          </Status>
        ) : undefined
      }
    >
      {rebooting && (
        <div role="status" aria-live="polite" className="mb-4 rounded-md border border-accent/40 bg-accent-soft px-3 py-2 text-[13px] text-accent">
          {t("esim.rebootBanner", "Device is rebooting onto the new profile. This page reconnects automatically (~2 min).")}
        </div>
      )}

      {msg && (
        <div
          role={msg.err ? "alert" : "status"}
          aria-live="polite"
          className={`mb-4 rounded-md border px-3 py-2 text-[13px] ${
            msg.err ? "border-error/40 bg-error/10 text-error" : "border-success/40 bg-success/10 text-success"
          }`}
        >
          {msg.text}
        </div>
      )}

      {error && !data && (
        <ErrorBanner
          message={t("esim.profilesLoadFailed", "Couldn't load profiles: {{msg}}", {
            msg: error instanceof ApiError ? error.message : "unknown",
          })}
          onRetry={() => mutate()}
        />
      )}

      <div className="-mt-1">
        {profiles.length === 0 && !busy && (
          <p className="py-6 text-center text-[13px] text-text-dim">
            {t("esim.noProfiles", "No profiles on the card yet — add one below.")}
          </p>
        )}
        {profiles.map((p) => {
          const active = p.profileState === "enabled";
          const editing = edit?.iccid === p.iccid;
          return (
            <div
              key={p.iccid}
              className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2 border-b border-border/50 py-3 last:border-0"
            >
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  {editing ? (
                    <Input
                      value={edit.nickname}
                      onChange={(e) => setEdit({ iccid: p.iccid, nickname: e.target.value })}
                      onKeyDown={(e) => e.key === "Enter" && saveNickname(p)}
                      placeholder={p.serviceProviderName}
                      className="h-8 max-w-[220px] text-[13px]"
                      autoFocus
                    />
                  ) : (
                    <span className="truncate font-medium text-text">
                      {p.profileNickname || p.serviceProviderName || p.profileName}
                    </span>
                  )}
                  {active && <Status tone="success">{t("esim.active", "Active")}</Status>}
                </div>
                <MetaRow
                  className="mt-1"
                  items={[
                    p.serviceProviderName,
                    p.profileName,
                    <span key="iccid" className="font-mono">{p.iccid}</span>,
                  ]}
                />
              </div>
              <div className="flex shrink-0 items-center gap-1.5">
                {editing ? (
                  <Button variant="outline" size="sm" onClick={() => saveNickname(p)} disabled={busy}>
                    <Check size={12} />
                    {t("common.save", "Save")}
                  </Button>
                ) : (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setEdit({ iccid: p.iccid, nickname: p.profileNickname ?? "" })}
                    disabled={busy}
                    title={t("esim.rename", "Rename")}
                  >
                    <Pencil size={12} />
                  </Button>
                )}
                {!active && (
                  <>
                    <Button variant="outline" size="sm" onClick={() => switchTo(p)} disabled={busy}>
                      <Power size={12} />
                      {t("esim.switch", "Switch")}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => deleteProfile(p)}
                      disabled={busy}
                      title={t("common.delete", "Delete")}
                      className="text-error hover:text-error"
                    >
                      <Trash2 size={12} />
                    </Button>
                  </>
                )}
              </div>
            </div>
          );
        })}
      </div>

      <hr className="admin-hairline-soft my-4" />

      <div className="space-y-2">
        <p className="text-[13px] font-medium text-text">{t("esim.addProfile", "Add profile")}</p>
        <div className="flex flex-col gap-2 sm:flex-row">
          <Input
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="LPA:1$smdp.example.com$XXXX-XXXX-XXXX"
            className="flex-1 font-mono text-[12px]"
            disabled={busy}
          />
          <Input
            value={confCode}
            onChange={(e) => setConfCode(e.target.value)}
            placeholder={t("esim.confCode", "Confirmation code (optional)")}
            className="sm:w-56"
            disabled={busy}
          />
          <Button onClick={download} disabled={busy || !code.trim()}>
            <Download size={13} />
            {t("esim.download", "Download")}
          </Button>
        </div>
        <p className="text-[12px] leading-relaxed text-text-dim">
          {t(
            "esim.addHint",
            "Paste the activation code from your carrier (the text under the QR code). The profile is downloaded to the card but not enabled — switch to it when ready."
          )}
        </p>
      </div>
    </SectionCard>
  );
}

/** Pending SGP.22 notifications — should normally be empty; a process button
 *  delivers them to the carriers' SM-DP+ servers. */
function NotificationsSection() {
  const { t } = useTranslation();
  const { data, mutate } = useApi<Notification[] | null>("/api/esim/notifications", {
    refreshInterval: 30000,
  });
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const list = data ?? [];

  async function processAll() {
    setBusy(true);
    setErr(null);
    try {
      const res = await apiFetch<{ job_id: number }>("/api/esim/notifications/process", {
        method: "POST",
        body: {},
      });
      // Poll the job inline; notification runs are short-lived.
      for (let i = 0; i < 60; i++) {
        await new Promise((r) => setTimeout(r, 2000));
        const j = await apiFetch<JobResp>("/api/esim/job");
        if (j.id === res.job_id && j.status !== "running") {
          if (j.status === "error") setErr(j.message);
          break;
        }
      }
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : "failed");
    } finally {
      setBusy(false);
      mutate();
    }
  }

  if (list.length === 0 && !err) return null;

  return (
    <SectionCard
      title={t("esim.notifications", "Pending notifications")}
      description={t(
        "esim.notificationsDesc",
        "Delivery receipts the card owes the carriers' servers after profile operations."
      )}
      className="mt-6"
      actions={
        <Button variant="outline" size="sm" onClick={processAll} disabled={busy} loading={busy}>
          <Bell size={12} />
          {t("esim.processAll", "Send all")}
        </Button>
      }
    >
      {err && <p className="mb-3 text-[13px] text-error">{err}</p>}
      <div className="-mt-1">
        {list.map((n) => (
          <div
            key={n.seqNumber}
            className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 border-b border-border/50 py-2.5 last:border-0"
          >
            <span className="text-[13px] text-text">
              #{n.seqNumber} · {n.profileManagementOperation}
            </span>
            <MetaRow items={[n.notificationAddress, <span key="i" className="font-mono">{n.iccid}</span>]} />
          </div>
        ))}
      </div>
    </SectionCard>
  );
}

function Vital({
  label,
  value,
  hint,
  mono,
}: {
  label: string;
  value: string | number;
  hint?: string;
  mono?: boolean;
}) {
  return (
    <div className="bg-bg-card px-4 py-3.5">
      <p className="text-[11px] font-medium uppercase tracking-[0.08em] text-text-dim">{label}</p>
      <p className={`mt-0.5 truncate text-[15px] font-semibold text-text ${mono ? "font-mono" : ""}`} title={hint}>
        {value}
      </p>
      {hint && <p className="truncate text-[11px] text-text-dim">{hint}</p>}
    </div>
  );
}

function fmtBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}
