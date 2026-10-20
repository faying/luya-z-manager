use std::io::Write;

use serde_json::Value;

/// The real binary: stdout has protocol lines only, logs go to stderr; an unreachable device is
/// an answer.
#[test]
fn stdout_is_protocol_only() {
    use std::process::{Command, Stdio};
    let mut c = Command::new(env!("CARGO_BIN_EXE_luyaz-mcp"))
        // nothing listens on port 9 here: refused at once
        .env("LUYAZ_MCP_URL", "http://127.0.0.1:9")
        .env("LUYAZ_MCP_TOKEN_FILE", "/nonexistent")
        .env("LUYAZ_MCP_DEVICES", "/nonexistent/devices")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#, "\n",
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_device_health","arguments":{}}}"#, "\n",
                "not json\n",
            )
            .as_bytes(),
        )
        .unwrap();
    let o = c.wait_with_output().unwrap();
    let out = String::from_utf8(o.stdout).unwrap();
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(lines[0]["id"], 1);
    assert!(
        lines[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cannot be reached")
    );
    assert_eq!(lines[2]["error"]["code"], -32700);
    assert!(String::from_utf8(o.stderr).unwrap().contains("[luyaz-mcp]"));
}

/// A stand-in admin web: answers `/agent-api/health` with the device id that belongs to the
/// bearer token (`tok-a` → `aaaaaaaaaaaa`, `tok-b` → `bbbbbbbbbbbb`; `tok-old` = no id).
fn stand_in() -> u16 {
    use std::io::{BufRead, BufReader};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut auth = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(a) = line.strip_prefix("Authorization: Bearer ") {
                        auth = a.trim().to_string();
                    }
                }
                let body = match auth.as_str() {
                    "tok-a" => r#"{"core":"up","device_id":"aaaaaaaaaaaa"}"#,
                    "tok-b" => r#"{"core":"up","device_id":"bbbbbbbbbbbb"}"#,
                    _ => r#"{"core":"up"}"#,
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    port
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("luyaz-mcp-stdio-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for (f, t) in [
        ("a.token", "tok-a"),
        ("b.token", "tok-b"),
        ("old.token", "tok-old"),
    ] {
        std::fs::write(d.join(f), format!("{t}\n")).unwrap();
    }
    d
}

/// R5 + O2: two `enroll` processes at once, for different devices: both ids are kept; an old
/// admin web (no device_id) is refused with a reason; a listed device is then called by name.
#[test]
fn enroll_two_at_once_then_call_by_name() {
    use std::process::{Command, Stdio};
    let port = stand_in();
    let d = tmp("enroll");
    let list = d.join("devices");
    std::fs::write(
        &list,
        format!("# test\nhome 127.0.0.1:{port} a.token\ntravel 127.0.0.1:{port} b.token # far\nold 127.0.0.1:{port} old.token\n"),
    )
    .unwrap();
    let enroll = |name: &str| {
        Command::new(env!("CARGO_BIN_EXE_luyaz-mcp"))
            .args(["enroll", name])
            .env("LUYAZ_MCP_DEVICES", &list)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let (a, b) = (enroll("home"), enroll("travel"));
    let (a, b) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
    assert!(a.status.success() && b.status.success(), "{a:?} {b:?}");
    let o = enroll("old").wait_with_output().unwrap();
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("too old"),
        "{o:?}"
    );
    let t = std::fs::read_to_string(&list).unwrap();
    assert!(
        t.contains(&format!("home 127.0.0.1:{port} a.token aaaaaaaaaaaa\n")),
        "{t}"
    );
    assert!(
        t.contains(&format!(
            "travel 127.0.0.1:{port} b.token bbbbbbbbbbbb # far\n"
        )),
        "{t}"
    );
    assert!(t.starts_with("# test\n") && t.contains(&format!("old 127.0.0.1:{port} old.token\n")));

    // now through the server: by name, confirmed; travel's token on home's line → refused
    let ask = |args: &str| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_luyaz-mcp"))
            .env("LUYAZ_MCP_DEVICES", &list)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"get_device_health","arguments":{args}}}}}"#
        );
        c.stdin
            .take()
            .unwrap()
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
        let o = c.wait_with_output().unwrap();
        let v: Value = serde_json::from_str(String::from_utf8(o.stdout).unwrap().trim()).unwrap();
        v["result"].clone()
    };
    let r = ask(r#"{"device":"travel"}"#);
    let v: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        (
            v["device"].as_str(),
            v["identity"].as_str(),
            v["device_id"].as_str()
        ),
        (Some("travel"), Some("confirmed"), Some("bbbbbbbbbbbb"))
    );
    let r = ask("{}");
    assert_eq!(r["structuredContent"]["error"], "device_required");
    std::fs::write(
        &list,
        t.replace("a.token aaaaaaaaaaaa", "b.token aaaaaaaaaaaa"),
    )
    .unwrap();
    let r = ask(r#"{"device":"home"}"#);
    assert_eq!(r["structuredContent"]["error"], "wrong_device", "{r}");
    let _ = std::fs::remove_dir_all(&d);
}
