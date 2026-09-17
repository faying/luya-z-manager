"use client";

import { FormEvent, Suspense, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useRouter, useSearchParams } from "next/navigation";
import { useAuth } from "@/lib/hooks/useAuth";
import { ApiError } from "@/lib/api/types";
import { apiFetch, getApiBase, setApiBase } from "@/lib/api/client";
import { Button, Input } from "@/components/admin/Button";
import { SectionCard, Status } from "@/components/admin/StatCard";

export default function LoginPage() {
  return (
    <Suspense fallback={<div className="flex min-h-screen items-center justify-center bg-bg text-text-dim font-mono text-xs">Loading…</div>}>
      <LoginForm />
    </Suspense>
  );
}

function LoginForm() {
  const { t } = useTranslation();
  const { login, authed, ready } = useAuth();
  const router = useRouter();
  const search = useSearchParams();
  const next = search?.get("next") ?? "/";

  const [password, setPassword] = useState("");
  const [base, setBase] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    setBase(getApiBase());
  }, []);

  useEffect(() => {
    if (ready && authed) router.replace(next);
  }, [ready, authed, next, router]);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    setErr(null);
    setBusy(true);
    try {
      if (base) setApiBase(base);
      await login(password);
      router.replace(next);
    } catch (e) {
      const m = e instanceof ApiError ? e.message : t("login.loginFailed", "Login failed");
      setErr(m);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="min-h-screen bg-bg px-4 py-10">
      <div className="mx-auto w-full max-w-3xl space-y-5">
        <form
          onSubmit={onSubmit}
          className="rounded-2xl border border-border bg-bg-card p-7 shadow-lg"
        >
          <h1 className="font-display text-xl font-semibold tracking-tight">{t("login.title", "U60 Pro Admin")}</h1>
          <p className="mt-1 text-sm text-text-dim">{t("login.subtitle", "Sign in to manage your router.")}</p>

          <label className="mt-6 block text-xs font-medium text-text-dim">{t("login.password", "Agent password")}</label>
          <Input
            type="password"
            autoFocus
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            className="mt-1.5"
          />

          <button
            type="button"
            onClick={() => setShowAdvanced((v) => !v)}
            className="mt-3 text-xs text-text-dim hover:text-accent"
          >
            {showAdvanced ? t("login.hide", "Hide") : t("login.advanced", "Advanced")}
          </button>

          {showAdvanced && (
            <div className="mt-2">
              <label className="block text-xs font-medium text-text-dim">{t("login.agentUrl", "Agent URL")}</label>
              <Input
                value={base}
                onChange={(e) => setBase(e.target.value)}
                placeholder="http://192.168.0.1:9090"
                className="mt-1.5 font-mono text-xs"
              />
            </div>
          )}

          {err && (
            <div className="mt-4 rounded-md border border-error/40 bg-error/10 px-3 py-2 text-sm text-error">
              {err}
            </div>
          )}

          <Button type="submit" className="mt-5 w-full" loading={busy}>
            {t("login.signIn", "Sign in")}
          </Button>
        </form>

        <PublicStatus />
      </div>
    </div>
  );
}

// ── Pre-login, read-only status (mirrors the stock UI, plus our services) ────

interface PublicData {
  network: { connected: boolean; type: string; operator: string; bar: number; rsrp: number };
  wifi: { on: boolean; ssid: string };
  battery: { percent: number; charging: boolean };
  services: {
    tailscale: { running: boolean; installed?: boolean; node: string };
    chill: { state: string; reason?: string | null };
    home_mode: { present: boolean; enabled: boolean; mode: string };
  };
  sms?: { unread?: number };
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3 border-b border-border/50 py-2 text-sm last:border-0">
      <span className="text-text-dim">{label}</span>
      <span className="text-right font-medium text-text">{children}</span>
    </div>
  );
}

function PublicStatus() {
  const { t } = useTranslation();
  const [d, setD] = useState<PublicData | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const r = await apiFetch<PublicData>("/api/public/status", { noAuth: true });
        if (alive) { setD(r); setFailed(false); }
      } catch {
        if (alive) setFailed(true);
      }
    };
    load();
    const t = setInterval(load, 10000);
    return () => { alive = false; clearInterval(t); };
  }, []);

  if (failed && !d) return null; // stay quiet if the device can't be reached yet
  if (!d) {
    return <div className="text-center text-xs text-text-dim">{t("login.loadingStatus", "Loading status…")}</div>;
  }

  const net = d.network;
  const svc = d.services;

  return (
    <div className="grid gap-4 sm:grid-cols-2">
      <SectionCard title={t("login.network", "Network")}>
        <Row label={t("login.connection", "Connection")}>
          {net.connected ? <Status tone="success">{t("login.connected", "Connected")}</Status> : <Status tone="danger">{t("login.offline", "Offline")}</Status>}
        </Row>
        <Row label={t("login.signal", "Signal")}>
          {net.bar >= 0 ? `${net.bar}/5` : "—"}
          {net.rsrp ? <span className="ml-2 font-mono text-xs text-text-dim">{net.rsrp} dBm</span> : null}
        </Row>
        <Row label={t("login.networkLabel", "Network")}>
          {net.type || "—"}
          {net.operator ? <span className="ml-2 text-text-dim">{net.operator}</span> : null}
        </Row>
      </SectionCard>

      <SectionCard title={t("login.device", "Device")}>
        <Row label="Wi-Fi">
          {d.wifi.on ? (
            <span className="font-mono">{d.wifi.ssid || t("common.on", "on")}</span>
          ) : (
            <Status tone="neutral">{t("common.off", "Off")}</Status>
          )}
        </Row>
        <Row label={t("login.battery", "Battery")}>
          {d.battery.percent >= 0 ? `${d.battery.percent}%` : "—"}
          {d.battery.charging ? <span className="ml-2 text-success">{t("login.charging", "charging")}</span> : null}
        </Row>
      </SectionCard>

      <SectionCard title={t("login.services", "Services")} className="sm:col-span-2">
        <Row label="Tailscale">
          {svc.tailscale.running ? (
            <Status tone="success">
              <span className="font-mono">{svc.tailscale.node || "up"}</span>
            </Status>
          ) : svc.tailscale.installed ? (
            <Status tone="warning">{t("common.stopped", "Stopped")}</Status>
          ) : (
            <Status tone="neutral">{t("common.notInstalled", "Not installed")}</Status>
          )}
        </Row>
        <Row label="CHILL">
          {svc.chill.state === "running" ? (
            <Status tone="success">{t("common.running", "Running")}</Status>
          ) : svc.chill.state === "direct" ? (
            <Status tone="warning">{t("chill.stDirect", "Direct")}</Status>
          ) : (
            <Status tone="neutral">{t("chill.stUnknown", "Not started")}</Status>
          )}
        </Row>
        <Row label={t("nav.homeMode", "Home Mode")}>
          {!svc.home_mode.present ? (
            <Status tone="neutral">{t("common.notInstalled", "Not installed")}</Status>
          ) : !svc.home_mode.enabled ? (
            <Status tone="neutral">{t("dashboard.paused", "Paused")}</Status>
          ) : svc.home_mode.mode === "home" ? (
            <Status tone="warning">{t("dashboard.activeWifiOff", "Active · Wi-Fi off")}</Status>
          ) : (
            <Status tone="success">{t("dashboard.activeWifiOn", "Active · Wi-Fi on")}</Status>
          )}
        </Row>
        <Row label={t("login.messages", "Messages")}>
          {(d.sms?.unread ?? 0) > 0 ? (
            <Status tone="warning">{t("dashboard.unread", "{{count}} unread", { count: d.sms?.unread })}</Status>
          ) : (
            <Status tone="neutral">{t("login.noUnread", "No unread")}</Status>
          )}
        </Row>
      </SectionCard>
    </div>
  );
}
