use super::*;

fn vault() -> Vault {
    Vault::open(None)
}

fn manual(enabled: &str, au_password: &str) -> Value {
    json!({"apnListArray": [
        {"profilename":"Japan","wanapn":"plus.4g","username":"","password":"","pdpType":3,
         "pppAuthMode":0,"profileId":"manu2","isEnable": enabled == "manu2"},
        {"profilename":"AU","wanapn":"x.au-net.ne.jp","username":"u","password":au_password,
         "pdpType":"3","pppAuthMode":2,"profileId":"manu3","isEnable": enabled == "manu3"}]})
}

#[test]
fn readback_value() {
    let v = vault();
    // B31 真机形状：自动模式、手动表全没启用
    let n = Now::parse(&json!({"apn_mode": 0}), &manual("", "p")).unwrap();
    assert_eq!(n.value(&v), json!({"mode":0,"manual":"","fp":""}));
    let n = Now::parse(&json!({"apn_mode": 1}), &manual("manu3", "p")).unwrap();
    let val = n.value(&v);
    assert_eq!(val["manual"], "manu3");
    assert_eq!(val["fp"].as_str().unwrap().len(), 32);
    assert!(!val.to_string().contains("\"p\""));
    // 只改了密码，指纹就变；数字写成字符串也一样
    let n2 = Now::parse(&json!({"apn_mode": 1}), &manual("manu3", "q")).unwrap();
    assert_ne!(n2.value(&v)["fp"], val["fp"]);
    assert_eq!(content(&json!({"pdpType":"3"}))["pdpType"], 3);
    assert!(Now::parse(&json!({}), &manual("", "")).is_none());
}

#[test]
fn mode_target_matches_whatever_profile_follows() {
    let v = vault();
    let n = Now::parse(&json!({"apn_mode": 1}), &manual("manu2", "")).unwrap();
    assert!(crate::ops::txn::matches(&json!({"mode": 1}), &n.value(&v)));
    assert!(!crate::ops::txn::matches(&json!({"mode": 0}), &n.value(&v)));
}

#[test]
fn steps_mode_enable_modify() {
    let v = vault();
    let auto = Now::parse(&json!({"apn_mode": 0}), &manual("", "p")).unwrap();
    fn m(s: &[(&'static str, &'static str, Value)]) -> Vec<&'static str> {
        s.iter().map(|x| x.1).collect()
    }
    assert_eq!(
        m(&steps(&json!({"mode":1}), &auto, None, &v).unwrap()),
        ["set_apn_mode"]
    );
    let on = Now::parse(&json!({"apn_mode": 1}), &manual("manu2", "p")).unwrap();
    assert_eq!(
        m(&steps(&json!({"mode":1,"manual":"manu3"}), &on, None, &v).unwrap()),
        ["enable_manu_apn_id"]
    );
    // 改正在用的那条：新内容来自这次的参数
    let base = on.content_of("manu2").unwrap().clone();
    let new = content_from_params(&json!({"name":"Japan","apn":"plus.5g"}), &base);
    let fp = v.fp(&new);
    let s = steps(
        &json!({"mode":1,"manual":"manu2","fp":fp}),
        &on,
        Some(&new),
        &v,
    )
    .unwrap();
    assert_eq!(m(&s), ["modify_manu_apn"]);
    assert_eq!(s[0].2["wanapn"], "plus.5g");
    assert_eq!(s[0].2["profileId"], "manu2");
    // 退回到旧内容：保管箱里没有就拒绝
    let old = v.fp(&base);
    let cur_new = Now::parse(
        &json!({"apn_mode": 1}),
        &json!({"apnListArray":[{"profilename":"Japan","wanapn":"plus.5g","profileId":"manu2","isEnable":true}]}),
    )
    .unwrap();
    assert!(
        steps(
            &json!({"mode":1,"manual":"manu2","fp":old}),
            &cur_new,
            None,
            &v
        )
        .is_err()
    );
    // 退回到自动：只切模式
    assert_eq!(
        m(&steps(&json!({"mode":0,"manual":"","fp":""}), &on, None, &v).unwrap()),
        ["set_apn_mode"]
    );
    // 从自动换到一条手动的（apn.use，第二期 D3）：先启用、再切模式（切模式的重拨要用上它）
    assert_eq!(
        m(&steps(&json!({"mode":1,"manual":"manu3"}), &auto, None, &v).unwrap()),
        ["enable_manu_apn_id", "set_apn_mode"]
    );
}

#[test]
fn vault_keeps_private_content() {
    let dir = std::env::temp_dir().join(format!("u60d-apn-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let v = Vault::open(Some(dir.clone()));
    let c = content(&json!({"profilename":"AU","password":"secret"}));
    v.keep("manu3", &c);
    let fp = v.fp(&c);
    assert_eq!(v.get(&fp), Some(c.clone()));
    // 同一个密钥：重开后指纹不变
    assert_eq!(Vault::open(Some(dir.clone())).fp(&c), fp);
    use std::os::unix::fs::PermissionsExt;
    for f in [KEY, VAULT] {
        let mode = fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{f}");
    }
    let _ = fs::remove_dir_all(&dir);
}
