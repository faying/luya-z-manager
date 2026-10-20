use super::slow_signal::*;
use luyaz_api::execution::slow_signal::{Advice, replay};
use serde_json::json;

fn run(state: &str, age: u64, cell: i64, wifi: &str) -> serde_json::Value {
    json!({"state": state, "age_s": age, "key": {"plmn": "460-11", "cell": cell, "ch": 1, "hour": 3},
    "layers": [
        {"id": "wifi", "level": wifi}, {"id": "signal", "level": "ok"},
        {"id": "crowd", "level": "warn"}, {"id": "e2e", "level": "blocked"},
        {"id": "dns", "level": "weird"}
    ]})
}

#[test]
fn a_fresh_check_on_this_cell_explains() {
    let e = evidence(&run("done", 40, 7, "bad"), Some(7));
    assert_eq!(e.same_cell, Some(true));
    assert_eq!(e.layers.len(), 4, "an unknown level is left out");
    assert_eq!(
        replay(&e),
        Advice::Explain {
            proven: vec!["wifi"],
            possible: vec!["crowd"]
        }
    );
}

#[test]
fn moved_running_idle_or_unread_cell() {
    let e = evidence(&run("done", 40, 7, "bad"), Some(8));
    assert_eq!(
        replay(&e),
        Advice::Unknown {
            why: "cell_changed"
        }
    );
    let e = evidence(&run("done", 40, 7, "bad"), None);
    assert_eq!(
        replay(&e),
        Advice::Unknown {
            why: "cell_unknown"
        }
    );
    let e = evidence(&run("running", 0, 7, "bad"), Some(7));
    assert_eq!(
        replay(&e),
        Advice::Unknown {
            why: "check_running"
        }
    );
    let e = evidence(&json!({"state": "idle"}), Some(7));
    assert_eq!(replay(&e), Advice::Unknown { why: "no_check" });
    let e = evidence(&serde_json::Value::Null, Some(7));
    assert_eq!(replay(&e), Advice::Unknown { why: "no_check" });
}

#[test]
fn the_cell_now_follows_the_runs_key_rule() {
    assert_eq!(
        cell_now(&json!({"type": "SA", "nr_cell_id": 23994463360_i64, "lte_cell_id": 5})),
        Some(23994463360)
    );
    assert_eq!(
        cell_now(&json!({"type": "NSA", "nr_cell_id": 9, "lte_cell_id": "12"})),
        Some(12)
    );
    assert_eq!(cell_now(&json!({"type": "SA", "nr_cell_id": 0})), None);
    // the type spellings the run's key normalises the same way
    assert_eq!(
        cell_now(&json!({"type": "NR", "nr_cell_id": 3, "lte_cell_id": 4})),
        Some(3)
    );
    assert_eq!(
        cell_now(&json!({"type": "ENDC", "nr_cell_id": 3, "lte_cell_id": 4})),
        Some(4)
    );
    assert_eq!(cell_now(&json!({"type": "WCDMA", "lte_cell_id": 4})), None);
    assert_eq!(cell_now(&json!({})), None);
}
