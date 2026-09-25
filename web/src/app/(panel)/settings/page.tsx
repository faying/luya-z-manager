"use client";
// Settings (new design) — settings of this browser only: appearance and
// language (tier 1, design §3.1 「界面设置」, same controls as the System hub),
// the admin-backend address, the polling-interval preference and the
// session. Nothing here writes to the device; "Test connectivity" is a read
// of /api/public/status (no login needed, so it can't sign you out).
//
// Log out / Clear stored token go through useAuth().logout: clearing the
// token and pushing /login left the auth context "signed in", and the gate
// sent the page straight back to /.
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { SignOut, Trash } from "@phosphor-icons/react";
import { apiFetch, getApiBase, setApiBase } from "@/lib/api/client";
import { useAuth } from "@/lib/hooks/useAuth";
import { applyTheme, readThemeChoice, type ThemeChoice } from "@/lib/theme";
import { LangSwitch } from "@/components/nd/shell/LangSwitch";
import { Button, Group, Row, Segmented, StatusMark } from "@/components/nd";

const POLL_KEY = "u60.poll_interval";
const DEFAULT_POLL = 2;

function readPoll(): number {
  if (typeof window === "undefined") return DEFAULT_POLL;
  return parseInt(window.localStorage.getItem(POLL_KEY) ?? "", 10) || DEFAULT_POLL;
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** Labelled form row: label (+ hint) above the control. */
function FieldRow({ id, label, hint, children }: { id: string; label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="nd-row flex-col items-stretch gap-2">
      <span className="nd-row__text">
        <label htmlFor={id} className="nd-row__label">
          {label}
        </label>
        {hint && (
          <span id={`${id}-hint`} className="nd-row__sub block">
            {hint}
          </span>
        )}
      </span>
      {children}
    </div>
  );
}

export default function SettingsPage() {
  const { t } = useTranslation();
  const { logout } = useAuth();

  // Pages mount on the client only (after AuthGate), so these read storage directly.
  const [theme, setTheme] = useState<ThemeChoice>(() => readThemeChoice());
  const [agentUrl, setAgentUrl] = useState(() => getApiBase());
  const [pollInterval, setPollInterval] = useState(() => readPoll());
  const [saved, setSaved] = useState<"agent" | "prefs" | null>(null);
  const [connectivity, setConnectivity] = useState<
    { state: "idle" } | { state: "checking" } | { state: "ok" | "error"; host: string }
  >({ state: "idle" });

  const savedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => {
    if (savedTimer.current) clearTimeout(savedTimer.current);
  }, []);

  function saveSettings(which: "agent" | "prefs") {
    setApiBase(agentUrl);
    window.localStorage.setItem(POLL_KEY, String(pollInterval));
    setSaved(which);
    if (savedTimer.current) clearTimeout(savedTimer.current);
    savedTimer.current = setTimeout(() => setSaved(null), 2000);
  }

  async function testConnectivity() {
    // Tests the saved address (apiFetch uses it), not an unsaved draft.
    const host = hostOf(getApiBase());
    setConnectivity({ state: "checking" });
    try {
      await apiFetch("/api/public/status", { noAuth: true });
      setConnectivity({ state: "ok", host });
    } catch {
      setConnectivity({ state: "error", host });
    }
  }

  function clearAgentUrl() {
    window.localStorage.removeItem("u60.agent_url");
    setAgentUrl(getApiBase());
  }

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("settings.title", "Settings")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">
        {t("settings.descBrowser", "Settings for this browser: appearance, language, the admin backend address and the session.")}
      </p>

      <span className="sr-only" role="status">
        {saved ? t("settings.saved", "Saved!") : ""}
      </span>

      <div className="grid max-w-[720px] gap-6">
        {/* Interface settings apply to this browser only (tier 1, no confirm). */}
        <Group title={t("nd.interface", "Interface")}>
          <Row
            label={t("nd.appearance", "Appearance")}
            control={
              <Segmented<ThemeChoice>
                label={t("nd.appearance", "Appearance")}
                value={theme}
                onChange={(v) => {
                  setTheme(v);
                  applyTheme(v);
                }}
                options={[
                  { id: "system", label: t("nd.themeSystem", "System") },
                  { id: "light", label: t("nd.themeLight", "Light") },
                  { id: "dark", label: t("nd.themeDark", "Dark") },
                ]}
              />
            }
          />
          <Row label={t("nd.language", "Language")} control={<LangSwitch />} />
        </Group>

        <section>
          <Group title={t("settings.agentConnection", "Agent Connection")}>
            <FieldRow
              id="settings-agent-url"
              label={t("settings.agentUrl", "Agent URL")}
              hint={t("settings.agentUrlHint", "URL where the zte-agent HTTP server is reachable")}
            >
              <input
                id="settings-agent-url"
                type="url"
                className="nd-field nd-mono"
                value={agentUrl}
                onChange={(e) => setAgentUrl(e.target.value)}
                placeholder="http://192.168.0.1:9090"
                aria-describedby="settings-agent-url-hint"
                autoComplete="off"
                autoCapitalize="off"
                autoCorrect="off"
                spellCheck={false}
              />
            </FieldRow>
            <div className="nd-row flex-wrap gap-2">
              <Button onPress={() => saveSettings("agent")}>
                {saved === "agent" ? t("settings.saved", "Saved!") : t("settings.save", "Save")}
              </Button>
              <Button variant="secondary" onPress={testConnectivity} pending={connectivity.state === "checking"}>
                {t("settings.testConnectivity", "Test Connectivity")}
              </Button>
              <Button variant="secondary" onPress={clearAgentUrl}>
                <Trash size={20} weight="bold" aria-hidden />
                {t("settings.clearStoredAgentUrl", "Clear Stored Agent URL")}
              </Button>
            </div>
          </Group>
          <div className="mt-2 px-1" role="status">
            {connectivity.state === "ok" && (
              <StatusMark tone="ok">
                {t("settings.reachable", "Reachable")} · <span className="nd-mono">{connectivity.host}</span>
              </StatusMark>
            )}
            {connectivity.state === "error" && (
              <StatusMark tone="bad">
                {t("settings.unreachable", "Unreachable")} · <span className="nd-mono">{connectivity.host}</span>
              </StatusMark>
            )}
          </div>
        </section>

        <Group title={t("settings.dashboard", "Dashboard")}>
          <FieldRow
            id="settings-poll"
            label={t("settings.pollingInterval", "Polling interval (seconds)")}
            hint={t("settings.pollingIntervalHint", "How often live data refreshes")}
          >
            <input
              id="settings-poll"
              type="number"
              inputMode="numeric"
              min={1}
              max={60}
              className="nd-field nd-mono sm:max-w-[160px]"
              value={pollInterval}
              aria-describedby="settings-poll-hint"
              onChange={(e) => setPollInterval(parseInt(e.target.value, 10) || DEFAULT_POLL)}
            />
          </FieldRow>
          <div className="nd-row">
            <Button onPress={() => saveSettings("prefs")}>
              {saved === "prefs" ? t("settings.saved", "Saved!") : t("settings.savePreferences", "Save Preferences")}
            </Button>
          </div>
        </Group>

        <Group title={t("settings.session", "Session")}>
          <div className="nd-row flex-wrap gap-2">
            <Button variant="secondary" onPress={logout}>
              <SignOut size={20} weight="bold" aria-hidden />
              {t("settings.logOut", "Log Out")}
            </Button>
            <Button variant="secondary" onPress={logout}>
              <Trash size={20} weight="bold" aria-hidden />
              {t("settings.clearStoredToken", "Clear Stored Token")}
            </Button>
          </div>
        </Group>
      </div>
    </>
  );
}
