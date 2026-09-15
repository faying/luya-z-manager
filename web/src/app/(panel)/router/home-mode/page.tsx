"use client";

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner, Status } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { Plus, Trash2, RefreshCw, Wifi, WifiOff } from "lucide-react";

interface HomeModeData {
  enabled: boolean;
  ssids: string[];
  using_default: boolean;
  default_ssids: string[];
  mode: string; // "home" | "normal"
  wifi_off: boolean;
  check_every: number; // recheck interval in minutes while at home
  exit_misses: number; // consecutive missed rechecks before Wi-Fi returns
}

interface ScanNet {
  ssid: string;
  signal: number;
}

interface LogData {
  events: string; // Wi-Fi on/off transitions
  scans: string; // periodic scan rechecks
}

type Msg = { text: string; err: boolean; undo?: () => void };

export default function HomeModePage() {
  const { t } = useTranslation();
  const { data, error, mutate } = useApi<HomeModeData>("/api/homemode", {
    refreshInterval: 15000,
  });
  const { data: logData, mutate: mutateLog } = useApi<LogData>("/api/homemode/log", {
    refreshInterval: 10000,
  });

  const [newSsid, setNewSsid] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<Msg | null>(null);

  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState<ScanNet[] | null>(null);
  const [scanNote, setScanNote] = useState<string | null>(null);

  // Detection tunables — seeded from the device once, then locally editable.
  const [cfg, setCfg] = useState<{ every: string; misses: string } | null>(null);
  useEffect(() => {
    if (data && cfg === null) {
      setCfg({ every: String(data.check_every), misses: String(data.exit_misses) });
    }
  }, [data, cfg]);

  const ssids = data?.ssids ?? [];

  function flash(text: string, err = false, undo?: () => void) {
    setMsg({ text, err, undo });
    if (!err) setTimeout(() => setMsg(null), undo ? 6000 : 3000);
  }

  // Write to the agent. Returns true on success; surfaces errors itself.
  async function persist(body: {
    ssids?: string[];
    enabled?: boolean;
    check_every?: number;
    exit_misses?: number;
  }): Promise<boolean> {
    setBusy(true);
    try {
      const next = await apiFetch<HomeModeData>("/api/homemode", { method: "PUT", body });
      await mutate(next, { revalidate: false });
      return true;
    } catch (e) {
      flash(e instanceof ApiError ? e.message : t("homemode.saveFailed", "Couldn’t save — try again"), true);
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function addSsid(value: string) {
    const v = value.trim();
    if (!v) return;
    if (ssids.some((s) => s.toLowerCase() === v.toLowerCase())) {
      flash(t("homemode.alreadyInList", "“{{ssid}}” is already in the list", { ssid: v }), true);
      return;
    }
    setNewSsid("");
    if (await persist({ ssids: [...ssids, v] })) flash(t("homemode.added", "Added {{ssid}}", { ssid: v }));
  }

  async function removeSsid(ssid: string) {
    const prev = ssids;
    if (await persist({ ssids: ssids.filter((s) => s !== ssid) })) {
      flash(t("homemode.removed", "Removed {{ssid}}", { ssid: ssid }), false, async () => {
        if (await persist({ ssids: prev })) flash(t("homemode.restored", "Restored {{ssid}}", { ssid: ssid }));
      });
    }
  }

  async function toggleEnabled(next: boolean) {
    if (await persist({ enabled: next })) {
      flash(
        next
          ? t("homemode.enabledMsg", "Home Mode on — Wi-Fi switches off while you’re home, and back on when you leave")
          : t("homemode.pausedMsg", "Home Mode paused — Wi-Fi stays on")
      );
    }
  }

  async function saveConfig() {
    if (!cfg) return;
    const every = Math.min(60, Math.max(1, parseInt(cfg.every, 10) || 0));
    const misses = Math.min(30, Math.max(1, parseInt(cfg.misses, 10) || 0));
    setCfg({ every: String(every), misses: String(misses) });
    if (await persist({ check_every: every, exit_misses: misses })) {
      flash(t("homemode.savedConfig", "Saved — rechecks every {{every}} min, Wi-Fi returns after {{misses}} miss(es)", { every, misses }));
    }
  }

  async function scan() {
    setScanning(true);
    setScanNote(null);
    try {
      const res = await apiFetch<{ networks: ScanNet[]; note?: string }>("/api/homemode/scan");
      setScanned(res.networks ?? []);
      if (res.note) setScanNote(res.note);
    } catch (e) {
      flash(e instanceof ApiError ? e.message : t("homemode.scanFailed", "Scan failed"), true);
    } finally {
      setScanning(false);
    }
  }

  // Nearby networks not already in the watch-list.
  const pickable = (scanned ?? []).filter(
    (n) => !ssids.some((s) => s.toLowerCase() === n.ssid.toLowerCase())
  );

  // Live status, shown beside the toggle in the header (no separate card).
  const statusEl = data ? (
    !data.enabled ? (
      <Status tone="neutral">{t("homemode.statusPaused", "Paused")}</Status>
    ) : data.wifi_off ? (
      <Status tone="warning">
        <span className="inline-flex items-center gap-1.5">
          <WifiOff size={13} /> {t("homemode.statusWifiOff", "Wi-Fi off — home nearby")}
        </span>
      </Status>
    ) : (
      <Status tone="success">
        <span className="inline-flex items-center gap-1.5">
          <Wifi size={13} /> {t("homemode.statusWifiOn", "Wi-Fi on")}
        </span>
      </Status>
    )
  ) : null;

  return (
    <>
      <PageHeader
        title={t("homemode.title", "Home Mode")}
        description={t("homemode.desc", "When a known home network is nearby, the U60 turns its own Wi-Fi off so your devices fall back to your home router. Wi-Fi comes back when you leave.")}
        actions={
          data ? (
            <div className="flex items-center gap-4">
              {statusEl}
              <Toggle
                checked={data.enabled}
                onChange={toggleEnabled}
                label={data.enabled ? t("homemode.on", "On") : t("homemode.off", "Off")}
              />
            </div>
          ) : undefined
        }
      />

      {error && <ErrorBanner message={error.message} />}

      {msg && (
        <div
          role={msg.err ? "alert" : "status"}
          aria-live={msg.err ? "assertive" : "polite"}
          className={`mb-4 flex items-center gap-3 rounded-md border px-3 py-2 text-sm ${
            msg.err
              ? "border-error/40 bg-error/10 text-error"
              : "border-success/40 bg-success/10 text-success"
          }`}
        >
          <span className="flex-1">{msg.text}</span>
          {msg.undo && (
            <button
              onClick={() => {
                msg.undo?.();
                setMsg(null);
              }}
              className="shrink-0 font-medium text-accent underline underline-offset-2 hover:text-accent-hover"
            >
              {t("homemode.undo", "Undo")}
            </button>
          )}
        </div>
      )}

      <div className="grid gap-4">
        {/* Watch-list */}
        <SectionCard
          title={t("homemode.homeSsidsTitle", "Home SSIDs")}
          description={t("homemode.homeSsidsDesc", "If any of these networks is seen nearby, the U60 turns its Wi-Fi off.")}
        >
          {data?.using_default && (
            <p className="mb-3 text-xs text-text-dim">
              {t("homemode.usingDefaults", "Using built-in defaults. Adding or removing an SSID below saves your own list.")}
            </p>
          )}

          <div className="mb-3 flex gap-2">
            <Input
              value={newSsid}
              onChange={(e) => setNewSsid(e.target.value)}
              placeholder={t("homemode.addSsidPlaceholder", "Add an SSID (exact name)")}
              aria-label={t("homemode.addSsidAria", "Add a home SSID")}
              onKeyDown={(e) => e.key === "Enter" && addSsid(newSsid)}
            />
            <Button onClick={() => addSsid(newSsid)} loading={busy} disabled={!newSsid.trim()}>
              <Plus size={14} /> {t("homemode.add", "Add")}
            </Button>
          </div>

          {ssids.length === 0 ? (
            <p className="text-sm text-text-dim">{t("homemode.noSsids", "No SSIDs configured — home mode will never trigger.")}</p>
          ) : (
            <div className="space-y-1">
              {ssids.map((s) => (
                <div
                  key={s}
                  className="flex items-center gap-3 border-b border-border/50 py-2 last:border-0"
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-sm">{s}</span>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => removeSsid(s)}
                    disabled={busy}
                    aria-label={t("homemode.removeAria", "Remove {{ssid}}", { ssid: s })}
                    className="shrink-0 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
                  >
                    <Trash2 size={13} className="text-error" />
                  </Button>
                </div>
              ))}
            </div>
          )}
        </SectionCard>

        {/* Scan to pick */}
        <SectionCard
          title={t("homemode.nearbyTitle", "Nearby Networks")}
          description={t("homemode.nearbyDesc", "Scan to add a network you can see right now.")}
          actions={
            <Button size="sm" variant="outline" onClick={scan} disabled={scanning}>
              <RefreshCw size={13} className={scanning ? "animate-spin" : ""} />
              {scanning ? (data?.wifi_off ? t("homemode.wakingWifi", "Waking Wi-Fi…") : t("homemode.scanning", "Scanning…")) : t("homemode.scan", "Scan")}
            </Button>
          }
        >
          {scanning && data?.wifi_off && (
            <p className="mb-2 text-xs text-text-dim">
              {t("homemode.wakingRadio", "Waking the 2.4 GHz radio to scan — this takes a few seconds while Wi-Fi is off.")}
            </p>
          )}
          {scanNote && <p className="mb-2 text-xs text-warning">{scanNote}</p>}
          {scanned === null ? (
            <p className="text-sm text-text-dim">{t("homemode.tapScan", "Tap Scan to list nearby Wi-Fi networks.")}</p>
          ) : pickable.length === 0 ? (
            <p className="text-sm text-text-dim">
              {scanned.length === 0 ? t("homemode.noNetworks", "No networks found.") : t("homemode.allInList", "All nearby networks are already in your list.")}
            </p>
          ) : (
            <div className="space-y-1">
              {pickable.map((n) => (
                <div
                  key={n.ssid}
                  className="flex items-center gap-3 border-b border-border/50 py-2 last:border-0"
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-sm">{n.ssid}</span>
                  <span className="shrink-0 text-xs text-text-dim">{n.signal} dBm</span>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => addSsid(n.ssid)}
                    disabled={busy}
                    aria-label={t("homemode.addAria", "Add {{ssid}}", { ssid: n.ssid })}
                    className="shrink-0 [@media(pointer:coarse)]:h-11 [@media(pointer:coarse)]:w-11"
                  >
                    <Plus size={13} />
                  </Button>
                </div>
              ))}
            </div>
          )}
        </SectionCard>

        {/* Detection settings */}
        <SectionCard
          title={t("homemode.detectionTitle", "Detection settings")}
          description={t("homemode.detectionDesc", "How often the device rechecks whether you’re still home, and how long to wait before turning Wi-Fi back on after you leave.")}
        >
          {cfg && (
            <div className="space-y-4">
              <div className="grid gap-4 sm:grid-cols-2">
                <label className="flex flex-col gap-1.5">
                  <span className="text-sm text-text">{t("homemode.recheckInterval", "Recheck interval")}</span>
                  <span className="flex items-center gap-2">
                    <Input
                      type="number"
                      min={1}
                      max={60}
                      value={cfg.every}
                      onChange={(e) => setCfg({ ...cfg, every: e.target.value })}
                      className="w-24"
                    />
                    <span className="text-sm text-text-dim">{t("homemode.minutes", "minutes")}</span>
                  </span>
                </label>
                <label className="flex flex-col gap-1.5">
                  <span className="text-sm text-text">{t("homemode.missesLabel", "Misses before Wi-Fi returns")}</span>
                  <span className="flex items-center gap-2">
                    <Input
                      type="number"
                      min={1}
                      max={30}
                      value={cfg.misses}
                      onChange={(e) => setCfg({ ...cfg, misses: e.target.value })}
                      className="w-24"
                    />
                    <span className="text-sm text-text-dim">{t("homemode.rechecks", "rechecks")}</span>
                  </span>
                </label>
              </div>
              <p className="text-xs text-text-dim">
                {t("homemode.returnLead", "After you leave, Wi-Fi comes back in about")}{" "}
                <span className="font-medium text-text">
                  {t("homemode.minValue", "{{n}} min", { n: (parseInt(cfg.every, 10) || 0) * (parseInt(cfg.misses, 10) || 0) })}
                </span>{" "}
                {t("homemode.returnDetail", "({{misses}} missed recheck(s) × {{every}} min). Shorter intervals react faster but wake the 2.4 GHz radio more often.", { misses: cfg.misses, every: cfg.every })}
              </p>
              <Button onClick={saveConfig} loading={busy}>
                {t("homemode.save", "Save")}
              </Button>
            </div>
          )}
        </SectionCard>

        {/* Switch events — the actual Wi-Fi on/off transitions */}
        <SectionCard
          title={t("homemode.switchEventsTitle", "Switch events")}
          description={t("homemode.switchEventsDesc", "When Wi-Fi was actually turned off / back on — newest first.")}
          actions={
            <Button size="sm" variant="outline" onClick={() => mutateLog()}>
              <RefreshCw size={13} /> {t("homemode.refresh", "Refresh")}
            </Button>
          }
        >
          {!logData?.events ? (
            <p className="text-sm text-text-dim">{t("homemode.noSwitchEvents", "No switch events yet.")}</p>
          ) : (
            <pre className="max-h-64 overflow-auto rounded-md border border-border bg-bg-elevated p-3 font-mono text-xs leading-relaxed text-text">
              {logData.events.split("\n").filter(Boolean).reverse().join("\n")}
            </pre>
          )}
        </SectionCard>

        {/* Scan activity — the frequent periodic rechecks (kept separate) */}
        <SectionCard
          title={t("homemode.scanActivityTitle", "Scan activity")}
          description={t("homemode.scanActivityDesc", "Periodic ~2-minute rechecks while at home — high volume, kept separate so it doesn’t bury the switch events.")}
        >
          {!logData?.scans ? (
            <p className="text-sm text-text-dim">{t("homemode.noScanActivity", "No scan activity yet.")}</p>
          ) : (
            <pre className="max-h-48 overflow-auto rounded-md border border-border bg-bg-elevated p-3 font-mono text-xs leading-relaxed text-text-dim">
              {logData.scans.split("\n").filter(Boolean).reverse().join("\n")}
            </pre>
          )}
        </SectionCard>
      </div>
    </>
  );
}
