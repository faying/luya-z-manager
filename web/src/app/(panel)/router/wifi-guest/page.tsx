"use client";
// Guest Wi-Fi (new design). Status first (on/off, name, time left), then
// the guest network settings, then one Apply — like before, one PUT
// /api/wifi/guest with every field.
//
// Writes (controls-inventory §/router/wifi-guest, design §3.1): tier 2
// locally, tier 3 over Tailscale — the old page had no confirm at all. The
// agent writes guest_2g + guest_5g, silently skipping a section that
// doesn't exist (wifi.rs:370-378), commits, answers, then reloads ALL of
// Wi-Fi in the background (wifi_radio::finish_in_background), so the main
// network blips too. Result is read back from GET /api/wifi/guest; a guest
// SSID that reads back empty after sending one means the section is
// missing and the change did not stick — reported as not applied.
//
// GET /api/wifi/guest returns the uci names (ssid, key, encryption,
// disabled_2g, disabled_5g, hidden, isolate, guest_active_time); the old
// page first looked for the PUT names (guest_ssid, guest_key, …), which
// the agent never sends. Read directly now.
import { useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { isRemoteAccess } from "@/lib/api/remote";
import { freshNow, subscribeFresh } from "@/lib/api/freshness";
import { useWriteOp } from "@/lib/api/writeOp";
import type { GuestWifi, GuestWifiBody } from "@/lib/api/schemas/wifi";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  GroupTitle,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";
import { FieldRow, PasswordField, SelectField, TextField, flag, hasStripped, keyProblem, ssidProblem, type Opt } from "../wifi/fields";

type GuestForm = Required<
  Pick<
    GuestWifiBody,
    | "guest_ssid"
    | "guest_key"
    | "guest_encryption"
    | "guest_disabled_2g"
    | "guest_disabled_5g"
    | "guest_hidden"
    | "guest_isolate"
    | "guest_active_time"
  >
>;

function seed(d: GuestWifi): GuestForm {
  return {
    guest_ssid: d.ssid ?? "",
    guest_key: d.key ?? "",
    guest_encryption: d.encryption ?? "psk2+ccmp",
    guest_disabled_2g: d.disabled_2g === "1" ? "1" : "0",
    guest_disabled_5g: d.disabled_5g === "1" ? "1" : "0",
    guest_hidden: flag(d.hidden),
    guest_isolate: d.isolate !== "0" ? "1" : "0",
    guest_active_time: String(parseInt(d.guest_active_time ?? "0", 10) || 0),
  };
}

const FORM_KEYS = Object.keys(seed({} as GuestWifi)) as (keyof GuestForm)[];

function readbackMatches(sent: GuestForm, d: GuestWifi): boolean {
  const got = seed(d);
  // seed() maps "" to a default for encryption/isolate; compare the raw
  // SSID/key so a missing guest section can't pass as "unchanged".
  return FORM_KEYS.every((k) => got[k] === sent[k]) && (d.ssid ?? "") === sent.guest_ssid && (d.key ?? "") === sent.guest_key;
}

function encOptions(t: TFunction): Opt[] {
  return [
    { value: "psk2+ccmp", label: "WPA2" },
    { value: "psk3+ccmp", label: "WPA3" },
    { value: "psk2+psk3+ccmp", label: t("guestwifi.encMixed", "WPA2/WPA3 Mixed") },
    { value: "none", label: t("guestwifi.encOpen", "Open") },
  ];
}

function fmtRemaining(secs: number, t: TFunction): string {
  if (secs <= 0) return t("guestwifi.expired", "Expired");
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0)
    return t("guestwifi.remainingHms", "{{h}}h {{m}}m {{s}}s remaining", {
      h,
      m: String(m).padStart(2, "0"),
      s: String(s).padStart(2, "0"),
    });
  return t("guestwifi.remainingMs", "{{m}}m {{s}}s remaining", { m, s: String(s).padStart(2, "0") });
}

interface Pending {
  tier: 2 | 3;
  body: GuestForm;
  consequence: string;
  recovery: string;
  reconnect: string | null;
}

export default function GuestWifiPage() {
  const { t } = useTranslation();
  const g = useApi<GuestWifi>("/api/wifi/guest", { refreshInterval: 30000 });
  const data = g.data;

  const [draft, setDraft] = useState<GuestForm | null>(null);
  const base = data ? seed(data) : null;
  const form = draft ?? base;
  const changed = form && base ? FORM_KEYS.filter((k) => form[k] !== base[k]) : [];

  const [pending, setPending] = useState<Pending | null>(null);
  const [reconnectNote, setReconnectNote] = useState<string | null>(null);
  const sentRef = useRef<GuestForm | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);

  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: [
      {
        label: t("guestwifi.stepSettings", "Guest Wi-Fi settings"),
        run: () => apiFetch("/api/wifi/guest", { method: "PUT", body: sentRef.current }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<GuestWifi>("/api/wifi/guest");
      await g.mutate(d, { revalidate: false });
      const ok = sentRef.current !== null && readbackMatches(sentRef.current, d);
      if (ok) setDraft(null);
      return ok;
    },
    waitDevice: pending ? { expectedSec: 30, recovery: pending.recovery } : undefined,
  });
  const busy = op.busy;
  const locked = !data || g.stale || busy;

  // Time left: the device's figure at the last fetch, counted down locally.
  const now = useSyncExternalStore(subscribeFresh, freshNow, () => 0);
  const rem0 = data?.remaining_seconds ?? -1;
  const remaining = rem0 > 0 && g.lastOkAt && now ? Math.max(0, rem0 - Math.floor((now - g.lastOkAt) / 1000)) : rem0;

  function set<K extends keyof GuestForm>(k: K, v: string) {
    if (!form) return;
    setDraft({ ...form, [k]: v });
  }

  // ── validation (changed fields only) ──
  const errs: string[] = [];
  if (form) {
    // Every apply sends every field and the agent strips these characters
    // from each (wifi.rs:365): check them all, changed or not.
    if (hasStripped(form.guest_ssid))
      errs.push(
        changed.includes("guest_ssid")
          ? t("guestwifi.errSsidChars", "The guest network name can't contain ' \" ; $ ` \\ | < > & — the device would silently drop them")
          : t("guestwifi.errSsidCharsCurrent", "The current guest network name contains one of ' \" ; $ ` \\ | < > &, which the device would silently drop on any save. Change it here first."),
      );
    if (form.guest_encryption !== "none" && hasStripped(form.guest_key))
      errs.push(
        changed.includes("guest_key")
          ? t("guestwifi.errKeyChars", "The guest password can't contain ' \" ; $ ` \\ | < > & — the device would silently drop them")
          : t("guestwifi.errKeyCharsCurrent", "The current guest password contains one of ' \" ; $ ` \\ | < > &, which the device would silently drop on any save. Change it here first."),
      );
    if (changed.includes("guest_ssid")) {
      const p = ssidProblem(form.guest_ssid);
      if (p === "empty") errs.push(t("guestwifi.errSsidEmpty", "Enter a guest network name"));
      if (p === "long") errs.push(t("guestwifi.errSsidLong", "The guest network name is longer than 32 bytes"));
    }
    if (changed.includes("guest_key") || changed.includes("guest_encryption")) {
      const p = keyProblem(form.guest_key, form.guest_encryption);
      if (p === "short") errs.push(t("guestwifi.errKeyShort", "The guest password needs at least 8 characters"));
      if (p === "long") errs.push(t("guestwifi.errKeyLong", "The guest password can have at most 63 characters"));
      if (p === "ascii") errs.push(t("guestwifi.errKeyAscii", "The guest password can only use letters, digits and ASCII symbols"));
    }
    if (changed.includes("guest_active_time")) {
      const n = Number(form.guest_active_time);
      if (!/^\d+$/.test(form.guest_active_time) || n > 1440)
        errs.push(t("guestwifi.errActiveTime", "Active time must be a whole number of minutes from 0 to 1440"));
    }
  }

  function describe(f: GuestForm, b: GuestForm) {
    const diff = (k: keyof GuestForm) => f[k] !== b[k];
    const wasOn = b.guest_disabled_2g === "0" || b.guest_disabled_5g === "0";
    const isOn = f.guest_disabled_2g === "0" || f.guest_disabled_5g === "0";
    const name = t("wifi.quoted", "“{{s}}”", { s: f.guest_ssid });
    const lines: string[] = [];
    if (!wasOn && isOn) lines.push(t("guestwifi.cOn", "The guest network {{name}} comes on; anyone with the password can join.", { name }));
    else if (wasOn && !isOn) lines.push(t("guestwifi.cOff", "The guest network goes off; devices on it drop."));
    else if (diff("guest_disabled_2g") || diff("guest_disabled_5g")) lines.push(t("guestwifi.cBands", "The guest network changes which bands it uses."));
    let nameOrKey = false;
    let keyChanged = false;
    if (diff("guest_ssid")) {
      nameOrKey = true;
      lines.push(t("guestwifi.cSsid", "The guest network name becomes {{name}}.", { name }));
    }
    if (diff("guest_encryption")) {
      nameOrKey = true;
      lines.push(
        f.guest_encryption === "none"
          ? t("guestwifi.cOpen", "The guest network becomes open: anyone nearby can join without a password.")
          : t("guestwifi.cEnc", "Guest security becomes {{enc}}.", {
              enc: encOptions(t).find((o) => o.value === f.guest_encryption)?.label ?? f.guest_encryption,
            }),
      );
    }
    if (diff("guest_key") && f.guest_encryption !== "none") {
      nameOrKey = true;
      keyChanged = true;
      lines.push(t("guestwifi.cKey", "The guest password changes."));
    }
    if (diff("guest_isolate") && f.guest_isolate === "0")
      lines.push(t("guestwifi.cIsolateOff", "Guest devices will be able to reach each other."));
    const others: string[] = [];
    if (diff("guest_hidden")) others.push(t("guestwifi.hideSsid", "Hide SSID"));
    if (diff("guest_isolate") && f.guest_isolate === "1") others.push(t("guestwifi.apIsolation", "AP Isolation"));
    if (diff("guest_active_time"))
      others.push(
        f.guest_active_time === "0"
          ? t("guestwifi.cUnlimited", "no time limit")
          : t("guestwifi.cMinutes", "active for {{n}} min", { n: f.guest_active_time }),
      );
    if (others.length) lines.push(t("wifi.cOthers", "Also changes: {{list}}.", { list: others.join(t("wifi.listSep", ", ")) }));
    lines.push(t("guestwifi.cReload", "All of Wi-Fi reloads for about 15–60 seconds: devices on the main network and the guest network drop briefly."));
    const reconnect =
      isOn && nameOrKey
        ? keyChanged
          ? t("guestwifi.reconnectWithKey", "Guest devices must reconnect to {{name}} with the new password.", { name })
          : t("guestwifi.reconnect", "Guest devices must reconnect to {{name}}.", { name })
        : null;
    if (reconnect) lines.push(reconnect);
    const recovery = [
      reconnect,
      t("guestwifi.recovery", "If this page doesn't come back, reconnect to the U60's main Wi-Fi and open it again; over a cable, USB or Tailscale it stays reachable."),
    ]
      .filter(Boolean)
      .join(" ");
    return { consequence: lines.join(" "), recovery, reconnect };
  }

  function askApply() {
    if (!form || !base || busy || changed.length === 0 || errs.length > 0) return;
    setPending({ tier: isRemoteAccess() ? 3 : 2, body: { ...form }, ...describe(form, base) });
  }

  function go() {
    if (!pending) return;
    sentRef.current = pending.body;
    setReconnectNote(pending.reconnect);
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("guestwifi.loading", "Reading guest Wi-Fi…");
  let reason: ReactNode = null;
  let meta: ReactNode = null;
  if (!data && g.error) {
    tone = "bad";
    state = t("guestwifi.unreadable", "Can't read the guest Wi-Fi settings");
    reason = g.error.message;
  } else if (data) {
    const on2 = data.disabled_2g !== "1";
    const on5 = data.disabled_5g !== "1";
    const activeMin = parseInt(data.guest_active_time ?? "0", 10) || 0;
    if (!data.ssid && !data.disabled_2g && !data.disabled_5g) {
      tone = "warn";
      state = t("guestwifi.notConfigured", "No guest network found");
      reason = t("guestwifi.notConfiguredReason", "The device returned no guest network settings. Changes saved here may not take effect.");
    } else if (!on2 && !on5) {
      state = t("guestwifi.statusOff", "Guest Wi-Fi off");
      reason = t("guestwifi.statusOffReason", "Turn on 2.4 GHz or 5 GHz below to let guests join.");
    } else {
      tone = "ok";
      state = t("guestwifi.statusOn", "Guest Wi-Fi on");
      reason = (
        <>
          <span className="nd-mono">{data.ssid || "—"}</span>
          {" · "}
          {on2 && on5 ? "2.4 GHz + 5 GHz" : on2 ? "2.4 GHz" : "5 GHz"}
        </>
      );
      meta =
        activeMin > 0 && remaining > 0
          ? fmtRemaining(remaining, t)
          : activeMin === 0
            ? t("guestwifi.unlimited", "No time limit")
            : null;
    }
    if (g.stale) {
      tone = "stale";
      meta = <Freshness stale lastOkAt={g.lastOkAt} />;
    }
  }

  const showReconnect =
    reconnectNote !== null && ["waitDevice", "waitTimeout", "applied", "unknown", "verifying"].includes(op.phase);

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("guestwifi.title", "Guest WiFi")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("guestwifi.desc", "Configure the guest network for temporary access.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={meta}
          actions={
            !data && g.error ? (
              <Button variant="secondary" size="sm" onPress={() => g.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {g.stale && data && (
          <p className="nd-aux px-1">
            <Freshness stale lastOkAt={g.lastOkAt} what={t("wifi.settingsWord", "Settings")} />
            {t("wifi.refreshToEdit", " — refresh before changing anything.")}{" "}
            <Button variant="secondary" size="sm" onPress={() => g.mutate()}>
              {t("wifi.refresh", "Refresh")}
            </Button>
          </p>
        )}

        <section>
          <GroupTitle>{t("guestwifi.guestNetwork", "Guest Network")}</GroupTitle>
          {!form ? (
            <div className="nd-group">
              {[0, 1, 2].map((i) => (
                <div key={i} className="nd-row">
                  <span className="nd-skel" style={{ width: "10ch" }} />
                </div>
              ))}
            </div>
          ) : (
            <div className={`nd-group${g.stale ? " nd-stale" : ""}`}>
              <Row
                label={t("guestwifi.enabled2g", "2.4 GHz Enabled")}
                sub={form.guest_disabled_2g === "0" ? t("guestwifi.on", "On") : t("guestwifi.off", "Off")}
                control={
                  <Switch
                    label={t("guestwifi.band2gSwitch", "Guest 2.4 GHz")}
                    isSelected={form.guest_disabled_2g === "0"}
                    isDisabled={locked}
                    onChange={(on) => set("guest_disabled_2g", on ? "0" : "1")}
                  />
                }
              />
              <Row
                label={t("guestwifi.enabled5g", "5 GHz Enabled")}
                sub={form.guest_disabled_5g === "0" ? t("guestwifi.on", "On") : t("guestwifi.off", "Off")}
                control={
                  <Switch
                    label={t("guestwifi.band5gSwitch", "Guest 5 GHz")}
                    isSelected={form.guest_disabled_5g === "0"}
                    isDisabled={locked}
                    onChange={(on) => set("guest_disabled_5g", on ? "0" : "1")}
                  />
                }
              />
              <FieldRow id="guest-ssid" label={t("guestwifi.ssidLabel", "Guest SSID")} stacked>
                <TextField
                  id="guest-ssid"
                  value={form.guest_ssid}
                  disabled={locked}
                  placeholder={t("guestwifi.guestSsid", "Guest SSID")}
                  onChange={(v) => set("guest_ssid", v)}
                />
              </FieldRow>
              <FieldRow id="guest-key" label={t("guestwifi.passwordLabel", "Guest password")} stacked>
                <PasswordField
                  id="guest-key"
                  whose={t("guestwifi.whose", "guest")}
                  showLabel={t("guestwifi.showPassword", "Show guest password")}
                  hideLabel={t("guestwifi.hidePassword", "Hide guest password")}
                  value={form.guest_key}
                  disabled={locked || form.guest_encryption === "none"}
                  placeholder={
                    form.guest_encryption === "none" ? t("guestwifi.noPasswordOpen", "No password (open)") : t("guestwifi.password", "Password")
                  }
                  onChange={(v) => set("guest_key", v)}
                />
              </FieldRow>
              <FieldRow id="guest-enc" label={t("guestwifi.encryptionLabel", "Guest encryption")}>
                <SelectField id="guest-enc" value={form.guest_encryption} options={encOptions(t)} disabled={locked} onChange={(v) => set("guest_encryption", v)} />
              </FieldRow>
              <Row
                label={t("guestwifi.hideSsid", "Hide SSID")}
                control={
                  <Switch
                    label={t("guestwifi.hideGuestSsid", "Hide guest SSID")}
                    isSelected={form.guest_hidden === "1"}
                    isDisabled={locked}
                    onChange={(on) => set("guest_hidden", on ? "1" : "0")}
                  />
                }
              />
              <Row
                label={t("guestwifi.apIsolation", "AP Isolation")}
                sub={form.guest_isolate === "1" ? t("guestwifi.isolateOnSub", "Guests can't reach each other") : t("guestwifi.isolateOffSub", "Guests can reach each other")}
                control={
                  <Switch
                    label={t("guestwifi.apIsolation", "AP Isolation")}
                    isSelected={form.guest_isolate === "1"}
                    isDisabled={locked}
                    onChange={(on) => set("guest_isolate", on ? "1" : "0")}
                  />
                }
              />
              <FieldRow id="guest-time" label={t("guestwifi.activeTimeLabel", "Active time (minutes)")} sub={t("guestwifi.activeTimeSub", "0 = unlimited · up to 1440")}>
                <input
                  id="guest-time"
                  className="nd-field nd-mono text-right"
                  type="number"
                  min={0}
                  max={1440}
                  inputMode="numeric"
                  value={form.guest_active_time}
                  disabled={locked}
                  onChange={(e) => set("guest_active_time", e.target.value)}
                />
              </FieldRow>
            </div>
          )}
        </section>

        <section aria-labelledby="guest-apply-title">
          <GroupTitle id="guest-apply-title">{t("guestwifi.apply", "Apply")}</GroupTitle>
          <div className="nd-group grid gap-3 p-4 lg:p-5">
            <p className="nd-aux" role="status">
              {!form
                ? t("guestwifi.loading", "Reading guest Wi-Fi…")
                : changed.length === 0
                  ? t("wifi.noChanges", "No changes yet. Change a setting above, then apply.")
                  : t("wifi.nChanges", "{{n}} change(s) not applied yet.", { n: changed.length })}
            </p>
            {errs.length > 0 && (
              <ul className="grid gap-1" role="alert">
                {errs.map((e) => (
                  <li key={e}>
                    <StatusMark tone="bad">{e}</StatusMark>
                  </li>
                ))}
              </ul>
            )}
            <div className="flex flex-wrap items-center gap-3">
              <span {...(pending?.tier === 2 ? inline.triggerProps : {})}>
                <Button onPress={askApply} isDisabled={locked || changed.length === 0 || errs.length > 0} pending={busy}>
                  {t("guestwifi.apply", "Apply")}
                </Button>
              </span>
              {changed.length > 0 && (
                <Button variant="secondary" isDisabled={busy} onPress={() => setDraft(null)}>
                  {t("wifi.discard", "Discard changes")}
                </Button>
              )}
            </div>
            {pending?.tier === 2 && (
              <ConfirmInline
                id={inline.id}
                open
                actionLabel={t("guestwifi.applyAction", "apply the guest Wi-Fi settings")}
                consequence={pending.consequence}
                onCancel={() => setPending(null)}
                onConfirm={go}
              />
            )}
            <OpResult op={op} />
            {showReconnect && (
              <p className="nd-body" role="status">
                <StatusMark tone="warn">{reconnectNote}</StatusMark>
              </p>
            )}
          </div>
        </section>
      </div>

      {pending?.tier === 3 && (
        <ConfirmDialog
          open
          onOpenChange={(o) => !o && setPending(null)}
          title={t("guestwifi.confirmTitle", "Apply the guest Wi-Fi settings?")}
          what={pending.consequence}
          downtime={t("guestwifi.downtime", "About 15–60 seconds while all of Wi-Fi reloads, main network included.")}
          recovery={pending.recovery}
          actionLabel={t("nd.confirmAction", "Confirm: {{action}}", { action: t("guestwifi.applyAction", "apply the guest Wi-Fi settings") })}
          cutsUplink
          onConfirm={go}
        />
      )}
    </>
  );
}
