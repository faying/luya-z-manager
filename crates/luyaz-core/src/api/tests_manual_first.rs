//! Step 9g: manual-first, u60d's with writes on.

use super::*;
use std::fs;
use std::time::Duration;

async fn wait(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never got there: {what}");
}

#[tokio::test]
async fn u60d_runs_manual_first() {
    let root = std::env::temp_dir().join(format!("u60d-mf-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("data/chill")).unwrap();
    fs::create_dir_all(root.join("tmp")).unwrap();
    // the agent left it on the Japan backup; CHILL is not running, so the loop asks mihomo nothing
    fs::write(
        root.join("data/chill/manual-first.json"),
        r#"{"backup":"🇯🇵 日本","manual":"jp-1","switched_at":1,"switches":[1],"last_region":"🇯🇵 日本",
            "notice":"","notice_at":0}"#,
    )
    .unwrap();
    let c = Compat::new(root.to_str().unwrap(), Sources::none()).with_writes(true);
    tokio::spawn(crate::engines::manual_first::run(c.clone()));

    // u60d runs it, from the file left behind
    wait("started", || {
        c.manual_first
            .live()
            .is_some_and(|m| !m.backup().is_empty())
    })
    .await;
    assert_eq!(c.manual_first.live().unwrap().backup(), "🇯🇵 日本");
    let lease = root.join("tmp/manual-first.owner");
    assert!(fs::read_to_string(&lease).unwrap().starts_with(&std::process::id().to_string()));
    let _ = fs::remove_dir_all(&root);
}
