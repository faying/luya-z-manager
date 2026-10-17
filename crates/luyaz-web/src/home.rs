//! / (home), the old `(panel)/page.tsx` + `useBatteryEstimate.ts` (batch 5b).
//!
//! ```text
//!  GET /  ─▶ frame: labels, skeletons, the signal status (rows come with the first push)
//!  GET /home/events?g ─▶ agent::stream(Poll): the page's leaves by id, only the changed ones
//!    from the hub (one SSE): signal, live (speeds, temperature, uptime, today/month), battery,
//!        clients, wifi, uci_device_info (title), sms, side (alerts, scenario, verdict, CHILL)
//!    polled with the tab's session (agent.rs, u60d first): /api/screen 10 s (battery estimate),
//!        /api/services/tailscale 15 s, /api/netinfo?lite=1 30 s, /api/wifi/status 30 s
//!        (only the actual channel and width are kept; the reply carries both passwords),
//!        CHILL: home/chill.rs (private)
//! ```
//! Not polled here: `/api/public/status`. luyaz-web's own side poller (topbar.rs) already reads
//! u60d's every 10 s into `hub.side.public` (scenario, verdict, CHILL notice, alerts). u60d's has
//! no device name, no operator and no SSID, so those come from the blocks instead
//! (`uci_device_info`, `signal.operator`, `wifi`).
//!
//! Hub values are shown only once this stream has seen the link Live (`Poll::live_seen`): the hub
//! keeps the last connection's blocks over an idle drop, and a first visit must not paint them as
//! current (opbar.rs `Seen`).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use askama::Template;
use axum::Router;
use axum::extract::{Query, Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::Value;

use crate::agent::{self, Caller, Polled};
use crate::hub::{Link, St};
use crate::i18n::Lang;
use crate::pages::{Shared, split_leaves};
use crate::view::{self, Readout, js_num, num};

mod chill;

const PATH: &str = "/";

pub fn routes() -> Router<Shared> {
    let r = Router::new()
        .route(PATH, get(page))
        .route("/home/events", get(events));
    let r = chill::routes(r);
    r
}

#[derive(Template)]
#[template(path = "home.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    status: String,
    chill: String,
}

async fn page(State(app): State<Shared>, req: Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let (shell, status) = app.hub.read(|st| {
        (
            crate::shell::Shell::new(&headers, PATH).live(
                st,
                &ctx,
                Some(format!("/home/events?g={}", st.generation)),
            ),
            crate::pages::status_frag(&view::signal_view(st, Instant::now(), ctx.lang)),
        )
    });
    let html = Page {
        shell,
        l: ctx.lang,
        status,
        chill: chill::frame(ctx.lang),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

async fn events(
    State(app): State<Shared>,
    Query(q): Query<crate::pages::StreamQuery>,
    req: Request,
) -> impl IntoResponse {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let p = Poll::new(ctx.lang, Caller::of(&app, &req));
    agent::stream(app, ctx, q.g, PATH, p)
}

// ---- one polled source ----

/// An agent read that repeats every `every`: its last good value, and whether it has stopped.
pub(crate) struct Src<T> {
    pub(crate) val: Option<T>,
    every: Duration,
    next: Instant,
    ok_at: Option<Instant>,
    fails: u32,
}

impl<T> Src<T> {
    pub(crate) fn new(secs: u64) -> Self {
        Src {
            val: None,
            every: Duration::from_secs(secs),
            next: Instant::now(),
            ok_at: None,
            fails: 0,
        }
    }
    pub(crate) fn due(&self, now: Instant) -> bool {
        now >= self.next
    }
    pub(crate) fn next(&self) -> Instant {
        self.next
    }
    /// A read ended: `Some` = the new value. A failed read keeps the last value.
    pub(crate) fn set(&mut self, now: Instant, r: Option<T>) {
        self.next = now + self.every;
        match r {
            Some(v) => {
                self.val = Some(v);
                self.ok_at = Some(now);
                self.fails = 0;
            }
            None => self.fails += 1,
        }
    }
    /// Two failures in a row, or nothing good for three intervals (at least 10 s): the old
    /// freshness rule (lib/api/freshness.ts).
    pub(crate) fn stale(&self, now: Instant) -> bool {
        self.val.is_some()
            && (self.fails >= 2
                || self
                    .ok_at
                    .is_some_and(|t| now.duration_since(t) > (self.every * 3).max(Duration::from_secs(10))))
    }
    /// Asked and never answered.
    pub(crate) fn failed(&self) -> bool {
        self.val.is_none() && self.fails > 0
    }
}

/// `None` = not due; `Some(None)` = asked and failed.
pub(crate) async fn fetch(
    app: &Shared,
    who: &Caller,
    due: bool,
    path: &str,
) -> Option<Option<Value>> {
    if !due {
        return None;
    }
    Some(agent::get(app, who, path).await.ok())
}

/// Tailscale as the card shows it (the reply also lists every peer).
#[derive(Clone, Default)]
struct Ts {
    installed: bool,
    running: bool,
    /// Running but the daemon did not answer (services.rs "fake success").
    error: bool,
    online: Option<bool>,
    relay: Option<String>,
    peers_online: i64,
    capped: bool,
}

fn ts_of(v: &Value) -> Ts {
    let peers = v["peers"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    Ts {
        installed: v["installed"].as_bool().unwrap_or(false),
        running: v["running"].as_bool().unwrap_or(false),
        error: v["error"].as_str().is_some_and(|e| !e.is_empty()),
        online: v["self"]["online"].as_bool(),
        relay: v["self"]["relay"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        peers_online: v["peer_online"]
            .as_i64()
            .unwrap_or_else(|| peers.iter().filter(|p| p["online"].as_bool() == Some(true)).count() as i64),
        capped: peers.len() >= 32 && v["peer_count"].as_i64().unwrap_or(0) > 32,
    }
}

// ---- the view ----

pub(crate) fn mark(tone: &str, text: &str) -> String {
    let sym = match tone {
        "warn" => "▲",
        "bad" => "■",
        _ => "●",
    };
    format!(
        "<span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{sym}</span><span>{}</span></span>",
        agent::esc(text)
    )
}

enum Scene {
    Loading,
    None,
    On(ScOn),
}

struct ScOn {
    name: String,
    pinned: bool,
    engine_off: bool,
    last: Option<String>,
}

struct CaRow {
    label: String,
    line: String,
    ids: String,
}

struct View {
    l: Lang,
    title: Option<String>,
    sub: Option<String>,
    sub_loading: bool,
    sms_n: u64,
    sms_label: String,
    alerts_n: u64,
    alerts_label: String,
    status: String,
    why: bool,
    /// The core's open "sure" finding (T6), one sentence; links to its evidence.
    finding: Option<String>,
    ro_stale: bool,
    bw: Readout,
    rsrp: Readout,
    sinr: Readout,
    dl: Readout,
    ul: Readout,
    temp: Readout,
    bat: Readout,
    d_stale: bool,
    clients: String,
    day: String,
    month: String,
    uptime: String,
    id_stale: bool,
    ip_label: &'static str,
    ip: String,
    ip_sub: Option<String>,
    id_chill: String,
    sim: String,
    reg: String,
    reg_sub: Option<String>,
    /// The serving operator's mark (oplogo.rs), shown before the name; empty = unknown.
    reg_logo: String,
    sub_logo: String,
    roaming: Option<bool>,
    w_stale: bool,
    wifi_on: String,
    wifi_2: String,
    wifi_5: String,
    ca_full: bool,
    ca_stale: bool,
    ca_rows: Vec<CaRow>,
    ca_off: Option<String>,
    ca_sel: Option<String>,
    scene: Scene,
    ts: String,
    ts_stale: bool,
}

#[derive(Template)]
#[template(path = "home_leaves.html")]
struct Leaves<'a> {
    v: &'a View,
}

struct Poll {
    l: Lang,
    who: Caller,
    /// The link was Live at some read of this stream.
    live_seen: bool,
    screen: Src<Value>,
    ts: Src<Ts>,
    ni: Src<Value>,
    wifi: Src<crate::wifi_main::Live>,
    /// Carriers group shape last sent: (rows, "not scheduled" row, selection row).
    ca_shape: Option<(usize, bool, bool)>,
    /// Operator logo files (`--logo-dir`), copied from the app at each render.
    logo_dir: std::path::PathBuf,
    chill: chill::Chill,
}

impl Poll {
    fn new(l: Lang, who: Caller) -> Poll {
        Poll {
            l,
            who,
            live_seen: false,
            screen: Src::new(10),
            ts: Src::new(15),
            ni: Src::new(30),
            wifi: Src::new(30),
            ca_shape: None,
            logo_dir: std::path::PathBuf::new(),
            chill: chill::Chill::new(),
        }
    }

    fn earliest(&self) -> Instant {
        let e = self
            .screen
            .next()
            .min(self.ts.next())
            .min(self.ni.next())
            .min(self.wifi.next());
        let e = e.min(self.chill.next());
        e
    }

    fn leaves(&mut self, app: &Shared) -> Vec<(String, String)> {
        let now = Instant::now();
        if self.logo_dir != app.logo_dir {
            self.logo_dir = app.logo_dir.clone();
        }
        let v = app.hub.read(|st| self.view(st, now));
        #[allow(unused_mut)]
        let mut out = split_leaves(&Leaves { v: &v }.render().unwrap());
        out.extend(self.chill.leaves(app, self.l, now));
        out
    }

    fn view(&mut self, st: &St, now: Instant) -> View {
        let l = self.l;
        self.live_seen |= st.link == Link::Live;
        let seen = self.live_seen;
        let live = st.link == Link::Live;
        let data = |n: &str| st.blocks.get(n).map(|b| &b.data);
        let block_ok = |n: &str| st.blocks.get(n).is_some_and(|b| !b.stale) && live;
        // a hub value is shown only after this stream saw the link live
        let have = |n: &str| seen && data(n).is_some();
        let dash = || "—".to_owned();
        let ro = |loading: bool, v: Option<String>, word: String| Readout {
            value: if loading {
                None
            } else {
                Some(v.clone().unwrap_or_else(dash))
            },
            empty: v.is_none(),
            word: if loading { String::new() } else { word },
        };

        // ---- signal ----
        let sv = view::signal_view(st, now, l);
        let sig = data("signal").unwrap_or(&Value::Null);
        let cs = view::carriers(sig);
        let sig_loading = !seen || sv.rsrp.loading();
        let (nr, lte) = (
            cs.iter().filter(|c| c.nr).count(),
            cs.iter().filter(|c| !c.nr).count(),
        );
        let bw_sum = cs.iter().any(|c| c.bw.is_some()).then(|| {
            cs.iter().filter_map(|c| c.bw).sum::<f64>()
        });
        let bw = ro(sig_loading, bw_sum.map(js_num), carrier_summary(l, nr, lte));
        let rsrp = Readout {
            value: if sig_loading { None } else { sv.rsrp.value.clone() },
            empty: sv.rsrp.empty,
            word: if sig_loading {
                String::new()
            } else if !sv.rsrq.empty {
                format!("RSRQ {} · {}", sv.rsrq.text(), sv.rsrp.word)
            } else {
                sv.rsrp.word.clone()
            },
        };
        let sinr = Readout {
            value: if sig_loading { None } else { sv.sinr.value.clone() },
            empty: sv.sinr.empty,
            word: if sig_loading { String::new() } else { sv.sinr.word.clone() },
        };
        let sig_stale = seen && sv.stale;
        let verdict = st
            .side
            .public
            .as_ref()
            .and_then(|p| p["network"]["verdict"].as_str())
            .filter(|s| !s.is_empty());
        // T6: the core's open "sure" finding (registered but no internet)
        let finding = st
            .side
            .public
            .as_ref()
            .map(|p| &p["network"]["finding"])
            .filter(|f| seen && f.is_object())
            .map(|f| {
                let zh = f["symptom"].as_str().unwrap_or("");
                let en = f["symptom_en"].as_str().unwrap_or("");
                match l {
                    Lang::En if !en.is_empty() => en.to_string(),
                    _ => zh.to_string(),
                }
            })
            .filter(|s| !s.is_empty());
        let why = seen
            && !sv.stale
            && match verdict {
                Some(v) => matches!(
                    v,
                    "limit" | "weak" | "noise" | "crowd" | "narrow" | "nodata" | "stall"
                ),
                None => matches!(
                    sv.reason_ref(),
                    view::Reason::Weak { .. } | view::Reason::NoService
                ),
            };

        // ---- live: speeds, temperature, today / month, uptime ----
        let lv = data("live").unwrap_or(&Value::Null);
        let live_have = have("live");
        let mbps = |k: &str| agent::mbps(num(&lv["traffic"][k]));
        let dl = ro(!live_have, mbps("rx_speed"), String::from("↓"));
        let ul = ro(!live_have, mbps("tx_speed"), String::from("↑"));
        let temp_c = num(&lv["system"]["cpu_temp"])
            .filter(|t| *t >= 0.0)
            .map(|t| if t >= 1000.0 { t / 1000.0 } else { t });
        let temp = ro(
            !live_have,
            temp_c.map(|t| js_num(t.round())),
            temp_c.map(|t| temp_word(l, t)).unwrap_or_default().to_owned(),
        );
        let usage = |rx: &str, tx: &str| {
            let (r, t) = (
                agent::bytes(num(&lv["traffic"][rx])),
                agent::bytes(num(&lv["traffic"][tx])),
            );
            if !live_have || (r.is_none() && t.is_none()) {
                dash()
            } else {
                format!(
                    "↓ {} · ↑ {}",
                    r.unwrap_or_else(dash),
                    t.unwrap_or_else(dash)
                )
            }
        };
        let uptime = if live_have {
            lv["system"]["uptime"]
                .as_i64()
                .and_then(|u| crate::device_info::uptime(u, l))
                .unwrap_or_else(dash)
        } else {
            dash()
        };
        let clients = if have("clients") {
            data("clients")
                .and_then(|c| c["total"].as_i64())
                .map_or_else(dash, |n| n.to_string())
        } else {
            dash()
        };

        // ---- battery ----
        let b = data("battery").unwrap_or(&Value::Null);
        let b_have = have("battery");
        // Plugged in = the charger input (`charger_connect`), not `online` (that is "the battery is
        // there": the recording says online=1 while discharging).
        let plugged = num(&b["charger_connect"]).map(|c| c != 0.0);
        let charging = plugged == Some(true) && num(&b["time_to_full"]).is_some_and(|t| t >= 0.0);
        let word = match (plugged, charging) {
            (_, true) => l.pick("Charging", "充电中"),
            (Some(true), _) => l.pick("Plugged in", "已接电源"),
            (Some(false), _) => l.pick("On battery", "使用电池"),
            _ => "",
        };
        let est = (!self.screen.stale(now))
            .then(|| self.screen.val.as_ref())
            .flatten()
            .and_then(|s| estimate_text(&s["battery"], l));
        let bat_word = match (word.is_empty(), est) {
            (_, None) => word.to_owned(),
            (true, Some(e)) => e,
            (false, Some(e)) => format!("{word} · {e}"),
        };
        let bat = ro(
            !b_have,
            num(&b["percent"]).filter(|p| *p >= 0.0).map(|p| js_num(p.round())),
            bat_word,
        );

        // ---- network identity (netinfo) ----
        let ni = self.ni.val.as_ref();
        let exit = |k: &str| ni.map(|n| &n[k]).filter(|x| x.is_object());
        let d = exit("direct");
        let p = exit("proxy");
        let s = |v: Option<&Value>, k: &str| {
            v.and_then(|v| v[k].as_str()).filter(|x| !x.is_empty()).map(str::to_owned)
        };
        let pk = |v: Option<&Value>, k: &str| -> String {
            let zh = s(v, k);
            let en = s(v, &format!("{k}_en"));
            match (l, en) {
                (Lang::En, Some(e)) => e,
                _ => zh.unwrap_or_default(),
            }
        };
        let two;
        let same;
        two = p.is_some() && s(p, "ip") != s(d, "ip");
        same = p.is_some() && !two;
        let exit_sub = |e: Option<&Value>, extra: Vec<String>| -> Option<String> {
            let Some(e) = e else {
                return ni.map(|_| l.pick("Looking up…", "正在查询…").to_owned());
            };
            if s(Some(e), "ip").is_none() && s(Some(e), "error").is_some() {
                return Some(match l {
                    Lang::En => format!("Lookup failed: {}", pk(Some(e), "error")),
                    Lang::Zh => format!("查询失败：{}", pk(Some(e), "error")),
                });
            }
            let mut parts = vec![pk(Some(e), "geo")];
            parts.extend(extra);
            let mut joined = parts.into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
            if s(Some(e), "error").is_some() {
                if !joined.is_empty() {
                    joined.push_str(" · ");
                }
                joined.push_str(l.pick("refresh failed", "刷新失败"));
            }
            (!joined.is_empty()).then_some(joined)
        };
        let _ = same;
        #[allow(unused_mut)]
        let mut extra = vec![pk(d, "isp")];
        if same {
            extra.push(l.pick("CHILL direct", "CHILL 直连").to_owned());
        }
        let op_name = |o: Option<&Value>| match (l, s(o, "operator_en")) {
            (Lang::En, Some(e)) => e,
            _ => s(o, "name").unwrap_or_default(),
        };
        let op = |o: Option<&Value>| -> String {
            let Some(ov) = o.filter(|v| v["name"].as_str().is_some_and(|x| !x.is_empty()) || v["mcc"].as_str().is_some_and(|x| !x.is_empty())) else {
                return dash();
            };
            let plmn = match (s(Some(ov), "mcc"), s(Some(ov), "mnc")) {
                (Some(a), Some(b)) => format!("{a}{b}"),
                _ => String::new(),
            };
            let abroad = match s(Some(ov), "country_iso") {
                Some(iso) => iso != "CN",
                None => s(Some(ov), "country").is_some_and(|c| c != "中国"),
            };
            let country = pk(Some(ov), "country");
            let where_ = if abroad && !country.is_empty() {
                match l {
                    Lang::En => format!(" ({country}) "),
                    Lang::Zh => format!("（{country}）"),
                }
            } else {
                " ".to_owned()
            };
            let name = op_name(Some(ov));
            format!("{}{where_}{plmn}", if name.is_empty() { "?" } else { &name })
                .trim()
                .to_owned()
        };
        let home = ni.map(|n| &n["home_operator"]).filter(|x| x.is_object());
        let serving = ni.map(|n| &n["serving_operator"]).filter(|x| x.is_object());
        let differs = match (plmn_of(serving), plmn_of(home)) {
            (Some(a), Some(b)) => a != b,
            _ => {
                let (a, b) = (s(serving, "name"), s(home, "name"));
                a.is_some() && b.is_some() && a != b
            }
        };
        let logo = |size: u32| match serving {
            Some(o) if o["name"].as_str().is_some_and(|x| !x.is_empty()) || plmn_of(Some(o)).is_some() => crate::oplogo::html(
                &self.logo_dir,
                o["mcc"].as_str().unwrap_or(""),
                o["mnc"].as_str().unwrap_or(""),
                &op_name(Some(o)),
                size,
            ),
            _ => String::new(),
        };
        let (reg_logo, sub_logo) = (logo(20), logo(18));
        let reg_sub = differs.then(|| {
            let n = op_name(home);
            let n = if n.is_empty() { op(home) } else { n };
            match l {
                Lang::En => format!("SIM: {n}"),
                Lang::Zh => format!("SIM：{n}"),
            }
        });
        let id_chill = chill::id_row(l, if two { p } else { None }, &exit_sub(p, vec![s(p, "node").unwrap_or_default()]));

        // ---- Wi-Fi: the wifi block + the actual channel/width from the agent ----
        let w = crate::wifi_main::wifi(st).filter(|_| seen);
        let lw = self.wifi.val.clone().unwrap_or_default();
        let band_line = |i: usize| -> String {
            let Some(w) = &w else { return dash() };
            let b = &w.bands[i];
            if !b.up() {
                return l.pick("Off", "关闭").to_owned();
            }
            let mhz = &lw.bw[i];
            let parts = [
                (!b.ssid.is_empty()).then(|| b.ssid.clone()),
                (!lw.ch[i].is_empty()).then(|| match l {
                    Lang::En => format!("ch {}", lw.ch[i]),
                    Lang::Zh => format!("信道 {}", lw.ch[i]),
                }),
                (!mhz.is_empty()).then(|| {
                    if mhz.to_ascii_lowercase().contains("mhz") {
                        mhz.clone()
                    } else {
                        format!("{mhz} MHz")
                    }
                }),
                (!b.encryption.is_empty()).then(|| b.encryption.clone()),
            ];
            let j = parts.into_iter().flatten().collect::<Vec<_>>().join(" · ");
            if j.is_empty() { dash() } else { j }
        };
        let wifi_on = match &w {
            Some(w) if w.on => l.pick("On", "开启").to_owned(),
            Some(_) => l.pick("Off", "关闭").to_owned(),
            None => dash(),
        };

        // ---- carriers ----
        let cell_id = {
            let lte_first = cs.first().is_some_and(|c| !c.nr);
            let i = |k: &str| num(&sig[k]).map(|f| f as i64);
            if lte_first {
                i("lte_cell_id").or_else(|| i("nr_cell_id"))
            } else {
                i("nr_cell_id").or_else(|| i("lte_cell_id"))
            }
            .filter(|c| *c != 0)
        };
        let band_name = |c: &view::Carrier| if c.nr { format!("n{}", c.band) } else { format!("B{}", c.band) };
        let is_active = |c: &view::Carrier| c.rsrp.is_none_or(|r| r > -140);
        let ca_rows: Vec<CaRow> = cs
            .iter()
            .filter(|c| is_active(c))
            .map(|c| CaRow {
                label: band_name(c),
                line: [
                    c.bw.map(|b| format!("{} MHz", js_num(b))),
                    c.rsrp.map(|r| format!("{r} dBm")),
                    c.sinr.map(|s| format!("SINR {}", js_num(s))),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
                ids: format!(
                    "PCI {} · ARFCN {}{}{}",
                    c.pci.map_or_else(dash, |p| p.to_string()),
                    c.arfcn.map_or_else(dash, |p| p.to_string()),
                    match (c.serving, cell_id) {
                        (true, Some(id)) => format!(" · Cell {id}"),
                        _ => String::new(),
                    },
                    if c.serving {
                        format!(" · {}", l.pick("serving", "服务小区"))
                    } else {
                        String::new()
                    }
                ),
            })
            .collect();
        let off: Vec<String> = cs.iter().filter(|c| !is_active(c)).map(band_name).collect();
        let ca_off = (!off.is_empty()).then(|| off.join(" · "));
        let ca_sel = sig["net_select_mode"]
            .as_str()
            .filter(|m| !m.is_empty())
            .map(|_| view::selection_word(&sig["net_select_mode"], l));
        let ca_rows = if seen { ca_rows } else { Vec::new() };
        let shape = (ca_rows.len(), ca_off.is_some(), ca_sel.is_some());
        let ca_full = self.ca_shape != Some(shape);
        self.ca_shape = Some(shape);

        // ---- the header: title, subtitle, counts ----
        let di = data("uci_device_info").filter(|_| seen);
        let title = di.and_then(|d| {
            let t = |k: &str| d[k].as_str().map(str::trim).filter(|s| !s.is_empty());
            let name = t("device_market_name").or_else(|| t("device_alias_name"));
            let model = t("device_model").or_else(|| t("common_model_name"));
            match (name, model) {
                (Some(n), Some(m)) if n != m => Some(format!("{n} · {m}")),
                (Some(n), _) => Some(n.to_owned()),
                (None, Some(m)) => Some(m.to_owned()),
                _ => None,
            }
        });
        let op_text = sig["operator"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let ssid = w.as_ref().and_then(|w| {
            Some(w.bands[0].ssid.clone())
                .filter(|s| !s.is_empty())
                .or_else(|| Some(w.bands[1].ssid.clone()).filter(|s| !s.is_empty()))
        });
        let sub = seen
            .then(|| op_text.map(str::to_owned).or(ssid))
            .flatten();
        let sub_loading = !seen;
        let sms_n = if seen {
            data("sms").and_then(|s| s["unread"].as_u64()).unwrap_or(0)
        } else {
            0
        };
        let alerts_n = st
            .side
            .alerts
            .as_ref()
            .and_then(|a| a["unread"].as_u64())
            .or_else(|| {
                st.side
                    .public
                    .as_ref()
                    .and_then(|p| p["alerts"]["unread"].as_u64())
            })
            .unwrap_or(0);
        let counted = |n: u64, one_en: &str, one_zh: &str, none_en: &'static str, none_zh: &'static str| {
            if n == 0 {
                l.pick(none_en, none_zh).to_owned()
            } else {
                match l {
                    Lang::En => format!("{n} {one_en}"),
                    Lang::Zh => format!("{n} {one_zh}"),
                }
            }
        };

        // ---- scenario (u60d's public status via the hub's side poller) ----
        let scene = match st.side.public.as_ref().map(|p| &p["scenario"]) {
            None => Scene::Loading,
            Some(sc) if sc["configured"].as_bool() != Some(true) => Scene::None,
            Some(sc) => {
                let name = pk(Some(sc), "name");
                let name = if name.is_empty() {
                    sc["current"].as_str().unwrap_or("—").to_owned()
                } else {
                    name
                };
                Scene::On(ScOn {
                    name,
                    pinned: sc["pin"].as_str().is_some_and(|p| !p.is_empty()),
                    engine_off: sc["enabled"].as_bool() == Some(false),
                    last: sc["last_switch"]
                        .as_u64()
                        .map(|t| agent::hhmm(UNIX_EPOCH + Duration::from_secs(t))),
                })
            }
        };

        // ---- Tailscale ----
        let ts = ts_html(l, &self.ts, now);

        View {
            l,
            title,
            sub,
            sub_loading,
            sms_n,
            sms_label: counted(sms_n, "unread SMS", "条未读短信", "SMS", "短信"),
            alerts_n,
            alerts_label: counted(alerts_n, "new alerts", "条新告警", "Alerts", "告警"),
            status: crate::pages::status_frag(&sv),
            why,
            finding,
            ro_stale: sig_stale || (seen && !block_ok("live")),
            bw,
            rsrp,
            sinr,
            dl,
            ul,
            temp,
            bat,
            d_stale: seen && (!block_ok("clients") || !block_ok("live")),
            clients,
            day: usage("day_rx_bytes", "day_tx_bytes"),
            month: usage("month_rx_bytes", "month_tx_bytes"),
            uptime,
            id_stale: self.ni.stale(now),
            ip_label: if two {
                l.pick("Cellular exit", "蜂窝出口")
            } else {
                l.pick("Public IP", "公网 IP")
            },
            ip: s(d, "ip").unwrap_or_else(dash),
            ip_sub: exit_sub(d, extra),
            id_chill,
            sim: op(home),
            reg: op(serving),
            reg_sub,
            reg_logo,
            sub_logo: if op_text.is_some() { sub_logo } else { String::new() },
            roaming: ni.and_then(|n| n["roaming"].as_bool()),
            w_stale: seen && (!block_ok("wifi") || self.wifi.stale(now)),
            wifi_on,
            wifi_2: band_line(0),
            wifi_5: band_line(1),
            ca_full,
            ca_stale: sig_stale,
            ca_rows,
            ca_off,
            ca_sel,
            scene,
            ts_stale: self.ts.stale(now),
            ts,
        }
    }
}

impl Polled for Poll {
    async fn step(&mut self, app: &Shared) -> (Vec<(String, String)>, Duration) {
        let now = Instant::now();
        // a step before the earliest deadline is a kick (CHILL write done): read CHILL again
        let kicked = now + Duration::from_millis(300) < self.earliest();
        let (screen, ts, ni, wifi) = tokio::join!(
            fetch(app, &self.who, self.screen.due(now), "/api/screen"),
            fetch(app, &self.who, self.ts.due(now), "/api/services/tailscale"),
            fetch(app, &self.who, self.ni.due(now), "/api/netinfo?lite=1"),
            fetch(app, &self.who, self.wifi.due(now), "/api/wifi/status"),
        );
        self.chill.poll(app, &self.who, now, kicked).await;
        let at = Instant::now();
        if let Some(r) = screen {
            self.screen.set(at, r);
        }
        if let Some(r) = ts {
            // running but the daemon does not answer = not a valid read (freshness.ts tailscaleValid)
            self.ts.set(at, r.map(|v| ts_of(&v)).filter(|t| !(t.running && t.error)));
        }
        if let Some(r) = ni {
            self.ni.set(at, r);
        }
        if let Some(r) = wifi {
            // only the channel and width: the reply holds both passwords
            self.wifi.set(at, r.map(|v| crate::wifi_main::Live::of(&v)));
        }
        let out = self.leaves(app);
        let delay = self
            .earliest()
            .saturating_duration_since(Instant::now())
            .max(Duration::from_secs(1));
        (out, delay)
    }

    fn watches(&self, block: &str) -> bool {
        matches!(
            block,
            "signal" | "live" | "battery" | "clients" | "wifi" | "wlan" | "sms" | "interfaces"
                | "uci_device_info" | "side"
        )
    }

    fn render(&mut self, app: &Shared) -> Option<Vec<(String, String)>> {
        Some(self.leaves(app))
    }
}

// ---- words ----

fn carrier_summary(l: Lang, nr: usize, lte: usize) -> String {
    match (nr, lte) {
        (0, 0) => l.pick("No active carriers", "没有激活的载波").to_owned(),
        (n, 0) => match l {
            Lang::En => format!("{n} NR carriers"),
            Lang::Zh => format!("{n} 个 NR 载波"),
        },
        (0, m) => match l {
            Lang::En => format!("{m} LTE carriers"),
            Lang::Zh => format!("{m} 个 LTE 载波"),
        },
        (n, m) => match l {
            Lang::En => format!("{m} LTE + {n} NR carriers"),
            Lang::Zh => format!("{m} 个 LTE + {n} 个 NR 载波"),
        },
    }
}

fn temp_word(l: Lang, c: f64) -> &'static str {
    if c >= 80.0 {
        l.pick("Hot", "过热")
    } else if c >= 65.0 {
        l.pick("Warm", "偏热")
    } else {
        l.pick("Normal", "正常")
    }
}

fn duration_text(m: i64, l: Lang) -> String {
    if m < 60 {
        return match l {
            Lang::En => format!("{m} min"),
            Lang::Zh => format!("{m} 分钟"),
        };
    }
    let (h, r) = (m / 60, m % 60);
    match (l, r) {
        (Lang::En, 0) => format!("{h} h"),
        (Lang::Zh, 0) => format!("{h} 小时"),
        (Lang::En, r) => format!("{h} h {r} min"),
        (Lang::Zh, r) => format!("{h} 小时 {r} 分"),
    }
}

/// `/api/screen`'s `battery` report (battery_eta.rs) in words; only a fresh `ok` one.
pub(crate) fn estimate_text(rep: &Value, l: Lang) -> Option<String> {
    if rep["state"].as_str() != Some("ok") {
        return None;
    }
    let target = rep["target_pct"].as_i64().unwrap_or(100);
    let mins = rep["minutes"].as_i64();
    Some(match rep["kind"].as_str()? {
        "charging_eta" => {
            let d = duration_text(mins?, l);
            match (l, target >= 100) {
                (Lang::En, true) => format!("Full in about {d}"),
                (Lang::Zh, true) => format!("约 {d}充满"),
                (Lang::En, false) => format!("{target}% in about {d}"),
                (Lang::Zh, false) => format!("约 {d}充到 {target}%"),
            }
        }
        "discharging_eta" => {
            let d = duration_text(mins?, l);
            match l {
                Lang::En => format!("About {d} left"),
                Lang::Zh => format!("约可用 {d}"),
            }
        }
        "reached_target" => match (l, target >= 100) {
            (Lang::En, true) => "Full".into(),
            (Lang::Zh, true) => "已充满".into(),
            (Lang::En, false) => format!("At the {target}% limit"),
            (Lang::Zh, false) => format!("已到上限 {target}%"),
        },
        "paused_at_limit" => l.pick("At the limit, charging paused", "已到上限，暂停充电").into(),
        _ => return None,
    })
}

/// "460-001" from mcc / mnc (lib/operatorLogo.ts normalizePlmn).
fn plmn_of(o: Option<&Value>) -> Option<String> {
    let o = o?;
    let (c, n) = (o["mcc"].as_str()?.trim(), o["mnc"].as_str()?.trim());
    (c.len() == 3
        && c.bytes().all(|b| b.is_ascii_digit())
        && (1..=3).contains(&n.len())
        && n.bytes().all(|b| b.is_ascii_digit()))
    .then(|| format!("{c}-{n:0>3}"))
}

fn ts_html(l: Lang, ts: &Src<Ts>, now: Instant) -> String {
    let Some(t) = &ts.val else {
        return if ts.failed() {
            format!(
                "<p class=\"nd-body text-nd-t2\">{}</p>",
                l.pick("Can't read Tailscale status", "读不到 Tailscale 状态")
            )
        } else {
            "<span class=\"nd-skel\" style=\"width:10ch\"></span>".to_owned()
        };
    };
    let _ = now;
    if !t.installed {
        return format!(
            "<p class=\"nd-body text-nd-t2\">{}</p>",
            l.pick("Not installed", "未安装")
        );
    }
    if !t.running {
        return mark("warn", l.pick("Not running · open ›", "未运行 · 去打开 ›"));
    }
    let online = t.online == Some(true);
    let n = if t.capped {
        "32+".to_owned()
    } else {
        t.peers_online.to_string()
    };
    let mut sub = match l {
        Lang::En => format!("{n} devices online"),
        Lang::Zh => format!("{n} 台设备在线"),
    };
    if let Some(r) = &t.relay {
        sub.push_str(&match l {
            Lang::En => format!(" · relay {r}"),
            Lang::Zh => format!(" · 中继 {r}"),
        });
    }
    format!(
        "{}<p class=\"nd-aux mt-1\" data-data>{}</p>",
        mark(
            if online { "ok" } else { "warn" },
            if online {
                l.pick("Online", "在线")
            } else {
                l.pick("Offline", "离线")
            }
        ),
        agent::esc(&sub)
    )
}

#[allow(dead_code)]
fn _unused(_: SystemTime) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn battery_estimate_words() {
        let r = |kind: &str, m: i64, t: i64| json!({"state":"ok","kind":kind,"minutes":m,"target_pct":t});
        assert_eq!(estimate_text(&r("charging_eta", 95, 100), Lang::En).unwrap(), "Full in about 1 h 35 min");
        assert_eq!(estimate_text(&r("charging_eta", 40, 80), Lang::Zh).unwrap(), "约 40 分钟充到 80%");
        assert_eq!(estimate_text(&r("discharging_eta", 272, 100), Lang::En).unwrap(), "About 4 h 32 min left");
        assert!(estimate_text(&json!({"state":"estimating","kind":"discharging_eta"}), Lang::En).is_none());
        assert!(estimate_text(&r("unknown", 0, 100), Lang::En).is_none());
    }

    #[test]
    fn plmn_is_normalised() {
        assert_eq!(plmn_of(Some(&json!({"mcc":"460","mnc":"1"}))).as_deref(), Some("460-001"));
        assert_eq!(plmn_of(Some(&json!({"mcc":"460","mnc":"01"}))), plmn_of(Some(&json!({"mcc":"460","mnc":"001"}))));
        assert!(plmn_of(Some(&json!({"mcc":"","mnc":"1"}))).is_none());
    }

    #[test]
    fn a_source_goes_stale_after_two_failures() {
        let mut s: Src<i32> = Src::new(10);
        let t = Instant::now();
        s.set(t, Some(1));
        assert!(!s.stale(t));
        s.set(t, None);
        assert!(!s.stale(t));
        s.set(t, None);
        assert!(s.stale(t) && s.val == Some(1));
    }

    fn feed(hub: &crate::hub::Hub, event: &str, v: Value) {
        hub.apply(&crate::sse::Frame { event: event.into(), data: v.to_string() });
    }

    fn block(name: &str, rev: u64, data: Value) -> Value {
        json!({"epoch":"e","name":name,"revision":rev,"observed_at":rev,"stale":false,"data":data})
    }

    fn poll() -> Poll {
        Poll::new(Lang::En, Caller { token: Some(None), peer: None, sid: None })
    }

    fn app_hub() -> std::sync::Arc<crate::hub::Hub> {
        let hub = crate::hub::Hub::new();
        feed(&hub, "snapshot", json!({"epoch":"e","blocks":{
            "signal": block("signal", 1, json!({"type":"SA","bars":4,"nr_rsrp":-97,"nr_snr":12,"nr_rsrq":-11,"nr_band":"n1","nr_bw":"40","nr_pci":5,"nr_channel":7,"nr_cell_id":99,"operator":"Chunghwa","net_select_mode":"auto_select",
                "nrca":"1,2,0,78,630000,100,0,-100,-11,9,-70;"})),
            "live": block("live", 1, json!({"system":{"cpu_temp":71,"uptime":93780},"traffic":{"rx_speed":1_250_000,"tx_speed":12_500,"day_rx_bytes":2_000_000_000_u64,"day_tx_bytes":185_000_000,"month_rx_bytes":58_000_000_000_u64,"month_tx_bytes":5_000_000_000_u64}})),
            "battery": block("battery", 1, json!({"percent":85,"online":1,"charger_connect":0,"time_to_full":-1})),
            "clients": block("clients", 1, json!({"lan":0,"wifi":3,"total":3,"list":[]})),
            "sms": block("sms", 1, json!({"unread":2,"count":4,"max_id":9})),
            "uci_device_info": block("uci_device_info", 1, json!({"device_market_name":"U60 Pro","device_model":"MU5250"})),
        }}));
        hub
    }

    #[test]
    fn nothing_from_the_hub_is_shown_before_the_stream_saw_the_link_live() {
        let hub = app_hub();
        hub.set_link_for_test(Link::Idle);
        let mut p = poll();
        // the hub holds blocks but the link was never Live in this stream (idle drop leftovers)
        let v = hub.read(|st| p.view(st, Instant::now()));
        assert!(v.rsrp.loading() && v.bat.loading() && v.dl.loading(), "skeletons");
        assert_eq!(v.clients, "—");
        assert!(v.title.is_none() && v.sub.is_none() && v.sub_loading);
        assert_eq!(v.sms_n, 0);
        assert!(!v.why);
    }

    #[test]
    fn values_come_from_the_blocks_once_live() {
        let hub = app_hub();
        hub.set_link_for_test(Link::Live);
        let mut p = poll();
        let v = hub.read(|st| p.view(st, Instant::now()));
        assert_eq!(v.title.as_deref(), Some("U60 Pro · MU5250"));
        assert_eq!(v.sub.as_deref(), Some("Chunghwa"));
        assert_eq!(v.dl.text(), "10.0");
        assert_eq!(v.ul.text(), "0.10");
        assert_eq!(v.temp.text(), "71");
        assert_eq!(v.temp.word, "Warm");
        assert_eq!(v.bat.text(), "85");
        assert_eq!(v.bat.word, "On battery");
        assert_eq!(v.clients, "3");
        assert_eq!(v.day, "↓ 2.00 GB · ↑ 185 MB");
        assert_eq!(v.uptime, "1 d 2 h");
        assert_eq!(v.sms_n, 2);
        // two carriers: the serving n1 (40) and n78 (100)
        assert_eq!(v.bw.text(), "140");
        assert_eq!(v.bw.word, "2 NR carriers");
        assert_eq!(v.ca_rows.len(), 2);
        assert!(v.ca_rows[0].ids.contains("Cell 99") && v.ca_rows[0].ids.contains("serving"));
        assert_eq!(v.rsrp.word, "RSRQ -11 · Good");
        assert_eq!(v.ca_sel.as_deref(), Some("Automatic"));
        assert!(v.ca_full);
        let v2 = hub.read(|st| p.view(st, Instant::now()));
        assert!(!v2.ca_full, "same shape: the group is not resent");
    }

    #[test]
    fn plugged_in_is_the_charger_not_online() {
        let hub = app_hub();
        hub.set_link_for_test(Link::Live);
        feed(&hub, "block", block("battery", 2, json!({"percent":50,"online":1,"charger_connect":1,"time_to_full":-1})));
        let mut p = poll();
        assert_eq!(hub.read(|st| p.view(st, Instant::now())).bat.word, "Plugged in");
        feed(&hub, "block", block("battery", 3, json!({"percent":50,"online":1,"charger_connect":1,"time_to_full":50})));
        assert_eq!(hub.read(|st| p.view(st, Instant::now())).bat.word, "Charging");
        p.screen.set(Instant::now(), Some(json!({"battery":{"state":"ok","kind":"charging_eta","minutes":50,"target_pct":100}})));
        assert_eq!(hub.read(|st| p.view(st, Instant::now())).bat.word, "Charging · Full in about 50 min");
    }

    #[test]
    fn the_diagnose_link_follows_the_verdict_else_the_signal() {
        let hub = app_hub();
        hub.set_link_for_test(Link::Live);
        let mut p = poll();
        assert!(!hub.read(|st| p.view(st, Instant::now())).why);
        hub.set_side(|s| s.public = Some(json!({"network":{"verdict":"stall"}})));
        assert!(hub.read(|st| p.view(st, Instant::now())).why);
        hub.set_side(|s| s.public = Some(json!({"network":{"verdict":"ok"}})));
        assert!(!hub.read(|st| p.view(st, Instant::now())).why);
    }

    #[test]
    fn a_sure_finding_shows_its_symptom() {
        let hub = app_hub();
        hub.set_link_for_test(Link::Live);
        let mut p = poll();
        hub.set_side(|s| s.public = Some(json!({"network":{"verdict":"ok","finding":null}})));
        assert_eq!(hub.read(|st| p.view(st, Instant::now())).finding, None);
        hub.set_side(|s| {
            s.public = Some(json!({"network":{"verdict":"ok","finding":{
                "kind":"no_internet","layer":"data",
                "symptom":"已注册，但数据连接没建立",
                "symptom_en":"Registered, but the data connection is down"}}}))
        });
        let f = hub.read(|st| p.view(st, Instant::now())).finding;
        assert!(f.is_some_and(|f| f.contains("data connection") || f.contains("数据连接")));
    }

    #[test]
    fn leaves_have_unique_ids_and_the_frame_has_them_all() {
        let hub = app_hub();
        hub.set_link_for_test(Link::Live);
        let mut p = poll();
        let v = hub.read(|st| p.view(st, Instant::now()));
        let ls = split_leaves(&Leaves { v: &v }.render().unwrap());
        let mut ids: Vec<&str> = ls.iter().map(|(i, _)| i.as_str()).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate leaf ids");
        let frame = Page {
            shell: crate::shell::Shell::new(&axum::http::HeaderMap::new(), PATH),
            l: Lang::En,
            status: String::new(),
            chill: chill::frame(Lang::En),
        }
        .render()
        .unwrap();
        for (id, _) in &ls {
            // carrier rows and the group appear only when there are carriers
            if id.starts_with("ca-") || id == "hm-ca" || id == "hc-off" || id == "hc-sel" || id == "sig-status" {
                continue;
            }
            assert!(frame.contains(&format!("id=\"{id}\"")), "the frame lacks #{id}");
        }
        assert_eq!(cjk(&ls.iter().map(|(_, h)| h.as_str()).collect::<String>()), Vec::<char>::new());
    }

    fn cjk(s: &str) -> Vec<char> {
        s.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).collect()
    }

    #[test]
    fn carriers_summary_words() {
        assert_eq!(carrier_summary(Lang::Zh, 1, 2), "2 个 LTE + 1 个 NR 载波");
        assert_eq!(carrier_summary(Lang::En, 0, 0), "No active carriers");
    }
}
