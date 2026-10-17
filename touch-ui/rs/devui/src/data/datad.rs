//! zwrt-datad (src/data.c): the /v2 snapshot (GET /v2/state) and its event stream
//! (/v2/events, SSE), put back into the old /state shape so one parser reads both; reconnect on
//! an epoch change or a seq gap, stale blocks left out, the 45 s silence check; and the writes
//! (POST /control), parked until datad answers (a client that hangs up first loses the request:
//! datad's server drops it before the handler runs, measured 2026-09-23).
// SPDX-License-Identifier: MIT

use super::http::{self, read_into, wait};
use super::util::{find, jget, mono_ms32, text, trunc, wall};
use crate::json;
use crate::lang::tr;
use crate::logic::{self, Ctl};
use devui_model::{data_notice_t, devui_data_t, devui_data_t_client, devui_data_t_sms};
use std::os::fd::{AsRawFd, OwnedFd};

pub const SMS_MAX: usize = 32;
pub const SMS_TEXT_MAX: usize = 16384;
const SMS_BUF_MAX: usize = 1048576;
const STATE_BUF_MAX: usize = 1048576;
const HTTP_BUF_MAX: usize = STATE_BUF_MAX + 8192;
const SSE_BUF_MAX: usize = STATE_BUF_MAX + 8192;
const SMS_OBJECT_MAX: usize = SMS_TEXT_MAX + 32768;
/// consecutive unparseable SSE events before the stream is dropped for HTTP polling
const SSE_MAX_PARSE_FAIL: u32 = 5;

const CONTROL_SLOTS: usize = 8;
const CONTROL_LINGER_MS: u32 = 5000;
const CONTROL_REPLY_MAX: usize = 2048;
/// The emergency write script (Luya-Z rename D1/D4).
const FALLBACK_SCRIPT: &str = "/data/luyaz/scripts/luyaz-fallback.sh";
/// = start.sh's -i 1000; 5000 = datad's upper limit
const PACE_LIT_MS: i32 = 1000;
const PACE_DARK_MS: i32 = 5000;
const SMS_ARM_MS: u32 = 4000;
const SMS_ARM_DEBOUNCE_MS: u32 = 400;

/// Where datad is and how patient to be (data.c's DEVUI_BACKEND_* defines; tests shorten them).
#[derive(Clone, Debug)]
pub struct Cfg {
    pub host: String,
    pub port: u16,
    pub retry_ms: u32,
    /// no byte at all for this long (datad heartbeats every 5 s) = gone
    pub silent_ms: u32,
    pub silent_retry_ms: u32,
    pub io_ms: i32,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg { host: "127.0.0.1".into(), port: 9460, retry_ms: 1000, silent_ms: 45000, silent_retry_ms: 5000, io_ms: 300 }
    }
}

// ---- /v2 blocks: the ones the screen reads and where they go in the old /state shape ----
const B_SIGNAL: usize = 0;
const B_BATTERY: usize = 1;
const B_CHARGER: usize = 2;
const B_LIVE: usize = 3;
const B_SMS: usize = 4;
const B_SMS_LIST: usize = 5;
const B_SIM: usize = 6;
const B_QOS: usize = 7;
const B_CLIENTS: usize = 8;
const B_WLAN: usize = 9;
const B_NFC: usize = 10;
const B_DHCP: usize = 11;
const B_INTERFACES: usize = 12;
const B_TYPEC: usize = 13;
const B_POWERBANK: usize = 14;
const B_COUNT: usize = 15;
const V2_NAMES: [&str; B_COUNT] =
    ["signal", "battery", "charger", "live", "sms", "sms_list", "sim", "qos", "clients", "wlan", "nfc", "dhcp", "interfaces", "typec", "powerbank"];

#[derive(Default, Clone)]
struct Blk {
    data: Vec<u8>,
    fresh: bool,
}

/// A /control request waiting for datad's answer.
struct Slot {
    fd: OwnedFd,
    t: u32,
    action: String,
    reply: Vec<u8>,
}

pub struct Datad {
    pub cfg: Cfg,
    sse: Option<OwnedFd>,
    sse_parse_fail_streak: u32,
    pub next_retry_ms: u32,
    sse_buf: Vec<u8>,
    live_json: Vec<u8>,
    pub live_data: devui_data_t,
    live_valid: bool,
    live_version: u64,
    committed_live_version: u64,
    current_data: devui_data_t,
    current_valid: bool,
    /// the last byte from datad (monotonic ms / wall s); 0 = never
    alive_ms: u32,
    alive_wall: i64,
    silent: bool,
    /// the /v2 stream position: another epoch or a seq gap = reconnect
    pub epoch: String,
    pub seq: u64,
    pub have_seq: bool,
    pub resync: bool,
    blk: Vec<Blk>,
    ctl: Vec<Slot>,
    notice: data_notice_t,
    op_seq: u32,
    /// the interval last asked for; -1 = unknown
    pace_sent: i32,
    sms_arm_id: i64,
    sms_arm_ms: u32,
}

impl Default for Datad {
    fn default() -> Self {
        Datad::new(Cfg::default())
    }
}

// ---- the old /state parser ----

/// Shown only; English names from manager docs/ui-glossary.md §8.
fn mainland_operator_cn(mcc: i32, mnc: i32, raw: &str) -> Option<&'static str> {
    if mcc == 460 {
        match mnc {
            0 | 2 | 4 | 7 | 8 => return Some(tr("中国移动")),
            1 | 6 | 9 => return Some(tr("中国联通")),
            3 | 5 | 11 => return Some(tr("中国电信")),
            15 => return Some(tr("中国广电")),
            _ => {}
        }
    }
    match raw {
        "China Mobile" | "CMCC" => Some(tr("中国移动")),
        "China Unicom" | "CUCC" => Some(tr("中国联通")),
        "China Telecom" | "CTCC" => Some(tr("中国电信")),
        "China Broadnet" | "China Broadcasting Network" => Some(tr("中国广电")),
        _ => None,
    }
}

fn truthy(v: &str) -> bool {
    matches!(v, "1" | "true" | "True" | "TRUE" | "yes" | "YES" | "on" | "ON")
}

fn force_hsr() -> bool {
    std::env::var("DEVUI_FORCE_HSR").is_ok_and(|v| truthy(&v)) || std::path::Path::new("/tmp/luyaz/force-hsr").exists()
}

/// From p (inside an array text), the next '{' outside a string (find_next_object).
fn next_object(s: &[u8], from: usize) -> Option<usize> {
    let (mut instr, mut esc) = (false, false);
    for (i, &c) in s.iter().enumerate().skip(from) {
        if c == 0 {
            break;
        }
        if instr {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                instr = false;
            }
        } else if c == b'"' {
            instr = true;
        } else if c == b'{' {
            return Some(i);
        }
    }
    None
}

/// The '}' closing the object at p ('{'), skipping braces in strings (obj_end).
fn obj_end(s: &[u8], p: usize) -> Option<usize> {
    let (mut depth, mut instr) = (0i32, false);
    let mut i = p;
    while i < s.len() && s[i] != 0 {
        let c = s[i];
        if instr {
            if c == b'\\' && i + 1 < s.len() && s[i + 1] != 0 {
                i += 1;
            } else if c == b'"' {
                instr = false;
            }
        } else if c == b'"' {
            instr = true;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The snapshot as the old /state renders it → the screen's struct (parse_snapshot). None =
/// nothing to parse.
pub fn parse_snapshot(buf: &[u8]) -> Option<devui_data_t> {
    let mut d = devui_data_t {
        cpu_usage: -1,
        dps_mode: -1,
        cell_data: -1,
        cell_roam: -1,
        usb_cc: -1,
        powerbank: -1,
        client: vec![devui_data_t_client::default(); 16],
        sms: vec![devui_data_t_sms::default(); SMS_MAX],
        ..Default::default()
    };
    if buf.first().is_none_or(|&c| c == 0) {
        return None;
    }
    let gs = jget;
    let gi = |o: &[u8], k: &str, def: i64| json::get_int(o, k, def);

    if let Some(sec) = json::get(buf, "net", STATE_BUF_MAX) {
        let s = &sec[..];
        d.net_type = gs(s, "type", 16);
        d.operator_name = gs(s, "operator", 48);
        d.roaming = gs(s, "roaming", 16);
        d.band = gs(s, "band", 16);
        d.nr_band = gs(s, "nr_band", 16);
        d.nr_snr = gs(s, "nr_snr", 12);
        d.wan_status = gs(s, "wan_status", 32);
        d.lte_snr = gs(s, "lte_snr", 12);
        d.bandwidth = gs(s, "bandwidth", 12);
        d.operate_mode = gs(s, "operate_mode", 16);
        d.lte_pci = gi(s, "lte_pci", 0) as i32;
        d.channel = gi(s, "channel", 0);
        d.nr_bw = gs(s, "nr_bw", 12);
        d.nrca = gs(s, "nrca", 256);
        d.lteca = gs(s, "lteca", 256);
        d.ltecasig = gs(s, "ltecasig", 256);
        d.net_select = gs(s, "net_select", 16);
        d.sa_bands = gs(s, "sa_bands", 256);
        d.nsa_bands = gs(s, "nsa_bands", 256);
        d.lte_bands = gs(s, "lte_bands", 256);
        d.sa_sup = gs(s, "nr_sa_supported_bands", 256);
        d.nsa_sup = gs(s, "nr_nsa_supported_bands", 256);
        d.lte_sup = gs(s, "lte_supported_bands", 256);
        d.hsr = json::get(s, "HSR", 16).is_some_and(|v| truthy(&text(&v))) as i32;
        d.bars = gi(s, "bars", 0) as i32;
        d.nr_rsrp = gi(s, "nr_rsrp", 0) as i32;
        d.nr_rsrq = gi(s, "nr_rsrq", 0) as i32;
        d.nr_rssi = gi(s, "nr_rssi", 0) as i32;
        d.lte_rsrp = gi(s, "lte_rsrp", 0) as i32;
        d.lte_rsrq = gi(s, "lte_rsrq", 0) as i32;
        d.lte_rssi = gi(s, "lte_rssi", 0) as i32;
        d.rssi = gi(s, "rssi", 0) as i32;
        d.mcc = gi(s, "mcc", 0) as i32;
        d.mnc = gi(s, "mnc", 0) as i32;
        d.nr_pci = gi(s, "nr_pci", 0) as i32;
        d.nr_cell_id = gi(s, "nr_cell_id", 0);
        d.nr_channel = gi(s, "nr_channel", 0);
        // Rust datad: lte_cell_id / lte_channel; MU5252 and the C datad: cell_id / channel
        d.lte_cell_id = gi(s, "lte_cell_id", 0);
        if d.lte_cell_id == 0 {
            d.lte_cell_id = gi(s, "cell_id", 0);
        }
        if d.channel == 0 {
            d.channel = gi(s, "lte_channel", 0);
        }
        if let Some(cn) = mainland_operator_cn(d.mcc, d.mnc, &d.operator_name) {
            d.operator_name = trunc(cn, 48);
        }
    }

    if let Some(sec) = json::get(buf, "battery", STATE_BUF_MAX) {
        let s = &sec[..];
        d.bat_percent = gi(s, "percent", 0) as i32;
        d.bat_temp = gi(s, "temp", 0) as i32;
        d.charging = gi(s, "charging", 0) as i32;
        d.charger_connect = gi(s, "charger_connect", 0) as i32;
        d.chg_uv = gi(s, "chg_uv", 0);
        d.chg_ua = gi(s, "chg_ua", 0);
        d.bat_uv = gi(s, "bat_uv", 0);
        d.bat_ua = gi(s, "bat_ua", 0);
    }

    if let Some(sec) = json::get(buf, "clients", STATE_BUF_MAX) {
        let s = &sec[..];
        d.clients_total = gi(s, "total", 0) as i32;
        d.clients_wifi = gi(s, "wifi", 0) as i32;
        d.clients_lan = gi(s, "lan", 0) as i32;
        d.client_n = 0;
        if let Some(list) = json::get(s, "list", 2048) {
            let mut p = 0;
            while (d.client_n as usize) < 16 {
                let Some(start) = next_object(&list, p) else { break };
                let Some(end) = obj_end(&list, start) else { break };
                let obj = &list[start..=end];
                if obj.len() < 160 {
                    let c = &mut d.client[d.client_n as usize];
                    c.name = gs(obj, "name", 40);
                    c.ip = gs(obj, "ip", 24);
                    c.mac = gs(obj, "mac", 20);
                    d.client_n += 1;
                }
                p = end + 1;
            }
        }
    }

    d.sms_unread = 0;
    d.sms_n = 0;
    if let Some(smsbuf) = json::get(buf, "sms", SMS_BUF_MAX) {
        d.sms_unread = gi(&smsbuf, "unread", 0) as i32;
        if let Some(list) = json::get(&smsbuf, "list", SMS_BUF_MAX) {
            let mut p = 0;
            while (d.sms_n as usize) < SMS_MAX {
                let Some(start) = next_object(&list, p) else { break };
                let Some(end) = obj_end(&list, start) else { break };
                let obj = &list[start..=end];
                if obj.len() < SMS_OBJECT_MAX {
                    let m = &mut d.sms[d.sms_n as usize];
                    m.id = gi(obj, "id", 0);
                    m.num = gs(obj, "num", 40);
                    m.date = gs(obj, "date", 16);
                    m.text = gs(obj, "text", SMS_TEXT_MAX);
                    m.unread = gi(obj, "unread", 0) as i32;
                    d.sms_n += 1;
                }
                p = end + 1;
            }
        }
    }

    if let Some(sec) = json::get(buf, "wlan", STATE_BUF_MAX) {
        d.wifi_ssid = gs(&sec, "ssid", 64);
        d.wifi_key = gs(&sec, "key", 64);
        d.wifi_enc = gs(&sec, "enc", 24);
        d.wifi_enabled = gi(&sec, "enabled", 1) as i32;
    }
    if let Some(sec) = json::get(buf, "nfc", STATE_BUF_MAX) {
        d.nfc_switch = gi(&sec, "switch", 0) as i32;
    }
    if let Some(sec) = json::get(buf, "typec", STATE_BUF_MAX) {
        d.usb_cc = gi(&sec, "cc_attch_state", -1) as i32;
        d.usb_power_role = gs(&sec, "power_role", 12);
        d.usb_data_role = gs(&sec, "data_role", 12);
    }
    if let Some(sec) = json::get(buf, "powerbank", STATE_BUF_MAX) {
        d.powerbank = gi(&sec, "state", -1) as i32;
    }
    if let Some(sec) = json::get(buf, "dhcp", STATE_BUF_MAX) {
        d.dhcp_ip = gs(&sec, "ip", 24);
        d.dhcp_start = gs(&sec, "start", 24);
        d.dhcp_limit = gs(&sec, "limit", 8);
        d.dhcp_leasetime = gs(&sec, "leasetime", 12);
    }
    if let Some(sec) = json::get(buf, "power", STATE_BUF_MAX)
        && let Some(sub) = json::get(&sec, "direct_supply", 2048)
        && let Some(mode) = json::get(&sub, "mode", 16)
    {
        match &mode[..] {
            b"enable" => d.dps_mode = 1,
            b"disable" => d.dps_mode = 0,
            _ => {}
        }
    }
    if let Some(sec) = json::get(buf, "interfaces", STATE_BUF_MAX)
        && let Some(sub) = json::get(&sec, "cellular", 2048)
    {
        // enable is the last value written: 0 after boot while the auto dial is connected
        // (E4 T12, B31). 0 counts as off only when the call is down (datad's data_switch()).
        let mut v = gi(&sub, "enable", -1);
        if v == 0 {
            match json::get(&sub, "connect_status", 48) {
                Some(st) => {
                    let st = text(&st);
                    if (st.contains("connected") && !st.contains("disconnect")) || st == "connecting" {
                        v = 1;
                    } else if st.is_empty() {
                        v = -1;
                    }
                }
                None => v = -1,
            }
        }
        d.cell_data = if v == 0 || v == 1 { v as i32 } else { -1 };
        let v = gi(&sub, "roam_enable", -1);
        d.cell_roam = if v == 0 || v == 1 { v as i32 } else { -1 };
    }
    if let Some(sec) = json::get(buf, "traffic", STATE_BUF_MAX) {
        let s = &sec[..];
        d.rx_speed = gi(s, "rx_speed", 0);
        d.tx_speed = gi(s, "tx_speed", 0);
        d.rx_bytes = gi(s, "rx_bytes", 0);
        d.tx_bytes = gi(s, "tx_bytes", 0);
        d.day_rx_bytes = gi(s, "day_rx_bytes", 0);
        d.day_tx_bytes = gi(s, "day_tx_bytes", 0);
        d.month_rx_bytes = gi(s, "month_rx_bytes", 0);
        d.month_tx_bytes = gi(s, "month_tx_bytes", 0);
    }
    if let Some(sec) = json::get(buf, "qos", STATE_BUF_MAX) {
        d.qci = gi(&sec, "qci", 0) as i32;
        d.ambr_dl = json::get(&sec, "ambr_dl", 32).map_or(0.0, |v| json::atof(&v));
        d.ambr_ul = json::get(&sec, "ambr_ul", 32).map_or(0.0, |v| json::atof(&v));
        d.qos_bearer = gs(&sec, "bearer", 16);
        d.qos_stale = json::get(&sec, "stale", 8).is_some_and(|v| v == b"true") as i32;
        d.usb_mode = gs(&sec, "usb_mode", 16);
    }
    if let Some(sec) = json::get(buf, "system", STATE_BUF_MAX) {
        let s = &sec[..];
        d.uptime = gi(s, "uptime", 0);
        d.cpu_temp = gi(s, "cpu_temp", 0);
        d.cpu_usage = gi(s, "cpu_usage", -1);
        d.mem_used_pct = gi(s, "mem_used_pct", 0);
        d.mem_total = gi(s, "mem_total", 0);
        d.mem_avail = gi(s, "mem_avail", 0);
        d.model = gs(s, "model", 64);
        d.fw = gs(s, "fw", 80);
        d.sw_version = gs(s, "sw_version", 80);
        d.imei = gs(s, "imei", 24);
    }
    if let Some(sec) = json::get(buf, "sim", STATE_BUF_MAX) {
        d.sim_state = gs(&sec, "state", 24);
        d.sim_iccid = gs(&sec, "iccid", 24);
        d.sim_imsi = gs(&sec, "imsi", 20);
        d.sim_msisdn = gs(&sec, "msisdn", 24);
        d.sim_spn = gs(&sec, "spn", 32);
    }
    if force_hsr() {
        d.hsr = 1;
    }
    d.valid = 1;
    Some(d)
}

/// The empty struct data_refresh hands out when there is none.
fn empty() -> devui_data_t {
    devui_data_t {
        cpu_usage: -1,
        client: vec![devui_data_t_client::default(); 16],
        sms: vec![devui_data_t_sms::default(); SMS_MAX],
        ..Default::default()
    }
}

fn trim_ws(s: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\r' | b'\n');
    let a = s.iter().position(|c| !ws(c)).unwrap_or(s.len());
    let b = s.iter().rposition(|c| !ws(c)).map_or(a, |i| i + 1);
    &s[a..b.max(a)]
}

/// Bytes up to the first blank line ("\n\n" or "\n\r\n"), or 0 when no event is complete.
fn next_sse_event_bytes(buf: &[u8]) -> usize {
    let mut line_start = 0;
    for (i, &c) in buf.iter().enumerate() {
        if c != b'\n' {
            continue;
        }
        let len = i - line_start;
        if len == 0 || (len == 1 && buf[line_start] == b'\r') {
            return i + 1;
        }
        line_start = i + 1;
    }
    0
}

/// The body of an HTTP response in buf: after "\r\n\r\n", else "\n\n".
fn body_start(buf: &[u8]) -> Option<usize> {
    find(buf, b"\r\n\r\n", 0).map(|p| p + 4).or_else(|| find(buf, b"\n\n", 0).map(|p| p + 2))
}

fn ok_200(buf: &[u8]) -> bool {
    buf.starts_with(b"HTTP/1.1 200") || buf.starts_with(b"HTTP/1.0 200")
}

impl Datad {
    pub fn new(cfg: Cfg) -> Datad {
        Datad {
            cfg,
            sse: None,
            sse_parse_fail_streak: 0,
            next_retry_ms: 0,
            sse_buf: Vec::new(),
            live_json: Vec::new(),
            live_data: empty(),
            live_valid: false,
            live_version: 0,
            committed_live_version: 0,
            current_data: empty(),
            current_valid: false,
            alive_ms: 0,
            alive_wall: 0,
            silent: false,
            epoch: String::new(),
            seq: 0,
            have_seq: false,
            resync: false,
            blk: vec![Blk::default(); B_COUNT],
            ctl: Vec::new(),
            notice: data_notice_t::default(),
            op_seq: 0,
            pace_sent: -1,
            sms_arm_id: -1,
            sms_arm_ms: 0,
        }
    }

    fn mark_alive(&mut self) {
        self.alive_ms = mono_ms32().max(1);
        self.alive_wall = wall();
        self.silent = false;
    }

    /// Own clock on purpose; latched until datad says something again, so a very long outage
    /// cannot wrap the counter back into "live".
    fn backend_silent(&mut self) -> bool {
        if self.alive_ms == 0 {
            return false;
        }
        if !self.silent && mono_ms32().wrapping_sub(self.alive_ms) > self.cfg.silent_ms {
            self.silent = true;
        }
        self.silent
    }

    fn close_sse_stream(&mut self) {
        self.sse = None;
        self.sse_buf.clear();
        self.sse_parse_fail_streak = 0;
        self.have_seq = false;
        self.resync = false;
        self.next_retry_ms = mono_ms32().wrapping_add(self.cfg.retry_ms);
    }

    // ---- /v2 ----

    /// blk = {"stale":..,"data":..} (a snapshot entry or a block event). false = malformed.
    fn v2_set_block(&mut self, i: usize, blk: &[u8]) -> bool {
        let (Some(stale), Some(data)) = (json::get(blk, "stale", 8), json::get(blk, "data", STATE_BUF_MAX)) else {
            return false;
        };
        let b = &mut self.blk[i];
        b.fresh = stale == b"false" && data.first() == Some(&b'{');
        if b.fresh {
            b.data = data;
        }
        true
    }

    /// The fresh blocks as one old-/state object: signal → net, charger → power, live →
    /// system + traffic, sms + sms_list → sms {unread, list}. None when it would not fit.
    fn v2_rebuild(&self) -> Option<Vec<u8>> {
        const SAME: [(usize, &str); 12] = [
            (B_SIGNAL, "net"),
            (B_BATTERY, "battery"),
            (B_CHARGER, "power"),
            (B_SIM, "sim"),
            (B_QOS, "qos"),
            (B_CLIENTS, "clients"),
            (B_WLAN, "wlan"),
            (B_NFC, "nfc"),
            (B_DHCP, "dhcp"),
            (B_INTERFACES, "interfaces"),
            (B_TYPEC, "typec"),
            (B_POWERBANK, "powerbank"),
        ];
        let mut out: Vec<u8> = b"{\"v2\":1".to_vec();
        let mut cat = |parts: &[&[u8]]| {
            for p in parts {
                out.extend_from_slice(p);
            }
        };
        for (b, key) in SAME {
            if self.blk[b].fresh {
                cat(&[b",\"", key.as_bytes(), b"\":", &self.blk[b].data]);
            }
        }
        if self.blk[B_LIVE].fresh {
            if let Some(sub) = json::get(&self.blk[B_LIVE].data, "system", STATE_BUF_MAX) {
                cat(&[b",\"system\":", &sub]);
            }
            if let Some(sub) = json::get(&self.blk[B_LIVE].data, "traffic", STATE_BUF_MAX) {
                cat(&[b",\"traffic\":", &sub]);
            }
        }
        if self.blk[B_SMS].fresh || self.blk[B_SMS_LIST].fresh {
            let unread = if self.blk[B_SMS].fresh { json::get_int(&self.blk[B_SMS].data, "unread", 0) } else { 0 };
            cat(&[b",\"sms\":{\"unread\":", unread.to_string().as_bytes()]);
            if self.blk[B_SMS_LIST].fresh
                && let Some(sub) = json::get(&self.blk[B_SMS_LIST].data, "list", STATE_BUF_MAX)
            {
                cat(&[b",\"list\":", &sub]);
            }
            cat(&[b"}"]);
        }
        cat(&[b"}"]);
        (out.len() < STATE_BUF_MAX).then_some(out)
    }

    /// 1 changed, 0 the same, -1 failed
    fn v2_apply_blocks(&mut self) -> i32 {
        match self.v2_rebuild() {
            Some(c) => self.apply_snapshot_json(&c),
            None => -1,
        }
    }

    /// A snapshot (an SSE "snapshot" event or the GET /v2/state body): every block afresh.
    /// stream = also its epoch and cut seq.
    pub fn v2_apply_snapshot(&mut self, j: &[u8], stream: bool) -> i32 {
        let Some(blocks) = json::get(j, "blocks", STATE_BUF_MAX).filter(|b| b.first() == Some(&b'{')) else { return -1 };
        for (i, name) in V2_NAMES.iter().enumerate() {
            self.blk[i].fresh = false;
            if let Some(blk) = json::get(&blocks, name, STATE_BUF_MAX)
                && !self.v2_set_block(i, &blk)
            {
                return -1;
            }
        }
        if stream {
            let Some(e) = json::get(j, "epoch", 40) else { return -1 };
            self.epoch = text(&e);
            self.seq = json::get_int(j, "seq", 0) as u64;
            self.have_seq = true;
        }
        self.v2_apply_blocks()
    }

    /// A block or heartbeat: the next seq of the same epoch, or resync (the only way back in
    /// step is a new connection's snapshot).
    fn v2_in_step(&mut self, j: &[u8]) -> bool {
        let seq = json::get_int(j, "seq", -1);
        let ep = json::get(j, "epoch", 40);
        if !self.have_seq || seq < 0 || ep.is_none_or(|e| e != self.epoch.as_bytes()) || seq as u64 != self.seq.wrapping_add(1) {
            self.resync = true;
            return false;
        }
        self.seq = seq as u64;
        true
    }

    fn v2_apply_event(&mut self, name: &str, j: &[u8]) -> i32 {
        if name == "snapshot" {
            return self.v2_apply_snapshot(j, true);
        }
        if name != "block" && name != "heartbeat" {
            return 0;
        }
        if !self.v2_in_step(j) || name == "heartbeat" {
            return 0;
        }
        let Some(block) = json::get(j, "name", 24) else { return -1 };
        match V2_NAMES.iter().position(|&n| n.as_bytes() == block.as_slice()) {
            Some(i) => {
                if self.v2_set_block(i, j) {
                    self.v2_apply_blocks()
                } else {
                    -1
                }
            }
            None => 0, // a block the screen does not read
        }
    }

    fn apply_snapshot_json(&mut self, j: &[u8]) -> i32 {
        let clean = trim_ws(j);
        if clean.is_empty() || clean.len() >= STATE_BUF_MAX {
            return -1;
        }
        if clean == self.live_json.as_slice() {
            return 0;
        }
        let Some(parsed) = parse_snapshot(clean) else { return -1 };
        self.live_json = clean.to_vec();
        self.live_data = parsed;
        self.live_valid = true;
        self.live_version += 1;
        1
    }

    // ---- transport ----

    fn fetch_state_http(&mut self) -> i32 {
        let Some(fd) = http::connect_tcp(&self.cfg.host, self.cfg.port, self.cfg.io_ms) else { return -1 };
        let req = format!("GET /v2/state HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n", self.cfg.host, self.cfg.port);
        if !http::send(fd.as_raw_fd(), req.as_bytes()) {
            return -1;
        }
        let mut resp = Vec::new();
        loop {
            if resp.len() + 1 >= HTTP_BUF_MAX {
                return -1;
            }
            if wait(fd.as_raw_fd(), false, self.cfg.io_ms) <= 0 {
                return -1;
            }
            let room = HTTP_BUF_MAX - 1 - resp.len();
            let n = read_into(fd.as_raw_fd(), &mut resp, room);
            if n == 0 {
                break;
            }
            if n < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return -1;
            }
        }
        drop(fd);
        if !ok_200(&resp) {
            return -1;
        }
        let Some(b) = body_start(&resp) else { return -1 };
        self.mark_alive();
        let body = resp[b..].to_vec();
        self.v2_apply_snapshot(&body, false)
    }

    /// One event's lines: "event:" names it, "data:" lines join with '\n'.
    pub fn process_sse_event(&mut self, buf: &[u8]) -> i32 {
        let mut name: Vec<u8> = Vec::new();
        let mut payload: Vec<u8> = Vec::new();
        let mut line_start = 0;
        for (i, &c) in buf.iter().enumerate() {
            if c != b'\n' {
                continue;
            }
            let mut line = &buf[line_start..i];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if let Some(v) = line.strip_prefix(b"event:") {
                let v = &v[v.iter().position(|&c| c != b' ').unwrap_or(v.len())..];
                name = v[..v.len().min(31)].to_vec();
            } else if let Some(v) = line.strip_prefix(b"data:") {
                let v = &v[v.iter().position(|&c| c != b' ').unwrap_or(v.len())..];
                if !payload.is_empty() && payload.len() + 1 < STATE_BUF_MAX {
                    payload.push(b'\n');
                }
                if payload.len() + v.len() >= STATE_BUF_MAX {
                    return 0;
                }
                payload.extend_from_slice(v);
            }
            line_start = i + 1;
        }
        if payload.is_empty() || name.is_empty() {
            return 0;
        }
        let rc = self.v2_apply_event(&text(&name), &payload);
        if rc < 0 {
            self.sse_parse_fail_streak += 1;
            eprintln!("devui: SSE snapshot parse failed ({}/{SSE_MAX_PARSE_FAIL})", self.sse_parse_fail_streak);
            return 0;
        }
        self.sse_parse_fail_streak = 0;
        rc
    }

    fn process_sse_buffer(&mut self) -> bool {
        let mut consumed = 0;
        let mut changed = false;
        loop {
            let n = next_sse_event_bytes(&self.sse_buf[consumed..]);
            if n == 0 {
                break;
            }
            let ev = self.sse_buf[consumed..consumed + n].to_vec();
            changed |= self.process_sse_event(&ev) > 0;
            consumed += n;
            if self.resync {
                break; // the rest is out of step too
            }
        }
        if consumed > 0 {
            self.sse_buf.drain(..consumed);
        }
        changed
    }

    fn open_sse_stream(&mut self) -> bool {
        let Some(fd) = http::connect_tcp(&self.cfg.host, self.cfg.port, self.cfg.io_ms) else { return false };
        let req = format!(
            "GET /v2/events HTTP/1.1\r\nHost: {}:{}\r\nAccept: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
            self.cfg.host, self.cfg.port
        );
        if !http::send(fd.as_raw_fd(), req.as_bytes()) {
            return false;
        }
        let mut hdr = Vec::new();
        loop {
            if hdr.len() + 1 >= 16384 {
                return false;
            }
            if wait(fd.as_raw_fd(), false, self.cfg.io_ms) <= 0 {
                return false;
            }
            let room = 16384 - 1 - hdr.len();
            let n = read_into(fd.as_raw_fd(), &mut hdr, room);
            if n <= 0 {
                if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return false;
            }
            if let Some(b) = body_start(&hdr) {
                if !ok_200(&hdr) {
                    return false;
                }
                // SAFETY: fcntl on our fd
                let ok = unsafe {
                    let f = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL, 0);
                    f >= 0 && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, f | libc::O_NONBLOCK) >= 0
                };
                if !ok {
                    return false;
                }
                self.sse = Some(fd);
                self.sse_buf = hdr[b..].to_vec();
                if self.sse_buf.len() >= SSE_BUF_MAX {
                    self.close_sse_stream();
                    return false;
                }
                self.next_retry_ms = 0;
                self.pace_sent = -1; // maybe a restarted datad: tell it the pace again
                self.mark_alive();
                return true;
            }
        }
    }

    /// A live but unparseable stream: drop it so the poll falls back to HTTP.
    fn sse_drop_if_unparseable(&mut self) -> bool {
        if self.sse.is_none() || self.sse_parse_fail_streak < SSE_MAX_PARSE_FAIL {
            return false;
        }
        eprintln!("devui: dropping SSE after {} consecutive parse failures; falling back to HTTP polling", self.sse_parse_fail_streak);
        self.close_sse_stream();
        true
    }

    /// Out of step with the stream: reconnect now; the new connection starts with a snapshot.
    fn sse_drop_if_out_of_step(&mut self) -> bool {
        if self.sse.is_none() || !self.resync {
            return false;
        }
        eprintln!("devui: /v2 stream out of step (epoch or seq), reconnecting");
        self.close_sse_stream();
        self.next_retry_ms = 0;
        true
    }

    fn drain_sse_stream(&mut self) -> bool {
        let mut changed = self.process_sse_buffer();
        if self.sse_drop_if_unparseable() || self.sse_drop_if_out_of_step() {
            return changed;
        }
        while let Some(fd) = self.sse.as_ref().map(|f| f.as_raw_fd()) {
            if self.sse_buf.len() + 1 >= SSE_BUF_MAX {
                self.close_sse_stream();
                break;
            }
            let room = SSE_BUF_MAX - 1 - self.sse_buf.len();
            let n = read_into(fd, &mut self.sse_buf, room);
            if n > 0 {
                self.mark_alive(); // snapshots and keep-alive comments alike
                changed |= self.process_sse_buffer();
                if self.sse_drop_if_unparseable() || self.sse_drop_if_out_of_step() {
                    break;
                }
                continue;
            }
            if n == 0 {
                self.close_sse_stream();
                break;
            }
            let e = std::io::Error::last_os_error().raw_os_error();
            if e == Some(libc::EINTR) {
                continue;
            }
            if e == Some(libc::EAGAIN) || e == Some(libc::EWOULDBLOCK) {
                break;
            }
            self.close_sse_stream();
            break;
        }
        changed
    }

    /// Start the transport and take the first snapshot if there is one (data_backend_init).
    pub fn init(&mut self) -> bool {
        let _ = self.fetch_state_http();
        if self.live_valid {
            self.commit_latest();
        }
        if self.sse.is_none() && !self.open_sse_stream() {
            self.next_retry_ms = mono_ms32().wrapping_add(self.cfg.retry_ms);
        }
        self.current_valid
    }

    /// The SSE socket (the dark loop waits on it).
    pub fn fd(&self) -> i32 {
        self.sse.as_ref().map_or(-1, |f| f.as_raw_fd())
    }

    pub fn alive_wall(&self) -> i64 {
        self.alive_wall
    }

    /// datad answered before but has said nothing for silent_ms.
    pub fn silent(&mut self) -> bool {
        self.backend_silent()
    }

    /// Bumps with each committed snapshot.
    pub fn version(&self) -> u64 {
        self.committed_live_version
    }

    fn due(&self, now: u32) -> bool {
        // 0 = due; the signed difference survives the 32-bit ms wrap
        self.next_retry_ms == 0 || now.wrapping_sub(self.next_retry_ms) as i32 >= 0
    }

    /// Drain the stream and reconnect as needed (every main-loop pass). true = the live
    /// snapshot changed.
    pub fn poll(&mut self, now: u32) -> bool {
        self.control_reap(now);
        let mut changed = false;
        // never had a snapshot: retry on next_retry_ms, not on every pass
        if !self.current_valid && !self.live_valid && self.due(now) {
            self.init();
        }
        if self.sse.is_some() {
            changed |= self.drain_sse_stream();
            // open but silent past the keep-alive: reconnect instead of waiting on it
            if self.sse.is_some() && self.backend_silent() {
                eprintln!("devui: SSE silent for {} ms, reconnecting", mono_ms32().wrapping_sub(self.alive_ms));
                self.close_sse_stream();
                self.next_retry_ms = now;
            }
            // gave up on an unparseable stream: one HTTP snapshot now
            if self.sse.is_none() && self.fetch_state_http() > 0 {
                changed = true;
            }
        }
        if self.sse.is_none() && self.due(now) {
            if self.open_sse_stream() {
                changed |= self.drain_sse_stream();
            } else {
                if self.fetch_state_http() > 0 {
                    changed = true;
                }
                // a datad that accepts but never answers costs two I/O timeouts per try
                let gap = if self.backend_silent() { self.cfg.silent_retry_ms } else { self.cfg.retry_ms };
                self.next_retry_ms = now.wrapping_add(gap);
            }
        }
        changed
    }

    /// Promote the latest live snapshot to the one the screen shows.
    pub fn commit_latest(&mut self) -> bool {
        if !self.live_valid || (self.current_valid && self.committed_live_version == self.live_version) {
            return false;
        }
        self.current_data = self.live_data.clone();
        self.current_valid = true;
        self.committed_live_version = self.live_version;
        true
    }

    pub fn close(&mut self) {
        self.sse = None;
        self.sse_buf.clear();
    }

    /// data_refresh's attempt: no snapshot yet, try now (a blocking connect, as C).
    pub fn refresh_init(&mut self) {
        if !self.current_valid {
            self.init();
        }
    }

    /// The snapshot the screen shows (data_refresh, without its connect attempt).
    pub fn refresh(&self) -> (bool, devui_data_t) {
        if !self.current_valid {
            return (false, empty());
        }
        let mut d = self.current_data.clone();
        if force_hsr() {
            d.hsr = 1;
        }
        (true, d)
    }

    /// The card the screen shows (the committed snapshot's sim.iccid), "" without one.
    pub fn sim_iccid(&self) -> &str {
        if self.current_valid { &self.current_data.sim_iccid } else { "" }
    }

    /// The card's key for "once per card" reads: the ICCID, else "imsi:" + IMSI (after a hot
    /// swap or an eSIM switch the vendor's sim_iccid can stay empty, 2026-10-11), "" without one.
    pub fn sim_key(&self) -> String {
        let d = &self.current_data;
        match (self.current_valid, d.sim_iccid.is_empty(), d.sim_imsi.is_empty()) {
            (false, _, _) | (true, true, true) => String::new(),
            (true, false, _) => d.sim_iccid.clone(),
            (true, true, false) => format!("imsi:{}", d.sim_imsi),
        }
    }

    /// The newest snapshot even if not committed (data_refresh_live).
    pub fn refresh_live(&self) -> (bool, devui_data_t) {
        if !self.live_valid {
            return (false, empty());
        }
        let mut d = self.live_data.clone();
        if force_hsr() {
            d.hsr = 1;
        }
        (true, d)
    }

    // ---- /control ----

    /// The write datad turned down: keep one line for the screen. 409 carries datad's own
    /// sentence; a refused write that still started a transaction ("op" in the reply) says
    /// nothing here (the transaction row shows how it ends).
    fn control_judge(&mut self, s: &Slot) {
        let k = logic::control_reply(&s.reply);
        if k == Ctl::Ok {
            return;
        }
        if k == Ctl::NoReply {
            eprintln!("ui: control {}: closed without an answer", s.action);
            return;
        }
        let body = find(&s.reply, b"\r\n\r\n", 0).map_or(&b""[..], |p| &s.reply[p + 4..]);
        if k == Ctl::Failed && find(body, b"\"op\":{", 0).is_some() {
            return;
        }
        let kind = match k {
            Ctl::Busy => 1,
            Ctl::Full => 2,
            _ => 3,
        };
        self.notice = data_notice_t { kind, action: trunc(&s.action, 40), at: mono_ms32(), ..Default::default() };
        if k == Ctl::Busy
            && let Some(doing) = json::get(body, "doing", 1024)
        {
            self.notice.say_zh = jget(&doing, "say_zh", 160);
            self.notice.say_en = jget(&doing, "say_en", 160);
        }
        eprintln!("ui: control {}: datad said {}", s.action, text(&s.reply[..s.reply.len().min(12)]));
    }

    /// Close the parked requests whose answer is in, or that waited CONTROL_LINGER_MS.
    fn control_reap(&mut self, now: u32) {
        let mut i = 0;
        while i < self.ctl.len() {
            let fd = self.ctl[i].fd.as_raw_fd();
            let mut done = false;
            while wait(fd, false, 0) > 0 {
                let s = &mut self.ctl[i];
                let room = CONTROL_REPLY_MAX - 1 - s.reply.len();
                let at = s.reply.len();
                s.reply.resize(at + room, 0);
                // SAFETY: room bytes reserved past `at`
                let n = unsafe { libc::recv(fd, s.reply[at..].as_mut_ptr().cast(), room, libc::MSG_DONTWAIT) };
                s.reply.truncate(at + n.max(0) as usize);
                if n > 0 {
                    if s.reply.len() >= CONTROL_REPLY_MAX - 1 {
                        done = true;
                        break;
                    }
                    continue;
                }
                let e = std::io::Error::last_os_error().raw_os_error();
                if n < 0 && (e == Some(libc::EAGAIN) || e == Some(libc::EWOULDBLOCK) || e == Some(libc::EINTR)) {
                    break;
                }
                done = true; // closed (0) or an error
                break;
            }
            if done {
                let s = self.ctl.remove(i);
                self.control_judge(&s);
                continue;
            }
            if now.wrapping_sub(self.ctl[i].t) >= CONTROL_LINGER_MS {
                // no answer yet: datad may still be doing it; nothing runs directly (twice could
                // register the modem twice)
                eprintln!("ui: control {}: no reply in {CONTROL_LINGER_MS} ms", self.ctl[i].action);
                self.ctl.remove(i);
                continue;
            }
            i += 1;
        }
    }

    fn control_park(&mut self, fd: OwnedFd, action: &str) {
        if self.ctl.len() >= CONTROL_SLOTS {
            // all busy: the oldest has had its chance
            let mut oldest = 0;
            for (i, s) in self.ctl.iter().enumerate() {
                if (s.t.wrapping_sub(self.ctl[oldest].t) as i32) < 0 {
                    oldest = i;
                }
            }
            self.ctl.remove(oldest);
        }
        self.ctl.push(Slot { fd, t: mono_ms32(), action: trunc(action, 40), reply: Vec::new() });
    }

    /// POST /control from the screen (source, op_id); true = it reached datad (parked).
    pub fn control_send_ex(&mut self, action: &str, params: &str, undo: bool) -> bool {
        let Some(fd) = http::connect_tcp(&self.cfg.host, self.cfg.port, 800) else { return false };
        self.op_seq += 1;
        let body = trunc(
            &format!(
                "{{\"action\":\"{action}\",\"source\":\"screen\",\"op_id\":\"screen-{}-{}\",{}\"params\":{params}}}",
                std::process::id(),
                self.op_seq,
                if undo { "\"undo\":true," } else { "" }
            ),
            SMS_MAX * 12 + 160,
        );
        let req = trunc(
            &format!(
                "POST /control HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                self.cfg.host,
                self.cfg.port,
                body.len()
            ),
            SMS_MAX * 12 + 512,
        );
        if !http::send(fd.as_raw_fd(), req.as_bytes()) {
            return false;
        }
        self.control_park(fd, action);
        true
    }

    /// datad first; not reachable at all: the emergency script (it checks again that datad is
    /// gone, D18). Never on 503, 409 or no answer: datad may still be doing it.
    pub fn control_fb(&mut self, action: &str, params: &str, fb_action: &str, fb_args: Option<&str>) -> bool {
        if self.control_send_ex(action, params, false) {
            return true;
        }
        if fb_args.is_none() {
            return false;
        }
        match logic::control_fallback_cmd(FALLBACK_SCRIPT, fb_action, fb_args, 512) {
            Some(cmd) => {
                eprintln!("ui: control {action}: datad unreachable, emergency script {fb_action}");
                let _ = std::process::Command::new("/bin/sh").arg("-c").arg(cmd).status();
            }
            None => eprintln!("ui: control {action}: datad unreachable, bad emergency arguments"),
        }
        false
    }

    /// The last write datad turned down, if younger than max_age_ms.
    pub fn notice(&self, max_age_ms: u32) -> Option<data_notice_t> {
        (self.notice.at != 0 && mono_ms32().wrapping_sub(self.notice.at) < max_age_ms).then(|| self.notice.clone())
    }

    /// datad's polling pace: 1 s lit, 5 s dark. Sent on a change only, and again after every
    /// reconnect (a restarted datad is back at 1 s); never on a timer: each call makes datad
    /// drop its slow-data cache.
    pub fn set_pace(&mut self, panel_lit: bool) {
        let want = if panel_lit { PACE_LIT_MS } else { PACE_DARK_MS };
        if want == self.pace_sent {
            return;
        }
        self.control_send_ex("state.set_interval", &format!("{{\"milliseconds\":{want}}}"), false);
        self.pace_sent = want;
    }

    // ---- SMS ----

    fn sms(&self) -> &[devui_data_t_sms] {
        &self.current_data.sms[..(self.current_data.sms_n.max(0) as usize).min(self.current_data.sms.len())]
    }

    pub fn sms_mark_read_id(&mut self, id: i64) -> bool {
        let Some(m) = self.sms().iter().find(|m| m.id == id) else { return false };
        if m.unread != 0 {
            self.control_send_ex("sms.mark_read", &format!("{{\"ids\":\"{id};\",\"tag\":0}}"), false);
        }
        true
    }

    /// One request for all of them ("a;b;c;"); returns how many were unread.
    pub fn sms_mark_all_read(&mut self) -> i32 {
        let mut ids = String::new();
        let mut n = 0;
        for m in self.sms().iter().filter(|m| m.unread != 0) {
            let w = format!("{};", m.id);
            if ids.len() + w.len() > SMS_MAX * 12 {
                break;
            }
            ids.push_str(&w);
            n += 1;
        }
        if n == 0 {
            return 0;
        }
        self.control_send_ex("sms.mark_read", &format!("{{\"ids\":\"{ids}\",\"tag\":0}}"), false);
        n
    }

    pub fn sms_delete_id(&mut self, id: i64) -> bool {
        if id < 0 {
            return false;
        }
        self.control_send_ex("sms.delete", &format!("{{\"ids\":\"{id};\"}}"), false);
        true
    }

    /// A long press arms the row (by message id: a re-read list does not move it).
    pub fn sms_delete_arm(&mut self, i: usize) -> bool {
        let Some(id) = self.sms().get(i).map(|m| m.id) else { return false };
        self.sms_arm_id = id;
        self.sms_arm_ms = mono_ms32();
        true
    }

    pub fn sms_delete_armed(&self, i: usize) -> bool {
        self.sms().get(i).is_some_and(|m| m.id == self.sms_arm_id && mono_ms32().wrapping_sub(self.sms_arm_ms) <= SMS_ARM_MS)
    }

    /// A tap on the armed row after the debounce (the long press's own release is not one).
    pub fn sms_delete_tap(&mut self, i: usize) -> bool {
        let Some(id) = self.sms().get(i).map(|m| m.id) else { return false };
        if self.sms_arm_id != id {
            return false;
        }
        let el = mono_ms32().wrapping_sub(self.sms_arm_ms);
        if el <= SMS_ARM_DEBOUNCE_MS || el > SMS_ARM_MS {
            return false;
        }
        self.sms_arm_id = -1;
        self.control_send_ex("sms.delete", &format!("{{\"ids\":\"{id};\"}}"), false);
        true
    }

    /// The armed row's index, -1 none (the view model's sms_armed).
    pub fn sms_armed_index(&self) -> i32 {
        (0..self.sms().len()).rev().find(|&i| self.sms_delete_armed(i)).map_or(-1, |i| i as i32)
    }

    /// The /control requests waiting for an answer (tests).
    #[cfg(test)]
    pub fn parked(&self) -> usize {
        self.ctl.len()
    }
}

#[cfg(test)]
mod tests;
