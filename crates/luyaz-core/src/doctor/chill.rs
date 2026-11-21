//! CHILL's local management listener check.

use super::{Row, row};

pub struct Cfg {
    pub env: String,
    pub template: String,
    pub dir: String,
}

pub fn rows(c: &super::Cfg) -> Vec<Row> {
    if std::path::Path::new(&c.chill.env).is_file() {
        vec![api(c)]
    } else {
        Vec::new()
    }
}

/// `cat tcp tcp6 | awk '$4 == "0A" && $2 ~ /:270F$/ {…print $2}' | sort -u`
fn listeners(c: &super::Cfg) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for f in ["net/tcp", "net/tcp6"] {
        let Ok(b) = std::fs::read(format!("{}/{f}", c.proc_dir)) else {
            continue;
        };
        for l in String::from_utf8_lossy(&b).split('\n') {
            let w: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
            if w.len() >= 4
                && w[3] == "0A"
                && let Some(a) = w[1].strip_suffix(":270F")
            {
                set.insert(a.to_owned());
            }
        }
    }
    set.into_iter().collect()
}

fn api(c: &super::Cfg) -> Row {
    let api = listeners(c);
    let mut apiw = String::new();
    for a in &api {
        match a.as_str() {
            "0100007F"
            | "00000000000000000000000001000000"
            | "0000000000000000FFFF00000100007F" => {}
            "00000000" => apiw.push_str(" 0.0.0.0"),
            "00000000000000000000000000000000" => apiw.push_str(" ::"),
            x => {
                apiw.push(' ');
                apiw.push_str(x);
            }
        }
    }
    if !apiw.is_empty() {
        row(
            "warn",
            "chill-api",
            "CHILL 管理接口",
            format!("对局域网开放（:9999 听在{apiw}）；chill.env 里 CHILL_API_LAN 应为 0"),
            "CHILL API",
            format!("Open to the LAN (:9999 on{apiw}); set CHILL_API_LAN=0 in chill.env"),
        )
    } else if !api.is_empty() {
        row(
            "ok",
            "chill-api",
            "CHILL 管理接口",
            "只听本机".into(),
            "CHILL API",
            "This device only".into(),
        )
    } else {
        row(
            "ok",
            "chill-api",
            "CHILL 管理接口",
            "没在监听（CHILL 没开）".into(),
            "CHILL API",
            "Not listening (CHILL off)".into(),
        )
    }
}
