use crate::state;
use serde_json::{Map, Value, json};
use std::time::Duration;

#[derive(Debug)]
pub enum Outcome {
    NotHandled,
    Invalid(String),
    Failed(String),
    Ok(Value),
}

pub const ACTIONS: &[&str] = &[
    "device.reboot",
    "device.poweroff",
    "cellular.set",
    "network.set_mode",
    "band.set_lte",
    "band.set_nr_sa",
    "band.set_nr_nsa",
    "band.set_nr",
    "cell.lock_lte",
    "cell.lock_nr",
    "band.reset",
    "wifi.set_module",
    "lan.set",
    "power.direct_supply.set",
    "usb.set",
    "usb.attach_mode",
    "nfc.set",
    "apn.set_mode",
    "apn.add",
    "apn.modify",
    "apn.delete",
    "apn.enable",
    "sms.delete",
    "sms.mark_read",
    "sms.send_raw",
    "sms.list_after",
];

/// E4 T7b 新加的写：agent 的搜网流程（D17，会话里的步骤）和其他影响上网的写。
/// 和 `op.*` 一样先不进 `/capabilities`（T13 一起加），旧客户端看到的能力表不变。
pub const E4_ACTIONS: &[&str] = &[
    "netselect.scan",
    "netselect.register",
    "netselect.auto",
    "cellular.redial",
    "modem.online",
    "apn.set_pdp_type",
    "wifi.apply",
    "wifi.reload",
    "vendor.call",
    "dns.doh",
    "sms.db_delete",
    "wifi.power_save",
];

/// E4 T7c：其余原厂设置的写，参数原样交给原厂（和 agent 以前直接调的一样），
/// 只是改由 datad 一家写、记流水账。一个 (对象, 方法) 一行，不在表里的一律拒。
pub const VENDOR_CALLS: &[(&str, &str)] = &[
    // 路由（网页的路由页）
    ("zwrt_router.api", "router_set_lan_para"),
    ("zwrt_router.api", "router_set_wan_dns"),
    ("zwrt_router.api", "router_set_firewall_switch"),
    ("zwrt_router.api", "router_set_firewall_level"),
    ("zwrt_router.api", "router_set_nat_switch"),
    ("zwrt_router.api", "router_set_dmz"),
    ("zwrt_router.api", "router_set_upnp_switch"),
    ("zwrt_router.api", "router_set_portforward"),
    ("zwrt_router.api", "router_set_portforward_switch"),
    ("zwrt_router.api", "router_set_alg_switch"),
    ("zwrt_router.api", "router_set_qos_switch"),
    ("zwrt_router.api", "router_set_domain_filter"),
    // SIM PIN / 网络锁（流水账里 PIN、PUK、NCK 只写已改）
    ("zwrt_zte_mdm.api", "sim_verify_pin_puk"),
    ("zwrt_zte_mdm.api", "sim_change_pin"),
    ("zwrt_zte_mdm.api", "sim_change_pin_mode"),
    ("zwrt_zte_mdm.api", "set_simlock_nck"),
    // STC 小区锁、信号检测
    ("zte_nwinfo_api", "nwinfo_set_stc_white_list_par"),
    ("zte_nwinfo_api", "nwinfo_stc_cell_lock_enable"),
    ("zte_nwinfo_api", "nwinfo_stc_cell_lock_disable"),
    ("zte_nwinfo_api", "nwinfo_stc_cell_lock_reset"),
    ("zte_nwinfo_api", "nwinfo_start_detect_signal_quality"),
    ("zte_nwinfo_api", "nwinfo_end_detect_signal_quality"),
    // 设备
    ("zwrt_mc.device.manager", "set_device_info"),
    ("zwrt_mc.device.manager", "device_reset"),
    ("system", "reboot"),
    ("zwrt_bsp.powerbank", "set"),
    ("zwrt_zte_sleep_faw.wakelock", "enableAutoSleep"),
    // 短信：agent 原样发的那种（sms.send_raw 会加密、固定 UNICODE，和它不一样）
    ("zwrt_wms", "zte_libwms_send_sms"),
];

fn object(params: &Value) -> &Map<String, Value> {
    params.as_object().expect("server validates params")
}
pub(crate) fn string(params: &Value, name: &str, required: bool) -> Result<Option<String>, String> {
    match object(params).get(name) {
        Some(Value::String(value)) if value.len() <= 8192 => Ok(Some(value.clone())),
        Some(_) => Err(format!("{name} must be a string")),
        None if required => Err(format!("missing parameter: {name}")),
        None => Ok(None),
    }
}
pub(crate) fn integer(params: &Value, name: &str, required: bool) -> Result<Option<i64>, String> {
    match object(params).get(name) {
        Some(Value::Number(value)) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be an integer")),
        Some(_) => Err(format!("{name} must be an integer")),
        None if required => Err(format!("missing parameter: {name}")),
        None => Ok(None),
    }
}
/// JSON 布尔或数字 0/1（上游 b8828c1 的放宽）。其余输入（缺失、2、1.5、字符串……）
/// 的错误文字保持旧版不变：`{name} must be boolean`。
pub(crate) fn boolean(params: &Value, name: &str) -> Result<bool, String> {
    match object(params).get(name) {
        Some(Value::Bool(v)) => Ok(*v),
        Some(Value::Number(n)) if n.as_u64() == Some(0) => Ok(false),
        Some(Value::Number(n)) if n.as_u64() == Some(1) => Ok(true),
        _ => Err(format!("{name} must be boolean")),
    }
}
fn mapped(params: &Value, specs: &[(&str, &str, bool, bool)]) -> Result<Value, String> {
    let mut args = Map::new();
    for (input, output, required, is_int) in specs {
        if *is_int {
            if let Some(value) = integer(params, input, *required)? {
                args.insert((*output).into(), json!(value));
            }
        } else if let Some(value) = string(params, input, *required)? {
            args.insert((*output).into(), json!(value));
        }
    }
    Ok(Value::Object(args))
}
async fn call(service: &str, method: &str, args: Value) -> Outcome {
    write_outcome(crate::executor::call(service, method, &args).await)
}

/// 原厂有的写成功时什么都不回（`nwinfo_set_netselect`、`zwrt_wlan reload`，E4 T12 真机 B31），
/// 这不算失败：结果靠之后的读回（事务）或调用方自己回读。
fn write_outcome(reply: Result<Value, crate::ubus::client::UbusError>) -> Outcome {
    match write_reply(reply) {
        Ok(value) => Outcome::Ok(value),
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

/// 原厂写的回复：什么都没回算成功（`{}`），其余原样。`/control` 和事务引擎共用。
pub(crate) fn write_reply(
    reply: Result<Value, crate::ubus::client::UbusError>,
) -> Result<Value, crate::ubus::client::UbusError> {
    match reply {
        Err(crate::ubus::client::UbusError::NoData { .. }) => Ok(json!({})),
        other => other,
    }
}
async fn mapped_call(
    params: &Value,
    service: &str,
    method: &str,
    specs: &[(&str, &str, bool, bool)],
    require_any: bool,
) -> Outcome {
    match mapped(params, specs) {
        Ok(Value::Object(args)) if require_any && args.is_empty() => {
            Outcome::Invalid("no fields supplied".into())
        }
        Ok(args) => call(service, method, args).await,
        Err(error) => Outcome::Invalid(error),
    }
}

pub async fn execute(action: &str, params: &Value) -> Outcome {
    match action {
        "device.reboot" => {
            call(
                "zwrt_mc.device.manager",
                "device_reboot",
                json!({"moduleName":"web"}),
            )
            .await
        }
        "device.poweroff" => {
            call(
                "zwrt_mc.device.manager",
                "device_poweroff",
                json!({"moduleName":"web"}),
            )
            .await
        }
        "cellular.set" => cellular_set(params).await,
        "network.set_mode" => {
            mapped_call(
                params,
                "zte_nwinfo_api",
                "nwinfo_set_netselect",
                &[("mode", "net_select", true, false)],
                false,
            )
            .await
        }
        "band.set_lte" => band(params, true, false).await,
        "band.set_nr_sa" => band(params, false, false).await,
        "band.set_nr_nsa" => band(params, false, true).await,
        "band.set_nr" => band_steps(params, &["sa", "nsa"]).await,
        "cell.lock_lte" => {
            mapped_call(
                params,
                "zte_nwinfo_api",
                "nwinfo_lock_lte_cell",
                &[
                    ("pci", "lock_lte_pci", true, false),
                    ("earfcn", "lock_lte_earfcn", true, false),
                ],
                false,
            )
            .await
        }
        "cell.lock_nr" => {
            mapped_call(
                params,
                "zte_nwinfo_api",
                "nwinfo_lock_nr_cell",
                &[
                    ("pci", "lock_nr_pci", true, false),
                    ("arfcn", "lock_nr_earfcn", true, false),
                    ("band", "lock_nr_cell_band", true, false),
                ],
                false,
            )
            .await
        }
        // D17：搜网会话里的步骤（会话规则在 ops::engine::session_gate）。
        // 原厂的搜网调用可能等到搜完才回：agent 本来就只给 3 秒，超时也照样看状态。
        "netselect.scan" => call("zte_nwinfo_api", "nwinfo_manual_scan", json!({})).await,
        "netselect.register" => netselect_register(params).await,
        // 回自动选网：原厂没有能用的 ubus 调用（9-26 读过原厂搜网协议），只能 AT+COPS=0
        "netselect.auto" => at_outcome(crate::at::send(crate::at::Cmd::CopsAuto).await),
        "cellular.redial" => redial(params).await,
        // nwinfo_set_mode ONLINE 不能把基带从 LPM 拉回来，只有 AT+CFUN=1 行
        "modem.online" => at_outcome(crate::at::send(crate::at::Cmd::CfunOnline).await),
        "apn.set_pdp_type" => apn_pdp_type(params).await,
        "wifi.apply" => wifi_apply(params).await,
        "wifi.reload" => call("zwrt_wlan", "reload", json!({})).await,
        "vendor.call" => vendor_call(params).await,
        "dns.doh" => dns_doh(params).await,
        "sms.db_delete" => sms_db_delete(params).await,
        "wifi.power_save" => wifi_power_save(params).await,
        // 带三组频段的 band.reset 是撤销「恢复默认」：把三组写回去（小区锁不动）。
        "band.reset" if params.as_object().is_some_and(|m| !m.is_empty()) => {
            band_steps(params, &["lte", "sa", "nsa"]).await
        }
        // 原厂「恢复默认频段/小区」：解开全部锁频和锁小区（触屏锁频页的重置按钮）。
        "band.reset" => {
            call(
                "zte_nwinfo_api",
                "nwinfo_reset_band_cell_setting",
                json!({}),
            )
            .await
        }
        "wifi.set_module" => wifi_module(params).await,
        "lan.set" => {
            mapped_call(
                params,
                "zwrt_router.api",
                "router_set_lan_para",
                &[
                    ("ip", "ipaddr", false, false),
                    ("netmask", "netmask", false, false),
                    ("dhcp_disabled", "ignore", false, true),
                    ("dhcp_start", "zte_start", false, false),
                    ("dhcp_end", "zte_end", false, false),
                    ("lease_seconds", "leasetime", false, false),
                ],
                true,
            )
            .await
        }
        "power.direct_supply.set" => direct_supply(params).await,
        "usb.set" => {
            mapped_call(
                params,
                "zwrt_bsp.usb",
                "set",
                &[
                    ("mode", "mode", false, false),
                    ("port_switch", "usb_port_switch", false, false),
                    ("network_protocol", "usb_network_protocal", false, false),
                ],
                true,
            )
            .await
        }
        "usb.attach_mode" => usb_attach_mode(params).await,
        "nfc.set" => nfc(params).await,
        "apn.set_mode" => {
            mapped_call(
                params,
                "zwrt_apn_object",
                "set_apn_mode",
                &[("mode", "apn_mode", true, true)],
                false,
            )
            .await
        }
        "apn.add" => apn(params, "add_manu_apn", false).await,
        "apn.modify" => apn(params, "modify_manu_apn", true).await,
        "apn.delete" => {
            // 正在用的那条不能删（删了连接就没了，也没法撤销）
            if let Ok(Some(id)) = string(params, "profile_id", true)
                && apn_in_use(&id).await
            {
                return Outcome::Invalid("cannot delete the APN profile in use".into());
            }
            mapped_call(
                params,
                "zwrt_apn_object",
                "delete_manu_apn",
                &[("profile_id", "profileId", true, false)],
                false,
            )
            .await
        }
        "apn.enable" => {
            mapped_call(
                params,
                "zwrt_apn_object",
                "enable_manu_apn_id",
                &[("profile_id", "profileId", true, false)],
                false,
            )
            .await
        }
        "sms.delete" => {
            mapped_call(
                params,
                "zwrt_wms",
                "zwrt_wms_delete_sms",
                &[("ids", "id", true, false)],
                false,
            )
            .await
        }
        "sms.mark_read" => {
            mapped_call(
                params,
                "zwrt_wms",
                "zwrt_wms_modify_tag",
                &[("ids", "id", true, false), ("tag", "tag", false, true)],
                false,
            )
            .await
        }
        "sms.send_raw" => match crate::sms::send(params).await {
            Ok(value) => Outcome::Ok(value),
            Err((true, error)) => Outcome::Invalid(error),
            Err((false, error)) => Outcome::Failed(error),
        },
        "sms.list_after" => match crate::sms::list_after(params).await {
            Ok(value) => Outcome::Ok(value),
            Err((true, error)) => Outcome::Invalid(error),
            Err((false, error)) => Outcome::Failed(error),
        },
        _ => Outcome::NotHandled,
    }
}

async fn cellular_set(params: &Value) -> Outcome {
    let overrides = match mapped(
        params,
        &[
            ("enabled", "enable", false, true),
            ("roaming", "roam_enable", false, true),
            ("connect_mode", "connect_mode", false, false),
        ],
    ) {
        Ok(Value::Object(v)) if !v.is_empty() => v,
        Ok(_) => return Outcome::Invalid("no cellular fields supplied".into()),
        Err(e) => return Outcome::Invalid(e),
    };
    let current = match state::ubus(
        "zwrt_data",
        "get_wwaniface",
        json!({"source_module":"web","cid":1,"connect_status":""}),
    )
    .await
    {
        Ok(Value::Object(v)) => v,
        Ok(_) => return Outcome::Failed("invalid get_wwaniface response".into()),
        Err(e) => return Outcome::Failed(e),
    };
    call(
        "zwrt_data",
        "set_wwaniface",
        cellular_args(current, overrides),
    )
    .await
}

/// 读回的整份 + 要改的。读回的 `enable` 不带回去：它是最近一次写进去的值，开机后默认 0，
/// 自动拨号照样连着；带回去就是一次「关数据」，改漫游会把数据断掉（T12 真机，B31）。
/// 原厂只改写了的键（T12：只写 `enable` 时漫游、拨号方式都没动）。
fn cellular_args(mut current: Map<String, Value>, overrides: Map<String, Value>) -> Value {
    current.remove("enable");
    current.extend(overrides);
    current.insert("source_module".into(), json!("WEBUI"));
    current.insert("cid".into(), json!(1));
    Value::Object(current)
}
fn at_outcome(r: Result<String, String>) -> Outcome {
    match r {
        Ok(a) => Outcome::Ok(json!({"answer": a.trim()})),
        Err(e) => Outcome::Failed(e),
    }
}

/// `{mcc_mnc:"46001", rat?:"…"}` → nwinfo_manual_register `{m_mcc_mnc, m_rat}`（agent 一直这样发）。
async fn netselect_register(params: &Value) -> Outcome {
    let plmn = match string(params, "mcc_mnc", true) {
        Ok(Some(v)) if (5..=6).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_digit()) => v,
        Ok(_) => return Outcome::Invalid("mcc_mnc must be 5-6 digits".into()),
        Err(e) => return Outcome::Invalid(e),
    };
    let rat = match string(params, "rat", false) {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => return Outcome::Invalid(e),
    };
    if rat.len() > 8 || !rat.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Outcome::Invalid("rat must be a short code".into());
    }
    call(
        "zte_nwinfo_api",
        "nwinfo_manual_register",
        json!({"m_mcc_mnc": plmn, "m_rat": rat}),
    )
    .await
}

/// 重新拨号：`type` 1（IPv4）、2（IPv6），不给就两条都拨（agent ensure_data_up 的做法）。
async fn redial(params: &Value) -> Outcome {
    let types: Vec<i64> = match integer(params, "type", false) {
        Ok(Some(t @ (1 | 2))) => vec![t],
        Ok(Some(_)) => return Outcome::Invalid("type must be 1 or 2".into()),
        Ok(None) => vec![1, 2],
        Err(e) => return Outcome::Invalid(e),
    };
    let mut last = Value::Null;
    for t in types {
        match state::ubus(
            "zwrt_qcmap_cli",
            "set_qcliiface",
            json!({"source_module":"zte_topsw_data","type":t,"enable":1,"sub_id":1}),
        )
        .await
        {
            Ok(v) => last = v,
            Err(e) => return Outcome::Failed(e),
        }
    }
    Outcome::Ok(last)
}

/// WAN IPv6 开关（agent router_wan_ipv6_set 的做法）：拨号 APN 的 PDP 类型改成 IPv4v6（3）或
/// 只 IPv4（1），别的字段照原样写回；再把正在用的连接的 IPv6 那条腿拉起或断开。
async fn apn_pdp_type(params: &Value) -> Outcome {
    let enabled = match boolean(params, "ipv6") {
        Ok(v) => v,
        Err(e) => return Outcome::Invalid(e),
    };
    let apn = match state::ubus("zwrt_apn_object", "get_apn_at_cid", json!({"cid":1})).await {
        Ok(v) => v,
        Err(e) => return Outcome::Failed(format!("read APN failed: {e}")),
    };
    let pdp = if enabled { 3 } else { 1 };
    let s = |k: &str| apn.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let n = |k: &str, d: i64| apn.get(k).and_then(Value::as_i64).unwrap_or(d);
    let req = json!({
        "profilename": s("profilename"),
        "wanapn": s("wanapn"),
        "username": s("username"),
        "password": s("password"),
        "pdpType": pdp,
        "pppAuthMode": n("pppAuthMode", 0),
        "profileId": s("profileId"),
        "isEnable": true,
        "cid": 1,
        "isValid": n("isValid", 1),
        "extraInt1": n("extraInt1", 0),
        "roamingPdpType": pdp,
    });
    if let Err(e) = state::ubus("zwrt_apn_object", "set_apn_at_cid", req).await {
        return Outcome::Failed(format!("set APN failed: {e}"));
    }
    // 正在用的连接：type 2 是 IPv6 那条腿；失败不算（PDP 类型已改，下次拨号生效）
    let _ = state::ubus(
        "zwrt_qcmap_cli",
        "set_qcliiface",
        json!({"source_module":"zte_topsw_data","type":2,"enable": if enabled {1} else {0},"sub_id":1}),
    )
    .await;
    Outcome::Ok(json!({"ipv6_enabled": enabled, "pdp_type": pdp}))
}

async fn band(params: &Value, lte: bool, nsa: bool) -> Outcome {
    let bands = match string(params, "bands", false) {
        Ok(v) => v.unwrap_or_default(),
        Err(e) => return Outcome::Invalid(e),
    };
    if !bands.bytes().all(|b| b.is_ascii_digit() || b == b',') {
        return Outcome::Invalid("bands must contain only numbers and commas".into());
    }
    if lte {
        call(
            "zte_nwinfo_api",
            "nwinfo_set_lte_ext_band",
            json!({"lte_band":bands}),
        )
        .await
    } else {
        call(
            "zte_nwinfo_api",
            "nwinfo_set_nrbandlock",
            json!({"nr5g_type":if nsa{"1"}else{"0"},"nr5g_band":bands}),
        )
        .await
    }
}
/// 一个 ubus 写一组频段：`lte` 走 LTE 扩展频段，`sa`/`nsa` 走 5G 锁频（原厂 nr5g_type 0/1）。
pub(crate) fn band_call(key: &str, bands: &str) -> (&'static str, &'static str, Value) {
    match key {
        "lte" => (
            "zte_nwinfo_api",
            "nwinfo_set_lte_ext_band",
            json!({"lte_band": bands}),
        ),
        _ => (
            "zte_nwinfo_api",
            "nwinfo_set_nrbandlock",
            json!({"nr5g_type": if key == "nsa" {"1"} else {"0"}, "nr5g_band": bands}),
        ),
    }
}

/// 几组频段依次写（`band.set_nr`、撤销恢复默认）：参数先全部校验，第一步失败就停。
async fn band_steps(params: &Value, keys: &[&str]) -> Outcome {
    let mut steps = Vec::new();
    for k in keys {
        match string(params, k, true) {
            Ok(Some(v)) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit() || b == b',') => {
                steps.push(band_call(k, &v));
            }
            Ok(_) => return Outcome::Invalid(format!("{k} must contain only numbers and commas")),
            Err(e) => return Outcome::Invalid(e),
        }
    }
    let mut last = Outcome::Ok(Value::Null);
    for (object, method, args) in steps {
        last = call(object, method, args).await;
        if !matches!(last, Outcome::Ok(_)) {
            break;
        }
    }
    last
}
/// `power.direct_supply.set` 的 `enabled`：JSON 布尔、0/1，以及旧 C 版也认的
/// 字符串 "0"/"1"/"true"/"false"（C 版 `required_bool_param`）。其余输入的错误文字不变。
pub(crate) fn direct_supply_enabled(params: &Value) -> Result<bool, String> {
    match object(params).get("enabled") {
        Some(Value::String(v)) if v == "1" || v == "true" => Ok(true),
        Some(Value::String(v)) if v == "0" || v == "false" => Ok(false),
        _ => boolean(params, "enabled"),
    }
}
/// 写入回复是否算失败（C 版逻辑）：没有数据（空或全空白，B20 成功时就这样）不算失败，
/// 靠后面的回读确认；有数据时必须是对象、没有 `error`、`result`（缺省 0）为 0。
fn direct_supply_write_failed(reply: &Result<Value, crate::ubus::client::UbusError>) -> bool {
    match reply {
        Err(crate::ubus::client::UbusError::NoData { .. }) => false,
        Err(_) => true,
        Ok(Value::Object(body)) => {
            let result = body.get("result").map_or(0, |v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    .unwrap_or(0)
            });
            body.contains_key("error") || result != 0
        }
        Ok(_) => true,
    }
}
async fn direct_supply(params: &Value) -> Outcome {
    let wanted = match direct_supply_enabled(params) {
        Ok(v) => v,
        Err(e) => return Outcome::Invalid(e),
    };
    let before = match state::ubus("zwrt_bsp.charger", "list", json!({})).await {
        Ok(v) => v,
        Err(e) => return Outcome::Failed(e),
    };
    let mode = before
        .get("direct_power_supply_mode")
        .and_then(Value::as_str);
    let Some(mode @ ("enable" | "disable")) = mode else {
        return Outcome::Failed("direct supply is not supported or state is unknown".into());
    };
    let changed = (mode == "enable") != wanted;
    if changed {
        let reply = crate::executor::call(
            "zwrt_bsp.charger",
            "set",
            &json!({"direct_power_supply_mode":if wanted{"enable"}else{"disable"}}),
        )
        .await;
        if direct_supply_write_failed(&reply) {
            return Outcome::Failed(match reply {
                Err(e) => e.to_string(),
                Ok(_) => "direct supply write failed".into(),
            });
        }
        let expected = if wanted { "enable" } else { "disable" };
        let mut verified = false;
        for attempt in 0..5 {
            if attempt != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            if state::ubus("zwrt_bsp.charger", "list", json!({}))
                .await
                .ok()
                .and_then(|value| {
                    value
                        .get("direct_power_supply_mode")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some(expected)
            {
                verified = true;
                break;
            }
        }
        if !verified {
            return Outcome::Failed(
                "direct supply readback did not confirm the requested mode".into(),
            );
        }
    }
    Outcome::Ok(
        json!({"supported":true,"enabled":wanted,"mode":if wanted{"enable"}else{"disable"},"changed":changed,"verified":true}),
    )
}
/// 原厂屏幕（`zte_topsw_devui`）调这两个写时带的 `source_module`，照抄。
const USB_SOURCE_MODULE: &str = "zte_topsw_devui";
const USB_MODES: &[&str] = &["share", "fast_charge", "accessory"];

fn int_field(v: &Value, name: &str) -> Option<i64> {
    let f = v.get(name)?;
    f.as_i64()
        .or_else(|| f.as_str().and_then(|s| s.trim().parse().ok()))
}

async fn poll_until(
    object: &str,
    method: &str,
    args: Value,
    tries: u32,
    ok: impl Fn(&Value) -> bool,
) -> bool {
    for attempt in 0..tries {
        if attempt != 0 {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if state::ubus(object, method, args.clone())
            .await
            .is_ok_and(|v| ok(&v))
        {
            return true;
        }
    }
    false
}

/// 插线时的 USB 用法（manager docs/designs/usb-attach-mode.md）。U60 给插上的手机供电时是
/// source + host，手机没有网卡；原厂屏幕这时弹窗问用法，这里做和它一样的调用（B31 真机 `ubus monitor`
/// 2026-10-07 抓到的原样，`source_module` 也照抄）：
/// - `share`（充电 + 上网）：`zwrt_bsp.typec set DR_Swap device`，读回 `data_role` = device；
/// - `fast_charge`（快速充电宝）：`zwrt_bsp.powerbank set state 1`，读回 `state` = 1；
/// - `accessory`（网口配件）：原厂什么都不调，这里也不写，回 ok。
///
/// 没插东西（`cc_attch_state` 不是 1）就不写。已经是要的状态不再写（`changed:false`）。
/// `remembered:true`（触屏按记住的选择自动做）时，插着 USB 网口转接头（`zwrt_bsp.usb list` 的
/// `usb2rj45` = 1）就不切 share：转接头要 U60 当主机。用户在弹窗里亲手选的不拦。
/// 读回有上限（执行者在等）：角色切换 3 秒、充电宝 2 秒，没确认就回失败。
async fn usb_attach_mode(params: &Value) -> Outcome {
    let mode = match string(params, "mode", true) {
        Ok(Some(m)) if USB_MODES.contains(&m.as_str()) => m,
        Ok(_) => return Outcome::Invalid("mode must be share, fast_charge or accessory".into()),
        Err(e) => return Outcome::Invalid(e),
    };
    let remembered = match object(params).get("remembered") {
        None => false,
        Some(_) => match boolean(params, "remembered") {
            Ok(v) => v,
            Err(e) => return Outcome::Invalid(e),
        },
    };
    let typec = match state::ubus("zwrt_bsp.typec", "list", json!({})).await {
        Ok(v) => v,
        Err(e) => return Outcome::Failed(e),
    };
    if int_field(&typec, "cc_attch_state") != Some(1) {
        return Outcome::Failed("nothing is plugged into the USB-C port".into());
    }
    let done =
        |changed: bool| Outcome::Ok(json!({"mode": mode, "changed": changed, "verified": true}));
    match mode.as_str() {
        "accessory" => done(false),
        "share" => {
            if typec.get("data_role").and_then(Value::as_str) == Some("device") {
                return done(false);
            }
            if remembered {
                match state::ubus("zwrt_bsp.usb", "list", json!({})).await {
                    Ok(usb) if int_field(&usb, "usb2rj45") == Some(1) => {
                        return Outcome::Failed(
                            "a USB network adapter is attached; left in host mode".into(),
                        );
                    }
                    Ok(_) => {}
                    Err(e) => return Outcome::Failed(e),
                }
            }
            let args = json!({"source_module": USB_SOURCE_MODULE, "DR_Swap": "device"});
            if let Outcome::Failed(e) = call("zwrt_bsp.typec", "set", args).await {
                return Outcome::Failed(e);
            }
            if poll_until("zwrt_bsp.typec", "list", json!({}), 15, |v| {
                v.get("data_role").and_then(Value::as_str) == Some("device")
            })
            .await
            {
                done(true)
            } else {
                Outcome::Failed("USB role readback did not show data_role device".into())
            }
        }
        _ => {
            let get = json!({"property": "state"});
            let on = |v: &Value| int_field(v, "state") == Some(1);
            match state::ubus("zwrt_bsp.powerbank", "get", get.clone()).await {
                Ok(v) if on(&v) => return done(false),
                Ok(_) => {}
                Err(e) => return Outcome::Failed(e),
            }
            let args = json!({"source_module": USB_SOURCE_MODULE, "state": 1});
            if let Outcome::Failed(e) = call("zwrt_bsp.powerbank", "set", args).await {
                return Outcome::Failed(e);
            }
            if poll_until("zwrt_bsp.powerbank", "get", get, 10, on).await {
                done(true)
            } else {
                Outcome::Failed("power bank readback did not show state 1".into())
            }
        }
    }
}
async fn nfc(params: &Value) -> Outcome {
    let args = match mapped(
        params,
        &[
            ("enabled", "switch", true, true),
            ("flag", "flag", false, true),
        ],
    ) {
        Ok(v) => v,
        Err(e) => return Outcome::Invalid(e),
    };
    match state::ubus("zwrt_nfc", "zwrt_nfc_wifi_set", args).await {
        Ok(value) => {
            let _ = state::ubus("zwrt_nfc", "zwrt_nfc_wifi_change", json!({})).await;
            Outcome::Ok(value)
        }
        Err(e) => Outcome::Failed(e),
    }
}
/// 手动模式下启用着的那条手动 profile 就是正在用的。读不到时当作在用（宁可不删）。
async fn apn_in_use(id: &str) -> bool {
    let o = "zwrt_apn_object";
    let (Ok(mode), Ok(list)) = (
        state::ubus(o, "get_apn_mode", json!({})).await,
        state::ubus(o, "get_manu_apn_list", json!({})).await,
    ) else {
        return true;
    };
    match crate::ops::apn::Now::parse(&mode, &list) {
        Some(n) => n.mode == 1 && n.manual == id,
        None => true,
    }
}

async fn apn(params: &Value, method: &str, profile_required: bool) -> Outcome {
    mapped_call(
        params,
        "zwrt_apn_object",
        method,
        &[
            ("profile_id", "profileId", profile_required, false),
            ("name", "profilename", true, false),
            ("apn", "wanapn", true, false),
            ("username", "username", false, false),
            ("password", "password", false, false),
            ("auth_mode", "pppAuthMode", false, true),
            ("pdp_type", "pdpType", false, true),
            ("roaming_pdp_type", "roamingPdpType", false, true),
        ],
        false,
    )
    .await
}

/// `zwrt_wlan set` arguments for the Wi-Fi master switch, as the stock web UI
/// builds them (B31 `/usr/zte_web/web/js/`, read 10-04): `{"zte_mbb":{"wifi_onoff":
/// "0"|"1"}}`, and when switching on also `lbd` (band steering) as it stands,
/// so turning Wi-Fi back on keeps it. `lbd` is left out when it can't be read.
fn wifi_module_args(enabled: bool, lbd: Option<&str>) -> Value {
    let mut mbb = Map::new();
    mbb.insert("wifi_onoff".into(), json!(if enabled { "1" } else { "0" }));
    if enabled && let Some(lbd @ ("0" | "1")) = lbd {
        mbb.insert("lbd".into(), json!(lbd));
    }
    json!({"zte_mbb": mbb})
}

/// `wifi.set_module {enabled: 0|1}`: the firmware's whole-Wi-Fi switch
/// (`wireless.zte_mbb.wifi_onoff`), switched the stock way.
async fn wifi_module(params: &Value) -> Outcome {
    let enabled = match integer(params, "enabled", true) {
        Ok(Some(v @ (0 | 1))) => v == 1,
        Ok(_) => return Outcome::Invalid("enabled must be 0 or 1".into()),
        Err(e) => return Outcome::Invalid(e),
    };
    let lbd = if enabled {
        state::ubus(
            "uci",
            "get",
            json!({"config":"wireless","section":"zte_mbb","option":"lbd"}),
        )
        .await
        .ok()
        .and_then(|v| v.get("value").and_then(Value::as_str).map(str::to_owned))
    } else {
        None
    };
    call(
        "zwrt_wlan",
        "set",
        wifi_module_args(enabled, lbd.as_deref()),
    )
    .await
}

/// AP options `wifi.apply` may set (zte-agent's Wi-Fi pages, AP switch, scenario,
/// home-mode scan) on `wireless.{main,guest}_{2g,5g}`.
const WIFI_APPLY_AP_SECTIONS: &[&str] = &["main_2g", "main_5g", "guest_2g", "guest_5g"];
const WIFI_APPLY_AP_OPTIONS: &[&str] = &[
    "ssid",
    "key",
    "encryption",
    "hidden",
    "isolate",
    "disabled",
    "guest_active_time",
];
/// Radio options, on `wireless.wifi0`/`wifi1` (what callers write) or the radio
/// sections the firmware names in `wireless.main_<band>.device`.
const WIFI_APPLY_RADIO_OPTIONS: &[&str] =
    &["country", "channel", "txpowerpercent", "htmode", "disabled"];
/// Paths the agent before manager 1d9755a sends (best_effort, alone in their
/// own call) for the Wi-Fi master switch and Wi-Fi 6. There is no `zte_mbb`
/// uci package: the firmware keeps these in `wireless.zte_mbb`, and changes
/// them through `zwrt_wlan set` (the master switch is `wifi.set_module`; Wi-Fi 6
/// goes with each radio's hwmode). A raw uci write is neither, so these are
/// accepted but never written, always listed in `skipped` — the same answer
/// that agent got when the write failed, without a refused request per save.
const WIFI_APPLY_DEAD_KEYS: &[&str] = &["zte_mbb.wifi.wifi_onoff", "zte_mbb.wifi.wifi6_switch"];

fn wifi_apply_key_ok(path: &str, radios: &[String; 2]) -> bool {
    if WIFI_APPLY_DEAD_KEYS.contains(&path) {
        return true;
    }
    let mut it = path.splitn(3, '.');
    let (Some("wireless"), Some(sec), Some(opt)) = (it.next(), it.next(), it.next()) else {
        return false;
    };
    (WIFI_APPLY_AP_SECTIONS.contains(&sec) && WIFI_APPLY_AP_OPTIONS.contains(&opt))
        || ((matches!(sec, "wifi0" | "wifi1") || radios.iter().any(|r| r == sec))
            && WIFI_APPLY_RADIO_OPTIONS.contains(&opt))
}

/// Several uci options at once, one commit per package, then (unless
/// `reload:false`) one `zwrt_wlan reload` — written and reloaded even when uci
/// already holds the values: the agent's AP switch retries that way, and its
/// own verify (polling hostapd) is the judge, not this reply (E4 T7b).
/// `best_effort:true`: an option that cannot be set (a guest section this
/// firmware lacks) is skipped and listed, not an error. The dead `zte_mbb.wifi.*`
/// paths are always skipped (see [`WIFI_APPLY_DEAD_KEYS`]).
async fn wifi_apply(params: &Value) -> Outcome {
    let Some(set) = object(params).get("set").and_then(Value::as_object) else {
        return Outcome::Invalid("missing parameter: set".into());
    };
    if set.is_empty() || set.len() > 32 {
        return Outcome::Invalid("set must have 1-32 options".into());
    }
    let reload = match object(params).get("reload") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Outcome::Invalid("reload must be boolean".into()),
    };
    let best_effort = object(params)
        .get("best_effort")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let radios = crate::wifi::radio_sections().await;
    let mut updates = Vec::new();
    let mut skipped = Vec::new();
    for (path, v) in set {
        if !wifi_apply_key_ok(path, &radios) {
            return Outcome::Invalid(format!("not a Wi-Fi option this action sets: {path}"));
        }
        let value = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            _ => {
                return Outcome::Invalid(format!(
                    "{path}: value must be a string, number or boolean"
                ));
            }
        };
        if value.len() > 128 || value.contains(['\0', '\r', '\n']) {
            return Outcome::Invalid(format!("{path}: invalid value"));
        }
        if WIFI_APPLY_DEAD_KEYS.contains(&path.as_str()) {
            skipped.push(path.clone());
            continue;
        }
        updates.push((path.clone(), value));
    }
    let mut packages: Vec<&str> = Vec::new();
    for (path, value) in &updates {
        if let Err(e) = state::uci_write("set", path, Some(value)).await {
            if best_effort {
                skipped.push(path.clone());
                continue;
            }
            let _ = state::uci_write("revert", "wireless", None).await;
            return Outcome::Failed(format!("{path}: {e}"));
        }
        if packages.is_empty() {
            packages.push("wireless");
        }
    }
    for p in &packages {
        if let Err(e) = state::uci_write("commit", p, None).await {
            return Outcome::Failed(format!("commit {p}: {e}"));
        }
    }
    let mut reload_error = Value::Null;
    if reload
        && !packages.is_empty()
        && let Err(e) = state::ubus("zwrt_wlan", "reload", json!({})).await
    {
        reload_error = json!(e);
    }
    Outcome::Ok(json!({
        "committed": packages,
        "skipped": skipped,
        "reloaded": reload && !packages.is_empty() && reload_error.is_null(),
        "reload_error": reload_error,
    }))
}

/// `vendor.call {object, method, args}`：表里的原厂调用，`args`（对象）原样交过去。
async fn vendor_call(params: &Value) -> Outcome {
    let (object_name, method) = match (
        string(params, "object", true),
        string(params, "method", true),
    ) {
        (Ok(Some(o)), Ok(Some(m))) => (o, m),
        (Err(e), _) | (_, Err(e)) => return Outcome::Invalid(e),
        _ => return Outcome::Invalid("missing object or method".into()),
    };
    if !VENDOR_CALLS.contains(&(object_name.as_str(), method.as_str())) {
        return Outcome::Invalid(format!(
            "not a vendor call the core makes: {object_name} {method}"
        ));
    }
    let args = match object(params).get("args") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) if v.to_string().len() <= 8192 => v.clone(),
        Some(_) => return Outcome::Invalid("args must be an object (at most 8 KB)".into()),
    };
    call(&object_name, &method, args).await
}

/// DoH：agent 的 DoH 代理（127.0.0.1:5353）随 agent 一起删了（8d-2c，2026-10-14），所以只剩
/// 「关」：删掉 dnsmasq 的附加文件 /tmp/dnsmasq.d/doh.conf 再重启 dnsmasq，给还留着旧转发的
/// 设备收尾。「开」一律拒：转给一个不存在的 5353，全局域网就没有 DNS 了。
/// 关的时候只删它，**不碰 uci 的 dhcp.lan_dns.server/noresolv**：那是透明代理让 dnsmasq 转给
/// 代理的设置，以前在这里整条删掉，关一次 DoH 就让局域网 DNS 绕过代理（审计 2026-10-09
/// F2）。DoH 从来没往 uci 写过；万一有手工写过的 `127.0.0.1#5353` 条目，只删那一条（有才删、
/// 才 commit，平时不写闪存）。
const DOH_OFF_SCRIPT: &str = "rm -f /tmp/dnsmasq.d/doh.conf; uci -q get dhcp.lan_dns.server | grep -qF '127.0.0.1#5353' && uci -q del_list dhcp.lan_dns.server='127.0.0.1#5353' && uci commit dhcp; /etc/init.d/dnsmasq restart";

const DOH_GONE: &str = "DoH was removed with zte-agent (2026-10-14): only enabled=false (drop the dnsmasq forward) is accepted";

async fn dns_doh(params: &Value) -> Outcome {
    match boolean(params, "enabled") {
        Ok(false) => {}
        Ok(true) => return Outcome::Invalid(DOH_GONE.into()),
        Err(e) => return Outcome::Invalid(e),
    }
    match crate::command::run(
        "sh",
        ["-c", DOH_OFF_SCRIPT],
        std::time::Duration::from_secs(8),
    )
    .await
    {
        Ok(_) => Outcome::Ok(json!({"enabled": false})),
        // agent 以前也不看结果：配置文件删了就算，dnsmasq 重启失败如实说
        Err(e) => Outcome::Failed(format!("dnsmasq: {e}")),
    }
}

/// Wi-Fi 节能（MU5250，E4：触屏以前自己写，10-04 用户定改经 datad）。和触屏原来的做法一样：
/// 选择落在 hotplug 脚本里（ifup 时重新套用，换机重启都留得住），再马上套到在用的接口上，
/// 读回 wlan0（没有就 wlan2）。`wifi.psm.set` 是 MU5252 按 SSID 的旧接口，冻结不动。
async fn wifi_power_save(params: &Value) -> Outcome {
    let enabled = match boolean(params, "enabled") {
        Ok(v) => v,
        Err(e) => return Outcome::Invalid(e),
    };
    let dir =
        std::env::var("LUYAZ_HOTPLUG_DIR").unwrap_or_else(|_| "/etc/hotplug.d/iface".into());
    power_save_apply(std::path::Path::new(&dir), &iw_bin(), enabled).await
}

const PSM_IFACES: [&str; 4] = ["wlan0", "wlan1", "wlan2", "wlan3"];

fn psm_script(enabled: bool) -> String {
    let m = if enabled { "on" } else { "off" };
    let mut s = String::from(
        "#!/bin/sh\n# written by luyaz-core (wifi.power_save)\n[ \"$ACTION\" = ifup ] && {\n",
    );
    for w in PSM_IFACES {
        s.push_str(&format!("  iw dev {w} set power_save {m} 2>/dev/null\n"));
    }
    s.push_str("}\n");
    s
}

async fn power_save_apply(dir: &std::path::Path, iw: &str, enabled: bool) -> Outcome {
    use std::os::unix::fs::PermissionsExt;
    // u60d (current_thread, E2 item 2): the file writes go to the blocking pool, same order
    let d = dir.to_owned();
    let saved = crate::state::blocking(move || {
        let dir = d.as_path();
        let file = dir.join("99-disable-powersave");
        let tmp = dir.join(".99-disable-powersave.tmp");
        let saved = std::fs::create_dir_all(dir)
            .and_then(|_| std::fs::write(&tmp, psm_script(enabled)))
            .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)))
            .and_then(|_| std::fs::rename(&tmp, &file));
        if saved.is_err() {
            let _ = std::fs::remove_file(&tmp);
        } else {
            // 很早的触屏版本写过的另一份，留着会和这份打架
            let _ = std::fs::remove_file(dir.join("psm"));
        }
        saved
    })
    .await;
    if let Err(e) = saved {
        return Outcome::Failed(format!("could not save Wi-Fi power save: {e}"));
    }
    let m = if enabled { "on" } else { "off" };
    for w in PSM_IFACES {
        // 关着的接口会报错，照旧忽略：下次 ifup 由脚本套用
        let _ = crate::command::run(
            iw,
            ["dev", w, "set", "power_save", m],
            Duration::from_secs(3),
        )
        .await;
    }
    let mut live = None;
    for w in ["wlan0", "wlan2"] {
        if let Ok(out) =
            crate::command::run(iw, ["dev", w, "get", "power_save"], Duration::from_secs(3)).await
        {
            let out = String::from_utf8_lossy(&out);
            if out.contains("Power save: on") {
                live = Some(true);
            } else if out.contains("Power save: off") {
                live = Some(false);
            }
            if live.is_some() {
                break;
            }
        }
    }
    match live {
        Some(v) if v != enabled => Outcome::Failed(format!(
            "Wi-Fi power save still {}",
            if v { "on" } else { "off" }
        )),
        _ => Outcome::Ok(json!({"enabled": enabled, "saved": true, "live": live})),
    }
}

/// 原厂 `zwrt_wms_delete_sms` 删不掉存在 SIM 里的短信（回 result 3，不删），列表又是从
/// 原厂的 sms.db 读的：agent 删完还在的就直接在库里删（E4 T7c，搬进 datad）。
/// `ids` 只能是数字和分号；SQL 是固定的。
async fn sms_db_delete(params: &Value) -> Outcome {
    let ids = match string(params, "ids", true) {
        Ok(Some(v)) => v,
        Ok(None) => return Outcome::Invalid("missing parameter: ids".into()),
        Err(e) => return Outcome::Invalid(e),
    };
    let list: Vec<&str> = ids.split(';').filter(|s| !s.is_empty()).collect();
    if list.is_empty()
        || list.len() > 500
        || !list
            .iter()
            .all(|s| s.len() <= 12 && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Outcome::Invalid("ids must be numbers separated by ';'".into());
    }
    let db = std::env::var("LUYAZ_SMS_DB")
        .unwrap_or_else(|_| "/etc_rw/ztembb/ztesms/sms_db/sms.db".into());
    let bin = std::env::var("LUYAZ_SQLITE_BIN").unwrap_or_else(|_| "/usr/bin/sqlite3".into());
    let sql = format!("DELETE FROM sms WHERE id IN ({});", list.join(","));
    match crate::command::run(
        &bin,
        ["-cmd".to_string(), ".timeout 2000".to_string(), db, sql],
        std::time::Duration::from_secs(5),
    )
    .await
    {
        Ok(_) => Outcome::Ok(json!({"deleted": list.len()})),
        Err(e) => Outcome::Failed(format!("sqlite3: {e}")),
    }
}

fn iw_bin() -> String {
    std::env::var("LUYAZ_IW_BIN").unwrap_or_else(|_| "/usr/sbin/iw".into())
}

#[cfg(test)]
mod wifi_apply_tests {
    use super::*;

    #[test]
    fn cellular_args_never_carry_the_read_enable() {
        let read = |v: Value| match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        };
        // 开机后读到 enable 0、数据连着；只改漫游时不能把 0 带回去
        let now = read(
            json!({"enable":0,"roam_enable":0,"connect_mode":1,"connect_status":"ipv4_ipv6_connected","cid":1}),
        );
        let a = cellular_args(now.clone(), read(json!({"roam_enable":1})));
        assert_eq!(a.get("enable"), None);
        assert_eq!(a["roam_enable"], json!(1));
        assert_eq!(a["connect_mode"], json!(1));
        assert_eq!(a["source_module"], json!("WEBUI"));
        // 要改数据开关时照写
        let a = cellular_args(now, read(json!({"enable":0})));
        assert_eq!(a["enable"], json!(0));
    }

    #[tokio::test]
    async fn vendor_call_only_listed_pairs() {
        for (o, m) in [
            ("zwrt_router.api", "router_get_dmz"),
            ("zte_nwinfo_api", "nwinfo_set_netselect"),
            ("zwrt_zte_dm", "set_update_mode"),
            ("system", "exec"),
        ] {
            assert!(
                matches!(
                    vendor_call(&json!({"object":o,"method":m,"args":{}})).await,
                    Outcome::Invalid(_)
                ),
                "{o} {m}"
            );
        }
        assert!(matches!(
            vendor_call(&json!({"object":"zwrt_router.api","method":"router_set_dmz","args":"x"}))
                .await,
            Outcome::Invalid(_)
        ));
        for bad in ["1;2) OR 1=1;--", "", ";;", "a;1"] {
            assert!(
                matches!(
                    sms_db_delete(&json!({"ids": bad})).await,
                    Outcome::Invalid(_)
                ),
                "{bad}"
            );
        }
        // FOTA 永远不在表里（硬规则）
        assert!(
            !VENDOR_CALLS
                .iter()
                .any(|(o, m)| o.contains("_dm") || m.contains("update"))
        );
    }

    #[test]
    fn only_listed_options() {
        let fallback = ["wifi0".to_string(), "wifi1".to_string()];
        for ok in [
            "wireless.main_2g.disabled",
            "wireless.guest_5g.key",
            "wireless.wifi1.htmode",
            "wireless.wifi0.country",
            "zte_mbb.wifi.wifi6_switch",
            "zte_mbb.wifi.wifi_onoff",
        ] {
            assert!(wifi_apply_key_ok(ok, &fallback), "{ok}");
        }
        for bad in [
            "wireless.main_2g",
            "wireless.main_2g.macfilter",
            "wireless.wifi2.channel",
            "network.lan.ipaddr",
            "zte_mbb.wifi.fota",
            "wireless.main_2g.ssid.x",
            "wireless.wifi0.disabled;reboot",
            // the real place of the vendor switches: changed through zwrt_wlan, not here
            "wireless.zte_mbb.wifi_onoff",
            "wireless.zte_mbb.wifi6_switch",
            "wireless.zte_mbb.lbd",
        ] {
            assert!(!wifi_apply_key_ok(bad, &fallback), "{bad}");
        }
        // radio sections named by wireless.main_<band>.device are radio sections too
        let named = ["radio0".to_string(), "radio1".to_string()];
        assert!(wifi_apply_key_ok("wireless.radio1.channel", &named));
        assert!(!wifi_apply_key_ok("wireless.radio1.ssid", &named));
        assert!(!wifi_apply_key_ok("wireless.radio1.channel", &fallback));
        assert!(wifi_apply_key_ok("wireless.wifi0.channel", &named));
    }

    #[test]
    fn wifi_module_is_the_stock_call() {
        // stock web: fo("zwrt_wlan","set",{zte_mbb:{wifi_onoff, lbd only when on}})
        assert_eq!(
            wifi_module_args(false, Some("1")),
            json!({"zte_mbb":{"wifi_onoff":"0"}})
        );
        assert_eq!(
            wifi_module_args(true, Some("1")),
            json!({"zte_mbb":{"wifi_onoff":"1","lbd":"1"}})
        );
        assert_eq!(
            wifi_module_args(true, Some("0")),
            json!({"zte_mbb":{"wifi_onoff":"1","lbd":"0"}})
        );
        for unreadable in [None, Some(""), Some("yes")] {
            assert_eq!(
                wifi_module_args(true, unreadable),
                json!({"zte_mbb":{"wifi_onoff":"1"}})
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn doh_off_leaves_proxy_dns_alone() {
        let off = DOH_OFF_SCRIPT;
        assert!(off.contains("rm -f /tmp/dnsmasq.d/doh.conf"));
        // 代理的上游 127.0.0.1 和 noresolv 不能被整条删掉
        assert!(!off.contains("uci delete"));
        assert!(!off.contains("noresolv"));
        assert!(off.contains("del_list dhcp.lan_dns.server='127.0.0.1#5353'"));
        // 只删，不再写转发
        assert!(!off.contains("printf"));
    }
    #[tokio::test]
    async fn doh_on_is_refused_without_running_anything() {
        // 「开」在跑任何命令之前就拒（DoH 代理随 agent 删了）
        match dns_doh(&json!({"enabled": true})).await {
            Outcome::Invalid(e) => assert!(e.contains("removed"), "{e}"),
            o => panic!("enabled=true must be refused: {o:?}"),
        }
        match dns_doh(&json!({})).await {
            Outcome::Invalid(_) => {}
            o => panic!("a missing enabled must be refused: {o:?}"),
        }
    }
    #[tokio::test]
    async fn power_save_saves_applies_and_reads_back() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("datad-psm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("psm"), "old").unwrap();
        // 假 iw：set 记下来，get 回最后一次 set 的值
        let iw = d.join("iw");
        let state = d.join("state");
        std::fs::write(
            &iw,
            format!(
                "#!/bin/sh\ncase \"$3\" in set) echo \"$5\" > {s};; get) echo \"Power save: $(cat {s})\";; esac\n",
                s = state.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&iw, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hp = d.join("hotplug");
        let out = power_save_apply(&hp, iw.to_str().unwrap(), false).await;
        assert!(
            matches!(&out, Outcome::Ok(v) if v["live"] == json!(false)),
            "{out:?}"
        );
        let script = std::fs::read_to_string(hp.join("99-disable-powersave")).unwrap();
        assert!(script.contains("iw dev wlan3 set power_save off"));
        assert!(script.starts_with("#!/bin/sh"));
        assert_eq!(
            std::fs::metadata(hp.join("99-disable-powersave"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        // 读回不对就如实说没改成
        let stuck = d.join("iw-stuck");
        std::fs::write(
            &stuck,
            "#!/bin/sh\n[ \"$3\" = get ] && echo 'Power save: on'\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            power_save_apply(&hp, stuck.to_str().unwrap(), false).await,
            Outcome::Failed(_)
        ));
        // Wi-Fi 关着读不到：存了就算，live 为空
        let none = d.join("iw-none");
        std::fs::write(&none, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&none, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = power_save_apply(&hp, none.to_str().unwrap(), true).await;
        assert!(
            matches!(&out, Outcome::Ok(v) if v["live"].is_null() && v["enabled"] == json!(true)),
            "{out:?}"
        );
        assert!(
            std::fs::read_to_string(hp.join("99-disable-powersave"))
                .unwrap()
                .contains("power_save on")
        );
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn boolean_accepts_json_bool_and_01_keeps_old_error() {
        for (v, want) in [
            (json!(true), true),
            (json!(false), false),
            (json!(1), true),
            (json!(0), false),
        ] {
            assert_eq!(boolean(&json!({"enabled": v}), "enabled"), Ok(want));
        }
        for v in [
            json!(2),
            json!(-1),
            json!(1.0),
            json!(0.5),
            json!("1"),
            json!("true"),
            json!(null),
            json!([]),
            json!({}),
        ] {
            assert_eq!(
                boolean(&json!({"enabled": v}), "enabled"),
                Err("enabled must be boolean".into()),
                "{v}"
            );
        }
        assert_eq!(
            boolean(&json!({}), "enabled"),
            Err("enabled must be boolean".into())
        );
    }
    #[test]
    fn direct_supply_enabled_accepts_bool_01_and_c_strings() {
        for (v, want) in [
            (json!(true), true),
            (json!(false), false),
            (json!(1), true),
            (json!(0), false),
            (json!("1"), true),
            (json!("true"), true),
            (json!("0"), false),
            (json!("false"), false),
        ] {
            assert_eq!(direct_supply_enabled(&json!({"enabled": v})), Ok(want));
        }
        for v in [
            json!(2),
            json!(1.5),
            json!("enable"),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert_eq!(
                direct_supply_enabled(&json!({"enabled": v})),
                Err("enabled must be boolean".into())
            );
        }
        assert!(direct_supply_enabled(&json!({})).is_err());
    }
    #[test]
    fn empty_write_reply_is_ok() {
        use crate::ubus::client::UbusError;
        let no_data = Err(UbusError::NoData {
            object: "zte_nwinfo_api".into(),
            detail: "invalid ubus JSON".into(),
        });
        assert!(matches!(write_outcome(no_data), Outcome::Ok(v) if v == json!({})));
        assert!(matches!(
            write_outcome(Err(UbusError::Io("exited with exit status: 1".into()))),
            Outcome::Failed(_)
        ));
    }
    #[test]
    fn direct_supply_write_reply_empty_ok_errors_fail() {
        use crate::ubus::client::UbusError;
        let no_data = Err(UbusError::NoData {
            object: "zwrt_bsp.charger".into(),
            detail: "invalid ubus JSON".into(),
        });
        assert!(!direct_supply_write_failed(&no_data));
        assert!(!direct_supply_write_failed(&Ok(json!({}))));
        assert!(!direct_supply_write_failed(&Ok(json!({"result": 0}))));
        assert!(direct_supply_write_failed(&Ok(json!({"result": 1}))));
        assert!(direct_supply_write_failed(&Ok(json!({"result": "-1"}))));
        assert!(direct_supply_write_failed(&Ok(json!({"error": "denied"}))));
        assert!(direct_supply_write_failed(&Ok(json!([1]))));
        assert!(direct_supply_write_failed(&Err(UbusError::Io(
            "invalid ubus JSON: x".into()
        ))));
    }
    #[test]
    fn band_validation() {
        for bad in ["1 3", "n78", "1;reboot"] {
            assert!(!bad.bytes().all(|b| b.is_ascii_digit() || b == b','));
        }
    }
}
