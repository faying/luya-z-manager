"use client";
// Wi-Fi settings (new design). Status first (is Wi-Fi on and broadcasting,
// per band: channel, width, devices), then the three groups of settings,
// then one Apply for the whole form — the page has always been one form
// that sends every field in a single PUT /api/wifi/settings.
//
// Writes (controls-inventory §/router/wifi, design §3.1): tier 2 locally,
// tier 3 over Tailscale (isRemoteAccess, checked at the press). The agent
// commits uci, answers, then reloads Wi-Fi in the background
// (wifi.rs:294-298), so the result is read back from GET /api/wifi/status
// — configuration only; whether the radios are really up shows in the
// status block once the reload is done. Changes that reload Wi-Fi wait for
// the device (30 s) and name the network to reconnect to.
//
// Wi-Fi master switch: sent exactly as before — `wifi_onoff` inside the
// same full-body PUT /api/wifi/settings, with the same 19 keys and the same
// seeding. Whether writing zte_mbb.wifi.wifi_onoff really switches Wi-Fi on
// this firmware is unconfirmed (wifi.rs:264-300: when only zte_mbb keys
// change the agent doesn't reload; with the full body an ""→"0" write to a
// wireless key can trigger a reload as a side effect). It will be checked
// on the device; until then do not switch it to /api/wifi/radio and do not
// trim the body to changed keys — both would change what gets tested.
import { useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { isRemoteAccess } from "@/lib/api/remote";
import { useWriteOp } from "@/lib/api/writeOp";
import type { WifiSettingsBody, WifiStatus } from "@/lib/api/schemas/wifi";
import {
  Button,
  ConfirmDialog,
  ConfirmInline,
  Freshness,
  Group,
  GroupTitle,
  Help,
  OpResult,
  Row,
  StatusBlock,
  StatusMark,
  Switch,
  useConfirmInline,
  type Tone,
} from "@/components/nd";
import { FieldRow, PasswordField, SelectField, TextField, flag, hasStripped, keyProblem, ssidProblem, type Opt } from "./fields";

type WifiForm = Required<Pick<WifiSettingsBody, keyof WifiSettingsBody>>;
type Band = "2g" | "5g";
const BAND_NAME: Record<Band, string> = { "2g": "2.4 GHz", "5g": "5 GHz" };

const CHANNELS_2G = ["auto", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13"];
const CHANNELS_5G = ["auto", "36", "40", "44", "48", "52", "56", "60", "64", "100", "104", "108", "112", "116", "132", "136", "140", "149", "153", "157", "161", "165"];
const BANDWIDTHS = ["auto", "HT20", "HT40", "VHT80", "VHT160"];

// Wi-Fi regulatory regions. The self-managed phy reads this country code; the
// firmware regdb governs allowed channels & power per region. Curated subset of
// the regions the firmware supports (CN/US are the common ones for this device).
const REGIONS: Opt[] = [
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

/** Same seeding as the old page (minus fallbacks to key names the agent
 *  never sends: ssid, password, key, encryption, channel, bandwidth,
 *  tx_power). The agent sends every key, "" when unset. */
function seed(d: WifiStatus): WifiForm {
  return {
    ssid_2g: d.ssid_2g ?? "",
    ssid_5g: d.ssid_5g ?? "",
    key_2g: d.key_2g ?? "",
    key_5g: d.key_5g ?? "",
    channel_2g: d.channel_2g ?? "auto",
    channel_5g: d.channel_5g ?? "auto",
    txpower_2g: d.txpower_2g ?? "100",
    txpower_5g: d.txpower_5g ?? "100",
    encryption_2g: d.encryption_2g ?? "psk2+ccmp",
    encryption_5g: d.encryption_5g ?? "psk2+ccmp",
    hidden_2g: flag(d.hidden_2g),
    hidden_5g: flag(d.hidden_5g),
    wifi_onoff: d.wifi_onoff !== "0" ? "1" : "0",
    radio2_disabled: flag(d.radio2_disabled),
    radio5_disabled: flag(d.radio5_disabled),
    wifi6_switch: d.wifi6_switch === "1" ? "1" : "0",
    htmode_2g: d.htmode_2g ?? "auto",
    htmode_5g: d.htmode_5g ?? "auto",
    country: (d.country_code || "CN").toUpperCase(),
  };
}

const FORM_KEYS = Object.keys(seed({} as WifiStatus)) as (keyof WifiForm)[];
const FLAG_KEYS = new Set<keyof WifiForm>(["hidden_2g", "hidden_5g", "radio2_disabled", "radio5_disabled", "wifi_onoff", "wifi6_switch"]);

/** Does the readback show every value that was sent? */
function readbackMatches(sent: WifiForm, d: WifiStatus): boolean {
  return FORM_KEYS.every((k) => {
    if (k === "country") return (d.country_code ?? "").toUpperCase() === sent.country.toUpperCase();
    const got = (d as unknown as Record<string, string | undefined>)[k] ?? "";
    if (k === "wifi_onoff") return (got !== "0" ? "1" : "0") === sent[k];
    if (FLAG_KEYS.has(k)) return flag(got) === sent[k];
    return got === sent[k];
  });
}

interface Pending {
  tier: 2 | 3;
  body: WifiForm;
  consequence: ReactNode;
  downtime: string;
  recovery: string;
  reloads: boolean;
  reconnect: string | null;
}

function encOptions(t: TFunction): Opt[] {
  return [
    { value: "psk2+ccmp", label: "WPA2" },
    { value: "psk3+ccmp", label: "WPA3" },
    { value: "psk2+psk3+ccmp", label: t("wifi.encMixed", "WPA2/WPA3 Mixed") },
    { value: "none", label: t("wifi.encOpen", "Open (no password)") },
  ];
}

/** 「X」 (2.4 GHz) and 「Y」 (5 GHz), or just 「X」 when both bands match. */
function networkNames(t: TFunction, f: WifiForm): string {
  const q = (s: string) => t("wifi.quoted", "“{{s}}”", { s });
  const on2 = f.radio2_disabled !== "1";
  const on5 = f.radio5_disabled !== "1";
  if (on2 && on5 && f.ssid_2g === f.ssid_5g) return q(f.ssid_2g);
  const parts: string[] = [];
  if (on2) parts.push(t("wifi.nameBand", "{{name}} ({{band}})", { name: q(f.ssid_2g), band: BAND_NAME["2g"] }));
  if (on5) parts.push(t("wifi.nameBand", "{{name}} ({{band}})", { name: q(f.ssid_5g), band: BAND_NAME["5g"] }));
  return parts.join(t("wifi.and", " and "));
}

export default function WifiPage() {
  const { t } = useTranslation();
  const st = useApi<WifiStatus>("/api/wifi/status", { refreshInterval: 30000 });
  const data = st.data;

  const [draft, setDraft] = useState<WifiForm | null>(null);
  const base = data ? seed(data) : null;
  const form = draft ?? base;
  const changed = form && base ? FORM_KEYS.filter((k) => form[k] !== base[k]) : [];

  const [pending, setPending] = useState<Pending | null>(null);
  const [reconnectNote, setReconnectNote] = useState<string | null>(null);
  const [switchedMaster, setSwitchedMaster] = useState(false);
  const sentRef = useRef<WifiForm | null>(null);
  const inline = useConfirmInline(pending !== null && pending.tier === 2);

  const op = useWriteOp({
    tier: pending?.tier ?? 2,
    steps: [
      {
        label: t("wifi.stepSettings", "Wi-Fi settings"),
        run: () => apiFetch("/api/wifi/settings", { method: "PUT", body: sentRef.current }),
      },
    ],
    verify: async () => {
      const d = await apiFetch<WifiStatus>("/api/wifi/status");
      await st.mutate(d, { revalidate: false });
      const ok = sentRef.current !== null && readbackMatches(sentRef.current, d);
      // Once the readback shows the change, the form re-seeds from it.
      if (ok) setDraft(null);
      return ok;
    },
    waitDevice: pending?.reloads ? { expectedSec: 30, recovery: pending.recovery } : undefined,
  });
  const busy = op.busy;
  const locked = !data || st.stale || busy;

  const cancelPending = () => setPending(null);

  function set<K extends keyof WifiForm>(k: K, v: string) {
    if (!form) return;
    setDraft({ ...form, [k]: v });
  }

  // US (FCC) disallows 2.4 GHz ch 12/13; hide them and snap a fixed pick to Auto.
  function setRegion(next: string) {
    if (!form) return;
    const f = { ...form, country: next };
    if (next === "US" && (f.channel_2g === "12" || f.channel_2g === "13")) f.channel_2g = "auto";
    setDraft(f);
  }

  // ── validation (only fields the user changed, so an odd stored value
  //    never blocks an unrelated change) ──
  function problems(f: WifiForm): string[] {
    const out: string[] = [];
    const charsErr = (band: string, what: string, isChanged: boolean) =>
      isChanged
        ? t("wifi.errChars", "{{band}}: {{what}} can't contain ' \" ; $ ` \\ | < > & — the device would silently drop them", { band, what })
        : t("wifi.errCharsCurrent", "{{band}}: the current {{what}} contains one of ' \" ; $ ` \\ | < > &, which the device would silently drop on any save. Change it here first.", { band, what });
    for (const b of ["2g", "5g"] as const) {
      const band = BAND_NAME[b];
      const ssidK = `ssid_${b}` as const;
      const keyK = `key_${b}` as const;
      const encK = `encryption_${b}` as const;
      // Every apply sends every field: check the characters the agent
      // strips on all of them, not just the changed ones.
      if (hasStripped(f[ssidK])) out.push(charsErr(band, t("wifi.ssidWord", "the network name"), changed.includes(ssidK)));
      if (f[encK] !== "none" && hasStripped(f[keyK])) out.push(charsErr(band, t("wifi.passwordWord", "the password"), changed.includes(keyK)));
      if (changed.includes(ssidK)) {
        const p = ssidProblem(f[ssidK]);
        if (p === "empty") out.push(t("wifi.errSsidEmpty", "{{band}}: enter a network name", { band }));
        if (p === "long") out.push(t("wifi.errSsidLong", "{{band}}: the network name is longer than 32 bytes", { band }));
      }
      if (changed.includes(keyK) || changed.includes(encK)) {
        const p = keyProblem(f[keyK], f[encK]);
        if (p === "short") out.push(t("wifi.errKeyShort", "{{band}}: the password needs at least 8 characters", { band }));
        if (p === "long") out.push(t("wifi.errKeyLong", "{{band}}: the password can have at most 63 characters", { band }));
        if (p === "ascii") out.push(t("wifi.errKeyAscii", "{{band}}: the password can only use letters, digits and ASCII symbols", { band }));
      }
    }
    return out;
  }
  const errs = form ? problems(form) : [];

  // ── consequence of the pending change, in plain words ──
  function describe(f: WifiForm, b: WifiForm) {
    const lines: string[] = [];
    const diff = (k: keyof WifiForm) => f[k] !== b[k];
    const onlyTx = FORM_KEYS.every((k) => !diff(k) || k === "txpower_2g" || k === "txpower_5g");
    const wifiOff = f.wifi_onoff === "0";
    if (diff("wifi_onoff")) {
      lines.push(
        wifiOff
          ? t("wifi.cOff", "All Wi-Fi goes off: every device on the U60's Wi-Fi drops. To turn it back on you need a cable, USB, Tailscale or the touchscreen.")
          : t("wifi.cOn", "Wi-Fi comes on."),
      );
    }
    let nameOrKey = false;
    let keyChanged = false;
    for (const bd of ["2g", "5g"] as const) {
      const band = BAND_NAME[bd];
      const radioK = bd === "2g" ? "radio2_disabled" : "radio5_disabled";
      if (diff(radioK)) {
        lines.push(
          f[radioK] === "1"
            ? t("wifi.cRadioOff", "{{band}} goes off; devices that only use it drop.", { band })
            : t("wifi.cRadioOn", "{{band}} comes on.", { band }),
        );
      }
      if (diff(`ssid_${bd}`)) {
        nameOrKey = true;
        lines.push(t("wifi.cSsid", "{{band}} network name becomes {{name}}.", { band, name: t("wifi.quoted", "“{{s}}”", { s: f[`ssid_${bd}`] }) }));
      }
      if (diff(`encryption_${bd}`)) {
        nameOrKey = true;
        lines.push(
          f[`encryption_${bd}`] === "none"
            ? t("wifi.cOpen", "{{band}} becomes an open network: anyone nearby can join without a password.", { band })
            : t("wifi.cEnc", "{{band}} security becomes {{enc}}.", {
                band,
                enc: encOptions(t).find((o) => o.value === f[`encryption_${bd}`])?.label ?? f[`encryption_${bd}`],
              }),
        );
      }
      if (diff(`key_${bd}`) && f[`encryption_${bd}`] !== "none") {
        nameOrKey = true;
        keyChanged = true;
        lines.push(t("wifi.cKey", "{{band}} password changes.", { band }));
      }
      if (diff(`hidden_${bd}`)) {
        lines.push(
          f[`hidden_${bd}`] === "1"
            ? t("wifi.cHidden", "{{band}} stops showing its name; new devices must add it by hand.", { band })
            : t("wifi.cShown", "{{band}} shows its name again.", { band }),
        );
      }
    }
    const others: string[] = [];
    if (diff("channel_2g") || diff("channel_5g")) others.push(t("wifi.channel", "Channel"));
    if (diff("htmode_2g") || diff("htmode_5g")) others.push(t("wifi.bandwidth", "Bandwidth"));
    if (diff("txpower_2g") || diff("txpower_5g")) others.push(t("wifi.txpower", "TX Power"));
    if (diff("country")) others.push(t("wifi.region", "Region"));
    if (diff("wifi6_switch")) others.push("Wi-Fi 6");
    if (others.length) lines.push(t("wifi.cOthers", "Also changes: {{list}}.", { list: others.join(t("wifi.listSep", ", ")) }));

    const names = networkNames(t, f);
    const reconnect =
      !wifiOff && nameOrKey
        ? keyChanged
          ? t("wifi.reconnectWithKey", "Devices on the U60's Wi-Fi must reconnect to {{names}} with the new password.", { names })
          : t("wifi.reconnect", "Devices on the U60's Wi-Fi must reconnect to {{names}}.", { names })
        : null;

    if (onlyTx) lines.push(t("wifi.cHot", "Only the transmit power changes: it applies at once and Wi-Fi does not restart."));
    else if (!wifiOff) lines.push(t("wifi.cReload", "Wi-Fi reloads for about 15–60 seconds; connected devices drop briefly."));
    if (reconnect) lines.push(reconnect);

    const downtime = wifiOff
      ? t("wifi.downOff", "Wi-Fi stays off until it is turned on again.")
      : onlyTx
        ? t("wifi.downNone", "None: Wi-Fi does not restart.")
        : t("wifi.downReload", "About 15–60 seconds while Wi-Fi reloads.");
    const recovery = wifiOff
      ? t("wifi.recoveryOff", "Turn Wi-Fi back on from the touchscreen, or open this page over a cable, USB or Tailscale.")
      : nameOrKey
        ? keyChanged
          ? t("wifi.recoveryReconnectKey", "Reconnect your device to {{names}} using the new password, then open this page again. Over a cable, USB or Tailscale the page stays reachable.", { names })
          : t("wifi.recoveryReconnect", "Reconnect your device to {{names}}, then open this page again. Over a cable, USB or Tailscale the page stays reachable.", { names })
        : t("wifi.recoveryGeneric", "Reconnect to the U60's Wi-Fi ({{names}}) and open this page again, or use a cable, USB or Tailscale.", { names });
    return { consequence: lines.join(" "), downtime, recovery, reloads: !onlyTx, reconnect };
  }

  function askApply() {
    if (!form || !base || busy || changed.length === 0 || errs.length > 0) return;
    const d = describe(form, base);
    setPending({ tier: isRemoteAccess() ? 3 : 2, body: { ...form }, ...d });
  }

  function go() {
    if (!pending) return;
    sentRef.current = pending.body;
    setReconnectNote(pending.reconnect);
    setSwitchedMaster(pending.body.wifi_onoff !== base?.wifi_onoff);
    op.start();
    op.confirm();
    setTimeout(() => setPending(null), 0);
  }

  // ── status ──
  let tone: Tone = "neutral";
  let state: ReactNode = t("wifi.statusLoading", "Reading Wi-Fi…");
  let reason: ReactNode = null;
  let meta: ReactNode = null;
  if (!data && st.error) {
    tone = "bad";
    state = t("wifi.unreadable", "Can't read the Wi-Fi settings");
    reason = st.error.message;
  } else if (data) {
    const bands = (["2g", "5g"] as const).map((b) => {
      const off = (b === "2g" ? data.radio2_disabled : data.radio5_disabled) === "1";
      const ch = b === "2g" ? data.actual_channel_2g : data.actual_channel_5g;
      const bw = b === "2g" ? data.actual_bw_2g : data.actual_bw_5g;
      const n = b === "2g" ? data.clients_2g : data.clients_5g;
      return { b, off, up: !off && !!ch, ch, bw, n };
    });
    if (data.wifi_onoff === "0") {
      state = t("wifi.statusOff", "Wi-Fi off");
      reason = t("wifi.statusOffReason", "The master switch is off. Devices reach the U60 by cable, USB or Tailscale only.");
    } else if (bands.every((x) => x.off)) {
      state = t("wifi.statusBothOff", "Both bands off");
      reason = t("wifi.statusBothOffReason", "The 2.4 GHz and 5 GHz radios are both switched off below.");
    } else if (!bands.some((x) => x.up)) {
      tone = "warn";
      state = t("wifi.statusNotUp", "Wi-Fi not broadcasting");
      reason = t("wifi.statusNotUpReason", "It is switched on, but neither band is up. Right after a change, give it a minute and look again.");
    } else {
      tone = "ok";
      state = t("wifi.statusOn", "Wi-Fi on");
      reason = t("wifi.clientsN", "{{n}} device(s) connected", { n: data.clients_total });
    }
    if (data.wifi_onoff !== "0") {
      meta = (
        <span className="grid">
          {bands.map((x) => (
            <span key={x.b}>
              {x.off
                ? t("wifi.bandOff", "{{band}}: off", { band: BAND_NAME[x.b] })
                : x.up
                  ? t("wifi.bandUp", "{{band}}: channel {{ch}} · {{bw}} · {{n}} device(s)", {
                      band: BAND_NAME[x.b],
                      ch: x.ch,
                      bw: x.bw || "—",
                      n: x.n,
                    })
                  : t("wifi.bandDown", "{{band}}: not broadcasting", { band: BAND_NAME[x.b] })}
            </span>
          ))}
          {st.stale && <Freshness stale lastOkAt={st.lastOkAt} />}
        </span>
      );
    } else if (st.stale) {
      meta = <Freshness stale lastOkAt={st.lastOkAt} />;
    }
    if (st.stale) tone = "stale";
  }

  const autoLabel = t("wifi.auto", "Auto");
  const chOpts = (list: string[]): Opt[] => list.map((c) => ({ value: c, label: c === "auto" ? autoLabel : c }));
  const bwOpts: Opt[] = BANDWIDTHS.map((b) => ({ value: b, label: b === "auto" ? autoLabel : b }));
  const txOpts: Opt[] = [
    { value: "25", label: t("wifi.txLow", "Low") },
    { value: "50", label: t("wifi.txMedium", "Medium") },
    { value: "100", label: t("wifi.txHigh", "High") },
  ];
  const channels2g = form?.country === "US" ? CHANNELS_2G.filter((c) => c !== "12" && c !== "13") : CHANNELS_2G;

  const skeleton = (
    <div className="nd-group">
      {[0, 1, 2].map((i) => (
        <div key={i} className="nd-row">
          <span className="nd-skel" style={{ width: "10ch" }} />
        </div>
      ))}
    </div>
  );

  function bandGroup(b: Band) {
    const band = BAND_NAME[b];
    const radioK = b === "2g" ? "radio2_disabled" : "radio5_disabled";
    return (
      <section>
        <GroupTitle>{b === "2g" ? t("wifi.radio2Title", "2.4 GHz Radio") : t("wifi.radio5Title", "5 GHz Radio")}</GroupTitle>
        <p className="nd-aux -mt-1 mb-3 px-1">
          {b === "2g"
            ? t("wifi.radio2Desc", "Longer range and better through walls — slower speeds.")
            : t("wifi.radio5Desc", "Faster speeds and less congestion — shorter range.")}
        </p>
        {!form ? (
          skeleton
        ) : (
          <div className={`nd-group${st.stale ? " nd-stale" : ""}`}>
            <Row
              label={t("wifi.radioEnabled", "Radio Enabled")}
              sub={form[radioK] === "1" ? t("common.off", "Off") : t("common.on", "On")}
              control={
                <Switch
                  label={t("wifi.radioSwitch", "{{band}} radio", { band })}
                  isSelected={form[radioK] !== "1"}
                  isDisabled={locked}
                  onChange={(on) => set(radioK, on ? "0" : "1")}
                />
              }
            />
            <FieldRow id={`ssid-${b}`} label={t("wifi.ssid", "SSID")} srPrefix={band} stacked>
              <TextField id={`ssid-${b}`} value={form[`ssid_${b}`]} disabled={locked} onChange={(v) => set(`ssid_${b}`, v)} />
            </FieldRow>
            <FieldRow id={`key-${b}`} label={t("wifi.password", "Password")} srPrefix={band} stacked>
              <PasswordField
                id={`key-${b}`}
                whose={band}
                value={form[`key_${b}`]}
                disabled={locked || form[`encryption_${b}`] === "none"}
                placeholder={form[`encryption_${b}`] === "none" ? t("guestwifi.noPasswordOpen", "No password (open)") : undefined}
                onChange={(v) => set(`key_${b}`, v)}
              />
            </FieldRow>
            <FieldRow
              id={`enc-${b}`}
              label={t("wifi.encryption", "Encryption")}
              srPrefix={band}
              help={t("wifi.helpEncryption", "WPA3 is most secure; WPA2/WPA3 Mixed keeps older devices working. Avoid Open.")}
            >
              <SelectField id={`enc-${b}`} value={form[`encryption_${b}`]} options={encOptions(t)} disabled={locked} onChange={(v) => set(`encryption_${b}`, v)} />
            </FieldRow>
            <FieldRow
              id={`ch-${b}`}
              label={t("wifi.channel", "Channel")}
              srPrefix={band}
              help={
                b === "2g"
                  ? t("wifi.helpChannel2", "Auto picks the clearest channel. Set a fixed one only to avoid a known busy channel.")
                  : t("wifi.helpChannel5", "Auto picks the clearest channel. On 5 GHz, higher channels (DFS) may pause briefly for radar checks.")
              }
            >
              <SelectField
                id={`ch-${b}`}
                value={form[`channel_${b}`]}
                options={chOpts(b === "2g" ? channels2g : CHANNELS_5G)}
                disabled={locked}
                onChange={(v) => set(`channel_${b}`, v)}
              />
            </FieldRow>
            <FieldRow
              id={`bw-${b}`}
              label={t("wifi.bandwidth", "Bandwidth")}
              srPrefix={band}
              help={
                b === "2g"
                  ? t("wifi.helpBandwidth2", "Wider channels are faster but more prone to interference and shorter range.")
                  : t("wifi.helpBandwidth5", "Wider channels (VHT80/160) are much faster but shorter range and more interference-prone.")
              }
            >
              <SelectField id={`bw-${b}`} value={form[`htmode_${b}`]} options={bwOpts} disabled={locked} onChange={(v) => set(`htmode_${b}`, v)} />
            </FieldRow>
            <FieldRow
              id={`tx-${b}`}
              label={t("wifi.txpower", "TX Power")}
              srPrefix={band}
              help={t("wifi.helpTxPower", "Transmit strength. Lower it to reduce range and interference; High for maximum coverage.")}
            >
              <SelectField id={`tx-${b}`} value={form[`txpower_${b}`]} options={txOpts} disabled={locked} onChange={(v) => set(`txpower_${b}`, v)} />
            </FieldRow>
            <Row
              label={
                <span className="flex items-center">
                  {t("wifi.hideSsid", "Hide SSID")}
                  <Help
                    text={t("wifi.helpHideSsid", "Stops broadcasting the network name. Devices must add it manually — modest privacy, not real security.")}
                    label={t("wifi.hideSsid", "Hide SSID")}
                  />
                </span>
              }
              control={
                <Switch
                  label={t("wifi.hideSsidOf", "Hide {{band}} SSID", { band })}
                  isSelected={form[`hidden_${b}`] === "1"}
                  isDisabled={locked}
                  onChange={(on) => set(`hidden_${b}`, on ? "1" : "0")}
                />
              }
            />
          </div>
        )}
      </section>
    );
  }

  const showReconnect =
    reconnectNote !== null && ["waitDevice", "waitTimeout", "applied", "unknown", "verifying"].includes(op.phase);

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("wifi.title", "WiFi Settings")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">{t("wifi.desc", "Configure 2.4 GHz and 5 GHz radios.")}</p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={meta}
          actions={
            !data && st.error ? (
              <Button variant="secondary" size="sm" onPress={() => st.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        {data && (
          <Group>
            <Row
              label={t("guestwifi.title", "Guest WiFi")}
              sub={data.guest_ssid ? <span className="nd-mono">{data.guest_ssid}</span> : undefined}
              value={
                data.guest_disabled_2g === "" && data.guest_disabled_5g === ""
                  ? "—"
                  : data.guest_disabled_2g !== "1" || data.guest_disabled_5g !== "1"
                    ? t("common.on", "On")
                    : t("common.off", "Off")
              }
              href="/router/wifi-guest"
            />
          </Group>
        )}

        {st.stale && data && (
          <p className="nd-aux px-1">
            <Freshness stale lastOkAt={st.lastOkAt} what={t("wifi.settingsWord", "Settings")} />
            {t("wifi.refreshToEdit", " — refresh before changing anything.")}{" "}
            <Button variant="secondary" size="sm" onPress={() => st.mutate()}>
              {t("wifi.refresh", "Refresh")}
            </Button>
          </p>
        )}

        {/* ── global ── */}
        <section>
          <GroupTitle>{t("wifi.globalTitle", "Global")}</GroupTitle>
          <p className="nd-aux -mt-1 mb-3 px-1">{t("wifi.globalDesc", "Master switches for both radios.")}</p>
          {!form ? (
            skeleton
          ) : (
            <div className={`nd-group${st.stale ? " nd-stale" : ""}`}>
              <Row
                label={t("wifi.onoff", "WiFi On/Off")}
                sub={form.wifi_onoff === "1" ? t("common.on", "On") : t("common.off", "Off")}
                control={
                  <Switch
                    label={t("wifi.onoff", "WiFi On/Off")}
                    isSelected={form.wifi_onoff === "1"}
                    isDisabled={locked}
                    onChange={(on) => set("wifi_onoff", on ? "1" : "0")}
                  />
                }
              />
              <Row
                label={
                  <span className="flex items-center">
                    {t("wifi.wifi6", "WiFi 6 (802.11ax)")}
                    <Help
                      text={t("wifi.helpWifi6", "Faster speeds and better performance when many devices are connected. Turn off only if old gear refuses to connect.")}
                      label={t("wifi.wifi6", "WiFi 6 (802.11ax)")}
                    />
                  </span>
                }
                sub={form.wifi6_switch === "1" ? t("common.enabled", "Enabled") : t("common.disabled", "Disabled")}
                control={
                  <Switch
                    label={t("wifi.wifi6", "WiFi 6 (802.11ax)")}
                    isSelected={form.wifi6_switch === "1"}
                    isDisabled={locked}
                    onChange={(on) => set("wifi6_switch", on ? "1" : "0")}
                  />
                }
              />
              <FieldRow
                id="region"
                label={t("wifi.region", "Region")}
                help={t("wifi.helpRegion", "Regulatory domain — sets which channels and TX power are allowed. Match your country. US enables more 5 GHz power and upper channels but drops 2.4 GHz ch 12–13. Changing it restarts WiFi.")}
              >
                <SelectField id="region" value={form.country} options={REGIONS} disabled={locked} onChange={setRegion} />
              </FieldRow>
            </div>
          )}
        </section>

        {bandGroup("2g")}
        {bandGroup("5g")}

        {/* ── apply ── */}
        <section aria-labelledby="wifi-apply-title">
          <GroupTitle id="wifi-apply-title">{t("wifi.applyTitle", "Apply")}</GroupTitle>
          <div className="nd-group grid gap-3 p-4 lg:p-5">
            <p className="nd-aux" role="status">
              {!form
                ? t("wifi.statusLoading", "Reading Wi-Fi…")
                : changed.length === 0
                  ? t("wifi.noChanges", "No changes yet. Change a setting above, then apply.")
                  : t("wifi.nChanges", "{{n}} change(s) not applied yet.", { n: changed.length })}
            </p>
            {errs.length > 0 && (
              <ul className="grid gap-1" role="alert">
                {errs.map((e) => (
                  <li key={e}>
                    <StatusMark tone="bad">{e}</StatusMark>
                  </li>
                ))}
              </ul>
            )}
            <div className="flex flex-wrap items-center gap-3">
              <span {...(pending?.tier === 2 ? inline.triggerProps : {})}>
                <Button
                  onPress={askApply}
                  isDisabled={locked || changed.length === 0 || errs.length > 0}
                  pending={busy}
                >
                  {t("common.apply", "Apply")}
                </Button>
              </span>
              {changed.length > 0 && (
                <Button variant="secondary" isDisabled={busy} onPress={() => setDraft(null)}>
                  {t("wifi.discard", "Discard changes")}
                </Button>
              )}
            </div>
            {pending?.tier === 2 && (
              <ConfirmInline
                id={inline.id}
                open
                actionLabel={t("wifi.applyAction", "apply the Wi-Fi settings")}
                consequence={pending.consequence}
                onCancel={cancelPending}
                onConfirm={go}
              />
            )}
            <OpResult op={op} />
            {showReconnect && (
              <p className="nd-body" role="status">
                <StatusMark tone="warn">{reconnectNote}</StatusMark>
              </p>
            )}
            {op.phase === "applied" && (
              <p className="nd-aux">
                {switchedMaster
                  ? t("wifi.appliedNoteMaster", "The device's settings show the new master switch. Whether that switch really turns Wi-Fi on or off on this firmware is not confirmed yet — check the status at the top in a minute.")
                  : t("wifi.appliedNote", "The device's settings show the change. Wi-Fi reloads in the background; the status at the top catches up within a minute.")}
              </p>
            )}
          </div>
        </section>
      </div>

      {pending?.tier === 3 && (
        <ConfirmDialog
          open
          onOpenChange={(o) => !o && setPending(null)}
          title={t("wifi.confirmTitle", "Apply the Wi-Fi settings?")}
          what={pending.consequence}
          downtime={pending.downtime}
          recovery={pending.recovery}
          actionLabel={t("nd.confirmAction", "Confirm: {{action}}", { action: t("wifi.applyAction", "apply the Wi-Fi settings") })}
          cutsUplink
          onConfirm={go}
        />
      )}
    </>
  );
}
