// The view model's records (manager docs/designs/u60-touch-rewrite.md §4), kept by hand.
#![allow(non_camel_case_types, clippy::all)]

use serde::{Deserialize, Serialize};

pub type dg_level_t = i32;
pub type dg_state_t = i32;
pub type nv_state_t = i32;
pub type speedtest_phase_t = i32;
pub type ui_net_cause_t = i32;
pub type ui_net_tone_t = i32;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct lan_client_t {
    pub name: String,
    pub ip: String,
    pub mac: String,
    /// lan-client-id: the user's name (shown first), the OUI vendor, a kind token, a random MAC
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alias: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vendor: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct sms_entry_t {
    pub id: i64,
    pub num: String,
    pub date: String,
    pub text: String,
    pub unread: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct core_snapshot_t {
    // whether the core has answered at all
    pub populated: i32,

    // the device
    pub model: String,
    pub firmware: String,
    pub sw_build: String,
    pub imei: String,
    pub uptime_s: i64,
    pub soc_temp: i64,
    pub cpu_pct: i64,
    pub mem_pct: i64,
    pub mem_size: i64,
    pub mem_free: i64,
    pub adb_mode: String,
    pub usb_cc: i32,
    pub powerbank: i32,
    pub usb_power_role: String,
    pub usb_data_role: String,

    // battery and charger (µV / µA)
    pub batt_pct: i32,
    pub batt_temp: i32,
    pub batt_charging: i32,
    pub charger_in: i32,
    pub batt_volt_uv: i64,
    pub batt_cur_ua: i64,
    pub chg_volt_uv: i64,
    pub chg_cur_ua: i64,
    /// heat care (the core's `heat` block): -1 no block, 0 normal, 1 warm, 2 hot, 3 critical
    pub heat_level: i32,
    /// charging paused for the heat (1)
    pub heat_paused: i32,
    /// what CHILL is doing for the heat now: 0 nothing, 1 eco, 2 direct
    pub heat_chill: i32,

    // SIM
    pub sim_state: String,
    pub sim_iccid: String,
    pub sim_imsi: String,
    pub sim_msisdn: String,
    pub sim_spn: String,
    /// `sim.say_short` / `say_short_en` in the screen language (core 1.32.0 on): the shared word
    /// for a SIM identity transition, "" when there is nothing to say
    #[serde(default)]
    pub sim_say: String,
    /// `sim.dual` (global edition, manager docs/designs/dual-sim-global.md); None = one slot
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim_dual: Option<sim_dual_t>,
    /// The core's `regress` block (incidents, core ≥ 1.45.0); None = a core without it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regress: Option<regress_t>,
    /// The core's `display` block (a plugin's card, core ≥ 1.56.0); None = an older core
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<display_t>,

    // the network
    pub carrier: String,
    pub mcc: i32,
    pub mnc: i32,
    pub roaming: String,
    pub radio: String,
    pub signal_level: i32,
    pub rssi: i32,
    pub wan_state: String,
    pub net_pref: String,
    pub operate_mode: String,
    pub rail_mode: i32,
    pub cell_data: i32,
    pub cell_roam: i32,
    pub dps_mode: i32,

    // 5G cell
    pub nr_band_text: String,
    pub nr_pci: i32,
    pub nr_cid: i64,
    pub nr_arfcn: i64,
    pub nr_width: String,
    pub nr_rsrp: i32,
    pub nr_rsrq: i32,
    pub nr_rssi: i32,
    pub nr_snr: String,
    pub nr_ca: String,

    // LTE cell
    pub band_text: String,
    pub lte_pci: i32,
    pub lte_cell_id: i64,
    pub channel: i64,
    pub bandwidth: String,
    pub lte_rsrp: i32,
    pub lte_rsrq: i32,
    pub lte_rssi: i32,
    pub lte_snr: String,
    pub lte_ca: String,
    pub lte_ca_sig: String,

    // band locks: allowed / supported
    pub sa_bands: String,
    pub nsa_bands: String,
    pub lte_bands: String,
    pub sa_sup: String,
    pub nsa_sup: String,
    pub lte_sup: String,

    // bearer QoS
    pub qos_class: i32,
    pub ambr_down: f64,
    pub ambr_up: f64,
    /// qos `bearer`: "eps" → qci is a QCI, "nr_pdu" → a 5QI
    #[serde(default)]
    pub qos_bearer: String,
    /// qos `stale`: the values are history, not this connection's
    #[serde(default)]
    pub qos_stale: i32,

    // traffic: now (B/s), session, day, month
    pub down_bps: i64,
    pub up_bps: i64,
    pub down_total: i64,
    pub up_total: i64,
    pub day_rx_bytes: i64,
    pub day_tx_bytes: i64,
    pub month_rx_bytes: i64,
    pub month_tx_bytes: i64,

    // Wi-Fi and LAN
    pub ap_on: i32,
    pub ap_ssid: String,
    pub ap_psk: String,
    pub ap_auth: String,
    pub nfc_on: i32,
    pub lan_ip: String,
    pub dhcp_first: String,
    pub dhcp_count: String,
    pub dhcp_lease: String,

    // who is connected
    pub lan_total: i32,
    pub lan_wifi: i32,
    pub lan_wired: i32,
    pub lan_n: i32,
    pub lan: Vec<lan_client_t>,

    // messages
    pub sms_new: i32,
    pub inbox_n: i32,
    pub inbox: Vec<sms_entry_t>,
}

/// The regress block as the screen keeps it (design review 4A/14A): what was last seen, whether it
/// is fresh now, when it last was (device wall clock, for 「事故数据停在 hh:mm」).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct regress_t {
    /// the block is fresh now (else the rows are the last seen, grey)
    pub fresh: i32,
    /// data came at least once (else 「读不到事故数据」)
    pub has: i32,
    /// device wall clock when it last came fresh
    pub at: i64,
    pub open_total: i32,
    pub open: Vec<regress_item_t>,
    pub closed: Vec<regress_item_t>,
}

/// A plugin's card on the screen (core `plug::display`): its code, and the short line after it
/// closes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct display_t {
    /// "" = no card up
    pub id: String,
    pub kind: String,
    pub code: String,
    /// seconds left (the core counts)
    pub left_s: i32,
    /// 「一致」 was pressed: waiting for the plugin to finish
    pub answered: i32,
    /// the closed card's line: "done", "cancelled", "" none
    pub said: String,
    pub said_id: String,
}

/// One incident row (names in the screen language).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct regress_item_t {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub sub: String,
    /// device wall clock; 0 = not trusted
    pub from: i64,
    pub to: i64,
    /// "" open, "recovered", "quiet"
    pub end: String,
    pub count: i32,
    pub observed: String,
}

/// Dual SIM as the core reads it: mode "single" | "dual", the main slot (1/2), smart switching,
/// the firmware switching on its own, both slots.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct sim_dual_t {
    pub mode: String,
    pub smart: i32,
    pub active_slot: i32,
    pub switching: i32,
    pub slots: Vec<sim_slot_t>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct sim_slot_t {
    pub slot: i32,
    pub provisioned: i32,
    /// a card is in it (an ICCID is known, on or off)
    pub present: i32,
    /// 1 ready, 0 not, -1 unknown (off: the firmware reports no state)
    pub ready: i32,
    pub operator: String,
    pub mcc: String,
    pub mnc: String,
    pub iccid_tail: String,
    /// operator-logos/<stem> from the core, same pick as the single-card logo (core 1.32.1+, else "")
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub logo: String,
    /// 1 roaming (serving and card country differ), 0 local, -1 unknown (core 1.32.3+, else -1)
    #[serde(default = "minus_one")]
    pub true_roaming: i32,
    /// 「本地」/「漫游到中国移动」/「漫游」 in the data card's words, "" unknown (core 1.32.4+)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub place: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub place_en: String,
    /// "ready" | "not_ready" | "locked" | "absent" | "off" (core 1.14+, else "")
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    /// "data" | "standby" | "off" (core 1.14+, else "")
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// 1 when core sent a `signal` reading for this slot
    #[serde(default, skip_serializing_if = "is_zero")]
    pub has_sig: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig_rat: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sig_rsrp: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sig_rsrq: i32,
    /// one decimal, as core sends it ("18.7"); "" when absent
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig_snr: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sig_bars: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct data_notice_t {
    pub kind: i32,
    pub action: String,
    pub say_zh: String,
    pub say_en: String,
    pub at: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct nv_carrier_t {
    pub kind: i32,
    pub band: i32,
    pub pci: i32,
    pub bw: i32,
    pub active: i32,
    pub arfcn: i64,
    pub rsrp: String,
    pub rsrq: String,
    pub sinr: String,
    pub label: String,
    pub label_short: String,
    pub sinr_tone: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ui_net_story_t {
    pub tone: i32,
    pub cause: i32,
    pub headline: String,
    pub hint: String,
    pub rat: String,
    pub link: String,
    pub sig: String,
    pub sig_tone: i32,
    pub noise: String,
    pub noise_tone: i32,
    pub load: String,
    pub limit: String,
    /// where the home footer's action goes (core 1.50 `story.fix`: sim_enable / cell /
    /// placement / diag:<layer> / op); empty = an older core, the touch decides as before
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fix: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct net_view_t {
    pub ca: Vec<nv_carrier_t>,
    pub ca_n: i32,
    pub act_n: i32,
    pub roam_known: i32,
    pub roam: i32,
    pub story: ui_net_story_t,
    pub nosvc: i32,
    pub sim_usable: i32,
    pub other: i32,
    pub logo: String,
    pub fine: String,
    pub name: String,
    pub r#where: String,
    pub ca_val: String,
    pub ca_sub: String,
    pub bars_tier: i32,
    pub mode_word: String,
    pub mode_auto: i32,
    pub state: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct op_item_t {
    pub have: i32,
    pub op_id: String,
    pub action: String,
    pub item: String,
    pub phase: String,
    pub reason: String,
    pub mark: i32,
    pub stay: i32,
    pub say: String,
    pub next: String,
    pub note: String,
    pub what: String,
    pub source: String,
    pub old_v: String,
    pub target: String,
    pub rollback_to: String,
    pub readback: String,
    pub rollback_to_raw: String,
    pub revert_label: String,
    pub keep_label: String,
    pub can_revert: i32,
    pub can_keep: i32,
    pub remaining_ms: i64,
    pub step: Vec<String>,
    pub step_done: Vec<i32>,
    pub undo_ok: i32,
    pub undo_label: String,
    pub undo_why: String,
    pub acked: i32,
    pub needs_ack: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct op_view_t {
    pub rollback_enabled: i32,
    pub active: op_item_t,
    pub last: op_item_t,
    pub notice: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct chill_group_info_t {
    pub name: String,
    pub selected: i32,
    pub auto_pick: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct chill_node_info_t {
    pub name: String,
    pub delay: i32,
    pub selected: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct chill_traffic_item_t {
    pub name: String,
    pub traffic: String,
    pub bytes: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct chill_home_t {
    pub known: i32,
    pub stale: i32,
    pub configured: i32,
    pub updating: i32,
    pub in_china: i32,
    pub roaming: i32,
    pub blocked: String,
    pub on: Vec<i32>,
    pub sites: i32,
    pub until: i64,
    pub can_extend: i32,
    pub home_ok: i32,
    pub ms: i32,
    pub fallback_to: String,
    pub ts: String,
    pub busy: i32,
    pub busy_item: i32,
    pub busy_on: i32,
    pub busy_phase: String,
    pub busy_until: i64,
    pub nkind: String,
    pub nitem: i32,
    pub ntext: String,
    pub ncode: String,
    pub nat: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct tailscale_status_t {
    pub available: i32,
    pub ok: i32,
    pub state: String,
    pub self_online: i32,
    pub name: String,
    pub ip: String,
    pub relay: String,
    pub routes: String,
    pub exit_node: String,
    pub exit_online: i32,
    pub health: String,
    pub peers: i32,
    pub peers_online: i32,
    pub active: i32,
    pub direct: i32,
    pub ip6: String,
    pub os: String,
    pub version: String,
    pub tailnet: String,
    pub key_expiry: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct tailscale_peer_t {
    pub name: String,
    pub ip: String,
    pub online: i32,
    pub active: i32,
    pub direct: i32,
    pub exit_node: i32,
    pub os: String,
    pub cur_addr: String,
    pub relay: String,
    pub rx: i64,
    pub tx: i64,
    pub hs_ago: i64,
    pub seen_ago: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct esim_profile_t {
    pub name: String,
    pub sub: String,
    pub enabled: i32,
    pub armed: i32,
    pub going: i32,
    pub card_busy: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct speedtest_server_t {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    pub id: i64,
    pub name: String,
    pub sponsor: String,
    pub country: String,
}

/// One row of the speed test's 「分层」 (net-performance.md 第 1 期实现契约 `verdict.layers`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct speed_layer_t {
    /// single / proxy / queue / radio / cell_wifi
    pub key: String,
    /// ok / warn / untested
    pub state: String,
    pub value_zh: String,
    pub value_en: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct speed_verdict_t {
    pub bottlenecks: Vec<String>,
    pub summary_zh: String,
    pub summary_en: String,
    pub layers: Vec<speed_layer_t>,
}

/// The v2 engine's fields of /api/speedtest/progress; absent (None in the model) from an old
/// core, which then looks exactly as before (Design Review DR4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct speedtest_v2_t {
    pub method_version: i32,
    /// full / coarse
    pub mode: String,
    pub roaming_capped: bool,
    /// direct / chill
    pub path: String,
    /// idle_latency / single_download / multi_download / chill_download / multi_upload, "" none
    pub stage: String,
    pub step: i32,
    pub steps: i32,
    pub connections: i32,
    pub sample_short: bool,
    /// ookla / librespeed, "" not known
    pub provider: String,
    pub server_name: String,
    pub server_sponsor: String,
    pub download_bytes: i64,
    pub upload_bytes: i64,
    pub loaded_down_ms: Option<f64>,
    pub verdict: Option<speed_verdict_t>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct agent_health_t {
    pub lost_secs: i64,
    pub unread: i32,
    pub checked: i32,
    pub bad: i32,
    pub warn: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct scenario_status_t {
    pub available: i32,
    pub enabled: i32,
    pub name: String,
    pub wifi_off: i32,
    pub pin: String,
    pub last_switch: i64,
    pub abroad: i32,
    pub chill_on: i32,
    pub chill_back: i32,
    pub auto_direct: i32,
    pub mf_notice: String,
    pub mf_kind: String,
    pub mf_at: i64,
    pub mf_seen: i64,
    pub mf_stale: i32,
    pub home_on: i32,
    pub home_until: i64,
    pub home_fallback: i32,
    /// services.chill.ai_gate: 0 off / AI through 🤖 AI, 1 AI blocked, 2 AI out the other way,
    /// checked to land in ai_region
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ai_gate: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ai_region: String,
    /// GET /api/scenario (logged-in read, core 1.50): 1 = read; the rest is only meaningful then
    #[serde(default, skip_serializing_if = "is_zero")]
    pub detail: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub current: String,
    /// the scenario the scans point at, not yet switched to ("" = none)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub candidate: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hits: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub enter_hits: i32,
    /// the Wi-Fi the last scan matched: its scenario, SSID and since when (wall s)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub matched_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub matched_ssid: String,
    #[serde(default, skip_serializing_if = "is_zero64")]
    pub matched_since: i64,
    /// scans in a row that missed the scenario in force, and how many it takes to leave
    #[serde(default, skip_serializing_if = "is_zero")]
    pub misses: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub exit_misses: i32,
    /// the candidate's display name (from the config; English when there is one)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cand_name: String,
    /// the SSID that last put the device in the scenario in force (kept while scans miss it)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cur_ssid: String,
}

/// The direct exit's latency (core GET /v2/latency/direct): ms of the last good probe, when.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct direct_lat_t {
    pub ms: i32,
    pub at: i64,
}

/// One line of today's timeline (core GET /v2/timeline/today), in the UI's language.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct today_event_t {
    pub t: i64,
    pub kind: String,
    pub text: String,
    /// NET_OK / NET_WARN / NET_BAD / NET_NEUTRAL
    pub tone: i32,
}

/// One hour of today's signal: the average SINR (None = no reading), the RAT, link drops.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct today_hour_t {
    pub hour: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sinr: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rat: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub link_down: i32,
}

/// Today's timeline: clock_trusted 0 = paused (no "today" without a set clock).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct today_t {
    pub clock_trusted: i32,
    pub now: i64,
    /// newest first, at most TODAY_EVENTS
    pub events: Vec<today_event_t>,
    /// how many older events were left out
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more: i32,
    pub hours: Vec<today_hour_t>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct alert_item_t {
    pub seq: i64,
    pub time: i64,
    pub uptime: i64,
    pub label: String,
    pub text: String,
    pub unread: i32,
}

/// The core's last landing (travel-assist.md §5): a new country since `at` (device clock).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct landing_t {
    pub mcc: i32,
    /// the country before (MCC)
    pub from: i32,
    pub at: i64,
    /// bytes since, all cards
    pub rx: i64,
    pub tx: i64,
}

/// One card's use on one device date (core GET /v2/usage/days): bytes up + down.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct usage_card_t {
    /// the core's card key (sha256 of the ICCID, 16 hex): a card's colour on the page
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub card: String,
    /// the ICCID's last 4 digits ("" when unknown)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tail: String,
    /// the operator as the core saw it (CMCC, …)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub op: String,
    pub bytes: i64,
}

/// One device date in the core's usage ledger (YYYY-MM-DD), its cards in the core's order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct usage_day_t {
    pub day: String,
    pub cards: Vec<usage_card_t>,
}

/// One component's version (docs/designs/component-versions.md).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ver_row_t {
    /// core, lweb, sentinel, touch, scripts
    pub name: String,
    /// empty: a build from before 1.0.0
    pub installed: String,
    /// empty: not known
    pub latest: String,
    pub update: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct health_item_t {
    pub bad: i32,
    /// 1 = a read-only notice from the core (`notices`), not a failed check
    #[serde(default, skip_serializing_if = "is_zero")]
    pub note: i32,
    pub id: String,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ni_exit_t {
    pub present: i32,
    pub ip: String,
    pub geo: String,
    pub isp: String,
    pub node: String,
    pub err: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ni_oper_t {
    pub name: String,
    pub country: String,
    pub country_iso: String,
    pub mcc: String,
    pub mnc: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ni_scan_op_t {
    pub plmn: String,
    pub name: String,
    pub country: String,
    pub country_iso: String,
    pub rat: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ni_cell_t {
    pub rat: String,
    /// "B3" (older cores and the C model leave it out)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub band: String,
    pub pci: String,
    pub arfcn: String,
    pub rsrp: String,
    pub rsrq: String,
    pub sinr: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ni_scene_t {
    pub id: String,
    pub name: String,
    pub wifi_off: i32,
    pub abroad: i32,
    pub when: String,
    pub does: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ni_client_t {
    pub name: String,
    pub mac: String,
    pub ip: String,
    pub down: i64,
    pub up: i64,
    pub down_rate: i64,
    pub up_rate: i64,
    pub signal: i32,
    pub signal_tier: String,
    pub band: String,
    pub wifi_gen: i32,
    pub link_down: i32,
    pub link_up: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ni_apn_t {
    pub id: String,
    pub name: String,
    pub apn: String,
    pub pdp: i32,
    pub selected: i32,
    pub in_use: i32,
    pub iot: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct netinfo_t {
    pub err: String,
    pub now: i64,
    pub direct: ni_exit_t,
    pub proxy: ni_exit_t,
    pub chill_running: i32,
    pub home: ni_oper_t,
    pub serving: ni_oper_t,
    pub roaming: i32,
    pub selection: String,
    pub guard_phase: String,
    pub guard_target: String,
    pub guard_reason: String,
    pub guard_reason_code: String,
    pub scan_state: String,
    pub scan_err: String,
    pub nops: i32,
    pub ops: Vec<ni_scan_op_t>,
    pub nbr_state: String,
    pub nbr_err: String,
    pub nbr_at: i64,
    pub ncells: i32,
    pub cells: Vec<ni_cell_t>,
    pub scene_known: i32,
    pub scene_enabled: i32,
    pub scene_takeover: i32,
    pub scene_current: String,
    pub scene_pin: String,
    pub nscenes: i32,
    pub scenes: Vec<ni_scene_t>,
    pub clients_known: i32,
    pub nclients: i32,
    pub clients: Vec<ni_client_t>,
    pub apn_known: i32,
    pub apn_manual: i32,
    pub apn_switching: i32,
    pub apn_switch_err: String,
    pub apn_in_use: ni_apn_t,
    pub napns: i32,
    pub apns: Vec<ni_apn_t>,
    pub ncands: i32,
    pub cands: Vec<ni_apn_t>,
    pub apn_picked: String,
    pub apn_notice: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct dg_layer_t {
    pub id: String,
    pub level: i32,
    pub detail: String,
    pub counted: i32,
}

/// The decision loop's pending proposal (core api/propose.rs view): only a redial is shown.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct dg_proposal_t {
    pub id: String,
    pub digest: String,
    pub tail: String,
}

/// The last verified connection (core `GET /api/verified`): what a restore would set back, and
/// how the one running goes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct dg_restore_t {
    /// a restore may start now (the core's `available`)
    pub avail: bool,
    /// the items that differ, in restore order (`steps[].item`)
    pub items: Vec<String>,
    /// the run: 0 none, 1 running, 2 restored, 3 restored but still down, 4 stopped
    pub state: i32,
    pub done: i32,
    pub total: i32,
    /// the item it stopped at (state 4)
    pub failed: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct diag_run_t {
    pub state: i32,
    pub id: i64,
    pub waiting_for: String,
    pub asked_at: i64,
    pub started_at: i64,
    pub finished_at: i64,
    pub step: i32,
    pub steps: i32,
    pub layer: Vec<dg_layer_t>,
    pub n: i32,
    pub has_main: i32,
    pub main_layer: String,
    pub main_has_level: i32,
    pub main_level: i32,
    pub main_text: String,
    pub main_action: String,
    pub action_to: String,
    pub more: i32,
    pub feedback: i32,
    pub has_speed: i32,
    pub speed: dg_layer_t,
    pub age_s: i64,
}

fn is_zero(v: &i32) -> bool {
    *v == 0
}

fn minus_one() -> i32 {
    -1
}

fn is_zero64(v: &i64) -> bool {
    *v == 0
}
