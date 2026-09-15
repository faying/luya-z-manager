"use client";

import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";

interface SimInfo {
  sim_imsi?: string;
  sim_iccid?: string;
  sim_states?: string;
}
interface ImeiInfo {
  imei?: string;
}
interface WanInfo {
  "ipv4-address"?: { address?: string; mask?: number }[];
  "dns-server"?: string[];
  route?: { nexthop?: string }[];
}
interface Wan6Info {
  "ipv6-address"?: { address?: string; mask?: number }[];
}
interface LanStatus {
  "ipv4-address"?: { address?: string; mask?: number }[];
}
interface SignalInfo {
  network_provider_fullname?: string;
  network_provider?: string;
  network_type?: string;
}
interface SystemInfo {
  hostname?: string;
  kernel?: string;
  uptime?: number;
  memory?: { total?: number; free?: number; available?: number };
}

function Row({ k, v }: { k: string; v?: string | number }) {
  return (
    <div className="flex justify-between border-b border-border/60 py-1.5 text-sm last:border-0">
      <span className="text-text-dim">{k}</span>
      <span className="font-mono text-right">{v ?? "—"}</span>
    </div>
  );
}

function fmtBytes(bytes?: number): string {
  if (!bytes) return "—";
  const mb = bytes / 1024 / 1024;
  return `${mb.toFixed(0)} MB`;
}

function fmtUptime(secs?: number): string {
  if (!secs) return "—";
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m`;
}

export default function DeviceInfoPage() {
  const { t } = useTranslation();
  const { data: sim, error: simErr } = useApi<SimInfo>("/api/sim/info");
  const { data: imei } = useApi<ImeiInfo>("/api/sim/imei");
  const { data: wan } = useApi<WanInfo>("/api/network/wan");
  const { data: wan6 } = useApi<Wan6Info>("/api/network/wan6");
  const { data: lan } = useApi<LanStatus>("/api/network/lan-status");
  const { data: sig } = useApi<SignalInfo>("/api/network/signal");
  const { data: sys } = useApi<SystemInfo>("/api/device/system");

  return (
    <>
      <PageHeader title={t("devinfo.title", "Device Info")} description={t("devinfo.desc", "Identity, network, and system details.")} />

      {simErr && <div className="mb-3"><ErrorBanner message={String(simErr.message ?? simErr)} /></div>}

      <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
        <SectionCard title={t("devinfo.identity", "Identity")}>
          <Row k="IMEI" v={imei?.imei} />
          <Row k="IMSI" v={sim?.sim_imsi} />
          <Row k="ICCID" v={sim?.sim_iccid} />
          <Row k={t("devinfo.simState", "SIM State")} v={sim?.sim_states} />
          <Row k={t("devinfo.operator", "Operator")} v={sig?.network_provider_fullname ?? sig?.network_provider} />
          <Row k={t("devinfo.networkType", "Network Type")} v={sig?.network_type} />
        </SectionCard>

        <SectionCard title={t("devinfo.network", "Network")}>
          <Row k="IPv4 (WAN)" v={wan?.["ipv4-address"]?.[0]?.address} />
          <Row k={t("devinfo.gateway", "Gateway")} v={wan?.route?.[0]?.nexthop} />
          <Row k="DNS" v={wan?.["dns-server"]?.[0]} />
          <Row k="IPv6 (WAN)" v={wan6?.["ipv6-address"]?.[0]?.address} />
          <Row k={t("devinfo.lanIp", "LAN IP")} v={lan?.["ipv4-address"]?.[0]?.address} />
        </SectionCard>

        <SectionCard title={t("devinfo.system", "System")}>
          <Row k={t("devinfo.hostname", "Hostname")} v={sys?.hostname} />
          <Row k={t("devinfo.kernel", "Kernel")} v={sys?.kernel} />
          <Row k={t("devinfo.uptime", "Uptime")} v={fmtUptime(sys?.uptime)} />
          <Row k={t("devinfo.memTotal", "Memory Total")} v={fmtBytes(sys?.memory?.total)} />
          <Row k={t("devinfo.memFree", "Memory Free")} v={fmtBytes(sys?.memory?.free)} />
          <Row k={t("devinfo.memAvail", "Memory Available")} v={fmtBytes(sys?.memory?.available)} />
        </SectionCard>
      </div>
    </>
  );
}
