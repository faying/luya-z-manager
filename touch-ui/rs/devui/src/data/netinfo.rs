//! The 网络 pages' data (src/netinfo.c): zte-agent's /api/netinfo (exits, operators, roaming,
//! the selection and its guard, the operator scan, neighbours, scenes, Wi-Fi clients, APNs),
//! read as much as the page on screen needs; and the actions, which only start something (the
//! agent does the slow part; the next read shows how it went).
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::util::{bytes_cap, jstr, mono_ms, obj_end, trunc, wall};
use crate::cf;
use crate::json;
use crate::lang::{self, tr};
use crate::source::{NI_APN, NI_CLIENTS, NI_HOME, NI_PAGE};
use devui_model::{netinfo_t, ni_apn_t, ni_cell_t, ni_client_t, ni_exit_t, ni_oper_t, ni_scan_op_t, ni_scene_t};

const NI_TTL_MS: i64 = 5000;
const NI_BUSY_MS: i64 = 2000;
const NI_HOME_MS: i64 = 30000;
/// a full read with apn (~1 KB) has room to spare; a cut one reads as 读网络信息失败
const NI_RESP_MAX: usize = 32768;
pub const NI_MAX_OPS: usize = 8;
pub const NI_MAX_CELLS: usize = 8;
pub const NI_MAX_SCENES: usize = 6;
pub const NI_MAX_CLIENTS: usize = 8;
pub const NI_MAX_APNS: usize = 10;
pub const NI_MAX_CANDS: usize = 6;

pub struct Netinfo {
    pub ni: netinfo_t,
    act_err: String,
    was_mode: i32,
    poll_ms: i64,
    /// what was shown last (now left out: it moves every read)
    last: Option<(netinfo_t, String)>,
    /// an action was just sent: read at the busy pace until then
    hurry_until: i64,
}

impl Default for Netinfo {
    fn default() -> Self {
        Netinfo { ni: empty(), act_err: String::new(), was_mode: 0, poll_ms: 0, last: None, hurry_until: 0 }
    }
}

/// netinfo_t with its fixed arrays (C keeps old rows past the count; so does this).
pub fn empty() -> netinfo_t {
    netinfo_t {
        ops: vec![ni_scan_op_t::default(); NI_MAX_OPS],
        cells: vec![ni_cell_t::default(); NI_MAX_CELLS],
        scenes: vec![ni_scene_t::default(); NI_MAX_SCENES],
        clients: vec![ni_client_t::default(); NI_MAX_CLIENTS],
        apns: vec![ni_apn_t::default(); NI_MAX_APNS],
        cands: vec![ni_apn_t::default(); NI_MAX_CANDS],
        ..Default::default()
    }
}

/// key_en in English when the agent gave it and it is not empty, else key.
fn jstr_pick(o: &[u8], key: &str, key_en: &str, cap: usize) -> String {
    if lang::is_en() {
        let v = jstr(o, key_en, cap);
        if !v.is_empty() {
            return v;
        }
    }
    jstr(o, key, cap)
}

/// The array's objects one by one (next_obj): from `cur`, skip to '{' or ']'; each object is
/// cut at cap - 1 bytes.
fn next_obj(s: &[u8], cur: &mut usize, cap: usize) -> Option<Vec<u8>> {
    let mut p = *cur;
    while p < s.len() && s[p] != 0 && s[p] != b'{' && s[p] != b']' {
        p += 1;
    }
    if s.get(p) != Some(&b'{') {
        return None;
    }
    let q = obj_end(s, p)?;
    *cur = q + 1;
    let len = (q - p + 1).min(cap - 1);
    Some(s[p..p + len].to_vec())
}

fn objs(arr: &[u8], max: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur = 0;
    while out.len() < max
        && let Some(o) = next_obj(arr, &mut cur, 1024)
    {
        out.push(o);
    }
    out
}

fn strtol(v: &str) -> i64 {
    json::atol(v.as_bytes())
}

fn parse_exit(data: &[u8], key: &str) -> ni_exit_t {
    let Some(o) = json::get(data, key, 1024).filter(|o| o.first() == Some(&b'{')) else { return ni_exit_t::default() };
    ni_exit_t {
        present: 1,
        ip: jstr(&o, "ip", 48),
        geo: jstr_pick(&o, "geo", "geo_en", 64),
        isp: jstr_pick(&o, "isp", "isp_en", 48),
        node: jstr(&o, "node", 64),
        err: jstr_pick(&o, "error", "error_en", 80),
    }
}

fn parse_oper(data: &[u8], key: &str) -> ni_oper_t {
    // name + operator_en + country/_en/_iso: a long broadcast name twice
    let Some(o) = json::get(data, key, 512).filter(|o| o.first() == Some(&b'{')) else { return ni_oper_t::default() };
    ni_oper_t {
        name: jstr_pick(&o, "name", "operator_en", 48),
        country: jstr_pick(&o, "country", "country_en", 24),
        country_iso: jstr(&o, "country_iso", 8),
        mcc: jstr(&o, "mcc", 4),
        mnc: jstr(&o, "mnc", 4),
    }
}

fn parse_apn(o: &[u8]) -> ni_apn_t {
    ni_apn_t {
        id: jstr(o, "id", 24),
        name: jstr(o, "name", 40),
        apn: jstr(o, "apn", 40),
        pdp: strtol(&jstr(o, "pdp", 16)) as i32,
        selected: (jstr(o, "selected", 16) == "true") as i32,
        in_use: (jstr(o, "in_use", 16) == "true") as i32,
        iot: (jstr(o, "iot", 16) == "true") as i32,
    }
}

/// /api/netinfo's "data" into the struct (parse).
pub fn parse(n: &mut netinfo_t, data: &[u8]) {
    n.now = json::get_int(data, "now", 0);
    n.direct = parse_exit(data, "direct");
    n.proxy = parse_exit(data, "proxy");
    n.home = parse_oper(data, "home_operator");
    n.serving = parse_oper(data, "serving_operator");
    n.roaming = match jstr(data, "roaming", 16).as_str() {
        "true" => 1,
        "false" => 0,
        _ => -1,
    };
    n.selection.clear();
    if let Some(o) = json::get(data, "selection", 1024) {
        n.selection = jstr(&o, "mode", 12);
    }

    n.guard_phase.clear();
    n.guard_target.clear();
    n.guard_reason.clear();
    n.guard_reason_code.clear();
    // reason (≤180 B) + reason_en (≤120) + the rest: target sorts last
    if let Some(o) = json::get(data, "guard", 1024) {
        n.guard_phase = jstr(&o, "phase", 16);
        n.guard_target = jstr(&o, "target", 8);
        n.guard_reason = jstr_pick(&o, "reason", "reason_en", 192);
        n.guard_reason_code = jstr(&o, "reason_code", 24);
    }

    n.scan_state.clear();
    n.scan_err.clear();
    n.nops = 0;
    if let Some(sub) = json::get(data, "scan", 8192) {
        n.scan_state = jstr(&sub, "state", 12);
        n.scan_err = jstr_pick(&sub, "error", "error_en", 128);
        if let Some(arr) = json::get(&sub, "operators", 6144) {
            for o in objs(&arr, NI_MAX_OPS) {
                n.ops[n.nops as usize] = ni_scan_op_t {
                    plmn: jstr(&o, "plmn", 8),
                    name: jstr_pick(&o, "name", "operator_en", 48),
                    country: jstr_pick(&o, "country", "country_en", 24),
                    country_iso: jstr(&o, "country_iso", 8),
                    rat: jstr(&o, "rat", 8),
                    status: jstr(&o, "status", 4),
                };
                n.nops += 1;
            }
        }
    }

    n.scene_known = 0;
    n.nscenes = 0;
    n.scene_current.clear();
    n.scene_pin.clear();
    if let Some(sub) = json::get(data, "scenes", 8192) {
        n.scene_known = 1;
        n.scene_enabled = (jstr(&sub, "enabled", 16) == "true") as i32;
        n.scene_takeover = (jstr(&sub, "guard_takeover", 16) == "true") as i32;
        n.scene_current = jstr(&sub, "current", 24);
        n.scene_pin = jstr(&sub, "pin", 24);
        // list or list_en: 6 scenes × id, name, when, does
        if let Some(arr) = json::get(&sub, "list", 4096) {
            for o in objs(&arr, NI_MAX_SCENES) {
                n.scenes[n.nscenes as usize] = ni_scene_t {
                    id: jstr(&o, "id", 24),
                    name: jstr(&o, "name", 32),
                    wifi_off: (jstr(&o, "wifi_off", 16) == "true") as i32,
                    abroad: (jstr(&o, "abroad", 16) == "true") as i32,
                    when: jstr(&o, "when", 160),
                    does: jstr(&o, "does", 160),
                };
                n.nscenes += 1;
            }
        }
        // English: list_en in the same order (name_en null: a renamed scene keeps its name)
        if lang::is_en()
            && let Some(arr) = json::get(&sub, "list_en", 4096)
        {
            let mut cur = 0;
            for i in 0..n.nscenes as usize {
                let Some(o) = next_obj(&arr, &mut cur, 1024) else { break };
                let sc = &mut n.scenes[i];
                if jstr(&o, "id", 24) != sc.id {
                    continue;
                }
                let v = jstr(&o, "name_en", 160);
                if !v.is_empty() {
                    sc.name = trunc(&v, 32);
                }
                let v = jstr(&o, "when_en", 160);
                if !v.is_empty() {
                    sc.when = trunc(&v, 160);
                }
                let v = jstr(&o, "does_en", 160);
                if !v.is_empty() {
                    sc.does = trunc(&v, 160);
                }
            }
        }
    }

    n.clients_known = 0;
    n.nclients = 0;
    if let Some(sub) = json::get(data, "clients", 8192).filter(|s| s.first() == Some(&b'{')) {
        n.clients_known = (json::get_int(&sub, "at", 0) > 0) as i32;
        if let Some(arr) = json::get(&sub, "list", 8192) {
            for o in objs(&arr, NI_MAX_CLIENTS) {
                let v = |k: &str| jstr(&o, k, 32);
                let rate = |k: &str| {
                    let s = v(k);
                    if s.is_empty() { -1 } else { strtol(&s) }
                };
                n.clients[n.nclients as usize] = ni_client_t {
                    name: jstr(&o, "name", 40),
                    mac: jstr(&o, "mac", 20),
                    ip: jstr(&o, "ip", 20),
                    down: strtol(&v("down_bytes")),
                    up: strtol(&v("up_bytes")),
                    down_rate: rate("down_rate"),
                    up_rate: rate("up_rate"),
                    signal: strtol(&v("signal")) as i32,
                    signal_tier: jstr(&o, "signal_tier", 8),
                    band: jstr(&o, "band", 12),
                    wifi_gen: strtol(&v("wifi_gen")) as i32,
                    link_down: strtol(&v("link_down_mbps")) as i32,
                };
                n.nclients += 1;
            }
        }
    }

    n.apn_known = 0;
    n.apn_manual = 0;
    n.apn_switching = 0;
    n.apn_switch_err.clear();
    n.napns = 0;
    n.ncands = 0;
    n.apn_picked.clear();
    n.apn_notice.clear();
    n.apn_in_use = ni_apn_t::default();
    if let Some(sub) = json::get(data, "apn", 8192).filter(|s| s.first() == Some(&b'{')) {
        let mode = jstr(&sub, "mode", 16);
        n.apn_known = !mode.is_empty() as i32;
        n.apn_manual = (mode == "manual") as i32;
        n.apn_switching = (jstr(&sub, "switching", 16) == "true") as i32;
        n.apn_switch_err = jstr(&sub, "switch_error", 64);
        if let Some(o) = json::get(&sub, "in_use", 1024).filter(|o| o.first() == Some(&b'{')) {
            n.apn_in_use = parse_apn(&o);
        }
        if let Some(arr) = json::get(&sub, "manual", 4096) {
            for o in objs(&arr, NI_MAX_APNS) {
                n.apns[n.napns as usize] = parse_apn(&o);
                n.napns += 1;
            }
        }
        if let Some(arr) = json::get(&sub, "auto", 4096) {
            for o in objs(&arr, NI_MAX_CANDS) {
                n.cands[n.ncands as usize] = parse_apn(&o);
                n.ncands += 1;
            }
        }
        n.apn_picked = jstr(&sub, "picked_id", 24);
        if let Some(o) = json::get(&sub, "notice", 1024).filter(|o| o.first() == Some(&b'{')) {
            let at = strtol(&jstr(&o, "at", 16));
            let now = wall();
            if at > 0 && now - at >= 0 && now - at < 600 {
                n.apn_notice = jstr_pick(&o, "text", "text_en", 96);
            }
        }
    }

    n.nbr_state.clear();
    n.nbr_err.clear();
    n.ncells = 0;
    n.nbr_at = 0;
    if let Some(sub) = json::get(data, "neighbors", 8192) {
        n.nbr_state = jstr(&sub, "state", 12);
        n.nbr_err = jstr_pick(&sub, "error", "error_en", 96);
        n.nbr_at = json::get_int(&sub, "scanned_at", 0);
        if let Some(arr) = json::get(&sub, "cells", 6144) {
            for o in objs(&arr, NI_MAX_CELLS) {
                n.cells[n.ncells as usize] = ni_cell_t {
                    rat: jstr(&o, "rat", 4),
                    pci: jstr(&o, "pci", 8),
                    arfcn: jstr(&o, "arfcn", 12),
                    rsrp: jstr(&o, "rsrp", 8),
                    rsrq: jstr(&o, "rsrq", 8),
                    sinr: jstr(&o, "sinr", 8),
                };
                n.ncells += 1;
            }
        }
    }
}

impl Netinfo {
    fn load(&mut self, agent: &mut CoreApi, mode: i32) {
        let path = match mode {
            NI_PAGE => "/api/netinfo",
            NI_APN => "/api/netinfo?lite=1&apn=1",
            NI_CLIENTS => "/api/netinfo?lite=1&clients=1",
            _ => "/api/netinfo?lite=1",
        };
        let (code, b) = agent.api("GET", path, None, NI_RESP_MAX);
        let msg = match code {
            0 => Some(tr("连不上管理后台").as_bytes().to_vec()),
            401 => Some(tr("登录管理后台失败（密码不对？）").as_bytes().to_vec()),
            404 => Some(tr("管理后台版本太旧，没有网络页接口").as_bytes().to_vec()),
            _ => None,
        };
        if let Some(m) = msg {
            self.ni.err = bytes_cap(&m, 96);
            return;
        }
        let Some(data) = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", NI_RESP_MAX)) else {
            self.ni.err = bytes_cap(&cf!(tr("读网络信息失败（HTTP %d）"), code), 96);
            return;
        };
        self.ni.err.clear();
        parse(&mut self.ni, &data);
    }

    /// The next `secs` read at the busy pace (an action was just sent).
    pub fn hurry(&mut self, secs: i32) {
        self.hurry_until = mono_ms() + secs as i64 * 1000;
    }

    fn busy(&self) -> bool {
        let n = &self.ni;
        mono_ms() < self.hurry_until
            || n.scan_state == "scanning"
            || n.nbr_state == "scanning"
            || n.guard_phase == "registering"
            || n.guard_phase == "reverting"
    }

    /// mode: NI_* (what the page on screen needs). At once when it changes; then 5 s (2 s
    /// while busy), 30 s for Home. true = what shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, mode: i32) -> bool {
        let just_shown = mode != 0 && mode != self.was_mode;
        let t = mono_ms();
        self.was_mode = mode;
        if mode == 0 {
            return false;
        }
        let ttl = if mode == NI_HOME {
            NI_HOME_MS
        } else if self.busy() {
            NI_BUSY_MS
        } else {
            NI_TTL_MS
        };
        if !just_shown && self.poll_ms != 0 && t - self.poll_ms < ttl {
            return false;
        }
        self.poll_ms = t;
        self.load(agent, mode);
        // now moves every read: not "changed"
        let mut cur = self.ni.clone();
        cur.now = 0;
        let sig = (cur, self.act_err.clone());
        if self.last.as_ref() == Some(&sig) {
            return false;
        }
        self.last = Some(sig);
        true
    }

    pub fn action_error(&self) -> &str {
        &self.act_err
    }

    /// Send an action; refused (409 …): keep the agent's reason for the page.
    fn act(&mut self, agent: &mut CoreApi, path: &str, json: Option<&str>) {
        let (code, b) = agent.api("POST", path, json, NI_RESP_MAX);
        self.act_err.clear();
        if code == 0 {
            self.act_err = trunc(tr("连不上管理后台"), 160);
        } else if code >= 300 {
            if let Some(b) = b {
                self.act_err = jstr_pick(&b, "error", "error_en", 160);
            }
            if self.act_err.is_empty() {
                self.act_err = bytes_cap(&cf!(tr("被拒绝（HTTP %d）"), code), 160);
            }
        }
        self.poll_ms = 0; // read again at once
        self.was_mode = 0;
    }

    pub fn scan(&mut self, agent: &mut CoreApi) {
        self.act(agent, "/api/netinfo/scan", None);
    }
    pub fn auto(&mut self, agent: &mut CoreApi) {
        self.act(agent, "/api/modem/netselect/auto", None);
    }
    pub fn nbr_scan(&mut self, agent: &mut CoreApi) {
        self.act(agent, "/api/netinfo/neighbors/scan", None);
    }

    /// Pin this scenario; "" = back to automatic.
    pub fn pin(&mut self, agent: &mut CoreApi, id: &str) {
        let js = if id.is_empty() { "{\"id\":null}".to_string() } else { trunc(&format!("{{\"id\":\"{id}\"}}"), 64) };
        self.act(agent, "/api/scenario/pin", Some(&js));
    }

    /// "auto", a manual APN's id, or a candidate's (cuts the data for a moment).
    pub fn apn_use(&mut self, agent: &mut CoreApi, id: &str) {
        if id.is_empty() || id.len() >= 24 || id.contains(['"', '\\']) {
            return;
        }
        self.act(agent, "/api/netinfo/apn", Some(&format!("{{\"id\":\"{id}\"}}")));
        // 202 = the agent switches in the background: an old failure is not this one's
        if self.act_err.is_empty() {
            self.ni.apn_switch_err.clear();
            self.ni.apn_switching = 1;
        }
    }

    pub fn register(&mut self, agent: &mut CoreApi, op: usize) {
        if op >= self.ni.nops.max(0) as usize {
            return;
        }
        let js = trunc(&format!("{{\"m_mcc_mnc\":\"{}\",\"m_rat\":\"{}\"}}", self.ni.ops[op].plmn, self.ni.ops[op].rat), 96);
        self.act(agent, "/api/modem/register", Some(&js));
    }
}

#[cfg(test)]
mod tests;
