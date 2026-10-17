//! The touch UI's view model (manager docs/designs/u60-touch-rewrite.md §3–4): what the C UI's
//! data getters return, as one value. `structs.rs` holds the C structs (generated from include/ until
//! Phase 10 X5 deleted the C UI; maintained by hand since); [`Model`] is tests/render/vm_model.c's object, key for key. The render
//! test writes one per shot (--dump-model); the Rust UI draws from it.
// SPDX-License-Identifier: MIT

mod structs;
pub use structs::*;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub data_ok: i32,
    pub data: devui_data_t,
    pub live_ok: i32,
    pub live: devui_data_t,
    pub backend_alive_wall: i64,
    pub backend_silent: i32,
    pub notice_ok: i32,
    pub notice: data_notice_t,
    /// data_backend_version(): bumps with each committed snapshot
    pub datad_version: u64,
    /// the SMS row a long press armed for deleting (sms_delete_armed), -1 none
    pub sms_armed: i32,

    pub screen_status: i32,
    pub screen_net: Option<net_view_t>,
    pub screen_exec_age: i64,
    pub screen_stuck: i32,
    pub screen_op: Option<String>,
    /// journal.list as the 改动记录 fetch gets it (op.c op_log_fetch)
    pub journal: Option<String>,

    pub chill_core: Option<String>,
    pub chill_mode: Option<String>,
    pub chill_exit_raw: Option<String>,
    pub chill_mode_raw: Option<String>,
    pub chill_group: Option<String>,
    pub chill_node: Option<String>,
    pub chill_traffic: Option<String>,
    pub chill_speed: Option<String>,
    pub chill_chain: Option<String>,
    /// The auto group 🚀 points at: its hand-picked node ("" = none). None = 🚀 is not on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chill_pin: Option<String>,
    pub chill_conns: Option<String>,
    pub chill_online: i32,
    pub chill_conn_split: Option<String>,
    pub chill_groups: Vec<chill_group_info_t>,
    pub chill_nodes: Vec<chill_node_info_t>,
    pub chill_pairs: Vec<chill_traffic_item_t>,
    pub chill_profile_raw: Option<String>,
    pub chill_profile_effective_raw: Option<String>,
    pub chill_thermal_eco: i32,
    pub chill_delay_pending: i32,
    pub chill_home: Option<chill_home_t>,
    pub tailscale: tailscale_status_t,
    pub tailscale_peers: Vec<tailscale_peer_t>,

    pub esim_current: Option<String>,
    pub esim_state: Option<String>,
    pub esim_ready: i32,
    pub esim_profiles: Vec<esim_profile_t>,
    pub esim_switching: i32,
    pub esim_loaded: i32,
    pub esim_enabled_iccid: Option<String>,

    pub st_phase: i32,
    pub st_phase_label: Option<String>,
    pub st_progress_pct: i32,
    pub st_live_mbps: Option<f64>,
    pub st_ping_ms: Option<f64>,
    pub st_jitter_ms: Option<f64>,
    pub st_download_mbps: Option<f64>,
    pub st_upload_mbps: Option<f64>,
    pub st_server: Option<String>,
    pub st_error: Option<String>,
    pub st_running: i32,
    pub st_agent_reachable: i32,
    pub st_start_error: Option<String>,
    pub st_servers: Vec<speedtest_server_t>,
    pub st_selected_index: i32,

    pub agent_health: agent_health_t,
    pub scenario: scenario_status_t,

    pub alerts_unread: i32,
    pub alerts: Vec<alert_item_t>,
    pub alerts_error: Option<String>,
    pub health_checked: i32,
    /// health_count(): the items in `health`, or -1 not read yet, -2 read but nothing came
    pub health_n: i32,
    pub health: Vec<health_item_t>,

    pub netinfo: Option<netinfo_t>,
    pub netinfo_action_error: Option<String>,

    pub diagnose_run: Option<diag_run_t>,
    pub diagnose_idle: i32,
    pub diagnose_agent_err: i32,
    pub diagnose_error: Option<String>,

    pub battery_est: Option<String>,

    /// component versions (luyaz-core GET /v2/versions), in its order; empty: not read yet
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<ver_row_t>,
    /// when the core last looked at the public repo, worded (「3 小时前」, 「检查失败」…)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versions_checked: Option<String>,

    pub bl_panel_lit: i32,
    pub bl_is_on: i32,
    pub bl_is_lit: i32,
    pub bl_get: i32,
    pub bl_max: i32,
    /// flushes pushed to the panel, touch reports seen (the 性能测试 page's counters)
    pub frames: u64,
    pub touch_reports: u64,
    pub boot_s: i64,
    pub now: i64,
}

impl Model {
    pub fn from_json(s: &str) -> serde_json::Result<Model> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model the render test wrote (good scene, Home) reads back field for field: nothing in the
    /// C object is unknown to the Rust types and nothing is lost.
    #[test]
    fn the_render_tests_model_round_trips() {
        // not in the public repo (it carries CHILL's fixture data): skipped there
        let Ok(s) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/good-home.json")) else {
            return;
        };
        let s = s.as_str();
        let m = Model::from_json(s).expect("parse");
        let back: serde_json::Value = serde_json::to_value(&m).unwrap();
        let orig: serde_json::Value = serde_json::from_str(s).unwrap();
        // C prints a double that is whole as "5" (%.17g): compare numbers as numbers
        fn same(a: &serde_json::Value, b: &serde_json::Value, path: &str) {
            use serde_json::Value::*;
            match (a, b) {
                (Number(x), Number(y)) => assert_eq!(x.as_f64(), y.as_f64(), "{path}"),
                (Object(x), Object(y)) => {
                    assert_eq!(x.len(), y.len(), "{path}: keys");
                    for (k, v) in x {
                        same(v, y.get(k).unwrap_or_else(|| panic!("{path}.{k} missing")), &format!("{path}.{k}"));
                    }
                }
                (Array(x), Array(y)) => {
                    assert_eq!(x.len(), y.len(), "{path}: length");
                    for (i, (u, v)) in x.iter().zip(y).enumerate() {
                        same(u, v, &format!("{path}[{i}]"));
                    }
                }
                _ => assert_eq!(a, b, "{path}"),
            }
        }
        same(&back, &orig, "");
        assert_eq!(m.data.operator_name, "中国电信");
        assert!(m.data.valid != 0);
    }
}
