//! 「恢复上次能用的设置」 on the deep check page (restore-verified-connection.md step 3): the
//! core's `GET /api/verified` as rows, and the one button (`/control verified.restore`, source
//! web). The core decides everything; this only words it.

use serde_json::Value;

use crate::i18n::Lang;
use crate::topbar::Via;
use crate::write;

/// The write area under the page (tier 3: a dialog, with the remote warning like /bandlock).
pub fn area(l: Lang, remote: Via, phase: &write::Phase) -> String {
    write::Area {
        id: "dgrs",
        l,
        remote,
        set: "/tools/diagnose/restore",
        cancel: "/tools/diagnose/restore/cancel",
        phase,
    }
    .html()
}

/// Several network settings in a row, the APN among them maybe: the strongest tier. Nothing
/// promises a revert for the whole run, so the remote warning is the plain one.
pub fn confirm(l: Lang) -> write::Confirm {
    write::Confirm {
        tier: write::Tier::Dialog,
        title: l
            .pick(
                "Restore the last settings that worked?",
                "恢复上次能用的设置？",
            )
            .into(),
        what: l
            .pick(
                "The items listed on the page are set back one at a time, in that order, each as its own change.",
                "页面上列出的几项按顺序一项一项改回去，每项都是一次单独的改动。",
            )
            .into(),
        downtime: Some(
            l.pick(
                "The data connection may drop for a while at each step.",
                "每一步数据连接都可能断开一阵。",
            )
            .into(),
        ),
        recovery: Some(
            l.pick(
                "It stops at the first item that doesn't take; the ones already done stay. Nothing else is changed (no redial, no reboot, no eSIM switch).",
                "哪一项没生效就停下，已完成的保持不变；不会再改别的（不重拨、不重启、不切 eSIM）。",
            )
            .into(),
        ),
        action: l.pick("Restore", "恢复").into(),
        cuts_uplink: true,
        word: None,
        danger: false,
        revert: write::Revert::No,
    }
}

pub struct Row {
    pub name: &'static str,
    pub now: String,
    pub to: String,
    pub back: &'static str,
}

pub struct RestoreV {
    pub at: Option<String>,
    pub rows: Vec<Row>,
    /// Why the button is off; None = it may run.
    pub why: Option<&'static str>,
    /// An APN step is in it: a separate tick (APN changes never roll back by themselves).
    pub apn: bool,
    /// While or after a restore: (item, state) per step, and the end.
    pub progress: Vec<(&'static str, String)>,
    pub end: Option<&'static str>,
}

fn name(item: &str, l: Lang) -> &'static str {
    match item {
        "cell.lte" => l.pick("4G cell lock", "4G 锁小区"),
        "cell.nr" => l.pick("5G cell lock", "5G 锁小区"),
        "band.lte" => l.pick("4G bands", "4G 频段"),
        "band.nr" => l.pick("5G bands", "5G 频段"),
        "network.mode" => l.pick("Network mode", "网络制式"),
        "apn" => "APN",
        _ => "?",
    }
}

fn val(item: &str, v: &Value, l: Lang) -> String {
    if v.is_null() {
        return l.pick("unreadable", "读不到").into();
    }
    match item {
        "cell.lte" | "cell.nr" if v["off"] == true => l.pick("not locked", "不锁").into(),
        "cell.lte" | "cell.nr" => {
            let ch = v["earfcn"].as_str().or(v["arfcn"].as_str()).unwrap_or("?");
            format!("PCI {} · {ch}", v["pci"].as_str().unwrap_or("?"))
        }
        "band.nr" => format!(
            "SA {} · NSA {}",
            v["sa"].as_str().unwrap_or("?"),
            v["nsa"].as_str().unwrap_or("?")
        ),
        "apn" if v["mode"] == 0 => l.pick("automatic", "自动").into(),
        "apn" => format!(
            "{} {}",
            l.pick("manual", "手动"),
            v["manual"].as_str().unwrap_or("")
        ),
        _ => v.as_str().map_or_else(|| v.to_string(), str::to_owned),
    }
}

fn why(w: &str, l: Lang) -> &'static str {
    match w {
        "no_country" => l.pick(
            "The current country can't be read, so nothing is restored",
            "读不到现在在哪个国家，不能恢复",
        ),
        "no_record" => l.pick(
            "Nothing recorded for this card in this country yet: run a check once while the network works",
            "这张卡在这个国家还没有记录：网络正常时按一次「开始检查」就会记下",
        ),
        "txn_active" => l.pick("Another change is running", "另一项改动正在进行"),
        "already_there" => l.pick(
            "The settings already match the last set that worked",
            "现在的设置就是上次能用的那一套",
        ),
        "busy" => l.pick("Checking or restoring now", "正在检查或恢复中"),
        _ => l.pick("Can't read the settings now", "现在读不到设置"),
    }
}

fn state(s: &Value, l: Lang) -> String {
    let w = match s["state"].as_str().unwrap_or("") {
        "waiting" => l.pick("waiting", "等待"),
        "running" => l.pick("in progress", "进行中"),
        "done" => l.pick("done", "完成"),
        "failed" => l.pick("stopped here", "停在这里"),
        _ => "",
    };
    match s["why"].as_str() {
        Some("apn_profile_gone") => format!(
            "{w} · {}",
            l.pick("that APN profile is gone", "那条 APN 已经不在了")
        ),
        Some("sim_changed") => format!("{w} · {}", l.pick("the card changed", "换卡了")),
        Some(_) => format!("{w} · {}", l.pick("not done", "没有执行")),
        None => w.into(),
    }
}

pub fn view(v: &Value, l: Lang) -> Option<RestoreV> {
    let progress: Vec<(&'static str, String)> = v["progress"]["steps"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| (name(s["item"].as_str().unwrap_or(""), l), state(s, l)))
                .collect()
        })
        .unwrap_or_default();
    let end = match v["progress"]["end"].as_str() {
        Some("restored") => Some(l.pick(
            "Restored, and the network works",
            "已恢复，网络通了",
        )),
        Some("restored_still_down") => Some(l.pick(
            "Settings restored, but the network still doesn't work. Nothing else was changed",
            "设置已恢复，但网络仍不通。没有再改别的",
        )),
        Some(_) => Some(l.pick(
            "Stopped; the steps after it were not done. The finished ones stay",
            "停下了，后面的步骤没做；已完成的保持不变",
        )),
        None => None,
    };
    let at = v["at"].as_i64().map(crate::alerts_page::mdhm);
    let w = v["why"].as_str();
    if at.is_none() && progress.is_empty() && w != Some("no_record") {
        return None;
    }
    let rows: Vec<Row> = v["steps"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| {
                    let item = s["item"].as_str().unwrap_or("");
                    Row {
                        name: name(item, l),
                        now: val(item, &s["now"], l),
                        to: val(item, &s["to"], l),
                        back: match s["rollback"].as_str() {
                            Some("proven") => l.pick("rolls back if it fails", "失败会自动退回"),
                            Some("unsupported") => l.pick("no automatic rollback", "不能自动退回"),
                            _ => l.pick("rollback not proven", "退回未验证"),
                        },
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Some(RestoreV {
        at,
        apn: v["steps"]
            .as_array()
            .is_some_and(|a| a.iter().any(|s| s["item"] == "apn")),
        rows,
        why: (v["available"] != true).then(|| why(w.unwrap_or(""), l)),
        progress,
        end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_say_what_changes_and_how_it_rolls_back() {
        let v = json!({"available": true, "at": 1792483200, "steps": [
            {"item": "cell.nr", "now": {"pci": "1", "arfcn": "627264", "band": "78"}, "to": {"off": true}, "rollback": "proven"},
            {"item": "apn", "now": {"mode": 1, "manual": "manu2", "fp": "x"}, "to": {"mode": 0, "manual": "", "fp": ""}, "rollback": "unsupported"},
        ], "progress": null});
        let r = view(&v, Lang::Zh).unwrap();
        assert!(r.why.is_none() && r.apn);
        assert_eq!(r.rows[0].now, "PCI 1 · 627264");
        assert_eq!(r.rows[0].to, "不锁");
        assert_eq!(r.rows[1].to, "自动");
        assert_eq!(r.rows[1].back, "不能自动退回");
    }

    #[test]
    fn nothing_without_a_record_except_the_hint() {
        assert!(view(&json!({"available": false, "why": "no_country"}), Lang::En).is_none());
        let r = view(&json!({"available": false, "why": "no_record"}), Lang::Zh).unwrap();
        assert!(r.why.unwrap().contains("开始检查"));
        let p = json!({"available": false, "why": "busy", "progress": {"steps": [
            {"item": "band.lte", "state": "done"}, {"item": "apn", "state": "failed", "why": "apn_profile_gone"}
        ], "end": "stopped"}});
        let r = view(&p, Lang::Zh).unwrap();
        assert_eq!(r.progress[1].1, "停在这里 · 那条 APN 已经不在了");
        assert!(r.end.unwrap().starts_with("停下了"));
    }
}
