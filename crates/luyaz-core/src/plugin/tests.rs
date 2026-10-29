use super::*;
use std::fs;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("u60d-plugin-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn manifest(dir: &Path, entry: serde_json::Value) {
    fs::write(
        dir.join("plugins.json"),
        serde_json::json!({"plugins": [entry]}).to_string(),
    )
    .unwrap();
}

#[test]
fn no_manifest_or_no_entry_is_quietly_builtin() {
    let d = tmp("none");
    assert_eq!(
        load_checked(&d, "battery-eta", "luyaz:policy@0.1.0"),
        Err(String::new())
    );
    manifest(
        &d,
        serde_json::json!({"name": "other", "abi": "x", "file": "o.cwasm", "sha256": ""}),
    );
    assert_eq!(
        load_checked(&d, "battery-eta", "luyaz:policy@0.1.0"),
        Err(String::new())
    );
}

#[test]
fn checks_abi_file_name_and_hash() {
    let d = tmp("checks");
    fs::write(d.join("b.cwasm"), b"bytes").unwrap();
    let sha = hex(&Sha256::digest(b"bytes"));
    let entry = |abi: &str, file: &str, sha: &str| serde_json::json!({"name": "b", "abi": abi, "file": file, "sha256": sha});

    manifest(&d, entry("luyaz:policy@0.0.9", "b.cwasm", &sha));
    assert!(
        load_checked(&d, "b", "luyaz:policy@0.1.0")
            .unwrap_err()
            .starts_with("abi luyaz:policy@0.0.9")
    );
    for bad in ["../b.cwasm", "/data/b.cwasm", ".b", ""] {
        manifest(&d, entry("luyaz:policy@0.1.0", bad, &sha));
        assert!(
            load_checked(&d, "b", "luyaz:policy@0.1.0")
                .unwrap_err()
                .starts_with("bad file name"),
            "{bad}"
        );
    }
    manifest(&d, entry("luyaz:policy@0.1.0", "b.cwasm", &"0".repeat(64)));
    assert!(
        load_checked(&d, "b", "luyaz:policy@0.1.0")
            .unwrap_err()
            .contains("does not match")
    );
    manifest(&d, entry("luyaz:policy@0.1.0", "b.cwasm", &sha));
    assert_eq!(
        load_checked(&d, "b", "luyaz:policy@0.1.0"),
        Ok((b"bytes".to_vec(), sha))
    );
}

#[test]
fn engine_builds_with_the_shared_config() {
    engine().unwrap();
}

/// Heat care: the screen resumes charging through /control directly; that is a manual resume
/// (owner 10-29), a web one too; the core's own writes and stops are not.
#[test]
fn a_hand_written_resume_is_a_manual_resume() {
    use super::charge::manual_resume_in;
    use serde_json::{Value, json};
    let w = |source: &str, enabled: Value| json!({"source": source, "params": {"enabled": enabled}});
    assert!(manual_resume_in(&w("screen", json!(false))));
    assert!(manual_resume_in(&w("web", json!("0"))));
    assert!(!manual_resume_in(&w("screen", json!(true))), "a stop");
    assert!(!manual_resume_in(&w("auto", json!(false))), "the core's own");
    assert!(!manual_resume_in(&json!({"params": {"enabled": false}})), "no source");
}
