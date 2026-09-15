"use client";

import { useState, useEffect } from "react";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";

interface FirewallConfig {
  firewall_enable?: string;
  level?: string;
  nat_enable?: string;
  dmz_enable?: string;
  dmz_ip?: string;
  dmz_hostname?: string;
  portforward_enable?: string;
  wan_ping_enable?: string;
  remote_web_access_enable?: string;
}

interface PortForwardRule {
  id: string;
  name?: string;
  protocol?: string;
  wan_port?: string;
  lan_ip?: string;
  lan_port?: string;
  enabled?: string;
}

interface FilterRule {
  id?: string;
  name?: string;
  protocol?: string;
  src_ip?: string;
  dst_ip?: string;
  dst_port?: string;
  action?: string;
}

interface UpnpData {
  upnp_switch?: string;
}

function isValidIPv4(s: string): boolean {
  const parts = s.split(".");
  if (parts.length !== 4) return false;
  return parts.every((p) => /^\d+$/.test(p) && Number(p) >= 0 && Number(p) <= 255);
}

export default function FirewallPage() {
  const { t } = useTranslation();
  const { data: config, error: configErr, mutate: mutateConfig } = useApi<FirewallConfig>("/api/router/firewall");
  const { data: upnpData, mutate: mutateUpnp } = useApi<UpnpData>("/api/router/firewall/upnp");
  const { data: portRules, mutate: mutatePF } = useApi<PortForwardRule[]>("/api/router/firewall/port-forward");
  const { data: filterRules } = useApi<FilterRule[]>("/api/router/firewall/filter-rules");

  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);

  // DMZ local state
  const [dmzEnabled, setDmzEnabled] = useState(false);
  const [dmzIp, setDmzIp] = useState("");
  const [dmzConfirm, setDmzConfirm] = useState(false);

  // Port forward form
  const [showAddPF, setShowAddPF] = useState(false);
  const [pfName, setPfName] = useState("");
  const [pfProtocol, setPfProtocol] = useState("TCP");
  const [pfWanPort, setPfWanPort] = useState("");
  const [pfLanIp, setPfLanIp] = useState("");
  const [pfLanPort, setPfLanPort] = useState("");
  const [pfEnabled, setPfEnabled] = useState(true);

  // Delete confirm
  const [deleteTarget, setDeleteTarget] = useState<PortForwardRule | null>(null);

  useEffect(() => {
    if (!config) return;
    setDmzEnabled(config.dmz_enable === "1");
    setDmzIp(config.dmz_ip ?? config.dmz_hostname ?? "");
  }, [config]);

  function showMsg(text: string, err: boolean) {
    setMsg({ text, err });
  }

  async function toggleFirewall(enabled: boolean) {
    setBusy(true);
    try {
      await apiFetch("/api/router/firewall/switch", { method: "PUT", body: { firewall_switch: enabled ? "1" : "0" } });
      showMsg(enabled ? t("firewall.msgFirewallEnabled", "Firewall enabled") : t("firewall.msgFirewallDisabled", "Firewall disabled"), false);
      mutateConfig();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally { setBusy(false); }
  }

  async function setLevel(level: string) {
    setBusy(true);
    try {
      await apiFetch("/api/router/firewall/level", { method: "PUT", body: { firewall_level: level } });
      showMsg(t("firewall.msgLevelSet", "Level set to {{level}}", { level }), false);
      mutateConfig();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally { setBusy(false); }
  }

  async function toggleNat(enabled: boolean) {
    setBusy(true);
    try {
      await apiFetch("/api/router/firewall/nat", { method: "PUT", body: { nat_switch: enabled ? "1" : "0" } });
      showMsg(enabled ? t("firewall.msgNatEnabled", "NAT enabled") : t("firewall.msgNatDisabled", "NAT disabled"), false);
      mutateConfig();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally { setBusy(false); }
  }

  async function toggleUpnp(enabled: boolean) {
    setBusy(true);
    try {
      await apiFetch("/api/router/firewall/upnp", { method: "PUT", body: { upnp_switch: enabled ? "1" : "0" } });
      showMsg(enabled ? t("firewall.msgUpnpEnabled", "UPnP enabled") : t("firewall.msgUpnpDisabled", "UPnP disabled"), false);
      mutateUpnp();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally { setBusy(false); }
  }

  async function togglePortForwardSwitch(enabled: boolean) {
    setBusy(true);
    try {
      await apiFetch("/api/router/firewall/port-forward/switch", { method: "PUT", body: { port_forward_switch: enabled ? "1" : "0" } });
      showMsg(enabled ? t("firewall.msgPortForwardingEnabled", "Port forwarding enabled") : t("firewall.msgPortForwardingDisabled", "Port forwarding disabled"), false);
      mutateConfig();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally { setBusy(false); }
  }

  function handleDmzToggle(val: boolean) {
    setDmzEnabled(val);
    if (val) setDmzConfirm(true);
  }

  async function applyDmz() {
    setDmzConfirm(false);
    if (!isValidIPv4(dmzIp) && dmzEnabled) {
      showMsg(t("firewall.msgInvalidDmzIp", "Invalid DMZ host IP"), true);
      return;
    }
    setBusy(true);
    try {
      await apiFetch("/api/router/firewall/dmz", { method: "PUT", body: { dmz_enabled: dmzEnabled ? "1" : "0", dmz_ip: dmzIp } });
      showMsg(t("firewall.msgDmzUpdated", "DMZ settings updated"), false);
      mutateConfig();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally { setBusy(false); }
  }

  async function addPortForward() {
    if (!pfName || !pfWanPort || !pfLanIp || !pfLanPort) {
      showMsg(t("firewall.msgAllFieldsRequired", "All port forward fields required"), true);
      return;
    }
    if (!isValidIPv4(pfLanIp)) { showMsg(t("firewall.msgInvalidLanIp", "Invalid LAN IP"), true); return; }
    setBusy(true);
    try {
      await apiFetch("/api/router/firewall/port-forward", {
        method: "POST",
        body: { action: "add", name: pfName, protocol: pfProtocol, wan_port: pfWanPort, lan_ip: pfLanIp, lan_port: pfLanPort, enabled: pfEnabled ? "1" : "0" },
      });
      showMsg(t("firewall.msgRuleAdded", "Rule added"), false);
      setPfName(""); setPfWanPort(""); setPfLanIp(""); setPfLanPort("");
      setShowAddPF(false);
      mutatePF();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally { setBusy(false); }
  }

  async function deletePortForward(rule: PortForwardRule) {
    setDeleteTarget(null);
    setBusy(true);
    try {
      await apiFetch("/api/router/firewall/port-forward", { method: "POST", body: { action: "delete", id: rule.id } });
      showMsg(t("firewall.msgRuleDeleted", "Rule deleted"), false);
      mutatePF();
    } catch (e) {
      showMsg(e instanceof ApiError ? e.message : String(e), true);
    } finally { setBusy(false); }
  }

  const firewallOn = config?.firewall_enable === "1";
  const natOn = config?.nat_enable === "1";
  const pfSwitchOn = config?.portforward_enable === "1";
  const upnpOn = upnpData?.upnp_switch === "1";
  const level = config?.level ?? "medium";

  return (
    <>
      <PageHeader title={t("firewall.title", "Firewall")} description={t("firewall.desc", "Manage firewall, NAT, DMZ, UPnP and port forwarding.")} />

      {configErr && <ErrorBanner message={configErr instanceof ApiError ? configErr.message : String(configErr)} />}
      {msg && (
        <div className={`mb-4 rounded-md px-3 py-2 text-sm ${msg.err ? "border border-error/40 bg-error/10 text-error" : "border border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      <SectionCard title={t("firewall.switches", "Switches")} className="mb-4">
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <span className="text-sm">{t("firewall.firewall", "Firewall")}</span>
            <Toggle checked={firewallOn} onChange={toggleFirewall} disabled={busy} />
          </div>
          <div className="flex items-center justify-between">
            <div>
              <span className="text-sm">{t("firewall.firewallLevel", "Firewall Level")}</span>
              <p className="text-xs text-text-dim">{t("firewall.current", "Current: {{level}}", { level })}</p>
            </div>
            <div className="flex gap-2">
              {(["low", "medium", "high"] as const).map((l) => (
                <Button
                  key={l}
                  variant={level === l ? "primary" : "outline"}
                  size="sm"
                  onClick={() => setLevel(l)}
                  disabled={busy}
                >
                  {l === "low" ? t("firewall.levelLow", "low") : l === "medium" ? t("firewall.levelMedium", "medium") : t("firewall.levelHigh", "high")}
                </Button>
              ))}
            </div>
          </div>
          <div className="flex items-center justify-between">
            <span className="text-sm">NAT</span>
            <Toggle checked={natOn} onChange={toggleNat} disabled={busy} />
          </div>
          <div className="flex items-center justify-between">
            <span className="text-sm">UPnP</span>
            <Toggle checked={upnpOn} onChange={toggleUpnp} disabled={busy} />
          </div>
          <div className="flex items-center justify-between">
            <span className="text-sm">{t("firewall.portForwarding", "Port Forwarding")}</span>
            <Toggle checked={pfSwitchOn} onChange={togglePortForwardSwitch} disabled={busy} />
          </div>
        </div>
      </SectionCard>

      <SectionCard title="DMZ" className="mb-4">
        {dmzConfirm && (
          <div className="mb-3 rounded-md border border-warning/40 bg-warning/10 p-3">
            <p className="mb-2 text-sm text-warning">{t("firewall.dmzWarning", "Exposing a host via DMZ bypasses firewall protections. Continue?")}</p>
            <div className="flex gap-2">
              <Button variant="danger" size="sm" onClick={applyDmz}>{t("firewall.enableDmz", "Enable DMZ")}</Button>
              <Button variant="ghost" size="sm" onClick={() => { setDmzConfirm(false); setDmzEnabled(false); }}>{t("firewall.cancel", "Cancel")}</Button>
            </div>
          </div>
        )}
        <div className="flex items-center gap-4 mb-3">
          <Toggle checked={dmzEnabled} onChange={handleDmzToggle} disabled={busy} label={dmzEnabled ? t("firewall.enabled", "Enabled") : t("firewall.disabled", "Disabled")} />
        </div>
        <div>
          <label className="mb-1 block text-xs font-medium text-text-dim">{t("firewall.dmzHostIp", "DMZ Host IP")}</label>
          <div className="flex gap-2">
            <Input value={dmzIp} onChange={(e) => setDmzIp(e.target.value)} placeholder="192.168.0.x" className="max-w-xs" disabled={!dmzEnabled} />
            {!dmzConfirm && (
              <Button variant="outline" onClick={() => applyDmz()} loading={busy} disabled={busy}>{t("firewall.apply", "Apply")}</Button>
            )}
          </div>
        </div>
      </SectionCard>

      <SectionCard title={t("firewall.portForwarding", "Port Forwarding")} className="mb-4">
        <div className="mb-3 flex items-center justify-between">
          <span className="text-sm text-text-dim">{t("firewall.rulesCount", "{{n}} rules", { n: (portRules ?? []).length })}</span>
          <Button variant="outline" size="sm" onClick={() => setShowAddPF((v) => !v)}>
            {showAddPF ? t("firewall.cancel", "Cancel") : t("firewall.addRule", "Add Rule")}
          </Button>
        </div>

        {showAddPF && (
          <div className="mb-4 rounded-lg border border-border bg-bg-elevated p-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <label className="mb-1 block text-xs font-medium text-text-dim">{t("firewall.name", "Name")}</label>
                <Input value={pfName} onChange={(e) => setPfName(e.target.value)} placeholder={t("firewall.ruleNamePlaceholder", "Rule name")} />
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-dim">{t("firewall.protocol", "Protocol")}</label>
                <select
                  className="h-9 w-full rounded-md border border-border bg-bg-input px-3 text-sm outline-none focus:border-accent"
                  value={pfProtocol}
                  onChange={(e) => setPfProtocol(e.target.value)}
                >
                  <option value="TCP">TCP</option>
                  <option value="UDP">UDP</option>
                  <option value="Both">{t("firewall.both", "Both")}</option>
                </select>
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-dim">{t("firewall.wanPort", "WAN Port")}</label>
                <Input value={pfWanPort} onChange={(e) => setPfWanPort(e.target.value)} placeholder="8080" />
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-dim">{t("firewall.lanIp", "LAN IP")}</label>
                <Input value={pfLanIp} onChange={(e) => setPfLanIp(e.target.value)} placeholder="192.168.0.x" />
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-dim">{t("firewall.lanPort", "LAN Port")}</label>
                <Input value={pfLanPort} onChange={(e) => setPfLanPort(e.target.value)} placeholder="80" />
              </div>
              <div className="flex items-end">
                <Toggle checked={pfEnabled} onChange={setPfEnabled} label={t("firewall.enabled", "Enabled")} />
              </div>
            </div>
            <div className="mt-3">
              <Button onClick={addPortForward} loading={busy}>{t("firewall.saveRule", "Save Rule")}</Button>
            </div>
          </div>
        )}

        {(portRules ?? []).length > 0 ? (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs text-text-dim">
                  <th className="pb-2 pr-3">{t("firewall.name", "Name")}</th>
                  <th className="pb-2 pr-3">{t("firewall.proto", "Proto")}</th>
                  <th className="pb-2 pr-3">WAN</th>
                  <th className="pb-2 pr-3">{t("firewall.lanIp", "LAN IP")}</th>
                  <th className="pb-2 pr-3">{t("firewall.lanPort", "LAN Port")}</th>
                  <th className="pb-2"></th>
                </tr>
              </thead>
              <tbody>
                {(portRules ?? []).map((r) => (
                  <tr key={r.id} className="border-b border-border/60 last:border-0">
                    <td className="py-1.5 pr-3">{r.name ?? "—"}</td>
                    <td className="py-1.5 pr-3">{r.protocol ?? "—"}</td>
                    <td className="py-1.5 pr-3 font-mono">{r.wan_port ?? "—"}</td>
                    <td className="py-1.5 pr-3 font-mono">{r.lan_ip ?? "—"}</td>
                    <td className="py-1.5 pr-3 font-mono">{r.lan_port ?? "—"}</td>
                    <td className="py-1.5">
                      <button
                        onClick={() => setDeleteTarget(r)}
                        className="text-text-dim hover:text-error transition"
                        aria-label={t("firewall.deleteRule", "Delete rule")}
                      >
                        <Trash2 className="h-4 w-4" />
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <p className="text-sm text-text-dim">{t("firewall.noPortForwardRules", "No port forwarding rules.")}</p>
        )}

        {deleteTarget && (
          <div className="mt-3 rounded-md border border-error/40 bg-error/10 p-3">
            <p className="mb-2 text-sm text-error">{t("firewall.confirmDelete", 'Delete rule "{{name}}"?', { name: deleteTarget.name })}</p>
            <div className="flex gap-2">
              <Button variant="danger" size="sm" onClick={() => deletePortForward(deleteTarget)} loading={busy}>{t("firewall.delete", "Delete")}</Button>
              <Button variant="ghost" size="sm" onClick={() => setDeleteTarget(null)}>{t("firewall.cancel", "Cancel")}</Button>
            </div>
          </div>
        )}
      </SectionCard>

      <SectionCard title={t("firewall.filterRulesTitle", "Filter Rules (read-only)")}>
        {(filterRules ?? []).length > 0 ? (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs text-text-dim">
                  <th className="pb-2 pr-3">{t("firewall.name", "Name")}</th>
                  <th className="pb-2 pr-3">{t("firewall.proto", "Proto")}</th>
                  <th className="pb-2 pr-3">{t("firewall.srcIp", "Src IP")}</th>
                  <th className="pb-2 pr-3">{t("firewall.dstIp", "Dst IP")}</th>
                  <th className="pb-2 pr-3">{t("firewall.dstPort", "Dst Port")}</th>
                  <th className="pb-2">{t("firewall.action", "Action")}</th>
                </tr>
              </thead>
              <tbody>
                {(filterRules ?? []).map((r, i) => (
                  <tr key={r.id ?? i} className="border-b border-border/60 last:border-0">
                    <td className="py-1.5 pr-3">{r.name ?? "—"}</td>
                    <td className="py-1.5 pr-3">{r.protocol ?? "—"}</td>
                    <td className="py-1.5 pr-3 font-mono">{r.src_ip ?? "—"}</td>
                    <td className="py-1.5 pr-3 font-mono">{r.dst_ip ?? "—"}</td>
                    <td className="py-1.5 pr-3 font-mono">{r.dst_port ?? "—"}</td>
                    <td className="py-1.5">{r.action ?? "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <p className="text-sm text-text-dim">{t("firewall.noFilterRules", "No filter rules configured.")}</p>
        )}
      </SectionCard>
    </>
  );
}
