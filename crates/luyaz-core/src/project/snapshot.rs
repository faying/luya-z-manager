//! 快照投影（纯函数）：采集轮读到的原始回复（[`Inputs`]）→ 旧 `/state` 形状的 `fields` 和 `/v2` 派生块。
//!
//! Phase 2a（manager `docs/designs/u60-platform.md`，D12）从 `state::collect` 拆出来：
//! 读 ubus、uci、sysfs、日志都在 `state.rs`（IO 层），这里只拿读好的值算，不发请求、不读文件、
//! 不看时钟、不碰全局状态（`project` 模块的测试会查）。同样的输入永远得到同样的输出。
//! MU5252 的多模组部分边读边算（按槽位、按接口再调 ubus），这次原样留在 IO 层（`state::mu5252_extras`）。
use crate::contract as c;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// DHCP 租约：MAC（小写）→（IP，主机名；`*` 记成空）。
pub(crate) type Leases = BTreeMap<String, (String, String)>;

/// 充电口和电池的电压、电流（sysfs，µV / µA；读不到为 0），给 `battery` 对象。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Rails {
    pub chg_uv: i64,
    pub chg_ua: i64,
    pub bat_uv: i64,
    pub bat_ua: i64,
}

/// 一轮采集读到的全部原始值。ubus 回复保留 `Result`（读失败也是输入：块据此置 stale）；
/// `*_read` 是「这次真发了请求」（`ubus_ttl_read`：沿用缓存的不算读到）。
pub(crate) struct Inputs {
    pub common: Result<Value, String>,
    pub board: Result<Value, String>,
    pub info: Result<Value, String>,
    pub net: Result<Value, String>,
    pub traffic: Result<Value, String>,
    pub accounting: Result<Value, String>,
    pub limit: Result<Value, String>,
    pub clear_day: Result<Value, String>,
    pub sim: Result<Value, String>,
    pub sim_read: bool,
    pub imei: Result<Value, String>,
    pub lan_clients: Result<Value, String>,
    pub wifi_clients: Result<Value, String>,
    pub router_status: Result<Value, String>,
    pub thermal: Result<Value, String>,
    pub hightemp: Result<Value, String>,
    pub usb: Result<Value, String>,
    /// 电池、充电器块最近一次读到的回复（`Hub::legacy`）。
    pub battery: Result<Value, String>,
    pub charger: Result<Value, String>,
    pub nfc: Result<Value, String>,
    /// `get_apn_mode`、`get_apn_at_cid {cid:1}`、`get_manu_apn_list`、`get_auto_apn_list`。
    pub apn: Result<[Value; 4], String>,
    /// router 块的八个读（`state::router_reads`，顺序见 [`router_block`]），各读各的。
    pub router: [Result<Value, String>; 8],
    /// 访客 Wi-Fi 剩余时间（`zwrt_wlan wlan_get_guest_access_left_time`）；访客没开就没读，为 `None`。
    pub guest_left: Option<Result<Value, String>>,
    pub sms_capacity: Result<Value, String>,
    pub capacity_read: bool,
    pub sms_nv: Result<Value, String>,
    pub nv_read: bool,
    pub sms_sim: Result<Value, String>,
    pub sim_list_read: bool,
    /// 两库第一页解密、合并后的短信列表（`sms::normalize_lists`）；容量没读到时没读，为 `None`。
    pub sms_list: Option<Vec<c::SmsItem>>,
    pub lan_if: Result<Value, String>,
    pub wan4_if: Result<Value, String>,
    pub wan6_if: Result<Value, String>,
    pub lan_config: Result<Value, String>,
    pub cellular: Result<Value, String>,
    /// [`PACKAGES`] 各包的 `uci show`，同样的顺序。
    pub uci_sets: Vec<BTreeMap<String, String>>,
    /// 承载日志解析结果（`qos::logs`），按这一轮的 PLMN、核心网挑一条。
    pub qos_logs: std::sync::Arc<crate::qos::Parsed>,
    pub leases: Leases,
    pub rails: Rails,
    /// sysfs 温区：（CPU 温区平均 ℃，`thermal.zones`），见 `state::thermal_zones`。
    pub cpu_sys: i64,
    pub zones: Value,
    /// `state::runtime` 的采样：CPU 占用（千分比，读不到 -1）和 `runtime` 对象。
    pub cpu_usage_tenths: i64,
    pub runtime: c::Runtime,
}

/// 读进 [`Inputs::uci_sets`] 的 uci 包，顺序固定（`uci_get` 按这个顺序找）。
pub(crate) const PACKAGES: [&str; 13] = [
    "zwrt_zte_mdm",
    "zwrt_common_info",
    "network",
    "dhcp",
    "zwrt_data_commit",
    "system",
    "zwrt_web",
    "zwrt_tr069",
    "zwrt_router",
    "zte_nwinfo",
    "zwrt_zte_nwinfo",
    "wireless",
    "mwan3",
];

/// MU5252 多模组部分（IO 层边读边算）要用的、投影中途算出来的值。
pub(crate) struct Mu5252Ctx {
    pub raw_net: Value,
    pub imsi: String,
    pub msisdn: String,
    pub cpu_temp: i64,
    pub sim: Value,
    pub imei: Value,
    pub cellular: Value,
    pub wan4_if: Value,
    pub wan6_if: Value,
    pub uci_sets: Vec<BTreeMap<String, String>>,
}

pub(crate) struct Projection {
    pub fields: Map<String, Value>,
    /// `/v2` 派生块，按记入 `Hub` 的顺序。
    pub blocks: Vec<(&'static str, Result<Value, String>)>,
    /// 机型模板是 MU5252 时才有：IO 层再读外挂模组，补 `modems`、`thermal.modems`、`aggregation`、`multiwan`。
    pub mu5252: Option<Mu5252Ctx>,
}

pub(crate) fn project(inputs: Inputs, sample_interval_ms: u64) -> Projection {
    let Inputs {
        common,
        board,
        info,
        net,
        traffic,
        accounting,
        limit,
        clear_day,
        sim,
        sim_read,
        imei,
        lan_clients,
        wifi_clients,
        router_status,
        thermal,
        hightemp,
        usb,
        battery,
        charger,
        nfc,
        apn,
        router,
        guest_left,
        sms_capacity,
        capacity_read,
        sms_nv,
        nv_read,
        sms_sim,
        sim_list_read,
        sms_list,
        lan_if,
        wan4_if,
        wan6_if,
        lan_config,
        cellular,
        uci_sets,
        qos_logs,
        leases,
        rails,
        cpu_sys,
        zones,
        cpu_usage_tenths,
        runtime,
    } = inputs;
    // `/v2` 派生块的健康：信号块看 nwinfo，live 块看 system info 和实时流量。
    let signal_ok = net.is_ok();
    let live_ok = info.is_ok() && traffic.is_ok();
    // 没发请求、沿用上一次值的不算读到（`ubus_ttl_read`）：块照 V2-12 置 stale。
    let sim_ok = sim_read;
    // 旧 /state 读者迁到 /v2 用的块（u60-features.md §0.1，V2-46）：各看自己的来源。
    let clients_ok = lan_clients.is_ok() && wifi_clients.is_ok();
    let qos_ok = signal_ok && usb.is_ok();
    let interfaces_ok = wan4_if.is_ok() && cellular.is_ok();
    let common = object(common);
    let board = object(board);
    let info = object(info);
    let mut raw_net = object(net);
    let traffic = object(traffic);
    let accounting = object(accounting);
    let limit = object(limit);
    let clear_day = object(clear_day);
    let sim = object(sim);
    let imei = object(imei);
    let lan_clients = object(lan_clients);
    let wifi_clients = object(wifi_clients);
    let router_status = object(router_status);
    let thermal = object(thermal);
    let usb = object(usb);
    let nfc_ok = nfc.is_ok();
    let nfc = object(nfc);
    let sms_ok = sms_capacity.is_ok();
    // V2-30：短信块。容量和两库第一页都读成功才算读成功（任一失败 → stale）。
    let sms_list_ok = matches!((&sms_nv, &sms_sim), (Ok(_), Ok(_))) && nv_read && sim_list_read;
    let sms_block = match (&sms_capacity, &sms_nv, &sms_sim) {
        (Ok(capacity), Ok(nv), Ok(sim)) if capacity_read && nv_read && sim_list_read => {
            Ok(crate::sms::block_data(capacity, nv, sim))
        }
        _ => Err("zwrt_wms capacity or SMS list read failed".to_string()),
    };
    let sms_capacity = object(sms_capacity);
    let lan_if = object(lan_if);
    let wan4_if = object(wan4_if);
    let wan6_if = object(wan6_if);
    let lan_config = object(lan_config);
    let cellular = object(cellular);
    let model_name = string(&common, "model_name");
    let hardware_version = string(&common, "hardware_version");
    let profile_source = if !model_name.is_empty() {
        "model_name"
    } else {
        "hardware_version"
    };
    let profile = normalize_profile(if !model_name.is_empty() {
        &model_name
    } else {
        &hardware_version
    });
    let template = match profile.as_str() {
        "mu5250" => "MU5250",
        "mu5252" => "MU5252",
        "mc7523" => "MC7523",
        "mc8532b" => "MC8532B",
        _ => "legacy_compat",
    };
    // Only templates the C side marks NETWORK_SOURCE_NWINFO_UBUS_WITH_UCI_FALLBACK
    // may fill network fields from the zte_nwinfo UCI cache. MU5250/MC7523 are
    // ubus-only: a failed nwinfo call must read as "no data", not stale UCI.
    if matches!(template, "MU5252" | "MC8532B") {
        topflow_net_fallback(&mut raw_net, &uci_sets);
    }
    // The *_band_lock fields are the current lock set: after locking to n78
    // they read "78", so they cannot list what the modem supports. The stock
    // web UI takes the choices from zwrt_zte_nwinfo.default_band_lock (the
    // full set a band reset goes back to); use it, and keep the lock field
    // only as a fallback for firmware without that section.
    let supported = |opt: &str, lock: &str| {
        let v = uci_get(
            &uci_sets,
            &format!("zwrt_zte_nwinfo.default_band_lock.{opt}"),
        );
        if v.is_empty() {
            string(&raw_net, lock)
        } else {
            v.to_string()
        }
    };
    let mut imsi = string(&sim, "sim_imsi");
    if !valid_imsi(&imsi) {
        imsi = uci_get(&uci_sets, "zwrt_zte_mdm.sim_info.sim_imsi").into();
        if !valid_imsi(&imsi) {
            imsi.clear();
        }
    }
    let s = |key: &str| string(&raw_net, key);
    let i = |key: &str| integer(&raw_net, key);
    let mcc = i("rmcc");
    let (serving_mcc, home_mcc, true_roaming) = roaming_fields(mcc, &imsi);
    // the `signal` block; the old `/state` `net` is the same object without the three
    // roaming fields, which only `/v2` has
    let signal = c::Signal {
        r#type: s("network_type"),
        roaming: s("simcard_roam"),
        operator: s("network_provider_fullname"),
        band: s("wan_active_band"),
        nr_band: s("nr5g_action_band"),
        nr_snr: s("nr5g_snr"),
        lte_snr: s("lte_snr"),
        nr_bw: s("nr5g_bandwidth"),
        nrca: s("nrca"),
        lteca: s("lteca"),
        ltecasig: s("ltecasig"),
        net_select: s("net_select"),
        net_select_mode: s("net_select_mode"),
        sa_bands: s("nr5g_sa_band_lock"),
        nsa_bands: s("nr5g_nsa_band_lock"),
        lte_bands: s("lte_band"),
        lte_cell_lock: s("lock_lte_cell"),
        nr_cell_lock: s("lock_nr_cell"),
        lte_supported_bands: supported("default_lte_ext_band_lock", "lte_band"),
        nr_sa_supported_bands: supported("default_nr5g_sa_band_lock", "nr5g_sa_band_lock"),
        nr_nsa_supported_bands: supported("default_nr5g_nsa_band_lock", "nr5g_nsa_band_lock"),
        bars: i("signalbar"),
        nr_rsrp: i("nr5g_rsrp"),
        nr_rsrq: i("nr5g_rsrq"),
        nr_rssi: i("nr5g_rssi"),
        lte_rsrp: i("lte_rsrp"),
        lte_rsrq: i("lte_rsrq"),
        lte_rssi: i("lte_rssi"),
        rssi: i("rssi"),
        mcc,
        mnc: i("rmnc"),
        lte_pci: i("lte_pci"),
        lte_cell_id: i("cell_id"),
        lte_channel: i("wan_active_channel"),
        nr_pci: i("nr5g_pci"),
        nr_cell_id: i("nr5g_cell_id"),
        nr_channel: i("nr5g_action_channel"),
        wan_status: string(&router_status, "current_wan_status"),
        hsr: false,
        serving_mcc,
        home_mcc,
        true_roaming,
    };
    let signal_v = c::value(&signal);
    let mut net = signal_v.as_object().cloned().unwrap_or_default();
    for k in ["serving_mcc", "home_mcc", "true_roaming"] {
        net.remove(k);
    }
    let mut tout = Map::new();
    for (to, from) in [
        ("rx_speed", "real_rx_speed"),
        ("tx_speed", "real_tx_speed"),
        ("max_rx_speed", "real_max_rx_speed"),
        ("max_tx_speed", "real_max_tx_speed"),
        ("rx_bytes", "real_rx_bytes"),
        ("tx_bytes", "real_tx_bytes"),
        ("session_time", "real_time"),
    ] {
        tout.insert(to.into(), json!(integer(&traffic, from)));
    }
    for key in [
        "day_rx_bytes",
        "day_tx_bytes",
        "month_rx_bytes",
        "month_tx_bytes",
        "total_rx_bytes",
        "total_tx_bytes",
    ] {
        tout.insert(key.into(), json!(integer(&accounting, key)));
    }
    tout.insert("limit".into(), limit);
    tout.insert("clear_day".into(), clear_day);
    let wifi = wifi_section(template, &uci_sets);
    // MU5250/MU5252 only trust ubus's `cpuss_temp` (matches the C template's
    // TEMP_SOURCE_U60_UBUS_ONLY) — no other-key or sysfs-average fallback,
    // since that silently substitutes a plausible-looking but wrong value.
    let cpu_temp = if matches!(template, "MU5250" | "MU5252") {
        let v = integer_or(&thermal, "cpuss_temp", -1);
        if v < 0 {
            0
        } else if v >= 1000 {
            (v + 500) / 1000
        } else {
            v
        }
    } else {
        ["cpuss_temp", "cpu_temp", "temperature", "temp"]
            .into_iter()
            .map(|k| integer(&thermal, k))
            .find(|v| *v > 0)
            .map(|v| if v >= 1000 { (v + 500) / 1000 } else { v })
            .unwrap_or(cpu_sys)
    };
    let (mt, ma, mp) = memory_fields(&info);
    let release = board.get("release").unwrap_or(&Value::Null);
    let sw = {
        let a = string(&common, "wa_inner_version");
        if a.is_empty() {
            string(&common, "integrate_version")
        } else {
            a
        }
    };
    let mut fields = Map::new();
    fields.insert("net".into(), Value::Object(net));
    let (client_list, wifi_count, lan_count) =
        connected_clients(&lan_clients, &wifi_clients, &leases);
    let clients = c::Clients {
        total: wifi_count + lan_count,
        wifi: wifi_count,
        lan: lan_count,
        list: client_list,
    };
    fields.insert("clients".into(), c::value(&clients));
    power_fields(&mut fields, template, battery, charger, rails);
    // the `sms_list` block, typed; the old `/state` has it only together with the unread count
    let sms_items = sms_list.clone().filter(|_| sms_ok);
    if let (true, Some(list)) = (sms_ok, sms_list) {
        fields.insert("sms".into(),json!({"unread":integer(&sms_capacity,"sms_dev_unread_num")+integer(&sms_capacity,"sms_sim_unread_num"),"list":list}));
    }
    fields.insert("traffic".into(), Value::Object(tout));
    if let Some(s) = wifi {
        let wlan = c::Wlan {
            ssid: uci_get(&uci_sets, &format!("wireless.{s}.ssid")).into(),
            enc: uci_get(&uci_sets, &format!("wireless.{s}.encryption")).into(),
            enabled: i64::from(uci_get(&uci_sets, &format!("wireless.{s}.disabled")) != "1"),
        };
        fields.insert("wlan".into(), c::value(&wlan));
    }
    let apn_ok = apn.is_ok();
    // 第四批 4-0：router 块（全部读都失败才 stale）、wifi 块（没有 wireless 配置才 stale）。
    let router_block = router_block(&router);
    let router_ok = router_block.is_some();
    if let Some(r) = &router_block {
        fields.insert("router".into(), c::value(r));
    }
    let wifi_block = wifi_block(&uci_sets, guest_left.as_ref());
    let wifi_ok = wifi_block.is_some();
    if let Some(w) = &wifi_block {
        fields.insert("wifi".into(), c::value(w));
    }
    if let Ok(a) = &apn {
        fields.insert("apn".into(), c::value(&apn_block(a)));
    }
    if nfc_ok
        && nfc.as_object().is_some_and(|v| {
            v.contains_key("switch") || v.contains_key("ap") || v.contains_key("wifi_ap")
        })
    {
        let nfc = c::Nfc {
            switch: integer(&nfc, "switch"),
        };
        fields.insert("nfc".into(), c::value(&nfc));
    }
    fields.insert(
        "thermal".into(),
        json!({"cpu_celsius":cpu_temp,"zones":zones,"modems":[],"hightemp_limit":hightemp_limit(&hightemp)}),
    );
    fields.insert("interfaces".into(),json!({"lan":interface(&lan_if),"wan4":interface(&wan4_if),"wan6":interface(&wan6_if),"lan_config":lan_config,"cellular":cellular}));
    // uci values, each left out when empty (the old `/state` map, now a type)
    let u = |path: &str| {
        Some(uci_get(&uci_sets, path))
            .filter(|v| !v.is_empty())
            .map(String::from)
    };
    let ui = c::UciDeviceInfo {
        iccid: u("zwrt_zte_mdm.sim_info.sim_iccid"),
        imsi: u("zwrt_zte_mdm.sim_info.sim_imsi"),
        msisdn: u("zwrt_zte_mdm.sim_info.msisdn"),
        mcc: u("zwrt_zte_mdm.sim_info.mdm_mcc"),
        mnc: u("zwrt_zte_mdm.sim_info.mdm_mnc"),
        imei: u("zwrt_zte_mdm.device_info.imei"),
        mac_address: u("zwrt_zte_mdm.device_info.wlan_mac_address"),
        modem_msn: u("zwrt_zte_mdm.device_info.modem_msn"),
        wa_inner_version: u("zwrt_common_info.common_config.wa_inner_version"),
        integrate_version: u("zwrt_common_info.common_config.integrate_version"),
        common_model_name: u("zwrt_common_info.common_config.model_name"),
        device_alias_name: u("zwrt_common_info.common_config.device_alias_name"),
        device_market_name: u("zwrt_common_info.common_config.device_market_name"),
        lan_ipaddr: u("network.lan.ipaddr"),
        lan_netmask: u("network.lan.netmask"),
        wan_dns: u("network.zte_wan.dns"),
        dhcp_enabled: u("dhcp.lan.ignore"),
        dhcp_start: u("dhcp.lan.zte_start"),
        dhcp_end: u("dhcp.lan.zte_end"),
        dhcp_lease_hour: u("dhcp.lan.leasetime"),
        hostname: u("system.@system[0].hostname"),
        timezone: u("system.@system[0].timezone"),
        web_language: u("zwrt_web.setting.web_language"),
        login_timeout: u("zwrt_web.config.login_timeout"),
        device_model: u("zwrt_tr069.DeviceInfo.ModelName"),
        device_manufacturer: u("zwrt_tr069.DeviceInfo.Manufacturer"),
        hardware_version: u("zwrt_tr069.DeviceInfo.HardwareVersion"),
        software_version: u("zwrt_tr069.DeviceInfo.SoftwareVersion"),
        serial_number: u("zwrt_tr069.DeviceInfo.SerialNumber"),
        mtu: u("zwrt_router.network.mtu"),
        mss: u("zwrt_router.network.mss"),
        sim_states: u("zwrt_zte_mdm.sim_info.sim_states"),
        modem_main_state: u("zwrt_zte_mdm.sim_info.modem_main_state"),
        pin_status: u("zwrt_zte_mdm.sim_info.pin_status"),
        hardware_version_ci: u("zwrt_common_info.common_config.hardware_version"),
        login_fail_num: u("zwrt_web.config.login_fail_num"),
        login_fail_lock_timeout: u("zwrt_web.config.login_fail_lock_timeout"),
        day_tx_bytes: u("zwrt_data_commit.wwancid1dst.day_tx_bytes"),
        day_rx_bytes: u("zwrt_data_commit.wwancid1dst.day_rx_bytes"),
        day_time: u("zwrt_data_commit.wwancid1dst.day_time"),
        month_tx_bytes: u("zwrt_data_commit.wwancid1dst.month_tx_bytes"),
        month_rx_bytes: u("zwrt_data_commit.wwancid1dst.month_rx_bytes"),
        month_time: u("zwrt_data_commit.wwancid1dst.month_time"),
        total_tx_bytes: u("zwrt_data_commit.wwancid1dst.total_tx_bytes"),
        total_rx_bytes: u("zwrt_data_commit.wwancid1dst.total_rx_bytes"),
        total_time: u("zwrt_data_commit.wwancid1dst.total_time"),
        radio_network_type: u("zte_nwinfo.sys_info.network_type"),
        radio_signalbar: u("zte_nwinfo.signal_strength.signalbar"),
        radio_operator: u("zte_nwinfo.plmn_info.network_provider_fullname"),
        radio_lte_band: u("zte_nwinfo.wan_active_band.GWLSA_band"),
        radio_nr_band: u("zte_nwinfo.wan_active_band.odu_nrband"),
        radio_lte_rsrp: u("zte_nwinfo.signal_strength.lte_rsrp"),
        radio_lte_rsrq: u("zte_nwinfo.signal_strength.lte_rsrq"),
        radio_lte_snr: u("zte_nwinfo.signal_strength.lte_snr"),
        radio_nr_rsrp: u("zte_nwinfo.signal_strength.nr5g_rsrp"),
        radio_nr_rsrq: u("zte_nwinfo.signal_strength.nr5g_rsrq"),
        radio_nr_snr: u("zte_nwinfo.signal_strength.nr5g_snr"),
        radio_lte_cell_id: u("zte_nwinfo.cell_info.cell_id"),
        radio_lte_pci: u("zte_nwinfo.cell_info.lte_pci"),
        radio_lte_channel: u("zte_nwinfo.cell_info.wan_active_channel"),
        radio_nr_pci: u("zte_nwinfo.cell_info.nr5g_pci"),
        radio_nr_channel: u("zte_nwinfo.cell_info.nr5g_action_channel"),
        radio_nr_bandwidth: u("zte_nwinfo.cell_info.nr5g_bandwidth"),
        radio_lteca: u("zte_nwinfo.sys_info.lteca"),
        radio_net_select: u("zte_nwinfo.sys_info.net_select"),
        radio_nr_sa_bands: u("zte_nwinfo.band_lock.nr5g_sa_band_lock"),
        radio_nr_nsa_bands: u("zte_nwinfo.band_lock.nr5g_nsa_band_lock"),
        radio_lte_bands: u("zte_nwinfo.band_lock.lte_ext_band_lock"),
    };
    fields.insert("uci_device_info".into(), c::value(&ui));
    let qos = crate::qos::select(&qos_logs, qos_query(&raw_net, &imsi));
    let qos = c::Qos {
        qci: qos.qci,
        ambr_dl: qos.ambr_dl,
        ambr_ul: qos.ambr_ul,
        bearer: qos.bearer.into(),
        stale: qos.stale,
        usb_mode: string(&usb, "mode"),
    };
    fields.insert("qos".into(), c::value(&qos));
    let mut msisdn = string(&sim, "msisdn");
    if !valid_msisdn(&msisdn) {
        msisdn = uci_get(&uci_sets, "zwrt_zte_mdm.sim_info.msisdn").into();
        if !valid_msisdn(&msisdn) {
            msisdn.clear();
        }
    }
    let sim_block = c::Sim {
        iccid: string(&sim, "sim_iccid"),
        imsi: imsi.clone(),
        msisdn: msisdn.clone(),
        spn: spn_from_ucs2_hex(&string(&sim, "spn_name_data")),
        state: string(&sim, "sim_states"),
        modem_state: string(&sim, "modem_main_state"),
        pin_status: string(&sim, "pin_status"),
        current_slot: integer(&sim, "current_sim_slot"),
        dual_sim: integer(&sim, "support_dual_sim"),
        sim1_provision: integer(&sim, "sim1_provision_state"),
        sim2_provision: integer(&sim, "sim2_provision_state"),
    };
    fields.insert("sim".into(), c::value(&sim_block));
    let mu5252 = if template == "MU5252" {
        Some(Mu5252Ctx {
            raw_net: raw_net.clone(),
            imsi: imsi.clone(),
            msisdn: msisdn.clone(),
            cpu_temp,
            sim: sim.clone(),
            imei: imei.clone(),
            cellular: cellular.clone(),
            wan4_if: wan4_if.clone(),
            wan6_if: wan6_if.clone(),
            uci_sets: uci_sets.clone(),
        })
    } else {
        fields.insert("modems".into(), json!([]));
        None
    };
    let dhcp = c::Dhcp {
        dhcp_enabled: uci_get(&uci_sets, "dhcp.lan.ignore") != "1",
        netmask: uci_get(&uci_sets, "network.lan.netmask").into(),
        ip: uci_get(&uci_sets, "network.lan.ipaddr").into(),
        start: uci_get(&uci_sets, "dhcp.lan.start").into(),
        limit: uci_get(&uci_sets, "dhcp.lan.limit").into(),
        leasetime: uci_get(&uci_sets, "dhcp.lan.leasetime").into(),
    };
    fields.insert("dhcp".into(), c::value(&dhcp));
    let template_label = if template == "legacy_compat" {
        "Legacy compatibility fallback"
    } else {
        template
    };
    fields.insert("device".into(),json!({"profile":profile,"profile_source":profile_source,"api_template":template,"api_template_label":template_label,"api_template_supported":i64::from(template!="legacy_compat"),"full_ubus":1,"vendor":string(&common,"manufacturer"),"model_name":model_name,"hardware_version":hardware_version,"market_name":string(&common,"device_market_name"),"alias_name":string(&common,"device_alias_name"),"board_name":string(&board,"board_name")}));
    let cpu_usage = if cpu_usage_tenths >= 0 {
        (cpu_usage_tenths + 5) / 10
    } else {
        -1
    };
    let system = c::System {
        uptime: integer(&info, "uptime"),
        cpu_temp,
        cpu_usage,
        mem_used_pct: mp,
        mem_total: mt,
        mem_avail: ma,
        model: string(&board, "model"),
        hostname: string(&board, "hostname"),
        fw: string(release, "description"),
        sw_version: sw,
        imei: string(&imei, "imei"),
    };
    fields.insert("system".into(), c::value(&system));
    fields.insert("sample_interval_ms".into(), json!(sample_interval_ms));
    fields.insert("runtime".into(), c::value(&runtime));
    // `/v2` 的派生块（不多调 ubus）：信号块 = `net`，live 块 = `system`、`runtime`、`traffic`。
    let mut blocks = Vec::new();
    blocks.push((
        "signal",
        if signal_ok {
            Ok(signal_v)
        } else {
            Err("zte_nwinfo_api nwinfo_get_netinfo failed".into())
        },
    ));
    blocks.push((
        "live",
        if live_ok {
            Ok(json!({"system":fields["system"],"runtime":fields["runtime"],"traffic":fields["traffic"]}))
        } else {
            Err("system info or zwrt_data get_wwandst failed".into())
        },
    ));
    blocks.push(("sms", sms_block));
    // V2-47：短信列表 = `{list: 旧 /state 的 sms.list}`（块的 data 必须是对象）；两库第一页都读到才算读到（容量另算，在 sms 块里）。
    blocks.push((
        "sms_list",
        match sms_items {
            Some(list) if sms_list_ok => Ok(c::value(&c::SmsList { list })),
            _ => Err("zwrt_wms SMS list read failed".into()),
        },
    ));
    // sim 块 = 旧 `/state` 的 `sim` 对象（含 iccid）。zte-agent 的换卡监视靠它，
    // 不再自己每 10 秒调 ubus（2026-10-03，apn_pick）。
    blocks.push((
        "sim",
        match fields.get("sim") {
            Some(v) if sim_ok => Ok(v.clone()),
            _ => Err("zwrt_zte_mdm.api get_sim_info failed".into()),
        },
    ));
    // V2-46：data 和旧 /state 的同名对象同形；没生成（wlan、nfc）或来源没读到就 stale。
    let dhcp_ok = !uci_get(&uci_sets, "network.lan.ipaddr").is_empty();
    let device_ok = fields
        .get("uci_device_info")
        .and_then(Value::as_object)
        .is_some_and(|m| !m.is_empty());
    for (name, ok, why) in [
        ("qos", qos_ok, "nwinfo or zwrt_bsp.usb read failed"),
        ("clients", clients_ok, "router access lists read failed"),
        ("wlan", true, "no wireless section"),
        ("nfc", true, "zwrt_nfc read failed or no NFC"),
        ("apn", apn_ok, "zwrt_apn_object read failed"),
        (
            "router",
            router_ok,
            "every zwrt_router.api / zwrt_smart_mng read failed",
        ),
        ("wifi", wifi_ok, "no wireless section"),
        ("dhcp", dhcp_ok, "uci network/dhcp read failed"),
        (
            "interfaces",
            interfaces_ok,
            "zte_wan status or get_wwaniface failed",
        ),
        ("uci_device_info", device_ok, "uci read failed"),
    ] {
        blocks.push((
            name,
            match fields.get(name) {
                Some(v) if ok => Ok(v.clone()),
                _ => Err(why.into()),
            },
        ));
    }
    Projection {
        fields,
        blocks,
        mu5252,
    }
}

/// 电池、充电器在旧 `/state` 里的样子。读失败（或块 stale）时和原来一样：没有 `battery`，
/// `power` 看充电器那份 `{}`。
pub(crate) fn power_fields(
    fields: &mut Map<String, Value>,
    template: &str,
    battery: Result<Value, String>,
    charger: Result<Value, String>,
    rails: Rails,
) {
    let battery_ok = battery.is_ok();
    let battery = object(battery);
    let charger = object(charger);
    let hide_battery = matches!(template, "MC7523" | "MC8532B");
    if !hide_battery
        && battery_ok
        && battery
            .as_object()
            .is_some_and(|v| v.keys().any(|k| k.starts_with("battery_")))
    {
        fields.insert("battery".into(), battery_object(&battery, &charger, rails));
    }
    if let Some(power) = power_object(&charger) {
        fields.insert("power".into(), power);
    }
}

/// 旧 `/state` 的 `battery` 对象（`/v2` 的 battery 块同形）。
pub(crate) fn battery_object(battery: &Value, charger: &Value, rails: Rails) -> Value {
    c::value(&c::Battery {
        percent: integer_or(battery, "battery_capacity", -1),
        temp: integer(battery, "battery_temperature"),
        online: integer(battery, "battery_online"),
        health: integer(battery, "battery_health"),
        time_to_full: integer_or(battery, "battery_time_to_full", -1),
        charging: integer(charger, "charge_status"),
        charger_connect: integer(charger, "charger_connect"),
        charger_type: integer(charger, "charger_type"),
        chg_uv: rails.chg_uv,
        chg_ua: rails.chg_ua,
        bat_uv: rails.bat_uv,
        bat_ua: rails.bat_ua,
    })
}

/// 旧 `/state` 的 `power` 对象；充电器回复里没有 `direct_power_supply_mode` 时旧 `/state` 不输出它。
pub(crate) fn power_object(charger: &Value) -> Option<Value> {
    let mode = charger
        .get("direct_power_supply_mode")
        .and_then(Value::as_str)?;
    let known = matches!(mode, "enable" | "disable");
    Some(c::value(&c::Charger {
        direct_supply: Some(c::DirectSupply {
            supported: true,
            enabled: known.then_some(mode == "enable"),
            mode: known.then(|| mode.to_string()),
        }),
    }))
}

/// `thermal.hightemp_limit`：1 固件在过热限速，0 没有，null 读不到（别的机型没有这个键）。
pub(crate) fn hightemp_limit(v: &Result<Value, String>) -> Value {
    v.as_ref()
        .ok()
        .and_then(|v| v.get("value"))
        .and_then(Value::as_str)
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map_or(Value::Null, |n| json!(i64::from(n != 0)))
}

pub(crate) fn object(v: Result<Value, String>) -> Value {
    v.unwrap_or_else(|_| json!({}))
}

pub(crate) fn string(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(x)) => x.to_string(),
        Some(Value::Bool(x)) => x.to_string(),
        _ => String::new(),
    }
}

pub(crate) fn integer(v: &Value, key: &str) -> i64 {
    integer_or(v, key, 0)
}

/// Like `integer`, but with an explicit default instead of 0 — for fields
/// where 0 is a valid reading and "no data" needs its own sentinel (e.g.
/// battery percent/time_to_full, matching the C implementation's -1).
pub(crate) fn integer_or(v: &Value, key: &str, default: i64) -> i64 {
    match v.get(key) {
        Some(Value::Number(x)) => x.as_i64().unwrap_or(default),
        Some(Value::String(x)) => x.parse().unwrap_or(default),
        Some(Value::Bool(x)) => i64::from(*x),
        _ => default,
    }
}

pub(crate) fn interface(v: &Value) -> Value {
    json!({"up":v.get("up").and_then(Value::as_bool).unwrap_or(false),"proto":string(v,"proto"),"device":string(v,"l3_device"),"ipv4":v.get("ipv4-address").cloned().unwrap_or_else(||json!([])),"ipv6":v.get("ipv6-address").cloned().unwrap_or_else(||json!([])),"dns":v.get("dns-server").cloned().unwrap_or_else(||json!([]))})
}

pub(crate) fn uci_get<'a>(sets: &'a [BTreeMap<String, String>], path: &str) -> &'a str {
    sets.iter()
        .find_map(|s| s.get(path))
        .map(String::as_str)
        .unwrap_or_default()
}

pub(crate) fn normalize_profile(v: &str) -> String {
    let mut out = String::new();
    let mut sep = true;
    for c in v.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            sep = false
        } else if !sep && matches!(c, '-' | '_' | ' ' | '/' | '.') {
            out.push('_');
            sep = true
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "unknown".into()
    } else {
        out
    }
}

/// MCC → 国家。一个国家有几个 MCC 的归到同一个（美国 310–316、印度 404–406、日本 440/441、
/// 英国 234/235）；港 454、澳 455、台 466 和 460 是不同的。901（国际共享）、001/999（测试）
/// 和不像 MCC 的值没有国家，给 None。
pub(crate) fn mcc_country(mcc: i64) -> Option<i64> {
    match mcc {
        310..=316 => Some(310),
        404..=406 => Some(404),
        440 | 441 => Some(440),
        234 | 235 => Some(234),
        901 | 999 => None,
        200..=799 => Some(mcc),
        _ => None,
    }
}

/// `/v2` signal 块专有的三个字段（E3 D2，全项目一处判断「在哪、是不是真漫游」）：
/// `serving_mcc` = 所在网络的 MCC（`net.mcc`，没注册上为 null）；`home_mcc` = 卡的 MCC
/// （IMSI 前三位，读不到为 null）；`true_roaming` = 两者不是同一个国家，任一为 null 就是 null。
/// 原厂 `roaming`（simcard_roam）在国外插当地卡时说「不漫游」，这里不用它。
/// 只进 `/v2`：旧 `/state` 的 `net` 冻结，不加。
/// `serving_mcc`, `home_mcc`, `true_roaming` of the signal block: the serving and the
/// SIM's country, null when unknown.
pub(crate) fn roaming_fields(mcc: i64, imsi: &str) -> (Option<i64>, Option<i64>, Option<bool>) {
    let serving = Some(mcc).filter(|m| mcc_country(*m).is_some());
    let home = crate::project::screen::imsi_plmn(imsi)
        .map(|(c, _)| c)
        .filter(|m| mcc_country(*m).is_some());
    let roaming = match (serving, home) {
        (Some(s), Some(h)) => Some(mcc_country(s) != mcc_country(h)),
        _ => None,
    };
    (serving, home, roaming)
}

/// 原厂的开关值（"1"/"0"、1/0、true/false、"on"/"off"）；没有或认不出为 `None`（不是「关」）。
fn flag(v: &Value, key: &str) -> Option<bool> {
    match v.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().filter(|n| *n == 0 || *n == 1).map(|n| n == 1),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => Some(true),
            "0" | "false" | "off" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// 数字或数字字符串；"" 和其它为 `None`。
fn number(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// 规则表的几种回复：裸数组、`{rule_list:[…]}`、只有一个数组值的对象、`{}`（B31 没有规则时）。
/// 别的形状认不出，为 `None`（不是「没有规则」）。和旧网页 `firewall/normalize.ts` 的 `ruleList` 一样。
fn rule_list(v: &Value) -> Option<Vec<&Value>> {
    fn rows(a: &[Value]) -> Vec<&Value> {
        a.iter().filter(|r| r.is_object()).collect()
    }
    match v {
        Value::Array(a) => Some(rows(a)),
        Value::Object(m) => {
            if let Some(Value::Array(a)) = m.get("rule_list") {
                return Some(rows(a));
            }
            if m.is_empty() {
                return Some(Vec::new());
            }
            let mut arrays = m.values().filter_map(Value::as_array);
            match (arrays.next(), arrays.next()) {
                (Some(a), None) => Some(rows(a)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// 第一个有值的键（字符串或数字），都没有为 ""。
fn first(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| string(v, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

/// router 块：`[防火墙, UPnP, 端口转发, MAC/IP/端口过滤, ALG, 域名过滤, smart_qos.mode, xdpi_support]`。
/// 每一部分只看自己的读：读失败（或形状认不出）为 null，全部失败才没有块。
pub(crate) fn router_block(reads: &[Result<Value, String>; 8]) -> Option<c::Router> {
    let [fw, upnp, pf, filter, alg, domains, qos_mode, xdpi] = reads;
    let ok = |r: &Result<Value, String>| r.as_ref().ok().filter(|v| v.is_object()).cloned();
    let uci = |r: &Result<Value, String>| {
        r.as_ref()
            .ok()
            .and_then(|v| v.get("value"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let firewall = ok(fw).map(|v| c::RouterFirewall {
        enabled: flag(&v, "firewall_enable"),
        level: string(&v, "level"),
        nat: flag(&v, "nat_enable"),
        dmz: flag(&v, "dmz_enable"),
        dmz_ip: string(&v, "dmz_ip"),
        port_forward: flag(&v, "portforward_enable"),
        wan_ping: flag(&v, "wan_ping_enable"),
        remote_web: flag(&v, "remote_web_access_enable"),
    });
    let upnp = ok(upnp).map(|v| c::RouterUpnp {
        enabled: flag(&v, "enable_upnp"),
        natpmp: number(&v, "enable_natpmp"),
        notify_interval: number(&v, "notify_interval"),
        ttl: number(&v, "ttl"),
    });
    let port_forward = pf.as_ref().ok().and_then(rule_list).map(|rows| {
        rows.into_iter()
            .map(|r| c::PortForwardRule {
                id: string(r, "id"),
                name: string(r, "name"),
                protocol: string(r, "protocol"),
                wan_port: string(r, "wan_port"),
                lan_ip: string(r, "lan_ip"),
                lan_port: string(r, "lan_port"),
                enabled: flag(r, "enabled"),
            })
            .collect()
    });
    let filter_rules = filter.as_ref().ok().and_then(rule_list).map(|rows| {
        rows.into_iter()
            .map(|r| c::FilterRule {
                id: string(r, "id"),
                name: string(r, "name"),
                protocol: string(r, "protocol"),
                src_mac: string(r, "src_mac"),
                src_ip: string(r, "src_ip"),
                src_port: string(r, "src_port"),
                dst_ip: first(r, &["dst_ip", "dest_ip"]),
                dst_port: first(r, &["dst_port", "dest_port"]),
                action: string(r, "action"),
                enabled: flag(r, "enabled"),
            })
            .collect()
    });
    let alg = ok(alg).map(|v| c::RouterAlg {
        sip: flag(&v, "alg_sip_enable"),
        l2tp: flag(&v, "l2tp_passthrough"),
        pptp: flag(&v, "pptp_passthrough"),
        ipsec: flag(&v, "ipsec_passthrough"),
    });
    // 旧网页读 `blocked_domains[]`，iOS 读 `rule_list[].domain`；B31 没有规则时回 `{}`。
    let blocked_domains = domains.as_ref().ok().and_then(|v| {
        if let Some(Value::Array(a)) = v.get("blocked_domains") {
            return Some(
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            );
        }
        rule_list(v).map(|rows| {
            rows.into_iter()
                .map(|r| string(r, "domain"))
                .filter(|d| !d.is_empty())
                .collect()
        })
    });
    let (mode, xdpi) = (uci(qos_mode), uci(xdpi));
    let qos = (mode.is_some() || xdpi.is_some()).then(|| c::RouterQos {
        mode: mode.unwrap_or_default(),
        xdpi: xdpi.and_then(|x| flag(&json!({ "x": x }), "x")),
    });
    let r = c::Router {
        alg,
        blocked_domains,
        filter_rules,
        firewall,
        port_forward,
        qos,
        upnp,
    };
    (r != c::Router::default()).then_some(r)
}

/// 这个 `wireless` 接口在不在、开没开（有 SSID，`disabled` 不是 "1"）。
pub(crate) fn wifi_iface_on(sets: &[BTreeMap<String, String>], iface: &str) -> bool {
    !uci_get(sets, &format!("wireless.{iface}.ssid")).is_empty()
        && uci_get(sets, &format!("wireless.{iface}.disabled")) != "1"
}

/// wifi 块：uci `wireless` 里的主网络两个频段、访客网络和总开关；密码只留「有没有」。
/// 没有 wireless 配置（两个主网络都没 SSID、也没有 `zte_mbb`）就没有块。
pub(crate) fn wifi_block(
    sets: &[BTreeMap<String, String>],
    guest_left: Option<&Result<Value, String>>,
) -> Option<c::Wifi> {
    let get = |s: &str, k: &str| uci_get(sets, &format!("wireless.{s}.{k}"));
    if get("main_2g", "ssid").is_empty()
        && get("main_5g", "ssid").is_empty()
        && get("zte_mbb", "wifi_onoff").is_empty()
    {
        return None;
    }
    let band = |ap: &str, radio: &str| c::WifiBand {
        ap_on: get(ap, "disabled") != "1",
        radio_on: get(radio, "disabled") != "1",
        ssid: get(ap, "ssid").into(),
        encryption: get(ap, "encryption").into(),
        has_password: !get(ap, "key").is_empty(),
        hidden: get(ap, "hidden") == "1",
        channel: get(radio, "channel").into(),
        txpower: get(radio, "txpowerpercent").into(),
        htmode: get(radio, "htmode").into(),
    };
    // 和 agent `wifi.rs` 的 master_on_from / wifi6_from 一样
    let on = get("zte_mbb", "wifi_onoff") != "0"
        && !(get("main_2g", "disabled") == "1" && get("main_5g", "disabled") == "1");
    let left_s = guest_left
        .and_then(|r| r.as_ref().ok())
        .and_then(|v| number(v, "guest_left_time"));
    Some(c::Wifi {
        on,
        wifi6: get("zte_mbb", "wifi6_switch") != "0",
        country: get("wifi0", "country").into(),
        band_2g: band("main_2g", "wifi0"),
        band_5g: band("main_5g", "wifi1"),
        guest: c::WifiGuest {
            ssid: get("guest_2g", "ssid").into(),
            encryption: get("guest_2g", "encryption").into(),
            has_password: !get("guest_2g", "key").is_empty(),
            on_2g: wifi_iface_on(sets, "guest_2g"),
            on_5g: wifi_iface_on(sets, "guest_5g"),
            hidden: get("guest_2g", "hidden") == "1",
            isolate: get("guest_2g", "isolate") != "0",
            active_time: get("guest_2g", "guest_active_time").parse().unwrap_or(0),
            left_s,
        },
    })
}

/// APN 块：原厂字段换成短名，密码只留「有没有」。
pub(crate) fn apn_block([mode, at_cid, manual, auto]: &[Value; 4]) -> c::Apn {
    let list = |v: &Value| -> Vec<c::ApnProfile> {
        v.get("apnListArray")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(apn_profile).collect())
            .unwrap_or_default()
    };
    c::Apn {
        mode: integer(mode, "apn_mode"),
        current: string(at_cid, "profileId"),
        manual: list(manual),
        auto: list(auto),
    }
}

fn apn_profile(p: &Value) -> c::ApnProfile {
    c::ApnProfile {
        id: string(p, "profileId"),
        name: string(p, "profilename"),
        apn: string(p, "wanapn"),
        username: string(p, "username"),
        has_password: !string(p, "password").is_empty(),
        auth: integer(p, "pppAuthMode"),
        pdp: integer(p, "pdpType"),
        roaming_pdp: integer(p, "roamingPdpType"),
        enabled: p.get("isEnable").and_then(Value::as_bool).unwrap_or(false),
    }
}

#[cfg(test)]
pub(crate) fn add_roaming_fields(net: &mut Value, imsi: &str) {
    let Some(obj) = net.as_object_mut() else {
        return;
    };
    let serving = obj
        .get("mcc")
        .and_then(Value::as_i64)
        .filter(|m| mcc_country(*m).is_some());
    let home = crate::project::screen::imsi_plmn(imsi)
        .map(|(c, _)| c)
        .filter(|m| mcc_country(*m).is_some());
    let roaming = match (serving, home) {
        (Some(s), Some(h)) => Some(mcc_country(s) != mcc_country(h)),
        _ => None,
    };
    obj.insert("serving_mcc".into(), json!(serving));
    obj.insert("home_mcc".into(), json!(home));
    obj.insert("true_roaming".into(), json!(roaming));
}

/// What `qos` needs to pick the live bearer: the registered PLMN, the SIM's
/// home PLMN (a home-routed APN abroad names it) and LTE/NSA vs SA.
pub(crate) fn qos_query(raw_net: &Value, imsi: &str) -> crate::qos::Query {
    let (mcc, mnc) = (integer(raw_net, "rmcc"), integer(raw_net, "rmnc"));
    crate::qos::Query {
        serving: (mcc > 0).then_some((mcc, mnc)),
        home: crate::project::screen::imsi_plmn(imsi),
        core: crate::project::screen::data_core(&string(raw_net, "network_type")),
    }
}

pub(crate) fn valid_imsi(value: &str) -> bool {
    (5..=20).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

/// SIM service provider name (EF_SPN) as the vendor stores it: UCS-2 big-endian
/// hex, e.g. "0043004D004C0069006E006B" = "CMLink". Anything else → "".
pub(crate) fn spn_from_ucs2_hex(value: &str) -> String {
    let v = value.trim();
    if v.is_empty() || !v.len().is_multiple_of(4) || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return String::new();
    }
    let units: Vec<u16> = (0..v.len())
        .step_by(4)
        .filter_map(|i| u16::from_str_radix(&v[i..i + 4], 16).ok())
        .filter(|&u| u != 0 && u != 0xffff)
        .collect();
    String::from_utf16(&units)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub(crate) fn valid_msisdn(value: &str) -> bool {
    let digits = value.strip_prefix('+').unwrap_or(value);
    (3..=32).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn realtime_traffic(value: &Value) -> Value {
    json!({
        "rx_speed":integer(value,"real_rx_speed"),
        "tx_speed":integer(value,"real_tx_speed"),
        "max_rx_speed":integer(value,"real_max_rx_speed"),
        "max_tx_speed":integer(value,"real_max_tx_speed"),
        "rx_bytes":integer(value,"real_rx_bytes"),
        "tx_bytes":integer(value,"real_tx_bytes"),
        "session_time":integer(value,"real_time")
    })
}

pub(crate) fn prefixed_string(value: &Value, prefix: &str, suffix: &str) -> String {
    string(value, &format!("{prefix}{suffix}"))
}

pub(crate) fn prefixed_integer(value: &Value, prefix: &str, suffix: &str) -> i64 {
    integer(value, &format!("{prefix}{suffix}"))
}

pub(crate) fn topflow_external_net(
    live: &Value,
    sets: &[BTreeMap<String, String>],
    index: usize,
    slot: i64,
) -> Value {
    let prefix = format!("msim_{}_{}_", index + 1, slot);
    let uci_prefix = format!("zte_nwinfo.sys_info.{prefix}");
    let use_uci = live.as_object().is_none_or(|value| value.is_empty());
    let text = |suffix: &str| {
        let live_value = prefixed_string(live, &prefix, suffix);
        if live_value.is_empty() && use_uci {
            uci_get(sets, &format!("{uci_prefix}{suffix}")).to_owned()
        } else {
            live_value
        }
    };
    let number = |suffix: &str| {
        let live_value = prefixed_string(live, &prefix, suffix);
        if live_value.is_empty() && use_uci {
            uci_get(sets, &format!("{uci_prefix}{suffix}"))
                .parse::<i64>()
                .unwrap_or_default()
        } else {
            prefixed_integer(live, &prefix, suffix)
        }
    };
    let bandwidth = text("lte_bandwidth");
    let bandwidth = bandwidth
        .trim_end_matches("MHz")
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| [1.4, 3.0, 5.0, 10.0, 15.0, 20.0].contains(v))
        .map(|v| {
            if v == 1.4 {
                "1.4".into()
            } else {
                format!("{v:.0}")
            }
        });
    let mut out = json!({
        "type":text("network_type"),
        "bars":number("signalbar"),
        "roaming":text("simcard_roam"),
        "operator":text("network_provider"),
        "plmn":text("rplmn_num"),
        "band":text("wan_active_band"),
        "lte_rsrp":number("lte_rsrp"),
        "lte_rsrq":number("lte_rsrq"),
        "lte_rssi":number("lte_rssi"),
        "lte_snr":text("lte_snr"),
        "lte_pci":number("lte_pci"),
        "cell_id":number("cell_id"),
        "channel":number("wan_active_channel"),
        "mode":text("net_select"),
        "operate_mode":text("operate_mode")
    });
    if let Some(bandwidth) = bandwidth {
        out["bandwidth"] = json!(bandwidth);
    }
    out
}

pub(crate) fn parse_bool(value: &Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(|v| v.as_bool().or_else(|| v.as_i64().map(|n| n != 0)))
        .unwrap_or(false)
}

pub(crate) fn split_uci_list(value: &str) -> Vec<String> {
    value
        .split("' '")
        .map(|v| v.trim_matches('\'').to_owned())
        .filter(|v| !v.is_empty())
        .collect()
}

pub(crate) fn topflow_multiwan(
    sets: &[BTreeMap<String, String>],
    mode: &str,
    running: bool,
) -> Value {
    let source = sets.iter().find(|set| set.contains_key("mwan3.globals"));
    let Some(source) = source else {
        return json!({"mode":mode,"active":mode=="MULTIWAN","service_running":running,"sections":[]});
    };
    let mut ids: Vec<_> = source
        .iter()
        .filter_map(|(key, value)| {
            let id = key.strip_prefix("mwan3.")?;
            (!id.contains('.')
                && matches!(
                    value.as_str(),
                    "interface" | "member" | "policy" | "rule" | "globals"
                ))
            .then(|| id.to_owned())
        })
        .collect();
    ids.sort();
    let mut sections = Vec::new();
    for id in ids {
        let kind = source
            .get(&format!("mwan3.{id}"))
            .cloned()
            .unwrap_or_default();
        let mut item =
            serde_json::Map::from_iter([("id".into(), json!(id)), ("type".into(), json!(kind))]);
        let names: &[&str] = match kind.as_str() {
            "interface" => &[
                "enabled",
                "family",
                "track_method",
                "reliability",
                "timeout",
                "interval",
                "down",
                "up",
            ],
            "member" => &["interface", "metric", "weight"],
            "policy" => &["last_resort"],
            "rule" => &[
                "family",
                "proto",
                "src_ip",
                "src_port",
                "dest_ip",
                "dest_port",
                "use_policy",
                "sticky",
                "logging",
            ],
            "globals" => &["mmx_mask"],
            _ => &[],
        };
        for name in names {
            if let Some(value) = source.get(&format!("mwan3.{id}.{name}")) {
                item.insert((*name).into(), json!(value));
            }
        }
        for (name, key) in [("track_ip", "track_ip"), ("use_member", "use_member")] {
            if let Some(value) = source.get(&format!("mwan3.{id}.{key}")) {
                item.insert(name.into(), json!(split_uci_list(value)));
            }
        }
        sections.push(Value::Object(item));
    }
    json!({"mode":mode,"active":mode=="MULTIWAN","service_running":running,"sections":sections})
}

/// Which `wireless.*` section feeds the `wlan` block. MU5250 always reads
/// `main_2g` (C: WIFI_SOURCE_U60_MAIN_2G), even while it is disabled, and shows
/// the block when any of ssid/key/encryption is set. Every other template picks
/// the first enabled section with an SSID, else the first with an SSID.
pub(crate) fn wifi_section(
    template: &str,
    sets: &[BTreeMap<String, String>],
) -> Option<&'static str> {
    let get = |s: &str, k: &str| uci_get(sets, &format!("wireless.{s}.{k}"));
    if template == "MU5250" {
        return ["ssid", "key", "encryption"]
            .into_iter()
            .any(|k| !get("main_2g", k).is_empty())
            .then_some("main_2g");
    }
    ["main_2g", "main_5g"]
        .into_iter()
        .find(|s| !get(s, "ssid").is_empty() && get(s, "disabled") != "1")
        .or_else(|| {
            ["main_2g", "main_5g"]
                .into_iter()
                .find(|s| !get(s, "ssid").is_empty())
        })
}

pub(crate) fn topflow_net_fallback(raw: &mut Value, sets: &[BTreeMap<String, String>]) {
    if raw.get("network_type").is_some_and(|v| !v.is_null()) {
        return;
    }
    let mut out = Map::new();
    for (key, path) in [
        ("network_type", "zte_nwinfo.sys_info.network_type"),
        ("signalbar", "zte_nwinfo.signal_strength.signalbar"),
        ("simcard_roam", "zte_nwinfo.sys_info.simcard_roam"),
        (
            "network_provider_fullname",
            "zte_nwinfo.plmn_info.network_provider_fullname",
        ),
        ("wan_active_band", "zte_nwinfo.wan_active_band.GWLSA_band"),
        ("nr5g_action_band", "zte_nwinfo.wan_active_band.odu_nrband"),
        ("nr5g_rsrp", "zte_nwinfo.signal_strength.nr5g_rsrp"),
        ("nr5g_rsrq", "zte_nwinfo.signal_strength.nr5g_rsrq"),
        ("nr5g_snr", "zte_nwinfo.signal_strength.nr5g_snr"),
        ("rmcc", "zte_nwinfo.plmn_info.rmcc"),
        ("rmnc", "zte_nwinfo.plmn_info.rmnc"),
        ("cell_id", "zte_nwinfo.cell_info.cell_id"),
        ("lte_pci", "zte_nwinfo.cell_info.lte_pci"),
        (
            "wan_active_channel",
            "zte_nwinfo.cell_info.wan_active_channel",
        ),
        ("nr5g_pci", "zte_nwinfo.cell_info.nr5g_pci"),
        ("nr5g_cell_id", "zte_nwinfo.cell_info.nr5g_cellid"),
        (
            "nr5g_action_channel",
            "zte_nwinfo.cell_info.nr5g_action_channel",
        ),
        ("nr5g_bandwidth", "zte_nwinfo.cell_info.nr5g_bandwidth"),
        ("lte_bandwidth", "zte_nwinfo.cell_info.lte_bandwidth"),
        ("net_select", "zte_nwinfo.sys_info.net_select"),
        (
            "nr5g_sa_band_lock",
            "zte_nwinfo.band_lock.nr5g_sa_band_lock",
        ),
        (
            "nr5g_nsa_band_lock",
            "zte_nwinfo.band_lock.nr5g_nsa_band_lock",
        ),
        ("lte_band", "zte_nwinfo.band_lock.lte_ext_band_lock"),
    ] {
        let value = uci_get(sets, path);
        if !value.is_empty() {
            out.insert(key.into(), json!(value));
        }
    }
    *raw = Value::Object(out);
}

pub(crate) fn client_array(value: &Value, key: &str, leases: &Leases) -> Option<Vec<c::Client>> {
    value.get(key)?.as_array().map(|items| {
        items
            .iter()
            .filter_map(|entry| {
                let mac = ["mac_address", "mac", "mac_addr"]
                    .into_iter()
                    .map(|key| string(entry, key))
                    .find(|value| !value.is_empty())?
                    .to_ascii_lowercase();
                let lease = leases.get(&mac);
                let ip = ["ip_address", "ip", "ip_addr"]
                    .into_iter()
                    .map(|key| string(entry, key))
                    .find(|value| !value.is_empty())
                    .or_else(|| lease.map(|value| value.0.clone()))
                    .unwrap_or_default();
                let name = ["hostname", "name"]
                    .into_iter()
                    .map(|key| string(entry, key))
                    .find(|value| !value.is_empty() && value != "--" && value != "*")
                    .or_else(|| lease.map(|value| value.1.clone()))
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| mac.clone());
                Some(c::Client { name, ip, mac })
            })
            .collect()
    })
}

pub(crate) fn connected_clients(
    lan: &Value,
    wifi: &Value,
    leases: &Leases,
) -> (Vec<c::Client>, i64, i64) {
    let wifi = client_array(wifi, "wireless_access_list_info", leases).unwrap_or_default();
    let lan = client_array(lan, "lan_access_list_info", leases).unwrap_or_default();
    let wifi_count = wifi.len() as i64;
    let lan_count = lan.len() as i64;
    (wifi.into_iter().chain(lan).collect(), wifi_count, lan_count)
}

pub(crate) fn memory_fields(info: &Value) -> (i64, i64, i64) {
    let m = info.get("memory").unwrap_or(&Value::Null);
    let t = integer(m, "total");
    let a = integer(m, "available");
    (t, a, if t > 0 { (t - a) * 100 / t } else { -1 })
}

#[cfg(test)]
mod batch4_tests {
    use super::*;

    /// B31 真机形状（10-13，只读；地址换成假的）。
    #[test]
    fn router_block_from_device_shapes() {
        let fw = json!({"firewall_enable":"1","dmz_enable":"0","dmz_ip":"","dmz_hostname":"",
            "portforward_enable":"0","macipport_filter_enable":"0","macipport_filter_policy":"ACCEPT",
            "portmapping_enable":"0","porttrigger_enable":"","remote_web_access_enable":"0",
            "wan_ping_enable":"0","nat_enable":"1","level":"","domain_filter_policy":""});
        let upnp = json!({"enabled":"0","enable_upnp":"0","notify_interval":"60","ttl":"","enable_natpmp":"0"});
        let reads = [
            Ok(fw),
            Ok(upnp),
            Ok(json!({})),
            Ok(json!({})),
            Ok(json!({"alg_sip_enable":"1"})),
            Ok(json!({})),
            Ok(json!({"value":"Nomal_mode"})),
            Ok(json!({"value":"0"})),
        ];
        let v = c::value(&router_block(&reads).unwrap());
        assert_eq!(v["firewall"]["enabled"], true);
        assert_eq!(v["firewall"]["nat"], true);
        assert_eq!(v["firewall"]["dmz"], false);
        assert_eq!(v["firewall"]["level"], "");
        assert_eq!(
            v["upnp"],
            json!({"enabled":false,"natpmp":0,"notify_interval":60,"ttl":null})
        );
        assert_eq!(v["port_forward"], json!([]));
        assert_eq!(v["filter_rules"], json!([]));
        assert_eq!(v["blocked_domains"], json!([]));
        assert_eq!(
            v["alg"],
            json!({"sip":true,"l2tp":null,"pptp":null,"ipsec":null})
        );
        assert_eq!(v["qos"], json!({"mode":"Nomal_mode","xdpi":false}));
        assert!(crate::contract::shape_error("router", &v).is_none());

        // 一个读失败只空那一部分；形状认不出不算「没有规则」；全部失败才没有块
        let mut part = reads.clone();
        part[0] = Err("timeout".into());
        part[2] = Ok(json!({"a":[],"b":[]}));
        part[3] = Ok(
            json!({"rule_list":[{"id":"3","name":"cam","src_mac":"02:00:00:00:00:01",
            "dest_ip":"192.0.2.9","dest_port":"80","enabled":"1"}]}),
        );
        part[5] = Ok(json!({"rule_list":[{"domain":"example.com"}]}));
        let v = c::value(&router_block(&part).unwrap());
        assert_eq!(v["firewall"], Value::Null);
        assert_eq!(v["port_forward"], Value::Null);
        assert_eq!(v["filter_rules"][0]["dst_ip"], "192.0.2.9");
        assert_eq!(v["filter_rules"][0]["enabled"], true);
        assert_eq!(v["blocked_domains"], json!(["example.com"]));
        assert_eq!(
            router_block(&[0, 1, 2, 3, 4, 5, 6, 7].map(|_| Err("x".into()))),
            None
        );
    }

    fn wireless(pairs: &[(&str, &str)]) -> Vec<BTreeMap<String, String>> {
        vec![
            pairs
                .iter()
                .map(|(k, v)| (format!("wireless.{k}"), v.to_string()))
                .collect(),
        ]
    }

    /// B31 `uci show wireless` 的形状（10-13；SSID、MAC 换成假的，密码是假的）。
    #[test]
    fn wifi_block_keeps_passwords_on_the_device() {
        let sets = wireless(&[
            ("wifi0.htmode", "EHT40"),
            ("wifi0.channel", "0"),
            ("wifi0.disabled", "0"),
            ("wifi0.country", "US"),
            ("wifi0.txpowerpercent", "100"),
            ("wifi1.htmode", "EHT160"),
            ("wifi1.channel", "0"),
            ("wifi1.disabled", "0"),
            ("wifi1.txpowerpercent", "100"),
            ("main_2g.ssid", "Fake-Net"),
            ("main_2g.encryption", "psk2+ccmp"),
            ("main_2g.key", "fake-secret-1"),
            ("main_2g.key_factory", "FAKEFACT01"),
            ("main_2g.disabled", "1"),
            ("main_2g.hidden", "0"),
            ("main_5g.ssid", "Fake-Net"),
            ("main_5g.encryption", "psk2+ccmp"),
            ("main_5g.key", "fake-secret-1"),
            ("main_5g.disabled", "1"),
            ("main_5g.hidden", "0"),
            ("guest_2g.ssid", "Fake_Guest"),
            ("guest_2g.encryption", "none"),
            ("guest_2g.hidden", "0"),
            ("guest_2g.isolate", "0"),
            ("guest_2g.disabled", "1"),
            ("guest_2g.guest_active_time", "240"),
            ("guest_2g.key_factory", "FAKEFACT02"),
            ("guest_5g.ssid", "Fake_Guest"),
            ("guest_5g.disabled", "1"),
            ("zte_mbb.wifi_onoff", "1"),
        ]);
        let v = c::value(&wifi_block(&sets, None).unwrap());
        // 厂商开关开着、两个主网络都关了 = 总开关关；没有 wifi6_switch 算开
        assert_eq!(v["on"], false);
        assert_eq!(v["wifi6"], true);
        assert_eq!(v["country"], "US");
        assert_eq!(v["band_5g"]["htmode"], "EHT160");
        assert_eq!(v["band_2g"]["has_password"], true);
        assert_eq!(v["band_2g"]["ap_on"], false);
        assert_eq!(v["band_2g"]["radio_on"], true);
        assert_eq!(v["guest"]["has_password"], false);
        assert_eq!(v["guest"]["on_2g"], false);
        assert_eq!(v["guest"]["isolate"], false);
        assert_eq!(v["guest"]["active_time"], 240);
        assert_eq!(v["guest"]["left_s"], Value::Null);
        let text = v.to_string();
        assert!(!text.contains("fake-secret") && !text.contains("FAKEFACT"));
        assert!(crate::contract::shape_error("wifi", &v).is_none());

        let mut on = sets.clone();
        on[0].insert("wireless.guest_2g.disabled".into(), "0".into());
        assert!(wifi_iface_on(&on, "guest_2g"));
        let left = Ok(json!({"guest_left_time":"3599"}));
        let v = c::value(&wifi_block(&on, Some(&left)).unwrap());
        assert_eq!(v["guest"]["on_2g"], true);
        assert_eq!(v["guest"]["left_s"], 3599);
        assert!(wifi_block(&wireless(&[("wifi0.country", "US")]), None).is_none());
    }
}
