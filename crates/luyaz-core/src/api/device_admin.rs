//! Batch 5c (u60-web.md): the agent's device-admin routes, at its paths with its reply shapes —
//! device power and fast boot, SIM PIN, STC cell lock, upstream DNS, USB mode, the process
//! monitor, the AT terminal, USSD and the SIM toolkit.
//!
//! The stateless reads are on demand, with a timeout, as `sys_reads.rs`; the writes and the AT
//! port's routes only with u60d's writes on. The writes go through u60d's own `/control` as the caller's
//! source, the actions the agent sends core (`zte-agent/src/core_write.rs`): `device.poweroff`,
//! `usb.set`, and `vendor.call` for the rest. The process monitor's kill is a signal, as in the agent.
//!
//! The scheduler's forbidden list and the scenario's allow-list keep refusing factory reset, power
//! off, kill-bloat and the AT terminal now that these routes exist here (`tests_device_admin.rs`).

use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{Request, State},
    response::Response,
    routing::{get, post, put},
};
use serde_json::{Map, Value, json};

use super::{Compat, charge, reply, sys_reads};

pub(super) mod phone;
pub(super) mod procs;

/// What the routes reach besides ubus and `/control` (tests swap these).
#[derive(Clone)]
pub struct Host {
    /// the AT port (`None`: `LUYAZ_AT_LOCK` / `LUYAZ_AT_PORT`, as the core's own commands)
    pub at: Option<crate::at::Target>,
    /// SIGKILL to one pid (kill-bloat)
    pub kill: Arc<dyn Fn(u32) -> bool + Send + Sync>,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            at: None,
            kill: Arc::new(procs::sigkill),
        }
    }
}

/// The reads.
pub(super) fn reads(r: Router<Compat>) -> Router<Compat> {
    r.route("/api/device/fast-boot", get(fast_boot_get))
        // a read in the agent (the body is get_device_info's); its PUT is the scenario's
        .route("/api/device/power-save", post(power_save_read))
        .route("/api/sim/info", get(sim_info))
        .route("/api/sim/lock-trials", get(sim_lock_trials))
        .route("/api/cell/stc/params", get(stc_params))
        .route("/api/cell/stc/status", get(stc_status))
        .route("/api/system/top", get(procs::top))
        .route("/api/router/dns", get(dns_get))
}

/// The writes and the AT port's routes (with u60d's writes on).
pub(super) fn writes() -> Router<Compat> {
    Router::new()
        .route("/api/device/fast-boot", put(fast_boot_set))
        .route("/api/device/poweroff", post(poweroff))
        .route("/api/device/factory-reset", post(factory_reset))
        .route("/api/sim/pin/verify", post(pin_verify))
        .route("/api/sim/pin/change", post(pin_change))
        .route("/api/sim/pin/mode", post(pin_mode))
        .route("/api/sim/unlock", post(sim_unlock))
        .route("/api/cell/stc/params", put(stc_params_set))
        .route("/api/cell/stc/enable", post(stc_enable))
        .route("/api/cell/stc/disable", post(stc_disable))
        .route("/api/cell/stc/reset", post(stc_reset))
        .route("/api/router/dns", put(dns_set))
        .route("/api/usb/mode", put(usb_mode))
        .route("/api/system/kill-bloat", post(procs::kill_bloat))
        .route("/api/at/port", get(phone::at_port))
        .route("/api/at/send", post(phone::at_send))
        .route("/api/ussd/send", post(phone::ussd_send))
        .route("/api/ussd/respond", post(phone::ussd_respond))
        .route("/api/ussd/cancel", post(phone::ussd_cancel))
        .route("/api/stk/menu", get(phone::stk_menu))
        .route("/api/stk/select", post(phone::stk_select))
}

fn ok(data: Value) -> Response {
    reply(200, json!({"ok": true, "data": data}))
}

fn fail(e: impl std::fmt::Display) -> Response {
    reply(503, json!({"ok": false, "error": e.to_string()}))
}

fn bad_json() -> Response {
    reply(400, json!({"ok": false, "error": "invalid JSON"}))
}

// ---- reads ----

async fn read(c: &Compat, object: &'static str, method: &'static str, args: Value) -> Response {
    match sys_reads::ubus(c, object, method, args).await {
        Ok(data) => ok(data),
        Err(e) => fail(e),
    }
}

/// GET /api/device/fast-boot
async fn fast_boot_get(State(c): State<Compat>) -> Response {
    let args = json!({"deviceInfoList": ["quicken_power_on"]});
    match sys_reads::ubus(&c, "zwrt_mc.device.manager", "get_device_info", args).await {
        Ok(data) => ok(json!({"fast_boot": data["quicken_power_on"].as_str().unwrap_or("0")})),
        Err(e) => fail(e),
    }
}

/// POST /api/device/power-save: `get_device_info` with the body as its arguments (the agent's
/// `device_power_save_get`).
async fn power_save_read(State(c): State<Compat>, body: Bytes) -> Response {
    let Ok(args) = serde_json::from_slice::<Value>(&body) else {
        return bad_json();
    };
    read(&c, "zwrt_mc.device.manager", "get_device_info", args).await
}

/// GET /api/sim/info
async fn sim_info(State(c): State<Compat>) -> Response {
    read(&c, "zwrt_zte_mdm.api", "get_sim_info", json!({})).await
}

/// GET /api/sim/lock-trials
async fn sim_lock_trials(State(c): State<Compat>) -> Response {
    read(
        &c,
        "zwrt_zte_mdm.api",
        "get_simlock_available_trials",
        json!({}),
    )
    .await
}

/// GET /api/cell/stc/params
async fn stc_params(State(c): State<Compat>) -> Response {
    read(
        &c,
        "zte_nwinfo_api",
        "nwinfo_get_stc_white_list_par",
        json!({}),
    )
    .await
}

/// GET /api/cell/stc/status
async fn stc_status(State(c): State<Compat>) -> Response {
    read(
        &c,
        "zte_nwinfo_api",
        "nwinfo_get_stc_white_list_status",
        json!({}),
    )
    .await
}

/// The agent's `router_dns_get`: the vendor's keys without their `wan_` prefix; manual DNS the
/// firmware answers empty is filled from uci `network.wan.dns`.
pub(super) fn dns_data(data: &Value, uci_dns: Option<&Value>) -> Value {
    let mut out = Map::new();
    if let Some(obj) = data.as_object() {
        for (k, v) in obj {
            out.insert(k.strip_prefix("wan_").unwrap_or(k).to_string(), v.clone());
        }
    }
    let empty = out
        .get("prefer_dns_manual")
        .and_then(Value::as_str)
        .unwrap_or("")
        .is_empty();
    if out.get("dns_mode").and_then(Value::as_str) == Some("manual") && empty {
        // a list option: ubus answers an array, the CLI a line
        let servers: Vec<String> = match uci_dns {
            Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        for (key, v) in ["prefer_dns_manual", "standby_dns_manual"]
            .iter()
            .zip(servers)
        {
            out.insert((*key).into(), Value::String(v));
        }
    }
    Value::Object(out)
}

/// GET /api/router/dns (upstream DNS; DoH stays the agent's)
async fn dns_get(State(c): State<Compat>) -> Response {
    let data = match sys_reads::ubus(&c, "zwrt_router.api", "router_get_dns_para", json!({})).await
    {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    let manual = data.get("wan_dns_mode").and_then(Value::as_str) == Some("manual");
    let uci = if manual {
        let args = json!({"config": "network", "section": "wan", "option": "dns"});
        sys_reads::ubus(&c, "uci", "get", args)
            .await
            .ok()
            .map(|v| v["value"].clone())
    } else {
        None
    };
    ok(dns_data(&data, uci.as_ref()))
}

// ---- writes ----

/// The caller's source and body.
async fn take(req: Request) -> (&'static str, Option<Bytes>) {
    let source = charge::source(&req);
    (
        source,
        axum::body::to_bytes(req.into_body(), 64 * 1024).await.ok(),
    )
}

/// `vendor.call` as `source`; the agent's `core_write::vendor` answer (200 data / 503 error).
async fn vendor(
    c: &Compat,
    source: &'static str,
    object: &str,
    method: &str,
    args: Value,
) -> Response {
    let params = json!({"object": object, "method": method, "args": args});
    match c
        .writer(source)
        .send("vendor.call", &params)
        .await
        .into_result()
    {
        Ok(data) => ok(data),
        Err(e) => fail(e),
    }
}

/// A route that hands its JSON body to one vendor call as is.
async fn body_to_vendor(
    c: &Compat,
    req: Request,
    object: &str,
    method: &str,
) -> Response {
    let (source, body) = take(req).await;
    let Some(args) = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
        return bad_json();
    };
    vendor(c, source, object, method, args).await
}

/// PUT /api/device/fast-boot `{"fast_boot": "0" | "1"}`
async fn fast_boot_set(State(c): State<Compat>, req: Request) -> Response {
    let (source, body) = take(req).await;
    let Some(parsed) = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
        return bad_json();
    };
    let val = match parsed["fast_boot"].as_str() {
        Some(v @ ("0" | "1")) => v.to_string(),
        _ => {
            return reply(
                400,
                json!({"ok": false, "error": "fast_boot must be \"0\" or \"1\""}),
            );
        }
    };
    let args = json!({"deviceInfoList": {"quicken_power_on": val}});
    let r = vendor(
        &c,
        source,
        "zwrt_mc.device.manager",
        "set_device_info",
        args,
    )
    .await;
    if r.status() != 200 {
        return r;
    }
    ok(json!({"fast_boot": val}))
}

/// POST /api/device/poweroff: `sync`, then `device.poweroff` (zwrt_mc.device.manager
/// device_poweroff). No plain fallback, as in the agent: a power-off that half-happens is worse
/// than one that says it failed.
async fn poweroff(State(c): State<Compat>, req: Request) -> Response {
    let source = charge::source(&req);
    super::sync_disks().await;
    let (status, body) = c
        .writer(source)
        .send("device.poweroff", &json!({}))
        .await
        .into_http();
    reply(status, body)
}

/// POST /api/device/factory-reset: the stock web UI's `device_reset` with resetFlag 0. Never run
/// on the owner's device: it wipes the setup.
async fn factory_reset(State(c): State<Compat>, req: Request) -> Response {
    let source = charge::source(&req);
    let args = json!({"moduleName": "web", "resetFlag": 0});
    vendor(
        &c,
        source,
        "zwrt_mc.device.manager",
        "device_reset",
        args,
    )
    .await
}

/// POST /api/sim/pin/verify
async fn pin_verify(State(c): State<Compat>, req: Request) -> Response {
    body_to_vendor(&c, req, "zwrt_zte_mdm.api", "sim_verify_pin_puk").await
}

/// POST /api/sim/pin/change
async fn pin_change(State(c): State<Compat>, req: Request) -> Response {
    body_to_vendor(&c, req, "zwrt_zte_mdm.api", "sim_change_pin").await
}

/// POST /api/sim/pin/mode
async fn pin_mode(State(c): State<Compat>, req: Request) -> Response {
    body_to_vendor(
        &c,
        req,
        "zwrt_zte_mdm.api",
        "sim_change_pin_mode",
    )
    .await
}

/// POST /api/sim/unlock
async fn sim_unlock(State(c): State<Compat>, req: Request) -> Response {
    body_to_vendor(&c, req, "zwrt_zte_mdm.api", "set_simlock_nck").await
}

/// PUT /api/cell/stc/params
async fn stc_params_set(State(c): State<Compat>, req: Request) -> Response {
    body_to_vendor(
        &c,
        req,
        "zte_nwinfo_api",
        "nwinfo_set_stc_white_list_par",
    )
    .await
}

/// POST /api/cell/stc/enable
async fn stc_enable(State(c): State<Compat>, req: Request) -> Response {
    let source = charge::source(&req);
    vendor(
        &c,
        source,
        "zte_nwinfo_api",
        "nwinfo_stc_cell_lock_enable",
        json!({}),
    )
    .await
}

/// POST /api/cell/stc/disable
async fn stc_disable(State(c): State<Compat>, req: Request) -> Response {
    let source = charge::source(&req);
    vendor(
        &c,
        source,
        "zte_nwinfo_api",
        "nwinfo_stc_cell_lock_disable",
        json!({}),
    )
    .await
}

/// POST /api/cell/stc/reset
async fn stc_reset(State(c): State<Compat>, req: Request) -> Response {
    let source = charge::source(&req);
    vendor(
        &c,
        source,
        "zte_nwinfo_api",
        "nwinfo_stc_cell_lock_reset",
        json!({}),
    )
    .await
}

/// PUT /api/router/dns
async fn dns_set(State(c): State<Compat>, req: Request) -> Response {
    body_to_vendor(&c, req, "zwrt_router.api", "router_set_wan_dns").await
}

/// The agent's `usb_mode_set` body mapping: the web's `{mode}` and the vendor's own key names →
/// `usb.set`'s parameters.
pub(super) fn usb_params(parsed: &Value) -> Result<Value, (u16, Value)> {
    let mut p = Map::new();
    for (from, to) in [
        ("mode", "mode"),
        ("port_switch", "port_switch"),
        ("usb_port_switch", "port_switch"),
        ("network_protocol", "network_protocol"),
        ("usb_network_protocal", "network_protocol"),
    ] {
        match parsed.get(from) {
            None => {}
            Some(Value::String(s)) => {
                p.insert(to.into(), json!(s));
            }
            Some(Value::Number(n)) => {
                p.insert(to.into(), json!(n.to_string()));
            }
            Some(_) => {
                return Err((
                    400,
                    json!({"ok": false, "error": format!("{from} must be a string")}),
                ));
            }
        }
    }
    if p.is_empty() {
        return Err((400, json!({"ok": false, "error": "nothing to set: mode"})));
    }
    Ok(Value::Object(p))
}

/// PUT /api/usb/mode: the common executor checks support immediately before the write.
async fn usb_mode(State(c): State<Compat>, req: Request) -> Response {
    let (source, body) = take(req).await;
    let Some(parsed) = body.and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
        return bad_json();
    };
    let params = match usb_params(&parsed) {
        Ok(p) => p,
        Err((s, v)) => return reply(s, v),
    };
    let (status, body) = c.writer(source).send("usb.set", &params).await.into_http();
    reply(status, body)
}
