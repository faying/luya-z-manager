//! 插件的显示请求（契约 v0：`display.show` / `display.dismiss`，luyaz-phone 设计 R1-4、DR12）。
//!
//! 插件要在触屏上弹一张卡片、等人按。core 不认识卡片的业务，只管：
//! - 触屏登记过它会显示卡片（`state.set_interval` 带 `caps:["display"]`，跟着登记过期、交还屏幕时拿掉）；
//!   没登记（触屏没在跑、在原厂界面、版本太旧）就回 `screen_unavailable`，不抢屏幕；
//! - 同一时间只有一张；截止时间在 core 这边算（单调时钟），到了没人按就送 `timeout`；
//! - 触屏的回答（`display.answer`，只认本机连线上的 `source:"screen"`）以事件送回插件：
//!   `{"t":"ev","ev":"display","id","answer":"confirm"|"cancel"|"timeout"}`；
//! - 插件断线：卡片收起（没人等回答了）。
//!
//! 触屏经 `display` 块看到卡片：`{"card":{id,kind,code,left_s,answered}|null,"said":{id,result}|null}`；
//! `said` 是收起后底部短暂的一句（「已配对 iPhone」「配对已取消」），留 [`SAID_MS`]。

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Notify;

use super::mono_ms;

/// 认得的卡片种类。
const KINDS: [&str; 1] = ["pair"];
/// 卡片最短、最长留多久（秒）。
const SECONDS_MIN: u64 = 10;
const SECONDS_MAX: u64 = 300;
/// 按了「确认」以后，插件还要等手机走完（luyaz-phone 等 hello 到卡片时间 + 5 秒）；
/// 超过截止时间这么久还没 dismiss，就当插件忘了，收起。
const ANSWERED_GRACE_MS: u64 = 30_000;
/// 收起后那一句留多久。
pub const SAID_MS: u64 = 4_000;
/// 卡片 id 最长几个字元。
const ID_MAX: usize = 64;

#[derive(Clone, Debug, PartialEq)]
struct Card {
    id: String,
    kind: String,
    code: String,
    until_ms: u64,
    /// 卡片给的秒数：按了「确认」以后插件还要等手机这么久（luyaz-phone 等 hello = 卡片时间 + 5 秒）
    seconds: u64,
    answered: bool,
    /// 发出这张卡片的插件连线；换了连线就收起。
    link: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
struct Said {
    id: String,
    result: &'static str,
    until_ms: u64,
}

#[derive(Default)]
struct State {
    card: Option<Card>,
    said: Option<Said>,
    /// 触屏的显示登记到什么时候（单调毫秒）；None = 没登记。
    touch_until: Option<u64>,
}

fn state() -> MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// 卡片变了（出现、回答、收起）：叫醒 [`spawn`] 的任务马上交块，不等下一轮。
fn changed() -> &'static Notify {
    static N: OnceLock<Notify> = OnceLock::new();
    N.get_or_init(Notify::new)
}

type Err = (String, String);

fn err(code: &str, message: impl Into<String>) -> Err {
    (code.into(), message.into())
}

/// 触屏的登记（`state.set_interval` 的 client `touch`）：`caps` 里有 `display` 就记到 ttl 结束；
/// 交还（release）或没带就拿掉。拿掉时有卡片：收起，并告诉插件「取消」（不抢屏幕）。
pub fn touch_caps(display: bool, ttl: Duration) {
    let mut s = state();
    s.touch_until = display.then(|| mono_ms() + ttl.as_millis() as u64);
    if !display {
        drop(s);
        withdraw();
    }
}

fn touch_ok(s: &State, now: u64) -> bool {
    s.touch_until.is_some_and(|u| u > now)
}

/// `display.show {id, kind, code, seconds}`：马上回 `{}` 或错误。同一个 id 再送一次算成功。
pub fn show(params: &Value, link: Option<u64>) -> Result<Value, Err> {
    let id = params["id"].as_str().unwrap_or("");
    // 触屏把 id 原样写进它回答的 JSON：只收字母、数字、`-`、`_`
    if id.is_empty()
        || id.len() > ID_MAX
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(err("invalid_parameter", "id: 1 to 64 of A-Z a-z 0-9 - _"));
    }
    let kind = params["kind"].as_str().unwrap_or("");
    if !KINDS.contains(&kind) {
        return Err(err("invalid_parameter", "kind: pair"));
    }
    let code = params["code"].as_str().unwrap_or("");
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err("invalid_parameter", "code: 6 digits"));
    }
    let Some(seconds) = params["seconds"]
        .as_u64()
        .filter(|s| (SECONDS_MIN..=SECONDS_MAX).contains(s))
    else {
        return Err(err("invalid_parameter", "seconds: 10 to 300"));
    };
    let now = mono_ms();
    let mut s = state();
    if !touch_ok(&s, now) {
        return Err(err(
            "screen_unavailable",
            "the touch screen is not showing cards (stock UI, off, or too old)",
        ));
    }
    if let Some(c) = &s.card {
        if c.id == id {
            return Ok(json!({}));
        }
        return Err(err("busy", "another card is up"));
    }
    s.card = Some(Card {
        id: id.into(),
        kind: kind.into(),
        code: code.into(),
        until_ms: now + seconds * 1000,
        seconds,
        answered: false,
        link,
    });
    s.said = None;
    drop(s);
    changed().notify_one();
    Ok(json!({}))
}

/// `display.dismiss {id, result}`：收起这张卡片，底部短暂说一句。不是现在这张（已经逾时、被取消）
/// 也回 `{}`：插件照流程送就好。
pub fn dismiss(params: &Value) -> Result<Value, Err> {
    let id = params["id"].as_str().unwrap_or("");
    let result = match params["result"].as_str() {
        Some("done") => "done",
        Some("cancelled") => "cancelled",
        _ => return Err(err("invalid_parameter", "result: done or cancelled")),
    };
    let now = mono_ms();
    let mut s = state();
    // 现在这张，或刚刚收起的那张（取消、逾时以后插件补送的 cancelled）：说一句
    let current = s.card.as_ref().is_some_and(|c| c.id == id);
    let recent = s.said.as_ref().is_some_and(|w| w.id == id);
    if current || recent {
        s.card = None;
        s.said = Some(Said {
            id: id.into(),
            result,
            until_ms: now + SAID_MS,
        });
        drop(s);
        changed().notify_one();
    }
    Ok(json!({}))
}

/// 触屏的回答。只有现在这张、还没回答过的才算；「确认」后卡片留着（插件还在等手机），
/// 「取消」马上收起。回 Err 时什么都没送。
pub fn answer(id: &str, answer: &str) -> Result<(), &'static str> {
    let confirm = match answer {
        "confirm" => true,
        "cancel" => false,
        _ => return Err("answer: confirm or cancel"),
    };
    let now = mono_ms();
    let mut s = state();
    let Some(c) = s.card.as_mut().filter(|c| c.id == id) else {
        return Err("no such card");
    };
    if c.answered {
        return Err("already answered");
    }
    if c.until_ms <= now {
        return Err("too late");
    }
    if confirm {
        c.answered = true;
        // 插件从现在起等手机（卡片时间 + 5 秒）：卡片留到那时再加 ANSWERED_GRACE_MS
        c.until_ms = c.until_ms.max(now + c.seconds * 1000 + 5_000);
    } else {
        // 取消：卡片收起，底部说「配对已取消」（插件随后的 dismiss cancelled 不再重复）
        s.card = None;
        s.said = Some(Said {
            id: id.into(),
            result: "cancelled",
            until_ms: now + SAID_MS,
        });
    }
    drop(s);
    super::link::hub().event("display", json!({"id": id, "answer": answer}));
    changed().notify_one();
    Ok(())
}

/// 截止时间、插件断线、触屏登记过期。每轮和卡片变化时调用。
fn tick(now: u64, link: Option<u64>) {
    let mut s = state();
    if s.said.as_ref().is_some_and(|w| w.until_ms <= now) {
        s.said = None;
    }
    let Some(c) = s.card.clone() else {
        return;
    };
    let gone = c.link.is_some() && c.link != link;
    if gone {
        s.card = None;
        return;
    }
    if !touch_ok(&s, now) {
        drop(s);
        withdraw();
        return;
    }
    if !c.answered && c.until_ms <= now {
        s.card = None;
        s.said = Some(Said {
            id: c.id.clone(),
            result: "cancelled",
            until_ms: now + SAID_MS,
        });
        drop(s);
        super::link::hub().event("display", json!({"id": c.id, "answer": "timeout"}));
        return;
    }
    if c.answered && c.until_ms + ANSWERED_GRACE_MS <= now {
        s.card = None;
    }
}

/// 屏幕不再显示卡片（交还原厂界面、登记过期）：卡片收起，插件收到「取消」。
fn withdraw() {
    let mut s = state();
    let Some(c) = s.card.take() else {
        return;
    };
    drop(s);
    if !c.answered {
        super::link::hub().event("display", json!({"id": c.id, "answer": "cancel"}));
    }
    changed().notify_one();
}

/// `display` 块的内容。
pub fn block_now() -> Value {
    let now = mono_ms();
    let s = state();
    let card = s.card.as_ref().map(|c| {
        json!({"id": c.id, "kind": c.kind, "code": c.code,
               "left_s": c.until_ms.saturating_sub(now).div_ceil(1000), "answered": c.answered})
    });
    let said = s
        .said
        .as_ref()
        .map(|w| json!({"id": w.id, "result": w.result}));
    json!({"card": card, "said": said})
}

fn link_now() -> Option<u64> {
    super::link::hub().current().map(|c| c.generation)
}

/// 每轮交一次 `display` 块（派生块不交就过期），先做截止时间。
pub fn block_round() -> Value {
    tick(mono_ms(), link_now());
    block_now()
}

/// 卡片变化时马上交块；有卡片或那一句还在时每秒交一次（倒数、截止时间）。
pub fn spawn(hub: Arc<crate::block::Hub>) {
    tokio::spawn(async move {
        loop {
            let busy = {
                let s = state();
                s.card.is_some() || s.said.is_some()
            };
            if busy {
                let _ = tokio::time::timeout(Duration::from_secs(1), changed().notified()).await;
            } else {
                changed().notified().await;
            }
            hub.record("display", Ok(block_round()), tokio::time::Instant::now());
        }
    });
}

/// 测试用：清掉全部状态。
#[cfg(test)]
pub(crate) fn reset() {
    *state() = State::default();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态是全进程一份：这几个测试排队跑。
    fn serial() -> MutexGuard<'static, ()> {
        static L: Mutex<()> = Mutex::new(());
        let g = L.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        g
    }

    fn card(id: &str) -> Value {
        json!({"id": id, "kind": "pair", "code": "482913", "seconds": 120})
    }

    #[test]
    fn without_the_touch_screen_a_card_is_refused() {
        let _g = serial();
        assert_eq!(show(&card("a"), None).unwrap_err().0, "screen_unavailable");
        touch_caps(true, Duration::from_secs(180));
        assert_eq!(show(&card("a"), None), Ok(json!({})));
        assert_eq!(block_now()["card"]["code"], "482913");
        assert_eq!(block_now()["card"]["left_s"], 120);
    }

    #[test]
    fn one_card_at_a_time_and_the_same_id_again_is_fine() {
        let _g = serial();
        touch_caps(true, Duration::from_secs(180));
        show(&card("a"), None).unwrap();
        assert_eq!(show(&card("a"), None), Ok(json!({})));
        assert_eq!(show(&card("b"), None).unwrap_err().0, "busy");
    }

    #[test]
    fn bad_parameters_are_refused() {
        let _g = serial();
        touch_caps(true, Duration::from_secs(180));
        for p in [
            json!({"id": "", "kind": "pair", "code": "482913", "seconds": 120}),
            json!({"id": "a b", "kind": "pair", "code": "482913", "seconds": 120}),
            json!({"id": "a\"b", "kind": "pair", "code": "482913", "seconds": 120}),
            json!({"id": 7, "kind": "pair", "code": "482913", "seconds": 120}),
            json!({"id": "a", "kind": "other", "code": "482913", "seconds": 120}),
            json!({"id": "a", "kind": "pair", "code": "48291", "seconds": 120}),
            json!({"id": "a", "kind": "pair", "code": "48291x", "seconds": 120}),
            json!({"id": "a", "kind": "pair", "code": "482913", "seconds": 5}),
            json!({"id": "a", "kind": "pair", "code": "482913"}),
        ] {
            assert_eq!(show(&p, None).unwrap_err().0, "invalid_parameter", "{p}");
        }
        assert!(block_now()["card"].is_null());
    }

    #[test]
    fn confirm_keeps_the_card_cancel_closes_it_and_only_once() {
        let _g = serial();
        touch_caps(true, Duration::from_secs(180));
        show(&card("a"), None).unwrap();
        assert_eq!(answer("b", "confirm"), Err("no such card"));
        assert_eq!(answer("a", "maybe"), Err("answer: confirm or cancel"));
        answer("a", "confirm").unwrap();
        assert_eq!(block_now()["card"]["answered"], true);
        assert_eq!(answer("a", "cancel"), Err("already answered"));
        dismiss(&json!({"id": "a", "result": "done"})).unwrap();
        assert!(block_now()["card"].is_null());
        assert_eq!(block_now()["said"]["result"], "done");

        show(&card("c"), None).unwrap();
        answer("c", "cancel").unwrap();
        assert!(block_now()["card"].is_null());
        assert_eq!(block_now()["said"]["result"], "cancelled");
        // 插件随后补送的 dismiss 不出错、不变
        dismiss(&json!({"id": "c", "result": "cancelled"})).unwrap();
        assert_eq!(block_now()["said"]["id"], "c");
        assert!(dismiss(&json!({"id": "c", "result": "x"})).is_err());
    }

    #[test]
    fn a_dismiss_for_another_card_changes_nothing() {
        let _g = serial();
        touch_caps(true, Duration::from_secs(180));
        show(&card("a"), None).unwrap();
        dismiss(&json!({"id": "zz", "result": "done"})).unwrap();
        assert_eq!(block_now()["card"]["id"], "a");
        assert!(block_now()["said"].is_null());
    }

    #[test]
    fn the_deadline_closes_an_unanswered_card() {
        let _g = serial();
        touch_caps(true, Duration::from_secs(180));
        show(
            &json!({"id": "a", "kind": "pair", "code": "482913", "seconds": 10}),
            None,
        )
        .unwrap();
        let until = state().card.as_ref().unwrap().until_ms;
        tick(until - 1, None);
        assert!(!block_now()["card"].is_null());
        tick(until, None);
        assert!(block_now()["card"].is_null());
        assert_eq!(block_now()["said"]["result"], "cancelled");
        assert_eq!(answer("a", "confirm"), Err("no such card"));
        // 那一句过一会自己消失
        tick(until + SAID_MS + 1, None);
        assert!(block_now()["said"].is_null());
    }

    #[test]
    fn an_answered_card_waits_for_the_plugin_then_gives_up() {
        let _g = serial();
        touch_caps(true, Duration::from_secs(600));
        show(
            &json!({"id": "a", "kind": "pair", "code": "482913", "seconds": 10}),
            None,
        )
        .unwrap();
        answer("a", "confirm").unwrap();
        let until = state().card.as_ref().unwrap().until_ms;
        // 从按下算起，插件还要等手机 10 + 5 秒
        assert!(until >= mono_ms() + 14_000);
        tick(until + 1, None);
        assert!(!block_now()["card"].is_null());
        tick(until + ANSWERED_GRACE_MS, None);
        assert!(block_now()["card"].is_null());
    }

    #[test]
    fn a_new_plugin_link_or_the_screen_handed_over_closes_the_card() {
        let _g = serial();
        touch_caps(true, Duration::from_secs(180));
        show(&card("a"), Some(1)).unwrap();
        tick(mono_ms(), Some(1));
        assert!(!block_now()["card"].is_null());
        tick(mono_ms(), Some(2));
        assert!(block_now()["card"].is_null());

        show(&card("b"), None).unwrap();
        touch_caps(false, Duration::ZERO);
        assert!(block_now()["card"].is_null());
        assert_eq!(show(&card("c"), None).unwrap_err().0, "screen_unavailable");
    }
}
