//! Block contract types (u60-platform.md Phase 2 step 5, T7): the shape of every `/v2`
//! block's `data`, each field marked for sensitivity (`sens.rs`).
//!
//! For now the projection still builds `serde_json::Value`s (moved byte for byte from
//! datad); these types sit at the event log's door: a block goes into the log only
//! through its contract (decoded, redacted for the log). A block that does not decode
//! is written as `{"untyped":true}`, never as its plain value; fields the contract does
//! not name are dropped and counted (`inexact`). `tools/u60d-parity.sh` ran every mock case
//! through `--evlog-stats` and required both counts to be zero (until Phase 10, X2).
//!
//! Shapes come from the parity cases (80 at 68416eb) and the device (10-07). Objects that are an
//! ubus/uci reply passed through (interfaces.cellular, lan_config, traffic.clear_day and
//! .limit, uci_device_info, typec, powerbank) have optional fields that are left out when
//! absent. Levels:
//! personal = SIM/device identifiers, phone numbers, client MAC/name/IP, the device's
//! public addresses, cell ids and PCIs, the SSID; secret = SMS text and the E4
//! operation details (old/new values; typed with the write path in Phase 3).

use crate::sens::{Ctx, Redact, contract};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;

contract! {
    pub struct Battery {
        normal bat_ua: i64,
        normal bat_uv: i64,
        normal charger_connect: i64,
        normal charger_type: i64,
        normal charging: i64,
        normal chg_ua: i64,
        normal chg_uv: i64,
        normal health: i64,
        normal online: i64,
        normal percent: i64,
        normal temp: i64,
        normal time_to_full: i64,
    }
}

contract! {
    pub struct DirectSupply {
        /// null when the vendor reports a mode other than enable/disable
        normal enabled: Option<bool>,
        normal mode: Option<String>,
        normal supported: bool,
    }
}

contract! {
    pub struct Charger {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nested direct_supply: Option<DirectSupply>,
    }
}

contract! {
    pub struct Client {
        personal ip: String,
        personal mac: String,
        personal name: String,
    }
}

contract! {
    pub struct Clients {
        normal lan: i64,
        nested list: Vec<Client>,
        normal total: i64,
        normal wifi: i64,
    }
}

contract! {
    pub struct Dhcp {
        /// `dhcp.lan.ignore` is not "1" (batch 4)
        #[serde(default)]
        normal dhcp_enabled: bool,
        normal ip: String,
        normal leasetime: String,
        normal limit: String,
        /// `network.lan.netmask` (batch 4)
        #[serde(default)]
        normal netmask: String,
        normal start: String,
    }
}

contract! {
    /// `zwrt_data get_wwaniface`, passed through.
    pub struct Cellular {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal cid: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal connect_fail_count: Option<i64>,
        /// "auto" on some firmware, a number on others
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal connect_mode: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal connect_status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal enable: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal ipv4_address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal ipv4_dev_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal ipv4_dns_prefer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal ipv4_dns_standby: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal ipv4_gateway: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal ipv4_netmask: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal ipv6_address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal ipv6_dev_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal ipv6_dns_prefer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal ipv6_dns_standby: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal ipv6_gateway: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal pdp_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal profile_id: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal roam_enable: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal roll_connect_status: Option<String>,
    }
}

contract! {
    pub struct Addr {
        personal address: String,
        normal mask: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal class: Option<String>,
    }
}

contract! {
    pub struct Iface {
        normal device: String,
        normal dns: Vec<String>,
        nested ipv4: Vec<Addr>,
        nested ipv6: Vec<Addr>,
        normal proto: String,
        normal up: bool,
    }
}

contract! {
    pub struct LanInfo {
        normal bridge_name: String,
        normal device: String,
        personal ipv6_default_gateway: String,
        personal ipv6_dhcp_end: String,
        personal ipv6_dhcp_start: String,
        normal ipv6_dns: String,
        personal ipv6_global_addr: String,
        personal ipv6_link_local_addr: String,
        normal ipv6_ra_enable: i64,
        normal ipv6_unique_local_addr: String,
        personal mac: String,
        normal rx_bytes: i64,
        normal rx_dropped: i64,
        normal rx_error: i64,
        normal rx_packets: i64,
        normal tx_bytes: i64,
        normal tx_dropped: i64,
        normal tx_error: i64,
        normal tx_packets: i64,
    }
}

contract! {
    pub struct LanConfig {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nested lan_info: Option<Vec<LanInfo>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal result: Option<String>,
    }
}

contract! {
    pub struct Interfaces {
        nested cellular: Cellular,
        nested lan: Iface,
        nested lan_config: LanConfig,
        nested wan4: Iface,
        nested wan6: Iface,
    }
}

contract! {
    pub struct Connections {
        normal tcp4: u64,
        normal tcp6: u64,
        normal tcp_active: u64,
        normal tcp_other: u64,
        normal udp4: u64,
        normal udp6: u64,
        normal unix: u64,
    }
}

contract! {
    pub struct Freq {
        normal cur: i64,
        normal max: i64,
    }
}

contract! {
    pub struct LinkRate {
        normal interface: String,
        normal rx_bps: Option<u64>,
        normal rx_bytes: u64,
        normal source: String,
        normal tx_bps: Option<u64>,
        normal tx_bytes: u64,
        normal window_ms: u64,
    }
}

contract! {
    pub struct MemoryKb {
        normal available: u64,
        normal buffers: u64,
        normal cached: u64,
        normal free: u64,
        normal swap_free: u64,
        normal swap_total: u64,
        normal total: u64,
    }
}

contract! {
    pub struct Storage {
        normal available: u64,
        normal total: u64,
        normal used: u64,
    }
}

contract! {
    pub struct ThermalZone {
        normal temp_milli: i64,
        normal r#type: String,
    }
}

contract! {
    pub struct Throughput {
        normal rx_bps: u64,
        normal tx_bps: u64,
        normal window_ms: u64,
    }
}

contract! {
    pub struct Runtime {
        nested connections: Connections,
        normal cpu_cores: BTreeMap<String, i64>,
        nested cpu_freq_mhz: BTreeMap<String, Freq>,
        normal cpu_usage_tenths: i64,
        nested link_rates: Vec<LinkRate>,
        nested memory_kb: MemoryKb,
        nested storage: Storage,
        nested thermal_zones: Vec<ThermalZone>,
        nested throughput: Throughput,
    }
}

contract! {
    pub struct System {
        normal cpu_temp: i64,
        normal cpu_usage: i64,
        normal fw: String,
        normal hostname: String,
        personal imei: String,
        normal mem_avail: i64,
        normal mem_total: i64,
        normal mem_used_pct: i64,
        normal model: String,
        normal sw_version: String,
        normal uptime: i64,
    }
}

contract! {
    /// `zwrt_data get_data_clear_day`, passed through.
    pub struct ClearDay {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal cid: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal clearday: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal enable: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal result: Option<String>,
    }
}

contract! {
    /// `zwrt_data get_data_limit`, passed through.
    pub struct Limit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal cid: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal enable: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal overflow: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal ratio: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal r#type: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal value: Option<String>,
    }
}

contract! {
    pub struct Traffic {
        nested clear_day: ClearDay,
        normal day_rx_bytes: i64,
        normal day_tx_bytes: i64,
        nested limit: Limit,
        normal max_rx_speed: i64,
        normal max_tx_speed: i64,
        normal month_rx_bytes: i64,
        normal month_tx_bytes: i64,
        normal rx_bytes: i64,
        normal rx_speed: i64,
        normal session_time: i64,
        normal total_rx_bytes: i64,
        normal total_tx_bytes: i64,
        normal tx_bytes: i64,
        normal tx_speed: i64,
    }
}

contract! {
    pub struct Live {
        nested runtime: Runtime,
        nested system: System,
        nested traffic: Traffic,
    }
}

contract! {
    /// One APN profile (`zwrt_apn_object` list entry). The password never leaves the device:
    /// only whether one is set.
    pub struct ApnProfile {
        normal apn: String,
        /// `pppAuthMode`: 0 none, 1 PAP, 2 CHAP, 3 PAP/CHAP
        normal auth: i64,
        normal enabled: bool,
        normal has_password: bool,
        /// `profileId`: `manuN` (manual) or `autoNNN` (from the SIM's operator table)
        normal id: String,
        normal name: String,
        /// `pdpType`: 1 IPv4, 2 IPv6, 3 IPv4v6
        normal pdp: i64,
        normal roaming_pdp: i64,
        personal username: String,
    }
}

contract! {
    /// APN (3-3): mode, the profile on cid 1, both lists.
    pub struct Apn {
        nested auto: Vec<ApnProfile>,
        /// `profileId` on cid 1 (what the data link uses)
        normal current: String,
        nested manual: Vec<ApnProfile>,
        /// `apn_mode`: 0 auto, 1 manual
        normal mode: i64,
    }
}

contract! {
    pub struct Nfc {
        normal switch: i64,
    }
}

contract! {
    /// E4 operation state. Its old/new values can be anything the write path touches,
    /// so the details are secret until Phase 3 types them.
    pub struct Op {
        secret active: Value,
        secret last: Value,
        normal rollback_enabled: bool,
    }
}

contract! {
    /// The experience library's word on where the device is now (u60-radio.md §7, D12):
    /// per preference bucket how long and how many crashes/drops, crashes per band. MCC-MNC is
    /// `normal`, as `signal`'s; no cell id, no PCI.
    pub struct Advice {
        normal net: String,
        normal pref: String,
        normal verdict: String,
        normal text: String,
        normal text_en: String,
        nested prefs: BTreeMap<String, AdvicePref>,
        normal bands: BTreeMap<String, u32>,
        normal unclear: u32,
        normal backfilled: bool,
    }
}

contract! {
    pub struct AdvicePref {
        normal secs: i64,
        normal crashes: u32,
        normal drops: u32,
    }
}

contract! {
    pub struct Qos {
        normal ambr_dl: String,
        normal ambr_ul: String,
        normal bearer: String,
        normal qci: i64,
        normal stale: bool,
        normal usb_mode: String,
    }
}

contract! {
    pub struct Signal {
        #[serde(rename = "HSR")]
        normal hsr: bool,
        normal band: String,
        normal bars: i64,
        /// null when the IMSI's MCC is not a known country
        normal home_mcc: Option<i64>,
        normal lte_bands: String,
        personal lte_cell_id: i64,
        normal lte_channel: i64,
        personal lte_pci: i64,
        normal lte_rsrp: i64,
        normal lte_rsrq: i64,
        normal lte_rssi: i64,
        normal lte_snr: String,
        /// current cell lock, raw: `pci,earfcn` (order inferred, unverified); unlocked = "" or zeros
        normal lte_cell_lock: String,
        normal lte_supported_bands: String,
        normal lteca: String,
        normal ltecasig: String,
        normal mcc: i64,
        normal mnc: i64,
        normal net_select: String,
        /// how the network is chosen: "auto_select" | "manual_select" (vendor nwinfo); not the
        /// radio preference, which is `net_select`
        normal net_select_mode: String,
        normal nr_band: String,
        normal nr_bw: String,
        /// current cell lock, raw `pci,arfcn,band` (B31); unlocked = "" or "0,0,0"
        normal nr_cell_lock: String,
        personal nr_cell_id: i64,
        normal nr_channel: i64,
        normal nr_nsa_supported_bands: String,
        personal nr_pci: i64,
        normal nr_rsrp: i64,
        normal nr_rsrq: i64,
        normal nr_rssi: i64,
        normal nr_sa_supported_bands: String,
        normal nr_snr: String,
        normal nrca: String,
        normal nsa_bands: String,
        normal operator: String,
        normal roaming: String,
        normal rssi: i64,
        normal sa_bands: String,
        normal serving_mcc: Option<i64>,
        /// null when either country is unknown
        normal true_roaming: Option<bool>,
        normal r#type: String,
        normal wan_status: String,
    }
}

contract! {
    pub struct Sim {
        normal current_slot: i64,
        normal dual_sim: i64,
        personal iccid: String,
        personal imsi: String,
        normal modem_state: String,
        personal msisdn: String,
        normal pin_status: String,
        normal sim1_provision: i64,
        normal sim2_provision: i64,
        normal spn: String,
        normal state: String,
    }
}

contract! {
    pub struct Sms {
        normal count: i64,
        normal max_id: u64,
        normal unread: i64,
    }
}

contract! {
    pub struct SmsItem {
        normal date: String,
        normal id: i64,
        personal num: String,
        secret text: String,
        normal unread: i64,
    }
}

contract! {
    pub struct SmsList {
        nested list: Vec<SmsItem>,
    }
}

contract! {
    /// uci values, each left out when empty.
    pub struct UciDeviceInfo {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal common_model_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal day_rx_bytes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal day_time: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal day_tx_bytes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal device_alias_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal device_manufacturer: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal device_market_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal device_model: Option<String>,
        #[serde(default, rename = "dhcpEnabled", skip_serializing_if = "Option::is_none")]
        normal dhcp_enabled: Option<String>,
        #[serde(default, rename = "dhcpEnd", skip_serializing_if = "Option::is_none")]
        normal dhcp_end: Option<String>,
        #[serde(default, rename = "dhcpLease_hour", skip_serializing_if = "Option::is_none")]
        normal dhcp_lease_hour: Option<String>,
        #[serde(default, rename = "dhcpStart", skip_serializing_if = "Option::is_none")]
        normal dhcp_start: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal hardware_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal hardware_version_ci: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal hostname: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal iccid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal imei: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal imsi: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal integrate_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal lan_ipaddr: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal lan_netmask: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal login_fail_lock_timeout: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal login_fail_num: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal login_timeout: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal mac_address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal mcc: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal mnc: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal modem_main_state: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal modem_msn: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal month_rx_bytes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal month_time: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal month_tx_bytes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal msisdn: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal mss: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal mtu: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal pin_status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_lte_band: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_lte_bands: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal radio_lte_cell_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_lte_channel: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal radio_lte_pci: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_lte_rsrp: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_lte_rsrq: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_lte_snr: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_lteca: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_net_select: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_network_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_nr_band: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_nr_bandwidth: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_nr_channel: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_nr_nsa_bands: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal radio_nr_pci: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_nr_rsrp: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_nr_rsrq: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_nr_sa_bands: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_nr_snr: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_operator: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal radio_signalbar: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        personal serial_number: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal sim_states: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal software_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal timezone: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal total_rx_bytes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal total_time: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal total_tx_bytes: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal wa_inner_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal wan_dns: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal web_language: Option<String>,
    }
}

contract! {
    /// `zwrt_bsp.typec list`, passed through.
    pub struct Typec {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal cc_attch_state: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal data_role: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal power_role: Option<String>,
    }
}

contract! {
    /// `zwrt_bsp.powerbank get {"property":"state"}`, passed through.
    pub struct Powerbank {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        normal state: Option<i64>,
    }
}

contract! {
    /// `router_get_firewall_para`. A switch is null when the firmware does not report it.
    pub struct RouterFirewall {
        normal dmz: Option<bool>,
        personal dmz_ip: String,
        /// `firewall_enable`
        normal enabled: Option<bool>,
        /// low / medium / high; "" on B31
        normal level: String,
        normal nat: Option<bool>,
        normal port_forward: Option<bool>,
        normal remote_web: Option<bool>,
        normal wan_ping: Option<bool>,
    }
}

contract! {
    /// `router_get_upnp`; the numbers are what `router_set_upnp_switch` takes back (null: not reported).
    pub struct RouterUpnp {
        /// `enable_upnp`
        normal enabled: Option<bool>,
        /// `enable_natpmp`
        normal natpmp: Option<i64>,
        normal notify_interval: Option<i64>,
        normal ttl: Option<i64>,
    }
}

contract! {
    /// One `router_get_portforward_rule` entry (non-empty shape not seen on the device yet).
    pub struct PortForwardRule {
        normal enabled: Option<bool>,
        /// "" when the firmware gives none (then it cannot be deleted)
        normal id: String,
        personal lan_ip: String,
        normal lan_port: String,
        normal name: String,
        normal protocol: String,
        normal wan_port: String,
    }
}

contract! {
    /// One `router_get_macipport_filter_rule` entry (non-empty shape not seen on the device yet).
    pub struct FilterRule {
        normal action: String,
        personal dst_ip: String,
        normal dst_port: String,
        normal enabled: Option<bool>,
        normal id: String,
        normal name: String,
        normal protocol: String,
        personal src_ip: String,
        personal src_mac: String,
        normal src_port: String,
    }
}

contract! {
    /// `router_get_alg_para` (B31 reports only SIP ALG; null = not reported).
    pub struct RouterAlg {
        normal ipsec: Option<bool>,
        normal l2tp: Option<bool>,
        normal pptp: Option<bool>,
        normal sip: Option<bool>,
    }
}

contract! {
    /// The vendor 带宽分配 mode (uci `zwrt_smart_mng`), read only.
    pub struct RouterQos {
        /// `smart_qos.mode`: Nomal_mode, Ai_mode, Live_mode, … ("" unread)
        normal mode: String,
        /// `smart_mng.xdpi_support`
        normal xdpi: Option<bool>,
    }
}

contract! {
    /// Router settings (batch 4): one part per vendor read, null when that read failed or its
    /// shape is not recognised; rule lists are [] when the firmware answers `{}`.
    pub struct Router {
        nested alg: Option<RouterAlg>,
        /// `router_get_domainfilter_rule`
        normal blocked_domains: Option<Vec<String>>,
        nested filter_rules: Option<Vec<FilterRule>>,
        nested firewall: Option<RouterFirewall>,
        nested port_forward: Option<Vec<PortForwardRule>>,
        nested qos: Option<RouterQos>,
        nested upnp: Option<RouterUpnp>,
    }
}

contract! {
    /// One main AP and its radio (uci `wireless`). The password never leaves the device.
    pub struct WifiBand {
        /// `main_*.disabled` is not "1"
        normal ap_on: bool,
        /// uci as set ("0"/"auto" = automatic)
        normal channel: String,
        normal encryption: String,
        normal has_password: bool,
        normal hidden: bool,
        normal htmode: String,
        /// `wifiN.disabled` is not "1"
        normal radio_on: bool,
        personal ssid: String,
        /// `txpowerpercent`
        normal txpower: String,
    }
}

contract! {
    /// Guest Wi-Fi (`guest_2g`, the 5 GHz one shares its settings). No password, only whether one is set.
    pub struct WifiGuest {
        /// `guest_active_time` as set
        normal active_time: i64,
        normal encryption: String,
        normal has_password: bool,
        normal hidden: bool,
        /// `isolate` is not "0"
        normal isolate: bool,
        /// seconds left (`zwrt_wlan wlan_get_guest_access_left_time`), read only while guest is on;
        /// null otherwise or unread
        normal left_s: Option<i64>,
        normal on_2g: bool,
        normal on_5g: bool,
        personal ssid: String,
    }
}

contract! {
    /// Wi-Fi settings (batch 4) for the web pages; `wlan` stays as it is for its readers.
    pub struct Wifi {
        nested band_2g: WifiBand,
        nested band_5g: WifiBand,
        normal country: String,
        nested guest: WifiGuest,
        /// master switch: vendor `wifi_onoff` not "0" and not both main APs disabled
        normal on: bool,
        /// `zte_mbb.wifi6_switch` not "0" (missing counts as on, as the stock page does)
        normal wifi6: bool,
    }
}

contract! {
    pub struct Wlan {
        normal enabled: i64,
        normal enc: String,
        personal ssid: String,
    }
}

/// Every block u60d publishes and the contract that covers it. A block missing here
/// is never logged in plain text (`for_log` refuses it).
macro_rules! blocks {
    ($($name:literal => $ty:ty),* $(,)?) => {
        pub const BLOCKS: &[&str] = &[$($name),*];

        fn dispatch(name: &str, v: &Value, c: &Ctx) -> Result<Checked, Refused> {
            match name {
                $($name => via::<$ty>(v, c),)*
                _ => Err(Refused::NoContract),
            }
        }

        /// Why a block does not decode or is not exact. Quotes values: tests and
        /// `--evlog-stats` with LUYAZ_CONTRACT_DEBUG=1 only.
        pub fn shape_error(name: &str, v: &Value) -> Option<String> {
            fn err<T: DeserializeOwned + Serialize>(v: &Value) -> Option<String> {
                match serde_json::from_value::<T>(v.clone()) {
                    Err(e) => Some(e.to_string()),
                    Ok(t) => {
                        let back = serde_json::to_value(&t).unwrap();
                        (back != *v).then(|| format!("not exact: {back}"))
                    }
                }
            }
            if v.is_null() {
                return None;
            }
            match name {
                $($name => err::<$ty>(v),)*
                _ => Some("no contract".into()),
            }
        }

        /// JSON Schema of every block's contract (`contract-schema.json`, written by a test).
        #[cfg(test)]
        pub fn schemas() -> Vec<(&'static str, ::schemars::Schema)> {
            vec![$(($name, ::schemars::schema_for!($ty))),*]
        }

        #[cfg(test)]
        pub fn samples() -> Vec<(&'static str, Value)> {
            vec![$(($name, serde_json::to_value(<$ty as crate::sens::Sample>::sample()).unwrap())),*]
        }
    };
}

blocks! {
    "advice" => Advice,
    "apn" => Apn,
    "battery" => Battery,
    "charger" => Charger,
    "clients" => Clients,
    "dhcp" => Dhcp,
    "interfaces" => Interfaces,
    "live" => Live,
    "nfc" => Nfc,
    "op" => Op,
    "powerbank" => Powerbank,
    "qos" => Qos,
    "router" => Router,
    "signal" => Signal,
    "sim" => Sim,
    "sms" => Sms,
    "sms_list" => SmsList,
    "typec" => Typec,
    "uci_device_info" => UciDeviceInfo,
    "wifi" => Wifi,
    "wlan" => Wlan,
}

/// A contract value as the JSON the snapshot and `/v2` carry (the projection builds the
/// type, so a field it forgets or mistypes does not compile).
pub fn value<T: Serialize>(x: &T) -> Value {
    serde_json::to_value(x).expect("contract types serialize")
}

/// A block value that passed its contract.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    /// redacted for the audience in `Ctx`
    pub value: Value,
    /// the contract reproduces the value exactly (no field dropped, no number changed)
    pub exact: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    NoContract,
    /// does not decode; the error is not kept (serde's message quotes the value)
    Shape,
}

fn via<T: DeserializeOwned + Serialize + Redact>(v: &Value, c: &Ctx) -> Result<Checked, Refused> {
    let t: T = serde_json::from_value(v.clone()).map_err(|_| Refused::Shape)?;
    let exact = serde_json::to_value(&t).ok().as_ref() == Some(v);
    let value = serde_json::to_value(t.redact(c)).map_err(|_| Refused::Shape)?;
    Ok(Checked { value, exact })
}

/// A block's data through its contract, redacted for `c.to`. `null` (a block never
/// read) passes as is.
pub fn redact(name: &str, v: &Value, c: &Ctx) -> Result<Checked, Refused> {
    if v.is_null() {
        if !BLOCKS.contains(&name) {
            return Err(Refused::NoContract);
        }
        return Ok(Checked {
            value: Value::Null,
            exact: true,
        });
    }
    dispatch(name, v, c)
}

#[cfg(test)]
mod tests;
