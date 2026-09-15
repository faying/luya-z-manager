"use client";

import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button, Input, Toggle } from "@/components/admin/Button";
import { Help } from "@/components/admin/Help";
import { useSWRConfig } from "swr";
import { Eye, EyeOff, Wifi, AlertTriangle } from "lucide-react";

type WiFiStatus = Record<string, string>;

const ENCRYPTIONS = [
  { value: "psk2+ccmp", label: "WPA2" },
  { value: "psk3+ccmp", label: "WPA3" },
  { value: "psk2+psk3+ccmp", label: "WPA2/WPA3 Mixed" },
  { value: "none", label: "Open (no password)" },
];

const CHANNELS_2G = ["auto", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13"];
const CHANNELS_5G = ["auto", "36", "40", "44", "48", "52", "56", "60", "64", "100", "104", "108", "112", "116", "132", "136", "140", "149", "153", "157", "161", "165"];
const BANDWIDTHS = ["auto", "HT20", "HT40", "VHT80", "VHT160"];
const TXPOWERS = [
  { value: "25", label: "Low" },
  { value: "50", label: "Medium" },
  { value: "100", label: "High" },
];

// WiFi regulatory regions. The self-managed phy reads this country code; the
// firmware regdb governs allowed channels & power per region. Curated subset of
// the regions the firmware supports (CN/US are the common ones for this device).
const REGIONS = [
  { value: "CN", label: "中国 (CN)" },
  { value: "US", label: "United States (US)" },
  { value: "HK", label: "Hong Kong (HK)" },
  { value: "TW", label: "Taiwan (TW)" },
  { value: "JP", label: "Japan (JP)" },
  { value: "KR", label: "Korea (KR)" },
  { value: "SG", label: "Singapore (SG)" },
  { value: "AU", label: "Australia (AU)" },
  { value: "GB", label: "United Kingdom (GB)" },
  { value: "DE", label: "Germany (DE)" },
  { value: "CA", label: "Canada (CA)" },
];

function FieldRow({ label, help, children }: { label: string; help?: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5 border-b border-border/60 py-3 last:border-0 sm:flex-row sm:items-center sm:justify-between">
      <span className="flex items-center text-sm text-text-dim">
        {label}
        {help && <Help text={help} />}
      </span>
      <div className="sm:w-52">{children}</div>
    </div>
  );
}

function SelectField({ value, onChange, options }: { value: string; onChange: (v: string) => void; options: { value: string; label: string }[] }) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className="h-9 w-full rounded-lg border border-border bg-bg-card px-3 text-sm text-text outline-none transition focus:border-accent focus:ring-2 focus:ring-accent/20"
    >
      {options.map((o) => (
        <option key={o.value} value={o.value}>{o.label}</option>
      ))}
    </select>
  );
}

export default function WiFiPage() {
  const { t } = useTranslation();
  const { data, error: fetchError, mutate } = useApi<WiFiStatus>("/api/wifi/status");
  const { mutate: globalMutate } = useSWRConfig();

  const autoLabel = t("wifi.auto", "Auto");
  const encOptions = ENCRYPTIONS.map((o) =>
    o.value === "none" ? { value: o.value, label: t("wifi.encOpen", o.label) } : o
  );
  const txOptions = [
    { value: "25", label: t("wifi.txLow", "Low") },
    { value: "50", label: t("wifi.txMedium", "Medium") },
    { value: "100", label: t("wifi.txHigh", "High") },
  ];
  const chOpts = (list: string[]) => list.map((c) => ({ value: c, label: c === "auto" ? autoLabel : c }));
  const bwOpts = BANDWIDTHS.map((b) => ({ value: b, label: b === "auto" ? autoLabel : b }));

  const [ssid2g, setSsid2g] = useState("");
  const [ssid5g, setSsid5g] = useState("");
  const [key2g, setKey2g] = useState("");
  const [key5g, setKey5g] = useState("");
  const [enc2g, setEnc2g] = useState("psk2+ccmp");
  const [enc5g, setEnc5g] = useState("psk2+ccmp");
  const [ch2g, setCh2g] = useState("auto");
  const [ch5g, setCh5g] = useState("auto");
  const [bw2g, setBw2g] = useState("auto");
  const [bw5g, setBw5g] = useState("auto");
  const [tx2g, setTx2g] = useState("100");
  const [tx5g, setTx5g] = useState("100");
  const [hidden2g, setHidden2g] = useState(false);
  const [hidden5g, setHidden5g] = useState(false);
  const [region, setRegion] = useState("CN");
  const [wifiOn, setWifiOn] = useState(true);
  const [radio2Disabled, setRadio2Disabled] = useState(false);
  const [radio5Disabled, setRadio5Disabled] = useState(false);
  const [wifi6, setWifi6] = useState(false);
  const [show2g, setShow2g] = useState(false);
  const [show5g, setShow5g] = useState(false);
  const [saving, setSaving] = useState(false);
  const [msg, setMsg] = useState<{ text: string; err: boolean } | null>(null);
  const [showWarn, setShowWarn] = useState(false);

  useEffect(() => {
    if (!data) return;
    setSsid2g(data.ssid_2g ?? data.ssid ?? "");
    setSsid5g(data.ssid_5g ?? data.ssid ?? "");
    setKey2g(data.key_2g ?? data.password ?? data.key ?? "");
    setKey5g(data.key_5g ?? data.password ?? data.key ?? "");
    setEnc2g(data.encryption_2g ?? data.encryption ?? "psk2+ccmp");
    setEnc5g(data.encryption_5g ?? data.encryption ?? "psk2+ccmp");
    setCh2g(data.channel_2g ?? data.channel ?? "auto");
    setCh5g(data.channel_5g ?? data.channel ?? "auto");
    setBw2g(data.htmode_2g ?? data.bandwidth ?? "auto");
    setBw5g(data.htmode_5g ?? data.bandwidth ?? "auto");
    setTx2g(data.txpower_2g ?? data.tx_power ?? "100");
    setTx5g(data.txpower_5g ?? data.tx_power ?? "100");
    setHidden2g(data.hidden_2g === "1");
    setHidden5g(data.hidden_5g === "1");
    setWifiOn(data.wifi_onoff !== "0");
    setRadio2Disabled(data.radio2_disabled === "1");
    setRadio5Disabled(data.radio5_disabled === "1");
    setWifi6(data.wifi6_switch === "1");
    setRegion((data.country_code || "CN").toUpperCase());
  }, [data]);

  // US (FCC) disallows 2.4 GHz ch 12/13; hide them and snap a fixed pick to Auto.
  const channels2g = region === "US" ? CHANNELS_2G.filter((c) => c !== "12" && c !== "13") : CHANNELS_2G;
  const regionOpts = REGIONS.some((r) => r.value === region)
    ? REGIONS
    : [{ value: region, label: region }, ...REGIONS];

  function handleRegionChange(next: string) {
    setRegion(next);
    if (next === "US" && (ch2g === "12" || ch2g === "13")) setCh2g("auto");
  }

  async function handleApply() {
    setSaving(true);
    setMsg(null);
    try {
      await apiFetch("/api/wifi/settings", {
        method: "PUT",
        body: {
          ssid_2g: ssid2g, ssid_5g: ssid5g,
          key_2g: key2g, key_5g: key5g,
          channel_2g: ch2g, channel_5g: ch5g,
          txpower_2g: tx2g, txpower_5g: tx5g,
          encryption_2g: enc2g, encryption_5g: enc5g,
          hidden_2g: hidden2g ? "1" : "0",
          hidden_5g: hidden5g ? "1" : "0",
          wifi_onoff: wifiOn ? "1" : "0",
          radio2_disabled: radio2Disabled ? "1" : "0",
          radio5_disabled: radio5Disabled ? "1" : "0",
          wifi6_switch: wifi6 ? "1" : "0",
          htmode_2g: bw2g,
          htmode_5g: bw5g,
          country: region,
        },
      });
      setMsg({ text: t("wifi.applied", "WiFi settings applied — WiFi will restart briefly."), err: false });
      await mutate();
      globalMutate("/api/wifi/status");
    } catch (e) {
      setMsg({ text: e instanceof ApiError ? e.message : t("wifi.applyFailed", "Failed to apply"), err: true });
    } finally {
      setSaving(false);
      setShowWarn(false);
    }
  }

  return (
    <>
      <PageHeader
        title={t("wifi.title", "WiFi Settings")}
        description={t("wifi.desc", "Configure 2.4 GHz and 5 GHz radios.")}
        actions={
          <Button onClick={() => setShowWarn(true)} loading={saving}>
            <Wifi size={14} /> {t("common.apply", "Apply")}
          </Button>
        }
      />

      {fetchError && <ErrorBanner message={fetchError.message} onRetry={() => mutate()} />}
      {msg && (
        <div className={`mb-4 rounded-md border px-3 py-2 text-sm ${msg.err ? "border-error/40 bg-error/10 text-error" : "border-success/40 bg-success/10 text-success"}`}>
          {msg.text}
        </div>
      )}

      {showWarn && (
        <div className="mb-4 flex items-start gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <AlertTriangle size={16} className="mt-0.5 shrink-0" />
          <div className="flex-1">
            {t("wifi.warnRestart", "WiFi will briefly restart. Confirm?")}
            <div className="mt-2 flex gap-2">
              <Button size="sm" onClick={handleApply} loading={saving}>{t("common.confirm", "Confirm")}</Button>
              <Button size="sm" variant="ghost" onClick={() => setShowWarn(false)}>{t("common.cancel", "Cancel")}</Button>
            </div>
          </div>
        </div>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        {/* Global */}
        <SectionCard title={t("wifi.globalTitle", "Global")} description={t("wifi.globalDesc", "Master switches for both radios.")} className="md:col-span-2">
          <FieldRow label={t("wifi.onoff", "WiFi On/Off")}>
            <Toggle checked={wifiOn} onChange={setWifiOn} label={wifiOn ? t("common.on", "On") : t("common.off", "Off")} />
          </FieldRow>
          <FieldRow
            label={t("wifi.wifi6", "WiFi 6 (802.11ax)")}
            help={t("wifi.helpWifi6", "Faster speeds and better performance when many devices are connected. Turn off only if old gear refuses to connect.")}
          >
            <Toggle checked={wifi6} onChange={setWifi6} label={wifi6 ? t("common.enabled", "Enabled") : t("common.disabled", "Disabled")} />
          </FieldRow>
          <FieldRow
            label={t("wifi.region", "Region")}
            help={t("wifi.helpRegion", "Regulatory domain — sets which channels and TX power are allowed. Match your country. US enables more 5 GHz power and upper channels but drops 2.4 GHz ch 12–13. Changing it restarts WiFi.")}
          >
            <SelectField value={region} onChange={handleRegionChange} options={regionOpts} />
          </FieldRow>
        </SectionCard>

        {/* 2.4 GHz */}
        <SectionCard title={t("wifi.radio2Title", "2.4 GHz Radio")} description={t("wifi.radio2Desc", "Longer range and better through walls — slower speeds.")}>
          <FieldRow label={t("wifi.radioEnabled", "Radio Enabled")}>
            <Toggle checked={!radio2Disabled} onChange={(v) => setRadio2Disabled(!v)} label={!radio2Disabled ? t("common.on", "On") : t("common.off", "Off")} />
          </FieldRow>
          <FieldRow label={t("wifi.ssid", "SSID")}>
            <Input value={ssid2g} onChange={(e) => setSsid2g(e.target.value)} />
          </FieldRow>
          <FieldRow label={t("wifi.password", "Password")}>
            <div className="relative">
              <Input
                type={show2g ? "text" : "password"}
                value={key2g}
                onChange={(e) => setKey2g(e.target.value)}
                className="pr-9"
                disabled={enc2g === "none"}
              />
              <button
                type="button"
                onClick={() => setShow2g(!show2g)}
                aria-label={show2g ? t("wifi.hidePassword", "Hide password") : t("wifi.showPassword", "Show password")}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-text-dim hover:text-text"
              >
                {show2g ? <EyeOff size={15} /> : <Eye size={15} />}
              </button>
            </div>
          </FieldRow>
          <FieldRow label={t("wifi.encryption", "Encryption")} help={t("wifi.helpEncryption", "WPA3 is most secure; WPA2/WPA3 Mixed keeps older devices working. Avoid Open.")}>
            <SelectField value={enc2g} onChange={setEnc2g} options={encOptions} />
          </FieldRow>
          <FieldRow label={t("wifi.channel", "Channel")} help={t("wifi.helpChannel2", "Auto picks the clearest channel. Set a fixed one only to avoid a known busy channel.")}>
            <SelectField value={ch2g} onChange={setCh2g} options={chOpts(channels2g)} />
          </FieldRow>
          <FieldRow label={t("wifi.bandwidth", "Bandwidth")} help={t("wifi.helpBandwidth2", "Wider channels are faster but more prone to interference and shorter range.")}>
            <SelectField value={bw2g} onChange={setBw2g} options={bwOpts} />
          </FieldRow>
          <FieldRow label={t("wifi.txpower", "TX Power")} help={t("wifi.helpTxPower", "Transmit strength. Lower it to reduce range and interference; High for maximum coverage.")}>
            <SelectField value={tx2g} onChange={setTx2g} options={txOptions} />
          </FieldRow>
          <FieldRow label={t("wifi.hideSsid", "Hide SSID")} help={t("wifi.helpHideSsid", "Stops broadcasting the network name. Devices must add it manually — modest privacy, not real security.")}>
            <Toggle checked={hidden2g} onChange={setHidden2g} />
          </FieldRow>
        </SectionCard>

        {/* 5 GHz */}
        <SectionCard title={t("wifi.radio5Title", "5 GHz Radio")} description={t("wifi.radio5Desc", "Faster speeds and less congestion — shorter range.")}>
          <FieldRow label={t("wifi.radioEnabled", "Radio Enabled")}>
            <Toggle checked={!radio5Disabled} onChange={(v) => setRadio5Disabled(!v)} label={!radio5Disabled ? t("common.on", "On") : t("common.off", "Off")} />
          </FieldRow>
          <FieldRow label={t("wifi.ssid", "SSID")}>
            <Input value={ssid5g} onChange={(e) => setSsid5g(e.target.value)} />
          </FieldRow>
          <FieldRow label={t("wifi.password", "Password")}>
            <div className="relative">
              <Input
                type={show5g ? "text" : "password"}
                value={key5g}
                onChange={(e) => setKey5g(e.target.value)}
                className="pr-9"
                disabled={enc5g === "none"}
              />
              <button
                type="button"
                onClick={() => setShow5g(!show5g)}
                aria-label={show5g ? t("wifi.hidePassword", "Hide password") : t("wifi.showPassword", "Show password")}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-text-dim hover:text-text"
              >
                {show5g ? <EyeOff size={15} /> : <Eye size={15} />}
              </button>
            </div>
          </FieldRow>
          <FieldRow label={t("wifi.encryption", "Encryption")} help={t("wifi.helpEncryption", "WPA3 is most secure; WPA2/WPA3 Mixed keeps older devices working. Avoid Open.")}>
            <SelectField value={enc5g} onChange={setEnc5g} options={encOptions} />
          </FieldRow>
          <FieldRow label={t("wifi.channel", "Channel")} help={t("wifi.helpChannel5", "Auto picks the clearest channel. On 5 GHz, higher channels (DFS) may pause briefly for radar checks.")}>
            <SelectField value={ch5g} onChange={setCh5g} options={chOpts(CHANNELS_5G)} />
          </FieldRow>
          <FieldRow label={t("wifi.bandwidth", "Bandwidth")} help={t("wifi.helpBandwidth5", "Wider channels (VHT80/160) are much faster but shorter range and more interference-prone.")}>
            <SelectField value={bw5g} onChange={setBw5g} options={bwOpts} />
          </FieldRow>
          <FieldRow label={t("wifi.txpower", "TX Power")} help={t("wifi.helpTxPower", "Transmit strength. Lower it to reduce range and interference; High for maximum coverage.")}>
            <SelectField value={tx5g} onChange={setTx5g} options={txOptions} />
          </FieldRow>
          <FieldRow label={t("wifi.hideSsid", "Hide SSID")} help={t("wifi.helpHideSsid", "Stops broadcasting the network name. Devices must add it manually — modest privacy, not real security.")}>
            <Toggle checked={hidden5g} onChange={setHidden5g} />
          </FieldRow>
        </SectionCard>
      </div>
    </>
  );
}
