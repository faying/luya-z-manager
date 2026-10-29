//! 契约 v0 的短信操作（O-2、R21、R23）：插件按（世代, 序号）补读日志、确认，问现在的卡。
//!
//! - `sms.read {generation?, after, limit?}` → `{generation, reset, entries, has_more, acked}`；
//!   世代不对（日志回复或重建过）就从头给，插件用自己的去重键对账，不重推。
//! - `sms.ack {generation, seq}` → `{ok}`：确认到第几条；超限时先清已确认的。
//! - `sms.send {to, text}` → `{result}`：从现在的卡发出（O-5）。`submitted` = 固件说发出了；
//!   固件说失败回错 `send_failed`；轮询逾时回 `unknown`（可能已发出，插件不要自己重发）。
//! - `sms.remove {items}` → `{removed, left}`：从固件两个库里删掉这些短信（App 删除是全局的）。
//!   收到的给 `{from, date, text}`，发出的给 `{to, text, at}`（找固件留的副本）。
//! - `sms.reports {after?}` → `{reports: [{id, number, ok, at}]}`：网络回的送达报告（固件 sms.db 的
//!   `sms_report` 表，`at` 是设备时钟）。只读数据库：原厂 `zwrt_wms_get_status_rpt_data` 读一次就清掉。
//! - `sim.identity` → `{phase, sim}`：只给身份键，不给 ICCID/IMSI（R14）。
//! - 新短信进日志时送 `{"t":"ev","ev":"sms","generation","seq"}`，插件收到就来读。

use serde_json::{Value, json};

use crate::sms_journal;

fn journal() -> Result<&'static std::sync::Mutex<sms_journal::Journal>, (String, String)> {
    sms_journal::global().ok_or_else(|| ("unavailable".into(), "sms journal not open".into()))
}

pub fn read(params: &Value) -> Result<Value, (String, String)> {
    let generation = params["generation"].as_str();
    let after = params["after"].as_u64().unwrap_or(0);
    let limit = params["limit"]
        .as_u64()
        .unwrap_or(sms_journal::READ_MAX as u64) as usize;
    let j = journal()?.lock().unwrap_or_else(|e| e.into_inner());
    Ok(j.read(generation, after, limit))
}

pub async fn ack(params: &Value) -> Result<Value, (String, String)> {
    let bad = |m: &str| ("invalid_parameter".to_string(), m.to_string());
    let generation = params["generation"]
        .as_str()
        .ok_or_else(|| bad("generation"))?
        .to_owned();
    let seq = params["seq"].as_u64().ok_or_else(|| bad("seq"))?;
    let j = journal()?;
    // 确认要落盘（fsync）：放到阻塞线程上，不卡 core 的单线程
    tokio::task::spawn_blocking(move || {
        j.lock()
            .unwrap_or_else(|e| e.into_inner())
            .ack(&generation, seq)
    })
    .await
    .map_err(|e| ("internal".to_string(), e.to_string()))?
    .map(|ok| json!({"ok": ok}))
    .map_err(|e| ("io".to_string(), e.to_string()))
}

/// 一条短信最多几个 UTF-16 单位（`sms::valid` 收 4096 个十六进制字，就是 1024 个）。
const SEND_MAX: usize = 1024;

pub async fn send(params: &Value) -> Result<Value, (String, String)> {
    let bad = |m: &str| ("invalid_parameter".to_string(), m.to_string());
    let to = params["to"].as_str().ok_or_else(|| bad("to"))?;
    let text = params["text"].as_str().ok_or_else(|| bad("text"))?;
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() || units.len() > SEND_MAX {
        return Err(bad("text length"));
    }
    // 设备时钟本来就是当地时间，时区按 core 算的时差取整点（和告警短信同一种写法）
    let tz = (crate::api::utc_offset().await / 3600) as i32;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // 明文参数，和告警短信同一条路（`luyaz-alerts::sms::send_args`，这台固件上实际发过）；
    // `sms.send_raw` 的加密送法在 B31 上固件不收
    let args = json!({"number": to, "message_body": ucs2_hex(&units), "encode_type": "UNICODE",
                      "sms_time": sms_time(now, tz), "id": "-1"});
    let reply = match crate::state::ubus("zwrt_wms", "zte_libwms_send_sms", args).await {
        Ok(r) => r,
        // 呼叫失败或逾时：固件可能已经收下了
        Err(e) => {
            eprintln!("[plug] sms.send: ubus: {e}");
            return Ok(json!({"result": "unknown"}));
        }
    };
    if !accepted(reply.get("result")) {
        eprintln!("[plug] sms.send: refused (result={})", reply["result"]);
        return Err(("send_failed".into(), format!("result={}", reply["result"])));
    }
    // 收下了：再看固件的发送状态（和 sms::send_host 一样最多 10 秒）
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let st = crate::state::ubus("zwrt_wms", "zwrt_wms_get_cmd_status", json!({"sms_cmd": 4}))
            .await
            .ok()
            .and_then(|r| r.get("sms_cmd_status_result").and_then(Value::as_i64));
        match st {
            Some(3) => return Ok(json!({"result": "submitted"})),
            Some(2) => {
                eprintln!("[plug] sms.send: the network refused it (status 2)");
                return Err(("send_failed".into(), "status 2".into()));
            }
            _ => {}
        }
    }
    eprintln!("[plug] sms.send: no final status in 10 s");
    Ok(json!({"result": "unknown"}))
}

fn accepted(r: Option<&Value>) -> bool {
    crate::sms::send_accepted(r)
}

fn ucs2_hex(units: &[u16]) -> String {
    units.iter().map(|u| format!("{u:04X}")).collect()
}

/// 固件的 `YY;MM;DD;HH;MM;SS;+TZ`（`wall` 是设备时钟，已经是当地时间）。
fn sms_time(wall: i64, tz: i32) -> String {
    let days = wall.div_euclid(86400);
    let secs = wall.rem_euclid(86400);
    // Howard Hinnant 的 civil_from_days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let sign = if tz < 0 { '-' } else { '+' };
    format!(
        "{:02};{m:02};{d:02};{:02};{:02};{:02};{sign}{}",
        y % 100,
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        tz.abs()
    )
}

/// 一次最多删几条（一整段对话分批送）。
const REMOVE_MAX: usize = 200;
/// 发出的副本：固件记的时间离插件记的 `at` 最多差这么多秒。
const SENT_SLACK: i64 = 120;

pub async fn remove(params: &Value) -> Result<Value, (String, String)> {
    let bad = |m: &str| ("invalid_parameter".to_string(), m.to_string());
    let want = params["items"].as_array().ok_or_else(|| bad("items"))?;
    if want.is_empty() || want.len() > REMOVE_MAX {
        return Err(bad("items count"));
    }
    let c = sms_journal::control()
        .ok_or_else(|| ("unavailable".to_string(), "no sms scan yet".to_string()))?;
    let io = |e: String| ("io".to_string(), e);
    let ids = matching(&sms_journal::list_all(c).await.map_err(io)?, want);
    if ids.is_empty() {
        return Ok(json!({"removed": 0, "left": 0}));
    }
    // 管理网页同样的两步：原厂删（SIM 库里的它删不掉），还在的再从 sms.db 删
    let mut left = ids.clone();
    for action in ["sms.delete", "sms.db_delete"] {
        let joined = left
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(";");
        let params = json!({"ids": joined});
        let r = c.src.control.send("auto", None, action, &params);
        if let Err(e) = r.await.into_result() {
            eprintln!("[plug] sms.remove: {action}: {e}");
        }
        let now = sms_journal::list_all(c).await.map_err(io)?;
        left.retain(|id| now.iter().any(|m| m["id"].as_u64() == Some(*id)));
        if left.is_empty() {
            break;
        }
    }
    if !left.is_empty() {
        eprintln!(
            "[plug] sms.remove: {} of {} still there",
            left.len(),
            ids.len()
        );
    }
    Ok(json!({"removed": ids.len() - left.len(), "left": left.len()}))
}

/// 固件里对得上的编号。收到的：号码、时间、文字都一样（插件的去重键也是这三样）；
/// 发出的：tag 2 的副本，号码和文字一样、时间离 `at` 最近（[`SENT_SLACK`] 以内），一条只配一个。
fn matching(list: &[Value], want: &[Value]) -> Vec<u64> {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_owned();
    let tag = |m: &Value| {
        m["tag"]
            .as_i64()
            .or_else(|| m["tag"].as_str().and_then(|t| t.trim().parse().ok()))
    };
    let mut ids: Vec<u64> = Vec::new();
    for w in want {
        let text = s(w, "text");
        let hit = if let Some(to) = w["to"].as_str() {
            let at = w["at"].as_i64().unwrap_or(0);
            list.iter()
                .filter(|m| tag(m) == Some(2) && s(m, "number") == to)
                .filter(|m| sms_journal::text_of(&s(m, "content")) == text)
                .filter_map(|m| Some((m["id"].as_u64()?, (wall_of(&s(m, "date"))? - at).abs())))
                .filter(|(id, d)| *d <= SENT_SLACK && !ids.contains(id))
                .min_by_key(|(_, d)| *d)
                .map(|(id, _)| id)
        } else {
            let (from, date) = (s(w, "from"), s(w, "date"));
            list.iter()
                .filter(|m| s(m, "number") == from && s(m, "date") == date)
                .filter(|m| sms_journal::text_of(&s(m, "content")) == text)
                .filter_map(|m| m["id"].as_u64())
                .find(|id| !ids.contains(id))
        };
        ids.extend(hit);
    }
    ids
}

/// 一次最多给几份报告。
const REPORTS_MAX: u64 = 200;

pub async fn reports(params: &Value) -> Result<Value, (String, String)> {
    let after = params["after"].as_u64().unwrap_or(0);
    let db = std::env::var("LUYAZ_SMS_DB")
        .unwrap_or_else(|_| "/etc_rw/ztembb/ztesms/sms_db/sms.db".into());
    let bin = std::env::var("LUYAZ_SQLITE_BIN").unwrap_or_else(|_| "/usr/bin/sqlite3".into());
    // `after` 是数字，SQL 是固定的
    let sql = format!(
        "SELECT id, address, content, Year, Month, Day, Hour, Minute, Second FROM sms_report \
         WHERE id > {after} ORDER BY id LIMIT {REPORTS_MAX};"
    );
    let out = crate::command::run(
        &bin,
        [
            "-readonly".to_string(),
            "-separator".to_string(),
            "\u{1f}".to_string(),
            "-cmd".to_string(),
            ".timeout 2000".to_string(),
            db,
            sql,
        ],
        std::time::Duration::from_secs(5),
    )
    .await
    .map_err(|e| ("io".to_string(), format!("sqlite3: {e}")))?;
    Ok(json!({"reports": parse_reports(&String::from_utf8_lossy(&out))}))
}

/// `sqlite3` 的输出一行一份。`content` 是 TP-Status 的说法：`1` = 送到了（B31 实测），别的不算。
fn parse_reports(out: &str) -> Vec<Value> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\u{1f}').collect();
            let [id, number, content, y, mo, d, h, mi, se] = f[..] else {
                return None;
            };
            let at = wall_of(&[y, mo, d, h, mi, se].join(","))?;
            Some(json!({"id": id.parse::<u64>().ok()?, "number": number,
                        "ok": content.trim() == "1", "at": at}))
        })
        .collect()
}

/// 固件的 `YY,MM,DD,HH,MM,SS,+TZ`（分号也认）→ 设备时钟的秒（当地时间，时区不用）。
fn wall_of(date: &str) -> Option<i64> {
    let f: Vec<i64> = date
        .split([',', ';'])
        .take(6)
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    let [y, m, d, hh, mm, ss] = f[..] else {
        return None;
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant 的 days_from_civil
    let y = 2000 + y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

pub fn identity() -> Value {
    let phase = crate::sim_identity::global()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .phase()
        .as_str();
    let mut v = match sms_journal::current_source() {
        sms_journal::Source::Sim(key) => json!({"phase": phase, "sim": key}),
        sms_journal::Source::Unknown => json!({"phase": phase, "sim": null}),
    };
    // 卡里存的本机号码（上一轮采集的快照，不读基带）：App 用来写「从哪个号码发」。卡里没存就不带
    // 双卡时是电话卡的号码（上网卡可能是没有号码的内置卡）
    if let Some(n) = sms_journal::voice_number()
        .filter(|n| crate::project::snapshot::valid_msisdn(n))
        .or_else(|| own_number(crate::usage_log::latest("sim").as_ref()))
    {
        v["number"] = json!(n);
    }
    v
}

fn own_number(sim: Option<&Value>) -> Option<String> {
    let n = sim?["msisdn"].as_str()?.trim();
    crate::project::snapshot::valid_msisdn(n).then(|| n.to_owned())
}

/// 有新短信进了日志：告诉插件（没连上就算了，它连上后自己补读）。
pub fn announce() {
    let Some(j) = sms_journal::global() else {
        return;
    };
    let (generation, seq) = {
        let j = j.lock().unwrap_or_else(|e| e.into_inner());
        (j.generation().to_owned(), j.last_seq())
    };
    super::link::hub().event("sms", json!({"generation": generation, "seq": seq}));
}

#[cfg(test)]
mod tests {
    use super::super::link::Hub;
    use super::super::link::tests::{hello, me, next, plugin, reply, server, wait_connected};
    use serde_json::json;

    #[tokio::test]
    async fn calls_from_the_plugin_are_answered_and_undeclared_ones_refused() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, mut w) = plugin(&path, hello(&["sim"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        reply(&mut w, json!({"t":"call","id":1,"op":"sim.identity"})).await;
        let v = next(&mut r).await.unwrap();
        assert_eq!(
            (v["t"].as_str(), v["id"].as_u64(), v["ok"].as_bool()),
            (Some("ret"), Some(1), Some(true))
        );
        assert!(v["result"]["phase"].is_string());
        // 没声明 sms：拒绝
        reply(
            &mut w,
            json!({"t":"call","id":2,"op":"sms.read","params":{}}),
        )
        .await;
        let v = next(&mut r).await.unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "not_declared");
    }

    #[test]
    fn own_number_only_when_the_card_stores_one() {
        let n = |v: serde_json::Value| super::own_number(Some(&v));
        assert_eq!(n(json!({"msisdn": "+8613800000000"})), Some("+8613800000000".into()));
        assert_eq!(n(json!({"msisdn": " "})), None);
        assert_eq!(n(json!({"msisdn": "unknown"})), None);
        assert_eq!(n(json!({})), None);
        assert_eq!(super::own_number(None), None);
    }

    #[test]
    fn delivery_reports_are_read_from_the_sqlite_lines() {
        let out = "2\u{1f}10086\u{1f}1\u{1f}26\u{1f}10\u{1f}27\u{1f}18\u{1f}30\u{1f}05\n\
                   3\u{1f}10010\u{1f}0\u{1f}26\u{1f}10\u{1f}27\u{1f}18\u{1f}31\u{1f}05\n\
                   broken\n";
        let r = super::parse_reports(out);
        assert_eq!(r.len(), 2);
        assert_eq!(
            r[0],
            json!({"id": 2, "number": "10086", "ok": true, "at": 1_793_125_805})
        );
        assert_eq!(r[1]["ok"], false);
    }

    #[test]
    fn firmware_dates_read_back_as_the_device_clock() {
        assert_eq!(super::wall_of("26,10,27,18,30,05,+32"), Some(1_793_125_805));
        assert_eq!(super::wall_of("00;01;01;00;00;00;-5"), Some(946_684_800));
        assert_eq!(super::wall_of("26,13,01,00,00,00"), None);
        assert_eq!(super::wall_of(""), None);
    }

    #[test]
    fn removal_finds_received_ones_exactly_and_sent_copies_by_time() {
        // 号码、内容都是假的
        let hex = |t: &str| super::ucs2_hex(&t.encode_utf16().collect::<Vec<_>>());
        let list = vec![
            json!({"id": 1, "number": "10086", "date": "26,10,27,18,30,05,+32", "content": hex("A"), "tag": "0"}),
            json!({"id": 2, "number": "10086", "date": "26,10,27,18,31,05,+32", "content": hex("A"), "tag": "1"}),
            json!({"id": 3, "number": "10010", "date": "26,10,27,18,30,10,+32", "content": hex("B"), "tag": "2"}),
            json!({"id": 4, "number": "10010", "date": "26,10,27,18,40,10,+32", "content": hex("B"), "tag": 2}),
            json!({"id": 5, "number": "10010", "date": "26,10,27,18,30,09,+32", "content": hex("B"), "tag": "1"}),
        ];
        let want = vec![
            json!({"from": "10086", "date": "26,10,27,18,30,05,+32", "text": "A"}),
            json!({"to": "10010", "text": "B", "at": 1_793_125_805}),
            // 第二条同样的：剩下的 4 离得太远，不配
            json!({"to": "10010", "text": "B", "at": 1_793_125_805}),
            json!({"from": "10086", "date": "26,10,27,18,30,05,+32", "text": "other"}),
        ];
        assert_eq!(super::matching(&list, &want), vec![1, 3]);
    }

    #[test]
    fn the_send_arguments_are_in_the_firmware_format() {
        // 2026-10-27 18:30:05（设备时钟，当地时间）
        assert_eq!(super::sms_time(1_793_125_805, 8), "26;10;27;18;30;05;+8");
        assert_eq!(super::sms_time(946_684_800, -5), "00;01;01;00;00;00;-5");
        let units: Vec<u16> = "测A😀".encode_utf16().collect();
        assert_eq!(super::ucs2_hex(&units), "6D4B0041D83DDE00");
    }

    #[test]
    fn the_firmware_reply_counts_as_accepted_like_alert_sms() {
        assert!(super::accepted(None));
        assert!(super::accepted(Some(&json!(3))));
        assert!(super::accepted(Some(&json!("3"))));
        assert!(!super::accepted(Some(&json!(2))));
        assert!(!super::accepted(Some(&json!("failure"))));
    }

    #[tokio::test]
    async fn a_send_without_text_is_refused_before_the_firmware() {
        let e = super::send(&json!({"to": "10086", "text": ""}))
            .await
            .unwrap_err();
        assert_eq!(e.0, "invalid_parameter");
        let e = super::send(&json!({"to": "10086"})).await.unwrap_err();
        assert_eq!(e.0, "invalid_parameter");
    }

    #[tokio::test]
    async fn events_go_only_to_a_plugin_that_declared_them() {
        let (hub, path, _d) = server(me()).await;
        let (mut r, _w) = plugin(&path, hello(&["sms"])).await;
        next(&mut r).await;
        wait_connected(&hub).await;
        hub.event("sms", json!({"generation":"g","seq":3}));
        hub.event("other", json!({}));
        hub.event("sms", json!({"generation":"g","seq":4}));
        assert_eq!(next(&mut r).await.unwrap()["seq"], 3);
        assert_eq!(next(&mut r).await.unwrap()["seq"], 4);
        // 没连上：什么都不做
        Hub::default().event("sms", json!({}));
    }
}
