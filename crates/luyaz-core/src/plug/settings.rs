//! 插件设置的转送（DR13，R26–R30）：网页经 `/control plugin.settings.get|set` 读写，core 转给插件。
//!
//! - 插件给的描述和值是不可信的：这里先按封闭的格式和上限查过（R29），不合格整份不用、不进缓存。
//!   网页那边只把文字放进文本节点，是第二层。
//! - 插件不在：读回最后一次确认的描述和值（只读，带确认时间）；写入直接拒绝，不排队（R27）。
//! - 写入带「我看到的版本」（R28），插件比对；逾时或送出后断线算「结果未知」，网页读回定案（R26、R30）。
//!
//! 描述：`{"title", "groups":[{"key","title","sub"?, "rows":[{"key","kind","label","note"?,"confirm"?,"page"?}]}]}`，
//! `kind` 只有 `toggle`、`value`、`page`、`status`。
//! 值：`{"rev", "rows":{<key>:{"value"?, "disabled"?, "note"?}}}`。

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::link::{CallError, Hub, Reply};

/// 描述整份的上限（R29）。
pub const DESC_MAX: usize = 16 * 1024;
const GROUPS_MAX: usize = 8;
const ROWS_MAX: usize = 32;
const KEY_MAX: usize = 40;
const LABEL_MAX: usize = 40;
const NOTE_MAX: usize = 200;
const CONFIRM_MAX: usize = 120;

/// 失败：HTTP 状态 + 固定错误码（OV2：网页按码处理，`result_unknown` 要读回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

fn fail(status: u16, code: &'static str, message: impl Into<String>) -> Fail {
    Fail {
        status,
        code,
        message: message.into(),
    }
}

/// 最后一次确认过的描述和值（只放内存，R27）。
#[derive(Default)]
struct Cache {
    /// 描述取自哪条连线；换连线就重取。
    generation: u64,
    descriptor: Option<Value>,
    values: Option<Value>,
    /// 确认时间（设备时钟的 Unix 秒；设备时钟是当地时间标成 UTC，网页按 UTC 格式显示就是当地时间）。
    confirmed_at: Option<u64>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    generation: 0,
    descriptor: None,
    values: None,
    confirmed_at: None,
});

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `plugin.settings.get`：描述 + 当前值。
pub async fn get(hub: &Hub, wait: Duration) -> Result<Value, Fail> {
    get_with(hub, &CACHE, wait).await
}

/// `plugin.settings.set`：`params = {key, value, rev}`；`extra` 是转给插件的 `source`/`approved_by`。
pub async fn set(
    hub: &Hub,
    params: &Value,
    extra: Map<String, Value>,
    wait: Duration,
) -> Result<Value, Fail> {
    set_with(hub, &CACHE, params, extra, wait).await
}

async fn get_with(hub: &Hub, cache: &Mutex<Cache>, wait: Duration) -> Result<Value, Fail> {
    if !hub.installed() {
        return Ok(json!({"installed": false}));
    }
    match hub.current() {
        Some(conn) if !conn.declares("settings") => {
            return Ok(json!({"installed": true, "online": true, "settings": false}));
        }
        Some(conn) => match fetch(hub, cache, conn.generation, wait).await {
            Ok(()) => return Ok(view(cache, true)),
            Err(Got::Down) => {}
            Err(Got::Fail(f)) => return Err(f),
        },
        None => {}
    }
    Ok(view(cache, false))
}

enum Got {
    /// 插件不在或没回：退回缓存的只读视图。
    Down,
    Fail(Fail),
}

/// 取描述（这条连线还没取过时）和当前值，查过才放进缓存。
async fn fetch(
    hub: &Hub,
    cache: &Mutex<Cache>,
    generation: u64,
    wait: Duration,
) -> Result<(), Got> {
    let have = {
        let c = cache.lock().unwrap();
        (c.generation == generation)
            .then(|| c.descriptor.clone())
            .flatten()
    };
    let descriptor = match have {
        Some(d) => d,
        None => {
            let d = call(hub, "settings.describe", json!({}), Map::new(), wait).await?;
            let d = check_descriptor(&d).map_err(|e| invalid(cache, &e))?;
            let mut c = cache.lock().unwrap();
            c.generation = generation;
            c.descriptor = Some(d.clone());
            c.values = None;
            d
        }
    };
    let v = call(hub, "settings.read", json!({}), Map::new(), wait).await?;
    let v = check_values(&v, &descriptor).map_err(|e| invalid(cache, &e))?;
    let mut c = cache.lock().unwrap();
    c.values = Some(v);
    c.confirmed_at = Some(now_unix());
    Ok(())
}

/// 不合格：整份不用、清掉缓存、写日志（R29）。
fn invalid(cache: &Mutex<Cache>, why: &str) -> Got {
    eprintln!("[plug] settings from the plugin refused: {why}");
    *cache.lock().unwrap() = Cache::default();
    Got::Fail(fail(
        502,
        "plugin_invalid",
        "plugin settings description is invalid",
    ))
}

async fn call(
    hub: &Hub,
    op: &str,
    params: Value,
    extra: Map<String, Value>,
    wait: Duration,
) -> Result<Value, Got> {
    match hub.request(op, params, extra, wait).await {
        Ok(Reply::Ok(v)) => Ok(v),
        Ok(Reply::Refused { code, .. }) => {
            eprintln!("[plug] {op} refused by the plugin: {code}");
            Err(Got::Fail(fail(502, "plugin_refused", "the plugin refused")))
        }
        Err(CallError::Busy) => Err(Got::Fail(fail(503, "busy", "plugin busy"))),
        Err(CallError::Offline | CallError::Unknown | CallError::Unsupported) => Err(Got::Down),
    }
}

fn view(cache: &Mutex<Cache>, online: bool) -> Value {
    let c = cache.lock().unwrap();
    let mut v = json!({"installed": true, "online": online});
    if let (Some(d), Some(vals)) = (&c.descriptor, &c.values) {
        v["descriptor"] = d.clone();
        v["rev"] = vals["rev"].clone();
        v["rows"] = vals["rows"].clone();
        v["confirmed_at"] = json!(c.confirmed_at);
    }
    v
}

async fn set_with(
    hub: &Hub,
    cache: &Mutex<Cache>,
    params: &Value,
    extra: Map<String, Value>,
    wait: Duration,
) -> Result<Value, Fail> {
    let bad = |m: &str| fail(400, "invalid_parameter", m);
    let key = params["key"]
        .as_str()
        .filter(|k| is_key(k))
        .ok_or_else(|| bad("key"))?;
    let rev = params["rev"].as_u64().ok_or_else(|| bad("rev"))?;
    let value = params
        .get("value")
        .filter(|v| v.is_boolean())
        .ok_or_else(|| bad("value"))?;
    let Some(conn) = hub.current() else {
        return Err(fail(503, "plugin_offline", "plugin not running"));
    };
    if !conn.declares("settings") {
        return Err(fail(404, "no_settings", "plugin has no settings"));
    }
    // 只有描述里的开关能写；还没取过这条连线的描述就先取
    let descriptor = {
        let c = cache.lock().unwrap();
        (c.generation == conn.generation)
            .then(|| c.descriptor.clone())
            .flatten()
    };
    let descriptor = match descriptor {
        Some(d) => d,
        None => match fetch(hub, cache, conn.generation, wait).await {
            Ok(()) => cache
                .lock()
                .unwrap()
                .descriptor
                .clone()
                .unwrap_or(Value::Null),
            Err(Got::Down) => return Err(fail(503, "plugin_offline", "plugin not running")),
            Err(Got::Fail(f)) => return Err(f),
        },
    };
    if row_kind(&descriptor, key) != Some("toggle") {
        return Err(bad("key is not a switch"));
    }
    let req = json!({"key": key, "value": value, "rev": rev});
    match hub.request("settings.write", req, extra, wait).await {
        Ok(Reply::Ok(v)) => {
            let v = check_values(&v, &descriptor).map_err(|e| match invalid(cache, &e) {
                Got::Fail(f) => f,
                Got::Down => unreachable!(),
            })?;
            let mut c = cache.lock().unwrap();
            c.values = Some(v.clone());
            c.confirmed_at = Some(now_unix());
            Ok(v)
        }
        Ok(Reply::Refused { code, .. }) if code == "stale_revision" => {
            Err(fail(409, "stale_revision", "changed elsewhere; read again"))
        }
        Ok(Reply::Refused { code, .. }) => {
            eprintln!("[plug] settings.write {key} refused by the plugin: {code}");
            Err(fail(502, "plugin_refused", "the plugin refused"))
        }
        Err(CallError::Offline) => Err(fail(503, "plugin_offline", "plugin not running")),
        Err(CallError::Busy) => Err(fail(503, "busy", "plugin busy")),
        Err(CallError::Unsupported) => Err(fail(404, "no_settings", "plugin has no settings")),
        Err(CallError::Unknown) => {
            Err(fail(504, "result_unknown", "no answer in time; read again"))
        }
    }
}

// ---- 格式检查（R29） ----

fn is_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= KEY_MAX
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
}

/// 一段给人看的文字：字数上限、不能有控制字符、不能带网址。
fn text(v: &Value, max: usize, what: &str) -> Result<String, String> {
    let s = v.as_str().ok_or_else(|| format!("{what}: not text"))?;
    if s.trim().is_empty() {
        return Err(format!("{what}: empty"));
    }
    if s.chars().count() > max {
        return Err(format!("{what}: over {max} characters"));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("{what}: control character"));
    }
    if s.contains("://") {
        return Err(format!("{what}: address"));
    }
    Ok(s.to_owned())
}

fn only(obj: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("{what}: unknown field {k}")),
        None => Ok(()),
    }
}

/// 查描述，回一份只带已知字段的副本。
pub fn check_descriptor(d: &Value) -> Result<Value, String> {
    let size = serde_json::to_vec(d).map(|b| b.len()).unwrap_or(usize::MAX);
    if size > DESC_MAX {
        return Err(format!("description is {size} bytes (max {DESC_MAX})"));
    }
    let obj = d.as_object().ok_or("description: not an object")?;
    only(obj, &["title", "groups"], "description")?;
    let title = text(&obj["title"], LABEL_MAX, "title")?;
    let groups = obj
        .get("groups")
        .and_then(Value::as_array)
        .ok_or("groups: not a list")?;
    if groups.is_empty() || groups.len() > GROUPS_MAX {
        return Err(format!("{} groups (1..={GROUPS_MAX})", groups.len()));
    }
    let mut group_keys = HashSet::new();
    let mut sub_groups = HashSet::new();
    let mut row_keys = HashSet::new();
    let mut pages = Vec::new();
    let mut rows_total = 0;
    let mut out_groups = Vec::new();
    for g in groups {
        let g = g.as_object().ok_or("group: not an object")?;
        only(g, &["key", "title", "sub", "rows"], "group")?;
        let key = g
            .get("key")
            .and_then(Value::as_str)
            .filter(|k| is_key(k))
            .ok_or("group: bad key")?;
        if !group_keys.insert(key.to_owned()) {
            return Err(format!("group {key}: twice"));
        }
        let sub = match g.get("sub") {
            None => false,
            Some(v) => v.as_bool().ok_or("group: sub is not true/false")?,
        };
        if sub {
            sub_groups.insert(key.to_owned());
        }
        let gtitle = text(
            g.get("title").unwrap_or(&Value::Null),
            LABEL_MAX,
            "group title",
        )?;
        let rows = g
            .get("rows")
            .and_then(Value::as_array)
            .ok_or("rows: not a list")?;
        rows_total += rows.len();
        if rows_total > ROWS_MAX {
            return Err(format!("over {ROWS_MAX} rows"));
        }
        let mut out_rows = Vec::new();
        for r in rows {
            let r = r.as_object().ok_or("row: not an object")?;
            only(
                r,
                &["key", "kind", "label", "note", "confirm", "page"],
                "row",
            )?;
            let rkey = r
                .get("key")
                .and_then(Value::as_str)
                .filter(|k| is_key(k))
                .ok_or("row: bad key")?;
            if !row_keys.insert(rkey.to_owned()) {
                return Err(format!("row {rkey}: twice"));
            }
            let kind = r.get("kind").and_then(Value::as_str).unwrap_or("");
            if !matches!(kind, "toggle" | "value" | "page" | "status") {
                return Err(format!("row {rkey}: unknown kind {kind:?}"));
            }
            let mut row = json!({"key": rkey, "kind": kind,
                "label": text(r.get("label").unwrap_or(&Value::Null), LABEL_MAX, "label")?});
            if let Some(n) = r.get("note") {
                row["note"] = json!(text(n, NOTE_MAX, "note")?);
            }
            match (r.get("confirm"), kind) {
                (None, _) => {}
                (Some(c), "toggle") => row["confirm"] = json!(text(c, CONFIRM_MAX, "confirm")?),
                (Some(_), _) => return Err(format!("row {rkey}: confirm on a {kind}")),
            }
            match (r.get("page"), kind) {
                (Some(p), "page") => {
                    let p = p.as_str().filter(|k| is_key(k)).ok_or("page: bad key")?;
                    pages.push(p.to_owned());
                    row["page"] = json!(p);
                }
                (None, "page") => return Err(format!("row {rkey}: page without target")),
                (Some(_), _) => return Err(format!("row {rkey}: page on a {kind}")),
                (None, _) => {}
            }
            out_rows.push(row);
        }
        let mut og = json!({"key": key, "title": gtitle, "rows": out_rows});
        if sub {
            og["sub"] = json!(true);
        }
        out_groups.push(og);
    }
    if let Some(p) = pages.iter().find(|p| !sub_groups.contains(*p)) {
        return Err(format!("page {p}: no such sub group"));
    }
    Ok(json!({"title": title, "groups": out_groups}))
}

fn row_kind<'a>(descriptor: &'a Value, key: &str) -> Option<&'a str> {
    descriptor["groups"]
        .as_array()?
        .iter()
        .filter_map(|g| g["rows"].as_array())
        .flatten()
        .find(|r| r["key"] == key)
        .and_then(|r| r["kind"].as_str())
}

/// 查值：每个键都要在描述里；开关是 true/false，值和状态是文字；`page` 行没有值。
pub fn check_values(v: &Value, descriptor: &Value) -> Result<Value, String> {
    let obj = v.as_object().ok_or("values: not an object")?;
    only(obj, &["rev", "rows"], "values")?;
    let rev = obj
        .get("rev")
        .and_then(Value::as_u64)
        .ok_or("values: no rev")?;
    let rows = obj
        .get("rows")
        .and_then(Value::as_object)
        .ok_or("values: rows not an object")?;
    let mut out = Map::new();
    for (key, r) in rows {
        let kind = row_kind(descriptor, key)
            .ok_or_else(|| format!("value {key}: not in the description"))?;
        let r = r
            .as_object()
            .ok_or_else(|| format!("value {key}: not an object"))?;
        only(r, &["value", "disabled", "note"], "value")?;
        let mut o = json!({});
        match (kind, r.get("value")) {
            ("page", Some(_)) => return Err(format!("value {key}: a page has no value")),
            ("toggle", Some(b)) if b.is_boolean() => o["value"] = b.clone(),
            ("toggle", Some(_)) => return Err(format!("value {key}: not true/false")),
            ("value" | "status", Some(s)) => o["value"] = json!(text(s, NOTE_MAX, "value")?),
            (_, None) => {}
            _ => return Err(format!("value {key}: unexpected")),
        }
        if let Some(d) = r.get("disabled") {
            o["disabled"] = json!(
                d.as_bool()
                    .ok_or_else(|| format!("value {key}: disabled"))?
            );
        }
        if let Some(n) = r.get("note") {
            o["note"] = json!(text(n, NOTE_MAX, "note")?);
        }
        out.insert(key.clone(), o);
    }
    Ok(json!({"rev": rev, "rows": out}))
}

#[cfg(test)]
mod tests {
    use super::super::link::tests::{hello, me, next, plugin, reply, server, wait_connected};
    use super::*;

    fn desc() -> Value {
        json!({"title":"功能","groups":[
            {"key":"switches","title":"功能开关","rows":[
                {"key":"push","kind":"toggle","label":"推送","note":"关掉后只在别处响","confirm":"确定关掉？"},
                {"key":"rec","kind":"page","label":"录音","page":"rec"},
                {"key":"line","kind":"status","label":"线路"}]},
            {"key":"rec","title":"录音","sub":true,"rows":[
                {"key":"rec.on","kind":"toggle","label":"录音"}]}]})
    }

    fn vals(rev: u64, push: bool) -> Value {
        json!({"rev":rev,"rows":{"push":{"value":push},"line":{"value":"正常"},"rec.on":{"value":true,"disabled":true,"note":"推送关闭时不录"}}})
    }

    #[test]
    fn a_good_description_passes_and_keeps_only_known_fields() {
        let d = check_descriptor(&desc()).unwrap();
        assert_eq!(d, desc());
        check_values(&vals(1, true), &d).unwrap();
    }

    #[test]
    fn bad_descriptions_are_refused_whole() {
        type Mutate = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Mutate)> = vec![
            (
                "unknown kind",
                Box::new(|d| d["groups"][0]["rows"][0]["kind"] = json!("html")),
            ),
            (
                "bad key",
                Box::new(|d| d["groups"][0]["rows"][0]["key"] = json!("Push!")),
            ),
            (
                "address",
                Box::new(|d| d["groups"][0]["rows"][0]["note"] = json!("see https://x.y")),
            ),
            (
                "control character",
                Box::new(|d| d["title"] = json!("a\u{1b}[31m")),
            ),
            (
                "long label",
                Box::new(|d| d["groups"][0]["rows"][0]["label"] = json!("字".repeat(41))),
            ),
            (
                "unknown field",
                Box::new(|d| d["groups"][0]["rows"][0]["style"] = json!("red")),
            ),
            (
                "confirm on status",
                Box::new(|d| d["groups"][0]["rows"][2]["confirm"] = json!("x")),
            ),
            (
                "dangling page",
                Box::new(|d| d["groups"][0]["rows"][1]["page"] = json!("nope")),
            ),
            (
                "duplicate row",
                Box::new(|d| d["groups"][1]["rows"][0]["key"] = json!("push")),
            ),
            (
                "too many groups",
                Box::new(|d| {
                    d["groups"] = json!(
                        (0..9)
                            .map(|i| json!({"key":format!("g{i}"),"title":"t","rows":[]}))
                            .collect::<Vec<_>>()
                    )
                }),
            ),
            (
                "too many rows",
                Box::new(|d| {
                    d["groups"][0]["rows"] = json!(
                        (0..33)
                            .map(|i| json!({"key":format!("r{i}"),"kind":"status","label":"l"}))
                            .collect::<Vec<_>>()
                    );
                    d["groups"].as_array_mut().unwrap().truncate(1);
                }),
            ),
            (
                "over 16 KB",
                Box::new(|d| {
                    d["groups"][0]["rows"] = json!((0..32).map(|i| json!({"key":format!("r{i}"),"kind":"status","label":"l","note":"字".repeat(200)})).collect::<Vec<_>>());
                    d["groups"].as_array_mut().unwrap().truncate(1);
                }),
            ),
        ];
        for (what, f) in cases {
            let mut d = desc();
            f(&mut d);
            assert!(check_descriptor(&d).is_err(), "{what} let through");
        }
    }


    #[test]
    fn markup_like_text_is_allowed_here_the_web_page_escapes_it() {
        // core 查结构，不禁字符；`<script>`、`$`、`@`、`data-` 由网页只放文本节点来挡（R29 两层）
        let mut d = desc();
        d["groups"][0]["rows"][0]["note"] =
            json!("<script>alert(1)</script> $x @post('/a') data-on-click");
        check_descriptor(&d).unwrap();
    }

    #[test]
    fn values_must_match_the_description() {
        let d = check_descriptor(&desc()).unwrap();
        for (what, v) in [
            (
                "unknown key",
                json!({"rev":1,"rows":{"zzz":{"value":true}}}),
            ),
            (
                "toggle not bool",
                json!({"rev":1,"rows":{"push":{"value":"on"}}}),
            ),
            (
                "page with value",
                json!({"rev":1,"rows":{"rec":{"value":true}}}),
            ),
            ("no rev", json!({"rows":{}})),
            (
                "extra field",
                json!({"rev":1,"rows":{"push":{"value":true,"color":"red"}}}),
            ),
        ] {
            assert!(check_values(&v, &d).is_err(), "{what} let through");
        }
    }

    /// 假插件：回描述、读值、写值（按版本比对）。
    async fn fake_plugin(
        mut r: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
        mut w: tokio::net::unix::OwnedWriteHalf,
        seen: std::sync::Arc<Mutex<Vec<Value>>>,
    ) {
        let mut rev = 1u64;
        let mut push = true;
        next(&mut r).await; // hello ok
        while let Some(req) = next(&mut r).await {
            seen.lock().unwrap().push(req.clone());
            let id = req["id"].clone();
            let res = match req["op"].as_str().unwrap() {
                "settings.describe" => json!({"t":"res","id":id,"ok":true,"result":desc()}),
                "settings.read" => json!({"t":"res","id":id,"ok":true,"result":vals(rev, push)}),
                "settings.write" if req["params"]["rev"] != rev => {
                    json!({"t":"res","id":id,"ok":false,"error":{"code":"stale_revision"}})
                }
                "settings.write" => {
                    push = req["params"]["value"].as_bool().unwrap();
                    rev += 1;
                    json!({"t":"res","id":id,"ok":true,"result":vals(rev, push)})
                }
                _ => json!({"t":"res","id":id,"ok":false,"error":{"code":"unknown"}}),
            };
            reply(&mut w, res).await;
        }
    }

    fn fresh() -> Mutex<Cache> {
        Mutex::new(Cache::default())
    }

    const W: Duration = Duration::from_millis(500);

    #[tokio::test]
    async fn get_set_and_read_back_through_the_plugin() {
        let (hub, path, _d) = server(me()).await;
        let (r, w) = plugin(&path, hello(&["settings"])).await;
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        tokio::spawn(fake_plugin(r, w, seen.clone()));
        wait_connected(&hub).await;
        let cache = fresh();
        let v = get_with(&hub, &cache, W).await.unwrap();
        assert_eq!(v["online"], true);
        assert_eq!(v["rev"], 1);
        assert_eq!(v["rows"]["push"]["value"], true);
        let mut extra = Map::new();
        extra.insert("source".into(), json!("web"));
        let v = set_with(
            &hub,
            &cache,
            &json!({"key":"push","value":false,"rev":1}),
            extra.clone(),
            W,
        )
        .await
        .unwrap();
        assert_eq!(v["rev"], 2);
        assert_eq!(v["rows"]["push"]["value"], false);
        // 来源原样转给插件（稽核要分出网页和 App）
        let write = seen
            .lock()
            .unwrap()
            .iter()
            .find(|r| r["op"] == "settings.write")
            .cloned()
            .unwrap();
        assert_eq!(write["source"], "web");
        assert!(write["deadline_ms"].as_u64().is_some());
        // 用旧版本再写：被拒，值不变
        let e = set_with(
            &hub,
            &cache,
            &json!({"key":"push","value":true,"rev":1}),
            extra.clone(),
            W,
        )
        .await
        .unwrap_err();
        assert_eq!((e.status, e.code), (409, "stale_revision"));
        let v = get_with(&hub, &cache, W).await.unwrap();
        assert_eq!(v["rows"]["push"]["value"], false);
        // 只有开关能写
        let e = set_with(
            &hub,
            &cache,
            &json!({"key":"line","value":true,"rev":2}),
            extra,
            W,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "invalid_parameter");
    }

    #[tokio::test]
    async fn offline_reads_show_the_last_values_and_writes_are_refused() {
        let (hub, path, _d) = server(me()).await;
        let cache = fresh();
        // core 刚起、插件不在、没有缓存：只说没在运行
        let v = get_with(&hub, &cache, W).await.unwrap();
        assert_eq!(v, json!({"installed":true,"online":false}));
        let (r, w) = plugin(&path, hello(&["settings"])).await;
        let task = tokio::spawn(fake_plugin(r, w, Default::default()));
        wait_connected(&hub).await;
        get_with(&hub, &cache, W).await.unwrap();
        task.abort();
        for _ in 0..200 {
            if hub.current().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let v = get_with(&hub, &cache, W).await.unwrap();
        assert_eq!(v["online"], false);
        assert_eq!(v["rows"]["push"]["value"], true);
        assert!(v["confirmed_at"].as_u64().is_some());
        let e = set_with(
            &hub,
            &cache,
            &json!({"key":"push","value":false,"rev":1}),
            Map::new(),
            W,
        )
        .await
        .unwrap_err();
        assert_eq!((e.status, e.code), (503, "plugin_offline"));
    }

    #[tokio::test]
    async fn a_silent_plugin_makes_a_write_result_unknown() {
        let (hub, path, _d) = server(me()).await;
        let cache = fresh();
        let (mut r, mut w) = plugin(&path, hello(&["settings"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        // 先答描述和读值，写入时不答
        let answer = tokio::spawn(async move {
            for _ in 0..2 {
                let req = next(&mut r).await.unwrap();
                let res = if req["op"] == "settings.describe" {
                    desc()
                } else {
                    vals(1, true)
                };
                reply(
                    &mut w,
                    json!({"t":"res","id":req["id"],"ok":true,"result":res}),
                )
                .await;
            }
            next(&mut r).await; // the write, never answered
            (r, w)
        });
        let e = set_with(
            &hub,
            &cache,
            &json!({"key":"push","value":false,"rev":1}),
            Map::new(),
            W,
        )
        .await
        .unwrap_err();
        assert_eq!((e.status, e.code), (504, "result_unknown"));
        drop(answer);
    }

    #[tokio::test]
    async fn an_invalid_description_is_not_cached() {
        let (hub, path, _d) = server(me()).await;
        let cache = fresh();
        let (mut r, mut w) = plugin(&path, hello(&["settings"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        tokio::spawn(async move {
            while let Some(req) = next(&mut r).await {
                let mut d = desc();
                d["groups"][0]["rows"][0]["kind"] = json!("script");
                reply(
                    &mut w,
                    json!({"t":"res","id":req["id"],"ok":true,"result":d}),
                )
                .await;
            }
        });
        let e = get_with(&hub, &cache, W).await.unwrap_err();
        assert_eq!((e.status, e.code), (502, "plugin_invalid"));
        assert!(cache.lock().unwrap().descriptor.is_none());
    }

    #[tokio::test]
    async fn not_installed_and_no_settings_hide_the_page() {
        let hub = Hub::default();
        assert_eq!(
            get_with(&hub, &fresh(), W).await.unwrap(),
            json!({"installed": false})
        );
        let (hub, path, _d) = server(me()).await;
        let (mut r, _w) = plugin(&path, hello(&["sms"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        assert_eq!(
            get_with(&hub, &fresh(), W).await.unwrap(),
            json!({"installed": true, "online": true, "settings": false})
        );
    }
}
