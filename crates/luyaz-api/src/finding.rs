//! "Registered but no internet": the pure judgement of u60-mcp.md step 1 phase 1
//! (`docs/designs/luyaz-intelligence-pilot.md`, eng review 10-17 D4/D7/D8; task T2).
//!
//! Four layers, in the order the first broken one is picked from: signal/registration, data
//! connection, internet, DNS. Each ends ok / warn / bad / na ("can't tell"). The pilot only
//! explains: a [`Finding`] has no proposal and no outcome (those are phase 2).
//!
//! [`judge`] gives one of three verdicts:
//! - [`Verdict::None`]: an expected outage (data off, roaming off while roaming, airplane, no SIM),
//!   not registered (another story), nothing down, or all four layers ok.
//! - [`Verdict::Unverified`]: down by the passive reading but not proven: probing not allowed
//!   right now (screen off and no client, D8), a premise unreadable, or the probes said na.
//!   Shown on the API only, never on the home screen.
//! - [`Verdict::Open`]: a probe proved it. Confidence is sure or likely; "guess" (only the
//!   symptom, no layer evidence) is never opened by `judge` under D8, it stays for other callers.
//!
//! Scoped network observations carry interface/family/source/route and separate path, DNS,
//! TLS and internet states. This pilot does not establish TLS/internet usability.
//! The path probe uses bound IP-literal HTTP sockets without DNS. It is only path evidence;
//! it does not establish TLS or overall internet usability and network failures stay likely.
//! Verified HTTPS recovery checks require a separate time/certificate design, never a TLS bypass.
//!
//! Times are device-clock seconds throughout.

use serde::Serialize;

use crate::diagnose::Level;

pub const KIND_NO_INTERNET: &str = "data.no_internet";
/// A reading older than this at judgement time only gives "likely".
pub const FRESH_SECS: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerId {
    Signal,
    Data,
    Internet,
    Dns,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Sure,
    Likely,
    Guess,
}

/// Registration from `AT+CEREG?` / `AT+C5GREG?`'s stat. B31 gives no reject cause (`CEREG=3`
/// answers ERROR, 10-17), so "denied" can't say why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reg {
    Home,
    Roaming,
    Searching,
    Denied,
    NotRegistered,
    Unknown,
}

impl Reg {
    pub fn from_stat(stat: i64) -> Reg {
        match stat {
            0 => Reg::NotRegistered,
            1 => Reg::Home,
            2 => Reg::Searching,
            3 => Reg::Denied,
            5 => Reg::Roaming,
            _ => Reg::Unknown,
        }
    }
    pub fn registered(self) -> bool {
        matches!(self, Reg::Home | Reg::Roaming)
    }
}

/// One layer's latest reading (D10: the block keeps only the latest per layer).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Evidence {
    pub layer: LayerId,
    pub level: Level,
    /// Short reading or why it can't tell; untrusted operator text is cut before it gets here.
    pub detail: String,
    pub detail_en: String,
    /// When it was read.
    pub at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkEvidence>,
}

impl Evidence {
    pub fn new(
        layer: LayerId,
        level: Level,
        zh: impl Into<String>,
        en: impl Into<String>,
        at: u64,
    ) -> Evidence {
        Evidence {
            layer,
            level,
            detail: zh.into(),
            detail_en: en.into(),
            at,
            network: None,
        }
    }
    fn fresh(&self, now: u64) -> bool {
        now.checked_sub(self.at)
            .is_some_and(|age| age <= FRESH_SECS)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub kind: &'static str,
    /// Dedup key with `kind` (the card); personal, masked on the way out like the rest of sim.
    pub iccid: String,
    /// The first bad layer.
    pub layer: LayerId,
    pub confidence: Confidence,
    /// One sentence for the home screen.
    pub symptom: String,
    pub symptom_en: String,
    pub evidence: Vec<Evidence>,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    None { why: &'static str },
    Unverified { why: &'static str },
    Open(Finding),
}

/// Why it is down on purpose (D8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Expected {
    NoSim,
    Airplane,
    DataOff,
    RoamingOff,
}

/// The premises, from the core's state: `sim.state` / the screen story's state (`nosim`,
/// `airplane`), `cellular.enable`, `cellular.roam_enable`, `net.roaming`. None = unreadable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Premises {
    pub sim_present: Option<bool>,
    pub airplane: Option<bool>,
    pub data_on: Option<bool>,
    pub roaming: Option<bool>,
    pub roam_allowed: Option<bool>,
}

/// A premise needed to tell is unreadable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unreadable;

/// Ok(Some) = expected outage, Ok(None) = none.
pub fn expected_outage(p: &Premises) -> Result<Option<Expected>, Unreadable> {
    if !p.sim_present.ok_or(Unreadable)? {
        return Ok(Some(Expected::NoSim));
    }
    if p.airplane.ok_or(Unreadable)? {
        return Ok(Some(Expected::Airplane));
    }
    if !p.data_on.ok_or(Unreadable)? {
        return Ok(Some(Expected::DataOff));
    }
    if p.roaming.ok_or(Unreadable)? && !p.roam_allowed.ok_or(Unreadable)? {
        return Ok(Some(Expected::RoamingOff));
    }
    Ok(None)
}

// ---- the four layers ------------------------------------------------------------------

pub fn judge_signal(reg: Reg, at: u64) -> Evidence {
    let (level, zh, en) = match reg {
        Reg::Home => (Level::Ok, "已注册", "Registered"),
        Reg::Roaming => (Level::Ok, "已注册（漫游）", "Registered (roaming)"),
        Reg::Searching => (Level::Bad, "在搜网", "Searching"),
        Reg::Denied => (Level::Bad, "注册被拒", "Registration denied"),
        Reg::NotRegistered => (Level::Bad, "没注册", "Not registered"),
        Reg::Unknown => (Level::Na, "读不到注册状态", "No registration reading"),
    };
    Evidence::new(LayerId::Signal, level, zh, en, at)
}

/// The data connection's own reading (`zwrt_data get_wwaniface` + `AT+CGACT?` / `AT+CEER`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataIn {
    /// `connect_status`, e.g. `ipv4_ipv6_connected`, `disconnected`.
    pub connect_status: Option<String>,
    pub fail_count: Option<i64>,
    pub has_ip: Option<bool>,
    /// `AT+CGACT?` for the data cid.
    pub pdp_active: Option<bool>,
    /// `AT+CEER`, already cut short.
    pub ceer: Option<String>,
}

pub fn judge_data(d: &DataIn, at: u64) -> Evidence {
    let e = |level, zh: String, en: String| Evidence::new(LayerId::Data, level, zh, en, at);
    let Some(status) = d.connect_status.as_deref() else {
        return e(
            Level::Na,
            "读不到数据连接".into(),
            "No data connection reading".into(),
        );
    };
    let cause = d
        .ceer
        .as_deref()
        .filter(|c| !c.is_empty() && !c.contains("No cause"))
        .map(|c| format!("（{c}）"))
        .unwrap_or_default();
    let connected = match status {
        "connected" | "ipv4_connected" | "ipv6_connected" | "ipv4_ipv6_connected" => true,
        "disconnected"
        | "ipv4_disconnected"
        | "ipv6_disconnected"
        | "ipv4_ipv6_disconnected"
        | "connecting" => false,
        _ => {
            return e(
                Level::Na,
                "未知数据连接状态".into(),
                "Unknown data connection status".into(),
            );
        }
    };
    if !connected || d.pdp_active == Some(false) {
        let fails = d
            .fail_count
            .filter(|n| *n > 0)
            .map(|n| format!("，失败 {n} 次"))
            .unwrap_or_default();
        let fails_en = d
            .fail_count
            .filter(|n| *n > 0)
            .map(|n| format!(", {n} failures"))
            .unwrap_or_default();
        return e(
            Level::Bad,
            format!("数据连接没建立{fails}{cause}"),
            format!("Data connection not up{fails_en}{cause}"),
        );
    }
    if d.has_ip == Some(false) {
        return e(
            Level::Bad,
            "已连接但没有 IP".into(),
            "Connected without an IP".into(),
        );
    }
    if d.has_ip.is_none() {
        return e(
            Level::Na,
            "已连接，IP 未知".into(),
            "Connected; IP unknown".into(),
        );
    }
    e(Level::Ok, "已连接".into(), "Connected".into())
}

/// A bounded observation, not a claim that every destination/service works.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultState {
    Passed,
    Failed,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PathEvidence {
    pub interface: String,
    pub family: &'static str,
    /// Provenance only; do not publish the subscriber's source address.
    pub source: &'static str,
    /// Binding constrains the socket; transparent redirection is not disproved.
    pub route: &'static str,
    pub path_reachable: ResultState,
    pub dns: ResultState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NetworkEvidence {
    pub paths: Vec<PathEvidence>,
    pub path_reachable: ResultState,
    pub dns: ResultState,
    pub tls: ResultState,
    pub internet_usable: ResultState,
    pub reason: String,
    pub observed_at: Option<crate::clock::BootTime>,
}

/// Only scoped path/DNS evidence. No HTTP status or resolver reply proves TLS/internet.
pub fn judge_network(n: NetworkEvidence, at: u64) -> (Evidence, Evidence) {
    use ResultState::*;
    let (level, zh, en) = match n.path_reachable {
        Passed => (
            Level::Ok,
            "蜂窝 HTTP 路径有回应，未验证 TLS/上网",
            "Cellular HTTP path replied; TLS/internet unverified",
        ),
        Failed => (
            Level::Bad,
            "检测目标未回应，尚不能确认整体断网",
            "Probe targets did not reply; overall outage unconfirmed",
        ),
        Unknown => (Level::Na, "蜂窝路径未知", "Cellular path unknown"),
    };
    let mut path = Evidence::new(LayerId::Internet, level, zh, en, at);
    let (level, zh, en) = match n.dns {
        Passed => (
            Level::Ok,
            "查询名有地址回答",
            "Query name resolved to an address",
        ),
        Failed => (
            Level::Bad,
            "DNS 查询未成功，尚不能确认所有解析失败",
            "DNS query failed; general resolver failure unconfirmed",
        ),
        Unknown => (Level::Na, "DNS 未验证", "DNS unverified"),
    };
    let dns = Evidence::new(LayerId::Dns, level, zh, en, at);
    path.network = Some(n);
    (path, dns)
}

/// One probe's result; None = not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Ok { ms: u32 },
    Failed,
}

pub fn judge_internet(p: Option<Probe>, at: u64) -> Evidence {
    let e = |level, zh: String, en: String| Evidence::new(LayerId::Internet, level, zh, en, at);
    match p {
        None => e(Level::Na, "没有实测".into(), "Not probed".into()),
        Some(Probe::Ok { ms }) => e(
            Level::Ok,
            format!("通，{ms} ms"),
            format!("Reachable, {ms} ms"),
        ),
        Some(Probe::Failed) => e(Level::Bad, "外网不通".into(), "Internet unreachable".into()),
    }
}

pub fn judge_dns(p: Option<Probe>, at: u64) -> Evidence {
    let e = |level, zh: String, en: String| Evidence::new(LayerId::Dns, level, zh, en, at);
    match p {
        None => e(Level::Na, "没有实测".into(), "Not probed".into()),
        Some(Probe::Ok { ms }) => e(
            Level::Ok,
            format!("能解析，{ms} ms"),
            format!("Resolving, {ms} ms"),
        ),
        Some(Probe::Failed) => e(
            Level::Bad,
            "域名解析不了".into(),
            "DNS not resolving".into(),
        ),
    }
}

// ---- the verdict ----------------------------------------------------------------------

pub struct Check {
    pub premises: Premises,
    /// The passive reading in the collection round says down or can't tell.
    pub passive_down: bool,
    /// Screen on, a client connected, or the user asked (D8).
    pub can_probe: bool,
    pub iccid: String,
    /// The four layers' evidence, any order; missing layers count as na.
    pub layers: Vec<Evidence>,
    pub now: u64,
}

const ORDER: [LayerId; 4] = [
    LayerId::Signal,
    LayerId::Data,
    LayerId::Internet,
    LayerId::Dns,
];

fn symptom(layer: LayerId) -> (&'static str, &'static str) {
    match layer {
        LayerId::Signal => ("没注册上网络", "Not registered"),
        LayerId::Data => (
            "已注册，但数据连接没建立",
            "Registered, but the data connection is down",
        ),
        LayerId::Internet => (
            "已连上，但外网不通",
            "Connected, but the internet is unreachable",
        ),
        LayerId::Dns => (
            "能上外网，但域名解析不了",
            "Internet reachable, but DNS is not resolving",
        ),
    }
}

pub fn judge(c: &Check) -> Verdict {
    match expected_outage(&c.premises) {
        Ok(Some(_)) => return Verdict::None { why: "expected" },
        Err(Unreadable) => return Verdict::Unverified { why: "premises" },
        Ok(None) => {}
    }
    if !c.passive_down {
        return Verdict::None { why: "up" };
    }
    if !c.can_probe {
        return Verdict::Unverified { why: "not_probed" };
    }
    let ev: Vec<Evidence> = ORDER
        .iter()
        .map(|id| {
            c.layers
                .iter()
                .find(|e| e.layer == *id)
                .cloned()
                .unwrap_or_else(|| Evidence::new(*id, Level::Na, "没有读数", "No reading", c.now))
        })
        .collect();
    if ev[0].level != Level::Ok && ev[0].level != Level::Warn {
        // Not registered (bad) is another story; unknown (na) can't tell this one.
        return if ev[0].level == Level::Bad {
            Verdict::None {
                why: "not_registered",
            }
        } else {
            Verdict::Unverified {
                why: "no_registration",
            }
        };
    }
    let Some(i) = ev.iter().position(|e| e.level == Level::Bad) else {
        return if ev.iter().all(|e| e.level == Level::Ok) {
            Verdict::None {
                why: if ev.iter().any(|e| {
                    e.network
                        .as_ref()
                        .is_some_and(|n| n.internet_usable != ResultState::Passed)
                }) {
                    "path_dns_ok"
                } else {
                    "all_ok"
                },
            }
        } else {
            Verdict::Unverified {
                why: "inconclusive",
            }
        };
    };
    let clean = ev[..i].iter().all(|e| e.level == Level::Ok);
    let scoped_network = i >= 2
        && ev.iter().any(|e| {
            e.network
                .as_ref()
                .is_some_and(|n| n.internet_usable != ResultState::Passed)
        });
    let confidence = if clean && !scoped_network && ev[..=i].iter().all(|e| e.fresh(c.now)) {
        Confidence::Sure
    } else {
        Confidence::Likely
    };
    let (zh, en) = if scoped_network {
        if ev[i].layer == LayerId::Dns {
            (
                "DNS 查询未成功，整体上网能力尚未确认",
                "DNS query failed; overall internet usability unconfirmed",
            )
        } else {
            (
                "检测目标未回应，尚不能确认整体断网",
                "Probe targets did not reply; overall outage unconfirmed",
            )
        }
    } else {
        symptom(ev[i].layer)
    };
    Verdict::Open(Finding {
        kind: KIND_NO_INTERNET,
        iccid: c.iccid.clone(),
        layer: ev[i].layer,
        confidence,
        symptom: zh.into(),
        symptom_en: en.into(),
        evidence: ev,
        at: c.now,
    })
}

#[cfg(test)]
mod tests;
