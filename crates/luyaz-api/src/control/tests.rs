use super::*;

#[test]
fn busy_uses_core_sentence() {
    let doing = json!({"action":"network.set_mode","say_zh":"正在换制式（触屏发起，32 秒），稍等","say_en":"Busy: network mode (Screen)"});
    let (s, v) = Reply::Busy { doing: Some(doing) }.into_http();
    assert_eq!(
        (s, v["error"].as_str(), v["error_en"].as_str()),
        (
            409,
            Some("正在换制式（触屏发起，32 秒），稍等"),
            Some("Busy: network mode (Screen)")
        )
    );
    let (_, v) = Reply::Busy {
        doing: Some(json!({"action":"network.set_mode"})),
    }
    .into_http();
    assert_eq!(
        v["error"],
        "设备正在改别的设置（network.set_mode），等它结束再试"
    );
}

#[test]
fn replies() {
    assert_eq!(
        parse(
            200,
            &json!({"ok":true,"action":"network.set_mode","result":{"result":"success"},"op":{"op_id":"web-1","phase":"applying"}})
        ),
        Reply::Done {
            result: json!({"result":"success"}),
            op: Some(json!({"op_id":"web-1","phase":"applying"}))
        }
    );
    assert_eq!(
        parse(200, &json!({"ok":true,"result":null})),
        Reply::Done {
            result: Value::Null,
            op: None
        }
    );
    assert_eq!(
        parse(
            409,
            &json!({"ok":false,"error":{"code":"busy","message":"busy"},"doing":{"action":"network.set_mode"}})
        ),
        Reply::Busy {
            doing: Some(json!({"action":"network.set_mode"}))
        }
    );
    assert_eq!(
        parse(
            503,
            &json!({"ok":false,"error":{"code":"busy","message":"control queue full"}})
        ),
        Reply::Busy { doing: None }
    );
    assert_eq!(
        parse(
            502,
            &json!({"ok":false,"error":{"code":"failed","message":"ubus call failed: Method not found"},"op":{"phase":"verifying"}})
        ),
        Reply::Failed {
            error: "ubus call failed: Method not found".into(),
            op: Some(json!({"phase":"verifying"}))
        }
    );
    assert_eq!(
        parse(
            400,
            &json!({"ok":false,"error":{"code":"invalid","message":"mode must be a string"}})
        ),
        Reply::Invalid("mode must be a string".into())
    );
    // 200 without ok:true is not success
    assert!(matches!(
        parse(200, &json!({"ok":false})),
        Reply::Failed { .. }
    ));
}

#[test]
fn op_busy_is_its_own_reply() {
    let r = parse(
        409,
        &json!({"ok":false,"error":{"code":"op_busy","message":"network mode is being confirmed","op":{"op_id":"web-1","item":"network.mode","phase":"verifying"}}}),
    );
    assert_eq!(
        r,
        Reply::OpBusy {
            message: "network mode is being confirmed".into(),
            op: Some(json!({"op_id":"web-1","item":"network.mode","phase":"verifying"}))
        }
    );
    assert!(r.is_op_busy() && !r.ok());
    // plain busy is still Busy, and other 409s are not op_busy
    assert!(
        !parse(
            409,
            &json!({"ok":false,"error":{"code":"busy","message":"busy"}})
        )
        .is_op_busy()
    );
    assert!(
        !parse(
            409,
            &json!({"ok":false,"error":{"code":"invalid_state","message":"x"}})
        )
        .is_op_busy()
    );
    assert!(
        !parse(
            503,
            &json!({"ok":false,"error":{"code":"op_busy","message":"x"}})
        )
        .is_op_busy()
    );
    // the page gets a 409 with the op, not a 503 "failed"
    let (s, v) = r.clone().into_http();
    assert_eq!(
        (s, v["busy"].as_str(), v["op"]["op_id"].as_str()),
        (409, Some("op"), Some("web-1"))
    );
    // an automatic loop can tell it from a failure
    let e = r.into_result().unwrap_err();
    assert!(is_op_busy_err(&e), "{e}");
    assert!(!is_op_busy_err("core busy (network.set_mode)"));
}

#[test]
fn retry_only_on_op_busy() {
    let busy: Result<(), String> = Reply::OpBusy {
        message: "m".into(),
        op: None,
    }
    .into_result()
    .map(|_| ());
    assert_eq!(retry_after(&busy), Some(OP_BUSY_RETRY));
    assert!(OP_BUSY_RETRY >= Duration::from_secs(10));
    assert_eq!(retry_after(&Ok::<(), String>(())), None);
    assert_eq!(
        retry_after(&Err::<(), String>("ubus call failed".into())),
        None
    );
    assert_eq!(
        retry_after(&Reply::Busy { doing: None }.into_result()),
        None
    );
    assert_eq!(
        retry_after(
            &Reply::Unreachable {
                error: "x".into(),
                maybe_done: false
            }
            .into_result()
        ),
        None
    );
}

#[test]
fn http_answers() {
    let (s, v) = Reply::Done {
        result: json!(1),
        op: Some(json!({"op_id":"x"})),
    }
    .into_http();
    assert_eq!(
        (
            s,
            v["ok"].clone(),
            v["data"].clone(),
            v["op"]["op_id"].clone()
        ),
        (200, json!(true), json!(1), json!("x"))
    );
    let (s, v) = Reply::Busy {
        doing: Some(json!({"action":"network.set_mode"})),
    }
    .into_http();
    assert_eq!((s, v["busy"].as_str()), (409, Some("core")));
    assert!(v["error"].as_str().unwrap().contains("network.set_mode"));
    let (s, v) = Reply::Failed {
        error: "Method not found".into(),
        op: None,
    }
    .into_http();
    assert_eq!((s, v["error"].as_str()), (503, Some("Method not found")));
    let (s, v) = Reply::Unreachable {
        error: "connect: refused".into(),
        maybe_done: false,
    }
    .into_http();
    assert_eq!(
        (s, v["error"].as_str(), v["core"].as_str()),
        (503, Some(DATAD_DOWN), Some("down"))
    );
    let (_, v) = Reply::Unreachable {
        error: "read: timed out".into(),
        maybe_done: true,
    }
    .into_http();
    assert_eq!(v["error"].as_str(), Some(NO_ANSWER));
}

#[test]
fn the_screen_is_not_sent_to_itself() {
    let (s, v) = Reply::Unreachable {
        error: "connect: refused".into(),
        maybe_done: false,
    }
    .into_http_screen();
    assert_eq!(
        (s, v["error"].as_str(), v["core"].as_str()),
        (503, Some(DATAD_DOWN_SCREEN), Some("down"))
    );
    let (_, v) = Reply::Unreachable {
        error: "x".into(),
        maybe_done: true,
    }
    .into_http_screen();
    assert_eq!(v["error"].as_str(), Some(NO_ANSWER));
}
