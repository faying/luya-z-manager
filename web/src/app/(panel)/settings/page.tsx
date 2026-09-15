"use client";

import { useState, useEffect } from "react";
import { apiFetch, getApiBase, setApiBase } from "@/lib/api/client";
import { logout } from "@/lib/api/auth";
import { PageHeader, SectionCard } from "@/components/admin/StatCard";
import { Button, Input } from "@/components/admin/Button";
import { useRouter } from "next/navigation";
import { CheckCircle, XCircle, LogOut, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";

const POLL_KEY = "u60.poll_interval";
const DEFAULT_POLL = 2;

function FieldRow({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5 border-b border-border/60 py-3 last:border-0 sm:flex-row sm:items-center sm:justify-between">
      <div>
        <div className="text-sm">{label}</div>
        {hint && <div className="text-xs text-text-dim">{hint}</div>}
      </div>
      <div className="sm:w-64">{children}</div>
    </div>
  );
}

export default function SettingsPage() {
  const { t } = useTranslation();
  const router = useRouter();
  const [agentUrl, setAgentUrl] = useState("");
  const [pollInterval, setPollInterval] = useState(DEFAULT_POLL);
  const [connectivity, setConnectivity] = useState<"idle" | "ok" | "error" | "checking">("idle");
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    setAgentUrl(getApiBase());
    const stored = typeof window !== "undefined" ? window.localStorage.getItem(POLL_KEY) : null;
    if (stored) setPollInterval(parseInt(stored) || DEFAULT_POLL);
  }, []);

  function saveSettings() {
    setApiBase(agentUrl);
    if (typeof window !== "undefined") {
      window.localStorage.setItem(POLL_KEY, String(pollInterval));
    }
    setSaved(true);
    setTimeout(() => setSaved(false), 2000);
  }

  async function testConnectivity() {
    setConnectivity("checking");
    try {
      const result = await apiFetch("/api/auth/login", {
        method: "POST",
        body: { password: "" },
        raw: true,
        noAuth: true,
      });
      // Any JSON response (incl. 401) means agent is reachable
      void result;
      setConnectivity("ok");
    } catch {
      setConnectivity("error");
    }
  }

  function clearToken() {
    if (typeof window !== "undefined") window.localStorage.removeItem("u60.token");
    router.push("/login");
  }

  function clearAgentUrl() {
    if (typeof window !== "undefined") window.localStorage.removeItem("u60.agent_url");
    setAgentUrl(getApiBase());
  }

  function handleLogout() {
    logout();
    router.push("/login");
  }

  return (
    <>
      <PageHeader title={t("settings.title", "Settings")} description={t("settings.desc", "Agent connection and dashboard preferences.")} />

      <div className="grid gap-4">
        <SectionCard title={t("settings.agentConnection", "Agent Connection")}>
          <FieldRow label={t("settings.agentUrl", "Agent URL")} hint={t("settings.agentUrlHint", "URL where the zte-agent HTTP server is reachable")}>
            <Input
              type="url"
              value={agentUrl}
              onChange={(e) => setAgentUrl(e.target.value)}
              placeholder="http://192.168.0.1:9090"
            />
          </FieldRow>
          <div className="flex items-center gap-3 pt-3">
            <Button onClick={saveSettings}>{saved ? t("settings.saved", "Saved!") : t("settings.save", "Save")}</Button>
            <Button variant="outline" onClick={testConnectivity} loading={connectivity === "checking"}>
              {t("settings.testConnectivity", "Test Connectivity")}
            </Button>
            {connectivity === "ok" && (
              <span className="flex items-center gap-1 text-sm text-success">
                <CheckCircle size={15} /> {t("settings.reachable", "Reachable")}
              </span>
            )}
            {connectivity === "error" && (
              <span className="flex items-center gap-1 text-sm text-error">
                <XCircle size={15} /> {t("settings.unreachable", "Unreachable")}
              </span>
            )}
          </div>
        </SectionCard>

        <SectionCard title={t("settings.dashboard", "Dashboard")}>
          <FieldRow label={t("settings.pollingInterval", "Polling interval (seconds)")} hint={t("settings.pollingIntervalHint", "How often live data refreshes")}>
            <Input
              type="number"
              min={1}
              max={60}
              value={pollInterval}
              onChange={(e) => setPollInterval(parseInt(e.target.value) || DEFAULT_POLL)}
            />
          </FieldRow>
          <div className="pt-3">
            <Button onClick={saveSettings}>{saved ? t("settings.saved", "Saved!") : t("settings.savePreferences", "Save Preferences")}</Button>
          </div>
        </SectionCard>

        <SectionCard title={t("settings.session", "Session")}>
          <div className="flex flex-wrap gap-2">
            <Button variant="danger" onClick={handleLogout}>
              <LogOut size={14} /> {t("settings.logOut", "Log Out")}
            </Button>
            <Button variant="outline" onClick={clearToken}>
              <Trash2 size={14} /> {t("settings.clearStoredToken", "Clear Stored Token")}
            </Button>
            <Button variant="outline" onClick={clearAgentUrl}>
              <Trash2 size={14} /> {t("settings.clearStoredAgentUrl", "Clear Stored Agent URL")}
            </Button>
          </div>
        </SectionCard>
      </div>
    </>
  );
}
