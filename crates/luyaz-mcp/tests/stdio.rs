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
