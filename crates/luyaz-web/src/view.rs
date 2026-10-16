//! The signal page's view model, ported from web/src/lib/home.ts (`carriers`, `sigState`,
//! `lteAnchor`, …) and lib/signalWords.ts, reading u60d's `signal` / `qos` / `interfaces` blocks
//! instead of the vendor reply behind /api/network/signal. Field map (vendor → block):
//! nr5g_rsrp→nr_rsrp, nr5g_action_band→nr_band, nr5g_action_channel→nr_channel, nr5g_bandwidth→nr_bw,
//! wan_active_channel→lte_channel, wan_active_band→band, signalbar→bars, network_type→type,
//! network_provider_fullname→operator, rmcc/rmnc→mcc/mnc, net_select_mode as is (how the network
//! is chosen; `net_select` is the radio preference, a different thing).

use std::time::Instant;

use serde_json::Value;

use crate::hub::{Link, St, TREND_SECS};
use crate::i18n::Lang;

/// JS `num()`: number, or a string that parses; "" and garbage are None.
pub fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.trim().is_empty() => {
            s.trim().parse::<f64>().ok().filter(|f| f.is_finite())
        }
        _ => None,
    }
}

fn int(v: &Value) -> Option<i64> {
    num(v).map(|f| f as i64)
}

/// JS truthiness of `num(x)`: present and not 0.
fn nonzero(v: &Value) -> bool {
    num(v).is_some_and(|f| f != 0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Carrier {
    pub nr: bool,
    pub band: String,
    pub pci: Option<i64>,
    pub arfcn: Option<i64>,
    pub bw: Option<f64>,
    pub rsrp: Option<i64>,
    pub rsrq: Option<f64>,
    pub sinr: Option<f64>,
    pub serving: bool,
}

fn parse_ca(s: &Value, nr: bool) -> Vec<Carrier> {
    let Some(s) = s.as_str() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for rec in s.split(';').filter(|r| !r.trim().is_empty()) {
        let f: Vec<f64> = match rec.split(',').map(|x| x.trim().parse::<f64>()).collect() {
            Ok(f) => f,
            Err(_) => continue,
        };
        let c = |pci: f64, band: f64, arfcn: f64, bw: f64| Carrier {
            nr,
            band: format!("{band}"),
            pci: Some(pci as i64),
            arfcn: Some(arfcn as i64),
            bw: Some(bw),
            rsrp: None,
            rsrq: None,
            sinr: None,
            serving: false,
        };
        match f.len() {
            5 => out.push(c(f[0], f[1], f[3], f[4])),
            11 => out.push(Carrier {
                rsrp: Some(f[7] as i64),
                rsrq: Some(f[8]),
                sinr: Some(f[9]),
                ..c(f[1], f[3], f[4], f[5])
            }),
            _ => {}
        }
    }
    out
}

fn strip(s: &Value, prefix: char) -> String {
    let s = match s {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let lower = prefix.to_ascii_lowercase();
    s.strip_prefix(prefix)
        .or_else(|| s.strip_prefix(lower))
        .map(str::to_owned)
        .unwrap_or(s)
}

/// Serving cell first, then the CA records (home.ts `carriers`).
pub fn carriers(sig: &Value) -> Vec<Carrier> {
    let mut out = Vec::new();
    if nonzero(&sig["nr_rsrp"]) {
        out.push(Carrier {
            nr: true,
            band: strip(&sig["nr_band"], 'N'),
            pci: int(&sig["nr_pci"]),
            arfcn: int(&sig["nr_channel"]),
            bw: num(&sig["nr_bw"]),
            rsrp: int(&sig["nr_rsrp"]),
            rsrq: num(&sig["nr_rsrq"]),
            sinr: num(&sig["nr_snr"]),
            serving: true,
        });
    } else if nonzero(&sig["lte_rsrp"]) {
        out.push(Carrier {
            nr: false,
            band: strip(&sig["band"], 'B'),
            pci: int(&sig["lte_pci"]),
            arfcn: int(&sig["lte_channel"]),
            bw: None,
            rsrp: int(&sig["lte_rsrp"]),
            rsrq: num(&sig["lte_rsrq"]),
            sinr: num(&sig["lte_snr"]),
            serving: true,
        });
    }
    for c in parse_ca(&sig["nrca"], true)
        .into_iter()
        .chain(parse_ca(&sig["lteca"], false))
    {
        if let Some(s) = out
            .iter_mut()
            .find(|o| o.serving && o.nr == c.nr && o.pci == c.pci && o.arfcn == c.arfcn)
        {
            if s.bw.is_none() {
                s.bw = c.bw;
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub fn serving_rsrp(sig: &Value) -> Option<i64> {
    carriers(sig).first().and_then(|c| c.rsrp)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SigState {
    Loading,
    Stale,
    None,
    Weak,
    Good,
}

/// home.ts `sigState` without the SIM branch (the signal page passes no SIM state).
pub fn sig_state(ever_valid: bool, valid: bool, bars: Option<i64>, sinr: Option<f64>) -> SigState {
    if !ever_valid {
        return SigState::Loading;
    }
    if !valid {
        return SigState::Stale;
    }
    match bars {
        None => SigState::None,
        Some(b) if b <= 0 => SigState::None,
        Some(b) if b <= 2 || sinr.is_some_and(|s| s < 0.0) => SigState::Weak,
        _ => SigState::Good,
    }
}

/// lib/signalWords.ts `rsrpWord` & co.
fn level(v: Option<f64>, good: f64, fair: f64, l: Lang) -> &'static str {
    match v {
        None => l.pick("No signal", "没有信号"),
        Some(v) if v >= good => l.pick("Good", "良好"),
        Some(v) if v >= fair => l.pick("Fair", "一般"),
        _ => l.pick("Weak", "偏弱"),
    }
}

fn radio_name(t: &str) -> String {
    match t.to_ascii_uppercase().as_str() {
        "SA" => "5G SA".into(),
        "NSA" => "5G NSA".into(),
        "LTE" => "4G LTE".into(),
        _ => t.into(),
    }
}

/// JS `String(number)` for the readouts: 3 → "3", 3.5 → "3.5".
pub fn js_num(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

pub(crate) fn fmt_mbps(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0}")
    } else {
        js_num((v * 10.0).round() / 10.0)
    }
}

pub struct Readout {
    /// None = still loading (skeleton); Some("") never happens; "—" for no value.
    pub value: Option<String>,
    pub empty: bool,
    pub word: String,
}

pub struct Anchor {
    pub pci: String,
    pub earfcn: String,
    pub band: String,
    pub signal: String,
}

/// Everything the page prints, already formatted, in the page language `l`.
pub struct SignalView {
    pub l: Lang,
    pub tone: &'static str,
    pub symbol: &'static str,
    pub word: &'static str,
    /// The status block's reason line, as variants the template knows how to print.
    pub reason: Reason,
    pub meta: Option<String>,
    pub retry: bool,
    pub stale: bool,
    pub rsrp: Readout,
    pub sinr: Readout,
    pub rsrq: Readout,
    pub bars: Readout,
    pub trend: Option<Trend>,
    pub cell: Vec<(&'static str, &'static str, String, bool)>,
    pub anchor: Option<Anchor>,
}

pub enum Reason {
    None,
    /// "数字停在 N 分钟前" (+ " · 正在重试" when u60d itself is gone).
    Stopped {
        minutes: u64,
        retrying: bool,
    },
    Weak {
        rsrp: Option<i64>,
    },
    NoService,
}

pub struct Trend {
    pub l: Lang,
    pub points: String,
    pub lo: i64,
    pub hi: i64,
}

/// SVG geometry of web/src/components/signal/TrendChart.tsx.
const W: f64 = 600.0;
const H: f64 = 160.0;
const TOP: f64 = -70.0;
const BOTTOM: f64 = -130.0;
const PAD_L: f64 = 40.0;
const PAD_R: f64 = 8.0;
const PAD_Y: f64 = 8.0;
/// The Next.js page waits for 30 samples at 2 s (one minute); u60d's signal block comes about
/// every ~6 s (device recording 10-11: 105 in 594 s), so one minute is 10 samples.
const MIN_SAMPLES: usize = 10;

pub fn trend_y(v: f64) -> f64 {
    let c = v.clamp(BOTTOM, TOP);
    PAD_Y + ((TOP - c) / (TOP - BOTTOM)) * (H - PAD_Y * 2.0)
}

fn trend(st: &St, l: Lang) -> Option<Trend> {
    if st.history.len() < MIN_SAMPLES {
        return None;
    }
    let last = st.history.back()?.0;
    let first = last.saturating_sub(TREND_SECS);
    let x = |t: u64| {
        PAD_L + ((t.saturating_sub(first)) as f64 / TREND_SECS as f64) * (W - PAD_L - PAD_R)
    };
    let points = st
        .history
        .iter()
        .map(|&(t, v)| format!("{:.1},{:.1}", x(t), trend_y(v as f64)))
        .collect::<Vec<_>>()
        .join(" ");
    let lo = st.history.iter().map(|h| h.1).min()?;
    let hi = st.history.iter().map(|h| h.1).max()?;
    Some(Trend { l, points, lo, hi })
}

/// `net_select_mode` in words (web/src/lib/home.ts selectionWord); other values as they are.
pub(crate) fn selection_word(v: &Value, l: Lang) -> String {
    match v.as_str().unwrap_or("") {
        "auto_select" => l.pick("Automatic", "自动").into(),
        "manual_select" => l.pick("Manual", "手动").into(),
        "" => "—".into(),
        other => other.into(),
    }
}

pub fn signal_view(st: &St, now: Instant, l: Lang) -> SignalView {
    let block = st.blocks.get("signal");
    let sig = block.map(|b| &b.data).unwrap_or(&Value::Null);
    let link_ok = st.link == Link::Live;
    let ever_valid = block.is_some();
    let valid = ever_valid && link_ok && !block.is_some_and(|b| b.stale);
    // Only a lost upstream is "lost contact". Idle (nobody was watching, so no connection) and
    // Connecting keep the last values greyed under a neutral "connecting", never an alarm: the
    // first visit after the idle disconnect would otherwise flash red and announce it.
    let all_down = ever_valid && st.link == Link::Down;
    let reconnecting = ever_valid && matches!(st.link, Link::Idle | Link::Connecting);
    let cs = carriers(sig);
    let serving = cs.first();
    let bars = int(&sig["bars"]);
    let state = sig_state(ever_valid, valid, bars, serving.and_then(|c| c.sinr));
    let loading = state == SigState::Loading;
    let minutes = st
        .last_ok
        .map(|t| now.duration_since(t).as_secs() / 60)
        .unwrap_or(0);

    let (tone, word, reason, retry) = match state {
        _ if reconnecting => (
            "neutral",
            l.pick("Connecting to the device…", "正在连接设备…"),
            Reason::None,
            false,
        ),
        SigState::Loading => (
            "neutral",
            l.pick("Connecting to the device…", "正在连接设备…"),
            Reason::None,
            false,
        ),
        SigState::Stale => (
            if all_down { "bad" } else { "stale" },
            if all_down {
                l.pick("Lost contact with the device", "设备连接中断")
            } else {
                l.pick("Signal and speed are not updating", "信号与网速暂未更新")
            },
            Reason::Stopped {
                minutes,
                retrying: all_down,
            },
            true,
        ),
        SigState::None => (
            "warn",
            l.pick("No network service", "没有网络服务"),
            Reason::NoService,
            false,
        ),
        SigState::Weak => (
            "warn",
            l.pick("Weak signal", "信号偏弱"),
            Reason::Weak {
                rsrp: serving.and_then(|c| c.rsrp),
            },
            false,
        ),
        SigState::Good => ("ok", l.pick("Signal good", "信号良好"), Reason::None, false),
    };
    let symbol = match tone {
        "warn" => "▲",
        "bad" => "■",
        _ => "●",
    };
    let (nr, lte) = (
        cs.iter().filter(|c| c.nr).count(),
        cs.iter().filter(|c| !c.nr).count(),
    );
    let cc = match (nr, lte) {
        (0, 0) => None,
        (n, 0) => Some(match l {
            Lang::En => format!("{n} NR carriers"),
            Lang::Zh => format!("{n} 个 NR 载波"),
        }),
        (0, m) => Some(match l {
            Lang::En => format!("{m} LTE carriers"),
            Lang::Zh => format!("{m} 个 LTE 载波"),
        }),
        (n, m) => Some(match l {
            Lang::En => format!("{m} LTE + {n} NR carriers"),
            Lang::Zh => format!("{m} 个 LTE + {n} 个 NR 载波"),
        }),
    };
    let meta = (!loading).then(|| {
        let t = sig["type"].as_str().unwrap_or_default();
        [
            (!t.is_empty()).then(|| radio_name(t)),
            sig["operator"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            cc,
            bars.filter(|b| *b >= 0).map(|b| match l {
                Lang::En => format!("{b}/5 bars"),
                Lang::Zh => format!("{b}/5 格"),
            }),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    });

    let readout = |v: Option<f64>, word: &str| Readout {
        value: if loading {
            None
        } else {
            Some(v.map(js_num).unwrap_or_else(|| "—".into()))
        },
        empty: v.is_none(),
        word: word.to_owned(),
    };
    let rsrp = serving.and_then(|c| c.rsrp).map(|v| v as f64);
    let sinr = serving.and_then(|c| c.sinr);
    let rsrq = serving.and_then(|c| c.rsrq);
    let bars_ok = bars.filter(|b| *b >= 0);

    let cell_id = if serving.is_some_and(|c| !c.nr) {
        int(&sig["lte_cell_id"]).or_else(|| int(&sig["nr_cell_id"]))
    } else {
        int(&sig["nr_cell_id"]).or_else(|| int(&sig["lte_cell_id"]))
    };
    let band = serving.map(|c| {
        if c.nr {
            format!("n{}", c.band)
        } else {
            format!("B{}", c.band)
        }
    });
    let rssi = match serving.map(|c| c.nr) {
        Some(true) => int(&sig["nr_rssi"]),
        Some(false) => int(&sig["lte_rssi"]),
        None => None,
    }
    .or_else(|| int(&sig["rssi"]));
    let plmn = match (int(&sig["mcc"]), int(&sig["mnc"])) {
        (Some(mcc), Some(mnc)) if (100..1000).contains(&mcc) && (0..1000).contains(&mnc) => {
            Some(format!("{mcc}-{mnc:02}"))
        }
        _ => None,
    };
    let dash = |o: Option<String>| o.unwrap_or_else(|| "—".into());
    let mut cell = vec![
        (
            "Cell ID",
            l.pick(
                "Unique ID of the tower sector serving you.",
                "为你服务的基站扇区唯一 ID。",
            ),
            dash(cell_id.map(|v| v.to_string())),
            true,
        ),
        (
            "PCI",
            l.pick(
                "Physical Cell ID — tells nearby towers apart.",
                "物理小区标识 —— 用于区分邻近基站。",
            ),
            dash(serving.and_then(|c| c.pci).map(|v| v.to_string())),
            true,
        ),
        (
            "ARFCN",
            l.pick(
                "The radio channel number your device is tuned to.",
                "你的设备所调谐到的无线信道号。",
            ),
            dash(serving.and_then(|c| c.arfcn).map(|v| v.to_string())),
            true,
        ),
        (
            l.pick("Band", "频段"),
            l.pick("The frequency band currently in use.", "当前使用的频段。"),
            dash(band),
            true,
        ),
        (
            l.pick("Bandwidth", "带宽"),
            l.pick(
                "Channel width — wider generally means faster.",
                "信道带宽 —— 越宽通常越快。",
            ),
            dash(
                serving
                    .and_then(|c| c.bw)
                    .map(|b| format!("{} MHz", js_num(b))),
            ),
            false,
        ),
        (
            "RSSI",
            l.pick(
                "Total power received on the channel, including noise and other users.",
                "这个信道上收到的总功率，包括噪声和别的用户。",
            ),
            dash(rssi.map(|r| format!("{r} dBm"))),
            false,
        ),
        (
            "PLMN",
            l.pick(
                "The network's country code and operator code (MCC-MNC).",
                "所在网络的国家码和运营商码（MCC-MNC）。",
            ),
            dash(plmn),
            true,
        ),
    ];
    if let Some(q) = st.blocks.get("qos").map(|b| &b.data) {
        let qci = int(&q["qci"]).filter(|q| *q > 0);
        let dl = num(&q["ambr_dl"]);
        let ambr = dl.map(|d| {
            format!(
                "↓ {} ↑ {} Mbps",
                fmt_mbps(d),
                num(&q["ambr_ul"])
                    .map(fmt_mbps)
                    .unwrap_or_else(|| "—".into())
            )
        });
        let v = [qci.map(|q| format!("QCI {q}")), ambr]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        cell.push((
            "QCI · AMBR",
            l.pick("QCI is the traffic class the carrier gives your data; AMBR is the speed cap the network set for this connection.", "QCI 是运营商给你数据分的业务等级；AMBR 是网络给这次连接设的速度上限。"),
            if v.is_empty() { "—".into() } else { v },
            false,
        ));
    }
    cell.push((
        l.pick("Network selection", "选网方式"),
        l.pick(
            "Whether the network is chosen automatically or manually.",
            "网络是自动还是手动选择。",
        ),
        selection_word(&sig["net_select_mode"], l),
        false,
    ));

    let anchor =
        (sig["type"].as_str() == Some("NSA") && num(&sig["lte_rsrp"]).is_some()).then(|| {
            let b = strip(&sig["band"], 'B');
            let parts = [
                ("RSRP", &sig["lte_rsrp"]),
                ("RSRQ", &sig["lte_rsrq"]),
                ("SINR", &sig["lte_snr"]),
                ("RSSI", &sig["lte_rssi"]),
            ]
            .into_iter()
            .filter_map(|(k, v)| num(v).map(|n| format!("{k} {}", js_num(n))))
            .collect::<Vec<_>>()
            .join(" · ");
            Anchor {
                pci: dash(int(&sig["lte_pci"]).map(|v| v.to_string())),
                earfcn: dash(int(&sig["lte_channel"]).map(|v| v.to_string())),
                band: if b.is_empty() || b.starts_with(['n', 'N']) {
                    "—".into()
                } else {
                    format!("B{b}")
                },
                signal: if parts.is_empty() {
                    "—".into()
                } else {
                    parts
                },
            }
        });

    SignalView {
        l,
        tone,
        symbol,
        word,
        reason,
        meta,
        retry,
        stale: !valid && ever_valid,
        rsrp: readout(rsrp, level(rsrp, -100.0, -110.0, l)),
        sinr: readout(sinr, level(sinr, 13.0, 0.0, l)),
        rsrq: readout(rsrq, level(rsrq, -10.0, -15.0, l)),
        bars: Readout {
            value: if loading {
                None
            } else {
                Some(bars_ok.map(|b| b.to_string()).unwrap_or_else(|| "—".into()))
            },
            empty: bars_ok.is_none(),
            word: bars_ok
                .map(|b| {
                    if b >= 3 {
                        l.pick("Good", "良好")
                    } else {
                        l.pick("Weak", "偏弱")
                    }
                })
                .unwrap_or_default()
                .to_owned(),
        },
        trend: trend(st, l),
        cell,
        anchor,
    }
}

/// Mobile-data roaming setting (`interfaces.cellular.roam_enable`), None until known.
pub fn roaming(st: &St) -> Option<bool> {
    let c = &st.blocks.get("interfaces")?.data["cellular"];
    num(&c["roam_enable"]).map(|v| v != 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selection_mode_in_words() {
        assert_eq!(selection_word(&json!("auto_select"), Lang::Zh), "自动");
        assert_eq!(selection_word(&json!("auto_select"), Lang::En), "Automatic");
        assert_eq!(selection_word(&json!("manual_select"), Lang::Zh), "手动");
        assert_eq!(selection_word(&json!(""), Lang::Zh), "—");
        assert_eq!(selection_word(&Value::Null, Lang::Zh), "—");
        assert_eq!(selection_word(&json!("other"), Lang::Zh), "other");
    }

    #[test]
    fn sa_serving_cell_from_the_device_recording_shape() {
        let sig = json!({"type":"SA","bars":4,"nr_rsrp":-97,"nr_snr":"3.0","nr_rsrq":-13,"nr_band":"n78","nr_bw":"100",
            "nr_pci":805,"nr_channel":426030,"lte_rsrp":-104,"lte_pci":0,"nrca":"","lteca":""});
        let cs = carriers(&sig);
        assert_eq!(cs.len(), 1);
        let c = &cs[0];
        assert!(c.nr && c.serving);
        assert_eq!(
            (c.band.as_str(), c.pci, c.arfcn, c.bw, c.rsrp, c.sinr),
            (
                "78",
                Some(805),
                Some(426030),
                Some(100.0),
                Some(-97),
                Some(3.0)
            )
        );
    }

    #[test]
    fn lte_serving_and_ca_record_that_is_the_serving_cell_only_fills_bandwidth() {
        let sig = json!({"lte_rsrp":"-95","lte_pci":"254","lte_channel":"1750","band":"B3","lteca":"254,3,0,1750,20;"});
        let cs = carriers(&sig);
        assert_eq!(cs.len(), 1);
        assert_eq!((cs[0].band.as_str(), cs[0].bw), ("3", Some(20.0)));
    }

    #[test]
    fn eleven_field_ca_records_are_extra_carriers() {
        let sig = json!({"nr_rsrp":-90,"nr_pci":1,"nr_channel":2,"lteca":"0,300,0,1,1300,20,0,-101,-11,7,0;garbage;"});
        let cs = carriers(&sig);
        assert_eq!(cs.len(), 2);
        assert_eq!(
            (cs[1].nr, cs[1].pci, cs[1].rsrp, cs[1].sinr),
            (false, Some(300), Some(-101), Some(7.0))
        );
    }

    #[test]
    fn states_match_home_ts() {
        assert_eq!(sig_state(false, false, None, None), SigState::Loading);
        assert_eq!(sig_state(true, false, Some(4), None), SigState::Stale);
        assert_eq!(sig_state(true, true, Some(0), None), SigState::None);
        assert_eq!(sig_state(true, true, None, None), SigState::None);
        assert_eq!(sig_state(true, true, Some(2), Some(20.0)), SigState::Weak);
        assert_eq!(sig_state(true, true, Some(4), Some(-1.0)), SigState::Weak);
        assert_eq!(sig_state(true, true, Some(3), Some(0.0)), SigState::Good);
    }

    fn st_with(link: Link) -> St {
        let mut blocks = std::collections::HashMap::new();
        blocks.insert(
            "signal".to_owned(),
            crate::hub::Block {
                revision: 1,
                observed_at: 1,
                stale: false,
                data: json!({"type":"SA","bars":4,"nr_rsrp":-97,"nr_snr":10}),
            },
        );
        St {
            epoch: "e".into(),
            blocks,
            link,
            last_ok: Some(Instant::now()),
            history: Default::default(),
            generation: 1,
            connects: 1,
            side: Default::default(),
            op_at: None,
            stuck: false,
            samples: Default::default(),
        }
    }

    #[test]
    fn only_a_lost_upstream_is_an_alarm() {
        let now = Instant::now();
        for link in [Link::Idle, Link::Connecting] {
            let v = signal_view(&st_with(link), now, Lang::Zh);
            assert_eq!(
                (v.tone, v.word, v.retry),
                ("neutral", "正在连接设备…", false),
                "{link:?}"
            );
            assert!(v.stale, "old values stay, greyed");
        }
        let v = signal_view(&st_with(Link::Down), now, Lang::Zh);
        assert_eq!((v.tone, v.word, v.retry), ("bad", "设备连接中断", true));
        let v = signal_view(&st_with(Link::Live), now, Lang::Zh);
        assert_eq!((v.tone, v.word), ("ok", "信号良好"));
    }

    #[test]
    fn numbers_print_like_js() {
        assert_eq!(js_num(3.0), "3");
        assert_eq!(js_num(-13.5), "-13.5");
        assert_eq!(fmt_mbps(2000.0), "2000");
        assert_eq!(fmt_mbps(12.34), "12.3");
    }
}
