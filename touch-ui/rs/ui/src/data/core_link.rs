//! The screen's line to luyaz-core. Reads: the /v2 snapshot (GET /v2/state) and then the
//! /v2/events stream (SSE), its blocks put back together in the old /state shape so one parser
//! reads both; a new epoch or a gap in seq means reconnecting, stale blocks are left out, and 45 s
//! without a byte means core is gone. Writes: POST /control, each socket parked until core
//! answers (a client that hangs up first loses the request: core drops it before the handler
//! runs, measured 2026-09-23).

use super::http::{self, read_into, wait};
use super::util::{find, jget, jstr, mono_ms32, text, trunc, wall};
use crate::json;
use crate::lang::tr;
use crate::logic::{self, Ctl};
use ui_model::{data_notice_t, core_snapshot_t, display_t, lan_client_t, regress_item_t, regress_t, sms_entry_t, sim_dual_t, sim_slot_t};
use std::os::fd::{AsRawFd, OwnedFd};

pub const INBOX_CAP: usize = 32;
pub const SMS_BODY_CAP: usize = 16384;
const SMS_FETCH_CAP: usize = 1048576;
const SNAPSHOT_CAP: usize = 1048576;
const REPLY_CAP: usize = SNAPSHOT_CAP + 8192;
const STREAM_CAP: usize = SNAPSHOT_CAP + 8192;
const SMS_ENTRY_CAP: usize = SMS_BODY_CAP + 32768;
/// consecutive unparseable SSE events before the stream is dropped for HTTP polling
const BAD_EVENTS_LIMIT: u32 = 5;

const CONTROL_SLOTS: usize = 8;
const CONTROL_LINGER_MS: u32 = 5000;
const CONTROL_REPLY_MAX: usize = 2048;
const USB_CONTROL_REPLY_MAX: usize = 8192; // includes Core's bounded capability explanation

fn usb_refusal(body: &[u8]) -> Option<(&'static str, &'static str)> {
    let error = json::get(body, "error", USB_CONTROL_REPLY_MAX)?;
    match jget(&error, "code", 80).as_str() {
        "capability_unverified" => Some(("USB 用法尚未验证 · 请手动确认", "USB use is unverified; confirm it manually")),
        "firmware_unsupported" | "usb_method_absent" => Some(("这版固件不支持此 USB 操作", "This firmware does not support this USB operation")),
        "capability_blocked" => Some(("当前能力报告阻止此 USB 操作", "The current capability report blocks this USB operation")),
        "capability_context_changed" => Some(("USB 状态已改变 · 请重新确认", "USB state changed; confirm again")),
        "usb_preflight_unavailable" => Some(("USB 检查未完成 · 没有发送切换", "USB preflight did not complete; no change was sent")),
        "usb_not_attached" => Some(("请先在 USB-C 口插上设备", "Plug a device into the USB-C port first")),
        _ => None,
    }
}
/// The emergency write script (Luya-Z rename D1/D4).
const FALLBACK_SCRIPT: &str = "/data/luyaz/scripts/luyaz-fallback.sh";
/// = start.sh's -i 1000; 5000 = the core's upper limit
const PACE_LIT_MS: i32 = 1000;
const PACE_DARK_MS: i32 = 5000;
/// the pace is a want that lapses in core after PACE_TTL_S (a UI that dies lit does not leave
/// core at 1 s); sent again every PACE_RENEW_MS while it holds
const PACE_TTL_S: u32 = 180;
const PACE_RENEW_MS: u32 = 60_000;
const SMS_ARM_MS: u32 = 4000;
const SMS_ARM_DEBOUNCE_MS: u32 = 400;

/// Where core is and how patient to be (tests shorten the waits).
#[derive(Clone, Debug)]
pub struct Cfg {
    pub host: String,
    pub port: u16,
    pub retry_ms: u32,
    /// no byte at all for this long (core heartbeats every 5 s) = gone
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
const B_REGRESS: usize = 15;
const B_DISPLAY: usize = 16;
const B_HEAT: usize = 17;
const B_COUNT: usize = 18;
const V2_NAMES: [&str; B_COUNT] =
    ["signal", "battery", "charger", "live", "sms", "sms_list", "sim", "qos", "clients", "wlan", "nfc", "dhcp", "interfaces", "typec", "powerbank", "regress", "display", "heat"];

#[derive(Default, Clone)]
struct Blk {
    data: Vec<u8>,
    fresh: bool,
    /// the core has this block (an older core does not: the regress block)
    present: bool,
    /// data came fresh at least once, and the device wall clock when it last did
    has: bool,
    at: i64,
}

/// A /control request waiting for the core's answer.
struct Slot {
    fd: OwnedFd,
    t: u32,
    action: String,
    reply: Vec<u8>,
}

type Contact = (Option<Vec<u8>>, Option<(OwnedFd, Vec<u8>)>);

pub struct CoreLink {
    background: bool,
    contact: Option<super::background::Task<Contact>>,
    pub cfg: Cfg,
    sse: Option<OwnedFd>,
    bad_events: u32,
    pub retry_at_ms: u32,
    stream_buf: Vec<u8>,
    latest_raw: Vec<u8>,
    pub fresh_snap: core_snapshot_t,
    latest_ok: bool,
    latest_gen: u64,
    shown_gen: u64,
    shown_snap: core_snapshot_t,
    shown_ok: bool,
    /// the last byte from core (monotonic ms / wall s); 0 = never
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
    pace_at: u32,
    sms_arm_id: i64,
    sms_arm_ms: u32,
}

impl Default for CoreLink {
    fn default() -> Self {
        CoreLink::new(Cfg::default())
    }
}

// ---- reading the old /state shape ----

/// The mainland operators, named for the screen whatever the network calls itself.
#[derive(Clone, Copy)]
enum Mainland {
    Mobile,
    Unicom,
    Telecom,
    Broadnet,
}

impl Mainland {
    /// By PLMN (460-xx) first, then by the names the network is known to send.
    fn of(mcc: i32, mnc: i32, raw: &str) -> Option<Mainland> {
        let by_plmn = match (mcc, mnc) {
            (460, 0 | 2 | 4 | 7 | 8 | 13) => Some(Mainland::Mobile),
            (460, 1 | 6 | 9) => Some(Mainland::Unicom),
            (460, 3 | 5 | 11) => Some(Mainland::Telecom),
            (460, 15) => Some(Mainland::Broadnet),
            _ => None,
        };
        by_plmn.or(match raw {
            "China Mobile" | "CMCC" => Some(Mainland::Mobile),
            "China Unicom" | "CUCC" => Some(Mainland::Unicom),
            "China Telecom" | "CTCC" => Some(Mainland::Telecom),
            "China Broadnet" | "China Broadcasting Network" => Some(Mainland::Broadnet),
            _ => None,
        })
    }

    /// English names from manager docs/ui-glossary.md §8.
    fn name(self) -> &'static str {
        tr(match self {
            Mainland::Mobile => "中国移动",
            Mainland::Unicom => "中国联通",
            Mainland::Telecom => "中国电信",
            Mainland::Broadnet => "中国广电",
        })
    }

    /// operator-logos/<stem>.png; the core's operator_logo uses the same 460 rows.
    fn logo(self) -> &'static str {
        match self {
            Mainland::Mobile => "china-mobile",
            Mainland::Unicom => "china-unicom",
            Mainland::Telecom => "china-telecom",
            Mainland::Broadnet => "china-broadnet",
        }
    }
}

/// The name the screen shows for a mainland operator, or None to show the network's own.
pub(crate) fn mainland_operator_cn(mcc: i32, mnc: i32, raw: &str) -> Option<&'static str> {
    Mainland::of(mcc, mnc, raw).map(Mainland::name)
}

/// A dual slot's 本地 / 漫游到X in the UI language ("" unknown, or an older core).
pub(crate) fn slot_place(s: &ui_model::sim_slot_t) -> &str {
    if crate::lang::is_en() && !s.place_en.is_empty() { &s.place_en } else { &s.place }
}

/// The logo for a mainland operator (the standby slot has no screen view).
pub(crate) fn mainland_logo(mcc: i32, mnc: i32, raw: &str) -> Option<&'static str> {
    Mainland::of(mcc, mnc, raw).map(Mainland::logo)
}

/// A switch written as text: 1, true, yes or on in the usual casings.
fn switched_on(v: &str) -> bool {
    ["1", "true", "True", "TRUE", "yes", "YES", "on", "ON"].contains(&v)
}

/// The HSR (high-speed rail) badge forced on for testing: the env var or the flag file.
fn force_hsr() -> bool {
    std::env::var("LUYAZ_UI_FORCE_HSR").is_ok_and(|v| switched_on(&v)) || std::path::Path::new("/tmp/luyaz/force-hsr").exists()
}

fn num(o: &[u8], key: &str, def: i64) -> i64 {
    json::get_int(o, key, def)
}

fn small(o: &[u8], key: &str) -> i32 {
    num(o, key, 0) as i32
}

fn read_net(s: &[u8], d: &mut core_snapshot_t) {
    for (field, key, cap) in [
        (&mut d.radio, "type", 16),
        (&mut d.carrier, "operator", 48),
        (&mut d.roaming, "roaming", 16),
        (&mut d.band_text, "band", 16),
        (&mut d.nr_band_text, "nr_band", 16),
        (&mut d.nr_snr, "nr_snr", 12),
        (&mut d.wan_state, "wan_status", 32),
        (&mut d.lte_snr, "lte_snr", 12),
        (&mut d.bandwidth, "bandwidth", 12),
        (&mut d.operate_mode, "operate_mode", 16),
        (&mut d.nr_width, "nr_bw", 12),
        (&mut d.nr_ca, "nrca", 256),
        (&mut d.lte_ca, "lteca", 256),
        (&mut d.lte_ca_sig, "ltecasig", 256),
        (&mut d.net_pref, "net_select", 16),
        (&mut d.sa_bands, "sa_bands", 256),
        (&mut d.nsa_bands, "nsa_bands", 256),
        (&mut d.lte_bands, "lte_bands", 256),
        (&mut d.sa_sup, "nr_sa_supported_bands", 256),
        (&mut d.nsa_sup, "nr_nsa_supported_bands", 256),
        (&mut d.lte_sup, "lte_supported_bands", 256),
    ] {
        *field = jget(s, key, cap);
    }
    for (field, key) in [
        (&mut d.lte_pci, "lte_pci"),
        (&mut d.signal_level, "bars"),
        (&mut d.nr_rsrp, "nr_rsrp"),
        (&mut d.nr_rsrq, "nr_rsrq"),
        (&mut d.nr_rssi, "nr_rssi"),
        (&mut d.lte_rsrp, "lte_rsrp"),
        (&mut d.lte_rsrq, "lte_rsrq"),
        (&mut d.lte_rssi, "lte_rssi"),
        (&mut d.rssi, "rssi"),
        (&mut d.mcc, "mcc"),
        (&mut d.mnc, "mnc"),
        (&mut d.nr_pci, "nr_pci"),
    ] {
        *field = small(s, key);
    }
    d.rail_mode = json::get(s, "HSR", 16).is_some_and(|v| switched_on(&text(&v))) as i32;
    d.nr_cid = num(s, "nr_cell_id", 0);
    d.nr_arfcn = num(s, "nr_channel", 0);
    // luyaz-core says lte_cell_id / lte_channel, older cores cell_id / channel: a zero falls
    // back to the other name
    d.lte_cell_id = match num(s, "lte_cell_id", 0) {
        0 => num(s, "cell_id", 0),
        v => v,
    };
    d.channel = match num(s, "channel", 0) {
        0 => num(s, "lte_channel", 0),
        v => v,
    };
    if let Some(name) = mainland_operator_cn(d.mcc, d.mnc, &d.carrier) {
        d.carrier = trunc(name, 48);
    }
}

fn read_battery(s: &[u8], d: &mut core_snapshot_t) {
    d.batt_pct = small(s, "percent");
    d.batt_temp = small(s, "temp");
    d.batt_charging = small(s, "charging");
    d.charger_in = small(s, "charger_connect");
    d.chg_volt_uv = num(s, "chg_uv", 0);
    d.chg_cur_ua = num(s, "chg_ua", 0);
    d.batt_volt_uv = num(s, "bat_uv", 0);
    d.batt_cur_ua = num(s, "bat_ua", 0);
}

/// Longest client object kept: name, alias, vendor, kind, ip, mac and private all near their caps
/// (core keeps each short, lan-client-id.md), with room for JSON escapes; a longer one is
/// passed over. The list is read up to CLIENT_LIST_CAP (16 such objects).
const CLIENT_ENTRY_CAP: usize = 512;
const CLIENT_LIST_CAP: usize = 16 * CLIENT_ENTRY_CAP;

/// Counts plus the first 16 clients; an entry too long for the screen's row is passed over.
fn read_clients(s: &[u8], d: &mut core_snapshot_t) {
    d.lan_total = small(s, "total");
    d.lan_wifi = small(s, "wifi");
    d.lan_wired = small(s, "lan");
    let list = json::get(s, "list", CLIENT_LIST_CAP).unwrap_or_default();
    let mut n = 0;
    for o in json::objects(&list).filter(|o| o.len() < CLIENT_ENTRY_CAP).take(16) {
        d.lan[n] = lan_client_t {
            name: jget(o, "name", 40),
            ip: jget(o, "ip", 24),
            mac: jget(o, "mac", 20),
            alias: jget(o, "alias", 40),
            vendor: jget(o, "vendor", 32),
            kind: jget(o, "kind", 16),
            private: json::get(o, "private", 8).as_deref() == Some(b"true"),
        };
        n += 1;
    }
    d.lan_n = n as i32;
}

/// The unread count and up to INBOX_CAP messages; one too big to keep is passed over.
fn read_sms(s: &[u8], d: &mut core_snapshot_t) {
    d.sms_new = small(s, "unread");
    let list = json::get(s, "list", SMS_FETCH_CAP).unwrap_or_default();
    let mut n = 0;
    for o in json::objects(&list).filter(|o| o.len() < SMS_ENTRY_CAP).take(INBOX_CAP) {
        d.inbox[n] = sms_entry_t {
            id: num(o, "id", 0),
            num: jget(o, "num", 40),
            date: jget(o, "date", 16),
            text: jget(o, "text", SMS_BODY_CAP),
            unread: small(o, "unread"),
        };
        n += 1;
    }
    d.inbox_n = n as i32;
}

/// Mobile data on (1), off (0) or unknown (-1). `enable` is only the last value written: right
/// after boot it is 0 while the automatic dial is up (E4 T12, B31), so a 0 counts as off only
/// when the call is down too, as the core's data_switch() decides.
fn data_switch(cell: &[u8]) -> i32 {
    match num(cell, "enable", -1) {
        1 => 1,
        0 => match json::get(cell, "connect_status", 48).map(|v| text(&v)) {
            None => -1,
            Some(st) if st.is_empty() => -1,
            Some(st) if (st.contains("connected") && !st.contains("disconnect")) || st == "connecting" => 1,
            Some(_) => 0,
        },
        _ => -1,
    }
}

fn read_traffic(s: &[u8], d: &mut core_snapshot_t) {
    for (field, key) in [
        (&mut d.down_bps, "rx_speed"),
        (&mut d.up_bps, "tx_speed"),
        (&mut d.down_total, "rx_bytes"),
        (&mut d.up_total, "tx_bytes"),
        (&mut d.day_rx_bytes, "day_rx_bytes"),
        (&mut d.day_tx_bytes, "day_tx_bytes"),
        (&mut d.month_rx_bytes, "month_rx_bytes"),
        (&mut d.month_tx_bytes, "month_tx_bytes"),
    ] {
        *field = num(s, key, 0);
    }
}

fn read_qos(s: &[u8], d: &mut core_snapshot_t) {
    let rate = |key| json::get(s, key, 32).map_or(0.0, |v| json::atof(&v));
    d.qos_class = small(s, "qci");
    d.ambr_down = rate("ambr_dl");
    d.ambr_up = rate("ambr_ul");
    d.qos_bearer = jget(s, "bearer", 16);
    d.qos_stale = (json::get(s, "stale", 8).as_deref() == Some(b"true")) as i32;
    d.adb_mode = jget(s, "usb_mode", 16);
}

fn read_system(s: &[u8], d: &mut core_snapshot_t) {
    d.uptime_s = num(s, "uptime", 0);
    d.soc_temp = num(s, "cpu_temp", 0);
    d.cpu_pct = num(s, "cpu_usage", -1);
    d.mem_pct = num(s, "mem_used_pct", 0);
    d.mem_size = num(s, "mem_total", 0);
    d.mem_free = num(s, "mem_avail", 0);
    d.model = jget(s, "model", 64);
    d.firmware = jget(s, "fw", 80);
    d.sw_build = jget(s, "sw_version", 80);
    d.imei = jget(s, "imei", 24);
}

fn read_sim(s: &[u8], d: &mut core_snapshot_t) {
    d.sim_state = jget(s, "state", 24);
    d.sim_iccid = jget(s, "iccid", 24);
    d.sim_imsi = jget(s, "imsi", 20);
    d.sim_msisdn = jget(s, "msisdn", 24);
    d.sim_spn = jget(s, "spn", 32);
    d.sim_say = crate::lang::pick(&jget(s, "say_short", 64), &jget(s, "say_short_en", 64)).to_string();
    d.sim_dual = json::get(s, "dual", 2048).and_then(|o| sim_dual(&o));
}

type Reader = fn(&[u8], &mut core_snapshot_t);

/// A snapshot in the old /state shape, as the screen's struct. None for empty text. Sections
/// that are missing (stale blocks are left out) keep their defaults.
pub fn decode_snapshot(buf: &[u8]) -> Option<core_snapshot_t> {
    if matches!(buf.first(), None | Some(0)) {
        return None;
    }
    let mut d = core_snapshot_t { dps_mode: -1, cell_data: -1, cell_roam: -1, usb_cc: -1, powerbank: -1, heat_level: -1, ..empty() };
    let section = |key| json::get(buf, key, SNAPSHOT_CAP);
    let readers: [(&str, Reader); 8] = [
        ("net", read_net),
        ("battery", read_battery),
        ("clients", read_clients),
        ("sms", read_sms),
        ("traffic", read_traffic),
        ("qos", read_qos),
        ("system", read_system),
        ("sim", read_sim),
    ];
    for (key, read) in readers {
        if let Some(s) = section(key) {
            read(&s, &mut d);
        }
    }
    if let Some(s) = section("wlan") {
        d.ap_ssid = jget(&s, "ssid", 64);
        d.ap_psk = jget(&s, "key", 64);
        d.ap_auth = jget(&s, "enc", 24);
        d.ap_on = num(&s, "enabled", 1) as i32;
    }
    if let Some(s) = section("nfc") {
        d.nfc_on = small(&s, "switch");
    }
    if let Some(s) = section("typec") {
        d.usb_cc = num(&s, "cc_attch_state", -1) as i32;
        d.usb_power_role = jget(&s, "power_role", 12);
        d.usb_data_role = jget(&s, "data_role", 12);
    }
    if let Some(s) = section("powerbank") {
        d.powerbank = num(&s, "state", -1) as i32;
    }
    if let Some(s) = section("regress") {
        d.regress = Some(regress(&s));
    }
    if let Some(s) = section("heat") {
        d.heat_level = match json::get(&s, "level", 16).as_deref() {
            Some(b"warm") => 1,
            Some(b"hot") => 2,
            Some(b"critical") => 3,
            _ => 0,
        };
        d.heat_paused = (json::get(&s, "charge_paused", 8).as_deref() == Some(b"true")) as i32;
        // what CHILL did, not what was asked: the banner says only that
        d.heat_chill = match json::get(&s, "chill_now", 16).as_deref() {
            Some(b"hot") => 1,
            Some(b"critical") => 2,
            _ => 0,
        };
    }
    if let Some(s) = section("display") {
        d.display = Some(display(&s));
    }
    if let Some(s) = section("dhcp") {
        d.lan_ip = jget(&s, "ip", 24);
        d.dhcp_first = jget(&s, "start", 24);
        d.dhcp_count = jget(&s, "limit", 8);
        d.dhcp_lease = jget(&s, "leasetime", 12);
    }
    let supply = section("power").and_then(|p| json::get(&p, "direct_supply", 2048)).and_then(|p| json::get(&p, "mode", 16));
    match supply.as_deref() {
        Some(b"enable") => d.dps_mode = 1,
        Some(b"disable") => d.dps_mode = 0,
        _ => {}
    }
    if let Some(cell) = section("interfaces").and_then(|i| json::get(&i, "cellular", 2048)) {
        d.cell_data = data_switch(&cell);
        d.cell_roam = match num(&cell, "roam_enable", -1) {
            v @ (0 | 1) => v as i32,
            _ => -1,
        };
    }
    if force_hsr() {
        d.rail_mode = 1;
    }
    d.populated = 1;
    Some(d)
}

/// `sim.dual` (absent or null on a one-slot device).
/// The regress pseudo-section (`{fresh, has, at, data}`, built by `v2_rebuild`).
fn regress(s: &[u8]) -> regress_t {
    let flag = |k: &str| (json::get(s, k, 8).as_deref() == Some(b"true")) as i32;
    let data = json::get(s, "data", SNAPSHOT_CAP).filter(|d| d.first() == Some(&b'{'));
    let items = |k: &str| -> Vec<regress_item_t> {
        let mut v = Vec::new();
        if let Some(arr) = data.as_deref().and_then(|d| json::get(d, k, 16384)) {
            let mut p = 0;
            while let Some((o, next)) = json::arr_next(&arr, p, 2048) {
                let pick = |zh: &str, en: &str| crate::lang::pick(&jget(&o, zh, 96), &jget(&o, en, 96)).to_string();
                v.push(regress_item_t {
                    id: jstr(&o, "id", 64),
                    kind: jstr(&o, "kind", 40),
                    name: pick("name_zh", "name_en"),
                    sub: jstr(&o, "sub", 24),
                    from: json::get_int(&o, "from", 0),
                    to: json::get_int(&o, "to", 0),
                    end: jstr(&o, "end", 16),
                    count: json::get_int(&o, "count", 1) as i32,
                    observed: pick("observed_zh", "observed_en"),
                });
                p = next;
            }
        }
        v
    };
    regress_t {
        fresh: flag("fresh"),
        has: flag("has"),
        at: json::get_int(s, "at", 0),
        open_total: data.as_deref().map_or(0, |d| json::get_int(d, "open_total", 0) as i32),
        open: items("open"),
        closed: items("closed"),
    }
}

/// The display block (`{card:{id,kind,code,left_s,answered}|null, said:{id,result}|null}`):
/// only while fresh (a card from a core that went quiet is not shown).
fn display(s: &[u8]) -> display_t {
    let card = json::get(s, "card", 1024).filter(|c| c.first() == Some(&b'{'));
    let said = json::get(s, "said", 512).filter(|c| c.first() == Some(&b'{'));
    let mut d = display_t::default();
    if let Some(c) = card.as_deref() {
        d.id = jstr(c, "id", 64);
        d.kind = jstr(c, "kind", 16);
        d.code = jstr(c, "code", 8);
        d.left_s = json::get_int(c, "left_s", 0) as i32;
        d.answered = (json::get(c, "answered", 8).as_deref() == Some(b"true")) as i32;
    }
    if let Some(w) = said.as_deref() {
        d.said = jstr(w, "result", 16);
        d.said_id = jstr(w, "id", 64);
    }
    d
}

fn sim_dual(o: &[u8]) -> Option<sim_dual_t> {
    if o.first() != Some(&b'{') {
        return None;
    }
    let flag = |o: &[u8], k: &str| match json::get(o, k, 8).as_deref() {
        Some(b"true") => 1,
        Some(b"false") => 0,
        _ => -1,
    };
    let mut slots = Vec::new();
    if let Some(arr) = json::get(o, "slots", 2048) {
        let mut p = 0;
        while let Some((s, next)) = json::arr_next(&arr, p, 768) {
            let sig = json::get(&s, "signal", 256).filter(|g| g.first() == Some(&b'{'));
            let gi = |k: &str| sig.as_deref().map_or(0, |g| json::get_int(g, k, 0) as i32);
            slots.push(sim_slot_t {
                state: jstr(&s, "state", 12),
                role: jstr(&s, "role", 10),
                has_sig: sig.is_some() as i32,
                sig_rat: sig.as_deref().map_or_else(String::new, |g| jstr(g, "rat", 8)),
                sig_rsrp: gi("rsrp"),
                sig_rsrq: gi("rsrq"),
                sig_snr: sig.as_deref().map_or_else(String::new, |g| jstr(g, "snr", 8)),
                sig_bars: gi("bars"),
                slot: json::get_int(&s, "slot", 0) as i32,
                provisioned: (flag(&s, "provisioned") == 1) as i32,
                present: (flag(&s, "present") == 1) as i32,
                ready: flag(&s, "ready"),
                operator: jstr(&s, "operator", 32),
                mcc: jstr(&s, "mcc", 8),
                mnc: jstr(&s, "mnc", 8),
                iccid_tail: jstr(&s, "iccid_tail", 8),
                logo: jstr(&s, "logo", 24),
                true_roaming: flag(&s, "true_roaming"),
                place: jstr(&s, "place", 64),
                place_en: jstr(&s, "place_en", 64),
            });
            p = next;
        }
    }
    Some(sim_dual_t {
        mode: jstr(o, "mode", 16),
        smart: (flag(o, "smart") == 1) as i32,
        active_slot: json::get_int(o, "active_slot", 0) as i32,
        switching: (flag(o, "switching") == 1) as i32,
        slots,
    })
}

/// What the screen gets before there is any snapshot.
fn empty() -> core_snapshot_t {
    core_snapshot_t {
        cpu_pct: -1,
        lan: vec![lan_client_t::default(); 16],
        inbox: vec![sms_entry_t::default(); INBOX_CAP],
        ..Default::default()
    }
}

/// s without blanks at either end.
fn trim_ws(s: &[u8]) -> &[u8] {
    let keep = |c: &u8| !matches!(c, b' ' | b'\t' | b'\r' | b'\n');
    match (s.iter().position(keep), s.iter().rposition(keep)) {
        (Some(a), Some(b)) => &s[a..=b],
        _ => &[],
    }
}

/// The length of the first complete SSE event in buf, its closing blank line included (a
/// blank line may be "\r\n"); 0 while it is still arriving.
fn next_sse_event_bytes(buf: &[u8]) -> usize {
    let mut from = 0;
    while let Some(nl) = buf[from..].iter().position(|&c| c == b'\n') {
        let end = from + nl;
        if matches!(&buf[from..end], b"" | b"\r") {
            return end + 1;
        }
        from = end + 1;
    }
    0
}

/// An event's "event:" name (the last one, at most 31 bytes) and its "data:" lines joined with
/// '\n', or None when either is missing or the data would not fit in SNAPSHOT_CAP.
fn sse_fields(ev: &[u8]) -> Option<(String, Vec<u8>)> {
    let after = |line: &[u8], tag: &[u8]| -> Option<Vec<u8>> {
        let v = line.strip_prefix(tag)?;
        Some(v[v.iter().take_while(|&&c| c == b' ').count()..].to_vec())
    };
    let mut name = Vec::new();
    let mut data = Vec::new();
    let mut lines = ev.split(|&c| c == b'\n');
    lines.next_back(); // whatever follows the last newline is not a whole line
    for line in lines {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(mut v) = after(line, b"event:") {
            v.truncate(31);
            name = v;
        } else if let Some(v) = after(line, b"data:") {
            if !data.is_empty() && data.len() + 1 < SNAPSHOT_CAP {
                data.push(b'\n');
            }
            if data.len() + v.len() >= SNAPSHOT_CAP {
                return None;
            }
            data.extend_from_slice(&v);
        }
    }
    (!name.is_empty() && !data.is_empty()).then(|| (text(&name), data))
}

/// Where the body of an HTTP response starts: after the blank line ending the head.
fn body_start(buf: &[u8]) -> Option<usize> {
    [&b"\r\n\r\n"[..], b"\n\n"].iter().find_map(|sep| find(buf, sep, 0).map(|p| p + sep.len()))
}

fn ok_200(buf: &[u8]) -> bool {
    [&b"HTTP/1.1 200"[..], b"HTTP/1.0 200"].iter().any(|v| buf.starts_with(v))
}

impl CoreLink {
    pub fn new(cfg: Cfg) -> CoreLink {
        CoreLink {
            background: false,
            contact: None,
            cfg,
            sse: None,
            bad_events: 0,
            retry_at_ms: 0,
            stream_buf: Vec::new(),
            latest_raw: Vec::new(),
            fresh_snap: empty(),
            latest_ok: false,
            latest_gen: 0,
            shown_gen: 0,
            shown_snap: empty(),
            shown_ok: false,
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
            pace_at: 0,
            sms_arm_id: -1,
            sms_arm_ms: 0,
        }
    }

    /// Production reads establish both HTTP and SSE away from the LVGL thread.
    pub fn background(cfg: Cfg) -> Self {
        Self { background: true, ..Self::new(cfg) }
    }

    fn poll_background(&mut self, now: u32) -> bool {
        self.control_reap(now);
        let mut changed = false;
        if let Some(task) = &self.contact {
            match task.take() {
                Ok((state, stream)) => {
                    self.contact = None;
                    if let Some(resp) = state
                        && let Some(body) = body_start(&resp).filter(|_| ok_200(&resp))
                    {
                        self.mark_alive();
                        changed |= self.v2_apply_snapshot(&resp[body..], false) > 0;
                    }
                    if let Some((fd, head)) = stream { self.install_stream(fd, head); }
                    let wait = if self.backend_silent() { self.cfg.silent_retry_ms } else { self.cfg.retry_ms };
                    self.retry_at_ms = now.wrapping_add(wait);
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.contact = None;
                    self.retry_at_ms = now.wrapping_add(self.cfg.retry_ms);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.sse.is_some() {
            changed |= self.drain_sse_stream();
            if self.sse.is_some() && self.backend_silent() { self.close_sse_stream(); }
        }
        if self.sse.is_none() && self.contact.is_none() && self.due(now) {
            let cfg = self.cfg.clone();
            self.contact = super::background::Task::start_notify(move || {
                let link = Self::new(cfg);
                let req = format!("GET /v2/state HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n", link.cfg.host, link.cfg.port);
                let state = link.ask(&req, REPLY_CAP, true).map(|(_, body)| body);
                let req = format!("GET /v2/events HTTP/1.1\r\nHost: {}:{}\r\nAccept: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n", link.cfg.host, link.cfg.port);
                let stream = link.ask(&req, 16384, false);
                (state, stream)
            }, |_| true);
        }
        changed
    }

    fn mark_alive(&mut self) {
        self.alive_ms = mono_ms32().max(1);
        self.alive_wall = wall();
        self.silent = false;
    }

    /// Own clock on purpose; latched until core says something again, so a very long outage
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

    /// Forget the stream and its position; the next attempt waits retry_ms.
    fn close_sse_stream(&mut self) {
        self.sse = None;
        self.stream_buf.clear();
        (self.bad_events, self.have_seq, self.resync) = (0, false, false);
        self.retry_at_ms = mono_ms32().wrapping_add(self.cfg.retry_ms);
    }

    // ---- /v2 ----

    /// blk = {"stale":..,"data":..} (a snapshot entry or a block event). false = malformed.
    fn v2_set_block(&mut self, i: usize, blk: &[u8]) -> bool {
        let (Some(stale), Some(data)) = (json::get(blk, "stale", 8), json::get(blk, "data", SNAPSHOT_CAP)) else {
            return false;
        };
        let b = &mut self.blk[i];
        b.present = true;
        b.fresh = stale == b"false" && data.first() == Some(&b'{');
        if b.fresh {
            if b.data != data || !b.has {
                b.at = wall();
            }
            b.data = data;
            b.has = true;
        }
        true
    }

    /// The fresh blocks as one old-/state object: signal → net, charger → power, live →
    /// system + traffic, sms + sms_list → sms {unread, list}. None when it would not fit.
    fn v2_rebuild(&self) -> Option<Vec<u8>> {
        const SAME: [(usize, &str); 13] = [
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
            (B_HEAT, "heat"),
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
            if let Some(sub) = json::get(&self.blk[B_LIVE].data, "system", SNAPSHOT_CAP) {
                cat(&[b",\"system\":", &sub]);
            }
            if let Some(sub) = json::get(&self.blk[B_LIVE].data, "traffic", SNAPSHOT_CAP) {
                cat(&[b",\"traffic\":", &sub]);
            }
        }
        // regress: kept when stale (the screen greys the last rows and says when they stopped)
        let r = &self.blk[B_REGRESS];
        if r.present {
            cat(&[
                b",\"regress\":{\"fresh\":",
                if r.fresh { b"true" } else { b"false" },
                b",\"has\":",
                if r.has { b"true" } else { b"false" },
                b",\"at\":",
                r.at.to_string().as_bytes(),
                b",\"data\":",
                if r.has { &r.data } else { b"null" },
                b"}",
            ]);
        }
        // display: only fresh (a card the core no longer hands in is gone)
        let dp = &self.blk[B_DISPLAY];
        if dp.present && dp.fresh && dp.data.first() == Some(&b'{') {
            cat(&[b",\"display\":", &dp.data]);
        }
        if self.blk[B_SMS].fresh || self.blk[B_SMS_LIST].fresh {
            let unread = if self.blk[B_SMS].fresh { json::get_int(&self.blk[B_SMS].data, "unread", 0) } else { 0 };
            cat(&[b",\"sms\":{\"unread\":", unread.to_string().as_bytes()]);
            if self.blk[B_SMS_LIST].fresh
                && let Some(sub) = json::get(&self.blk[B_SMS_LIST].data, "list", SNAPSHOT_CAP)
            {
                cat(&[b",\"list\":", &sub]);
            }
            cat(&[b"}"]);
        }
        cat(&[b"}"]);
        (out.len() < SNAPSHOT_CAP).then_some(out)
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
        let Some(blocks) = json::get(j, "blocks", SNAPSHOT_CAP).filter(|b| b.first() == Some(&b'{')) else { return -1 };
        for (i, name) in V2_NAMES.iter().enumerate() {
            self.blk[i].fresh = false;
            self.blk[i].present = false;
            if let Some(blk) = json::get(&blocks, name, SNAPSHOT_CAP)
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

    /// Take an old-/state text as the newest snapshot: 1 new, 0 the same text as last time,
    /// -1 empty, too big or unreadable.
    fn apply_snapshot_json(&mut self, j: &[u8]) -> i32 {
        let text = trim_ws(j);
        if text == self.latest_raw.as_slice() && !text.is_empty() {
            return 0;
        }
        match Some(text).filter(|t| !t.is_empty() && t.len() < SNAPSHOT_CAP).and_then(decode_snapshot) {
            None => -1,
            Some(d) => {
                (self.latest_raw, self.fresh_snap, self.latest_ok) = (text.to_vec(), d, true);
                self.latest_gen += 1;
                1
            }
        }
    }

    // ---- transport ----

    /// Sends `req` and reads core's answer within a total read budget of io_ms: all of it (until core
    /// hangs up) when `whole`, else just past the head. None on any failure or past `limit`.
    fn ask(&self, req: &str, limit: usize, whole: bool) -> Option<(OwnedFd, Vec<u8>)> {
        let fd = http::connect_tcp(&self.cfg.host, self.cfg.port, self.cfg.io_ms)?;
        let raw = fd.as_raw_fd();
        if !http::send(raw, req.as_bytes()) {
            return None;
        }
        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(self.cfg.io_ms.max(0) as u64);
        loop {
            if !whole && body_start(&got).is_some() {
                return Some((fd, got));
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if got.len() + 1 >= limit || left.is_zero() || wait(raw, false, left.as_millis().max(1).min(i32::MAX as u128) as i32) <= 0 {
                return None;
            }
            let room = limit - 1 - got.len();
            match read_into(raw, &mut got, room) {
                0 if whole => return Some((fd, got)),
                n if n > 0 => {}
                n if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) => {}
                _ => return None,
            }
        }
    }

    /// One GET /v2/state on its own connection: 1 changed, 0 the same, -1 failed.
    fn fetch_state_http(&mut self) -> i32 {
        let req = format!("GET /v2/state HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\r\n", self.cfg.host, self.cfg.port);
        let Some((_, resp)) = self.ask(&req, REPLY_CAP, true) else { return -1 };
        let Some(body) = body_start(&resp).filter(|_| ok_200(&resp)) else { return -1 };
        self.mark_alive();
        self.v2_apply_snapshot(&resp[body..], false)
    }

    /// One whole event: 1 when it changed the snapshot, else 0. Unparseable ones count toward
    /// dropping the stream.
    pub fn process_sse_event(&mut self, buf: &[u8]) -> i32 {
        let Some((name, data)) = sse_fields(buf) else { return 0 };
        let rc = self.v2_apply_event(&name, &data);
        if rc >= 0 {
            self.bad_events = 0;
            return rc;
        }
        self.bad_events += 1;
        eprintln!("ui: SSE snapshot parse failed ({}/{BAD_EVENTS_LIMIT})", self.bad_events);
        0
    }

    /// Every whole event buffered so far, stopping early once out of step (what follows is out
    /// of step too). true = the snapshot changed.
    fn process_sse_buffer(&mut self) -> bool {
        let mut used = 0;
        let mut changed = false;
        while !self.resync {
            let n = next_sse_event_bytes(&self.stream_buf[used..]);
            if n == 0 {
                break;
            }
            let ev = self.stream_buf[used..used + n].to_vec();
            used += n;
            changed |= self.process_sse_event(&ev) > 0;
        }
        self.stream_buf.drain(..used);
        changed
    }

    /// GET /v2/events and keep the socket, non-blocking, for drain_sse_stream.
    fn open_sse_stream(&mut self) -> bool {
        let req = format!(
            "GET /v2/events HTTP/1.1\r\nHost: {}:{}\r\nAccept: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
            self.cfg.host, self.cfg.port
        );
        let Some((fd, head)) = self.ask(&req, 16384, false) else { return false };
        self.install_stream(fd, head)
    }

    fn install_stream(&mut self, fd: OwnedFd, head: Vec<u8>) -> bool {
        let Some(body) = body_start(&head).filter(|_| ok_200(&head)) else { return false };
        // SAFETY: fcntl on a socket we own
        let nonblocking = unsafe {
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL, 0);
            flags >= 0 && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
        };
        if !nonblocking {
            return false;
        }
        self.sse = Some(fd);
        self.stream_buf = head[body..].to_vec(); // the first events may have come with the head
        self.retry_at_ms = 0;
        self.pace_sent = -1; // maybe a restarted core: tell it the pace again
        self.mark_alive();
        true
    }

    /// Too many unparseable events in a row: drop the stream, the poll falls back to HTTP.
    fn sse_drop_if_unparseable(&mut self) -> bool {
        let give_up = self.sse.is_some() && self.bad_events >= BAD_EVENTS_LIMIT;
        if give_up {
            eprintln!("ui: dropping SSE after {} consecutive parse failures; falling back to HTTP polling", self.bad_events);
            self.close_sse_stream();
        }
        give_up
    }

    /// Out of step with the stream: reconnect now; the new connection starts with a snapshot.
    fn sse_drop_if_out_of_step(&mut self) -> bool {
        if self.sse.is_none() || !self.resync {
            return false;
        }
        eprintln!("ui: /v2 stream out of step (epoch or seq), reconnecting");
        self.close_sse_stream();
        self.retry_at_ms = 0;
        true
    }

    /// Whatever the stream has for us now, without waiting. true = the snapshot changed.
    fn drain_sse_stream(&mut self) -> bool {
        let mut changed = self.process_sse_buffer();
        while !(self.sse_drop_if_unparseable() || self.sse_drop_if_out_of_step()) {
            let Some(fd) = self.sse.as_ref().map(|f| f.as_raw_fd()) else { break };
            let room = (STREAM_CAP - 1).saturating_sub(self.stream_buf.len());
            if room == 0 {
                self.close_sse_stream(); // an event bigger than any snapshot: start over
                break;
            }
            let n = read_into(fd, &mut self.stream_buf, room);
            if n > 0 {
                self.mark_alive(); // events and keep-alive comments alike
                changed |= self.process_sse_buffer();
                continue;
            }
            let err = std::io::Error::last_os_error().raw_os_error();
            match err {
                _ if n == 0 => {} // core hung up
                Some(libc::EINTR) => continue,
                Some(e) if e == libc::EAGAIN || e == libc::EWOULDBLOCK => break,
                _ => {}
            }
            self.close_sse_stream();
            break;
        }
        changed
    }

    /// First contact: a snapshot over HTTP if core answers, then the stream (or a retry
    /// later). true = the screen has a snapshot.
    pub fn init(&mut self) -> bool {
        self.fetch_state_http();
        if self.latest_ok {
            self.commit_latest();
        }
        if self.sse.is_none() && !self.open_sse_stream() {
            self.retry_at_ms = mono_ms32().wrapping_add(self.cfg.retry_ms);
        }
        self.shown_ok
    }

    /// The SSE socket (the dark loop waits on it).
    pub fn fd(&self) -> i32 {
        self.sse.as_ref().map_or(-1, |f| f.as_raw_fd())
    }

    pub fn alive_wall(&self) -> i64 {
        self.alive_wall
    }

    /// core answered before but has said nothing for silent_ms.
    pub fn silent(&mut self) -> bool {
        self.backend_silent()
    }

    /// Bumps with each committed snapshot.
    pub fn version(&self) -> u64 {
        self.shown_gen
    }

    fn due(&self, now: u32) -> bool {
        // 0 = due; the signed difference survives the 32-bit ms wrap
        self.retry_at_ms == 0 || now.wrapping_sub(self.retry_at_ms) as i32 >= 0
    }

    /// Every main-loop pass: settle parked writes, read the stream, reconnect when it is gone,
    /// silent or unusable. true = the live snapshot changed.
    pub fn poll(&mut self, now: u32) -> bool {
        if self.background { return self.poll_background(now); }
        self.control_reap(now);
        let mut changed = false;
        if !self.shown_ok && !self.latest_ok && self.due(now) {
            self.init(); // nothing yet: try again, paced by retry_at_ms
        }
        if self.sse.is_some() {
            changed |= self.drain_sse_stream();
            if self.sse.is_some() && self.backend_silent() {
                // past the keep-alive with the socket still open: reconnect now
                eprintln!("ui: SSE silent for {} ms, reconnecting", mono_ms32().wrapping_sub(self.alive_ms));
                self.close_sse_stream();
                self.retry_at_ms = now;
            }
            if self.sse.is_none() {
                // just dropped: one HTTP snapshot to cover the gap
                changed |= self.fetch_state_http() > 0;
            }
        }
        if self.sse.is_none() && self.due(now) {
            if self.open_sse_stream() {
                changed |= self.drain_sse_stream();
            } else {
                changed |= self.fetch_state_http() > 0;
                // a core that accepts and never answers costs two I/O timeouts a try: back off
                let wait = if self.backend_silent() { self.cfg.silent_retry_ms } else { self.cfg.retry_ms };
                self.retry_at_ms = now.wrapping_add(wait);
            }
        }
        changed
    }

    /// Make the newest snapshot the one the screen shows. false = nothing new.
    pub fn commit_latest(&mut self) -> bool {
        let fresh = self.latest_ok && !(self.shown_ok && self.shown_gen == self.latest_gen);
        if fresh {
            self.shown_snap = self.fresh_snap.clone();
            self.shown_ok = true;
            self.shown_gen = self.latest_gen;
        }
        fresh
    }

    pub fn close(&mut self) {
        self.contact = None;
        self.sse = None;
        self.stream_buf.clear();
    }

    /// No snapshot on screen yet: try to get one now (blocks for the connect).
    pub fn refresh_init(&mut self) {
        if !self.shown_ok && !self.background {
            self.init();
        }
    }

    /// A copy of the data as handed to the screen, with the HSR test switch applied.
    fn handed_out(valid: bool, d: &core_snapshot_t) -> (bool, core_snapshot_t) {
        if !valid {
            return (false, empty());
        }
        let mut d = d.clone();
        d.rail_mode |= force_hsr() as i32;
        (true, d)
    }

    /// The committed snapshot, the one the screen shows.
    pub fn refresh(&self) -> (bool, core_snapshot_t) {
        Self::handed_out(self.shown_ok, &self.shown_snap)
    }

    /// The card the screen shows (the committed snapshot's sim.iccid), "" without one.
    pub fn sim_iccid(&self) -> &str {
        if self.shown_ok { &self.shown_snap.sim_iccid } else { "" }
    }

    /// The card's key for "once per card" reads: the ICCID, else "imsi:" + IMSI (after a hot
    /// swap or an eSIM switch the vendor's sim_iccid can stay empty, 2026-10-11), "" without one.
    pub fn sim_key(&self) -> String {
        let d = &self.shown_snap;
        match (self.shown_ok, d.sim_iccid.is_empty(), d.sim_imsi.is_empty()) {
            (false, _, _) | (true, true, true) => String::new(),
            (true, false, _) => d.sim_iccid.clone(),
            (true, true, false) => format!("imsi:{}", d.sim_imsi),
        }
    }

    /// The newest snapshot, committed or not.
    pub fn refresh_live(&self) -> (bool, core_snapshot_t) {
        Self::handed_out(self.latest_ok, &self.fresh_snap)
    }

    // ---- /control ----

    /// The write core turned down: keep one line for the screen. 409 carries the core's own
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
        if s.action == "usb.attach_mode"
            && let Some((zh, en)) = usb_refusal(body)
        {
            self.notice.kind = 3; // a capability refusal is not "another change is busy"
            self.notice.say_zh = zh.into();
            self.notice.say_en = en.into();
        }
        eprintln!("ui: control {}: core said {}", s.action, text(&s.reply[..s.reply.len().min(12)]));
    }

    /// Close the parked requests whose answer is in, or that waited CONTROL_LINGER_MS.
    fn control_reap(&mut self, now: u32) {
        let mut i = 0;
        while i < self.ctl.len() {
            let fd = self.ctl[i].fd.as_raw_fd();
            let mut done = false;
            while wait(fd, false, 0) > 0 {
                let s = &mut self.ctl[i];
                let max_reply = if s.action == "usb.attach_mode" { USB_CONTROL_REPLY_MAX } else { CONTROL_REPLY_MAX };
                let room = max_reply - 1 - s.reply.len();
                let at = s.reply.len();
                s.reply.resize(at + room, 0);
                // SAFETY: room bytes reserved past `at`
                let n = unsafe { libc::recv(fd, s.reply[at..].as_mut_ptr().cast(), room, libc::MSG_DONTWAIT) };
                s.reply.truncate(at + n.max(0) as usize);
                if n > 0 {
                    if s.reply.len() >= max_reply - 1 {
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
            // USB's final Core preflight and readback run after its existing write lock.
            let linger_ms = if self.ctl[i].action == "usb.attach_mode" { 75_000 } else { CONTROL_LINGER_MS };
            if now.wrapping_sub(self.ctl[i].t) >= linger_ms {
                // no answer yet: core may still be doing it; nothing runs directly (twice could
                // register the modem twice)
                eprintln!("ui: control {}: no reply in {linger_ms} ms", self.ctl[i].action);
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

    /// POST /control from the screen (source, op_id); true = it reached core (parked).
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
            INBOX_CAP * 12 + 160,
        );
        let req = trunc(
            &format!(
                "POST /control HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                self.cfg.host,
                self.cfg.port,
                body.len()
            ),
            INBOX_CAP * 12 + 512,
        );
        if !http::send(fd.as_raw_fd(), req.as_bytes()) {
            return false;
        }
        self.control_park(fd, action);
        true
    }

    /// core first; not reachable at all: the emergency script (it checks again that core is
    /// gone, D18). Never on 503, 409 or no answer: core may still be doing it.
    pub fn control_fb(&mut self, action: &str, params: &str, fb_action: &str, fb_args: Option<&str>) -> bool {
        if self.control_send_ex(action, params, false) {
            return true;
        }
        if fb_args.is_none() {
            return false;
        }
        match logic::control_fallback_cmd(FALLBACK_SCRIPT, fb_action, fb_args, 512) {
            Some(cmd) => {
                eprintln!("ui: control {action}: core unreachable, emergency script {fb_action}");
                let _ = std::process::Command::new("/bin/sh").arg("-c").arg(cmd).status();
            }
            None => eprintln!("ui: control {action}: core unreachable, bad emergency arguments"),
        }
        false
    }

    /// The last write core turned down, if younger than max_age_ms.
    pub fn notice(&self, max_age_ms: u32) -> Option<data_notice_t> {
        (self.notice.at != 0 && mono_ms32().wrapping_sub(self.notice.at) < max_age_ms).then(|| self.notice.clone())
    }

    /// the core's polling pace: 1 s lit, 5 s dark, as this UI's want (client "touch"): core runs
    /// at the fastest want and drops one that is not renewed. Sent on a change, after every
    /// reconnect (a restarted core has no wants), and once a minute while it holds.
    pub fn set_pace(&mut self, panel_lit: bool) {
        let want = if panel_lit { PACE_LIT_MS } else { PACE_DARK_MS };
        let now = mono_ms32();
        if want == self.pace_sent && now.wrapping_sub(self.pace_at) < PACE_RENEW_MS {
            return;
        }
        self.control_send_ex(
            "state.set_interval",
            &format!("{{\"client\":\"touch\",\"milliseconds\":{want},\"ttl_s\":{PACE_TTL_S}}}"),
            false,
        );
        self.pace_sent = want;
        self.pace_at = now;
    }

    /// this UI stops (or hands the screen over): take its want back; core falls to its idle pace.
    pub fn release_pace(&mut self) {
        self.control_send_ex("state.set_interval", "{\"client\":\"touch\",\"release\":true}", false);
        self.pace_sent = -1;
    }

    // ---- SMS ----

    fn sms(&self) -> &[sms_entry_t] {
        &self.shown_snap.inbox[..(self.shown_snap.inbox_n.max(0) as usize).min(self.shown_snap.inbox.len())]
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
            if ids.len() + w.len() > INBOX_CAP * 12 {
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
