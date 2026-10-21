//! 事务的界面数据（E4 T13，docs/STATE_V2.md 第 12 节；manager `docs/designs/write-op-layer.md`
//! 设计评审的「状态文案表」，DD6、DD7、DD10、DD12、DD14、DD16）。
//!
//! 纯函数：给事务和上下文，出文字和标志。文字中英各一份，触屏和网页照着显示。
//! 中文排版：中文和数字、英文之间空一格，中文和中文之间不空（[`cat`]）。

use super::spec::{
    APN, BAND_ALL, BAND_LTE, BAND_NR, CELL_LTE, CELL_NR, NETWORK_MODE, WIFI_GUEST, WIFI_MAIN,
    is_wifi,
};
use super::txn::{Phase, Reason, Source, Txn, Who};
use crate::project::screen::{ScreenOp, ScreenOpKind};
use serde_json::{Value, json};

type Words = (String, String);

/// DD14：来源显示名。
pub fn source_words(s: Source) -> (&'static str, &'static str) {
    match s {
        Source::Screen | Source::Legacy => ("触屏", "Screen"),
        Source::Web => ("网页", "Web"),
        Source::Scenario => ("情景", "Scene"),
        Source::Scheduler => ("定时任务", "Schedule"),
        Source::Auto => ("自动", "Auto"),
        Source::Guard => ("自动恢复", "Auto-recovery"),
        Source::Mcp => ("MCP", "MCP"),
    }
}

/// 来源加确认者（T22，D23）：两者相同时就是 [`source_words`]；人确认了别人的提议时写
/// 「MCP 提议 · 你在触屏确认」/「Proposed by MCP · confirmed on the screen」。
pub fn who_words(who: impl Into<Who>) -> Words {
    let who = who.into();
    let (src_zh, src_en) = source_words(who.source);
    let Some(by) = who.confirmed_by() else {
        return (src_zh.to_owned(), src_en.to_owned());
    };
    let (by_zh, by_en) = source_words(by);
    (
        format!(
            "{} · {}",
            cat(src_zh, "提议"),
            cat(&cat("你在", by_zh), "确认")
        ),
        format!(
            "Proposed by {src_en} · confirmed on the {}",
            by_en.to_ascii_lowercase()
        ),
    )
}

/// 一项的叫法：(进行中的大字, 项名, 英文项名)。描述表里没有的先用通用的。
fn item_words(item: &str) -> (&'static str, &'static str, &'static str) {
    match item {
        NETWORK_MODE => ("正在换制式", "制式", "Network mode"),
        BAND_LTE => ("正在锁频", "4G 频段", "4G bands"),
        BAND_NR => ("正在锁频", "5G 频段", "5G bands"),
        BAND_ALL => ("正在恢复默认频段", "频段和小区锁", "Bands and cell lock"),
        CELL_LTE => ("正在锁小区", "4G 小区锁", "4G cell lock"),
        CELL_NR => ("正在锁小区", "5G 小区锁", "5G cell lock"),
        APN => ("正在换 APN", "APN", "APN"),
        WIFI_MAIN => ("正在改 Wi-Fi", "Wi-Fi", "Wi-Fi"),
        WIFI_GUEST => ("正在改访客 Wi-Fi", "访客 Wi-Fi", "Guest Wi-Fi"),
        super::chill::PROFILE => ("正在换 CHILL 档位", "CHILL 档位", "CHILL profile"),
        super::chill::EXIT => ("正在换 CHILL 出口", "CHILL 出口", "CHILL exit"),
        "netselect.session" => ("正在搜网", "搜网", "Network search"),
        super::spec::DUAL_SIM => ("正在切换 SIM 卡", "双卡", "Dual SIM"),
        _ => ("正在改设置", "设置", "Setting"),
    }
}

/// 值的显示名：网络模式用首页同一张表（自动 / 只用 4G …）；频段写「B1 B3」「SA n78 · NSA n41 n78」，
/// 太长的只写个数；别的项先用字符串本身或紧凑 JSON。
pub fn value_words(item: &str, v: &Value) -> Words {
    match (item, v) {
        (_, v) if super::txn::blank(v) => ("-".into(), "-".into()),
        (NETWORK_MODE, Value::String(s)) => crate::project::screen::net_select_word(s),
        (BAND_LTE, Value::String(s)) => band_words("B", s),
        (CELL_LTE | CELL_NR, v) => cell_words(v),
        (APN, v) => apn_words(v),
        (super::spec::DUAL_SIM, v) => dual_sim_words(v),
        // Wi-Fi：总开关、名称，其余只说几项；密码和指纹都不显示
        (WIFI_MAIN | WIFI_GUEST, v) => super::wifi::words(v),
        (super::chill::PROFILE, Value::String(s)) => chill_profile_words(s),
        (super::chill::EXIT, Value::String(s)) => chill_exit_words(s),
        (BAND_NR | BAND_ALL, Value::Object(m)) if m.contains_key("reset") => {
            ("默认".into(), "Default".into())
        }
        (BAND_NR | BAND_ALL, Value::Object(m)) => {
            let part = |k: &str, label: &str, prefix: &str| {
                m.get(k).and_then(Value::as_str).map(|s| {
                    let (zh, en) = band_words(prefix, s);
                    (format!("{label} {zh}"), format!("{label} {en}"))
                })
            };
            let mut parts: Vec<Words> = [
                part("lte", "4G", "B"),
                part("sa", "SA", "n"),
                part("nsa", "NSA", "n"),
            ]
            .into_iter()
            .flatten()
            .collect();
            // 恢复默认的值带两种小区锁：锁着的才写
            for k in ["lte_cell", "nr_cell"] {
                if let Some(c) = m.get(k).filter(|c| **c != super::spec::cell_off()) {
                    parts.push(cell_words(c));
                }
            }
            let join = |f: fn(&Words) -> &str| parts.iter().map(f).collect::<Vec<_>>().join(" · ");
            (join(|w| &w.0), join(|w| &w.1))
        }
        (_, Value::String(s)) => (s.clone(), s.clone()),
        (_, v) => (v.to_string(), v.to_string()),
    }
}

/// 双卡：「单卡 · 卡槽 1」「双卡 · 主卡 卡槽 2」「双卡 · 智能切换」。
fn dual_sim_words(v: &Value) -> Words {
    let slot = v.get("active_slot").and_then(Value::as_i64).unwrap_or(0);
    match (v.get("mode").and_then(Value::as_str), v.get("smart") == Some(&Value::Bool(true))) {
        (Some("single"), _) => (format!("单卡 · 卡槽 {slot}"), format!("Single SIM · slot {slot}")),
        (Some("dual"), true) => ("双卡 · 智能切换".into(), "Dual SIM · smart switching".into()),
        (Some("dual"), false) => (format!("双卡 · 主卡 卡槽 {slot}"), format!("Dual SIM · main slot {slot}")),
        _ => ("-".into(), "-".into()),
    }
}

/// 小区锁：「PCI 603 · 627264 · n78」，没锁写「不锁」。
fn cell_words(v: &Value) -> Words {
    if *v == super::spec::cell_off() {
        return ("不锁小区".into(), "No cell lock".into());
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
    let ch = if v.get("arfcn").is_some() {
        s("arfcn")
    } else {
        s("earfcn")
    };
    let mut w = format!("PCI {} · {ch}", s("pci"));
    if v.get("band").is_some() {
        w.push_str(&format!(" · n{}", s("band")));
    }
    (w.clone(), w)
}

/// APN：「自动」「手动 · manu3」；只知道模式（切模式的目标）就只写模式。指纹不显示。
fn apn_words(v: &Value) -> Words {
    let manual = v.get("manual").and_then(Value::as_str).unwrap_or("");
    match v.get("mode").and_then(Value::as_i64) {
        Some(0) => ("自动".into(), "Auto".into()),
        Some(1) if manual.is_empty() => ("手动".into(), "Manual".into()),
        Some(1) => (format!("手动 · {manual}"), format!("Manual · {manual}")),
        _ => ("-".into(), "-".into()),
    }
}

fn chill_profile_words(s: &str) -> Words {
    let (zh, en) = match s {
        "eco" => ("省电", "Eco"),
        "standard" => ("标准", "Standard"),
        "perf" => ("性能", "Performance"),
        _ => (s, s),
    };
    (zh.into(), en.into())
}

fn chill_exit_words(s: &str) -> Words {
    let (zh, en) = match s {
        "proxy" => ("代理", "Proxy"),
        "direct_keep_ai" => ("直连 · AI 不动", "Direct · AI stays"),
        "direct_all" => ("全部直连", "All direct"),
        "global" => ("全局", "Global"),
        _ => (s, s),
    };
    (zh.into(), en.into())
}

/// 一组频段：6 个以内逐个写（`B1 B3`），再多只写个数。
fn band_words(prefix: &str, s: &str) -> Words {
    let list: Vec<&str> = s.split(',').filter(|b| !b.is_empty()).collect();
    if list.len() > 6 {
        return (
            format!("{} 个频段", list.len()),
            format!("{} bands", list.len()),
        );
    }
    let w = list
        .iter()
        .map(|b| format!("{prefix}{b}"))
        .collect::<Vec<_>>()
        .join(" ");
    (w.clone(), w)
}

/// 中文拼接：两边挨着的是中文就不空格，否则空一格（「退回到自动」「退回到 4G + 5G」）。
fn cat(a: &str, b: &str) -> String {
    let wide = |c: Option<char>| c.is_some_and(|c| !c.is_ascii());
    if a.is_empty() || b.is_empty() || (wide(a.chars().last()) && wide(b.chars().next())) {
        format!("{a}{b}")
    } else {
        format!("{a} {b}")
    }
}

/// 倒计时 `m:ss`。
pub fn clock(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    format!("{}:{:02}", s / 60, s % 60)
}

/// 结果停留多久（DD16）。
fn stay(t: &Txn) -> &'static str {
    if !t.phase.is_final() {
        return "live";
    }
    match (t.phase, t.reason) {
        (Phase::RollbackFailed, _) => "alert",
        (Phase::Cancelled, Some(Reason::Superseded)) => "none",
        (Phase::Confirmed, Some(Reason::Verified | Reason::NoVerify))
        | (Phase::RolledBack, Some(Reason::UserRevert)) => "brief",
        _ => "sticky",
    }
}

/// 终态的一句话（状态文案表，不带符号）：(mark, 中, 英)。`x` 退回目标，`y` 目标，`old` 写之前的值。
fn final_words(t: &Txn, x: &Words, y: &Words, old: &Words) -> (&'static str, String, String) {
    final_line(&t.item, t.phase, t.reason, x, y, old)
}

/// 终态的一句话，按阶段和原因（改动记录也用，T8c）。Wi-Fi 的项没通说的是 Wi-Fi 没起来，不是没数据。
pub fn final_line(
    item: &str,
    phase: Phase,
    reason: Option<Reason>,
    x: &Words,
    y: &Words,
    old: &Words,
) -> (&'static str, String, String) {
    use Phase::*;
    use Reason::*;
    let wifi = is_wifi(item);
    let (mark, zh, en) = match (phase, reason) {
        (Unverified, _) if wifi => (
            "bad",
            format!("Wi-Fi 没起来 · {}", cat("还是", &y.0)),
            format!("Wi-Fi not up · still {}", y.1),
        ),
        (RolledBack, Some(NoReceipt)) => (
            "warn",
            format!("远端没回来 · {}", cat("已退回", &x.0)),
            format!("Remote did not come back · back to {}", x.1),
        ),
        (RolledBack, Some(Timeout)) if wifi => (
            "warn",
            format!("Wi-Fi 没起来 · {}", cat("已退回", &x.0)),
            format!("Wi-Fi not up · back to {}", x.1),
        ),
        (RollbackFailed, _) if wifi => (
            "bad",
            "退回后 Wi-Fi 也没起来".into(),
            "Revert failed".into(),
        ),
        (Confirmed, Some(UserKeep)) => (
            "warn",
            format!("{} · 没确认通", cat("保留", &y.0)),
            format!("Kept {} · unconfirmed", y.1),
        ),
        (Confirmed, Some(NoVerify)) => (
            "ok",
            "已改 · 没有可确认的读数".into(),
            "Saved · not checked".into(),
        ),
        (Confirmed, _) => ("ok", cat("已切到", &y.0), format!("Now {}", y.1)),
        (Unverified, Some(ApnNoData)) => (
            "warn",
            "APN 已存 · 数据关着没法试".into(),
            "APN saved · untested".into(),
        ),
        (Unverified, _) => (
            "bad",
            format!("没通 · {}", cat("还是", &y.0)),
            format!("No data · still {}", y.1),
        ),
        (RolledBack, Some(UserRevert)) => ("ok", cat("已退回", &x.0), format!("Back to {}", x.1)),
        (RolledBack, Some(RebootLoop)) => (
            "warn",
            format!("重启了两次 · {}", cat("已退回", &x.0)),
            format!("2 reboots · back to {}", x.1),
        ),
        (RolledBack, _) => (
            "warn",
            format!("没通 · {}", cat("已退回", &x.0)),
            format!("No data · back to {}", x.1),
        ),
        (NotApplied, _) => (
            "warn",
            format!("没切成 · {}", cat("还是", &old.0)),
            format!("Didn't apply · still {}", old.1),
        ),
        (RollbackFailed, _) => ("bad", "退回也没通".into(), "Revert failed".into()),
        (Cancelled, Some(Superseded)) => (
            "ok",
            "被新的改动接替".into(),
            "Replaced by a newer change".into(),
        ),
        (Cancelled, Some(Preempted)) => (
            "warn",
            "已关数据 · 不再自动退回".into(),
            "Data off · no auto revert".into(),
        ),
        (Cancelled, Some(Takeover)) => (
            "warn",
            "手动接管 · 不再自动退回".into(),
            "Manual override".into(),
        ),
        (Cancelled, Some(SimChanged)) => {
            ("warn", "换过卡 · 不再自动退回".into(), "SIM changed".into())
        }
        // D40：确认或退回中用户又改了别的影响上网的设置（文字照设计稿）
        (Cancelled, Some(OtherChange)) => (
            "warn",
            "改了别的设置，不再自动退回".into(),
            "Other change; no auto revert".into(),
        ),
        (Cancelled, _) => (
            "warn",
            "别处改过 · 不再自动退回".into(),
            "Changed elsewhere".into(),
        ),
        // 进行中的阶段不会走到这里。
        _ => ("warn", "已结束".into(), "Ended".into()),
    };
    (mark, zh, en)
}

/// 视图里要用、但事务自己不知道的：同一项后来又有别的事务（结束的或进行中的）。
#[derive(Clone, Copy, Debug, Default)]
pub struct Ctx {
    pub later_change: bool,
}

/// DD10：能不能撤销，不能时的原因。
fn undo_denied(t: &Txn, ctx: Ctx) -> Option<(&'static str, &'static str)> {
    if super::txn::blank(&t.old) {
        return Some(("没有旧值可退回", "No previous value"));
    }
    if ctx.later_change {
        return Some(SINCE);
    }
    undo_why(
        Some(t.phase),
        t.reason,
        t.readback
            .as_ref()
            .is_some_and(|r| super::txn::matches(&t.target, r)),
    )
}

const SINCE: (&str, &str) = ("之后又改过", "Changed since");

/// 按终态和原因：能撤为 None，否则原因（改动记录也用，T8c）。`target_still` = 读回还是写进去的值。
pub fn undo_why(
    phase: Option<Phase>,
    reason: Option<Reason>,
    target_still: bool,
) -> Option<(&'static str, &'static str)> {
    match phase {
        Some(Phase::Confirmed | Phase::Unverified) => None,
        Some(Phase::RolledBack | Phase::NotApplied) => {
            Some(("设置没变 · 不用撤销", "Nothing to undo"))
        }
        Some(Phase::RollbackFailed) => Some(("先再试一次退回", "Retry the revert instead")),
        Some(Phase::Cancelled) => match reason {
            Some(Reason::SimChanged) => Some(("换过卡", "SIM changed")),
            Some(Reason::Superseded) => Some(SINCE),
            _ if target_still => None,
            _ => Some(SINCE),
        },
        _ => Some(("这类操作不能撤销", "Can't be undone")),
    }
}

/// 进度三行（DD12）看的值：退回中、退回后看退回目标。
fn goal(t: &Txn) -> &Value {
    match t.phase {
        Phase::RollingBack | Phase::RolledBack | Phase::RollbackFailed => &t.rollback_to,
        _ => &t.target,
    }
}

/// 事务视图：`Txn::status` 加上界面字段（V2-35、V2-36）。
pub fn view(t: &Txn, now: u64, ctx: Ctx) -> Value {
    let mut v = t.status(now);
    let item = t.item.as_str();
    let (doing, what_zh, what_en) = item_words(item);
    let (src_zh, src_en) = who_words(t.who());
    let x = value_words(item, &t.rollback_to);
    let y = value_words(item, &t.target);
    let old = value_words(item, &t.old);
    let readback = t.readback.as_ref().map(|r| value_words(item, r));
    let live = !t.phase.is_final();

    let (mark, say_zh, say_en) = match t.phase {
        Phase::Accepted | Phase::Applying => (None, doing.to_string(), "Switching".to_string()),
        Phase::Verifying => (None, "正在确认".into(), "Checking".into()),
        Phase::RollingBack => (None, "正在退回".into(), "Reverting".into()),
        _ => {
            let (m, zh, en) = final_words(t, &x, &y, &old);
            (Some(m), zh, en)
        }
    };
    let wifi = is_wifi(item);
    let next = match t.phase {
        _ if !t.counting() => None,
        Phase::Verifying if t.rollback_enabled && wifi => Some((
            cat("{t} 后 Wi-Fi 没起来就退回到", &x.0),
            format!("Back to {} in {{t}} if Wi-Fi isn't up", x.1),
        )),
        Phase::Verifying if t.rollback_enabled => Some((
            cat("{t} 后没通就退回到", &x.0),
            format!("Back to {} in {{t}} if no data", x.1),
        )),
        Phase::Verifying => Some((
            "还剩 {t} · 自动退回没开".to_string(),
            "{t} left · auto revert off".to_string(),
        )),
        _ => Some((
            format!("{} · 还剩 {{t}}", cat("退回到", &x.0)),
            format!("To {} · {{t}} left", x.1),
        )),
    };
    let note = (live && t.boots > 0).then_some(("重启过 · 重新确认", "Rebooted · checking again"));
    let applied = t
        .readback
        .as_ref()
        .is_some_and(|r| super::txn::matches(goal(t), r));
    let chill = matches!(t.confirm, super::txn::Confirm::Chill);
    // Wi-Fi 的项：第二步是 AP 已按设置起来（读数的 registered 在这里是这个意思），第三步不等数据；
    // CHILL 的项：第二步是 mihomo 在运行，第三步是经代理出口连续两次通。
    let (reg_zh, reg_en, data_zh, data_en) = if wifi {
        ("Wi-Fi 已起来", "Wi-Fi up", "不用等数据", "No data check")
    } else if chill {
        ("CHILL 在运行", "CHILL running", "出口连通", "Exit reachable")
    } else {
        ("已注册", "Registered", "数据", "Data")
    };
    let steps = json!([
        {"key": "applied", "zh": "设置已生效", "en": "Setting applied", "done": applied},
        {"key": "registered", "zh": reg_zh, "en": reg_en, "done": t.registered},
        {"key": "data", "zh": data_zh, "en": data_en, "done": t.data_ok},
    ]);
    let can = t.phase == Phase::Verifying && t.counting();
    let can_revert = can && !super::txn::blank(&t.rollback_to);
    let undo = if live {
        Value::Null
    } else {
        let (label_zh, label_en) = if t.undo {
            ("重做", "Redo")
        } else {
            ("撤销", "Undo")
        };
        let why = undo_denied(t, ctx);
        json!({
            "ok": why.is_none(),
            "label_zh": label_zh,
            "label_en": label_en,
            "why_zh": why.map(|w| w.0),
            "why_en": why.map(|w| w.1),
            // Wi-Fi：只退回那次改过的键（和改动记录的撤销一样）
            "value": if wifi { super::wifi::project(&t.old, &t.target) } else { t.old.clone() },
        })
    };

    let m = v.as_object_mut().expect("status is an object");
    let mut put = |k: &str, val: Value| {
        m.insert(k.into(), val);
    };
    put("source_zh", json!(src_zh));
    put("source_en", json!(src_en));
    put("what_zh", json!(what_zh));
    put("what_en", json!(what_en));
    put("old_zh", json!(old.0));
    put("old_en", json!(old.1));
    put("target_zh", json!(y.0));
    put("target_en", json!(y.1));
    put("rollback_to_zh", json!(x.0));
    put("rollback_to_en", json!(x.1));
    put("readback_zh", json!(readback.as_ref().map(|r| &r.0)));
    put("readback_en", json!(readback.as_ref().map(|r| &r.1)));
    put("say_zh", json!(say_zh));
    put("say_en", json!(say_en));
    put("mark", json!(mark));
    put("stay", json!(stay(t)));
    put("next_zh", json!(next.as_ref().map(|n| &n.0)));
    put("next_en", json!(next.as_ref().map(|n| &n.1)));
    put("note_zh", json!(note.map(|n| n.0)));
    put("note_en", json!(note.map(|n| n.1)));
    put("steps", steps);
    // DD13：按钮写具体值（「退回自动」「保留只用 4G」），中文拼接规则只在这里。
    put("revert_label_zh", json!(cat("退回", &x.0)));
    put("revert_label_en", json!(format!("Revert to {}", x.1)));
    put("keep_label_zh", json!(cat("保留", &y.0)));
    put("keep_label_en", json!(format!("Keep {}", y.1)));
    put("can_revert", json!(can_revert));
    put("can_keep", json!(can));
    put("undo", undo);
    v
}

/// 结束的事务要不要等「知道了」（常驻类）。
pub fn sticky(t: &Txn) -> bool {
    matches!(stay(t), "sticky" | "alert")
}

/// V2-39：busy 回复里的一句。`item` 为 None 时按动作名猜不出项，用通用的。
pub fn busy_words(item: &str, who: impl Into<Who>, age_ms: u64) -> Words {
    let (doing, _, what_en) = item_words(item);
    let who = who.into();
    let (mut src_zh, src_en) = who_words(who);
    if who.confirmed_by().is_none() {
        src_zh.push_str("发起");
    }
    (
        format!("{doing}（{src_zh}，{} 秒），稍等", age_ms / 1000),
        format!("Busy: {} ({src_en})", what_en.to_ascii_lowercase()),
    )
}

/// 首页结论用的一行（V2-38）：进行中的事务，或者还没点「知道了」的常驻结果。
pub fn screen_op(active: Option<&Txn>, last: Option<(&Txn, bool)>, now: u64) -> Option<ScreenOp> {
    if let Some(t) = active {
        let v = view(t, now, Ctx::default());
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        let t_left = clock(t.remaining_ms(now).unwrap_or(0));
        let (mut hint_zh, mut hint_en) = (
            s("next_zh").replace("{t}", &t_left),
            s("next_en").replace("{t}", &t_left),
        );
        if let (Some(nz), Some(ne)) = (v["note_zh"].as_str(), v["note_en"].as_str()) {
            hint_zh = if hint_zh.is_empty() {
                nz.into()
            } else {
                format!("{nz} · {hint_zh}")
            };
            hint_en = if hint_en.is_empty() {
                ne.into()
            } else {
                format!("{ne} · {hint_en}")
            };
        }
        return Some(ScreenOp {
            kind: ScreenOpKind::Live,
            head: (s("say_zh"), s("say_en")),
            hint: (hint_zh, hint_en),
        });
    }
    let (t, acked) = last?;
    if acked || !sticky(t) {
        return None;
    }
    let item = t.item.as_str();
    let (say_zh, say_en) = {
        let x = value_words(item, &t.rollback_to);
        let y = value_words(item, &t.target);
        let old = value_words(item, &t.old);
        let (_, zh, en) = final_words(t, &x, &y, &old);
        (zh, en)
    };
    if t.phase == Phase::RollbackFailed {
        let x = value_words(item, &t.rollback_to);
        let (now_zh, now_en) = match t.readback.as_ref() {
            Some(r) => {
                let w = value_words(item, r);
                (cat("现在是", &w.0), format!("Now {}", w.1))
            }
            None => ("当前设置未知".into(), "Current setting unknown".into()),
        };
        return Some(ScreenOp {
            kind: ScreenOpKind::Alert,
            head: ("退回也没通".into(), "Failed".into()),
            hint: (
                format!(
                    "{now_zh} · {} · 再试一次退回或重启设备",
                    cat("上次确认是", &x.0)
                ),
                format!("{now_en} · last good {} · retry revert or restart", x.1),
            ),
        });
    }
    Some(ScreenOp {
        kind: ScreenOpKind::Sticky,
        head: (String::new(), String::new()),
        hint: (say_zh, say_en),
    })
}

#[cfg(test)]
mod tests;
