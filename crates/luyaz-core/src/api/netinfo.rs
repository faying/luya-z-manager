//! GET `/api/netinfo[?lite=1[&apn=1][&clients=1]]` for the touch screen (Phase 4 step 8c),
//! assembled by u60-api's `netinfo::page` as the agent assembles it.
//!
//! Where each piece comes from while the agent still runs everything (the shadow period):
//! - the exits (`direct`, and `proxy` while the proxy runs): the agent's lookups, from the file it
//!   writes after every refresh pass (`/tmp/netinfo-exits.json`); u60d never asks the outside
//!   services itself until step 9
//! - operators, roaming, network type, data: `nwinfo_get_netinfo`, the SIM (u60d's own `sim`
//!   block, one `get_sim_info` read when it is not fresh) and `get_wwaniface`, read in a refresh
//!   pass at most every 10 s and only because someone asked (the agent's `local_snapshot`)
//! - selection: only what `nwinfo_get_netinfo` reports (`net_select_mode`); no `AT+COPS?` here,
//!   the AT port is the agent's and the write layer's
//! - clients: `iw` station dumps and the lease file, only while a page showing them was asked for in
//!   the last minute
//! - scenes: the engine's files, rendered by the shared picker
//! - apn (full read or `apn=1`): the four `zwrt_apn_object` reads and the agent's pick file
//! - guard, scan, `apn.switching`/`switch_error`: u60d's own writes (super::ops, step 8d), idle
//!   while the writes are off; while the agent registers or searches, the two replies differ there
//! - neighbors: the fixed `unsupported` block (the vendor scan drops data)

use axum::{
    extract::{RawQuery, State},
    response::Response,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use luyaz_api::netinfo as ni;
use luyaz_policy::scenario as sc;

use super::{Compat, reply};

/// The agent's PASS_MIN_GAP.
const PASS_MIN_GAP: i64 = 10;
/// A page showing per-client traffic counts as open this long after its last read.
const CLIENTS_WANTED: i64 = 60;

fn now() -> i64 {
    super::home::now_unix() as i64
}

#[derive(Default)]
struct Cached {
    local: Value,
    selection: Option<&'static str>,
    selection_at: i64,
    clients: Vec<Value>,
    clients_prev: ni::Counters,
    clients_at: i64,
}

/// What the refresh pass leaves for the requests, and the writes in progress (the agent's `Inner`).
#[derive(Default)]
pub struct NetCache {
    cached: Mutex<Cached>,
    refreshing: AtomicBool,
    last_pass: AtomicI64,
    last_full: AtomicI64,
    last_clients: AtomicI64,
    /// search, register and its guard, back to automatic (super::ops; idle with writes off)
    pub(super) ops: Mutex<ni::Ops>,
    /// the APN switch running in the background, and why the last one failed
    pub(super) apn_switching: AtomicBool,
    apn_err: Mutex<String>,
    /// one redial at a time
    pub(super) dial: tokio::sync::Mutex<()>,
}

impl NetCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Cached> {
        self.cached.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A write settled the selection mode: show it, and let the next read refresh at once.
    pub(super) fn selected(&self, mode: &'static str, at: i64) {
        let mut g = self.lock();
        g.selection = Some(mode);
        g.selection_at = at;
        drop(g);
        self.last_pass.store(0, Ordering::SeqCst);
    }

    pub(super) fn apn_error(&self, e: &str) {
        *self.apn_err.lock().unwrap_or_else(|e| e.into_inner()) = e.to_string();
    }
}

/// `iw` on every AP interface (the agent's `wifi_stations`). Blocking.
pub fn wifi_stations() -> Vec<ni::Station> {
    let sh = |cmd: &str| {
        std::process::Command::new("sh")
            .args(["-c", cmd])
            .stdin(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for iface in ni::ap_ifaces(&sh("iw dev 2>/dev/null")) {
        let ap = ni::parse_ap_info(&sh(&format!("iw dev {iface} info 2>/dev/null")));
        for mut st in ni::parse_station_dump(
            &sh(&format!("iw dev {iface} station dump 2>/dev/null")),
            &iface,
        ) {
            st.band = ap.band;
            st.channel = ap.channel;
            st.width_mhz = ap.width_mhz;
            out.push(st);
        }
    }
    out
}

/// The SIM's (state, imsi): u60d's `sim` block when fresh, else one direct read (the agent's
/// `sim_state_imsi`, with u60d's own block in place of the agent's subscription).
async fn sim_state_imsi(c: &Compat) -> (String, String) {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    if let Some(b) = c
        .src
        .hub
        .as_ref()
        .and_then(|h| h.view("sim"))
        .filter(|v| !v.stale)
    {
        return (s(&b.data, "state"), s(&b.data, "imsi"));
    }
    let sim = (c.src.ubus)("zwrt_zte_mdm.api", "get_sim_info", json!({}))
        .await
        .unwrap_or(Value::Null);
    (s(&sim, "sim_states"), s(&sim, "sim_imsi"))
}

/// One refresh pass (the agent's `refresh_pass` without the lookups and the AT read).
async fn refresh(c: Compat) {
    /// Lets the next request start a pass even if this one panics.
    struct Done(Arc<NetCache>);
    impl Drop for Done {
        fn drop(&mut self) {
            self.0.refreshing.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done(c.net.clone());
    let t = now();
    // someone is looking: whoever owns `exits` (the agent, or u60d's `exits.rs`) looks them up
    // into EXITS_FILE while this is fresh
    let _ = std::fs::write(c.path(ni::EXITS_WANTED), b"");
    let net = (c.src.ubus)("zte_nwinfo_api", "nwinfo_get_netinfo", json!({}))
        .await
        .unwrap_or(Value::Null);
    let (sim_state, imsi) = sim_state_imsi(&c).await;
    let wwan = (c.src.ubus)(
        "zwrt_data",
        "get_wwaniface",
        json!({"source_module": "zte_topsw_data", "cid": 1}),
    )
    .await
    .unwrap_or(Value::Null);
    let (snap, sel) = ni::local_json(&net, &sim_state, &imsi, &wwan);
    {
        let mut g = c.net.lock();
        g.local = snap;
        if let Some(sel) = sel {
            g.selection = Some(sel);
            g.selection_at = now();
        }
    }
    let n = &c.net;
    let wanted = t - n
        .last_full
        .load(Ordering::SeqCst)
        .max(n.last_clients.load(Ordering::SeqCst))
        < CLIENTS_WANTED;
    if wanted {
        let (c2, leases) = (c.clone(), c.path(ni::LEASES));
        let fresh = tokio::task::spawn_blocking(move || {
            let leases = ni::parse_leases(&std::fs::read_to_string(leases).unwrap_or_default());
            let prev = c2.net.lock().clients_prev.clone();
            ni::clients_list((c2.src.stations)(), &leases, &prev, t)
        })
        .await;
        if let Ok((list, next)) = fresh {
            let mut g = c.net.lock();
            g.clients = list;
            g.clients_prev = next;
            g.clients_at = t;
        }
    }
}

fn read_json(c: &Compat, p: &str) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(c.path(p)).ok()?).ok()
}

/// The picker from the engine's files (the agent's `picker_try`).
fn scenes(c: &Compat) -> Value {
    use super::status::{
        SCENARIO_CONFIG, SCENARIO_DISABLED, SCENARIO_PIN, SCENARIO_STATE, TAKEOVER_FILE,
    };
    let read = |p: &str| std::fs::read_to_string(c.path(p)).ok();
    let cfg: sc::Config = read(SCENARIO_CONFIG)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let st: sc::RunState = read(SCENARIO_STATE)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let pin = read(SCENARIO_PIN)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let bodies: Vec<Vec<Option<Value>>> = cfg
        .scenarios
        .iter()
        .map(|s| {
            s.actions
                .iter()
                .map(|a| a.action.body.as_ref().map(Value::from))
                .collect()
        })
        .collect();
    let views: Vec<luyaz_api::scenario::Scene> = cfg
        .scenarios
        .iter()
        .zip(&bodies)
        .map(|(s, b)| luyaz_api::scenario::Scene {
            id: &s.id,
            name: &s.name,
            inhibit_sleep: s.inhibit_sleep,
            abroad: sc::is_abroad(s),
            when: match &s.detect {
                sc::Detect::Ssid { entries } => {
                    luyaz_api::scenario::When::Ssid(entries.iter().map(|e| e.ssid.as_str()).collect())
                }
                sc::Detect::Mcc { mccs } => luyaz_api::scenario::When::Mcc(mccs),
                sc::Detect::Abroad => luyaz_api::scenario::When::Abroad,
                sc::Detect::Fallback => luyaz_api::scenario::When::Fallback,
            },
            does: s
                .actions
                .iter()
                .zip(b)
                .map(|(a, body)| luyaz_api::scenario::Does {
                    path: &a.action.path,
                    body: body.as_ref(),
                })
                .collect(),
        })
        .collect();
    luyaz_api::scenario::picker(
        !c.path(SCENARIO_DISABLED).exists(),
        &st.current,
        pin,
        c.path(TAKEOVER_FILE).exists(),
        &views,
        &cfg.home_mcc,
    )
}

/// The `apn` block (the agent's `apn_read`); nothing switches here yet (8d).
async fn apn(c: &Compat) -> Value {
    let call = |m: &'static str, a: Value| (c.src.ubus)("zwrt_apn_object", m, a);
    let reads = match call("get_apn_mode", json!({})).await {
        Err(e) => Err(e),
        Ok(mode) => Ok((
            mode,
            call("get_apn_at_cid", json!({"cid": 1}))
                .await
                .unwrap_or(Value::Null),
            call("get_auto_apn_list", json!({}))
                .await
                .unwrap_or(Value::Null),
            call("get_manu_apn_list", json!({}))
                .await
                .unwrap_or(Value::Null),
        )),
    };
    let iccid = match c
        .src
        .hub
        .as_ref()
        .and_then(|h| h.view("sim"))
        .filter(|v| !v.stale)
    {
        Some(b) => b.data["iccid"].as_str().unwrap_or("").to_string(),
        None => (c.src.ubus)("zwrt_zte_mdm.api", "get_sim_info", json!({}))
            .await
            .ok()
            .and_then(|v| v["sim_iccid"].as_str().map(str::to_string))
            .unwrap_or_default(),
    };
    let iccid = iccid.trim().trim_end_matches(['F', 'f']).to_string();
    let store = read_json(c, ni::APN_PICK_STORE).unwrap_or(Value::Null);
    let (picked, notice) = ni::apn_pick_view(&store, &iccid, now() as u64);
    let err = c
        .net
        .apn_err
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    ni::apn_block(
        reads,
        picked,
        notice,
        c.net.apn_switching.load(Ordering::SeqCst),
        &err,
    )
}

/// GET /api/netinfo
pub async fn get(State(c): State<Compat>, RawQuery(query): RawQuery) -> Response {
    let q = ni::Query::parse(query.as_deref().unwrap_or(""));
    let t = now();
    let n = &c.net;
    if !q.lite {
        n.last_full.store(t, Ordering::SeqCst);
    }
    if q.clients {
        n.last_clients.store(t, Ordering::SeqCst);
    }
    if t - n.last_pass.load(Ordering::SeqCst) >= PASS_MIN_GAP
        && !n.refreshing.swap(true, Ordering::SeqCst)
    {
        n.last_pass.store(t, Ordering::SeqCst);
        tokio::spawn(refresh(c.clone()));
    }
    let apn = if q.wants_apn() {
        apn(&c).await
    } else {
        Value::Null
    };
    let c2 = c.clone();
    let files = tokio::task::spawn_blocking(move || {
        let exits = read_json(&c2, ni::EXITS_FILE).unwrap_or(Value::Null);
        let proxy_on = false;
        (exits, proxy_on, scenes(&c2))
    })
    .await;
    let Ok((exits, proxy_on, scenes)) = files else {
        return reply(500, json!({"ok": false, "error": "internal error"}));
    };
    let _ = proxy_on; // read only by the proxy lines (the public build has none)
    let (guard, scan) = {
        let o = n.ops.lock().unwrap_or_else(|e| e.into_inner());
        (o.guard.json(), o.scan.json())
    };
    let g = n.lock();
    let data = ni::page(ni::Page {
        now: t,
        direct: exits["direct"].clone(),
        selection: g.selection,
        selection_at: g.selection_at,
        guard,
        scan,
        neighbors: ni::neighbors_json(0, &[]),
        clients: &g.clients,
        clients_at: g.clients_at,
        scenes,
        apn,
        local: &g.local,
    });
    reply(200, json!({"ok": true, "data": data}))
}
