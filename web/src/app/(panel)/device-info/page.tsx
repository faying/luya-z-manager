"use client";
// Device info (new design): a read-only "look" page. Identity (IMEI, IMSI,
// ICCID — full, in mono, never shortened), network addresses, and system
// facts. Every request is made once when the page opens (no polling,
// controls-inventory §/device-info). Each group shows its own read error with
// a retry and keeps whatever it already has.
//
// Hostname / kernel: the old page read them from /api/device/system, which is
// `ubus call system info` and never carries them (schemas/device.ts). They now
// come from GET /api/device (handlers.rs `device`: /proc/sys/kernel/hostname
// and /proc/version), which the agent builds itself.
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import type { SimImei, SimInfo } from "@/lib/api/schemas/sim";
import type { NetifdStatus, NetworkSignal } from "@/lib/api/schemas/network";
import type { DeviceSystem } from "@/lib/api/schemas/device";
import { Button, Group, Row, StatusMark } from "@/components/nd";

/** GET /api/device — zte-agent handlers.rs `device` (system.rs read_device_info).
 *  Not in lib/api/schemas yet. */
interface AgentDevice {
  hostname: string;
  uptime_secs: number;
  load_avg: [number, number, number];
  /** Whole /proc/version line. */
  kernel: string;
}

type AnyApi = { data?: unknown; error?: unknown; mutate: () => Promise<unknown> };

/** Value cell: skeleton while the first read is out, "—" for unknown. */
function cell(api: AnyApi, v: ReactNode | null | undefined): ReactNode {
  if (api.data === undefined && !api.error) return <span className="nd-skel" style={{ width: "10ch" }} />;
  return v === undefined || v === null || v === "" ? "—" : v;
}

function fmtMB(bytes?: number): string | null {
  if (!bytes) return null;
  return `${Math.round(bytes / 1024 / 1024)} MB`;
}

/** "Linux version 5.4.210 (…) …" → "5.4.210". */
function kernelRelease(v?: string): string | null {
  if (!v) return null;
  const m = /Linux version (\S+)/.exec(v);
  return m ? m[1] : v;
}

/** One line under a group naming what could not be read, with a retry. */
function ReadErrors({ items }: { items: { name: string; api: AnyApi }[] }) {
  const { t } = useTranslation();
  const failed = items.filter((i) => i.api.error);
  if (failed.length === 0) return null;
  return (
    <div className="mt-2 flex flex-wrap items-center gap-2 px-1" role="alert">
      <span className="nd-aux">
        <StatusMark tone="warn">
          {t("devinfo.readFailed", "Could not read: {{what}}", { what: failed.map((f) => f.name).join(" · ") })}
        </StatusMark>
      </span>
      <Button variant="secondary" size="sm" onPress={() => failed.forEach((f) => f.api.mutate())}>
        {t("common.retry", "Retry")}
      </Button>
    </div>
  );
}

export default function DeviceInfoPage() {
  const { t } = useTranslation();
  const once = { revalidateOnFocus: false } as const;
  const sim = useApi<SimInfo>("/api/sim/info", once);
  const imei = useApi<SimImei>("/api/sim/imei", once);
  const sig = useApi<NetworkSignal>("/api/network/signal", once);
  const wan = useApi<NetifdStatus>("/api/network/wan", once);
  const wan6 = useApi<NetifdStatus>("/api/network/wan6", once);
  const lan = useApi<NetifdStatus>("/api/network/lan-status", once);
  const sys = useApi<DeviceSystem>("/api/device/system", once);
  const dev = useApi<AgentDevice>("/api/device", once);

  // Firmware SIM state words (value set unconfirmed); unknown ones show as-is.
  function simWord(v?: string): string | undefined {
    if (!v) return v;
    const s = v.toLowerCase();
    if (s.includes("waitpuk")) return t("devinfo.simPuk", "PUK needed");
    if (s.includes("waitpin")) return t("devinfo.simPin", "PIN needed");
    if (s.includes("absent") || s.includes("undetected")) return t("devinfo.simAbsent", "No SIM");
    if (s.includes("init_complete") || s.includes("ready")) return t("devinfo.simReady", "Ready");
    return v;
  }

  // Uptime from `system info`; /api/device has it too, used if that one fails.
  const upSecs = sys.data?.uptime ?? dev.data?.uptime_secs;
  const upApi: AnyApi = !sys.data && dev.data ? dev : sys;
  function fmtUptime(secs?: number): string | null {
    if (!secs) return null;
    const d = Math.floor(secs / 86400);
    const h = Math.floor((secs % 86400) / 3600);
    const m = Math.floor((secs % 3600) / 60);
    if (d) return t("devinfo.upDH", "{{d}} d {{h}} h", { d, h });
    if (h) return t("devinfo.upHM", "{{h}} h {{m}} min", { h, m });
    return t("devinfo.upM", "{{m}} min", { m });
  }

  const dns = wan.data?.["dns-server"]?.filter(Boolean) ?? [];
  const kernel = dev.data?.kernel;

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("devinfo.title", "Device Info")}</h1>
      <div className="grid gap-4 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)] lg:gap-6">
        <div className="grid content-start gap-4">
          <section>
            <Group title={t("devinfo.identity", "Identity")}>
              <Row label="IMEI" value={cell(imei, imei.data?.imei)} mono />
              <Row label="IMSI" value={cell(sim, sim.data?.sim_imsi)} mono />
              <Row label="ICCID" value={cell(sim, sim.data?.sim_iccid)} mono />
              <Row label={t("devinfo.simState", "SIM State")} value={cell(sim, simWord(sim.data?.sim_states))} />
              <Row
                label={t("devinfo.operator", "Operator")}
                value={cell(sig, sig.data?.network_provider_fullname || sig.data?.network_provider)}
              />
              <Row label={t("devinfo.networkType", "Network Type")} value={cell(sig, sig.data?.network_type)} />
            </Group>
            <ReadErrors
              items={[
                { name: "IMEI", api: imei },
                { name: t("devinfo.simWord", "SIM"), api: sim },
                { name: t("devinfo.operator", "Operator"), api: sig },
              ]}
            />
          </section>

          <section>
            <Group title={t("devinfo.network", "Network")}>
              <Row label="IPv4 (WAN)" value={cell(wan, wan.data?.["ipv4-address"]?.[0]?.address)} mono />
              <Row label={t("devinfo.gateway", "Gateway")} value={cell(wan, wan.data?.route?.[0]?.nexthop)} mono />
              <Row label="DNS" value={cell(wan, dns.length ? dns.join(", ") : null)} mono />
              <Row label="IPv6 (WAN)" value={cell(wan6, wan6.data?.["ipv6-address"]?.[0]?.address)} mono />
              <Row label={t("devinfo.lanIp", "LAN IP")} value={cell(lan, lan.data?.["ipv4-address"]?.[0]?.address)} mono />
            </Group>
            <ReadErrors
              items={[
                { name: "WAN", api: wan },
                { name: "WAN IPv6", api: wan6 },
                { name: "LAN", api: lan },
              ]}
            />
          </section>
        </div>

        <div className="grid content-start gap-4">
          <section>
            <Group title={t("devinfo.system", "System")}>
              <Row label={t("devinfo.hostname", "Hostname")} value={cell(dev, dev.data?.hostname)} mono />
              <Row
                label={t("devinfo.kernel", "Kernel")}
                sub={kernel ? <span className="nd-mono break-all">{kernel}</span> : undefined}
                value={cell(dev, kernelRelease(kernel))}
                mono
              />
              <Row label={t("devinfo.uptime", "Uptime")} value={cell(upApi, fmtUptime(upSecs))} />
              <Row label={t("devinfo.memTotal", "Memory Total")} value={cell(sys, fmtMB(sys.data?.memory?.total))} />
              <Row label={t("devinfo.memFree", "Memory Free")} value={cell(sys, fmtMB(sys.data?.memory?.free))} />
              <Row label={t("devinfo.memAvail", "Memory Available")} value={cell(sys, fmtMB(sys.data?.memory?.available))} />
            </Group>
            <ReadErrors
              items={[
                { name: t("devinfo.systemInfo", "system info"), api: sys },
                { name: t("devinfo.hostKernel", "hostname and kernel"), api: dev },
              ]}
            />
          </section>
        </div>
      </div>
    </>
  );
}
