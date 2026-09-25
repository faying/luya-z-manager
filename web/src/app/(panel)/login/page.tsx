"use client";
// Login. Rendered outside the app shell, so this page sets up its own .nd
// root on the deep-green band.
//
//   U60 Pro · MU5250 (design doc §4, 2A)
//   sign-in form: password, "Advanced" → agent URL, sign in
//   public status (no login needed): network, device, services
import { FormEvent, Suspense, useEffect, useId, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useRouter, useSearchParams } from "next/navigation";
import { useAuth } from "@/lib/hooks/useAuth";
import { ApiError } from "@/lib/api/types";
import { getApiBase, setApiBase } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import type { PublicStatus } from "@/lib/api/schemas/public";
import { useDeviceLabel } from "@/lib/publicStatus";
import { Button, Freshness, Group, Row, StatusMark } from "@/components/nd";


export default function LoginPage() {
  return (
    <Suspense fallback={<Fallback />}>
      <LoginForm />
    </Suspense>
  );
}

function Fallback() {
  const { t } = useTranslation();
  return (
    <div className="nd flex min-h-dvh items-center justify-center bg-nd-bg">
      <p className="nd-aux">{t("login.loading", "Loading…")}</p>
    </div>
  );
}

function LoginForm() {
  const { t } = useTranslation();
  const deviceLabel = useDeviceLabel();
  const { login, authed, ready } = useAuth();
  const router = useRouter();
  const search = useSearchParams();
  const next = search?.get("next") ?? "/";
  const pwId = useId();
  const urlId = useId();
  const advId = useId();

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
    if (busy) return;
    setErr(null);
    setBusy(true);
    try {
      if (base) setApiBase(base);
      await login(password);
      router.replace(next);
    } catch (x) {
      if (x instanceof ApiError && x.status === 401) setErr(t("nd.wrongPassword", "Wrong password."));
      else if (x instanceof ApiError && x.status === 0)
        setErr(t("login.noReply", "The device did not answer. Check the connection or the agent address under Advanced, then try again."));
      else if (x instanceof ApiError) setErr(t("login.failedWith", "Login failed: {{msg}}", { msg: x.message }));
      else setErr(t("login.loginFailed", "Login failed"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="nd nd-bandpage min-h-dvh px-4 pb-10 pt-12 sm:pt-20">
      <main className="mx-auto grid w-full max-w-[480px] gap-6">
        <header className="px-1">
          <h1 className="nd-title">
            {deviceLabel}
          </h1>
          <p className="nd-body mt-1 nd-bandpage__sub">{t("login.subtitle", "Sign in to manage your router.")}</p>
        </header>

        <form onSubmit={onSubmit} className="nd-group grid gap-4 p-4 lg:p-5" aria-label={t("login.signIn", "Sign in")}>
          <div>
            <label htmlFor={pwId} className="mb-2 block text-[14px] font-semibold text-nd-t2">
              {t("login.password", "Agent password")}
            </label>
            <input
              id={pwId}
              className="nd-field"
              type="password"
              autoFocus
              autoComplete="current-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              aria-invalid={err ? true : undefined}
              aria-describedby={err ? `${pwId}-err` : undefined}
            />
          </div>

          <div>
            <Button
              variant="ghost"
              size="sm"
              className="-ms-3"
              onPress={() => setShowAdvanced((v) => !v)}
              aria-expanded={showAdvanced}
              aria-controls={advId}
            >
              {showAdvanced ? t("login.hide", "Hide") : t("login.advanced", "Advanced")}
            </Button>
            {showAdvanced && (
              <div id={advId} className="mt-2">
                <label htmlFor={urlId} className="mb-2 block text-[14px] font-semibold text-nd-t2">
                  {t("login.agentUrl", "Agent URL")}
                </label>
                <input
                  id={urlId}
                  className="nd-field nd-mono"
                  type="url"
                  inputMode="url"
                  autoComplete="off"
                  autoCapitalize="off"
                  spellCheck={false}
                  value={base}
                  onChange={(e) => setBase(e.target.value)}
                  placeholder="http://192.168.0.1:9090"
                />
                <p className="nd-aux mt-2">{t("login.agentUrlHelp", "Only needed when this page isn't served by the device itself. Saved in this browser.")}</p>
              </div>
            )}
          </div>

          {err && (
            <p id={`${pwId}-err`} role="alert" className="rounded-nd-field bg-nd-washB px-4 py-3 text-[15px] font-medium text-nd-badT">
              {err}
            </p>
          )}

          <Button type="submit" className="w-full" pending={busy}>
            {t("login.signIn", "Sign in")}
          </Button>
        </form>

        <PublicStatusPanel />
      </main>
    </div>
  );
}

// ── Before login: read-only status (/api/public/status needs no token) ────

function Title({ children }: { children: ReactNode }) {
  return (
    <h2 className="nd-group-title">
      {children}
    </h2>
  );
}

function PublicStatusPanel() {
  const { t } = useTranslation();
  const pub = useApi<PublicStatus>("/api/public/status", { refreshInterval: 10000, noAuth: true });
  const d = pub.data;

  if (!d) {
    if (pub.error) {
      return (
        <section className="nd-status nd-status--bad" aria-label={t("login.statusTitle", "Device status")}>
          <div className="nd-status__main">
            <div className="nd-status__state" role="alert">
              <StatusMark tone="bad">{t("login.unreachable", "Can't reach the device")}</StatusMark>
            </div>
            <div className="nd-status__reason">{t("login.unreachableNext", "Check the network (or the agent address under Advanced), then retry.")}</div>
          </div>
          <div className="nd-status__actions">
            <Button variant="secondary" size="sm" onPress={() => pub.mutate()}>
              {t("common.retry", "Retry")}
            </Button>
          </div>
        </section>
      );
    }
    return (
      <p className="nd-aux px-1" role="status">
        {t("login.loadingStatus", "Loading status…")}
      </p>
    );
  }

  const net = d.network;
  const svc = d.services;
  const unread = d.sms?.unread ?? 0;

  return (
    <section aria-label={t("login.statusTitle", "Device status")} className="grid gap-2">
      {pub.stale && (
        <div className="flex flex-wrap items-center gap-3 px-1">
          <Freshness stale lastOkAt={pub.lastOkAt} />
          <Button variant="ghost" size="sm" onPress={() => pub.mutate()}>
            {t("common.retry", "Retry")}
          </Button>
        </div>
      )}

      <div>
        <Title>{t("login.network", "Network")}</Title>
        <Group stale={pub.stale}>
          <Row
            label={t("login.connection", "Connection")}
            value={
              net.connected ? (
                <StatusMark tone="ok">{t("login.connected", "Connected")}</StatusMark>
              ) : (
                <StatusMark tone="bad">{t("login.offline", "Offline")}</StatusMark>
              )
            }
          />
          <Row
            label={t("login.signal", "Signal")}
            value={
              <>
                {net.bar >= 0 ? `${net.bar}/5` : "—"}
                {net.rsrp ? <span className="nd-mono ms-2 text-nd-t3">{net.rsrp} dBm</span> : null}
              </>
            }
          />
          <Row
            label={t("login.networkLabel", "Network")}
            value={
              <>
                {net.type || "—"}
                {net.operator ? <span className="ms-2 text-nd-t3">{net.operator}</span> : null}
              </>
            }
          />
        </Group>
      </div>

      <div>
        <Title>{t("login.device", "Device")}</Title>
        <Group stale={pub.stale}>
          <Row
            label="Wi-Fi"
            value={
              d.wifi.on ? (
                <span className="nd-mono">{d.wifi.ssid || t("common.on", "On")}</span>
              ) : (
                <StatusMark tone="neutral">{t("common.off", "Off")}</StatusMark>
              )
            }
          />
          <Row
            label={t("login.battery", "Battery")}
            value={
              <>
                {d.battery.percent >= 0 ? `${d.battery.percent}%` : "—"}
                {d.battery.charging ? <span className="ms-2 text-nd-okT">{t("login.charging", "charging")}</span> : null}
              </>
            }
          />
        </Group>
      </div>

      <div>
        <Title>{t("login.services", "Services")}</Title>
        <Group stale={pub.stale}>
          <Row
            label="Tailscale"
            value={
              svc.tailscale.running ? (
                <StatusMark tone="ok">
                  <span className="nd-mono">{svc.tailscale.node || t("common.running", "Running")}</span>
                </StatusMark>
              ) : svc.tailscale.installed ? (
                <StatusMark tone="warn">{t("common.stopped", "Stopped")}</StatusMark>
              ) : (
                <StatusMark tone="neutral">{t("common.notInstalled", "Not installed")}</StatusMark>
              )
            }
          />
          <Row
            label="CHILL"
            value={
              svc.chill.state === "running" ? (
                <StatusMark tone="ok">{t("common.running", "Running")}</StatusMark>
              ) : svc.chill.state === "direct" ? (
                <StatusMark tone="warn">{t("chill.stDirect", "Direct")}</StatusMark>
              ) : (
                <StatusMark tone="neutral">{t("chill.stUnknown", "Not started")}</StatusMark>
              )
            }
          />
          <Row
            label={t("nav.homeMode", "Home Mode")}
            value={
              !svc.home_mode.present ? (
                <StatusMark tone="neutral">{t("common.notInstalled", "Not installed")}</StatusMark>
              ) : !svc.home_mode.enabled ? (
                <StatusMark tone="neutral">{t("dashboard.paused", "Paused")}</StatusMark>
              ) : svc.home_mode.mode === "home" ? (
                <StatusMark tone="warn">{t("dashboard.activeWifiOff", "Active · Wi-Fi off")}</StatusMark>
              ) : (
                <StatusMark tone="ok">{t("dashboard.activeWifiOn", "Active · Wi-Fi on")}</StatusMark>
              )
            }
          />
          <Row
            label={t("login.messages", "Messages")}
            value={
              unread > 0 ? (
                <StatusMark tone="warn">{t("dashboard.unread", "{{count}} unread", { count: unread })}</StatusMark>
              ) : (
                <StatusMark tone="neutral">{t("login.noUnread", "No unread")}</StatusMark>
              )
            }
          />
        </Group>
      </div>
    </section>
  );
}
