"use client";

import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";

interface LanConfig {
  lan_ipaddr?: string;
  lan_netmask?: string;
  dhcp_enable?: string;
  dhcp_start?: string;
  dhcp_end?: string;
  dhcp_lease_time?: string;
}

function isValidIPv4(s: string): boolean {
  const parts = s.split(".");
  if (parts.length !== 4) return false;
  return parts.every((p) => /^\d+$/.test(p) && Number(p) >= 0 && Number(p) <= 255);
}

export default function LanPage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<LanConfig>("/api/router/lan");

  const [ipaddr, setIpaddr] = useState("");
  const [netmask, setNetmask] = useState("");
  const [dhcpEnable, setDhcpEnable] = useState(true);
  const [dhcpStart, setDhcpStart] = useState("");
  const [dhcpEnd, setDhcpEnd] = useState("");
  const [leaseTime, setLeaseTime] = useState("");

  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const [showConfirm, setShowConfirm] = useState(false);
  const [formErr, setFormErr] = useState<string | null>(null);

  useEffect(() => {
    if (!data) return;
    setIpaddr(data.lan_ipaddr ?? "");
    setNetmask(data.lan_netmask ?? "");
    setDhcpEnable(data.dhcp_enable === "1");
    setDhcpStart(data.dhcp_start ?? "");
    setDhcpEnd(data.dhcp_end ?? "");
    setLeaseTime(data.dhcp_lease_time ?? "");
  }, [data]);

  function validate(): string | null {
    if (!isValidIPv4(ipaddr)) return t("lan.errInvalidIp", "Invalid LAN IP address");
    if (!isValidIPv4(netmask)) return t("lan.errInvalidNetmask", "Invalid netmask");
    const lease = Number(leaseTime);
    if (!Number.isInteger(lease) || lease <= 0) return t("lan.errInvalidLease", "Lease time must be a positive integer");
    return null;
  }

  function handleApplyClick() {
    const err = validate();
    if (err) { setFormErr(err); return; }
    setFormErr(null);
    setShowConfirm(true);
  }

  async function doApply() {
    setShowConfirm(false);
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch("/api/router/lan", {
        method: "PUT",
        body: {
          lan_ipaddr: ipaddr,
          lan_netmask: netmask,
          dhcp_enable: dhcpEnable ? "1" : "0",
          dhcp_start: dhcpStart,
          dhcp_end: dhcpEnd,
          dhcp_lease_time: leaseTime,
        },
      });
      setMsg({ text: t("lan.applied", "LAN settings applied"), err: false });
      mutate();
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : String(e), err: true });
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <PageHeader title={t("lan.title", "LAN Settings")} description={t("lan.desc", "Configure the router's LAN IP and DHCP server.")} />

      {error && <ErrorBanner message={error instanceof ApiError ? error.message : String(error)} />}
      {msg && (
        <div className={`mb-4 rounded-md px-3 py-2 text-sm ${msg.err ? "border border-error/40 bg-error/10 text-error" : "border border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}
      {formErr && <ErrorBanner message={formErr} />}

      {showConfirm && (
        <div className="mb-4 rounded-xl border border-warning/40 bg-warning/10 p-4">
          <p className="mb-3 text-sm font-medium text-warning">{t("lan.confirmDisconnect", "Changing LAN settings will disconnect clients. Continue?")}</p>
          <div className="flex gap-2">
            <Button variant="danger" onClick={doApply} loading={busy}>{t("lan.applyAnyway", "Apply Anyway")}</Button>
            <Button variant="ghost" onClick={() => setShowConfirm(false)}>{t("lan.cancel", "Cancel")}</Button>
          </div>
        </div>
      )}

      <SectionCard title={t("lan.ipConfig", "IP Configuration")} className="mb-4">
        <div className="grid gap-3 sm:grid-cols-2">
          <div>
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("lan.lanIpAddress", "LAN IP Address")}</label>
            <Input value={ipaddr} onChange={(e) => setIpaddr(e.target.value)} placeholder="192.168.0.1" />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("lan.subnetMask", "Subnet Mask")}</label>
            <Input value={netmask} onChange={(e) => setNetmask(e.target.value)} placeholder="255.255.255.0" />
          </div>
        </div>
      </SectionCard>

      <SectionCard title={t("lan.dhcpServer", "DHCP Server")} className="mb-4">
        <div className="mb-4 flex items-center gap-3">
          <Toggle checked={dhcpEnable} onChange={setDhcpEnable} label={dhcpEnable ? t("lan.enabled", "Enabled") : t("lan.disabled", "Disabled")} />
        </div>
        <div className="grid gap-3 sm:grid-cols-3">
          <div>
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("lan.startAddress", "Start Address")}</label>
            <Input value={dhcpStart} onChange={(e) => setDhcpStart(e.target.value)} placeholder="192.168.0.100" disabled={!dhcpEnable} />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("lan.endAddress", "End Address")}</label>
            <Input value={dhcpEnd} onChange={(e) => setDhcpEnd(e.target.value)} placeholder="192.168.0.200" disabled={!dhcpEnable} />
          </div>
          <div>
            <label className="mb-1 block text-xs font-medium text-text-dim">{t("lan.leaseTimeSeconds", "Lease Time (seconds)")}</label>
            <Input value={leaseTime} onChange={(e) => setLeaseTime(e.target.value)} placeholder="86400" disabled={!dhcpEnable} />
          </div>
        </div>
      </SectionCard>

      <Button onClick={handleApplyClick} loading={busy}>{t("lan.apply", "Apply")}</Button>
    </>
  );
}
