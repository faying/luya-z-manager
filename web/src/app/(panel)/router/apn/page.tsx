"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner, Status, MetaRow } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { Help } from "@/components/admin/Help";
import { useSWRConfig } from "swr";
import { Plus, Pencil, Trash2, RadioTower } from "lucide-react";

interface APNProfile {
  cid?: number;
  isEnable?: boolean;
  isValid?: number;
  password?: string;
  pdpType?: string;
  pppAuthMode?: string;
  profilename?: string;
  wanapn?: string;
  username?: string;
  // derived convenience (not from API)
  id?: string;
  name?: string;
  apn?: string;
  pdp_type?: string;
  auth_type?: string;
  active?: boolean;
}

interface APNMode {
  apn_mode?: number;
}

interface APNProfilesResponse {
  apnListArray?: APNProfile[];
}

const PDP_TYPES = ["IPv4", "IPv6", "IPv4v6"];
const AUTH_MODES = ["none", "PAP", "CHAP"];

const EMPTY_FORM: FormState = {
  name: "",
  apn: "",
  username: "",
  password: "",
  pdpType: "IPv4",
  pppAuthMode: "none",
  setAsDefault: false,
};

interface FormState {
  name: string;
  apn: string;
  username: string;
  password: string;
  pdpType: string;
  pppAuthMode: string;
  setAsDefault: boolean;
}

export default function APNPage() {
  const { t } = useTranslation();
  const { mutate } = useSWRConfig();

  const { data: modeData, error: modeErr } = useApi<APNMode>("/api/router/apn/mode");
  const { data: profilesData, error: profilesErr } = useApi<APNProfilesResponse>("/api/router/apn/profiles");
  const { data: autoData } = useApi<APNProfilesResponse>("/api/router/apn/auto-profiles");

  const refreshAll = () => {
    mutate("/api/router/apn/mode");
    mutate("/api/router/apn/profiles");
    mutate("/api/router/apn/auto-profiles");
  };

  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsError, setMsgIsError] = useState(false);

  // Form state
  const [showForm, setShowForm] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [formError, setFormError] = useState<string | null>(null);

  const isManual = modeData?.apn_mode === 1;
  const profiles = (profilesData?.apnListArray ?? []).map((p) => ({
    ...p,
    id: p.id ?? String(p.cid ?? ""),
    name: p.name ?? p.profilename ?? "",
    apn: p.apn ?? p.wanapn ?? "",
    pdp_type: p.pdp_type ?? p.pdpType ?? "",
    auth_type: p.auth_type ?? p.pppAuthMode ?? "",
    active: p.active ?? p.isEnable ?? false,
  }));
  const autoProfiles = (autoData?.apnListArray ?? []).map((p) => ({
    ...p,
    id: p.id ?? String(p.cid ?? ""),
    name: p.name ?? p.profilename ?? "",
    apn: p.apn ?? p.wanapn ?? "",
    pdp_type: p.pdp_type ?? p.pdpType ?? "",
    auth_type: p.auth_type ?? p.pppAuthMode ?? "",
    active: p.active ?? p.isEnable ?? false,
  }));

  const showMsg = (text: string, isError: boolean) => {
    setMsg(text);
    setMsgIsError(isError);
  };

  const handleModeToggle = async (manual: boolean) => {
    setSaving(true);
    try {
      await apiFetch("/api/router/apn/mode", {
        method: "PUT",
        body: { apn_mode: manual ? 1 : 0 },
      });
      await mutate("/api/router/apn/mode");
      showMsg(manual ? t("apn.msgModeManual", "APN mode set to manual") : t("apn.msgModeAuto", "APN mode set to auto"), false);
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setSaving(false);
    }
  };

  const startAdd = () => {
    setEditingId(null);
    setForm(EMPTY_FORM);
    setFormError(null);
    setShowForm(true);
  };

  const startEdit = (p: APNProfile) => {
    setEditingId(p.id ?? null);
    setForm({
      name: p.name ?? "",
      apn: p.apn ?? "",
      username: p.username ?? "",
      password: p.password ?? "",
      pdpType: p.pdp_type ?? "IPv4",
      pppAuthMode: p.auth_type ?? "none",
      setAsDefault: p.active ?? false,
    });
    setFormError(null);
    setShowForm(true);
  };

  const handleSave = async () => {
    if (!form.name.trim() || !form.apn.trim()) {
      setFormError(t("apn.errRequired", "Name and APN are required"));
      return;
    }
    const isDuplicate = profiles.some(
      (p) => p.name === form.name && p.id !== (editingId ?? undefined)
    );
    if (isDuplicate) {
      setFormError(t("apn.errDuplicate", "An APN with this name already exists"));
      return;
    }

    setSaving(true);
    setFormError(null);
    try {
      if (editingId) {
        await apiFetch("/api/router/apn/profiles", {
          method: "PUT",
          body: {
            profileId: editingId,
            profilename: form.name,
            wanapn: form.apn,
            pdpType: form.pdpType,
            pppAuthMode: form.pppAuthMode,
            username: form.username,
            password: form.password,
          },
        });
        if (form.setAsDefault) {
          await apiFetch("/api/router/apn/profiles/activate", {
            method: "POST",
            body: { profileId: editingId },
          });
        }
        showMsg(t("apn.msgUpdated", "APN updated"), false);
      } else {
        await apiFetch("/api/router/apn/profiles", {
          method: "POST",
          body: {
            profilename: form.name,
            wanapn: form.apn,
            pdpType: form.pdpType,
            pppAuthMode: form.pppAuthMode,
            username: form.username,
            password: form.password,
          },
        });
        showMsg(t("apn.msgAdded", "APN added"), false);
      }
      setShowForm(false);
      setEditingId(null);
      await mutate("/api/router/apn/profiles");
    } catch (e) {
      setFormError(e instanceof ApiError ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (p: APNProfile) => {
    if (p.active) {
      showMsg(t("apn.errCantDelete", "Cannot delete the active APN"), true);
      return;
    }
    if (!window.confirm(t("apn.confirmDelete", 'Delete APN "{{name}}"?', { name: p.name ?? p.apn }))) return;
    setSaving(true);
    try {
      await apiFetch("/api/router/apn/profiles/delete", {
        method: "POST",
        body: { profileId: p.id },
      });
      showMsg(t("apn.msgDeleted", "APN deleted"), false);
      await mutate("/api/router/apn/profiles");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setSaving(false);
    }
  };

  const handleActivate = async (p: APNProfile) => {
    setSaving(true);
    try {
      await apiFetch("/api/router/apn/profiles/activate", {
        method: "POST",
        body: { profileId: p.id },
      });
      showMsg(t("apn.msgActivated", "APN activated"), false);
      await mutate("/api/router/apn/profiles");
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <PageHeader
        title={t("apn.title", "APN Settings")}
        description={t("apn.desc", "Manage Access Point Name profiles for mobile data.")}
      />

      {(modeErr || profilesErr) && (
        <ErrorBanner message={String(modeErr ?? profilesErr)} onRetry={refreshAll} />
      )}
      {msg && (
        <div
          className={`mb-4 rounded-md border px-3 py-2 text-sm ${
            msgIsError
              ? "border-error/40 bg-error/10 text-error"
              : "border-success/40 bg-success/10 text-success"
          }`}
        >
          {msg}
        </div>
      )}

      {/* Mode toggle */}
      <SectionCard title={t("apn.modeTitle", "APN Mode")} className="mb-4">
        <div className="flex items-center justify-between">
          <div>
            <p className="text-sm font-medium">{isManual ? t("apn.manual", "Manual") : t("apn.auto", "Auto")}</p>
            <p className="mt-0.5 text-xs text-text-dim">
              {isManual
                ? t("apn.manualHint", "Using manually configured APN profile")
                : t("apn.autoHint", "Carrier APN detected automatically")}
            </p>
          </div>
          <Toggle
            checked={isManual}
            onChange={handleModeToggle}
            disabled={saving}
            label={t("apn.manual", "Manual")}
          />
        </div>
      </SectionCard>

      {/* Operator WAN IPv6 toggle */}
      <WanIpv6Card />

      {/* Manual profiles */}
      <SectionCard
        title={t("apn.manualProfiles", "Manual Profiles")}
        className="mb-4"
        actions={
          <Button size="sm" onClick={startAdd} disabled={saving}>
            <Plus className="h-3.5 w-3.5" />
            {t("apn.addProfile", "Add Profile")}
          </Button>
        }
      >
        {profiles.length === 0 ? (
          <div className="flex flex-col items-center gap-3 py-10 text-center">
            <span className="flex h-12 w-12 items-center justify-center rounded-full bg-accent-soft text-accent">
              <RadioTower size={22} />
            </span>
            <div className="space-y-1">
              <p className="text-sm font-medium text-text">{t("apn.emptyTitle", "No manual profiles yet")}</p>
              <p className="mx-auto max-w-[38ch] text-[13px] leading-relaxed text-text-dim">
                {t("apn.emptyDesc", "Add a profile if your carrier needs a specific APN. Otherwise leave APN Mode on Auto.")}
              </p>
            </div>
          </div>
        ) : (
          <div className="-my-1">
            {profiles.map((p, i) => (
              <div
                key={p.id ?? i}
                className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2 border-b border-border/50 py-3 last:border-0"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate font-medium text-text">{p.name || "—"}</span>
                    {p.active && <Status tone="success">{t("apn.active", "Active")}</Status>}
                  </div>
                  <MetaRow
                    className="mt-1"
                    items={[
                      p.apn && <span className="font-mono">{p.apn}</span>,
                      p.pdp_type,
                      p.auth_type && p.auth_type !== "none" && p.auth_type,
                    ]}
                  />
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  {!p.active && (
                    <Button size="sm" variant="outline" disabled={saving} onClick={() => handleActivate(p)}>
                      {t("apn.activate", "Activate")}
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={saving}
                    onClick={() => startEdit(p)}
                    aria-label={t("apn.editAria", "Edit {{name}}", { name: p.name || "profile" })}
                  >
                    <Pencil className="h-3.5 w-3.5" />
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={saving || !!p.active}
                    onClick={() => handleDelete(p)}
                    aria-label={t("apn.deleteAria", "Delete {{name}}", { name: p.name || "profile" })}
                  >
                    <Trash2 className="h-3.5 w-3.5 text-error" />
                  </Button>
                </div>
              </div>
            ))}
          </div>
        )}
      </SectionCard>

      {/* Auto-detected profiles (read-only) */}
      {autoProfiles.length > 0 && (
        <SectionCard
          title={t("apn.autoTitle", "Auto-detected Profiles")}
          description={t("apn.autoDesc", "Supplied by your carrier — read-only.")}
        >
          <div className="-my-1">
            {autoProfiles.map((p, i) => (
              <div
                key={p.id ?? i}
                className="flex items-center justify-between gap-3 border-b border-border/50 py-2.5 text-sm last:border-0"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-text">{p.name || "—"}</span>
                    {p.active && <Status tone="success">{t("apn.active", "Active")}</Status>}
                  </div>
                  <MetaRow
                    className="mt-1"
                    items={[p.apn && <span className="font-mono">{p.apn}</span>, p.pdp_type]}
                  />
                </div>
              </div>
            ))}
          </div>
        </SectionCard>
      )}

      {/* Add/Edit Form */}
      {showForm && (
        <div className="fixed inset-0 z-[60] flex items-center justify-center bg-text/40 p-4 backdrop-blur-sm">
          <div className="max-h-[calc(100vh-2rem)] w-full max-w-md overflow-y-auto rounded-2xl border border-border bg-bg-card p-6 shadow-lg">
            <h3 className="mb-4 font-display text-base font-semibold">
              {editingId ? t("apn.editProfile", "Edit APN Profile") : t("apn.addProfileTitle", "Add APN Profile")}
            </h3>

            {formError && <ErrorBanner message={formError} />}

            <div className="mt-3 space-y-3">
              <Field label={t("apn.profileName", "Profile Name *")}>
                <Input
                  value={form.name}
                  onChange={(e) => setForm((f) => ({ ...f, name: e.target.value }))}
                  placeholder={t("apn.phMyApn", "My APN")}
                />
              </Field>
              <Field label={t("apn.apnField", "APN *")} help={t("apn.helpApn", "The carrier's data gateway name (e.g. 'internet'). Your provider supplies this.")}>
                <Input
                  value={form.apn}
                  onChange={(e) => setForm((f) => ({ ...f, apn: e.target.value }))}
                  placeholder={t("apn.phInternet", "internet")}
                />
              </Field>
              <Field label={t("apn.username", "Username")}>
                <Input
                  value={form.username}
                  onChange={(e) => setForm((f) => ({ ...f, username: e.target.value }))}
                  placeholder={t("apn.phOptional", "(optional)")}
                />
              </Field>
              <Field label={t("apn.password", "Password")}>
                <Input
                  type="password"
                  value={form.password}
                  onChange={(e) => setForm((f) => ({ ...f, password: e.target.value }))}
                  placeholder={t("apn.phOptional", "(optional)")}
                />
              </Field>
              <Field label={t("apn.pdpType", "PDP Type")} help={t("apn.helpPdp", "Which IP versions this connection uses. IPv4v6 works for most carriers.")}>
                <select
                  value={form.pdpType}
                  onChange={(e) => setForm((f) => ({ ...f, pdpType: e.target.value }))}
                  className="h-9 w-full rounded-lg border border-border bg-bg-card px-3 text-sm text-text outline-none transition focus:border-accent focus:ring-2 focus:ring-accent/20"
                >
                  {PDP_TYPES.map((t) => (
                    <option key={t} value={t}>
                      {t}
                    </option>
                  ))}
                </select>
              </Field>
              <Field label={t("apn.authMode", "Auth Mode")} help={t("apn.helpAuth", "Authentication your carrier requires — usually 'none' unless they gave you a username and password.")}>
                <select
                  value={form.pppAuthMode}
                  onChange={(e) => setForm((f) => ({ ...f, pppAuthMode: e.target.value }))}
                  className="h-9 w-full rounded-lg border border-border bg-bg-card px-3 text-sm text-text outline-none transition focus:border-accent focus:ring-2 focus:ring-accent/20"
                >
                  {AUTH_MODES.map((a) => (
                    <option key={a} value={a}>
                      {a}
                    </option>
                  ))}
                </select>
              </Field>
              <div className="flex items-center gap-2 pt-1">
                <Toggle
                  checked={form.setAsDefault}
                  onChange={(v) => setForm((f) => ({ ...f, setAsDefault: v }))}
                />
                <span className="text-sm text-text-dim">{t("apn.setActive", "Set as active profile")}</span>
              </div>
            </div>

            <div className="mt-5 flex justify-end gap-2">
              <Button
                variant="ghost"
                onClick={() => {
                  setShowForm(false);
                  setEditingId(null);
                  setFormError(null);
                }}
                disabled={saving}
              >
                {t("common.cancel", "Cancel")}
              </Button>
              <Button onClick={handleSave} loading={saving} disabled={saving}>
                {editingId ? t("common.saveChanges", "Save Changes") : t("apn.addProfile", "Add Profile")}
              </Button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}

function Field({ label, help, children }: { label: string; help?: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="mb-1 flex items-center text-xs text-text-dim">
        {label}
        {help && <Help text={help} />}
      </label>
      {children}
    </div>
  );
}

interface WanIpv6State {
  ipv6_enabled: boolean;
  pdp_type: number;
  wan_has_ipv6: boolean;
}

/**
 * Operator WAN IPv6 toggle. Off makes the dialing APN request IPv4-only
 * (PDP type 1), so the carrier stops assigning an IPv6 prefix — useful when
 * IPv6 is leaking past an IPv4-only proxy. Turning it off drops the live IPv6
 * leg immediately without interrupting IPv4; the change persists across
 * reconnects and reboots.
 */
function WanIpv6Card() {
  const { t } = useTranslation();
  const { data, error, mutate, isLoading } = useApi<WanIpv6State>("/api/router/wan-ipv6");
  const [saving, setSaving] = useState(false);
  const [note, setNote] = useState<{ text: string; err: boolean } | null>(null);

  const onToggle = async (enabled: boolean) => {
    setSaving(true);
    setNote(null);
    try {
      await apiFetch("/api/router/wan-ipv6", { method: "PUT", body: { enabled } });
      setNote({
        text: enabled
          ? t("wanIpv6.enabledMsg", "Operator IPv6 enabled — reconnecting the data call.")
          : t("wanIpv6.disabledMsg", "Operator IPv6 disabled. IPv4 stays connected."),
        err: false,
      });
      await mutate();
    } catch (e) {
      setNote({ text: e instanceof ApiError ? e.message : String(e), err: true });
    } finally {
      setSaving(false);
    }
  };

  const enabled = data?.ipv6_enabled ?? false;

  return (
    <SectionCard title={t("wanIpv6.title", "Operator IPv6 (WAN)")} className="mb-4">
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0">
          <p className="text-sm font-medium">
            {isLoading
              ? t("common.loading", "Loading…")
              : enabled
                ? t("wanIpv6.on", "IPv6 requested from carrier")
                : t("wanIpv6.off", "IPv4-only")}
          </p>
          <p className="mt-0.5 text-xs leading-relaxed text-text-dim">
            {t(
              "wanIpv6.hint",
              "Turn off to stop the carrier from assigning a WAN IPv6 (dialing APN becomes IPv4-only). Fixes IPv6 leaking past an IPv4-only proxy."
            )}
          </p>
          {data && (
            <MetaRow
              className="mt-1"
              items={[
                `PDP ${data.pdp_type === 3 ? "IPv4v6" : data.pdp_type === 2 ? "IPv6" : "IPv4"}`,
                data.wan_has_ipv6
                  ? t("wanIpv6.liveOn", "live IPv6 up")
                  : t("wanIpv6.liveOff", "no live IPv6"),
              ]}
            />
          )}
        </div>
        <Toggle
          checked={enabled}
          onChange={onToggle}
          disabled={saving || isLoading || !!error}
          label={t("wanIpv6.title", "Operator IPv6 (WAN)")}
        />
      </div>
      {error && (
        <p className="mt-2 text-xs text-error">
          {t("wanIpv6.loadErr", "Couldn't read WAN IPv6 state.")}
        </p>
      )}
      {note && (
        <p className={`mt-2 text-xs ${note.err ? "text-error" : "text-success"}`}>{note.text}</p>
      )}
    </SectionCard>
  );
}
