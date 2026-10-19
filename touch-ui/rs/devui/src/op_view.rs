//! datad's E4 transaction view (src/op_view.c): the /v2/screen "op" object, the change log
//! (journal.list) and who last changed a setting, read into plain fields. Texts come in both
//! languages (<key>_zh / <key>_en); the screen's language picks. Field sizes as in C (a longer
//! value is cut where C cuts it).
// SPDX-License-Identifier: MIT

use crate::json;
use crate::lang;

pub const MARK_NONE: i32 = 0;
pub const MARK_OK: i32 = 1;
pub const MARK_WARN: i32 = 2;
pub const MARK_BAD: i32 = 3;
pub const STAY_LIVE: i32 = 0;
pub const STAY_BRIEF: i32 = 1;
pub const STAY_STICKY: i32 = 2;
pub const STAY_ALERT: i32 = 3;
pub const STAY_NONE: i32 = 4;
pub const STEPS: usize = 3;
pub const LOG_MAX: usize = 20;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpItem {
    pub have: bool,
    pub op_id: String,
    pub action: String,
    pub item: String,
    pub phase: String,
    pub reason: String,
    pub mark: i32,
    pub stay: i32,
    pub say: String,
    pub next: String,
    pub note: String,
    pub what: String,
    pub source: String,
    pub old_v: String,
    pub target: String,
    pub rollback_to: String,
    pub readback: String,
    pub rollback_to_raw: String,
    pub revert_label: String,
    pub keep_label: String,
    pub can_revert: bool,
    pub can_keep: bool,
    /// -1 when not counting
    pub remaining_ms: i64,
    pub step: [String; STEPS],
    pub step_done: [bool; STEPS],
    /// -1: no undo (in progress)
    pub undo_ok: i32,
    pub undo_label: String,
    pub undo_why: String,
    pub acked: bool,
    pub needs_ack: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpView {
    pub rollback_enabled: bool,
    pub active: OpItem,
    pub last: OpItem,
    /// "rollback_on": the one-time 自动退回已打开 (DD18); "" none
    pub notice: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpLog {
    pub what: String,
    pub change: String,
    pub result: String,
    /// 10-03 14:32 (device clock = local time)
    pub when: String,
    pub source: String,
    pub mark: i32,
    pub undo_have: bool,
    pub undo_ok: bool,
    pub undo_label: String,
    pub undo_why: String,
    pub undo_action: String,
    pub undo_params: String,
    pub op_id: String,
}

/// 「上次改动 10-03 14:32 · 网页 ›」 under a setting (DD5).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpOwner {
    pub have: bool,
    pub when: String,
    pub source: String,
    pub op_id: String,
}

fn s(obj: &[u8], key: &str, cap: usize) -> String {
    match json::get(obj, key, cap) {
        Some(v) if v != b"null" => String::from_utf8_lossy(&v).into_owned(),
        _ => String::new(),
    }
}

/// datad sends every sentence twice: <key>_zh and <key>_en
fn text(obj: &[u8], key: &str, cap: usize) -> String {
    if lang::is_en() {
        let v = s(obj, &format!("{key}_en"), cap);
        if !v.is_empty() {
            return v;
        }
    }
    s(obj, &format!("{key}_zh"), cap)
}

fn flag(obj: &[u8], key: &str) -> bool {
    json::get(obj, key, 8).is_some_and(|v| v == b"true")
}

fn mark_of(v: &str) -> i32 {
    match v {
        "ok" => MARK_OK,
        "warn" => MARK_WARN,
        "bad" => MARK_BAD,
        _ => MARK_NONE,
    }
}

/// phase is a final one (confirmed, unverified, rolled_back, …)
pub fn phase_final(p: &str) -> bool {
    !p.is_empty() && !matches!(p, "accepted" | "applying" | "verifying" | "rolling_back")
}

fn item(o: &[u8]) -> OpItem {
    let mut it = OpItem::default();
    if o.first() != Some(&b'{') {
        return it;
    }
    it.op_id = s(o, "op_id", 72);
    it.phase = s(o, "phase", 16);
    if it.op_id.is_empty() || it.phase.is_empty() {
        return it;
    }
    it.have = true;
    it.action = s(o, "action", 40);
    it.item = s(o, "item", 32);
    it.reason = s(o, "reason", 20);
    it.mark = mark_of(&s(o, "mark", 24));
    it.stay = match s(o, "stay", 24).as_str() {
        "brief" => STAY_BRIEF,
        "sticky" => STAY_STICKY,
        "alert" => STAY_ALERT,
        "none" => STAY_NONE,
        _ => STAY_LIVE,
    };
    it.say = text(o, "say", 96);
    it.next = text(o, "next", 96);
    it.note = text(o, "note", 48);
    it.what = text(o, "what", 32);
    it.source = text(o, "source", 24);
    it.old_v = text(o, "old", 48);
    it.target = text(o, "target", 48);
    it.rollback_to = text(o, "rollback_to", 48);
    it.readback = text(o, "readback", 48);
    it.rollback_to_raw = s(o, "rollback_to", 40);
    it.revert_label = text(o, "revert_label", 64);
    it.keep_label = text(o, "keep_label", 64);
    it.can_revert = flag(o, "can_revert");
    it.can_keep = flag(o, "can_keep");
    it.remaining_ms = json::get_int(o, "remaining_ms", -1);
    it.acked = flag(o, "acked");
    it.needs_ack = flag(o, "needs_ack");
    if let Some(steps) = json::get(o, "steps", 1024)
        && steps.first() == Some(&b'[')
    {
        let mut p = 0;
        for i in 0..STEPS {
            let Some((st, np)) = json::arr_next(&steps, p, 256) else { break };
            p = np;
            it.step[i] = s(&st, if lang::is_en() { "en" } else { "zh" }, 32);
            it.step_done[i] = flag(&st, "done");
        }
    }
    it.undo_ok = -1;
    if let Some(undo) = json::get(o, "undo", 512)
        && undo.first() == Some(&b'{')
    {
        it.undo_ok = flag(&undo, "ok") as i32;
        it.undo_label = text(&undo, "label", 16);
        it.undo_why = text(&undo, "why", 48);
    }
    it
}

/// The "op" object text ("" or garbage: None).
pub fn parse(j: &str) -> Option<OpView> {
    let j = j.as_bytes();
    if j.first() != Some(&b'{') {
        return None;
    }
    let mut v = OpView { rollback_enabled: flag(j, "rollback_enabled"), notice: s(j, "notice", 24), ..Default::default() };
    if let Some(a) = json::get(j, "active", 4096) {
        v.active = item(&a);
    }
    if let Some(a) = json::get(j, "last", 4096) {
        v.last = item(&a);
    }
    Some(v)
}

/// line with "{t}" replaced by m:ss of ms (rounded up, at most 99:00).
pub fn fill_clock(line: &str, ms: i64) -> String {
    let s = if ms > 0 { (ms + 999) / 1000 } else { 0 }.min(99 * 60);
    let t = format!("{}:{:02}", s / 60, s % 60);
    match line.find("{t}") {
        Some(i) => format!("{}{t}{}", &line[..i], &line[i + 3..]),
        None => line.to_string(),
    }
}

/// "2026-10-03 14:32:00" → "10-03 14:32"
fn when_of(t: &str) -> String {
    let b = t.as_bytes();
    format!("{} {}", String::from_utf8_lossy(&b[5..10]), String::from_utf8_lossy(&b[11..16]))
}

/// The whole /control reply of journal.list: its rows (hidden ones left out), at most max;
/// None when it is not one.
pub fn log_parse(reply: &str, max: usize) -> Option<Vec<OpLog>> {
    let res = json::get(reply.as_bytes(), "result", 49152)?;
    if res.first() != Some(&b'{') {
        return None;
    }
    let arr = json::get(&res, "entries", 49152).filter(|a| a.first() == Some(&b'['))?;
    let mut out = Vec::new();
    let mut p = 0;
    while out.len() < max {
        let Some((e, np)) = json::arr_next(&arr, p, 4096) else { break };
        p = np;
        if e.first() != Some(&b'{') || flag(&e, "hide") {
            continue;
        }
        let mut o = OpLog {
            what: text(&e, "what", 40),
            change: text(&e, "change", 96),
            result: text(&e, "result", 96),
            source: text(&e, "source", 24),
            op_id: s(&e, "op_id", 72),
            mark: mark_of(&s(&e, "mark", 16)),
            ..Default::default()
        };
        let t = s(&e, "t", 32);
        if t.len() >= 16 {
            o.when = when_of(&t);
        }
        if let Some(u) = json::get(&e, "undo_view", 1024)
            && u.first() == Some(&b'{')
        {
            o.undo_have = true;
            o.undo_ok = flag(&u, "ok");
            o.undo_label = text(&u, "label", 16);
            o.undo_why = text(&u, "why", 48);
            if let Some(req) = json::get(&u, "request", 512)
                && req.first() == Some(&b'{')
            {
                o.undo_action = s(&req, "action", 40);
                o.undo_params = match json::get(&req, "params", 160) {
                    Some(p) if p.first() == Some(&b'{') => String::from_utf8_lossy(&p).into_owned(),
                    _ => String::new(),
                };
            }
            if o.undo_action.is_empty() || o.undo_params.is_empty() {
                o.undo_ok = false;
            }
        }
        out.push(o);
    }
    Some(out)
}

/// DD14 display names; datad's journal rows carry them, owners don't
const SRC: [(&str, &str, &str); 8] = [
    ("screen", "触屏", "Screen"),
    ("legacy", "触屏", "Screen"),
    ("web", "网页", "Web"),
    ("scenario", "情景", "Scene"),
    ("scheduler", "定时任务", "Schedule"),
    ("auto", "自动", "Auto"),
    ("guard", "自动恢复", "Auto-recovery"),
    ("mcp", "MCP", "MCP"),
];

/// Who last wrote `item` (have = false when nobody is on record), None when the reply is not
/// a journal.list answer.
pub fn owner_parse(reply: &str, item: &str) -> Option<OpOwner> {
    let mut o = OpOwner::default();
    let res = json::get(reply.as_bytes(), "result", 49152).filter(|r| r.first() == Some(&b'{'))?;
    let Some(own) = json::get(&res, "owners", 4096).filter(|v| v.first() == Some(&b'{')) else { return Some(o) };
    let Some(it) = json::get(&own, item, 1024).filter(|v| v.first() == Some(&b'{')) else { return Some(o) };
    let t = s(&it, "t", 32);
    if t.len() < 16 {
        return Some(o);
    }
    o.when = when_of(&t);
    let src = s(&it, "source", 24);
    o.source = SRC.iter().find(|k| k.0 == src).map_or(src.clone(), |k| (if lang::is_en() { k.2 } else { k.1 }).to_string());
    o.op_id = s(&it, "op_id", 72);
    o.have = true;
    Some(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    // tests/op_view_test.c: active verifying with rollback on; last rolled back, not acked
    const OP: &str = concat!(
        "{\"rollback_enabled\":true,",
        "\"active\":{\"action\":\"network.set_mode\",\"can_keep\":true,\"can_revert\":true,\"data_ok\":false,",
        "\"ever_matched\":true,\"item\":\"network.mode\",\"keep_label_en\":\"Keep 4G only\",\"keep_label_zh\":\"保留只用 4G\",",
        "\"mark\":null,\"next_en\":\"Back to Auto in {t} if no data\",\"next_zh\":\"{t} 后没通就退回到自动\",",
        "\"note_en\":null,\"note_zh\":null,\"old\":\"WL_AND_5G\",\"old_en\":\"Auto\",\"old_zh\":\"自动\",\"op_id\":\"screen-12-3\",",
        "\"phase\":\"verifying\",\"readback_en\":\"4G only\",\"readback_zh\":\"只用 4G\",\"reason\":null,\"remaining_ms\":102000,",
        "\"revert_label_en\":\"Revert to Auto\",\"revert_label_zh\":\"退回自动\",\"rollback_enabled\":true,",
        "\"rollback_reason\":null,\"rollback_to\":\"WL_AND_5G\",\"rollback_to_en\":\"Auto\",\"rollback_to_zh\":\"自动\",",
        "\"say_en\":\"Checking\",\"say_zh\":\"正在确认\",\"source\":\"web\",\"source_en\":\"Web\",\"source_zh\":\"网页\",",
        "\"stay\":\"live\",\"steps\":[{\"done\":true,\"en\":\"Setting applied\",\"key\":\"applied\",\"zh\":\"设置已生效\"},",
        "{\"done\":true,\"en\":\"Registered\",\"key\":\"registered\",\"zh\":\"已注册\"},",
        "{\"done\":false,\"en\":\"Data\",\"key\":\"data\",\"zh\":\"数据\"}],\"target\":\"Only_LTE\",\"target_en\":\"4G only\",",
        "\"target_zh\":\"只用 4G\",\"undo\":null,\"what_en\":\"Network mode\",\"what_zh\":\"制式\"},",
        "\"last\":{\"acked\":false,\"action\":\"network.set_mode\",\"can_keep\":false,\"can_revert\":false,\"item\":\"network.mode\",",
        "\"mark\":\"warn\",\"needs_ack\":true,\"next_en\":null,\"next_zh\":null,\"note_en\":null,\"note_zh\":null,",
        "\"op_id\":\"web-7\",\"phase\":\"rolled_back\",\"reason\":\"timeout\",\"remaining_ms\":null,",
        "\"say_en\":\"No data · back to Auto\",\"say_zh\":\"没通 · 已退回自动\",\"stay\":\"sticky\",",
        "\"steps\":[{\"done\":true,\"en\":\"Setting applied\",\"key\":\"applied\",\"zh\":\"设置已生效\"},",
        "{\"done\":true,\"en\":\"Registered\",\"key\":\"registered\",\"zh\":\"已注册\"},",
        "{\"done\":true,\"en\":\"Data\",\"key\":\"data\",\"zh\":\"数据\"}],",
        "\"undo\":{\"label_en\":\"Undo\",\"label_zh\":\"撤销\",\"ok\":false,\"value\":\"WL_AND_5G\",",
        "\"why_en\":\"Nothing to undo\",\"why_zh\":\"设置没变 · 不用撤销\"},\"rollback_to\":\"WL_AND_5G\"}}"
    );

    #[test]
    fn the_op_object() {
        let _l = lang::TEST_LANG.lock().unwrap_or_else(|e| e.into_inner());
        lang::set_en(false);
        assert!(parse("").is_none());
        let v = parse("{\"rollback_enabled\":false,\"active\":null,\"last\":null}").unwrap();
        assert!(!v.rollback_enabled && !v.active.have && !v.last.have && v.notice.is_empty());
        let v = parse("{\"rollback_enabled\":true,\"active\":null,\"last\":null,\"notice\":\"rollback_on\"}").unwrap();
        assert_eq!(v.notice, "rollback_on");
        let v = parse("{\"rollback_enabled\":true,\"active\":null,\"last\":null,\"notice\":null}").unwrap();
        assert!(v.notice.is_empty());

        let v = parse(OP).unwrap();
        assert!(v.rollback_enabled);
        let (a, l) = (&v.active, &v.last);
        assert!(a.have && a.op_id == "screen-12-3" && a.phase == "verifying" && !phase_final(&a.phase));
        assert!(a.say == "正在确认" && a.next == "{t} 后没通就退回到自动" && a.note.is_empty());
        assert!(a.source == "网页" && a.what == "制式" && a.old_v == "自动" && a.target == "只用 4G");
        assert!(a.revert_label == "退回自动" && a.keep_label == "保留只用 4G" && a.can_revert && a.can_keep);
        assert!(a.remaining_ms == 102000 && a.mark == MARK_NONE && a.stay == STAY_LIVE && a.undo_ok == -1);
        assert!(a.step[0] == "设置已生效" && a.step_done[0] && a.step_done[1] && !a.step_done[2]);
        assert_eq!(a.rollback_to_raw, "WL_AND_5G");
        assert!(l.have && phase_final(&l.phase) && l.mark == MARK_WARN && l.stay == STAY_STICKY);
        assert!(l.say == "没通 · 已退回自动" && l.needs_ack && !l.acked && l.remaining_ms == -1);
        assert!(l.undo_ok == 0 && l.undo_why == "设置没变 · 不用撤销" && l.undo_label == "撤销");

        assert_eq!(fill_clock(&a.next, a.remaining_ms), "1:42 后没通就退回到自动");
        assert_eq!(fill_clock("还剩 {t} · 自动退回没开", 48001), "还剩 0:49 · 自动退回没开");
        assert_eq!(fill_clock("{t} left", -5), "0:00 left");
        assert_eq!(fill_clock("no clock", 1000), "no clock");

        lang::set_en(true);
        let v = parse(OP).unwrap();
        let (a, l) = (&v.active, &v.last);
        assert!(a.say == "Checking" && a.revert_label == "Revert to Auto" && a.step[2] == "Data");
        assert!(l.say == "No data · back to Auto" && l.undo_why == "Nothing to undo");
        assert!(a.source == "Web" && a.old_v == "Auto");
        lang::set_en(false);
    }

    #[test]
    fn the_change_log_and_owners() {
        let _l = lang::TEST_LANG.lock().unwrap_or_else(|e| e.into_inner());
        lang::set_en(false);
        let rep = concat!(
            "{\"action\":\"journal.list\",\"ok\":true,\"result\":{\"entries\":[",
            "{\"source\":\"screen\",\"action\":\"op.ack\",\"result\":\"ok\",\"hide\":true,\"what_zh\":\"op.ack\"},",
            "{\"op_id\":\"b\",\"action\":\"network.set_mode\",\"item\":\"network.mode\",\"source\":\"web\",\"undo\":false,",
            "\"t\":\"2026-10-03 14:32:07\",\"what_zh\":\"制式\",\"what_en\":\"Network mode\",",
            "\"change_zh\":\"自动 → 只用 4G\",\"change_en\":\"Auto → 4G only\",\"result_zh\":\"已切到只用 4G\",",
            "\"result_en\":\"Now 4G only\",\"mark\":\"ok\",\"source_zh\":\"网页\",\"source_en\":\"Web\",\"hide\":false,",
            "\"undo_view\":{\"ok\":true,\"label_zh\":\"撤销\",\"label_en\":\"Undo\",\"why_zh\":null,\"why_en\":null,",
            "\"request\":{\"action\":\"network.set_mode\",\"undo\":true,\"params\":{\"mode\":\"WL_AND_5G\"}}}},",
            "{\"action\":\"cellular.set\",\"source\":\"screen\",\"params\":{\"enabled\":0},\"result\":\"failed\",",
            "\"t\":\"2026-10-03 09:01:00\",\"what_zh\":\"移动数据\",\"change_zh\":\"关掉数据\",\"result_zh\":\"没改成\",",
            "\"mark\":\"bad\",\"source_zh\":\"触屏\",\"hide\":false,\"undo_view\":null}",
            "],\"owners\":{}}}"
        );
        let lg = log_parse(rep, LOG_MAX).unwrap();
        assert_eq!(lg.len(), 2);
        assert!(lg[0].what == "制式" && lg[0].change == "自动 → 只用 4G" && lg[0].result == "已切到只用 4G");
        assert!(lg[0].when == "10-03 14:32" && lg[0].source == "网页" && lg[0].mark == MARK_OK);
        assert!(lg[0].undo_have && lg[0].undo_ok && lg[0].undo_label == "撤销");
        assert!(lg[0].undo_action == "network.set_mode" && lg[0].undo_params == "{\"mode\":\"WL_AND_5G\"}");
        assert!(lg[1].what == "移动数据" && lg[1].mark == MARK_BAD && !lg[1].undo_have && lg[1].when == "10-03 09:01");
        assert!(log_parse("{\"ok\":false}", LOG_MAX).is_none());
        assert_eq!(log_parse("{\"result\":{\"entries\":[]}}", LOG_MAX).unwrap().len(), 0);
        let one = log_parse(rep, 1).unwrap();
        assert!(one.len() == 1 && one[0].op_id == "b");

        let own = concat!(
            "{\"ok\":true,\"result\":{\"entries\":[],\"owners\":{\"network.mode\":{\"source\":\"scenario\",",
            "\"user\":false,\"undo\":false,\"value\":\"Only_LTE\",\"op_id\":\"sc-7\",\"ts\":1,",
            "\"t\":\"2026-10-04 09:15:02\"},\"wifi\":{\"source\":\"mystery\",\"op_id\":null,",
            "\"t\":\"2026-10-04 08:00:00\"},\"chill.profile\":{\"source\":\"mcp\",\"op_id\":\"mcp-1\",",
            "\"t\":\"2026-10-04 10:00:00\"}}}}"
        );
        let ow = owner_parse(own, "network.mode").unwrap();
        assert!(ow.have && ow.when == "10-04 09:15" && ow.source == "情景" && ow.op_id == "sc-7");
        let ow = owner_parse(own, "wifi").unwrap();
        assert!(ow.have && ow.source == "mystery" && ow.op_id.is_empty());
        let ow = owner_parse(own, "chill.profile").unwrap();
        assert!(ow.have && ow.source == "MCP" && ow.op_id == "mcp-1");
        assert!(!owner_parse(own, "band.lock").unwrap().have);
        assert!(!owner_parse(rep, "network.mode").unwrap().have);
        assert!(owner_parse("{\"ok\":false}", "network.mode").is_none());
        lang::set_en(true);
        assert_eq!(owner_parse(own, "network.mode").unwrap().source, "Scene");
        lang::set_en(false);
    }
}
