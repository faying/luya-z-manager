"use client";

import { useState, useEffect, useRef } from "react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { useSWRConfig } from "swr";
import { Eye, EyeOff, Timer } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

interface GuestWiFi {
  guest_onoff?: string;
  ssid?: string;
  key?: string;
  guest_ssid?: string;
  guest_key?: string;
  encryption?: string;
  guest_encryption?: string;
  hide?: string;
  guest_hidden?: string;
  isolate?: string;
  guest_isolate?: string;
  active_time?: string;
  guest_active_time?: string;
  disabled_2g?: string;
  guest_disabled_2g?: string;
  disabled_5g?: string;
  guest_disabled_5g?: string;
  remaining_seconds?: number;
}

function encryptions(t: TFunction): { value: string; label: string }[] {
  return [
    { value: "psk2+ccmp", label: "WPA2" },
    { value: "psk3+ccmp", label: "WPA3" },
    { value: "psk2+psk3+ccmp", label: t("guestwifi.encMixed", "WPA2/WPA3 Mixed") },
    { value: "none", label: t("guestwifi.encOpen", "Open") },
  ];
}

function FieldRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5 border-b border-border/60 py-3 last:border-0 sm:flex-row sm:items-center sm:justify-between">
      <span className="text-sm text-text-dim">{label}</span>
      <div className="sm:w-52">{children}</div>
    </div>
  );
}

function SelectField({ value, onChange, options }: { value: string; onChange: (v: string) => void; options: { value: string; label: string }[] }) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className="h-9 w-full rounded-md border border-border bg-bg-input px-3 text-sm outline-none transition focus:border-accent"
    >
      {options.map((o) => (
        <option key={o.value} value={o.value}>{o.label}</option>
      ))}
    </select>
  );
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
  return t("guestwifi.remainingMs", "{{m}}m {{s}}s remaining", {
    m,
    s: String(s).padStart(2, "0"),
  });
}

export default function GuestWiFiPage() {
  const { t } = useTranslation();
  const { data, error: fetchError, mutate } = useApi<GuestWiFi>("/api/wifi/guest");
  const { mutate: globalMutate } = useSWRConfig();

  const [enabled2g, setEnabled2g] = useState(false);
  const [enabled5g, setEnabled5g] = useState(false);
  const [ssid, setSsid] = useState("");
  const [key, setKey] = useState("");
  const [enc, setEnc] = useState("psk2+ccmp");
  const [hidden, setHidden] = useState(false);
  const [isolate, setIsolate] = useState(true);
  const [activeTime, setActiveTime] = useState(0);
  const [remaining, setRemaining] = useState(-1);
  const [showKey, setShowKey] = useState(false);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    if (!data) return;
    const d2g = data.guest_disabled_2g ?? data.disabled_2g;
    const d5g = data.guest_disabled_5g ?? data.disabled_5g;
    setEnabled2g(d2g !== "1");
    setEnabled5g(d5g !== "1");
    setSsid(data.guest_ssid ?? data.ssid ?? "");
    setKey(data.guest_key ?? data.key ?? "");
    setEnc(data.guest_encryption ?? data.encryption ?? "psk2+ccmp");
    setHidden((data.guest_hidden ?? data.hide) === "1");
    setIsolate((data.guest_isolate ?? data.isolate) !== "0");
    setActiveTime(parseInt(data.guest_active_time ?? data.active_time ?? "0") || 0);
    const rem = data.remaining_seconds ?? -1;
    if (remaining <= 0) setRemaining(rem);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data]);

  // Countdown timer
  const hasRemaining = remaining > 0;
  useEffect(() => {
    if (timerRef.current) clearInterval(timerRef.current);
    if (hasRemaining) {
      timerRef.current = setInterval(() => {
        setRemaining((r) => (r > 0 ? r - 1 : 0));
      }, 1000);
    }
    return () => { if (timerRef.current) clearInterval(timerRef.current); };
  }, [hasRemaining]);

  async function handleApply() {
    setSaving(true);
    setMsg(null);
    try {
      await apiFetch("/api/wifi/guest", {
        method: "PUT",
        body: {
          guest_ssid: ssid,
          guest_key: key,
          guest_encryption: enc,
          guest_disabled_2g: enabled2g ? "0" : "1",
          guest_disabled_5g: enabled5g ? "0" : "1",
          guest_hidden: hidden ? "1" : "0",
          guest_isolate: isolate ? "1" : "0",
          guest_active_time: String(activeTime),
        },
      });
      setMsg({ text: t("guestwifi.applied", "Guest WiFi settings applied."), err: false });
      await mutate();
      globalMutate("/api/wifi/guest");
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : t("guestwifi.applyFailed", "Failed to apply"), err: true });
    } finally {
      setSaving(false);
    }
  }

  const anyEnabled = enabled2g || enabled5g;

  return (
    <>
      <PageHeader
        title={t("guestwifi.title", "Guest WiFi")}
        description={t("guestwifi.desc", "Configure the guest network for temporary access.")}
        actions={
          <Button onClick={handleApply} loading={saving}>{t("guestwifi.apply", "Apply")}</Button>
        }
      />

      {fetchError && <ErrorBanner message={fetchError.message} />}
      {msg && (
        <div className={`mb-4 rounded-md border px-3 py-2 text-sm ${msg.err ? "border-error/40 bg-error/10 text-error" : "border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      {anyEnabled && activeTime > 0 && remaining > 0 && (
        <div className="mb-4 flex items-center gap-2 rounded-md border border-accent/30 bg-accent/10 px-3 py-2 text-sm text-accent">
          <Timer size={15} />
          {fmtRemaining(remaining, t)}
        </div>
      )}

      <SectionCard title={t("guestwifi.guestNetwork", "Guest Network")}>
        <FieldRow label={t("guestwifi.enabled2g", "2.4 GHz Enabled")}>
          <Toggle checked={enabled2g} onChange={setEnabled2g} label={enabled2g ? t("guestwifi.on", "On") : t("guestwifi.off", "Off")} />
        </FieldRow>
        <FieldRow label={t("guestwifi.enabled5g", "5 GHz Enabled")}>
          <Toggle checked={enabled5g} onChange={setEnabled5g} label={enabled5g ? t("guestwifi.on", "On") : t("guestwifi.off", "Off")} />
        </FieldRow>
        <FieldRow label="SSID">
          <Input value={ssid} onChange={(e) => setSsid(e.target.value)} placeholder={t("guestwifi.guestSsid", "Guest SSID")} />
        </FieldRow>
        <FieldRow label={t("guestwifi.password", "Password")}>
          <div className="relative">
            <Input
              type={showKey ? "text" : "password"}
              value={key}
              onChange={(e) => setKey(e.target.value)}
              className="pr-9"
              disabled={enc === "none"}
              placeholder={enc === "none" ? t("guestwifi.noPasswordOpen", "No password (open)") : t("guestwifi.password", "Password")}
            />
            <button
              type="button"
              onClick={() => setShowKey(!showKey)}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-text-dim hover:text-text"
            >
              {showKey ? <EyeOff size={15} /> : <Eye size={15} />}
            </button>
          </div>
        </FieldRow>
        <FieldRow label={t("guestwifi.encryption", "Encryption")}>
          <select
            value={enc}
            onChange={(e) => setEnc(e.target.value)}
            className="h-9 w-full rounded-md border border-border bg-bg-input px-3 text-sm outline-none transition focus:border-accent"
          >
            {encryptions(t).map((o) => (
              <option key={o.value} value={o.value}>{o.label}</option>
            ))}
          </select>
        </FieldRow>
        <FieldRow label={t("guestwifi.hideSsid", "Hide SSID")}>
          <Toggle checked={hidden} onChange={setHidden} />
        </FieldRow>
        <FieldRow label={t("guestwifi.apIsolation", "AP Isolation")}>
          <Toggle checked={isolate} onChange={setIsolate} label={isolate ? t("guestwifi.enabled", "Enabled") : t("guestwifi.disabled", "Disabled")} />
        </FieldRow>
        <FieldRow label={t("guestwifi.activeTime", "Active time (min, 0 = unlimited)")}>
          <Input
            type="number"
            min={0}
            max={1440}
            value={activeTime}
            onChange={(e) => setActiveTime(parseInt(e.target.value) || 0)}
          />
        </FieldRow>
      </SectionCard>
    </>
  );
}
