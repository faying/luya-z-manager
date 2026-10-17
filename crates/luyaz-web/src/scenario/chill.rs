//! The CHILL parts of /router/scenario (batch 5b): a scenario's CHILL node (what CHILL's main
//! group is set to on entering it), the abroad section (the shipped foreign-SIM scenario and the
//! restores waiting for the way home), and the node choices from `GET /api/services/chill` (read
//! once; the built-in regions when CHILL does not list its own).
//!
//! Same rules as the rest of the page: the browser sends the intent (`id`, `m`), the server edits
//! the config it has just read and proves the change by reading it again. A node `<select>`
//! changes in the browser before anything is sent: a cancel (or a lost reply) re-reads the page,
//! and the select carries the page's re-read counter in its id, so it is replaced by what the
//! device has instead of morphed.

use std::time::{Duration, Instant};

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::post;
use serde_json::{Value, json};

use super::{Args, Edit, Got as Page, Kind, PATH, Caller, ask_with, enc, find, gone, inline, name_of, scenarios, t};
use crate::agent::Fail;
use crate::i18n::Lang;
use crate::pages::Shared;

/// Must match CHILL_MAIN_GROUP in the agent's scenario code and scripts/chill/template.yaml.
pub const GROUP: &str = "🚀 节点选择";
const REGION_PATH: &str = "/api/services/chill/regions";
const STATUS: &str = "/api/services/chill";
const ACTIVE: &str = "/data/region/active";
/// Used when CHILL is not running and cannot list its own members.
const FALLBACK: [&str; 5] = ["DIRECT", "🇹🇼 台湾", "🇯🇵 日本", "🇸🇬 新加坡", "🇺🇸 美国"];
/// The agent ships exactly one abroad scenario.
const SHIPPED_ABROAD: usize = 1;
const RETRY: Duration = Duration::from_secs(60);

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/router/scenario/ask/node", post(ask_node))
        .route("/router/scenario/ask/abroad", post(ask_abroad))
}

/// What the page has from CHILL.
#[derive(Clone, Default)]
pub struct Got {
    /// The node choices of the main group (empty: not read).
    opts: Vec<String>,
    tried: Option<Instant>,
}

impl Got {
    /// Read the choices once (again after a minute if CHILL did not answer).
    pub async fn fetch(&mut self, app: &Shared, who: &Caller) {
        if !self.opts.is_empty() || self.tried.is_some_and(|t| t.elapsed() < RETRY) {
            return;
        }
        self.tried = Some(Instant::now());
        match crate::agent::get(app, who, STATUS).await {
            Ok(v) => {
                self.opts = v["region"]["options"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|o| o.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
            }
            Err(Fail::SignedOut | Fail::Unreachable | Fail::Refused(..)) => {}
        }
    }
    fn listed(&self) -> bool {
        !self.opts.is_empty()
    }
    fn options(&self) -> Vec<String> {
        if self.listed() {
            self.opts.clone()
        } else {
            FALLBACK.iter().map(|s| (*s).to_owned()).collect()
        }
    }
}

fn is_main_group(a: &Value) -> bool {
    a["path"] == REGION_PATH && a["body"]["group"] == GROUP
}

pub fn node_of(def: &Value) -> String {
    def["actions"]
        .as_array()
        .and_then(|a| a.iter().find(|a| is_main_group(a)))
        .and_then(|a| a["body"]["member"].as_str())
        .unwrap_or_default()
        .to_owned()
}

pub fn is_abroad(def: &Value) -> bool {
    matches!(def["detect"]["type"].as_str(), Some("mcc" | "abroad"))
}

/// Best-effort (CHILL may be off), verified by reading the group back, restored on leaving.
fn main_group_action(member: &str) -> Value {
    json!({
        "method": "PUT",
        "path": REGION_PATH,
        "body": {"group": GROUP, "member": member},
        "verify_field": {"path": STATUS, "pointer": ACTIVE, "equals": member},
        "best_effort": true,
        "restore_on_exit": {"read_path": STATUS, "read_pointer": ACTIVE, "field": "member"},
    })
}

fn node_label(o: &str, l: Lang) -> String {
    if o == "DIRECT" {
        l.pick("Direct", "直连").into()
    } else {
        o.to_owned()
    }
}

// ---- edits ----

struct Node {
    id: String,
    member: String,
    name: String,
}

impl Edit for Node {
    fn apply(&self, cfg: &mut Value, _: Option<&Value>, l: Lang) -> Result<(), String> {
        let def = cfg["scenarios"]
            .as_array_mut()
            .and_then(|a| a.iter_mut().find(|s| s["id"] == self.id))
            .ok_or_else(|| gone(l))?;
        let mut acts: Vec<Value> = def["actions"]
            .as_array()
            .map(|a| a.iter().filter(|a| !is_main_group(a)).cloned().collect())
            .unwrap_or_default();
        if !self.member.is_empty() {
            acts.push(main_group_action(&self.member));
        }
        def["actions"] = Value::Array(acts);
        Ok(())
    }
    fn holds(&self, cfg: &Value) -> bool {
        cfg["scenarios"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["id"] == self.id))
            .is_some_and(|d| node_of(d) == self.member)
    }
    fn pending(&self, l: Lang) -> String {
        l.pick("Saving…", "正在保存…").into()
    }
    fn applied(&self, l: Lang) -> String {
        if self.member.is_empty() {
            t!(l,
                "Entering “{n}” no longer touches CHILL's node",
                "进入「{n}」时不再改动 CHILL 节点",
                n = self.name)
        } else {
            t!(l,
                "CHILL node in “{n}”: {m}",
                "「{n}」的 CHILL 节点已设为 {m}",
                n = self.name,
                m = node_label(&self.member, l))
        }
    }
}

/// Append whichever shipped abroad scenarios are missing. Never rewrites what is already there —
/// the home networks in particular took effort to pick.
struct AddAbroad;

impl Edit for AddAbroad {
    fn needs_template(&self) -> bool {
        true
    }
    fn apply(&self, cfg: &mut Value, tpl: Option<&Value>, l: Lang) -> Result<(), String> {
        let have: Vec<String> = cfg["scenarios"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let missing: Vec<Value> = tpl
            .and_then(|t| t["scenarios"].as_array())
            .map(|a| {
                a.iter()
                    .filter(|s| is_abroad(s) && !have.iter().any(|h| s["id"] == h.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if missing.is_empty() {
            return Err(l
                .pick(
                    "The abroad scenario is already there",
                    "国外情景已经在了",
                )
                .into());
        }
        let list = cfg["scenarios"]
            .as_array_mut()
            .ok_or_else(|| gone(l))?;
        list.extend(missing);
        Ok(())
    }
    fn holds(&self, cfg: &Value) -> bool {
        cfg["scenarios"]
            .as_array()
            .is_some_and(|a| a.iter().any(is_abroad))
    }
    fn pending(&self, l: Lang) -> String {
        l.pick("Adding the abroad scenario…", "正在添加国外情景…").into()
    }
    fn applied(&self, l: Lang) -> String {
        l.pick("Abroad scenario added", "已添加国外情景").into()
    }
}

// ---- asks ----

async fn ask_node(
    State(app): State<Shared>,
    Query(a): Query<Args>,
    req: axum::extract::Request,
) -> Response {
    ask_with(&app, req, "sc-does", true, move |st, _, l, _| {
        let def = find(st, &a.id).ok_or_else(|| gone(l))?;
        let name = name_of(st, def, l);
        let label = if a.m.is_empty() {
            l.pick("leave as is", "不动").to_owned()
        } else {
            node_label(&a.m, l)
        };
        let what = if a.m.is_empty() {
            t!(l,
                "Entering “{name}” no longer touches CHILL's node.",
                "进入「{name}」时不再改动 CHILL 节点。")
        } else {
            t!(l,
                "On entering “{name}” CHILL's main group switches to {label}, and back on leaving. Nothing changes right now.",
                "进入「{name}」时 CHILL 主节点组切到 {label}，离开时恢复。现在不会有变化。")
        };
        let action = t!(l,
            "Confirm: CHILL node in “{name}”: {label}",
            "确认：「{name}」的 CHILL 节点改为 {label}");
        Ok((
            Kind::Edit(Box::new(Node {
                id: a.id.clone(),
                member: a.m.clone(),
                name,
            })),
            inline(l.pick("CHILL node", "CHILL 节点").into(), what, action),
        ))
    })
    .await
}

async fn ask_abroad(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    ask_with(&app, req, "sc-abroad", true, |st, _, l, _| {
        if scenarios(st).iter().filter(|s| is_abroad(s)).count() >= SHIPPED_ABROAD {
            return Err(l
                .pick(
                    "The abroad scenario is already there",
                    "国外情景已经在了",
                )
                .into());
        }
        Ok((
            Kind::Edit(Box::new(AddAbroad)),
            inline(
                l.pick("Abroad scenario", "国外情景").into(),
                l.pick(
                    "With a foreign SIM in the slot, CHILL's main group goes direct on arrival and comes back when you are home. Your home networks are kept.",
                    "插着外国 SIM 时，CHILL 的主节点组在到达时改为直连，回国后恢复。已有的在家网络保持不变。",
                )
                .into(),
                l.pick("Confirm: add abroad scenario", "确认：添加国外情景")
                    .into(),
            ),
        ))
    })
    .await
}

// ---- views ----

pub struct NodeRow<'a> {
    pub l: Lang,
    pub idx: usize,
    pub name: String,
    pub url: String,
    pub node: String,
    pub opts: Vec<(String, String)>,
    pub locked: bool,
    pub rev: u64,
    #[allow(dead_code)]
    pub tag: &'a str,
}

#[derive(Template)]
#[template(path = "scenario_chill.html", block = "node")]
struct NodeTpl<'a> {
    r: &'a NodeRow<'a>,
}

/// One scenario's CHILL node `<select>` row (inside the "what each scenario does" list).
pub fn node_row(
    st: &Value,
    def: &Value,
    idx: usize,
    chill: &Got,
    locked: bool,
    rev: u64,
    l: Lang,
) -> String {
    let node = node_of(def);
    let mut opts: Vec<String> = chill.options();
    if !node.is_empty() && !opts.contains(&node) {
        opts.push(node.clone());
    }
    let id = def["id"].as_str().unwrap_or_default();
    NodeTpl {
        r: &NodeRow {
            l,
            idx,
            name: name_of(st, def, l),
            url: format!("{PATH}/ask/node?id={}&m=", enc(id)),
            node,
            opts: opts
                .into_iter()
                .map(|o| {
                    let label = node_label(&o, l);
                    (o, label)
                })
                .collect(),
            locked,
            rev,
            tag: "",
        },
    }
    .render()
    .unwrap()
}

/// The note above the list when CHILL does not list its nodes.
pub fn does_note(chill: &Got, l: Lang) -> String {
    if chill.listed() {
        return String::new();
    }
    format!(
        "<p class=\"nd-aux mb-3 px-1\">{}</p>",
        l.pick(
            "CHILL is not listing its nodes right now, so the choices below are the built-in regions.",
            "CHILL 现在列不出节点，下面的选项是内置的几个地区。",
        )
    )
}

pub struct AbroadRow {
    pub name: String,
    pub value: String,
    /// The MCC list is device data; "any foreign SIM" is ours.
    pub mono: bool,
}

pub struct AbroadView {
    pub l: Lang,
    pub on: bool,
    pub stale: bool,
    pub locked: bool,
    pub rows: Vec<AbroadRow>,
    pub restores: Vec<String>,
    pub can_add: bool,
}

#[derive(Template)]
#[template(path = "scenario_chill.html", block = "abroad")]
struct AbroadTpl<'a> {
    v: &'a AbroadView,
}

/// One `pending_restore[]` entry in words.
fn restore_text(key: &str, body: &Value, l: Lang) -> String {
    match key {
        "chill-on-after-abroad" => l
            .pick(
                "CHILL was switched off abroad; it is switched back on when you return.",
                "CHILL 是在国外关掉的，回国后会自动打开。",
            )
            .into(),
        "chill-exit-after-abroad" => match body["state"].as_str() {
            Some("proxy") => l.pick("Exit back to “Proxy”", "出口恢复为「代理」").into(),
            other => t!(l,
                "Exit back to {x}",
                "出口恢复为 {x}",
                x = other.unwrap_or("—")),
        },
        _ => match body["member"].as_str() {
            Some(m) => node_label(m, l),
            None => "—".into(),
        },
    }
}

pub fn abroad_view(g: &Page, l: Lang) -> AbroadView {
    let nul = Value::Null;
    let s = g.st.as_ref().unwrap_or(&nul);
    let rows: Vec<AbroadRow> = scenarios(s)
        .iter()
        .filter(|d| is_abroad(d))
        .map(|d| {
            let mccs = d["detect"]["mccs"].as_array();
            let mono = d["detect"]["type"] == "mcc";
            AbroadRow {
                name: name_of(s, d, l),
                value: if mono {
                    mccs.map(|a| {
                        a.iter()
                            .filter_map(|m| m.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default()
                } else {
                    l.pick("any foreign SIM", "所有外国 SIM").into()
                },
                mono,
            }
        })
        .collect();
    AbroadView {
        l,
        on: g.configured(),
        stale: g.stale(),
        locked: g.locked(),
        can_add: rows.len() < SHIPPED_ABROAD,
        restores: s["pending_restore"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| restore_text(p["key"].as_str().unwrap_or_default(), &p["body"], l))
                    .collect()
            })
            .unwrap_or_default(),
        rows,
    }
}

/// The abroad section (`sc-abroad`).
pub fn leaves(g: &Page, l: Lang) -> Vec<(String, String)> {
    let html = AbroadTpl {
        v: &abroad_view(g, l),
    }
    .render()
    .unwrap();
    vec![("sc-abroad".into(), html)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Value {
        json!({"version":1,"home_mcc":"460","params":{},"scenarios":[
            {"id":"home","name":"在家","detect":{"type":"ssid","entries":[]},"actions":[{"method":"PUT","path":"/api/wifi/radio","body":{"ap_2g":false,"ap_5g":false},"snapshot":["a"]}],"inhibit_sleep":true},
            {"id":"away","name":"外出","detect":{"type":"fallback"},"actions":[],"inhibit_sleep":false}]})
    }

    #[test]
    fn a_node_is_set_replaced_and_cleared_without_touching_the_rest() {
        let mut c = cfg();
        let n = |m: &str| Node {
            id: "home".into(),
            member: m.into(),
            name: "x".into(),
        };
        n("DIRECT").apply(&mut c, None, Lang::En).unwrap();
        assert_eq!(node_of(&c["scenarios"][0]), "DIRECT");
        assert!(n("DIRECT").holds(&c));
        // the Wi-Fi action is as it was (snapshot kept)
        assert_eq!(c["scenarios"][0]["actions"][0]["snapshot"][0], "a");
        let main = &c["scenarios"][0]["actions"][1];
        assert_eq!(main["best_effort"], true);
        assert_eq!(main["restore_on_exit"]["field"], "member");
        assert_eq!(main["verify_field"]["equals"], "DIRECT");
        n("🇯🇵 日本").apply(&mut c, None, Lang::En).unwrap();
        assert_eq!(c["scenarios"][0]["actions"].as_array().unwrap().len(), 2);
        assert_eq!(node_of(&c["scenarios"][0]), "🇯🇵 日本");
        n("").apply(&mut c, None, Lang::En).unwrap();
        assert_eq!(c["scenarios"][0]["actions"].as_array().unwrap().len(), 1);
        assert!(n("").holds(&c));
        assert!(Node { id: "nope".into(), member: "".into(), name: "".into() }
            .apply(&mut c, None, Lang::En)
            .is_err());
    }

    #[test]
    fn abroad_adds_only_what_is_missing() {
        let mut c = cfg();
        let tpl = json!({"scenarios":[
            {"id":"home","name":"在家","detect":{"type":"ssid","entries":[]},"actions":[],"inhibit_sleep":true},
            {"id":"abroad","name":"国外","detect":{"type":"abroad"},"actions":[],"inhibit_sleep":false}]});
        AddAbroad.apply(&mut c, Some(&tpl), Lang::En).unwrap();
        let ids: Vec<&str> = c["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["home", "away", "abroad"]);
        assert!(AddAbroad.holds(&c));
        // twice: nothing to add
        assert!(AddAbroad.apply(&mut c, Some(&tpl), Lang::En).is_err());
    }

    #[test]
    fn restores_in_words() {
        assert!(restore_text("chill-on-after-abroad", &Value::Null, Lang::En).contains("switched back on"));
        assert_eq!(restore_text("chill-exit-after-abroad", &json!({"state":"proxy"}), Lang::En), "Exit back to “Proxy”");
        assert_eq!(restore_text("x", &json!({"member":"DIRECT"}), Lang::En), "Direct");
        assert_eq!(restore_text("x", &json!({"member":"🇯🇵 日本"}), Lang::Zh), "🇯🇵 日本");
        assert_eq!(restore_text("x", &Value::Null, Lang::Zh), "—");
    }
}
