//! 双卡切换（全球版，manager `docs/designs/dual-sim-global.md`）：一项 `sim.dual`，值
//! `{mode: "single", active_slot}` / `{mode: "dual", smart: false, active_slot}` / `{mode: "dual", smart: true}`。
//!
//! - 读：`get_sim_info` 的两槽 provision 旗标 + `current_sim_slot`，UCI `st_auto_switch_card_flag`。
//!   单卡时不报 smart（原厂切回单卡不清这个旗标）；智能开时不报主卡（原厂自己会换，D3）。
//! - 写：照原厂网页（`dual_sim_switch.js`）：旗标要变时先槽 2 再槽 1 `zwrt_mdm_change_provision_session`；
//!   双卡再 `st_set_auto_switch_slot`，智能关时 `zwrt_zte_mdm_activate_sim` 指定主卡。
//! - SIM 身份：读数里固定为空（同 CHILL），切主卡本来就换 ICCID，不能让 `sim_check` 取消事务（D1）；
//!   改由读回 + 数据通确认。
//! - 前置：支持双卡、不在原厂自动切换中（`switch_card_status=0`）、目标槽有卡。

use serde_json::{Value, json};

type Step = (&'static str, &'static str, Value);

const MDM: &str = "zwrt_zte_mdm.api";
/// 智能切换旗标（UCI，没有 = 关）。
pub const SMART_UCI: &str = "zwrt_zte_mdm.sim_info.st_auto_switch_card_flag";

fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn slot(v: &Value) -> Result<i64, String> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .filter(|s| matches!(s, 1 | 2))
    .ok_or_else(|| "active_slot must be 1 or 2".into())
}

/// `/control` 参数 → 规范化的目标值（也认自己的输出：退回、撤销的参数就是值本身）。
pub fn normalize(params: &Value) -> Result<Value, String> {
    let mode = params.get("mode").and_then(Value::as_str).unwrap_or_default();
    let smart = match params.get("smart") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err("smart must be true or false".into()),
    };
    let active = params.get("active_slot").map(slot).transpose()?;
    match (mode, smart) {
        ("single", Some(true)) => Err("smart switching needs dual mode".into()),
        ("single", _) => Ok(json!({"mode": "single", "active_slot": active.ok_or("active_slot is required")?})),
        ("dual", Some(true)) => Ok(json!({"mode": "dual", "smart": true})),
        ("dual", _) => Ok(json!({
            "mode": "dual",
            "smart": false,
            "active_slot": active.ok_or("active_slot is required")?,
        })),
        _ => Err("mode must be single or dual".into()),
    }
}

/// `get_sim_info` + 智能旗标 → 同形的读回；读不懂（不支持双卡、两槽都没开）为 None。
pub fn readback(sim: &Value, smart_flag: &str) -> Option<Value> {
    if text(sim, "support_dual_sim") != "1" {
        return None;
    }
    let on = |n: u8| text(sim, &format!("sim{n}_provision_state")) == "1";
    let active = slot(&Value::String(text(sim, "current_sim_slot"))).ok();
    match (on(1), on(2)) {
        (true, true) if smart_flag == "1" => Some(json!({"mode": "dual", "smart": true})),
        (true, true) => Some(json!({"mode": "dual", "smart": false, "active_slot": active?})),
        (true, false) | (false, true) => Some(json!({"mode": "single", "active_slot": active?})),
        (false, false) => None,
    }
}

/// 提交前的检查：`sim` 是现在的 `get_sim_info`，`iccids` 是 UCI 里两槽的 ICCID（没启用的槽也有）。
pub fn precheck(sim: &Value, iccids: [&str; 2], target: &Value) -> Result<(), String> {
    if text(sim, "support_dual_sim") != "1" {
        return Err("this device has no dual SIM".into());
    }
    if text(sim, "switch_card_status") == "0" {
        return Err("the SIM is being switched automatically; try again later".into());
    }
    if let Some(s) = target.get("active_slot").and_then(Value::as_i64)
        && iccids[(s - 1) as usize].is_empty()
    {
        return Err(format!("no SIM in slot {s}"));
    }
    if target["mode"] == "dual" && iccids.iter().any(|i| i.is_empty()) {
        return Err("dual mode needs a SIM in both slots".into());
    }
    Ok(())
}

/// 把双卡设成 `value` 要调的 ubus（依次）。`sim` 是写之前的 `get_sim_info`（看旗标要不要变）。
pub fn steps(value: &Value, sim: &Value) -> Result<Vec<Step>, String> {
    let dual = match value["mode"].as_str() {
        Some("dual") => true,
        Some("single") => false,
        _ => return Err("bad dual SIM value".into()),
    };
    let active = value["active_slot"].as_i64();
    let want = |n: i64| i64::from(dual || active == Some(n));
    let now = |n: i64| text(sim, &format!("sim{n}_provision_state")).parse::<i64>().unwrap_or(-1);
    let mut out = Vec::new();
    if (want(1), want(2)) != (now(1), now(2)) {
        for n in [2, 1] {
            out.push((
                MDM,
                "zwrt_mdm_change_provision_session",
                json!({"active_slot": n, "active_flag": want(n)}),
            ));
        }
    }
    if dual {
        let smart = value["smart"] == true;
        out.push((MDM, "st_set_auto_switch_slot", json!({"auto_switch_slot_flag": i64::from(smart)})));
        if !smart {
            let id = active.ok_or("active_slot is required")?;
            out.push((MDM, "zwrt_zte_mdm_activate_sim", json!({"sim_card_id": id})));
        }
    }
    // 单卡、槽也对：什么都不调，交给读回确认
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(p1: &str, p2: &str, cur: &str) -> Value {
        json!({"support_dual_sim":"1","sim1_provision_state":p1,"sim2_provision_state":p2,
            "current_sim_slot":cur,"switch_card_status":"1"})
    }

    #[test]
    fn normalize_round_trips() {
        for v in [
            json!({"mode":"single","active_slot":2}),
            json!({"mode":"dual","smart":false,"active_slot":1}),
            json!({"mode":"dual","smart":true}),
        ] {
            assert_eq!(normalize(&v).unwrap(), v);
        }
        // 智能开时主卡不进目标（D3）
        assert_eq!(
            normalize(&json!({"mode":"dual","smart":true,"active_slot":"2"})).unwrap(),
            json!({"mode":"dual","smart":true})
        );
        assert_eq!(normalize(&json!({"mode":"single","active_slot":"1"})).unwrap()["active_slot"], 1);
        for bad in [
            json!({"mode":"single"}),
            json!({"mode":"single","active_slot":3}),
            json!({"mode":"single","active_slot":1,"smart":true}),
            json!({"mode":"dual","active_slot":1,"smart":"yes"}),
            json!({"mode":"dual"}),
            json!({}),
        ] {
            assert!(normalize(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn readback_shapes() {
        assert_eq!(readback(&info("1", "0", "1"), ""), Some(json!({"mode":"single","active_slot":1})));
        // 单卡时残留的智能旗标不进读回
        assert_eq!(readback(&info("0", "1", "2"), "1"), Some(json!({"mode":"single","active_slot":2})));
        assert_eq!(
            readback(&info("1", "1", "2"), "0"),
            Some(json!({"mode":"dual","smart":false,"active_slot":2}))
        );
        assert_eq!(readback(&info("1", "1", "2"), "1"), Some(json!({"mode":"dual","smart":true})));
        assert_eq!(readback(&json!({"support_dual_sim":"0"}), ""), None);
        assert_eq!(readback(&info("1", "1", ""), "0"), None);
        // 读回和规范化同形（matches 才对得上）
        for (s, f) in [(info("1", "0", "1"), ""), (info("1", "1", "1"), "0"), (info("1", "1", "1"), "1")] {
            let r = readback(&s, f).unwrap();
            assert_eq!(normalize(&r).unwrap(), r);
        }
    }

    #[test]
    fn precheck_refuses() {
        let both = ["89860000000000001111", "89860000000000002222"];
        let s = info("1", "0", "1");
        assert!(precheck(&s, both, &json!({"mode":"dual","smart":true})).is_ok());
        assert!(precheck(&json!({"support_dual_sim":"0"}), both, &json!({"mode":"single","active_slot":1})).is_err());
        let mut busy = s.clone();
        busy["switch_card_status"] = json!("0");
        assert!(precheck(&busy, both, &json!({"mode":"single","active_slot":1})).is_err());
        let one = ["89860000000000001111", ""];
        assert!(precheck(&s, one, &json!({"mode":"single","active_slot":2})).is_err());
        assert!(precheck(&s, one, &json!({"mode":"dual","smart":true})).is_err());
        assert!(precheck(&s, one, &json!({"mode":"single","active_slot":1})).is_ok());
    }

    fn calls(v: Value, sim: Value) -> Vec<(String, Value)> {
        steps(&v, &sim).unwrap().into_iter().map(|(_, m, a)| (m.into(), a)).collect()
    }

    #[test]
    fn steps_follow_stock_order() {
        // 单卡 1 → 双卡智能：旗标先槽 2 再槽 1，再开智能
        assert_eq!(
            calls(json!({"mode":"dual","smart":true}), info("1", "0", "1")),
            vec![
                ("zwrt_mdm_change_provision_session".into(), json!({"active_slot":2,"active_flag":1})),
                ("zwrt_mdm_change_provision_session".into(), json!({"active_slot":1,"active_flag":1})),
                ("st_set_auto_switch_slot".into(), json!({"auto_switch_slot_flag":1})),
            ]
        );
        // 双卡里换主卡：不动旗标，关智能 + 指定主卡
        assert_eq!(
            calls(json!({"mode":"dual","smart":false,"active_slot":2}), info("1", "1", "1")),
            vec![
                ("st_set_auto_switch_slot".into(), json!({"auto_switch_slot_flag":0})),
                ("zwrt_zte_mdm_activate_sim".into(), json!({"sim_card_id":2})),
            ]
        );
        // 回单卡 1（退回的典型）：只改旗标
        assert_eq!(
            calls(json!({"mode":"single","active_slot":1}), info("1", "1", "2")),
            vec![
                ("zwrt_mdm_change_provision_session".into(), json!({"active_slot":2,"active_flag":0})),
                ("zwrt_mdm_change_provision_session".into(), json!({"active_slot":1,"active_flag":1})),
            ]
        );
        assert!(calls(json!({"mode":"single","active_slot":1}), info("1", "0", "1")).is_empty());
    }
}
