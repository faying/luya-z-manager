//! Emergency writes while datad is gone (write-op-layer.md D19, D23): the
//! agent runs the same `u60-fallback.sh` the touch screen does. The script
//! checks that datad really is gone (D18), takes the write lock (D29), leaves
//! the takeover marker and writes; it refuses when datad is there.
//!
//! Only writes that affect connectivity (10-03 user decision): the ones a
//! person would have to fix by hand if they went wrong. Everything else
//! answers "数据服务没响应" until datad is back.
//!
//! Script path: `ZTE_AGENT_FALLBACK_SCRIPT`, default
//! `/data/u60-guard/u60-fallback.sh` (shipped with the touch-ui scripts).

use std::process::Command;

use serde_json::Value;

use crate::datad_write::{Reply, Source};

/// Actions the agent may write without datad. Each must be one the script
/// knows (touch-ui `scripts/u60-fallback.sh`).
const COVERED: &[&str] = &[
    "cellular.set",
    "network.set_mode",
    "band.set_lte",
    "band.set_nr_sa",
    "band.set_nr_nsa",
    "band.reset",
    "cell.lock_lte",
    "cell.lock_nr",
    "apn.set_mode",
    "apn.enable",
    "netselect.auto",
    "modem.online",
    "cellular.redial",
];

pub fn covers(action: &str) -> bool {
    COVERED.contains(&action)
}

fn script() -> String {
    std::env::var("ZTE_AGENT_FALLBACK_SCRIPT").unwrap_or_else(|_| "/data/u60-guard/u60-fallback.sh".into())
}

/// The script's `key=value` arguments for `params`: booleans as 0/1, numbers
/// and strings as they are (the script checks them). Nested values: none.
pub fn args(params: &Value) -> Option<Vec<String>> {
    let Value::Object(m) = params else {
        return params.is_null().then(Vec::new);
    };
    m.iter()
        .map(|(k, v)| {
            let v = match v {
                Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.clone(),
                _ => return None,
            };
            Some(format!("{k}={v}"))
        })
        .collect()
}

/// The script's exit code → [`Reply`].
pub fn reply(code: Option<i32>, out: &str) -> Reply {
    let line = out.lines().next().unwrap_or("").trim().to_string();
    match code {
        Some(0) => Reply::Fallback,
        // 3: datad is there after all (alive but not answering): not written
        Some(3) => Reply::Unreachable { error: format!("fallback: {line}"), maybe_done: false },
        // 4: the write lock stayed busy: datad is coming back
        Some(4) => Reply::Busy { doing: None },
        _ => Reply::Failed { error: format!("fallback: {}", if line.is_empty() { "script failed".into() } else { line }), op: None },
    }
}

pub fn run(source: Source, action: &str, params: &Value) -> Reply {
    let Some(kv) = args(params) else {
        return Reply::Invalid("fallback: parameters must be flat".into());
    };
    match Command::new("sh").arg(script()).args(["--by", source.as_str(), action]).args(&kv).output() {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout);
            eprintln!("fallback_write: {action} {kv:?} → {:?} {}", o.status.code(), out.trim());
            reply(o.status.code(), &out)
        }
        Err(e) => Reply::Unreachable { error: format!("fallback: {e}"), maybe_done: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn covered_actions() {
        assert!(covers("network.set_mode") && covers("cellular.set") && covers("band.reset"));
        assert!(covers("cell.lock_nr") && covers("apn.enable") && covers("apn.set_mode"));
        // the rest wait for datad (10-03: only connectivity)
        assert!(!covers("sms.send_raw") && !covers("power.direct_supply.set") && !covers("lan.set"));
    }

    #[test]
    fn arguments() {
        assert_eq!(args(&json!({"enabled": true, "roaming": false})).unwrap(), vec!["enabled=1", "roaming=0"]);
        assert_eq!(args(&json!({"mode": "Only_LTE"})).unwrap(), vec!["mode=Only_LTE"]);
        assert_eq!(args(&json!({"connect_mode": "1"})).unwrap(), vec!["connect_mode=1"]);
        assert_eq!(args(&json!({})).unwrap(), Vec::<String>::new());
        assert_eq!(args(&Value::Null).unwrap(), Vec::<String>::new());
        assert!(args(&json!({"x": {"y": 1}})).is_none());
        assert!(args(&json!([1])).is_none());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(reply(Some(0), "ok\n"), Reply::Fallback);
        assert!(matches!(reply(Some(3), "datad running: not written"), Reply::Unreachable { maybe_done: false, .. }));
        assert_eq!(reply(Some(4), "busy: being taken over"), Reply::Busy { doing: None });
        assert_eq!(reply(Some(1), "failed: zwrt_data set_wwaniface"), Reply::Failed { error: "fallback: failed: zwrt_data set_wwaniface".into(), op: None });
        assert!(matches!(reply(None, ""), Reply::Failed { .. }));
    }

    /// The real script, run against a stand-in ubus: what the agent sends is
    /// what the script accepts (argument names and value forms).
    #[test]
    fn the_script_accepts_what_we_send() {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../touch-ui/scripts/u60-fallback.sh");
        if !script.exists() || Command::new("flock").arg("-h").output().is_err() {
            eprintln!("skipped: no touch-ui checkout or no flock here");
            return;
        }
        let t = std::env::temp_dir().join(format!("agent-fallback-{}", std::process::id()));
        std::fs::create_dir_all(t.join("ops")).unwrap();
        let ubus = t.join("ubus");
        std::fs::write(&ubus, format!("#!/bin/sh\necho \"$2 $3 $4\" >>{}/calls\ncase $3 in get_wwaniface) echo '{{\"enable\":1}}';; esac\n", t.display())).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ubus, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for (action, params, sent) in [
            ("network.set_mode", json!({"mode":"Only_LTE"}), r#"nwinfo_set_netselect {"net_select":"Only_LTE"}"#),
            ("cellular.set", json!({"enabled":false}), r#"set_wwaniface {"enable":0,"source_module":"WEBUI","cid":1}"#),
            ("band.set_nr_sa", json!({"bands":"78"}), r#"nwinfo_set_nrbandlock {"nr5g_type":"0","nr5g_band":"78"}"#),
            ("cell.lock_nr", json!({"pci":"5","arfcn":"627264","band":"78"}), r#"nwinfo_lock_nr_cell {"lock_nr_pci":"5","lock_nr_earfcn":"627264","lock_nr_cell_band":"78"}"#),
            ("apn.set_mode", json!({"mode":1}), r#"set_apn_mode {"apn_mode":1}"#),
            ("apn.enable", json!({"profile_id":"2"}), r#"enable_manu_apn_id {"profileId":"2"}"#),
            ("wifi.radio", json!({"ap_2g":1,"ap_5g":0}), r#"reload {}"#),
        ] {
            let _ = std::fs::remove_file(t.join("calls"));
            let o = Command::new("sh")
                .arg(&script)
                .args(["--by", "web", action])
                .args(args(&params).unwrap())
                .env("U60_FALLBACK_UBUS", &ubus)
                .env("U60_FALLBACK_UCI", "true")
                .env("U60_FALLBACK_HEALTH_URL", "")
                .env("U60_FALLBACK_GAP", "0")
                .env("ZWRT_DATAD_WRITE_LOCK", t.join("lock"))
                .env("ZWRT_DATAD_PID_FILE", t.join("pid"))
                .env("ZWRT_DATAD_OPS_DIR", t.join("ops"))
                .output()
                .unwrap();
            assert_eq!(reply(o.status.code(), &String::from_utf8_lossy(&o.stdout)), Reply::Fallback, "{action}: {}", String::from_utf8_lossy(&o.stdout));
            let calls = std::fs::read_to_string(t.join("calls")).unwrap();
            assert!(calls.lines().any(|l| l.ends_with(sent)), "{action}: {calls}");
        }
        let marks = std::fs::read_to_string(t.join("ops/takeover")).unwrap();
        assert_eq!(marks.lines().count(), 7);
        assert!(marks.contains(r#""by":"web""#));
        let _ = std::fs::remove_dir_all(&t);
    }
}
