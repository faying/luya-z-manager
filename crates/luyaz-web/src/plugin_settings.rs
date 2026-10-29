//! /settings/plugin（插件设置，DR13、Eng Review 2 R26–R29、OV2）：一个进程插件自己描述的设置页。
//! 页面通用：分组、行名、说明、确认文案都是插件在运行时给的，这里只照封闭的格式摆出来。
//!
//! - 读：`/control plugin.settings.get`（core 转给插件；插件不在时回最后一次确认的描述和值）。
//! - 写：`/control plugin.settings.set {key, value, rev}`，表单 POST，写完回到这页并说明结果。
//! - 插件给的文字只进元素内容（askama 转义），不进任何属性、`data-*`、signal 或网址（R29 第二层；
//!   core 已按格式和上限查过）。键在这里再按 `[a-z0-9_.]{1,40}` 查一次，因为它会进 id、表单和网址。
//! - 错误码按码处理（OV2）：`stale_revision` 重读并显示现在的值；`result_unknown` 和没回音都读回定案；
//!   `plugin_offline` 是只读页；`plugin_invalid` 只说「插件描述无效」。
//! - 插件不在（R27）：开关不能按，标「最后确认于 MM-DD HH:mm」（设备时钟，按 UTC 格式就是当地时间）。

use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Form, FromRequest, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::i18n::Lang;
use crate::pages::Shared;

pub const PATH: &str = "/settings/plugin";
const SET_PATH: &str = "/settings/plugin/set";
/// core 等插件每次 3 秒；读最多两次（描述 + 值），写最多三次（冷缓存时先取描述和值）。
const GET_WAIT: Duration = Duration::from_secs(7);
const SET_WAIT: Duration = Duration::from_secs(10);

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page))
        .route(SET_PATH, post(set))
}

/// 键：只有小写字母、数字、`_`、`.`，1–40 个（和 core 的检查一致）。
fn is_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 40
        && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

// ---- 跟 core 说话 ----

enum Reply {
    Ok(Value),
    /// core 回的固定错误码。
    Err(String),
    /// 送出了但没回音（逾时、断线）：结果未知。
    Silent,
    /// 连不上 core。
    Down,
}

async fn control(app: &Shared, action: &str, params: Value, wait: Duration) -> Reply {
    let body = json!({
        "action": action,
        "params": params,
        "source": "web",
        "op_id": crate::write::next_op_id(),
    });
    let r = app.http.post(&app.control_url).json(&crate::pin::stamp(&body)).timeout(wait).send().await;
    match r {
        Err(e) if e.is_connect() => Reply::Down,
        Err(_) => Reply::Silent,
        Ok(r) => match r.json::<Value>().await {
            Err(_) => Reply::Silent,
            Ok(v) if v["ok"] == true => Reply::Ok(v["result"].clone()),
            Ok(v) => Reply::Err(v["error"]["code"].as_str().unwrap_or("").to_owned()),
        },
    }
}

async fn read(app: &Shared) -> Reply {
    control(app, "plugin.settings.get", json!({}), GET_WAIT).await
}

/// `/settings` 的入口：装了、有设置时回插件的标题（没有缓存时 None 标题）；其余 None。
pub async fn entry(app: &Shared, wait: Duration) -> Option<Option<String>> {
    let Ok(Reply::Ok(v)) = tokio::time::timeout(wait, read(app)).await else {
        return None;
    };
    if v["installed"] != true || v["settings"] == false {
        return None;
    }
    let title = parse(&v).ok().map(|d| d.title);
    Some(title)
}

// ---- 描述和值 → 页面 ----

#[derive(Debug, PartialEq)]
pub enum Kind {
    Toggle,
    Value,
    Page,
    Status,
}

pub struct Row {
    pub key: String,
    pub kind: Kind,
    pub label: String,
    /// 描述里的说明。
    pub note: Option<String>,
    /// 值里附的一句（现在的状态，例如为什么变灰）。
    pub vnote: Option<String>,
    pub confirm: Option<String>,
    pub page: Option<String>,
    /// 开关的值；None = 插件没给。
    pub on: Option<bool>,
    /// 只读值、状态行的文字。
    pub text: Option<String>,
    pub disabled: bool,
}

impl Row {
    /// 开关能不能按。
    pub fn live(&self, online: bool) -> bool {
        online && !self.disabled && self.on.is_some()
    }
}

pub struct Group {
    pub key: String,
    pub title: String,
    pub sub: bool,
    pub rows: Vec<Row>,
}

pub struct Desc {
    pub title: String,
    pub groups: Vec<Group>,
    pub rev: u64,
    pub online: bool,
    /// 「MM-DD HH:mm」。
    pub confirmed: Option<String>,
}

impl Desc {
    fn row(&self, key: &str) -> Option<&Row> {
        self.groups.iter().flat_map(|g| &g.rows).find(|r| r.key == key)
    }
}

pub enum View {
    /// 没装插件，或插件没有设置。
    None,
    /// 插件没在运行，也没有缓存。
    Offline,
    /// 描述不合格（core 或这里查出来的）。
    Invalid,
    /// 读不到 core。
    Unread,
    Ready(Desc),
}

fn s(v: &Value) -> Option<String> {
    v.as_str().filter(|t| !t.is_empty()).map(str::to_owned)
}

/// 把 core 回的 `{installed, online, descriptor, rev, rows, confirmed_at}` 变成页面要的样子。
/// 键不合格就整份不用。
fn parse(v: &Value) -> Result<Desc, View> {
    if v["installed"] != true || v["settings"] == false {
        return Err(View::None);
    }
    let online = v["online"] == true;
    let d = &v["descriptor"];
    if !d.is_object() {
        return Err(if online { View::Invalid } else { View::Offline });
    }
    let vals = &v["rows"];
    let mut groups = Vec::new();
    for g in d["groups"].as_array().ok_or(View::Invalid)? {
        let key = g["key"].as_str().filter(|k| is_key(k)).ok_or(View::Invalid)?;
        let mut rows = Vec::new();
        for r in g["rows"].as_array().ok_or(View::Invalid)? {
            let rkey = r["key"].as_str().filter(|k| is_key(k)).ok_or(View::Invalid)?;
            let kind = match r["kind"].as_str() {
                Some("toggle") => Kind::Toggle,
                Some("value") => Kind::Value,
                Some("page") => Kind::Page,
                Some("status") => Kind::Status,
                _ => return Err(View::Invalid),
            };
            let page = match &r["page"] {
                Value::Null => None,
                p => Some(p.as_str().filter(|k| is_key(k)).ok_or(View::Invalid)?.to_owned()),
            };
            let val = &vals[rkey];
            rows.push(Row {
                key: rkey.to_owned(),
                label: s(&r["label"]).ok_or(View::Invalid)?,
                note: s(&r["note"]),
                vnote: s(&val["note"]),
                confirm: if kind == Kind::Toggle { s(&r["confirm"]) } else { None },
                page,
                on: if kind == Kind::Toggle { val["value"].as_bool() } else { None },
                text: if matches!(kind, Kind::Value | Kind::Status) { s(&val["value"]) } else { None },
                disabled: val["disabled"] == true,
                kind,
            });
        }
        groups.push(Group {
            key: key.to_owned(),
            title: s(&g["title"]).ok_or(View::Invalid)?,
            sub: g["sub"] == true,
            rows,
        });
    }
    Ok(Desc {
        title: s(&d["title"]).ok_or(View::Invalid)?,
        groups,
        rev: v["rev"].as_u64().unwrap_or(0),
        online,
        confirmed: v["confirmed_at"].as_i64().map(crate::alerts_page::mdhm),
    })
}

fn view_of(r: Reply) -> View {
    match r {
        Reply::Ok(v) => parse(&v).map_or_else(|e| e, View::Ready),
        Reply::Err(c) if c == "plugin_invalid" => View::Invalid,
        Reply::Err(c) if c == "unknown_action" => View::None,
        _ => View::Unread,
    }
}

// ---- 页面 ----

#[derive(Template)]
#[template(path = "plugin_settings.html")]
struct Page {
    shell: crate::shell::Shell,
    view: View,
    /// 打开的子页（子分组的键）。
    sub: Option<String>,
    /// 等确认的开关。
    ask: Option<String>,
    /// 上一次写入的结果：(语气, 一句话)。
    note: Option<(&'static str, String)>,
}

impl Page {
    /// 这一屏显示的分组：主页是非子分组，子页只有那一组。
    fn shown<'a>(&self, d: &'a Desc) -> Vec<&'a Group> {
        match &self.sub {
            Some(k) => d.groups.iter().filter(|g| g.sub && &g.key == k).collect(),
            None => d.groups.iter().filter(|g| !g.sub).collect(),
        }
    }
    fn sub_q(&self) -> String {
        self.sub.as_ref().map_or(String::new(), |k| format!("page={k}&"))
    }
    fn sub_title<'a>(&self, d: &'a Desc) -> Option<&'a str> {
        let k = self.sub.as_ref()?;
        d.groups.iter().find(|g| g.sub && &g.key == k).map(|g| g.title.as_str())
    }
}

#[derive(Deserialize, Default)]
struct PageQuery {
    #[serde(default)]
    page: String,
    #[serde(default)]
    ask: String,
    /// 结果码（固定的几个，见 [`note_of`]）。
    #[serde(default)]
    m: String,
    #[serde(default)]
    k: String,
}

/// 结果码 → 一句话。插件的文字（行名）在这里由服务器放进去，不经网址。
fn note_of(l: Lang, m: &str, row: Option<&Row>) -> Option<(&'static str, String)> {
    let label = row.map_or(String::new(), |r| r.label.clone());
    let state = |on: Option<bool>| match on {
        Some(true) => l.pick("on", "开"),
        Some(false) => l.pick("off", "关"),
        None => "—",
    };
    Some(match m {
        "done" => (
            "ok",
            match l {
                Lang::En => format!("Changed: {label} is {}", state(row.and_then(|r| r.on))),
                _ => format!("已改好：{label}现在是{}", state(row.and_then(|r| r.on))),
            },
        ),
        "stale" => (
            "warn",
            l.pick(
                "Changed elsewhere in the meantime. Nothing was written; these are the current values.",
                "已被别处改过，这次没有写入；下面是现在的值。",
            )
            .into(),
        ),
        "unknown_not" => (
            "warn",
            l.pick(
                "The plugin did not answer in time; reading back shows it did not change.",
                "插件没有及时回应；读回显示没有改成。",
            )
            .into(),
        ),
        "unknown" => (
            "warn",
            l.pick(
                "Result unknown: the plugin did not answer and the value could not be read back.",
                "结果未知：插件没有回应，也读不回现在的值。",
            )
            .into(),
        ),
        "offline" => ("bad", l.pick("Not changed: the plugin is not running", "没改成：插件没在运行").into()),
        "busy" => ("bad", l.pick("Not changed: the plugin is busy, try again shortly", "没改成：插件正忙，稍后再试").into()),
        "invalid" => ("bad", l.pick("Not changed: the plugin's settings description is invalid", "没改成：插件描述无效").into()),
        "core" => ("bad", l.pick("Not changed: can't reach the core service", "没改成：连不上核心服务").into()),
        "failed" => ("bad", l.pick("Not changed: the plugin refused", "没改成：插件拒绝了").into()),
        _ => return None,
    })
}

async fn page(State(app): State<Shared>, Query(q): Query<PageQuery>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let view = view_of(read(&app).await);
    let row = match &view {
        View::Ready(d) if is_key(&q.k) => d.row(&q.k),
        _ => None,
    };
    let note = note_of(ctx.lang, &q.m, row);
    let key_in = |k: &str| match &view {
        View::Ready(d) if is_key(k) => d.row(k).is_some(),
        _ => false,
    };
    let ask = key_in(&q.ask).then(|| q.ask.clone());
    let sub = match &view {
        View::Ready(d) if is_key(&q.page) && d.groups.iter().any(|g| g.sub && g.key == q.page) => Some(q.page.clone()),
        _ => None,
    };
    let shell = app.hub.read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let html = Page { shell, view, sub, ask, note }.render().unwrap();
    crate::assets::maybe_gzip(&headers, "text/html; charset=utf-8", html.into_bytes(), "no-store")
}

#[derive(Deserialize)]
struct SetForm {
    key: String,
    to: String,
    rev: u64,
    #[serde(default)]
    page: String,
}

/// 写入的结果码（[`note_of`]）。
async fn write(app: &Shared, key: &str, to: bool, rev: u64) -> &'static str {
    let r = control(app, "plugin.settings.set", json!({"key": key, "value": to, "rev": rev}), SET_WAIT).await;
    match r {
        Reply::Ok(v) => {
            if v["rows"][key]["value"].as_bool() == Some(to) {
                "done"
            } else {
                read_back(app, key, to).await
            }
        }
        Reply::Err(c) => match c.as_str() {
            "stale_revision" => "stale",
            "result_unknown" => read_back(app, key, to).await,
            "plugin_offline" => "offline",
            "busy" => "busy",
            "plugin_invalid" => "invalid",
            _ => "failed",
        },
        Reply::Silent => read_back(app, key, to).await,
        Reply::Down => "core",
    }
}

/// 结果不明时读一次定案：插件在、值对上了算改好；插件在、值没变算没改成；读不到或只拿到缓存就是未知。
async fn read_back(app: &Shared, key: &str, to: bool) -> &'static str {
    match view_of(read(app).await) {
        View::Ready(d) if d.online => match d.row(key).and_then(|r| r.on) {
            Some(v) if v == to => "done",
            Some(_) => "unknown_not",
            None => "unknown",
        },
        _ => "unknown",
    }
}

/// POST /settings/plugin/set：`key`、`to`（true/false）、`rev`（页面读到的版本）、`page`（回到哪个子页）。
async fn set(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let back = |page: &str, m: &str, k: &str| {
        let page = if is_key(page) { format!("page={page}&") } else { String::new() };
        let k = if is_key(k) { k } else { "" };
        Redirect::to(&format!("{PATH}?{page}m={m}&k={k}")).into_response()
    };
    let Ok(Form(f)) = Form::<SetForm>::from_request(req, &app).await else {
        return back("", "failed", "");
    };
    let to = match f.to.as_str() {
        "true" => true,
        "false" => false,
        _ => return back(&f.page, "failed", ""),
    };
    if !is_key(&f.key) {
        return back(&f.page, "failed", "");
    }
    let m = write(&app, &f.key, to, f.rev).await;
    back(&f.page, m, &f.key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;
    use axum::http::HeaderMap;

    pub(crate) fn sample(online: bool) -> Value {
        json!({"installed": true, "online": online, "rev": 7, "confirmed_at": 1_793_000_000,
            "descriptor": {"title": "<b>插件</b>", "groups": [
                {"key": "main", "title": "开关", "rows": [
                    {"key": "alpha", "kind": "toggle", "label": "<script>alert(1)</script>", "note": "$x @post('/x')", "confirm": "data-on:click=\"@get('/x')\" 确定？"},
                    {"key": "beta", "kind": "toggle", "label": "第二项"},
                    {"key": "more", "kind": "page", "label": "更多", "page": "more"},
                    {"key": "line", "kind": "status", "label": "状态"}]},
                {"key": "more", "title": "更多设置", "sub": true, "rows": [
                    {"key": "more.x", "kind": "value", "label": "数字"}]}]},
            "rows": {"alpha": {"value": true}, "beta": {"value": false, "disabled": true, "note": "第一项关着时不能改"},
                     "line": {"value": "正常"}, "more.x": {"value": "25"}}})
    }

    fn render(v: &Value, sub: Option<&str>, ask: Option<&str>) -> String {
        let mut h = HeaderMap::new();
        h.insert("cookie", "u60_lang=zh".parse().unwrap());
        Page {
            shell: Shell::new(&h, PATH),
            view: parse(v).map_or_else(|e| e, View::Ready),
            sub: sub.map(str::to_owned),
            ask: ask.map(str::to_owned),
            note: None,
        }
        .render()
        .unwrap()
    }

    #[test]
    fn plugin_text_is_only_text() {
        let h = render(&sample(true), None, Some("alpha"));
        assert!(h.contains("&#60;script&#62;alert(1)&#60;/script&#62;"));
        assert!(!h.contains("<script>alert"));
        assert!(h.contains("&#60;b&#62;插件&#60;/b&#62;"));
        // the confirm sentence shows, escaped, and never inside an attribute
        assert!(h.contains("data-on:click=&quot;@get(&#x27;/x&#x27;)&quot; 确定？") || h.contains("data-on:click=&#34;@get(&#39;/x&#39;)&#34; 确定？"), "{h}");
        assert!(!h.contains("\"@get('/x')"));
        for tag in h.split('<').skip(1) {
            let attrs = tag.split('>').next().unwrap_or("");
            assert!(!attrs.contains("$x") && !attrs.contains("alert(") && !attrs.contains("'/x'") && !attrs.contains("确定"), "attribute carries plugin text: {attrs}");
        }
    }

    #[test]
    fn rows_and_sub_pages() {
        let h = render(&sample(true), None, None);
        // alpha asks first (a GET form), beta is disabled: nothing posts until the question is answered
        assert!(!h.contains(r#"action="/settings/plugin/set""#));
        let asked = render(&sample(true), None, Some("alpha"));
        assert!(asked.contains(r#"action="/settings/plugin/set""#) && asked.contains(r#"name="rev" value="7""#));
        assert!(asked.contains(r#"name="to" value="false""#));
        assert!(h.contains(r#"href="/settings/plugin?page=more""#));
        assert!(h.contains("正常") && h.contains("第一项关着时不能改"));
        assert!(!h.contains("数字"), "a sub group is not on the main page");
        // alpha asks first (a GET to ?ask=), beta is disabled
        assert!(h.contains(r#"name="ask" value="alpha""#));
        let sub = render(&sample(true), Some("more"), None);
        assert!(sub.contains("数字") && sub.contains("25") && sub.contains("更多设置"));
        assert!(!sub.contains("第二项"));
    }

    #[test]
    fn offline_is_read_only_with_the_time() {
        let h = render(&sample(false), None, None);
        assert!(h.contains("插件没在运行"));
        assert!(h.contains("最后确认于"));
        assert!(!h.contains(r#"action="/settings/plugin/set""#) && !h.contains(r#"name="ask""#));
        let none = render(&json!({"installed": true, "online": false}), None, None);
        assert!(none.contains("插件没在运行") && !none.contains("最后确认于"));
    }

    #[test]
    fn bad_keys_and_missing_plugin() {
        let mut v = sample(true);
        v["descriptor"]["groups"][0]["rows"][1]["key"] = json!("x\" onclick=\"y");
        assert!(matches!(parse(&v), Err(View::Invalid)));
        assert!(matches!(parse(&json!({"installed": false})), Err(View::None)));
        assert!(matches!(parse(&json!({"installed": true, "online": true, "settings": false})), Err(View::None)));
        assert!(render(&json!({"installed": false}), None, None).contains("没有可设置的插件"));
        assert!(!is_key("") && !is_key("A") && !is_key(&"a".repeat(41)) && is_key("rec.on_2"));
    }

    #[test]
    fn result_sentences() {
        let d = parse(&sample(true)).ok().unwrap();
        let (tone, t) = note_of(Lang::Zh, "done", d.row("alpha")).unwrap();
        assert_eq!(tone, "ok");
        assert!(t.contains("<script>") && t.ends_with("开"), "{t}");
        assert!(note_of(Lang::Zh, "stale", None).unwrap().1.contains("已被别处改过"));
        assert!(note_of(Lang::Zh, "unknown", None).unwrap().1.starts_with("结果未知"));
        assert!(note_of(Lang::Zh, "nope", None).is_none());
    }
}
