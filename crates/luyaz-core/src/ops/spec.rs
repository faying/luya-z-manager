//! 动作描述表（write-op-layer.md「动作描述表 ActionSpec」）：哪些 `/control` 动作走事务、改的是哪一项、
//! 目标值怎么从参数算出来、确认时限多长。T2 先只接网络模式；其余动作照旧走 `control::execute`。
//! 读回和写怎么做在 `UbusDevice`（按 `item` 分）。
//!
//! 目标值是规范化后的 JSON 值（3-0）：只有一个参数的写（网络模式）就是那个字符串本身，和旧版落的盘、
//! 流水账、owners 一样；多个参数的写（锁频、锁小区、APN）是对象。事务只按 `==` 比较目标、旧值、
//! 退回目标和读回，所以读回必须经同一套规范化（排序、统一类型、不带多余字段），否则永远对不上。

use super::txn::Confirm;
use serde_json::Value;

pub struct Spec {
    pub action: &'static str,
    /// 改的是哪一项：同一项的写才能互相覆盖，撤销、owners 也按它算。
    pub item: &'static str,
    /// `/control` 参数 → 规范化的目标值；错误文字回给客户端。
    pub normalize: fn(&Value) -> Result<Value, String>,
    /// 目标值 → `/control` 参数（撤销请求用；`normalize` 的逆）。
    pub params: fn(&Value) -> Value,
    /// 确认时限（D24 暂定值，T11 用 netwatch 统计定稿）。
    pub deadline_ms: u64,
    /// 覆盖确认时限的环境变量（毫秒）。
    pub deadline_env: &'static str,
    /// 确认规则（D34：只有 APN 要求写之后的新连接）。
    pub confirm: Confirm,
}

pub const NETWORK_MODE: &str = "network.mode";

pub const BAND_LTE: &str = "band.lte";
/// 5G 锁频：SA、NSA 算一项（用户 10-12 定），一个事务两步写，撤销一起退。
pub const BAND_NR: &str = "band.nr";
/// 「恢复默认」：三组频段一起（D22；原厂调用同时清小区锁，小区锁的读回在 3-2 补进来）。
pub const BAND_ALL: &str = "band.all";

/// 锁小区（3-2）：值是 `{pci, earfcn}` / `{pci, arfcn, band}`，没锁是 [`cell_off`]。
pub const CELL_LTE: &str = "cell.lte";
pub const CELL_NR: &str = "cell.nr";

/// APN（3-3，见 `ops::apn`）：一项，值 `{mode, manual, fp}`；切模式、启用、改正在用的那条都是它的事务。
pub const APN: &str = "apn";

/// Wi-Fi 设置（第四批 4-0b，见 `ops::wifi`）：主网络（两个频段、总开关、国家码）和访客网络各一项，
/// 值是 `wifi` 块展开成一层的键，密码只有指纹。
pub const WIFI_MAIN: &str = "wifi.main";
pub const WIFI_GUEST: &str = "wifi.guest";

/// Wi-Fi 的项（不影响上网：D40 的「别的写」、安全类写都不取消它）。
pub fn is_wifi(item: &str) -> bool {
    matches!(item, WIFI_MAIN | WIFI_GUEST)
}

pub static SPECS: &[Spec] = &[
    Spec {
        action: "network.set_mode",
        item: NETWORK_MODE,
        normalize: network_mode,
        params: network_mode_params,
        deadline_ms: 120_000,
        deadline_env: "LUYAZ_DEADLINE_NETWORK_MODE_MS",
        confirm: Confirm::Registered,
    },
    Spec {
        action: "band.set_lte",
        item: BAND_LTE,
        normalize: band_lte,
        params: band_lte_params,
        deadline_ms: 120_000,
        deadline_env: "LUYAZ_DEADLINE_BAND_MS",
        confirm: Confirm::Registered,
    },
    Spec {
        action: "band.set_nr",
        item: BAND_NR,
        normalize: band_nr,
        params: same,
        deadline_ms: 120_000,
        deadline_env: "LUYAZ_DEADLINE_BAND_MS",
        confirm: Confirm::Registered,
    },
    Spec {
        action: "band.reset",
        item: BAND_ALL,
        normalize: band_all,
        params: same,
        deadline_ms: 120_000,
        deadline_env: "LUYAZ_DEADLINE_BAND_MS",
        confirm: Confirm::Registered,
    },
    Spec {
        action: "cell.lock_lte",
        item: CELL_LTE,
        normalize: cell_lte,
        params: cell_params,
        deadline_ms: 120_000,
        deadline_env: "LUYAZ_DEADLINE_CELL_MS",
        confirm: Confirm::Registered,
    },
    Spec {
        action: "cell.lock_nr",
        item: CELL_NR,
        normalize: cell_nr,
        params: cell_params,
        deadline_ms: 120_000,
        deadline_env: "LUYAZ_DEADLINE_CELL_MS",
        confirm: Confirm::Registered,
    },
    Spec {
        action: "apn.set_mode",
        item: APN,
        normalize: apn_mode,
        params: apn_restore,
        deadline_ms: 90_000,
        deadline_env: "LUYAZ_DEADLINE_APN_MS",
        confirm: Confirm::Apn,
    },
    Spec {
        action: "apn.enable",
        item: APN,
        normalize: apn_enable,
        params: apn_restore,
        deadline_ms: 90_000,
        deadline_env: "LUYAZ_DEADLINE_APN_MS",
        confirm: Confirm::Apn,
    },
    Spec {
        action: "apn.modify",
        item: APN,
        normalize: apn_modify,
        params: apn_restore,
        deadline_ms: 90_000,
        deadline_env: "LUYAZ_DEADLINE_APN_MS",
        confirm: Confirm::Apn,
    },
    Spec {
        action: "wifi.settings",
        item: WIFI_MAIN,
        normalize: super::wifi::check_main,
        params: apn_restore,
        deadline_ms: 90_000,
        deadline_env: "LUYAZ_DEADLINE_WIFI_MS",
        confirm: Confirm::Wifi,
    },
    Spec {
        action: "wifi.guest",
        item: WIFI_GUEST,
        normalize: super::wifi::check_guest,
        params: apn_restore,
        deadline_ms: 90_000,
        deadline_env: "LUYAZ_DEADLINE_WIFI_MS",
        confirm: Confirm::Wifi,
    },
];

/// 撤销/退回的参数 `{restore: 值}`（撤销请求另带 `expect`：那次改完的值，`resolve` 核对现状还是它）。
/// APN 和 Wi-Fi 都这样。
fn apn_restore(v: &Value) -> Value {
    serde_json::json!({ "restore": v })
}

/// `{restore}` 的值：只认 `mode`/`manual`/`fp` 三个键。
fn apn_restored(params: &Value) -> Option<Result<Value, String>> {
    let r = params.get("restore")?;
    let ok = r.as_object().is_some_and(|m| {
        !m.is_empty()
            && m.iter().all(|(k, v)| match k.as_str() {
                "mode" => matches!(v.as_i64(), Some(0 | 1)),
                "manual" | "fp" => v.is_string(),
                _ => false,
            })
    });
    Some(if ok {
        Ok(r.clone())
    } else {
        Err("restore must be an APN value".into())
    })
}

fn apn_mode(params: &Value) -> Result<Value, String> {
    if let Some(r) = apn_restored(params) {
        return r;
    }
    match crate::control::integer(params, "mode", true)? {
        Some(m @ (0 | 1)) => Ok(serde_json::json!({ "mode": m })),
        _ => Err("mode must be 0 or 1".into()),
    }
}

/// 手动 profile 的编号（`manuN`）；自动表的不能启用、不能改。
fn manual_id(params: &Value) -> Result<String, String> {
    let id = crate::control::string(params, "profile_id", true)?.unwrap_or_default();
    if !id.starts_with("manu") || id.len() > 16 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("profile_id must be a manual profile (manuN)".into());
    }
    Ok(id)
}

fn apn_enable(params: &Value) -> Result<Value, String> {
    if let Some(r) = apn_restored(params) {
        return r;
    }
    Ok(serde_json::json!({ "mode": 1, "manual": manual_id(params)? }))
}

/// 改内容：指纹要本机密钥和改之前的那条，`resolve` 再算；这里先校验参数（和 `control.rs` 的 `apn()` 一样）。
fn apn_modify(params: &Value) -> Result<Value, String> {
    if let Some(r) = apn_restored(params) {
        return r;
    }
    let id = manual_id(params)?;
    crate::control::string(params, "name", true)?;
    crate::control::string(params, "apn", true)?;
    Ok(serde_json::json!({ "mode": 1, "manual": id, "modify": true }))
}

/// 没锁小区的值。固件没有单独的解锁：写它 = 原厂一键恢复 + 补回原来的频段和另一种小区锁（用户 10-12 定）。
pub fn cell_off() -> Value {
    serde_json::json!({ "off": true })
}

/// 一个小区参数：字符串或数字，只有数字，规范成十进制字符串（去前导零）。
fn cell_num(params: &Value, key: &str) -> Result<String, String> {
    let raw = match params.get(key) {
        Some(Value::String(v)) => v.trim().to_owned(),
        Some(Value::Number(n)) => n.to_string(),
        None => return Err(format!("missing parameter: {key}")),
        Some(_) => return Err(format!("{key} must be a string")),
    };
    match raw.parse::<u64>() {
        Ok(n) if raw.bytes().all(|b| b.is_ascii_digit()) => Ok(n.to_string()),
        _ => Err(format!("{key} must be a number")),
    }
}

fn wants_off(params: &Value) -> bool {
    params.get("off") == Some(&Value::Bool(true))
}

fn cell_lte(params: &Value) -> Result<Value, String> {
    if wants_off(params) {
        return Ok(cell_off());
    }
    Ok(serde_json::json!({
        "pci": cell_num(params, "pci")?,
        "earfcn": cell_num(params, "earfcn")?,
    }))
}

fn cell_nr(params: &Value) -> Result<Value, String> {
    if wants_off(params) {
        return Ok(cell_off());
    }
    let band = params.get("band").map(|b| match b {
        Value::String(s) => Value::String(s.trim_start_matches(['n', 'N']).to_owned()),
        other => other.clone(),
    });
    Ok(serde_json::json!({
        "pci": cell_num(params, "pci")?,
        "arfcn": cell_num(params, "arfcn")?,
        "band": cell_num(&serde_json::json!({ "band": band }), "band")?,
    }))
}

/// 撤销的参数：锁的就是原样，没锁的是 `{off: true}`（只有事务能处理，旧 `/control` 不认）。
fn cell_params(v: &Value) -> Value {
    v.clone()
}

/// `band.reset {}` 的目标值占位：提交时由设备换成默认的三组频段（`Device::resolve`）。
pub fn reset_marker() -> Value {
    serde_json::json!({ "reset": true })
}

/// 频段表的规范写法：数字升序、去重、逗号分隔（和原厂 `*_band_lock` 读回一样）。
/// 有数字和逗号以外的字符为 None；空表为空字符串。
pub fn bands(s: &str) -> Option<String> {
    let mut v = Vec::new();
    for p in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        v.push(p.parse::<u32>().ok()?);
    }
    v.sort_unstable();
    v.dedup();
    Some(v.iter().map(u32::to_string).collect::<Vec<_>>().join(","))
}

/// 一个频段参数：字符串、只有数字和逗号（错误文字和 `control.rs` 的 `band()` 一样）、不能是空表。
fn band_param(params: &Value, key: &str) -> Result<String, String> {
    let raw = crate::control::string(params, key, true)?.unwrap_or_default();
    let v = bands(&raw).ok_or_else(|| format!("{key} must contain only numbers and commas"))?;
    if v.is_empty() {
        return Err(format!("{key} must list at least one band"));
    }
    Ok(v)
}

fn band_lte(params: &Value) -> Result<Value, String> {
    band_param(params, "bands").map(Value::String)
}

fn band_lte_params(v: &Value) -> Value {
    serde_json::json!({ "bands": v })
}

fn band_nr(params: &Value) -> Result<Value, String> {
    Ok(serde_json::json!({
        "sa": band_param(params, "sa")?,
        "nsa": band_param(params, "nsa")?,
    }))
}

/// `{}` = 恢复默认（占位，见 [`reset_marker`]）；带 `lte`/`sa`/`nsa` 三组 = 撤销到这三组。
fn band_all(params: &Value) -> Result<Value, String> {
    let map = params.as_object().ok_or("params must be an object")?;
    if map.is_empty() || params == &reset_marker() {
        return Ok(reset_marker());
    }
    let mut v = serde_json::json!({
        "lte": band_param(params, "lte")?,
        "sa": band_param(params, "sa")?,
        "nsa": band_param(params, "nsa")?,
    });
    // D22：恢复默认也清小区锁，所以旧值里带两种小区锁（撤销时一起补回）
    for (k, f) in [
        ("lte_cell", cell_lte as fn(&Value) -> Result<Value, String>),
        ("nr_cell", cell_nr),
    ] {
        if let Some(c) = params.get(k) {
            v[k] = f(c)?;
        }
    }
    Ok(v)
}

fn same(v: &Value) -> Value {
    v.clone()
}

fn network_mode(params: &Value) -> Result<Value, String> {
    crate::control::string(params, "mode", true).map(|v| Value::String(v.unwrap_or_default()))
}

fn network_mode_params(v: &Value) -> Value {
    serde_json::json!({ "mode": v })
}

pub fn find(action: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.action == action)
}

impl Spec {
    pub fn deadline_ms(&self) -> u64 {
        std::env::var(self.deadline_env)
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(self.deadline_ms)
    }

    /// 目标值；网络模式的错误文字和旧接口一致（`control.rs` 的 `string()`）。
    pub fn target(&self, params: &Value) -> Result<Value, String> {
        (self.normalize)(params)
    }

    /// 撤销到 `v` 的 `/control` 参数。
    pub fn to_params(&self, v: &Value) -> Value {
        (self.params)(v)
    }
}

/// D14：`cellular.set` 只把数据或漫游关掉（参数只有 `enabled`/`roaming`，且都是关）算安全类写，
/// 马上插队（新旧客户端一样）。参数名按 `/control` 的（不是 ubus 的 enable/roam_enable）。
pub fn is_safety(action: &str, params: &Value) -> bool {
    if action != "cellular.set" {
        return false;
    }
    let Some(map) = params.as_object() else {
        return false;
    };
    !map.is_empty()
        && map.keys().all(|k| k == "enabled" || k == "roaming")
        && map
            .keys()
            .all(|k| crate::control::boolean(params, k) == Ok(false))
}

/// D40：`vendor.call` 里影响上网的 (对象, 方法)：STC 小区锁、SIM PIN/PUK/网络锁。
const NETWORK_VENDOR_CALLS: &[(&str, &str)] = &[
    ("zte_nwinfo_api", "nwinfo_set_stc_white_list_par"),
    ("zte_nwinfo_api", "nwinfo_stc_cell_lock_enable"),
    ("zte_nwinfo_api", "nwinfo_stc_cell_lock_disable"),
    ("zte_nwinfo_api", "nwinfo_stc_cell_lock_reset"),
    ("zwrt_zte_mdm.api", "sim_verify_pin_puk"),
    ("zwrt_zte_mdm.api", "sim_change_pin"),
    ("zwrt_zte_mdm.api", "sim_change_pin_mode"),
    ("zwrt_zte_mdm.api", "set_simlock_nck"),
];

/// D40：事务在确认或退回中时要按来源处理的「影响上网的写」：会话期间收 409 的那些（描述表里的动作除外，
/// 同一项由引擎按覆盖处理）、回自动和重拨、`vendor.call` 的 STC 小区锁和 SIM PIN 类。
/// 搜网会话的步骤照会话规则；安全类写（[`is_safety`]）由调用方先排除，照旧插队。
pub fn affects_network(action: &str, params: &Value) -> bool {
    use super::engine::{SESSION_BLOCKS, SESSION_OPTIONAL};
    if find(action).is_some() {
        return false;
    }
    if SESSION_BLOCKS.contains(&action) || SESSION_OPTIONAL.contains(&action) {
        return true;
    }
    action == "vendor.call"
        && NETWORK_VENDOR_CALLS.iter().any(|(o, m)| {
            params.get("object").and_then(Value::as_str) == Some(o)
                && params.get("method").and_then(Value::as_str) == Some(m)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn network_affecting_writes() {
        for a in [
            "cellular.set",
            "band.set_nr_sa",
            "apn.add",
            "apn.delete",
            "modem.online",
            "apn.set_pdp_type",
            "netselect.auto",
            "cellular.redial",
        ] {
            assert!(affects_network(a, &json!({})), "{a}");
        }
        // 描述表里的（引擎按同一项处理）、搜网步骤、别的写都不算
        for a in [
            "network.set_mode",
            "band.set_lte",
            "band.set_nr",
            "netselect.scan",
            "netselect.register",
            "sms.delete",
            "wifi.apply",
            "usb.set",
        ] {
            assert!(!affects_network(a, &json!({})), "{a}");
        }
        let v = |o: &str, m: &str| affects_network("vendor.call", &json!({"object":o,"method":m}));
        assert!(v("zte_nwinfo_api", "nwinfo_stc_cell_lock_enable"));
        assert!(v("zte_nwinfo_api", "nwinfo_set_stc_white_list_par"));
        assert!(v("zwrt_zte_mdm.api", "sim_verify_pin_puk"));
        assert!(v("zwrt_zte_mdm.api", "set_simlock_nck"));
        assert!(!v("zte_nwinfo_api", "nwinfo_start_detect_signal_quality"));
        assert!(!v("zwrt_router.api", "router_set_dmz"));
        assert!(!v("zwrt_router.api", "sim_change_pin"));
        assert!(!affects_network("vendor.call", &json!({})));
        // 表里的每一行都在 VENDOR_CALLS 里（不会因为改名悄悄失效）
        for pair in NETWORK_VENDOR_CALLS {
            assert!(crate::control::VENDOR_CALLS.contains(pair), "{pair:?}");
        }
    }

    #[test]
    fn safety_is_only_switching_off() {
        assert!(is_safety("cellular.set", &json!({"enabled":0})));
        assert!(is_safety("cellular.set", &json!({"roaming":false})));
        assert!(is_safety(
            "cellular.set",
            &json!({"enabled":false,"roaming":0})
        ));
        assert!(!is_safety("cellular.set", &json!({"enabled":1})));
        assert!(!is_safety(
            "cellular.set",
            &json!({"enabled":0,"roaming":1})
        ));
        assert!(!is_safety(
            "cellular.set",
            &json!({"enabled":0,"connect_mode":"1"})
        ));
        assert!(!is_safety("cellular.set", &json!({})));
        assert!(!is_safety("cellular.set", &json!({"enabled":"0"})));
        assert!(!is_safety("network.set_mode", &json!({"enabled":0})));
    }

    #[test]
    fn target_errors_match_the_old_api() {
        let s = find("network.set_mode").unwrap();
        assert_eq!(s.target(&json!({"mode":"Only_LTE"})), Ok(json!("Only_LTE")));
        assert_eq!(s.target(&json!({})), Err("missing parameter: mode".into()));
        assert_eq!(
            s.target(&json!({"mode":1})),
            Err("mode must be a string".into())
        );
        assert!(find("band.set_nr_sa").is_none());
    }

    #[test]
    fn params_invert_normalize() {
        let s = find("network.set_mode").unwrap();
        let t = json!("Only_LTE");
        assert_eq!(s.to_params(&t), json!({"mode":"Only_LTE"}));
        assert_eq!(s.target(&s.to_params(&t)), Ok(t));
    }

    #[test]
    fn band_targets_are_canonical() {
        assert_eq!(bands(" 78, 41,78,1 "), Some("1,41,78".into()));
        assert_eq!(bands(""), Some(String::new()));
        assert_eq!(bands("n78"), None);
        let lte = find("band.set_lte").unwrap();
        assert_eq!(lte.target(&json!({"bands":"3,1,3"})), Ok(json!("1,3")));
        assert_eq!(
            lte.target(&json!({"bands":"1;3"})),
            Err("bands must contain only numbers and commas".into())
        );
        assert!(lte.target(&json!({"bands":""})).is_err());
        assert!(lte.target(&json!({})).is_err());
        let t = lte.target(&json!({"bands":"7,3"})).unwrap();
        assert_eq!(lte.target(&lte.to_params(&t)), Ok(t));

        let nr = find("band.set_nr").unwrap();
        let t = nr.target(&json!({"sa":"78,41","nsa":"78"})).unwrap();
        assert_eq!(t, json!({"sa":"41,78","nsa":"78"}));
        assert_eq!(nr.target(&nr.to_params(&t)), Ok(t));
        assert!(nr.target(&json!({"sa":"78"})).is_err());

        let all = find("band.reset").unwrap();
        assert_eq!(all.target(&json!({})), Ok(reset_marker()));
        assert_eq!(all.target(&reset_marker()), Ok(reset_marker()));
        let t = json!({"lte":"1,3","sa":"78","nsa":"41,78"});
        assert_eq!(all.target(&all.to_params(&t)), Ok(t));
        assert!(all.target(&json!({"lte":"1"})).is_err());
        let t = json!({"lte":"1","sa":"78","nsa":"78","lte_cell":{"off":true},
                       "nr_cell":{"pci":"603","arfcn":"627264","band":"78"}});
        assert_eq!(all.target(&all.to_params(&t)), Ok(t));
    }

    #[test]
    fn apn_targets() {
        let m = find("apn.set_mode").unwrap();
        assert_eq!(m.target(&json!({"mode":1})), Ok(json!({"mode":1})));
        assert!(m.target(&json!({"mode":2})).is_err());
        let e = find("apn.enable").unwrap();
        assert_eq!(
            e.target(&json!({"profile_id":"manu2"})),
            Ok(json!({"mode":1,"manual":"manu2"}))
        );
        assert!(e.target(&json!({"profile_id":"auto109590"})).is_err());
        let md = find("apn.modify").unwrap();
        assert_eq!(
            md.target(&json!({"profile_id":"manu3","name":"AU","apn":"x","password":"p"})),
            Ok(json!({"mode":1,"manual":"manu3","modify":true}))
        );
        assert!(
            md.target(&json!({"profile_id":"manu3","name":"AU"}))
                .is_err()
        );
        // 撤销：三个动作都认 {restore}，参数里不带密码
        let old = json!({"mode":1,"manual":"manu2","fp":"ab"});
        for s in [m, e, md] {
            assert_eq!(s.item, APN);
            assert_eq!(s.confirm, Confirm::Apn);
            assert_eq!(s.target(&s.to_params(&old)), Ok(old.clone()));
        }
        assert!(m.target(&json!({"restore":{"password":"x"}})).is_err());
    }

    #[test]
    fn cell_targets_are_canonical() {
        let nr = find("cell.lock_nr").unwrap();
        let t = nr
            .target(&json!({"pci":"0603","arfcn":627264,"band":"n78"}))
            .unwrap();
        assert_eq!(t, json!({"pci":"603","arfcn":"627264","band":"78"}));
        assert_eq!(nr.target(&nr.to_params(&t)), Ok(t));
        assert_eq!(nr.target(&json!({"off":true})), Ok(cell_off()));
        assert!(
            nr.target(&json!({"pci":"603","arfcn":"x","band":"78"}))
                .is_err()
        );
        assert!(nr.target(&json!({"pci":"603","arfcn":"1"})).is_err());
        let lte = find("cell.lock_lte").unwrap();
        assert_eq!(
            lte.target(&json!({"pci":"12","earfcn":"1650"})),
            Ok(json!({"pci":"12","earfcn":"1650"}))
        );
        assert_eq!(
            lte.target(&json!({"pci":"12"})),
            Err("missing parameter: earfcn".into())
        );
    }
}
