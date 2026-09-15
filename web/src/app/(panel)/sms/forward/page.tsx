"use client";

import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Plus, Pencil, Trash2, Play, RefreshCw, XCircle } from "lucide-react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { decodeSms } from "@/lib/sms";
import { useSWRConfig } from "swr";

// ─── Types ───────────────────────────────────────────────────────────────────

type FilterType = "all" | "sender" | "content" | "sender_and_content";
type DestType = "telegram" | "webhook" | "sms" | "ntfy" | "discord" | "slack";

interface SmsFilter {
  type: FilterType;
  patterns?: string[];
  keywords?: string[];
}

interface DestBase { type: DestType }
interface TelegramDest extends DestBase { type: "telegram"; bot_token: string; chat_id: string; silent: boolean }
interface WebhookDest extends DestBase { type: "webhook"; url: string; method: string }
interface SmsDest extends DestBase { type: "sms"; forward_number: string }
interface NtfyDest extends DestBase { type: "ntfy"; url: string; topic: string; token?: string }
interface DiscordDest extends DestBase { type: "discord"; webhook_url: string }
interface SlackDest extends DestBase { type: "slack"; webhook_url: string }
type Destination = TelegramDest | WebhookDest | SmsDest | NtfyDest | DiscordDest | SlackDest;

interface ForwardRule {
  id: number;
  name: string;
  enabled: boolean;
  filter: SmsFilter;
  destination: Destination;
}

interface ForwardConfig {
  enabled: boolean;
  poll_interval_secs: number;
  mark_read_after_forward: boolean;
  delete_after_forward: boolean;
  rules: ForwardRule[];
}

interface ConfigResp {
  config: ForwardConfig;
  last_forwarded_id?: number;
}

interface LogEntry {
  id?: string;
  timestamp: number;
  sms_id: number;
  sender: string;
  content_preview: string;
  rule_name: string;
  destination_type: string;
  success: boolean;
  error?: string;
}

type Tab = "config" | "rules" | "log";

// ─── Helpers ─────────────────────────────────────────────────────────────────

function fmtTs(ts: number) {
  return new Date(ts * 1000).toLocaleString();
}

function emptyDest(type: DestType): Destination {
  switch (type) {
    case "telegram": return { type, bot_token: "", chat_id: "", silent: false };
    case "webhook":  return { type, url: "", method: "POST" };
    case "sms":      return { type, forward_number: "" };
    case "ntfy":     return { type, url: "", topic: "" };
    case "discord":  return { type, webhook_url: "" };
    case "slack":    return { type, webhook_url: "" };
  }
}

function emptyRule(): Omit<ForwardRule, "id"> {
  return {
    name: "",
    enabled: true,
    filter: { type: "all" },
    destination: emptyDest("telegram"),
  };
}

// ─── Rule Form ────────────────────────────────────────────────────────────────

function RuleForm({
  initial,
  onSave,
  onCancel,
  saving,
}: {
  initial: Omit<ForwardRule, "id"> & { id?: number };
  onSave: (rule: Omit<ForwardRule, "id"> & { id?: number }) => void;
  onCancel: () => void;
  saving: boolean;
}) {
  const { t } = useTranslation();
  const [rule, setRule] = useState(initial);

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function setDest(key: string, value: any) {
    setRule((r) => ({ ...r, destination: { ...r.destination, [key]: value } as Destination }));
  }

  const dest = rule.destination;

  return (
    <div className="space-y-4 rounded-xl border border-border bg-bg-card p-5">
      <h3 className="font-display text-sm font-semibold">{initial.id ? t("smsfwd.editRule", "Edit Rule") : t("smsfwd.newRule", "New Rule")}</h3>

      <div>
        <label className="mb-1 block text-xs font-medium text-text-dim">{t("smsfwd.ruleName", "Rule Name")}</label>
        <Input value={rule.name} onChange={(e) => setRule((r) => ({ ...r, name: e.target.value }))} placeholder={t("smsfwd.ruleNamePh", "My rule")} />
      </div>

      <Toggle
        checked={rule.enabled}
        onChange={(v) => setRule((r) => ({ ...r, enabled: v }))}
        label={t("common.enabled", "Enabled")}
      />

      {/* Filter */}
      <div>
        <label className="mb-1 block text-xs font-medium text-text-dim">{t("smsfwd.filter", "Filter")}</label>
        <select
          className="h-9 w-full rounded-md border border-border bg-bg-input px-3 text-sm outline-none"
          value={rule.filter.type}
          onChange={(e) => setRule((r) => ({ ...r, filter: { type: e.target.value as FilterType } }))}
        >
          <option value="all">{t("smsfwd.filterAll", "All messages")}</option>
          <option value="sender">{t("smsfwd.filterSender", "By sender (patterns)")}</option>
          <option value="content">{t("smsfwd.filterContent", "By content (keywords)")}</option>
          <option value="sender_and_content">{t("smsfwd.filterBoth", "Sender + Content")}</option>
        </select>
        {(rule.filter.type === "sender" || rule.filter.type === "sender_and_content") && (
          <div className="mt-2">
            <label className="mb-1 block text-xs text-text-dim">{t("smsfwd.senderPatterns", "Sender patterns (comma-separated)")}</label>
            <Input
              value={(rule.filter.patterns ?? []).join(", ")}
              onChange={(e) =>
                setRule((r) => ({
                  ...r,
                  filter: { ...r.filter, patterns: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) },
                }))
              }
              placeholder="+1234, Carrier"
            />
          </div>
        )}
        {(rule.filter.type === "content" || rule.filter.type === "sender_and_content") && (
          <div className="mt-2">
            <label className="mb-1 block text-xs text-text-dim">{t("smsfwd.keywords", "Keywords (comma-separated)")}</label>
            <Input
              value={(rule.filter.keywords ?? []).join(", ")}
              onChange={(e) =>
                setRule((r) => ({
                  ...r,
                  filter: { ...r.filter, keywords: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) },
                }))
              }
              placeholder="OTP, verification"
            />
          </div>
        )}
      </div>

      {/* Destination type */}
      <div>
        <label className="mb-1 block text-xs font-medium text-text-dim">{t("smsfwd.destination", "Destination")}</label>
        <select
          className="h-9 w-full rounded-md border border-border bg-bg-input px-3 text-sm outline-none"
          value={dest.type}
          onChange={(e) => setRule((r) => ({ ...r, destination: emptyDest(e.target.value as DestType) }))}
        >
          <option value="telegram">Telegram</option>
          <option value="webhook">Webhook</option>
          <option value="sms">{t("smsfwd.destSms", "SMS Forward")}</option>
          <option value="ntfy">Ntfy</option>
          <option value="discord">Discord</option>
          <option value="slack">Slack</option>
        </select>
      </div>

      {/* Destination fields */}
      {dest.type === "telegram" && (
        <div className="space-y-2">
          <div>
            <label className="mb-1 block text-xs text-text-dim">Bot Token</label>
            <Input value={dest.bot_token} onChange={(e) => setDest("bot_token", e.target.value)} placeholder="123456:ABC-..." />
          </div>
          <div>
            <label className="mb-1 block text-xs text-text-dim">Chat ID</label>
            <Input value={dest.chat_id} onChange={(e) => setDest("chat_id", e.target.value)} placeholder="-100123456" />
          </div>
          <Toggle checked={dest.silent} onChange={(v) => setDest("silent", v)} label={t("smsfwd.silent", "Silent notifications")} />
        </div>
      )}

      {dest.type === "webhook" && (
        <div className="space-y-2">
          <div>
            <label className="mb-1 block text-xs text-text-dim">URL</label>
            <Input value={dest.url} onChange={(e) => setDest("url", e.target.value)} placeholder="https://example.com/hook" />
          </div>
          <div>
            <label className="mb-1 block text-xs text-text-dim">{t("smsfwd.method", "Method")}</label>
            <select
              className="h-9 w-full rounded-md border border-border bg-bg-input px-3 text-sm outline-none"
              value={dest.method}
              onChange={(e) => setDest("method", e.target.value)}
            >
              <option>POST</option>
              <option>PUT</option>
              <option>GET</option>
            </select>
          </div>
        </div>
      )}

      {dest.type === "sms" && (
        <div>
          <label className="mb-1 block text-xs text-text-dim">{t("smsfwd.forwardNumber", "Forward to number")}</label>
          <Input value={dest.forward_number} onChange={(e) => setDest("forward_number", e.target.value)} placeholder="+1234567890" />
        </div>
      )}

      {dest.type === "ntfy" && (
        <div className="space-y-2">
          <div>
            <label className="mb-1 block text-xs text-text-dim">{t("smsfwd.serverUrl", "Server URL")}</label>
            <Input value={dest.url} onChange={(e) => setDest("url", e.target.value)} placeholder="https://ntfy.sh" />
          </div>
          <div>
            <label className="mb-1 block text-xs text-text-dim">{t("smsfwd.topic", "Topic")}</label>
            <Input value={dest.topic} onChange={(e) => setDest("topic", e.target.value)} placeholder="my-topic" />
          </div>
          <div>
            <label className="mb-1 block text-xs text-text-dim">{t("smsfwd.tokenOptional", "Token (optional)")}</label>
            <Input value={dest.token ?? ""} onChange={(e) => setDest("token", e.target.value)} placeholder="tk_..." />
          </div>
        </div>
      )}

      {(dest.type === "discord" || dest.type === "slack") && (
        <div>
          <label className="mb-1 block text-xs text-text-dim">{t("smsfwd.webhookUrl", "Webhook URL")}</label>
          <Input value={dest.webhook_url} onChange={(e) => setDest("webhook_url", e.target.value)} placeholder="https://..." />
        </div>
      )}

      <div className="flex gap-2 pt-1">
        <Button onClick={() => onSave(rule)} loading={saving} disabled={!rule.name.trim()}>
          {initial.id ? t("smsfwd.update", "Update") : t("smsfwd.create", "Create")}
        </Button>
        <Button variant="outline" onClick={onCancel} disabled={saving}>{t("common.cancel", "Cancel")}</Button>
      </div>
    </div>
  );
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function SMSForwardPage() {
  const { t } = useTranslation();
  const { mutate } = useSWRConfig();
  const [tab, setTab] = useState<Tab>("config");
  const tabLabel: Record<Tab, string> = {
    config: t("smsfwd.tabConfig", "config"),
    rules: t("smsfwd.tabRules", "rules"),
    log: t("smsfwd.tabLog", "log"),
  };
  const [err, setErr] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Config tab state
  const [configDraft, setConfigDraft] = useState<ForwardConfig | null>(null);

  // Rules tab state
  const [editingRule, setEditingRule] = useState<(Omit<ForwardRule, "id"> & { id?: number }) | null>(null);
  const [showAddForm, setShowAddForm] = useState(false);

  const { data: cfgResp, isLoading: cfgLoading } = useApi<ConfigResp>("/api/sms/forward/config");
  const { data: logEntries, isLoading: logLoading } = useApi<LogEntry[]>(
    tab === "log" ? "/api/sms/forward/log" : null
  );

  // Sync config draft when fetched
  useEffect(() => {
    if (cfgResp?.config && configDraft === null) {
      setConfigDraft(cfgResp.config);
    }
  }, [cfgResp, configDraft]);

  const config = configDraft ?? cfgResp?.config;
  const rules = config?.rules ?? [];

  function flash(msg: string, isErr = false) {
    if (isErr) { setErr(msg); setOk(null); }
    else { setOk(msg); setErr(null); }
  }

  async function saveConfig() {
    if (!config) return;
    setBusy(true);
    try {
      await apiFetch("/api/sms/forward/config", {
        method: "PUT",
        body: {
          enabled: config.enabled,
          poll_interval_secs: config.poll_interval_secs,
          mark_read_after_forward: config.mark_read_after_forward,
          delete_after_forward: config.delete_after_forward,
        },
      });
      mutate("/api/sms/forward/config");
      flash(t("smsfwd.settingsSaved", "Settings saved"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function createRule(rule: Omit<ForwardRule, "id">) {
    setBusy(true);
    try {
      await apiFetch("/api/sms/forward/rules", {
        method: "POST",
        body: { name: rule.name, enabled: rule.enabled, filter: rule.filter, destination: rule.destination },
      });
      setShowAddForm(false);
      setConfigDraft(null);
      mutate("/api/sms/forward/config");
      flash(t("smsfwd.ruleCreated", "Rule created"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function updateRule(rule: Omit<ForwardRule, "id"> & { id?: number }) {
    if (!rule.id) return;
    setBusy(true);
    try {
      await apiFetch("/api/sms/forward/rules", {
        method: "PUT",
        body: { id: rule.id, name: rule.name, enabled: rule.enabled, filter: rule.filter, destination: rule.destination },
      });
      setEditingRule(null);
      setConfigDraft(null);
      mutate("/api/sms/forward/config");
      flash(t("smsfwd.ruleUpdated", "Rule updated"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function deleteRule(id: number) {
    if (!confirm(t("smsfwd.confirmDeleteRule", "Delete this rule?"))) return;
    setBusy(true);
    try {
      await apiFetch("/api/sms/forward/rules", { method: "DELETE", body: { id } });
      setConfigDraft(null);
      mutate("/api/sms/forward/config");
      flash(t("smsfwd.ruleDeleted", "Rule deleted"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function toggleRule(id: number, enabled: boolean) {
    setBusy(true);
    try {
      await apiFetch("/api/sms/forward/rules/toggle", { method: "PUT", body: { id, enabled } });
      setConfigDraft((prev) =>
        prev
          ? { ...prev, rules: prev.rules.map((r) => (r.id === id ? { ...r, enabled } : r)) }
          : null
      );
      mutate("/api/sms/forward/config");
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function testRule(id: number) {
    setBusy(true);
    try {
      await apiFetch("/api/sms/forward/test", { method: "POST", body: { rule_id: id } });
      flash(t("smsfwd.testSent", "Test message sent"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function clearLog() {
    if (!confirm(t("smsfwd.confirmClear", "Clear all log entries?"))) return;
    setBusy(true);
    try {
      await apiFetch("/api/sms/forward/log/clear", { method: "POST", body: {} });
      mutate("/api/sms/forward/log");
      flash(t("smsfwd.logCleared", "Log cleared"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  async function retryFailed() {
    setBusy(true);
    try {
      await apiFetch("/api/sms/forward/retry", { method: "POST", body: {} });
      mutate("/api/sms/forward/log");
      flash(t("smsfwd.retryQueued", "Retry queued"));
    } catch (e) {
      flash(e instanceof ApiError ? e.message : String(e), true);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <PageHeader title={t("smsfwd.title", "SMS Forwarding")} description={t("smsfwd.desc", "Automatically forward SMS to Telegram, webhooks, and more.")} />

      {err && <div className="mb-3"><ErrorBanner message={err} /></div>}
      {ok && (
        <div className="mb-3 rounded-md border border-success/40 bg-success/10 px-3 py-2 text-sm text-success">
          {ok}
        </div>
      )}

      {/* Tabs */}
      <div className="mb-5 flex gap-1 rounded-lg border border-border bg-bg-card p-1 w-fit">
        {(["config", "rules", "log"] as Tab[]).map((tabId) => (
          <button
            key={tabId}
            onClick={() => setTab(tabId)}
            className={`rounded-md px-4 py-1.5 text-sm font-medium transition ${
              tab === tabId ? "bg-accent text-white" : "text-text-dim hover:text-text"
            }`}
          >
            {tabLabel[tabId]}
          </button>
        ))}
      </div>

      {/* Config Tab */}
      {tab === "config" && (
        <SectionCard title={t("smsfwd.forwardSettings", "Forward Settings")} className="max-w-md">
          {cfgLoading || !config ? (
            <div className="py-6 text-center text-sm text-text-dim">{t("common.loading", "Loading…")}</div>
          ) : (
            <div className="space-y-4">
              <Toggle
                checked={config.enabled}
                onChange={(v) => setConfigDraft((d) => d ? { ...d, enabled: v } : null)}
                label={t("smsfwd.enableForwarding", "Enable SMS Forwarding")}
              />
              <div>
                <label className="mb-1 block text-xs font-medium text-text-dim">{t("smsfwd.pollInterval", "Poll Interval (seconds)")}</label>
                <Input
                  type="number"
                  min={5}
                  max={3600}
                  value={config.poll_interval_secs}
                  onChange={(e) =>
                    setConfigDraft((d) => d ? { ...d, poll_interval_secs: parseInt(e.target.value, 10) || 30 } : null)
                  }
                />
              </div>
              <Toggle
                checked={config.mark_read_after_forward}
                onChange={(v) => setConfigDraft((d) => d ? { ...d, mark_read_after_forward: v } : null)}
                label={t("smsfwd.markRead", "Mark read after forwarding")}
              />
              <Toggle
                checked={config.delete_after_forward}
                onChange={(v) => setConfigDraft((d) => d ? { ...d, delete_after_forward: v } : null)}
                label={t("smsfwd.deleteAfter", "Delete after forwarding")}
              />
              <Button onClick={saveConfig} loading={busy}>{t("smsfwd.saveSettings", "Save Settings")}</Button>
            </div>
          )}
        </SectionCard>
      )}

      {/* Rules Tab */}
      {tab === "rules" && (
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <span className="text-sm text-text-dim">{t("smsfwd.rulesCount", "{{count}} rules", { count: rules.length })}</span>
            <Button size="sm" onClick={() => { setShowAddForm(true); setEditingRule(null); }}>
              <Plus size={13} /> {t("smsfwd.addRule", "Add Rule")}
            </Button>
          </div>

          {showAddForm && !editingRule && (
            <RuleForm
              initial={emptyRule()}
              onSave={(r) => createRule(r as Omit<ForwardRule, "id">)}
              onCancel={() => setShowAddForm(false)}
              saving={busy}
            />
          )}

          {rules.length === 0 && !showAddForm && (
            <SectionCard>
              <div className="py-6 text-center text-sm text-text-dim">{t("smsfwd.noRules", "No rules yet. Add one above.")}</div>
            </SectionCard>
          )}

          {rules.map((rule) =>
            editingRule?.id === rule.id ? (
              <RuleForm
                key={rule.id}
                initial={editingRule}
                onSave={updateRule}
                onCancel={() => setEditingRule(null)}
                saving={busy}
              />
            ) : (
              <SectionCard key={rule.id}>
                <div className="flex items-start gap-3">
                  <Toggle
                    checked={rule.enabled}
                    onChange={(v) => toggleRule(rule.id, v)}
                    disabled={busy}
                  />
                  <div className="flex-1 min-w-0">
                    <div className="font-medium">{rule.name}</div>
                    <div className="mt-0.5 text-xs text-text-dim">
                      {t("smsfwd.filterCol", "Filter")}: <span className="font-mono">{rule.filter.type}</span>
                      {" · "}
                      {t("smsfwd.destCol", "Dest")}: <span className="font-mono">{rule.destination.type}</span>
                    </div>
                  </div>
                  <div className="flex items-center gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      title={t("smsfwd.testRule", "Test rule")}
                      onClick={() => testRule(rule.id)}
                      disabled={busy}
                    >
                      <Play size={13} />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        setEditingRule({ ...rule });
                        setShowAddForm(false);
                      }}
                    >
                      <Pencil size={13} />
                    </Button>
                    <Button
                      variant="danger"
                      size="sm"
                      onClick={() => deleteRule(rule.id)}
                      disabled={busy}
                    >
                      <Trash2 size={13} />
                    </Button>
                  </div>
                </div>
              </SectionCard>
            )
          )}
        </div>
      )}

      {/* Log Tab */}
      {tab === "log" && (
        <div className="space-y-3">
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => mutate("/api/sms/forward/log")}
            >
              <RefreshCw size={13} /> {t("common.refresh", "Refresh")}
            </Button>
            <Button variant="outline" size="sm" onClick={retryFailed} loading={busy}>
              {t("smsfwd.retryFailed", "Retry Failed")}
            </Button>
            <Button variant="danger" size="sm" onClick={clearLog} disabled={busy}>
              <XCircle size={13} /> {t("smsfwd.clearLog", "Clear Log")}
            </Button>
          </div>

          <SectionCard>
            {logLoading ? (
              <div className="py-6 text-center text-sm text-text-dim">{t("common.loading", "Loading…")}</div>
            ) : !logEntries || logEntries.length === 0 ? (
              <div className="py-6 text-center text-sm text-text-dim">{t("smsfwd.noLog", "No log entries.")}</div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-border text-left text-xs text-text-dim">
                      <th className="pb-2 pr-3 font-medium">{t("smsfwd.colTime", "Time")}</th>
                      <th className="pb-2 pr-3 font-medium">{t("smsfwd.colSender", "Sender")}</th>
                      <th className="pb-2 pr-3 font-medium">{t("smsfwd.colPreview", "Preview")}</th>
                      <th className="pb-2 pr-3 font-medium">{t("smsfwd.colRule", "Rule")}</th>
                      <th className="pb-2 pr-3 font-medium">{t("smsfwd.colDest", "Dest")}</th>
                      <th className="pb-2 font-medium">{t("smsfwd.colStatus", "Status")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {logEntries.map((entry, i) => (
                      <tr
                        key={entry.id ?? `${entry.timestamp}-${i}`}
                        className="border-b border-border/50 last:border-0"
                        title={entry.error ?? ""}
                      >
                        <td className="py-2 pr-3 font-mono text-xs text-text-dim">{fmtTs(entry.timestamp)}</td>
                        <td className="py-2 pr-3 font-mono text-xs">{entry.sender}</td>
                        <td className="max-w-[14rem] py-2 pr-3 text-xs text-text-dim">
                          <span className="line-clamp-1">{decodeSms(entry.content_preview)}</span>
                        </td>
                        <td className="py-2 pr-3 text-xs">{entry.rule_name}</td>
                        <td className="py-2 pr-3 text-xs font-mono">{entry.destination_type}</td>
                        <td className="py-2">
                          <span
                            className={`rounded-full px-2 py-0.5 text-xs font-medium ${
                              entry.success
                                ? "bg-success/15 text-success"
                                : "bg-error/15 text-error"
                            }`}
                          >
                            {entry.success ? t("smsfwd.ok", "OK") : t("smsfwd.failed", "Failed")}
                          </span>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </SectionCard>
        </div>
      )}
    </>
  );
}
