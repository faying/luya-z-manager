//! 改动记录的显示文字（E4 T8c，docs/STATE_V2.md V2-41；manager `write-op-layer.md` DD5、DD10、DD11）。
//!
//! `journal.list` 回的每一行原样保留，再加上界面直接显示的字段，触屏和网页照着显示，不自己拼：
//! `what_zh/_en`（改的是什么）、`change_zh/_en`（旧 → 新，没有为 ""）、`result_zh/_en`、`mark`、
//! `source_zh/_en`、`hide`（不单独成行）、`undo_view`（只有事务的行有；能不能撤、原因、照发就行的请求）。
//! 原来的 `undo`（这一行本身是不是撤销）不动。
//! 纯函数：输入是新的在前的一串行。

use super::{
    spec,
    txn::{Phase, Reason, Source, Who},
    ui::{self, value_words},
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

type Words = (String, String);

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// 动作或项的叫法。表里没有的原样显示。
fn name(key: &str) -> Words {
    let (zh, en) = match key {
        "network.set_mode" | "network.mode" => ("制式", "Network mode"),
        "cellular.set" => ("移动数据", "Mobile data"),
        "cellular.connect" => ("连接数据", "Connect data"),
        "cellular.disconnect" => ("断开数据", "Disconnect data"),
        "cellular.redial" => ("重新拨号", "Redial"),
        "band.set_lte" => ("锁频 4G", "4G band lock"),
        "band.set_nr_sa" => ("锁频 5G SA", "5G SA band lock"),
        "band.set_nr_nsa" => ("锁频 5G NSA", "5G NSA band lock"),
        "band.lte" => ("4G 频段", "4G bands"),
        "band.set_nr" | "band.nr" => ("5G 频段", "5G bands"),
        "band.reset" | "band.all" => ("频段恢复默认", "Band reset"),
        "cell.lock_lte" | "cell.lte" => ("4G 小区锁", "4G cell lock"),
        "cell.lock_nr" | "cell.nr" => ("5G 小区锁", "5G cell lock"),
        "cell.unlock_all" => ("解锁小区", "Cell unlock"),
        "sim.set_slot" => ("卡槽", "SIM slot"),
        "apn.set_mode" | "apn.mode" => ("APN 方式", "APN mode"),
        "apn.add" | "apn.modify" | "apn.delete" | "apn.enable" | "apn.profile"
        | "apn.set_pdp_type" | "apn.use" | "apn" => ("APN", "APN"),
        "modem.airplane" => ("飞行模式", "Airplane mode"),
        "modem.online" => ("打开移动网络", "Radio on"),
        "netselect.scan" => ("搜网", "Network scan"),
        "netselect.register" => ("手动注册", "Manual network"),
        "netselect.auto" => ("自动选网", "Automatic network"),
        "netselect.session" => ("搜网", "Network search"),
        "wifi.apply" | "wifi.configure" | "wifi.reload" | "wifi.set_dual_band"
        | "wifi.set_module" | "wifi.set_chip" => ("Wi-Fi", "Wi-Fi"),
        "wifi.settings" | "wifi.main" => ("Wi-Fi 设置", "Wi-Fi settings"),
        "wifi.guest" => ("访客 Wi-Fi", "Guest Wi-Fi"),
        "wifi.psm.set" => ("Wi-Fi 省电", "Wi-Fi power save"),
        "wifi.power_save" => ("Wi-Fi 节能", "Wi-Fi power save"),
        "nfc.set" => ("碰一碰", "NFC"),
        "power.direct_supply.set" => ("直供电", "Direct power"),
        "usb.set" => ("USB", "USB"),
        "usb.attach_mode" => ("插线用法", "USB use"),
        "sleep.set" => ("自动休眠", "Auto sleep"),
        "device.reboot" => ("重启", "Restart"),
        "device.poweroff" => ("关机", "Power off"),
        "lan.set" | "lan.set_mtu" => ("局域网", "LAN"),
        "dns.set" | "dns.doh" => ("DNS", "DNS"),
        "sms.delete" | "sms.db_delete" | "sms.send_raw" => ("短信", "SMS"),
        "client.kick" | "client.rename" | "client.block" | "client.unblock" => {
            ("已连设备", "Clients")
        }
        "traffic.set_limit" | "traffic.set_clear_day" | "traffic.calibrate" => {
            ("流量", "Data usage")
        }
        "vendor.call" => ("原厂设置", "Vendor setting"),
        "esim" => ("eSIM", "eSIM"),
        "chill" => ("CHILL", "CHILL"),
        "chill.profile" => ("CHILL 档位", "CHILL profile"),
        "chill.exit" => ("CHILL 出口", "CHILL exit"),
        k if k.starts_with("esim.") => ("eSIM", "eSIM"),
        k if k.starts_with("chill") => ("CHILL", "CHILL"),
        other => return (other.to_owned(), other.to_owned()),
    };
    (zh.into(), en.into())
}

fn onoff(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        Value::String(s) => match s.as_str() {
            "1" | "true" | "on" | "enable" => Some(true),
            "0" | "false" | "off" | "disable" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// 不走事务的写：参数里看得出改成了什么的几个。
fn change_of(action: &str, params: &Value) -> Words {
    let on = |zh_on: &str, zh_off: &str, en_on: &str, en_off: &str, v: &Value| -> Option<Words> {
        onoff(v).map(|b| {
            if b {
                (zh_on.to_owned(), en_on.to_owned())
            } else {
                (zh_off.to_owned(), en_off.to_owned())
            }
        })
    };
    let w = match action {
        "cellular.set" => {
            let parts: Vec<Words> = [
                params
                    .get("enabled")
                    .and_then(|v| on("打开数据", "关掉数据", "data on", "data off", v)),
                params
                    .get("roaming")
                    .and_then(|v| on("打开漫游", "关掉漫游", "roaming on", "roaming off", v)),
            ]
            .into_iter()
            .flatten()
            .collect();
            (!parts.is_empty()).then(|| {
                (
                    parts
                        .iter()
                        .map(|p| p.0.as_str())
                        .collect::<Vec<_>>()
                        .join("、"),
                    parts
                        .iter()
                        .map(|p| p.1.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            })
        }
        "network.set_mode" => params.get("mode").and_then(Value::as_str).map(|m| {
            let (zh, en) = value_words(spec::NETWORK_MODE, &json!(m));
            (format!("改成{zh}"), format!("to {en}"))
        }),
        "band.set_lte" | "band.set_nr" | "band.reset" | "cell.lock_lte" | "cell.lock_nr" => {
            spec::find(action)
                .and_then(|sp| sp.target(params).ok().map(|t| value_words(sp.item, &t)))
        }
        "band.set_nr_sa" | "band.set_nr_nsa" => params
            .get("bands")
            .and_then(Value::as_str)
            .map(|b| (b.to_owned(), b.to_owned())),
        "nfc.set"
        | "power.direct_supply.set"
        | "usb.set"
        | "sleep.set"
        | "wifi.psm.set"
        | "wifi.power_save" => params
            .get("enabled")
            .and_then(|v| on("打开", "关掉", "on", "off", v)),
        "usb.attach_mode" => params.get("mode").and_then(Value::as_str).and_then(|m| {
            let (zh, en) = match m {
                "share" => ("充电 + 上网", "charge + internet"),
                "fast_charge" => ("快速充电宝", "fast charging"),
                "accessory" => ("网口配件", "Ethernet adapter"),
                _ => return None,
            };
            Some((zh.to_owned(), en.to_owned()))
        }),
        "wifi.apply" => params.get("set").and_then(Value::as_object).and_then(|m| {
            let ap: Vec<bool> = ["wireless.main_2g.disabled", "wireless.main_5g.disabled"]
                .iter()
                .filter_map(|k| m.get(*k).and_then(onoff))
                .collect();
            (!ap.is_empty() && m.len() == ap.len()).then(|| {
                if ap.iter().all(|d| *d) {
                    ("关掉".to_owned(), "off".to_owned())
                } else {
                    ("打开".to_owned(), "on".to_owned())
                }
            })
        }),
        _ => None,
    };
    w.unwrap_or_default()
}

/// 不走事务的行的结果。
fn plain_result(result: &str) -> (&'static str, &'static str, &'static str) {
    match result {
        "ok" => ("已改", "Done", "ok"),
        "failed" => ("没改成", "Failed", "bad"),
        "requested" => ("已发出", "Sent", "ok"),
        "queued" => ("排队中", "Queued", "warn"),
        "replaced" => ("被新的替换", "Replaced", "warn"),
        "dropped" => ("没执行", "Dropped", "warn"),
        "opened" => ("开始", "Started", "ok"),
        "closed" => ("结束", "Ended", "ok"),
        "expired" => ("到点收回", "Timed out", "warn"),
        "agent_gone" => ("没续约收回", "Lease lost", "warn"),
        // D3：等 Wi-Fi 锁超时，放掉写锁、整次重试
        "lock_wait" => ("等 Wi-Fi 锁超时，重试", "Wi-Fi lock busy, retrying", "warn"),
        _ => ("", "", "warn"),
    }
}

fn skip_why(reason: &str) -> Words {
    match reason {
        "user_hold" => ("你手动改过".into(), "you changed it".into()),
        "busy" => ("设备正忙".into(), "busy".into()),
        "" => (String::new(), String::new()),
        other => (other.to_owned(), other.to_owned()),
    }
}

fn parse_enum<T: serde::de::DeserializeOwned>(v: &str) -> Option<T> {
    serde_json::from_value(json!(v)).ok()
}

/// 一行事务（`txn_line`）：终态文字和撤销。
fn txn_entry(line: &Value, later_change: bool) -> (Words, Words, &'static str, Value) {
    let item = s(line, "item");
    let null = Value::Null;
    let raw = |k: &str| line.get(k).unwrap_or(&null);
    let old = value_words(item, raw("old"));
    let new = value_words(item, raw("new"));
    let x = value_words(item, raw("rollback_to"));
    let phase: Option<Phase> = parse_enum(s(line, "result"));
    let reason: Option<Reason> = parse_enum(s(line, "reason"));
    let (mark, zh, en) = match phase {
        Some(p) => ui::final_line(item, p, reason, &x, &new, &old),
        None => (
            "warn",
            s(line, "result").to_owned(),
            s(line, "result").to_owned(),
        ),
    };
    let readback = line.get("readback").filter(|v| !v.is_null());
    let why = if super::txn::blank(raw("old")) {
        Some(("没有旧值可退回", "No previous value"))
    } else if later_change {
        Some(("之后又改过", "Changed since"))
    } else {
        ui::undo_why(
            phase,
            reason,
            readback.is_some_and(|r| super::txn::matches(raw("new"), r)),
        )
    };
    let undo_flag = line.get("undo").and_then(Value::as_bool).unwrap_or(false);
    let (label_zh, label_en) = if undo_flag {
        ("重做", "Redo")
    } else {
        ("撤销", "Undo")
    };
    let action = s(line, "action");
    let request = spec::find(action).map(|sp| {
        // Wi-Fi：旧值是整项的读回，只退回那次改过的键（之后情景、看门狗改的别的不动）
        let old = if spec::is_wifi(sp.item) {
            super::wifi::project(raw("old"), raw("new"))
        } else {
            raw("old").clone()
        };
        let mut params = sp.to_params(&old);
        // APN、Wi-Fi：撤销前 u60d 核对现状还是那次改完的样子（被别人改过就不撤）
        if sp.item == spec::APN || spec::is_wifi(sp.item) {
            params["expect"] = raw("new").clone();
        }
        json!({"action": action, "undo": true, "params": params})
    });
    let undo = json!({
        "ok": why.is_none() && request.is_some(),
        "label_zh": label_zh,
        "label_en": label_en,
        "why_zh": why.map(|w| w.0),
        "why_en": why.map(|w| w.1),
        "request": request,
    });
    (
        (
            format!("{} → {}", old.0, new.0),
            format!("{} → {}", old.1, new.1),
        ),
        (zh, en),
        mark,
        undo,
    )
}

/// 给 `journal.list` 的每一行（新的在前）加显示字段。
pub fn decorate(entries: &mut [Value]) {
    // 同一项后来改过（事务结束，或不走事务的写成功了）：旧的事务不能再撤
    let mut changed_later: HashSet<String> = HashSet::new();
    // 跳过段：结束行（新）先看到，再看到它的开始行（旧）时把开始行收起来
    let mut open_end: HashMap<(String, String), ()> = HashMap::new();
    for line in entries.iter_mut() {
        // T22：行里有 `approved_by`（人确认了别人的提议）时写「MCP 提议 · 你在触屏确认」
        let who = Source::parse(s(line, "source")).map(|src| {
            let by = Source::parse(s(line, "approved_by"));
            Who::new(src, by).unwrap_or_else(|_| src.into())
        });
        let (src_zh, src_en) = who.map(ui::who_words).unwrap_or_default();
        let item = s(line, "item").to_owned();
        let action = s(line, "action").to_owned();
        let key_name = if !item.is_empty() { &item } else { &action };
        let (what_zh, what_en) = name(if action.is_empty() { &item } else { &action });
        let result = s(line, "result").to_owned();
        let is_txn = line.get("op_id").is_some() && line.get("new").is_some();
        let mut change = (String::new(), String::new());
        let mut res: Words;
        let mark: &'static str;
        let mut undo = Value::Null;
        // 只记账的界面动作不单独成行：「知道了」、提示的「知道了」、op.interrupt（取消已经在事务那一行里）
        let mut hide = matches!(action.as_str(), "op.ack" | "op.notice_ack" | "op.interrupt");

        if is_txn {
            let (c, r, m, u) = txn_entry(line, changed_later.contains(&item));
            change = c;
            res = r;
            mark = m;
            undo = u;
        } else if result == "skipped" {
            let skip = s(line, "skip");
            let k = (s(line, "source").to_owned(), key_name.clone());
            let why = skip_why(s(line, "reason"));
            mark = "warn";
            if skip == "end" {
                open_end.insert(k, ());
                let n = line.get("count").and_then(Value::as_u64).unwrap_or(1);
                res = (
                    format!("{src_zh}跳过 ×{n}（{}）", why.0),
                    format!("{src_en} skipped ×{n} ({})", why.1),
                );
            } else {
                hide = open_end.remove(&k).is_some();
                res = (
                    format!("{src_zh}跳过中（{}）", why.0),
                    format!("{src_en} skipping ({})", why.1),
                );
            }
        } else {
            let (zh, en, m) = plain_result(&result);
            mark = m;
            res = if zh.is_empty() {
                (result.clone(), result.clone())
            } else {
                (zh.to_owned(), en.to_owned())
            };
            if let Some(p) = line.get("params") {
                change = change_of(&action, p);
            }
            if line.get("journal_append").is_some() {
                // eSIM、CHILL 这类只记账的：旧 → 新有就写上
                let (o, n) = (s(line, "old"), s(line, "new"));
                if !n.is_empty() {
                    change = if o.is_empty() {
                        (n.to_owned(), n.to_owned())
                    } else {
                        (format!("{o} → {n}"), format!("{o} → {n}"))
                    };
                }
            }
        }
        // 这一行之后（更旧的行）看到同一项，就是「之后又改过」
        if !item.is_empty() && !hide && (is_txn || result == "ok") {
            changed_later.insert(item.clone());
        }
        if res.0.is_empty() {
            res = (result.clone(), result.clone());
        }
        let m = line.as_object_mut().expect("journal line is an object");
        m.insert("what_zh".into(), json!(what_zh));
        m.insert("what_en".into(), json!(what_en));
        m.insert("change_zh".into(), json!(change.0));
        m.insert("change_en".into(), json!(change.1));
        m.insert("result_zh".into(), json!(res.0));
        m.insert("result_en".into(), json!(res.1));
        m.insert("mark".into(), json!(mark));
        m.insert("source_zh".into(), json!(src_zh));
        m.insert("source_en".into(), json!(src_en));
        m.insert("hide".into(), json!(hide));
        m.insert("undo_view".into(), undo);
    }
}

#[cfg(test)]
mod tests;
