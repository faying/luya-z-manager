//! `<core> regress …` on the device (design §4; CEO review S3, OV-C4):
//!
//! - `accept <regress-mem|regress-power>`: the running core's control route, as the owner;
//! - `limit mem <heap MiB> <anon MiB>`, `limit power <mW>`: the absolute bars, only from here;
//! - `sms on|off`: the regress SMS switch both senders read.

use std::io::{Read, Write};
use std::path::PathBuf;

const CORE: &str = "127.0.0.1:9460";

fn usage() -> i32 {
    eprintln!(
        "usage: regress accept <regress-mem|regress-power>\n       regress limit mem <heap MiB> <anon MiB>\n       regress limit power <mW>\n       regress sms on|off"
    );
    2
}

/// One HTTP/1.0 POST to the core on loopback; the reply body.
fn post(path: &str, body: &str) -> std::io::Result<String> {
    let mut s = std::net::TcpStream::connect(CORE)?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    write!(
        s,
        "POST {path} HTTP/1.0\r\nHost: {CORE}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    Ok(out
        .split_once("\r\n\r\n")
        .map_or(out.clone(), |(_, b)| b.to_string()))
}

pub fn main(args: &[String]) -> i32 {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let long = PathBuf::from(crate::state::host_path(crate::longterm::DIR));
    let num = |s: &str| s.parse::<f64>().ok();
    match a.as_slice() {
        ["accept", kind] => {
            let body = serde_json::json!({"action": "regress.accept", "source": "cli",
                                          "params": {"kind": kind}})
            .to_string();
            match post("/control", &body) {
                Ok(r) => {
                    let v: serde_json::Value = serde_json::from_str(&r).unwrap_or_default();
                    if v["ok"] == true {
                        println!("{}", v["data"]["said"].as_str().unwrap_or(&r));
                        0
                    } else {
                        eprintln!(
                            "regress accept: {}",
                            v["error"]["message"].as_str().unwrap_or(&r)
                        );
                        1
                    }
                }
                Err(e) => {
                    eprintln!("regress accept: the core does not answer on {CORE}: {e}");
                    1
                }
            }
        }
        ["limit", "mem", h, an] => match (num(h), num(an)) {
            (Some(h), Some(an)) => say(super::io::set_limits(&long, Some((h, an)), None)),
            _ => usage(),
        },
        ["limit", "power", mw] => match num(mw) {
            Some(mw) => say(super::io::set_limits(&long, None, Some(mw))),
            None => usage(),
        },
        ["sms", onoff @ ("on" | "off")] => {
            let p = PathBuf::from(crate::state::host_path(luyaz_api::alerts::REGRESS_SMS_OFF));
            let r = if *onoff == "off" {
                std::fs::write(&p, "")
            } else {
                match std::fs::remove_file(&p) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                    _ => Ok(()),
                }
            };
            say(r
                .map(|()| format!("regress SMS {onoff} (inbox and MCP unchanged)"))
                .map_err(|e| e.to_string()))
        }
        _ => usage(),
    }
}

fn say(r: Result<String, String>) -> i32 {
    match r {
        Ok(s) => {
            println!("{s}");
            0
        }
        Err(e) => {
            eprintln!("regress: {e}");
            1
        }
    }
}
