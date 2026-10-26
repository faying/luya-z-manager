//! The layered verdict (1a「分层结论」): only measured numbers; a layer that could not be
//! measured says why. One rule per layer, in the contract's fixed order.
use crate::Radio;
use serde::Serialize;

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LayerState {
    Ok,
    Warn,
    Untested,
}
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Layer {
    pub key: &'static str,
    pub state: LayerState,
    pub value_zh: String,
    pub value_en: String,
    /// only on untested layers
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Verdict {
    pub bottlenecks: Vec<&'static str>,
    pub layers: Vec<Layer>,
    pub summary_zh: String,
    pub summary_en: String,
}
/// A loaded stage's latency probes: median (capped samples included) and how many.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Loaded {
    pub median: Option<f64>,
    pub n: usize,
}
/// What the rules read. `Err` = not measured, with the contract's reason.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Inputs<'a> {
    pub multi: f64,
    pub single: Result<f64, &'static str>,
    pub chill: Result<f64, &'static str>,
    pub idle_ms: Option<f64>,
    pub loaded_down: Loaded,
    pub loaded_up: Loaded,
    pub min_probes: usize,
    pub radio: Option<&'a Radio>,
    pub cpu_peak: Option<f64>,
    pub mihomo_cpu: Option<f64>,
}
/// single: multi ÷ single ≥ 2
pub(crate) const SINGLE_RATIO: f64 = 2.0;
/// proxy: CHILL ÷ direct multi ≤ 0.7
pub(crate) const PROXY_RATIO: f64 = 0.7;
/// queue: loaded median − idle median ≥ 100 ms
pub(crate) const QUEUE_MS: f64 = 100.0;
/// below min_probes, this many samples already over QUEUE_MS still count (slow probes)
const FEW_PROBES: usize = 5;
/// radio: SINR below this is a weak signal
pub(crate) const WEAK_SINR_DB: f64 = 3.0;

type Rule = fn(&Inputs) -> Layer;
/// The fixed order of `layers`; `bottlenecks` keeps it too.
const RULES: [(&str, Rule); 5] = [
    ("single", single),
    ("proxy", proxy),
    ("queue", queue),
    ("radio", radio),
    ("cell_wifi", cell_wifi),
];

fn mbps(v: f64) -> String {
    if v >= 10.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}
fn layer(key: &'static str, warn: bool, zh: String, en: String) -> Layer {
    Layer {
        key,
        state: if warn {
            LayerState::Warn
        } else {
            LayerState::Ok
        },
        value_zh: zh,
        value_en: en,
        reason: None,
    }
}
/// An unmeasured layer: the row's text names the reason.
fn untested(key: &'static str, reason: &'static str) -> Layer {
    let (zh, en) = match reason {
        "not_requested" => ("没开「经 CHILL」", "\"Via CHILL\" off"),
        "chill_not_running" => ("没测 · CHILL 没在运行", "Not tested · CHILL not running"),
        "chill_stopped" => ("没测 · CHILL 中途停了", "Not tested · CHILL stopped midway"),
        "changed_cell" => ("没测 · 中途换了小区", "Not tested · cell changed"),
        "changed_node" => ("没测 · 中途换了节点", "Not tested · node changed"),
        "changed_exit" => ("没测 · 中途换了主出口", "Not tested · exit changed"),
        "budget" => ("没测 · 流量上限", "Not tested · data limit"),
        "few_samples" => ("没测 · 样本不足", "Not tested · too few samples"),
        _ => ("没测", "Not tested"),
    };
    Layer {
        key,
        state: LayerState::Untested,
        value_zh: zh.into(),
        value_en: en.into(),
        reason: Some(reason),
    }
}
fn single(i: &Inputs) -> Layer {
    let s = match i.single {
        Ok(s) if s > 0.0 => s,
        Ok(_) => return untested("single", "budget"),
        Err(r) => return untested("single", r),
    };
    let ratio = i.multi / s;
    let warn = ratio >= SINGLE_RATIO;
    let (mut zh, mut en) = (
        format!("{} Mbps，多连接的 1/{ratio:.1}", mbps(s)),
        format!("{} Mbps, 1/{ratio:.1} of multi", mbps(s)),
    );
    if let (true, Some(c)) = (warn, i.cpu_peak) {
        zh.push_str(&format!("，CPU 峰值 {c:.0}%"));
        en.push_str(&format!(", CPU peak {c:.0}%"));
    }
    layer("single", warn, zh, en)
}
fn proxy(i: &Inputs) -> Layer {
    let c = match i.chill {
        Ok(c) => c,
        Err(r) => return untested("proxy", r),
    };
    let ratio = if i.multi > 0.0 { c / i.multi } else { 1.0 };
    let warn = ratio <= PROXY_RATIO;
    let pct = (ratio * 100.0).round();
    let (mut zh, mut en) = (
        format!("经 CHILL {} Mbps，直连的 {pct:.0}%", mbps(c)),
        format!("{} Mbps via CHILL, {pct:.0}% of direct", mbps(c)),
    );
    if let Some(m) = i.mihomo_cpu {
        zh.push_str(&format!("，mihomo CPU {m:.0}%"));
        en.push_str(&format!(", mihomo CPU {m:.0}%"));
    }
    layer("proxy", warn, zh, en)
}
fn queue(i: &Inputs) -> Layer {
    let Some(idle) = i.idle_ms else {
        return untested("queue", "few_samples");
    };
    // A probe stuck in the queue is itself the evidence: when each probe takes seconds the
    // phase cannot reach min_probes, so a few samples already over the line still count.
    let deltas: Vec<(f64, &str, &str)> = [
        (i.loaded_down, "下载时", "downloading"),
        (i.loaded_up, "上传时", "uploading"),
    ]
    .into_iter()
    .filter_map(|(l, zh, en)| {
        let d = l.median? - idle;
        (l.n >= i.min_probes || (l.n >= FEW_PROBES && d >= QUEUE_MS)).then_some((d, zh, en))
    })
    .collect();
    let Some((worst, zh, en)) = deltas
        .into_iter()
        .reduce(|a, b| if b.0 > a.0 { b } else { a })
    else {
        return untested("queue", "few_samples");
    };
    let shown = worst.max(0.0);
    layer(
        "queue",
        worst >= QUEUE_MS,
        format!("{zh}延迟 +{shown:.0} ms"),
        format!("Latency +{shown:.0} ms {en}"),
    )
}
fn radio(i: &Inputs) -> Layer {
    let Some(r) = i.radio.filter(|r| r.sinr_db.is_some() || r.band.is_some()) else {
        // no reading from the core: not a contract reason, so the row carries none
        return Layer {
            key: "radio",
            state: LayerState::Untested,
            value_zh: "没有读数".into(),
            value_en: "No reading".into(),
            reason: None,
        };
    };
    let mut parts = Vec::new();
    if let Some(s) = r.sinr_db {
        parts.push(format!(
            "SINR {:.0} dB",
            if s.round() == 0.0 { 0.0 } else { s }
        )); // no "-0"
    }
    if let Some(b) = &r.band {
        parts.push(b.clone());
    }
    if let Some(n) = r.ca.filter(|n| *n > 1) {
        parts.push(format!("{n}CA"));
    }
    let read = parts.join(" · ");
    let weak = r.sinr_db.is_some_and(|s| s < WEAK_SINR_DB);
    if weak {
        layer(
            "radio",
            true,
            format!("{read}，信号差，以上数字受限于无线"),
            format!("{read}, weak signal: the numbers above are limited by radio"),
        )
    } else {
        layer(
            "radio",
            false,
            format!("{read}（只供参考）"),
            format!("{read} (reference only)"),
        )
    }
}
fn cell_wifi(_: &Inputs) -> Layer {
    untested("cell_wifi", "phase_two")
}
fn name(key: &str) -> (&'static str, &'static str) {
    match key {
        "single" => ("单连接", "single connection"),
        "proxy" => ("代理", "proxy"),
        "queue" => ("下载时卡", "lag under load"),
        _ => ("无线", "radio"),
    }
}
pub(crate) fn judge(i: &Inputs) -> Verdict {
    let layers: Vec<Layer> = RULES.iter().map(|(_, rule)| rule(i)).collect();
    let bottlenecks: Vec<&'static str> = layers
        .iter()
        .filter(|l| l.state == LayerState::Warn && l.key != "cell_wifi")
        .map(|l| l.key)
        .collect();
    let (zh, en): (Vec<_>, Vec<_>) = bottlenecks.iter().take(2).map(|k| name(k)).unzip();
    let (summary_zh, summary_en) = match bottlenecks.len() {
        0 => ("没发现瓶颈".to_string(), "No bottleneck found".to_string()),
        1 | 2 => (
            format!("慢在{}", zh.join("、")),
            format!("Slow at {}", en.join(", ")),
        ),
        n => (
            format!("慢在{}等 {n} 处", zh.join("、")),
            format!("Slow at {} and {} more", en.join(", "), n - 2),
        ),
    };
    Verdict {
        bottlenecks,
        layers,
        summary_zh,
        summary_en,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base(radio: Option<&Radio>) -> Inputs<'_> {
        Inputs {
            multi: 100.0,
            single: Ok(80.0),
            chill: Err("not_requested"),
            idle_ms: Some(30.0),
            loaded_down: Loaded {
                median: Some(50.0),
                n: 40,
            },
            loaded_up: Loaded {
                median: Some(60.0),
                n: 40,
            },
            min_probes: 20,
            radio,
            cpu_peak: Some(91.0),
            mihomo_cpu: Some(45.0),
        }
    }
    type Edit = fn(&mut Inputs);
    /// (layer, edit, radio, expected state, expected reason)
    type Case<'a> = (
        &'a str,
        Edit,
        Option<&'a Radio>,
        LayerState,
        Option<&'a str>,
    );
    /// Each rule: hit, miss and untested (with the reason).
    #[test]
    fn every_rule_hits_misses_and_reports_untested() {
        let good = Radio {
            sinr_db: Some(14.0),
            band: Some("n78".into()),
            ca: Some(2),
        };
        let weak = Radio {
            sinr_db: Some(1.0),
            ..good.clone()
        };
        let cases: &[Case] = &[
            (
                "single",
                |i| i.single = Ok(40.0),
                None,
                LayerState::Warn,
                None,
            ),
            (
                "single",
                |i| i.single = Ok(51.0),
                None,
                LayerState::Ok,
                None,
            ),
            (
                "single",
                |i| i.single = Err("budget"),
                None,
                LayerState::Untested,
                Some("budget"),
            ),
            (
                "proxy",
                |i| i.chill = Ok(70.0),
                None,
                LayerState::Warn,
                None,
            ),
            ("proxy", |i| i.chill = Ok(71.0), None, LayerState::Ok, None),
            (
                "proxy",
                |_| {},
                None,
                LayerState::Untested,
                Some("not_requested"),
            ),
            (
                "proxy",
                |i| i.chill = Err("chill_not_running"),
                None,
                LayerState::Untested,
                Some("chill_not_running"),
            ),
            (
                "proxy",
                |i| i.chill = Err("chill_stopped"),
                None,
                LayerState::Untested,
                Some("chill_stopped"),
            ),
            (
                "proxy",
                |i| i.chill = Err("changed_exit"),
                None,
                LayerState::Untested,
                Some("changed_exit"),
            ),
            (
                "queue",
                |i| i.loaded_down.median = Some(130.0),
                None,
                LayerState::Warn,
                None,
            ),
            (
                "queue",
                |i| i.loaded_up.median = Some(130.0),
                None,
                LayerState::Warn,
                None,
            ),
            (
                "queue",
                |i| i.loaded_down.median = Some(129.0),
                None,
                LayerState::Ok,
                None,
            ),
            // a hit with very few probes is not judged
            (
                "queue",
                |i| {
                    i.loaded_down = Loaded {
                        median: Some(900.0),
                        n: 4,
                    };
                    i.loaded_up.n = 3
                },
                None,
                LayerState::Untested,
                Some("few_samples"),
            ),
            // probes slowed by the queue: few samples over the line still count
            (
                "queue",
                |i| {
                    i.loaded_down.n = 3;
                    i.loaded_up = Loaded {
                        median: Some(1800.0),
                        n: 5,
                    }
                },
                None,
                LayerState::Warn,
                None,
            ),
            (
                "queue",
                |i| {
                    i.loaded_down = Loaded {
                        median: Some(900.0),
                        n: 19,
                    }
                },
                None,
                LayerState::Warn,
                None,
            ),
            ("radio", |_| {}, Some(&weak), LayerState::Warn, None),
            ("radio", |_| {}, Some(&good), LayerState::Ok, None),
            ("radio", |_| {}, None, LayerState::Untested, None),
            (
                "cell_wifi",
                |_| {},
                Some(&good),
                LayerState::Untested,
                Some("phase_two"),
            ),
        ];
        for (k, (key, edit, radio, state, reason)) in cases.iter().enumerate() {
            let mut i = base(*radio);
            edit(&mut i);
            let v = judge(&i);
            let l = v.layers.iter().find(|l| l.key == *key).unwrap();
            assert_eq!((l.state, l.reason), (*state, *reason), "case {k}: {l:?}");
            assert_eq!(
                v.bottlenecks.contains(key),
                *state == LayerState::Warn,
                "case {k}"
            );
        }
    }
    #[test]
    fn layers_keep_the_fixed_order_and_summaries_name_at_most_two() {
        let i = base(None);
        let v = judge(&i);
        let keys: Vec<_> = v.layers.iter().map(|l| l.key).collect();
        assert_eq!(keys, ["single", "proxy", "queue", "radio", "cell_wifi"]);
        assert_eq!(v.summary_zh, "没发现瓶颈");
        assert_eq!(v.layers[0].value_zh, "80 Mbps，多连接的 1/1.2");
        let weak = Radio {
            sinr_db: Some(1.0),
            band: Some("n78".into()),
            ca: Some(2),
        };
        let mut i = base(Some(&weak));
        i.single = Ok(25.0);
        i.chill = Ok(30.0);
        i.loaded_down.median = Some(230.0);
        let v = judge(&i);
        assert_eq!(v.bottlenecks, ["single", "proxy", "queue", "radio"]);
        assert_eq!(v.summary_zh, "慢在单连接、代理等 4 处");
        assert_eq!(v.summary_en, "Slow at single connection, proxy and 2 more");
        assert_eq!(
            v.layers[0].value_zh,
            "25 Mbps，多连接的 1/4.0，CPU 峰值 91%"
        );
        assert_eq!(
            v.layers[1].value_zh,
            "经 CHILL 30 Mbps，直连的 30%，mihomo CPU 45%"
        );
        assert_eq!(v.layers[2].value_zh, "下载时延迟 +200 ms");
        assert_eq!(
            v.layers[3].value_zh,
            "SINR 1 dB · n78 · 2CA，信号差，以上数字受限于无线"
        );
        i.chill = Err("chill_stopped");
        i.radio = None;
        let v = judge(&i);
        assert_eq!(v.summary_zh, "慢在单连接、下载时卡");
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["layers"][1]["reason"], "chill_stopped");
        assert!(json["layers"][0].get("reason").is_none());
    }
}
