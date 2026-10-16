//! `Json`'s text must be serde_json's: restore keys built from it are compared with the ones
//! the agent wrote.

use super::*;
use serde_json::json;

#[test]
fn display_is_serde_json_compact_output() {
    for v in [
        json!(null),
        json!({"ap_2g": true, "ap_5g": false}),
        json!({"member": "DIRECT", "group": "\u{1F680} \u{8282}\u{70B9}\u{9009}\u{62E9}"}),
        json!({"z": 1, "a": [1, -2, 3.5, 1.0, 1e300, 18446744073709551615u64], "m": {}}),
        json!(
            "quote \" backslash \\ newline \n tab \t cr \r bs \u{8} ff \u{c} nul \u{0} us \u{1f} del \u{7f}"
        ),
        json!({"state": "proxy", "nested": {"b": [true, null, "x"], "a": []}}),
        json!(-70.0),
        json!(0.1),
    ] {
        let j = Json::from(v.clone());
        assert_eq!(j.to_string(), serde_json::to_string(&v).unwrap(), "{v}");
        assert_eq!(serde_json::Value::from(&j), v, "round trip {v}");
    }
}

#[test]
fn accessors() {
    let j = Json::obj([("a", Json::from(true)), ("b", Json::from("x"))]);
    assert_eq!(j.get("a").and_then(Json::as_bool), Some(true));
    assert_eq!(j.get("b").and_then(Json::as_str), Some("x"));
    assert!(j.get("c").is_none());
    assert!(Json::from("x").get("a").is_none());
    assert!(Json::Null.is_null());
}

#[test]
fn methods_are_the_four_the_router_knows() {
    for m in ["GET", "POST", "PUT", "DELETE"] {
        assert!(is_method(m));
    }
    for m in ["get", "PATCH", ""] {
        assert!(!is_method(m), "{m}");
    }
}

#[cfg(feature = "serde")]
#[test]
fn action_serialises_like_the_agent() {
    let text = r#"{"method":"PUT","path":"/api/wifi/radio","body":{"ap_2g":true,"ap_5g":1.0}}"#;
    let a: Action = serde_json::from_str(text).unwrap();
    assert_eq!(
        a.body.as_ref().unwrap().get("ap_5g"),
        Some(&Json::Number("1.0".into()))
    );
    assert_eq!(serde_json::to_string(&a).unwrap(), text);
    let none = Action {
        method: "POST".into(),
        path: "/x".into(),
        body: None,
    };
    assert_eq!(
        serde_json::to_string(&none).unwrap(),
        r#"{"method":"POST","path":"/x"}"#
    );
}
