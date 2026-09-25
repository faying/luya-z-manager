"use client";
// APN (new design). Settings page: status first (which APN the data call
// dials with), then APN mode, operator IPv6, the manual profiles (with the
// add / edit form inline), then the carrier's auto-detected profiles.
//
// Writes (controls-inventory §/router/apn, design §3.1) — all tier 3:
//   APN mode switch        PUT  /api/router/apn/mode          readback apn_mode, waits ~30 s (re-dial)
//   operator IPv6 switch   PUT  /api/router/wan-ipv6          readback pdp_type (wan_has_ipv6 lags)
//   activate a profile     POST /api/router/apn/profiles/activate   readback isEnable, waits ~30 s
//   add                    POST /api/router/apn/profiles       readback: new entry in the list
//   save edit              PUT  /api/router/apn/profiles  [+ POST …/activate when「设为使用中」]
//                          two steps (R10): the edit can be saved while the switch is not
//   delete                 POST /api/router/apn/profiles/delete  readback: entry gone
// The passthrough writes never check the firmware's result (router.rs:259-326),
// so every write is read back; readbacks retry a few times because the
// firmware can lag.
//
// Page-vs-agent fixes (schemas/router.ts):
//   - profile id: firmware's string `profileId` first, `String(cid)` only as a fallback
//   - isEnable may be true / 1 / "1" / "true"; apn_mode may be a string
//   - pdpType / pppAuthMode may be numbers (1/2/3, 0/1/2): shown as labels
//   - empty pre-allocated slots (no name and no APN) are hidden
// Request bodies use the type the device's own list uses: B27 (recorded
// 2026-09-25) sends pdpType / pppAuthMode / roamingPdpType as integers, and
// the agent's own APN write (router.rs router_wan_ipv6_set) sends integers
// too, so when the list is numeric the page sends pdpType 1/2/3,
// pppAuthMode 0/1/2 and roamingPdpType (the edited profile's own value, or
// pdpType for a new one). With a string list (older mock / other firmware)
// the old string labels are sent as before. Unmapped numbers (roaming 0 on
// B27) are shown raw.
import { useId, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ArrowClockwise, PencilSimple, Plus, Trash } from "@phosphor-icons/react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { useWriteOp, type WriteStep } from "@/lib/api/writeOp";
import type { RouterApnList, RouterApnMode, RouterApnProfile, RouterApnProfileBody, RouterWanIpv6 } from "@/lib/api/schemas/router";
import {
  Button,
  ConfirmDialog,
  Freshness,
  Group,
  GroupTitle,
  Help,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  Switch,
  type Tone,
} from "@/components/nd";

const PDP_TYPES = ["IPv4", "IPv6", "IPv4v6"];
const AUTH_MODES = ["none", "PAP", "CHAP"];
const PDP_BY_NUM: Record<number, string> = { 1: "IPv4", 2: "IPv6", 3: "IPv4v6" };
const AUTH_BY_NUM: Record<number, string> = { 0: "none", 1: "PAP", 2: "CHAP" };
const PDP_NUM: Record<string, number> = { IPv4: 1, IPv6: 2, IPv4v6: 3 };
const AUTH_NUM: Record<string, number> = { none: 0, PAP: 1, CHAP: 2 };

/** Network writes re-dial the data call; wait for the agent before reading back. */
const NET_WAIT_SEC = 30;

interface Apn {
  id: string;
  name: string;
  apn: string;
  username: string;
  password: string;
  pdp: string;
  auth: string;
  /** Raw roamingPdpType as the device sent it (number on B27). */
  roamingRaw: unknown;
  active: boolean;
}

interface FormState {
  name: string;
  apn: string;
  username: string;
  password: string;
  pdpType: string;
  pppAuthMode: string;
  setAsDefault: boolean;
}

const EMPTY_FORM: FormState = {
  name: "",
  apn: "",
  username: "",
  password: "",
  pdpType: "IPv4",
  pppAuthMode: "none",
  setAsDefault: false,
};

function asBool(v: unknown): boolean {
  if (typeof v === "boolean") return v;
  if (typeof v === "number") return v !== 0;
  if (typeof v === "string") return ["1", "true", "on"].includes(v.toLowerCase());
  return false;
}

function label(v: unknown, byNum: Record<number, string>): string {
  if (typeof v === "number") return byNum[v] ?? String(v);
  if (typeof v === "string" && /^\d+$/.test(v)) return byNum[Number(v)] ?? v;
  return typeof v === "string" ? v : "";
}

function normalize(list: RouterApnProfile[] | undefined): Apn[] {
  return (list ?? [])
    .map((p) => ({
      id: typeof p.profileId === "string" && p.profileId !== "" ? p.profileId : p.cid != null ? String(p.cid) : "",
      name: p.profilename ?? "",
      apn: p.wanapn ?? "",
      username: p.username ?? "",
      password: p.password ?? "",
      pdp: label(p.pdpType, PDP_BY_NUM),
      auth: label(p.pppAuthMode, AUTH_BY_NUM),
      roamingRaw: p.roamingPdpType,
      active: asBool(p.isEnable),
    }))
    .filter((p) => p.name !== "" || p.apn !== "");
}

/** True when the device's APN list carries PDP / auth as numbers (B27). */
function listIsNumeric(...lists: (RouterApnProfile[] | undefined)[]): boolean {
  return lists.some((l) => (l ?? []).some((p) => typeof p.pdpType === "number" || typeof p.pppAuthMode === "number"));
}

/** Read until `ok` holds (the firmware can lag a write by a moment). */
async function settle<T>(read: () => Promise<T>, ok: (d: T) => boolean, tries = 3, gapMs = 2000): Promise<boolean> {
  for (let i = 0; i < tries; i++) {
    if (ok(await read())) return true;
    if (i < tries - 1) await new Promise((r) => setTimeout(r, gapMs));
  }
  return false;
}

type Dialog = null | "mode" | "ipv6" | "activate" | "delete" | "save";

export default function APNPage() {
  const { t } = useTranslation();
  const mode = useApi<RouterApnMode>("/api/router/apn/mode");
  const manual = useApi<RouterApnList>("/api/router/apn/profiles");
  const auto = useApi<RouterApnList>("/api/router/apn/auto-profiles");
  const wan = useApi<RouterWanIpv6>("/api/router/wan-ipv6");

  const profiles = normalize(manual.data?.apnListArray);
  const autoProfiles = normalize(auto.data?.apnListArray);
  const isManual = mode.data ? Number(mode.data.apn_mode) === 1 : null;
  const numeric = listIsNumeric(manual.data?.apnListArray, auto.data?.apnListArray);

  const readManual = async () => {
    const d = await apiFetch<RouterApnList>("/api/router/apn/profiles");
    await manual.mutate(d, { revalidate: false });
    return normalize(d.apnListArray);
  };

  const [dialog, setDialog] = useState<Dialog>(null);
  const recovery = t(
    "apn.recoveryNd",
    "Switch APN Mode back to Auto on this page (or on the device's touchscreen). If the page can't be reached, connect to the U60's Wi-Fi and open it from there."
  );

  // ── APN mode ──
  // Targets are state: every op is started from its dialog's confirm, a
  // render after the value was set, so start() snapshots the right one.
  const [modeWant, setModeWant] = useState(false);
  const modeOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("apn.modeTitle", "APN Mode"),
        run: () => apiFetch("/api/router/apn/mode", { method: "PUT", body: { apn_mode: modeWant ? 1 : 0 } }),
      },
    ],
    waitDevice: { expectedSec: NET_WAIT_SEC, recovery },
    verify: () =>
      settle(
        async () => {
          const d = await apiFetch<RouterApnMode>("/api/router/apn/mode");
          await mode.mutate(d, { revalidate: false });
          return d;
        },
        (d) => (Number(d.apn_mode) === 1) === modeWant
      ),
  });

  // ── operator IPv6 ──
  const [ipv6Want, setIpv6Want] = useState(false);
  const ipv6Op = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("wanIpv6.title", "Operator IPv6 (WAN)"),
        run: () => apiFetch("/api/router/wan-ipv6", { method: "PUT", body: { enabled: ipv6Want } }),
      },
    ],
    // pdp_type is the configuration; wan_has_ipv6 follows the live PDN and may lag.
    verify: () =>
      settle(
        async () => {
          const d = await apiFetch<RouterWanIpv6>("/api/router/wan-ipv6");
          await wan.mutate(d, { revalidate: false });
          return d;
        },
        (d) => (d.pdp_type === 2 || d.pdp_type === 3) === ipv6Want
      ),
  });

  // ── per-profile actions ──
  const [target, setTarget] = useState<Apn | null>(null);
  const activateOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("apn.activate", "Activate"),
        run: () => apiFetch("/api/router/apn/profiles/activate", { method: "POST", body: { profileId: target?.id } }),
      },
    ],
    waitDevice: { expectedSec: NET_WAIT_SEC, recovery },
    verify: () => settle(readManual, (list) => !!list.find((p) => p.id === target?.id)?.active),
  });
  const deleteOp = useWriteOp({
    tier: 3,
    steps: [
      {
        label: t("common.delete", "Delete"),
        run: () => apiFetch("/api/router/apn/profiles/delete", { method: "POST", body: { profileId: target?.id } }),
      },
    ],
    verify: () => settle(readManual, (list) => !list.some((p) => p.id === target?.id)),
  });

  // ── add / edit form ──
  const [showForm, setShowForm] = useState(false);
  const [editing, setEditing] = useState<Apn | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [formError, setFormError] = useState<string | null>(null);
  /** What the save dialog will send: fixed when the dialog opens. */
  const [sub, setSub] = useState<{ form: FormState; editing: Apn | null } | null>(null);
  const willActivate = !!sub && !!sub.editing && sub.form.setAsDefault && !sub.editing.active;
  const saveSteps: WriteStep[] = [];
  if (sub) {
    const f = sub.form;
    const fields: Omit<RouterApnProfileBody, "profileId"> = {
      profilename: f.name,
      wanapn: f.apn,
      pdpType: f.pdpType,
      pppAuthMode: f.pppAuthMode,
      username: f.username,
      password: f.password,
    };
    if (numeric) {
      const pdp = PDP_NUM[f.pdpType] ?? 1;
      fields.pdpType = pdp;
      fields.pppAuthMode = AUTH_NUM[f.pppAuthMode] ?? 0;
      const r = sub.editing?.roamingRaw;
      fields.roamingPdpType = typeof r === "number" ? r : typeof r === "string" && /^\d+$/.test(r) ? Number(r) : pdp;
    }
    if (sub.editing) {
      const id = sub.editing.id;
      saveSteps.push({
        label: t("apn.stepSave", "save the profile"),
        run: () => apiFetch("/api/router/apn/profiles", { method: "PUT", body: { profileId: id, ...fields } }),
      });
      if (willActivate) {
        saveSteps.push({
          label: t("apn.stepActivate", "make it the active profile"),
          run: () => apiFetch("/api/router/apn/profiles/activate", { method: "POST", body: { profileId: id } }),
        });
      }
    } else {
      saveSteps.push({
        label: t("apn.stepAdd", "add the profile"),
        run: () => apiFetch("/api/router/apn/profiles", { method: "POST", body: fields }),
      });
    }
  }
  // Editing the profile the data call uses (or switching to it) re-dials.
  const saveRedials = !!sub?.editing && (sub.editing.active || willActivate);
  const saveOp = useWriteOp({
    tier: 3,
    steps: saveSteps,
    waitDevice: saveRedials ? { expectedSec: NET_WAIT_SEC, recovery } : undefined,
    verify: () =>
      settle(readManual, (list) => {
        const s = sub;
        if (!s) return false;
        const p = s.editing ? list.find((x) => x.id === s.editing!.id) : list.find((x) => x.name === s.form.name);
        if (!p || p.name !== s.form.name || p.apn !== s.form.apn) return false;
        return s.editing && s.form.setAsDefault ? p.active : true;
      }),
  });

  // Close the form once the device shows the change; keep it (and the input)
  // otherwise. Adjusted during render, once per run.
  const [closedRun, setClosedRun] = useState(0);
  if (saveOp.phase === "applied" && saveOp.runId !== closedRun) {
    setClosedRun(saveOp.runId);
    setShowForm(false);
    setEditing(null);
  }

  const busy = modeOp.busy || ipv6Op.busy || activateOp.busy || deleteOp.busy || saveOp.busy;
  const listLocked = !manual.data || manual.stale || busy;

  function startAdd() {
    setEditing(null);
    setForm(EMPTY_FORM);
    setFormError(null);
    setShowForm(true);
  }
  function startEdit(p: Apn) {
    setEditing(p);
    setForm({
      name: p.name,
      apn: p.apn,
      username: p.username,
      password: p.password,
      pdpType: PDP_TYPES.includes(p.pdp) ? p.pdp : "IPv4",
      pppAuthMode: AUTH_MODES.includes(p.auth) ? p.auth : "none",
      setAsDefault: p.active,
    });
    setFormError(null);
    setShowForm(true);
  }
  function askSave() {
    const name = form.name.trim();
    const apn = form.apn.trim();
    if (!name || !apn) {
      setFormError(t("apn.errRequired", "Name and APN are required"));
      return;
    }
    if (profiles.some((p) => p.name === name && p.id !== editing?.id)) {
      setFormError(t("apn.errDuplicate", "An APN with this name already exists"));
      return;
    }
    setFormError(null);
    setSub({ form: { ...form, name, apn }, editing });
    setDialog("save");
  }

  function go(which: Exclude<Dialog, null>) {
    setDialog(null);
    const op = { mode: modeOp, ipv6: ipv6Op, activate: activateOp, delete: deleteOp, save: saveOp }[which];
    op.start();
    op.confirm();
  }

  // ── status ──
  const dialing = isManual ? profiles.find((p) => p.active) : autoProfiles.find((p) => p.active);
  const loadErr = (!mode.data && mode.error) || (!manual.data && manual.error);
  let tone: Tone = "neutral";
  let state: ReactNode = t("apn.loadingNd", "Reading APN settings…");
  let reason: ReactNode = null;
  if (loadErr) {
    tone = "bad";
    state = t("apn.unreadable", "Can't read the APN settings");
    reason = loadErr.message;
  } else if (mode.data && manual.data) {
    // Only manual mode with nothing active is a problem; auto mode is fine
    // whether or not the carrier's list flags a profile.
    tone = isManual && !dialing ? "warn" : "ok";
    state = dialing
      ? t("apn.dialingWith", "Data uses {{name}}", { name: dialing.name || dialing.apn })
      : isManual
        ? t("apn.noActiveManual", "Manual mode, but no profile is active")
        : t("apn.autoNoneShown", "Automatic APN");
    reason = isManual
      ? t("apn.manualHint", "Using manually configured APN profile")
      : t("apn.autoHint", "Carrier APN detected automatically");
    if (!dialing && isManual) reason = t("apn.noActiveNext", "Activate one of the manual profiles below, or switch back to Auto.");
  }
  const stale = (!!mode.data && mode.stale) || (!!manual.data && manual.stale);
  if (stale && !loadErr) tone = "stale";

  const retryAll = () => {
    void mode.mutate();
    void manual.mutate();
    void auto.mutate();
    void wan.mutate();
  };

  const tgt = target;
  const tgtName = tgt ? tgt.name || tgt.apn : "";
  const downtime = t("apn.downtime", "The mobile data connection drops for about 30 seconds while it re-dials with the new APN.");

  return (
    <>
      <div className="mb-4 mt-2 flex items-center gap-2">
        <h1 className="nd-title flex-1">{t("apn.title", "APN Settings")}</h1>
        <Button variant="ghost" iconOnly onPress={retryAll} aria-label={t("common.refresh", "Refresh")}>
          <ArrowClockwise size={20} weight="bold" aria-hidden />
        </Button>
      </div>

      <div className="grid max-w-[720px] gap-6">
        <p className="nd-body -mt-2 text-nd-t2">{t("apn.desc", "Manage Access Point Name profiles for mobile data.")}</p>

        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={
            stale ? (
              <>
                <Freshness stale lastOkAt={manual.lastOkAt ?? mode.lastOkAt} what={t("apn.settingsWord", "Settings")} />
                {t("apn.refreshToEdit", " — refresh before changing anything.")}
              </>
            ) : dialing?.apn ? (
              <span className="nd-mono">{[dialing.apn, dialing.pdp].filter(Boolean).join(" · ")}</span>
            ) : undefined
          }
          actions={
            loadErr || stale ? (
              <Button variant="secondary" size="sm" onPress={retryAll}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {/* ── APN mode ── */}
        <section>
          <Group title={t("apn.modeTitle", "APN Mode")} stale={mode.stale}>
            <Row
              label={t("apn.manual", "Manual")}
              sub={
                isManual === null
                  ? undefined
                  : isManual
                    ? t("apn.manualHint", "Using manually configured APN profile")
                    : t("apn.autoHint", "Carrier APN detected automatically")
              }
              control={
                isManual === null ? (
                  mode.error ? undefined : <span className="nd-skel" />
                ) : (
                  <Switch
                    label={t("apn.manual", "Manual")}
                    isSelected={modeOp.busy ? modeWant : isManual}
                    isDisabled={!mode.data || mode.stale || busy}
                    onChange={(v) => {
                      setModeWant(v);
                      setDialog("mode");
                    }}
                  />
                )
              }
            />
          </Group>
          <div className="mt-2 px-1">
            <OpResult op={modeOp} />
          </div>
        </section>

        {/* ── operator IPv6 ── */}
        <section>
          <Group title={t("wanIpv6.title", "Operator IPv6 (WAN)")} stale={wan.stale}>
            <Row
              label={
                !wan.data
                  ? wan.error
                    ? t("wanIpv6.loadErr", "Couldn't read WAN IPv6 state.")
                    : t("common.loading", "Loading…")
                  : wan.data.ipv6_enabled
                    ? t("wanIpv6.on", "IPv6 requested from carrier")
                    : t("wanIpv6.off", "IPv4-only")
              }
              sub={
                wan.data ? (
                  <>
                    <span className="nd-mono">PDP {PDP_BY_NUM[wan.data.pdp_type] ?? "—"}</span>
                    {" · "}
                    {wan.data.wan_has_ipv6 ? t("wanIpv6.liveOn", "live IPv6 up") : t("wanIpv6.liveOff", "no live IPv6")}
                  </>
                ) : undefined
              }
              control={
                wan.data ? (
                  <Switch
                    label={t("wanIpv6.title", "Operator IPv6 (WAN)")}
                    isSelected={ipv6Op.busy ? ipv6Want : wan.data.ipv6_enabled}
                    isDisabled={wan.stale || busy}
                    onChange={(v) => {
                      setIpv6Want(v);
                      setDialog("ipv6");
                    }}
                  />
                ) : wan.error ? (
                  <Button variant="secondary" size="sm" onPress={() => wan.mutate()}>
                    {t("common.retry", "Retry")}
                  </Button>
                ) : (
                  <span className="nd-skel" />
                )
              }
            />
          </Group>
          <p className="nd-aux mt-2 px-1">
            {t(
              "wanIpv6.hint",
              "Turn off to stop the carrier from assigning a WAN IPv6 (dialing APN becomes IPv4-only). Fixes IPv6 leaking past an IPv4-only proxy."
            )}
          </p>
          <div className="mt-2 px-1">
            <OpResult op={ipv6Op} />
          </div>
        </section>

        {/* ── manual profiles ── */}
        <section>
          <div className="flex items-end gap-2">
            <div className="flex-1">
              <GroupTitle>{t("apn.manualProfiles", "Manual Profiles")}</GroupTitle>
            </div>
            <Button variant="secondary" size="sm" onPress={startAdd} isDisabled={listLocked} className="mb-2">
              <Plus size={20} weight="bold" aria-hidden />
              {t("apn.addProfile", "Add Profile")}
            </Button>
          </div>
          <div className={`nd-group${manual.stale ? " nd-stale" : ""}`}>
            {!manual.data ? (
              manual.error ? (
                <div className="nd-row text-nd-t2">{t("apn.listUnreadable", "Couldn't read the profiles.")}</div>
              ) : (
                <>
                  <div className="nd-row"><span className="nd-skel" style={{ width: "14ch" }} /></div>
                  <div className="nd-row"><span className="nd-skel" style={{ width: "10ch" }} /></div>
                </>
              )
            ) : profiles.length === 0 ? (
              <div className="nd-row nd-row--two">
                <span className="nd-row__text">
                  <span className="nd-row__label">{t("apn.emptyTitle", "No manual profiles yet")}</span>
                  <span className="nd-row__sub block">
                    {t("apn.emptyDesc", "Add a profile if your carrier needs a specific APN. Otherwise leave APN Mode on Auto.")}
                  </span>
                </span>
              </div>
            ) : (
              profiles.map((p) => (
                <Row
                  key={p.id || p.name}
                  label={
                    <span className="inline-flex flex-wrap items-center gap-x-3">
                      <span>{p.name || "—"}</span>
                      {/* isEnable on a manual profile only marks manual mode's pick: in
                          Auto it is not what the data call dials (read on the device 9-25). */}
                      {p.active && isManual === true && <StatusMark tone="ok">{t("apn.active", "Active")}</StatusMark>}
                      {p.active && isManual === false && (
                        <StatusMark tone="neutral">{t("apn.manualPick", "Used in Manual mode")}</StatusMark>
                      )}
                    </span>
                  }
                  sub={
                    <>
                      {p.apn && <span className="nd-mono">{p.apn}</span>}
                      {[p.pdp, p.auth && p.auth !== "none" ? p.auth : ""].filter(Boolean).map((x) => ` · ${x}`)}
                    </>
                  }
                  control={
                    <span className="flex shrink-0 items-center gap-1">
                      {!p.active && (
                        <Button
                          variant="secondary"
                          size="sm"
                          isDisabled={listLocked}
                          aria-label={t("apn.activateAria", "Activate {{name}}", { name: p.name || p.apn })}
                          onPress={() => {
                            setTarget(p);
                            setDialog("activate");
                          }}
                        >
                          {t("apn.activate", "Activate")}
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        iconOnly
                        isDisabled={listLocked}
                        aria-label={t("apn.editAria", "Edit {{name}}", { name: p.name || "profile" })}
                        onPress={() => startEdit(p)}
                      >
                        <PencilSimple size={20} weight="bold" aria-hidden />
                      </Button>
                      <Button
                        variant="ghost"
                        iconOnly
                        isDisabled={listLocked || p.active}
                        aria-label={
                          p.active
                            ? t("apn.errCantDelete", "Cannot delete the active APN")
                            : t("apn.deleteAria", "Delete {{name}}", { name: p.name || "profile" })
                        }
                        onPress={() => {
                          setTarget(p);
                          setDialog("delete");
                        }}
                      >
                        <Trash size={20} weight="bold" aria-hidden />
                      </Button>
                    </span>
                  }
                />
              ))
            )}
          </div>
          <div className="mt-2 grid gap-2 px-1">
            <OpResult op={activateOp} />
            <OpResult op={deleteOp} />
            {!showForm && <OpResult op={saveOp} />}
          </div>

          {showForm && (
            <ApnForm
              form={form}
              setForm={setForm}
              editing={editing}
              error={formError}
              locked={busy}
              pending={saveOp.busy}
              onSave={askSave}
              onCancel={() => {
                setShowForm(false);
                setEditing(null);
                setFormError(null);
              }}
              result={<OpResult op={saveOp} />}
            />
          )}
        </section>

        {/* ── auto-detected (read-only) ── */}
        {autoProfiles.length > 0 && (
          <section>
            <Group title={t("apn.autoTitle", "Auto-detected Profiles")} stale={auto.stale}>
              {autoProfiles.map((p) => (
                <Row
                  key={p.id || p.name}
                  label={
                    <span className="inline-flex flex-wrap items-center gap-x-3">
                      <span>{p.name || "—"}</span>
                      {p.active && isManual === false && <StatusMark tone="ok">{t("apn.active", "Active")}</StatusMark>}
                    </span>
                  }
                  sub={
                    <>
                      {p.apn && <span className="nd-mono">{p.apn}</span>}
                      {p.pdp && ` · ${p.pdp}`}
                    </>
                  }
                />
              ))}
            </Group>
            <p className="nd-aux mt-2 px-1">{t("apn.autoDesc", "Supplied by your carrier — read-only.")}</p>
          </section>
        )}
      </div>

      <ConfirmDialog
        open={dialog === "mode"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={modeWant ? t("apn.confirmManualTitle", "Switch to manual APN?") : t("apn.confirmAutoTitle", "Switch to automatic APN?")}
        what={
          modeWant
            ? t("apn.confirmManualWhat", "The data connection re-dials with the active manual profile instead of the carrier's APN. With no working manual profile, mobile data stops.")
            : t("apn.confirmAutoWhat", "The data connection re-dials with the APN the carrier supplies. Manual profiles stay saved.")
        }
        downtime={downtime}
        recovery={recovery}
        actionLabel={modeWant ? t("apn.toManual", "Use manual APN") : t("apn.toAuto", "Use automatic APN")}
        cutsUplink
        onConfirm={() => go("mode")}
      />
      <ConfirmDialog
        open={dialog === "ipv6"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={ipv6Want ? t("wanIpv6.confirmOnTitle", "Request IPv6 from the carrier?") : t("wanIpv6.confirmOffTitle", "Stop requesting IPv6?")}
        what={
          ipv6Want
            ? t("wanIpv6.confirmOnWhat", "The dialing APN changes to IPv4v6 and the IPv6 leg of the data call comes up. Devices may start using IPv6 past an IPv4-only proxy.")
            : t("wanIpv6.confirmOffWhat", "The dialing APN changes to IPv4-only. The IPv6 leg drops now; IPv4 stays connected. It stays this way after reconnects and reboots.")
        }
        downtime={t("wanIpv6.downtime", "IPv4 is not interrupted. Connections that were using IPv6 drop and reconnect over IPv4.")}
        recovery={t("wanIpv6.recovery", "Flip this switch back.")}
        actionLabel={ipv6Want ? t("wanIpv6.turnOn", "Request IPv6") : t("wanIpv6.turnOff", "Use IPv4 only")}
        cutsUplink
        onConfirm={() => go("ipv6")}
      />
      <ConfirmDialog
        open={dialog === "activate"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("apn.confirmActivateTitle", "Switch to “{{name}}”?", { name: tgtName })}
        what={
          isManual
            ? t("apn.confirmActivateWhat", "The data connection re-dials with APN {{apn}}. If the carrier doesn't accept it, mobile data stops.", { apn: tgt?.apn ?? "" })
            : t("apn.confirmActivateWhatAuto", "“{{name}}” becomes the active manual profile. APN Mode is Auto, so it is used once you switch to Manual.", { name: tgtName })
        }
        downtime={downtime}
        recovery={recovery}
        actionLabel={t("apn.activate", "Activate")}
        cutsUplink
        onConfirm={() => go("activate")}
      />
      <ConfirmDialog
        open={dialog === "delete"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={t("apn.confirmDeleteTitle", "Delete “{{name}}”?", { name: tgtName })}
        what={t("apn.confirmDeleteWhat", "The profile is removed from the device. It isn't the active one, so the data connection is not affected.")}
        recovery={t("apn.deleteRecovery", "Add it again with “Add Profile” (note the APN, username and password first).")}
        actionLabel={t("common.delete", "Delete")}
        danger
        onConfirm={() => go("delete")}
      />
      <ConfirmDialog
        open={dialog === "save"}
        onOpenChange={(o) => !o && setDialog(null)}
        title={
          sub?.editing
            ? t("apn.confirmEditTitle", "Save changes to “{{name}}”?", { name: sub.editing.name || sub.editing.apn })
            : t("apn.confirmAddTitle", "Add “{{name}}”?", { name: sub?.form.name ?? "" })
        }
        what={
          saveRedials
            ? t("apn.confirmEditActiveWhat", "The data connection re-dials with APN {{apn}}. If the carrier doesn't accept it, mobile data stops.", { apn: sub?.form.apn ?? "" })
            : sub?.editing
              ? t("apn.confirmEditWhat", "The saved profile changes. It isn't in use, so the data connection is not affected.")
              : t("apn.confirmAddWhat", "A new manual profile is saved. It isn't used until you activate it.")
        }
        downtime={saveRedials ? downtime : undefined}
        recovery={saveRedials ? recovery : undefined}
        actionLabel={sub?.editing ? t("common.saveChanges", "Save Changes") : t("apn.addProfile", "Add Profile")}
        cutsUplink={saveRedials}
        onConfirm={() => go("save")}
      />
    </>
  );
}

function ApnForm({
  form,
  setForm,
  editing,
  error,
  locked,
  pending,
  onSave,
  onCancel,
  result,
}: {
  form: FormState;
  setForm: (f: (prev: FormState) => FormState) => void;
  editing: Apn | null;
  error: string | null;
  locked: boolean;
  pending: boolean;
  onSave: () => void;
  onCancel: () => void;
  result: ReactNode;
}) {
  const { t } = useTranslation();
  const id = useId();
  const set = <K extends keyof FormState>(k: K, v: FormState[K]) => setForm((f) => ({ ...f, [k]: v }));
  const field = (key: "name" | "apn" | "username" | "password", text: string, placeholder: string, help?: string) => (
    <div className="grid gap-1">
      <label htmlFor={`${id}-${key}`} className="flex items-center font-semibold">
        {text}
        {help && <Help text={help} label={text} />}
      </label>
      <input
        id={`${id}-${key}`}
        className={`nd-field${key === "apn" ? " nd-mono" : ""}`}
        type={key === "password" ? "password" : "text"}
        autoComplete="off"
        value={form[key]}
        placeholder={placeholder}
        disabled={locked}
        onChange={(e) => set(key, e.target.value)}
      />
    </div>
  );
  return (
    <section aria-labelledby={`${id}-title`} className="mt-6">
      <h2 id={`${id}-title`} className="nd-group-title">
        {editing ? t("apn.editProfile", "Edit APN Profile") : t("apn.addProfileTitle", "Add APN Profile")}
      </h2>
      <div className="nd-group grid gap-4 p-4 lg:p-5">
        {field("name", t("apn.profileName", "Profile Name *"), t("apn.phMyApn", "My APN"))}
        {field("apn", t("apn.apnField", "APN *"), t("apn.phInternet", "internet"), t("apn.helpApn", "The carrier's data gateway name (e.g. 'internet'). Your provider supplies this."))}
        {field("username", t("apn.username", "Username"), t("apn.phOptional", "(optional)"))}
        {field("password", t("apn.password", "Password"), t("apn.phOptional", "(optional)"))}
        <div className="grid gap-1">
          <label htmlFor={`${id}-pdp`} className="flex items-center font-semibold">
            {t("apn.pdpType", "PDP Type")}
            <Help text={t("apn.helpPdp", "Which IP versions this connection uses. IPv4v6 works for most carriers.")} label={t("apn.pdpType", "PDP Type")} />
          </label>
          <select id={`${id}-pdp`} className="nd-field" value={form.pdpType} disabled={locked} onChange={(e) => set("pdpType", e.target.value)}>
            {PDP_TYPES.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </div>
        <div className="grid gap-1">
          <label htmlFor={`${id}-auth`} className="flex items-center font-semibold">
            {t("apn.authMode", "Auth Mode")}
            <Help
              text={t("apn.helpAuth", "Authentication your carrier requires — usually 'none' unless they gave you a username and password.")}
              label={t("apn.authMode", "Auth Mode")}
            />
          </label>
          <select id={`${id}-auth`} className="nd-field" value={form.pppAuthMode} disabled={locked} onChange={(e) => set("pppAuthMode", e.target.value)}>
            {AUTH_MODES.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
        </div>
        <div className="flex items-center gap-3">
          <span className="flex-1">
            <span className="block font-semibold">{t("apn.setActive", "Set as active profile")}</span>
            {!editing && <span className="nd-aux block">{t("apn.setActiveAddHint", "Applies when editing. After adding, use “Activate” on the new profile.")}</span>}
          </span>
          <Switch
            label={t("apn.setActive", "Set as active profile")}
            isSelected={form.setAsDefault}
            isDisabled={locked || !editing}
            onChange={(v) => set("setAsDefault", v)}
          />
        </div>
        {error && (
          <p role="alert">
            <StatusMark tone="bad">{error}</StatusMark>
          </p>
        )}
        <div className="flex flex-wrap gap-2">
          <Button onPress={onSave} isDisabled={locked} pending={pending}>
            {editing ? t("common.saveChanges", "Save Changes") : t("apn.saveNew", "Save profile")}
          </Button>
          <Button variant="secondary" onPress={onCancel} isDisabled={pending}>
            {t("common.cancel", "Cancel")}
          </Button>
        </div>
        {result}
      </div>
    </section>
  );
}
