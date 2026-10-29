//! /device-info (设备信息): identity, network addresses and
//! system facts in three groups, read-only, and a fourth: the component versions and whether
//! the public repo has newer ones (u60d `/v2/versions`, docs/designs/component-versions.md). The old page read nine agent endpoints once; here
//! almost everything is a u60d block on the one upstream connection (and so live), checked field
//! by field against the agent's sources (u60-web.md 「2-1 做法」):
//!
//! | row | old (agent) | now |
//! |---|---|---|
//! | IMEI | `/api/sim/imei` (`get_imei`) | `live.system.imei` (same `get_imei`) |
//! | IMSI, ICCID, SIM state | `/api/sim/info` (`get_sim_info`) | `sim.imsi`, `sim.iccid`, `sim.state` (= `sim_states`) |
//! | Phone number | `/api/cell/extra` (core `sim.msisdn`) | `sim.msisdn` |
//! | Operator, network type | `/api/network/signal` (`nwinfo_get_netinfo`) | `signal.operator` (full name; the short name only via `uci_device_info.radio_operator`), `signal.type` |
//! | WAN IPv4, DNS / IPv6 / LAN IP | `/api/network/wan` / `wan6` / `lan-status` (netifd) | `interfaces.wan4` / `wan6` / `lan` (same netifd status) |
//! | Gateway | `wan.route[0].nexthop` (netifd) | `interfaces.cellular.ipv4_gateway` (u60d drops netifd's routes; the modem's gateway, in the WAN subnet in the recording; not compared with netifd's nexthop) |
//! | Hostname, uptime | `/api/device`, `/api/device/system` | `live.system.hostname`, `live.system.uptime` |
//! | Memory total / free / available | `/api/device/system` (`system info`, bytes) | `live.runtime.memory_kb` (/proc/meminfo, ×1024 = the same) |
//! | Kernel | `/api/device` (`/proc/version`) | **agent** `/api/device`, once per stream (u60d has no kernel line) |

use std::time::Duration;

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::agent::{Caller, Fail, Polled};
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, StreamQuery, split_leaves};

const PATH: &str = "/device-info";
/// Blocks this page shows (a change re-renders it).
const BLOCKS: [&str; 5] = ["sim", "signal", "live", "interfaces", "uci_device_info"];

pub enum Val {
    /// Nothing read yet (skeleton).
    Loading,
    Empty,
    Text(String),
}

pub struct Row {
    pub id: &'static str,
    pub label: String,
    pub val: Val,
    pub mono: bool,
    /// The value is the device's own text (names, numbers): not translated.
    pub data: bool,
    pub sub: Option<String>,
    pub stale: bool,
}

pub struct Group {
    pub key: &'static str,
    pub title: &'static str,
    pub rows: Vec<Row>,
    /// "Couldn't read: …" (names of what failed), with a retry.
    pub failed: Vec<String>,
}

pub struct View {
    pub l: Lang,
    pub groups: [Group; 4],
}

/// The kernel line from the agent: `None` = not asked yet.
pub type Kernel = Option<Result<String, Fail>>;

/// u60d's `/v2/versions` as last read (None: not yet; Err: the read failed). Shared by every
/// open tab, so 「检查更新」 in one shows in all.
static VERSIONS: std::sync::Mutex<Option<Result<Value, String>>> = std::sync::Mutex::new(None);

fn versions_now() -> Option<Result<Value, String>> {
    VERSIONS.lock().unwrap().clone()
}

async fn read_versions(app: &Shared, who: &Caller) {
    let r = crate::agent::get(app, who, "/versions")
        .await
        .map_err(|e| e.text(Lang::Zh));
    *VERSIONS.lock().unwrap() = Some(r);
}

fn comp_name(c: &str, l: Lang) -> &'static str {
    match c {
        "core" => l.pick("Core", "核心"),
        "lweb" => l.pick("Admin web", "管理网页"),
        "sentinel" => l.pick("Sentinel", "哨兵"),
        "touch" => l.pick("Touch screen", "触屏界面"),
        "scripts" => l.pick("Device scripts", "设备脚本"),
        _ => "?",
    }
}

/// "3 h ago" from Unix seconds (u60d and this server share the device clock).
fn ago(t: i64, l: Lang) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let d = (now - t).max(0);
    match (l, d) {
        (Lang::En, 0..120) => "just now".into(),
        (Lang::Zh, 0..120) => "刚刚".into(),
        (Lang::En, 120..7200) => format!("{} min ago", d / 60),
        (Lang::Zh, 120..7200) => format!("{} 分钟前", d / 60),
        (Lang::En, 7200..172_800) => format!("{} h ago", d / 3600),
        (Lang::Zh, 7200..172_800) => format!("{} 小时前", d / 3600),
        (Lang::En, _) => format!("{} d ago", d / 86400),
        (Lang::Zh, _) => format!("{} 天前", d / 86400),
    }
}

/// The versions group: one row per component (installed; the newer one under it), then when
/// the public repo was last looked at.
fn version_group(vers: &Option<Result<Value, String>>, l: Lang) -> Group {
    let mut rows = Vec::new();
    let mut failed = Vec::new();
    match vers {
        Some(Ok(v)) => {
            for c in v["components"].as_array().into_iter().flatten() {
                let name = c["name"].as_str().unwrap_or("");
                let cur = c["installed"].as_str();
                let new = c["latest"].as_str();
                let sub = if c["update"] == true {
                    new.map(|n| match l {
                        Lang::En => format!("Update available: {n}"),
                        Lang::Zh => format!("有新版：{n}"),
                    })
                } else if new.is_some() && new == cur {
                    Some(l.pick("Up to date", "已是最新").into())
                } else {
                    None
                };
                rows.push(Row {
                    id: match name {
                        "core" => "di-ver-core",
                        "lweb" => "di-ver-lweb",
                        "sentinel" => "di-ver-sentinel",
                        "touch" => "di-ver-touch",
                        "scripts" => "di-ver-scripts",
                        _ => "di-ver-other",
                    },
                    label: comp_name(name, l).into(),
                    val: Val::Text(cur.map_or_else(|| l.pick("Unnumbered", "未编号").into(), str::to_owned)),
                    mono: cur.is_some(),
                    data: false,
                    sub,
                    stale: false,
                });
            }
            let checked = v["checked_at"].as_i64().map(|t| {
                let when = ago(t, l);
                match v["error"].as_str() {
                    Some(e) => match l {
                        Lang::En => format!("{when}, failed: {e}"),
                        Lang::Zh => format!("{when}，失败：{e}"),
                    },
                    None => when,
                }
            });
            rows.push(Row {
                id: "di-ver-checked",
                label: l.pick("Last checked", "上次检查").into(),
                val: Val::Text(checked.unwrap_or_else(|| l.pick("Not yet", "还没查过").into())),
                mono: false,
                data: false,
                sub: None,
                stale: false,
            });
        }
        Some(Err(e)) => failed.push(match l {
            Lang::En => format!("versions ({e})"),
            Lang::Zh => format!("版本（{e}）"),
        }),
        None => rows.push(Row {
            id: "di-ver-checked",
            label: l.pick("Last checked", "上次检查").into(),
            val: Val::Loading,
            mono: false,
            data: false,
            sub: None,
            stale: false,
        }),
    }
    // people sell devices with Luya-Z installed: say plainly that it is free
    rows.push(Row {
        id: "di-ver-free",
        label: "Luya-Z".into(),
        val: Val::Text(l.pick("Free", "免费").into()),
        mono: false,
        data: false,
        sub: Some(
            l.pick(
                "If you paid for a device with it installed, the price and support are the seller's; Luya-Z has nothing to do with the sale.",
                "如果你付钱买了装好的设备，费用和售后归卖家，与 Luya-Z 无关。",
            )
            .into(),
        ),
        stale: false,
    });
    Group {
        key: "versions",
        title: l.pick("Component versions", "组件版本"),
        rows,
        failed,
    }
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v[k].as_str().map(str::trim).filter(|x| !x.is_empty())
}

fn first_addr(iface: &Value, fam: &str) -> Option<String> {
    iface[fam].as_array()?.first()?["address"]
        .as_str()
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
}

/// The firmware's SIM state words (`get_sim_info.sim_states`), like the old page; unknown ones
/// as they are.
pub fn sim_word(v: &str, l: Lang) -> (String, bool) {
    let x = v.to_lowercase();
    let w = if x.contains("waitpuk") {
        l.pick("PUK needed", "需要 PUK")
    } else if x.contains("waitpin") {
        l.pick("PIN needed", "需要 PIN")
    } else if x.contains("absent") || x.contains("undetected") {
        l.pick("No SIM", "没有 SIM 卡")
    } else if x.contains("init_complete") || x.contains("ready") {
        l.pick("Ready", "就绪")
    } else {
        return (v.to_owned(), true);
    };
    (w.to_owned(), false)
}

pub fn uptime(secs: i64, l: Lang) -> Option<String> {
    if secs <= 0 {
        return None;
    }
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    Some(match (l, d > 0, h > 0) {
        (Lang::En, true, _) => format!("{d} d {h} h"),
        (Lang::Zh, true, _) => format!("{d} 天 {h} 小时"),
        (Lang::En, false, true) => format!("{h} h {m} min"),
        (Lang::Zh, false, true) => format!("{h} 小时 {m} 分钟"),
        (Lang::En, false, false) => format!("{m} min"),
        (Lang::Zh, false, false) => format!("{m} 分钟"),
    })
}

/// kB → "1590 MB" (the old page: bytes / 1024 / 1024, rounded).
pub fn mb(kb: Option<f64>) -> Option<String> {
    kb.filter(|k| *k > 0.0)
        .map(|k| format!("{} MB", (k / 1024.0).round()))
}

/// "Linux version 5.4.210 (…) …" → "5.4.210".
pub fn kernel_release(v: &str) -> String {
    v.strip_prefix("Linux version ")
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or(v)
        .to_owned()
}

/// One block as the page sees it.
struct B<'a> {
    data: Option<&'a Value>,
    /// Greyed: u60d says stale, or the values are from a connection that is not up now.
    stale: bool,
    /// Listed under "Couldn't read": stale, missing while connected, or u60d is down.
    bad: bool,
    /// Not there yet and the link is still coming up: skeletons, not "—".
    waiting: bool,
}

fn block<'a>(st: &'a St, name: &str) -> B<'a> {
    let b = st.blocks.get(name);
    let stale = b.is_some_and(|b| b.stale);
    B {
        data: b.map(|b| &b.data),
        stale: stale || (b.is_some() && st.link != Link::Live),
        bad: stale || st.link == Link::Down || (b.is_none() && st.link == Link::Live),
        waiting: b.is_none() && !matches!(st.link, Link::Live | Link::Down),
    }
}

fn val(b: &B, v: Option<String>) -> Val {
    match v {
        Some(t) => Val::Text(t),
        None if b.waiting => Val::Loading,
        None => Val::Empty,
    }
}

fn row(id: &'static str, label: &str, b: &B, v: Option<String>, mono: bool) -> Row {
    Row {
        id,
        label: label.to_owned(),
        val: val(b, v),
        mono,
        data: true,
        sub: None,
        stale: b.stale,
    }
}

fn unread(b: &B) -> bool {
    b.bad
}

pub fn view(st: &St, kernel: &Kernel, l: Lang) -> View {
    view_with(st, kernel, &versions_now(), l)
}

pub fn view_with(
    st: &St,
    kernel: &Kernel,
    vers: &Option<Result<Value, String>>,
    l: Lang,
) -> View {
    let sim = block(st, "sim");
    let sig = block(st, "signal");
    let live = block(st, "live");
    let ifs = block(st, "interfaces");
    let uci = block(st, "uci_device_info");
    let null = Value::Null;
    let simd = sim.data.unwrap_or(&null);
    let sigd = sig.data.unwrap_or(&null);
    let sys = live.data.map_or(&null, |d| &d["system"]);
    let mem = live.data.map_or(&null, |d| &d["runtime"]["memory_kb"]);
    let ucid = uci.data.unwrap_or(&null);
    let ifd = ifs.data.unwrap_or(&null);

    // identity
    let imei = s(sys, "imei").or(s(ucid, "imei")).map(str::to_owned);
    let mut state = row(
        "di-simstate",
        l.pick("SIM State", "SIM 状态"),
        &sim,
        None,
        false,
    );
    if let Some(w) = s(simd, "state") {
        let (word, raw) = sim_word(w, l);
        state.val = Val::Text(word);
        state.data = raw;
    } else {
        state.val = val(&sim, None);
    }
    let mut ident = vec![
        row("di-imei", "IMEI", &live, imei, true),
        row(
            "di-imsi",
            "IMSI",
            &sim,
            s(simd, "imsi").map(str::to_owned),
            true,
        ),
        row(
            "di-iccid",
            "ICCID",
            &sim,
            s(simd, "iccid").map(str::to_owned),
            true,
        ),
    ];
    // the old page hid this row on agents without /api/cell/extra; the block always has the field
    let n = s(simd, "msisdn");
    ident.push(Row {
        id: "di-msisdn",
        label: l.pick("Phone number", "本机号码").into(),
        val: match (sim.data, n) {
            (_, Some(n)) => Val::Text(n.to_owned()),
            // dual SIM with data on slot 2: the firmware reports no number for that card
            (Some(_), None) if simd["current_slot"] == 2 => {
                Val::Text(l.pick("SIM 2 reports no number", "卡 2 不提供号码").into())
            }
            (Some(_), None) => Val::Text(l.pick("Not on the SIM", "卡里没存").into()),
            (None, None) => val(&sim, None),
        },
        mono: n.is_some(),
        data: n.is_some(),
        sub: None,
        stale: sim.stale,
    });
    ident.push(state);
    ident.push(row(
        "di-operator",
        l.pick("Operator", "运营商"),
        &sig,
        s(sigd, "operator")
            .or(s(ucid, "radio_operator"))
            .map(str::to_owned),
        false,
    ));
    ident.push(row(
        "di-nettype",
        l.pick("Network Type", "网络制式"),
        &sig,
        s(sigd, "type")
            .or(s(ucid, "radio_network_type"))
            .map(str::to_owned),
        false,
    ));
    let mut ident_failed = Vec::new();
    if unread(&live) && unread(&uci) {
        ident_failed.push("IMEI".into());
    }
    if unread(&sim) {
        ident_failed.push(l.pick("SIM", "SIM 卡").into());
    }
    if unread(&sig) {
        ident_failed.push(l.pick("Operator", "运营商").into());
    }

    // network
    let wan4 = &ifd["wan4"];
    let dns: Vec<&str> = wan4["dns"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let net = vec![
        row(
            "di-wan4",
            "IPv4 (WAN)",
            &ifs,
            first_addr(wan4, "ipv4"),
            true,
        ),
        row(
            "di-gw",
            l.pick("Gateway", "网关"),
            &ifs,
            s(&ifd["cellular"], "ipv4_gateway").map(str::to_owned),
            true,
        ),
        row(
            "di-dns",
            "DNS",
            &ifs,
            (!dns.is_empty()).then(|| dns.join(", ")),
            true,
        ),
        row(
            "di-wan6",
            "IPv6 (WAN)",
            &ifs,
            first_addr(&ifd["wan6"], "ipv6"),
            true,
        ),
        row(
            "di-lan",
            l.pick("LAN IP", "LAN IP"),
            &ifs,
            first_addr(&ifd["lan"], "ipv4"),
            true,
        ),
    ];
    let net_failed = if unread(&ifs) {
        vec![l.pick("Network interfaces", "网络接口").into()]
    } else {
        Vec::new()
    };

    // system
    let kernel_row = match kernel {
        None => Row {
            id: "di-kernel",
            label: l.pick("Kernel", "内核").into(),
            val: Val::Loading,
            mono: true,
            data: true,
            sub: None,
            stale: false,
        },
        Some(Ok(k)) => Row {
            id: "di-kernel",
            label: l.pick("Kernel", "内核").into(),
            val: Val::Text(kernel_release(k)),
            mono: true,
            data: true,
            sub: Some(k.clone()),
            stale: false,
        },
        Some(Err(_)) => Row {
            id: "di-kernel",
            label: l.pick("Kernel", "内核").into(),
            val: Val::Empty,
            mono: true,
            data: true,
            sub: None,
            stale: false,
        },
    };
    let mut up = row(
        "di-uptime",
        l.pick("Uptime", "运行时间"),
        &live,
        sys["uptime"].as_i64().and_then(|u| uptime(u, l)),
        false,
    );
    up.data = false;
    let mut sysrows = vec![
        row(
            "di-host",
            l.pick("Hostname", "主机名"),
            &live,
            s(sys, "hostname")
                .or(s(ucid, "hostname"))
                .map(str::to_owned),
            true,
        ),
        kernel_row,
        up,
    ];
    for (id, en, zh, k) in [
        ("di-memtotal", "Memory Total", "内存总量", "total"),
        ("di-memfree", "Memory Free", "内存空闲", "free"),
        ("di-memavail", "Memory Available", "内存可用", "available"),
    ] {
        let mut r = row(id, l.pick(en, zh), &live, mb(mem[k].as_f64()), false);
        r.data = false;
        sysrows.push(r);
    }
    let mut sys_failed = Vec::new();
    if unread(&live) {
        sys_failed.push(l.pick("system info", "系统信息").into());
    }
    if let Some(Err(e)) = kernel {
        sys_failed.push(match l {
            Lang::En => format!("kernel ({})", e.text(l)),
            Lang::Zh => format!("内核（{}）", e.text(l)),
        });
    }

    View {
        l,
        groups: [
            Group {
                key: "identity",
                title: l.pick("Identity", "身份标识"),
                rows: ident,
                failed: ident_failed,
            },
            Group {
                key: "network",
                title: l.pick("Network", "网络"),
                rows: net,
                failed: net_failed,
            },
            Group {
                key: "system",
                title: l.pick("System", "系统"),
                rows: sysrows,
                failed: sys_failed,
            },
            version_group(vers, l),
        ],
    }
}

// ---- templates and the stream ----

#[derive(Template)]
#[template(path = "device_info_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

pub struct Poll {
    who: Caller,
    l: Lang,
    kernel: Kernel,
}

impl Poll {
    async fn fetch(&mut self, app: &Shared) {
        read_versions(app, &self.who).await;
        self.kernel = Some(
            crate::agent::get(app, &self.who, "/api/device")
                .await
                .and_then(|v| {
                    v["kernel"].as_str().map(str::to_owned).ok_or(Fail::Refused(
                        200,
                        String::new(),
                        String::new(),
                    ))
                }),
        );
    }

    fn leaves(&self, app: &Shared) -> Vec<(String, String)> {
        app.hub.read(|st| {
            let v = view(st, &self.kernel, self.l);
            split_leaves(&Leaves { v: &v }.render().unwrap())
        })
    }
}

impl Polled for Poll {
    /// The kernel line is read once per stream (the old page read everything once) and again
    /// on Retry; everything else follows the blocks.
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        if !matches!(self.kernel, Some(Ok(_))) {
            self.fetch(app).await;
        } else {
            read_versions(app, &self.who).await;
        }
        // the versions: u60d looks once a day; read again hourly
        (self.leaves(app), Duration::from_secs(3600))
    }
    fn watches(&self, block: &str) -> bool {
        BLOCKS.contains(&block)
    }
    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(self.leaves(app))
    }
}

#[derive(Template)]
#[template(path = "device_info.html")]
struct Page {
    shell: crate::shell::Shell,
    v: View,
    leaves: Vec<(String, String)>,
}

impl Page {
    fn leaf(&self, id: &str) -> &str {
        self.leaves
            .iter()
            .find(|(k, _)| k == id)
            .map_or("", |(_, h)| h.as_str())
    }
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let mut p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
        kernel: None,
    };
    p.fetch(&app).await;
    let shell = app.hub.read(|st| {
        crate::shell::Shell::new(&headers, PATH).live(
            st,
            &ctx,
            Some(format!("{PATH}/events?g={}", st.generation)),
        )
    });
    let (v, leaves) = app.hub.read(|st| {
        let v = view(st, &p.kernel, ctx.lang);
        let leaves = split_leaves(&Leaves { v: &v }.render().unwrap());
        (v, leaves)
    });
    let html = Page { shell, v, leaves }.render().unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

pub async fn events(
    State(app): State<Shared>,
    Query(q): Query<StreamQuery>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll {
        who: Caller::of(&app, &req),
        l: ctx.lang,
        kernel: None,
    };
    crate::agent::stream(app, ctx, q.g, PATH, p)
}

/// 「检查更新」: u60d asks the public repo now (up to 20 s), then every open tab re-renders.
pub async fn check_versions(
    State(app): State<Shared>,
    req: axum::extract::Request,
) -> axum::http::StatusCode {
    let who = Caller::of(&app, &req);
    let r = crate::agent::post(&app, &who, "/versions/check", &Value::Null)
        .await
        .map_err(|e| e.text(Lang::Zh));
    *VERSIONS.lock().unwrap() = Some(r);
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

/// Retry: reconnect to u60d now if it is down, and read the kernel line again.
pub async fn refresh(State(app): State<Shared>) -> axum::http::StatusCode {
    app.hub.retry();
    let _ = app.kick.send(PATH);
    axum::http::StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::Hub;
    use crate::sse::Frame;
    use serde_json::json;

    /// Synthetic values only (documentation ranges, made-up identifiers).
    fn hub() -> std::sync::Arc<Hub> {
        let hub = Hub::new();
        let b = |d: Value| json!({"revision":1,"observed_at":1,"stale":false,"data":d});
        hub.apply(&Frame {
            event: "snapshot".into(),
            data: json!({"epoch":"e","blocks":{
                "sim": b(json!({"state":"sim ready","imsi":"001010123456789","iccid":"8988210000000000001","msisdn":" ","spn":""})),
                "signal": b(json!({"operator":"中国电信","type":"SA"})),
                "live": b(json!({"system":{"imei":"350000000000001","hostname":"MU5250","uptime":93_780},
                    "runtime":{"memory_kb":{"total":1_628_400,"free":569_064,"available":1_017_172}}})),
                "interfaces": b(json!({
                    "wan4":{"ipv4":[{"address":"192.0.2.10","mask":29}],"dns":["198.51.100.1","198.51.100.2"]},
                    "wan6":{"ipv6":[{"address":"2001:db8::10","mask":64}]},
                    "lan":{"ipv4":[{"address":"10.0.0.1","mask":24}]},
                    "cellular":{"ipv4_gateway":"192.0.2.9"}})),
            }})
            .to_string(),
        });
        hub.set_link_for_test(Link::Live);
        hub
    }

    fn text<'a>(v: &'a View, id: &str) -> &'a str {
        let r = v
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("no row {id}"));
        match &r.val {
            Val::Text(t) => t,
            Val::Empty => "—",
            Val::Loading => "…",
        }
    }

    #[test]
    fn every_row_maps_from_its_block() {
        let k: Kernel = Some(Ok(
            "Linux version 5.15.153 (builder@host) (gcc 12) #1 SMP PREEMPT".into(),
        ));
        let v = hub().read(|st| view(st, &k, Lang::Zh));
        assert_eq!(text(&v, "di-imei"), "350000000000001");
        assert_eq!(text(&v, "di-imsi"), "001010123456789");
        assert_eq!(text(&v, "di-iccid"), "8988210000000000001");
        assert_eq!(text(&v, "di-msisdn"), "卡里没存", "blank msisdn");
        assert_eq!(text(&v, "di-simstate"), "就绪");
        assert_eq!(text(&v, "di-operator"), "中国电信");
        assert_eq!(text(&v, "di-nettype"), "SA");
        assert_eq!(text(&v, "di-wan4"), "192.0.2.10");
        assert_eq!(text(&v, "di-gw"), "192.0.2.9");
        assert_eq!(text(&v, "di-dns"), "198.51.100.1, 198.51.100.2");
        assert_eq!(text(&v, "di-wan6"), "2001:db8::10");
        assert_eq!(text(&v, "di-lan"), "10.0.0.1");
        assert_eq!(text(&v, "di-host"), "MU5250");
        assert_eq!(text(&v, "di-kernel"), "5.15.153");
        assert_eq!(text(&v, "di-uptime"), "1 天 2 小时");
        assert_eq!(text(&v, "di-memtotal"), "1590 MB");
        assert_eq!(text(&v, "di-memfree"), "556 MB");
        assert_eq!(text(&v, "di-memavail"), "993 MB");
        assert!(v.groups.iter().all(|g| g.failed.is_empty()));
    }

    #[test]
    fn words_like_the_old_page() {
        assert_eq!(sim_word("sim ready", Lang::En).0, "Ready");
        assert_eq!(sim_word("modem_waitpin", Lang::Zh).0, "需要 PIN");
        assert_eq!(sim_word("modem_sim_undetected", Lang::En).0, "No SIM");
        assert_eq!(
            sim_word("something_new", Lang::En),
            ("something_new".into(), true)
        );
        assert_eq!(
            uptime(3 * 3600 + 120, Lang::En).as_deref(),
            Some("3 h 2 min")
        );
        assert_eq!(uptime(59 * 60, Lang::Zh).as_deref(), Some("59 分钟"));
        assert_eq!(uptime(0, Lang::Zh), None);
        assert_eq!(kernel_release("custom"), "custom");
    }

    #[test]
    fn missing_or_stale_blocks_say_what_could_not_be_read() {
        let hub = Hub::new();
        hub.apply(&Frame {
            event: "snapshot".into(),
            data: json!({"epoch":"e","blocks":{
                "interfaces":{"revision":1,"observed_at":1,"stale":true,"data":{"wan4":{"ipv4":[{"address":"192.0.2.10","mask":29}]}}}}})
            .to_string(),
        });
        hub.set_link_for_test(Link::Live);
        let k: Kernel = Some(Err(Fail::Unreachable));
        let v = hub.read(|st| view(st, &k, Lang::En));
        assert_eq!(v.groups[0].failed, vec!["IMEI", "SIM", "Operator"]);
        assert_eq!(v.groups[1].failed, vec!["Network interfaces"]);
        assert!(v.groups[1].rows[0].stale, "stale value greyed, kept");
        assert_eq!(text(&v, "di-wan4"), "192.0.2.10");
        assert_eq!(text(&v, "di-imsi"), "—");
        assert_eq!(
            text(&v, "di-msisdn"),
            "—",
            "no sim block: unknown, not 'not on the SIM'"
        );
        assert_eq!(
            v.groups[2].failed,
            vec!["system info", "kernel (The device service did not answer)"]
        );
        // before the link is up: skeletons, nothing listed as failed
        let fresh = Hub::new();
        let v = fresh.read(|st| view(st, &None, Lang::En));
        assert!(matches!(v.groups[0].rows[0].val, Val::Loading));
        assert!(v.groups.iter().all(|g| g.failed.is_empty()));
    }

    #[test]
    fn the_english_page_has_no_chinese_outside_device_text() {
        let k: Kernel = Some(Ok("Linux version 5.15.153 #1".into()));
        let v = hub().read(|st| view(st, &k, Lang::En));
        let html = Leaves { v: &v }.render().unwrap();
        // drop the device's own text (operator name here), as the e2e does
        let mut rest = String::new();
        for part in html.split("data-data>") {
            rest.push_str(part.split_once('<').map_or(part, |(_, r)| r));
        }
        assert!(
            !rest.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
                || ('\u{3000}'..='\u{303f}').contains(&c)
                || ('\u{ff00}'..='\u{ffef}').contains(&c)),
            "{rest}"
        );
        assert!(html.contains("Not on the SIM"));
    }
}
