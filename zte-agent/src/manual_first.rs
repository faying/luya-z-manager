//! CHILL "manual first" (§5 of `docs/designs/slow-diagnosis.md`).
//!
//! When 🚀 节点选择 points at 🎯 手动节点 and the hand-picked node stops
//! working while the phone is actually using the proxy, switch 🚀 to that
//! node's region group (url-test, 🇯🇵 日本 …) and say so; once the node has
//! been fine for 10 minutes, switch back and say so too.
//!
//! Not acting is the default. Nothing happens unless CHILL is running in rule
//! mode, 🚀 is on 🎯 (or on the backup we put there), the proxy is in use, the
//! cellular link is alive and a direct DNS query is quick — a slow direct path
//! means the radio is the problem and another node won't help.
//!
//! "In use" = a connection through 🚀 from a LAN client sent more bytes since
//! the last look (the design said "cellular tx rising"; that also counts our own
//! probes and Tailscale keepalives, so it would never go quiet). Idle ⇒ no probes.
//!
//! The decision logic is [`Machine::tick`], with all I/O behind [`Io`] and time
//! passed in, so the whole flow is unit-tested without a device.

use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAIN_GROUP: &str = "🚀 节点选择";
pub const MANUAL_GROUP: &str = "🎯 手动节点";
pub const REGIONS: [&str; 4] = ["🇯🇵 日本", "🇹🇼 台湾", "🇸🇬 新加坡", "🇺🇸 美国"];

const STATE_FILE: &str = "/data/chill/manual-first.json";
const DEFAULT_API: &str = "http://127.0.0.1:9999";
const PROBE_URL: &str = "https://www.gstatic.com/generate_204";

const TICK: Duration = Duration::from_secs(10);
const PROBE_EVERY: u64 = 60;
const PROBE_BACK_EVERY: u64 = 120;
const SLOW_MS: u32 = 800;
const TIMEOUTS_TO_LEAVE: u32 = 2;
const BAD_TO_LEAVE: u32 = 3;
const GOOD_TO_RETURN: u64 = 600;
const MAX_SWITCHES_PER_HOUR: usize = 2;
/// How long the "switched back" line stays up.
const NOTICE_KEEP: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Probe {
    Ms(u32),
    Timeout,
}

/// What the loop sees on each tick (cheap local reads only).
#[derive(Clone, Debug, Default)]
pub struct Obs {
    pub now: u64,
    /// CHILL running and mihomo in rule mode.
    pub active: bool,
    pub main_now: String,
    pub manual_now: String,
}

/// Everything that talks to the outside. `None` from a probe = could not ask
/// (mihomo not answering): skip, never count it against the node.
pub trait Io {
    fn in_use(&mut self) -> bool;
    fn probe(&mut self, node: &str) -> Option<Probe>;
    fn cell_alive(&mut self) -> bool;
    fn direct_ms(&mut self) -> Option<u32>;
    /// Region group whose members include `node`.
    fn region_of(&mut self, node: &str) -> Option<String>;
    /// Re-test `group` now; return what it picked and whether that works.
    fn retest(&mut self, group: &str) -> Option<(String, bool)>;
    fn select_main(&mut self, member: &str) -> bool;
    /// Close connections still going through the dead node via 🎯.
    fn drop_manual_conns(&mut self);
    fn save(&mut self, p: &Persist);
}

/// Survives an agent restart (written only on switch / return / clear).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Persist {
    /// Non-empty = we moved 🚀 off 🎯 onto this group.
    pub backup: String,
    pub manual: String,
    pub switched_at: u64,
    /// Times we switched away, for the 2-per-hour cap.
    pub switches: Vec<u64>,
    pub last_region: String,
    pub notice: String,
    pub notice_at: u64,
}

#[derive(Default)]
pub struct Machine {
    pub p: Persist,
    manual: String,
    timeouts: u32,
    bad: u32,
    good_since: Option<u64>,
    last_probe: u64,
}

impl Machine {
    pub fn new(p: Persist) -> Self {
        Machine { manual: p.manual.clone(), p, ..Default::default() }
    }

    fn reset_counts(&mut self) {
        self.timeouts = 0;
        self.bad = 0;
        self.good_since = None;
    }

    fn say(&mut self, now: u64, text: String) {
        eprintln!("[manual_first] {text}");
        self.p.notice = text;
        self.p.notice_at = now;
    }

    fn clear_backup(&mut self, io: &mut dyn Io) {
        self.p.backup.clear();
        self.reset_counts();
        io.save(&self.p);
    }

    pub fn tick(&mut self, o: &Obs, io: &mut dyn Io) {
        if !o.active || o.main_now.is_empty() {
            return;
        }
        if REGIONS.contains(&o.main_now.as_str()) && o.main_now != self.p.backup && o.main_now != self.p.last_region {
            self.p.last_region = o.main_now.clone();
        }
        if !self.p.backup.is_empty() {
            self.tick_on_backup(o, io);
        } else {
            self.tick_on_manual(o, io);
        }
    }

    fn tick_on_backup(&mut self, o: &Obs, io: &mut dyn Io) {
        if o.main_now != self.p.backup {
            // The owner (or the exit switch, or abroad mode) chose something
            // else: hands off.
            self.p.notice.clear();
            self.clear_backup(io);
            return;
        }
        if !o.manual_now.is_empty() && o.manual_now != self.p.manual {
            // A new hand pick while on the backup: that is a clear wish to use
            // it, so go straight back to 🎯 (a tap must not look ignored).
            if io.select_main(MANUAL_GROUP) {
                self.say(o.now, format!("按你新选的 {} 换回手动节点", o.manual_now));
                self.p.manual = o.manual_now.clone();
                self.manual = o.manual_now.clone();
                self.clear_backup(io);
            }
            return;
        }
        if o.now.saturating_sub(self.last_probe) < PROBE_BACK_EVERY || !io.in_use() {
            return;
        }
        self.last_probe = o.now;
        if !io.cell_alive() {
            return;
        }
        let manual = self.p.manual.clone();
        match io.probe(&manual) {
            Some(Probe::Ms(ms)) if ms <= SLOW_MS => {
                let since = *self.good_since.get_or_insert(o.now);
                if o.now - since >= GOOD_TO_RETURN && io.select_main(MANUAL_GROUP) {
                    self.say(o.now, format!("{manual} 恢复，已换回"));
                    self.clear_backup(io);
                }
            }
            Some(_) => self.good_since = None,
            None => {}
        }
    }

    fn tick_on_manual(&mut self, o: &Obs, io: &mut dyn Io) {
        if o.main_now != MANUAL_GROUP || o.manual_now.is_empty() {
            self.reset_counts();
            return;
        }
        if o.manual_now != self.manual {
            self.manual = o.manual_now.clone();
            self.reset_counts();
        }
        if o.now.saturating_sub(self.last_probe) < PROBE_EVERY || !io.in_use() {
            return;
        }
        self.last_probe = o.now;
        let manual = self.manual.clone();
        match io.probe(&manual) {
            None => return,
            Some(Probe::Ms(ms)) if ms <= SLOW_MS => {
                self.reset_counts();
                return;
            }
            Some(Probe::Timeout) => {
                self.timeouts += 1;
                self.bad += 1;
            }
            Some(Probe::Ms(_)) => {
                self.timeouts = 0;
                self.bad += 1;
            }
        }
        if self.timeouts < TIMEOUTS_TO_LEAVE && self.bad < BAD_TO_LEAVE {
            return;
        }
        // From here on this round decides; start counting afresh either way.
        self.reset_counts();
        if !io.cell_alive() {
            return; // cellular down: of course the node fails
        }
        if io.direct_ms().is_none_or(|ms| ms > SLOW_MS) {
            return; // direct is slow too: signal/cell problem, not the node
        }
        self.p.switches.retain(|t| o.now.saturating_sub(*t) < 3600);
        if self.p.switches.len() >= MAX_SWITCHES_PER_HOUR {
            self.say(o.now, format!("手选的 {manual} 不稳，1 小时内已换走 {} 次，这次不换", self.p.switches.len()));
            return;
        }
        let backup = io
            .region_of(&manual)
            .or_else(|| (!self.p.last_region.is_empty()).then(|| self.p.last_region.clone()))
            .unwrap_or_else(|| REGIONS[0].to_string());
        match io.retest(&backup) {
            Some((picked, true)) if picked != manual => {}
            _ => {
                self.say(o.now, format!("手选的 {manual} 和同地区备用 {backup} 都不通"));
                return;
            }
        }
        if !io.select_main(&backup) {
            return;
        }
        io.drop_manual_conns();
        self.p.backup = backup.clone();
        self.p.manual = manual.clone();
        self.p.switched_at = o.now;
        self.p.switches.push(o.now);
        self.say(o.now, format!("手选的 {manual} 不通，已临时换到 {backup}（自动）"));
        io.save(&self.p);
    }
}

/* -------------------------------------------------------------- *
 *  Pure helpers
 * -------------------------------------------------------------- */

/// Ids of connections that go through 🎯 (so a dead hand-picked node) — not
/// 🤖 AI / 📞 VoWiFi ones that happen to use the same node.
pub fn manual_conn_ids(conns: &Value) -> Vec<String> {
    conn_list(conns)
        .filter(|c| chains(c).any(|h| h == MANUAL_GROUP))
        .filter_map(|c| c.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Upload bytes of LAN connections through 🚀, by id. The agent's own lookups
/// (127.0.0.1 → :7894) are left out so they cannot keep it "in use".
pub fn main_uploads(conns: &Value) -> HashMap<String, u64> {
    conn_list(conns)
        .filter(|c| chains(c).any(|h| h == MAIN_GROUP))
        .filter(|c| c.pointer("/metadata/sourceIP").and_then(Value::as_str) != Some("127.0.0.1"))
        .filter_map(|c| Some((c.get("id")?.as_str()?.to_string(), c.get("upload")?.as_u64()?)))
        .collect()
}

/// Something was sent through 🚀 since the previous look.
pub fn sent_since(prev: &HashMap<String, u64>, now: &HashMap<String, u64>) -> bool {
    now.iter().any(|(id, up)| prev.get(id).is_none_or(|p| up > p))
}

/// node name → provider, from `/providers/proxies`. Only file/http
/// providers: mihomo also lists every proxy group as a "Compatible" provider.
pub fn node_providers(all: &Value) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(provs) = all.get("providers").and_then(Value::as_object) else { return map };
    for (name, p) in provs {
        if p.get("vehicleType").and_then(Value::as_str) == Some("Compatible") {
            continue;
        }
        for n in p.get("proxies").and_then(Value::as_array).into_iter().flatten() {
            if let Some(n) = n.get("name").and_then(Value::as_str) {
                map.entry(n.to_string()).or_insert_with(|| name.clone());
            }
        }
    }
    map
}

fn conn_list(conns: &Value) -> impl Iterator<Item = &Value> {
    conns.get("connections").and_then(Value::as_array).into_iter().flatten()
}

fn chains(c: &Value) -> impl Iterator<Item = &str> {
    c.get("chains").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str)
}

/* -------------------------------------------------------------- *
 *  Runtime: the thread, mihomo I/O, status for the screens
 * -------------------------------------------------------------- */

/// The group we switched 🚀 to, or empty. Kept apart from the machine so the
/// exit switch can read it without waiting for a tick's network calls.
static BACKUP: Mutex<String> = Mutex::new(String::new());
static PUBLIC: Mutex<Option<Persist>> = Mutex::new(None);

/// What `last-main` should remember for 🚀's current value: while we hold the
/// backup, the owner's real choice is still 🎯.
pub fn main_to_remember(now: &str) -> &str {
    let b = BACKUP.lock().unwrap_or_else(|e| e.into_inner());
    if !b.is_empty() && *b == now {
        MANUAL_GROUP
    } else {
        now
    }
}

/// For `/api/services/chill` and `/api/public/status`.
pub fn summary() -> Value {
    let p = PUBLIC.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default();
    let on_backup = !p.backup.is_empty();
    let fresh = on_backup || now_unix().saturating_sub(p.notice_at) < NOTICE_KEEP;
    json!({
        "on_backup": on_backup,
        "manual": (!p.manual.is_empty()).then_some(p.manual.as_str()),
        "backup": on_backup.then_some(p.backup.as_str()),
        "notice": (fresh && !p.notice.is_empty()).then_some(p.notice.as_str()),
        "notice_at": (fresh && !p.notice.is_empty()).then_some(p.notice_at),
    })
}

fn publish(p: &Persist) {
    *BACKUP.lock().unwrap_or_else(|e| e.into_inner()) = p.backup.clone();
    *PUBLIC.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
}

pub fn start() {
    thread::Builder::new()
        .name("manual-first".into())
        .spawn(|| {
            let p: Persist = fs::read_to_string(STATE_FILE)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            publish(&p);
            let mut m = Machine::new(p);
            let mut io = Mihomo::new();
            loop {
                let o = io.observe();
                let before = m.p.clone();
                m.tick(&o, &mut io);
                if m.p != before {
                    publish(&m.p);
                }
                thread::sleep(TICK);
            }
        })
        .ok();
}

struct Mihomo {
    api: String,
    fast: ureq::Agent,
    slow: ureq::Agent,
    uploads: HashMap<String, u64>,
    providers: HashMap<String, String>,
}

enum Got {
    Json(u16, Value),
    Status(u16),
    Unreachable,
}

impl Mihomo {
    fn new() -> Self {
        let agent = |secs| -> ureq::Agent {
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(secs)))
                .http_status_as_error(false)
                .build()
                .into()
        };
        Mihomo {
            // Overridable for the local fake-mihomo run (docs: §9 本机).
            api: std::env::var("ZTE_AGENT_MIHOMO_API").unwrap_or_else(|_| DEFAULT_API.to_string()),
            fast: agent(3),
            // A 3 s delay test needs more than 3 s of HTTP, or every probe
            // would look like a node timeout.
            slow: agent(15),
            uploads: HashMap::new(),
            providers: HashMap::new(),
        }
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>, slow: bool) -> Got {
        let a = if slow { &self.slow } else { &self.fast };
        let url = format!("{}{path}", self.api);
        let res = match (method, body) {
            ("PUT", Some(b)) => a.put(&url).send_json(b),
            ("DELETE", _) => a.delete(&url).call(),
            _ => a.get(&url).call(),
        };
        match res {
            Ok(mut r) => {
                let code = r.status().as_u16();
                match r.body_mut().read_json::<Value>() {
                    Ok(v) => Got::Json(code, v),
                    Err(_) => Got::Status(code),
                }
            }
            Err(_) => Got::Unreachable,
        }
    }

    fn get(&self, path: &str) -> Option<Value> {
        match self.call("GET", path, None, false) {
            Got::Json(c, v) if c < 400 => Some(v),
            _ => None,
        }
    }

    fn group_now(&self, group: &str) -> String {
        self.get(&format!("/proxies/{}", enc(group)))
            .and_then(|v| v.get("now").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    }

    /// Subscription (provider) a node comes from. Provider nodes are not in
    /// `/proxies` (404 there), only under `/providers/proxies/<provider>`.
    fn provider_of(&mut self, node: &str) -> Option<String> {
        if let Some(p) = self.providers.get(node) {
            return Some(p.clone());
        }
        let all = self.get("/providers/proxies")?;
        self.providers = node_providers(&all);
        self.providers.get(node).cloned()
    }

    fn probe_once(&mut self, node: &str) -> Option<Probe> {
        let Some(prov) = self.provider_of(node) else {
            return None;
        };
        let path = format!(
            "/providers/proxies/{}/{}/healthcheck?url={}&timeout=3000",
            enc(&prov),
            enc(node),
            enc(PROBE_URL)
        );
        match self.call("GET", &path, None, true) {
            Got::Json(200, v) => match v.get("delay").and_then(Value::as_u64) {
                Some(d) if d > 0 => Some(Probe::Ms(d as u32)),
                _ => Some(Probe::Timeout),
            },
            // mihomo answered, the node didn't.
            Got::Json(408 | 503 | 504, _) | Got::Status(408 | 503 | 504) => Some(Probe::Timeout),
            Got::Json(404, _) | Got::Status(404) => {
                // Node moved or left the subscription: forget the map, don't
                // blame the node for our stale lookup.
                self.providers.clear();
                None
            }
            _ => None,
        }
    }

    fn observe(&mut self) -> Obs {
        let now = now_unix();
        let running = crate::chill::running();
        let rule = running
            && self.get("/configs").and_then(|c| c.get("mode").and_then(Value::as_str).map(str::to_string)).as_deref()
                == Some("rule");
        if !rule {
            return Obs { now, ..Default::default() };
        }
        Obs {
            now,
            active: true,
            main_now: self.group_now(MAIN_GROUP),
            manual_now: self.group_now(MANUAL_GROUP),
        }
    }
}

impl Io for Mihomo {
    fn in_use(&mut self) -> bool {
        let Some(c) = self.get("/connections") else { return false };
        let now = main_uploads(&c);
        let used = sent_since(&self.uploads, &now);
        self.uploads = now;
        used
    }

    fn probe(&mut self, node: &str) -> Option<Probe> {
        // A node that sat idle often misses the first 3 s (measured 9-25: 504,
        // then 65 ms right after), so one timeout is re-asked at once.
        match self.probe_once(node)? {
            Probe::Timeout => self.probe_once(node),
            ok => Some(ok),
        }
    }

    fn cell_alive(&mut self) -> bool {
        crate::netwatch::cell_alive()
    }

    fn direct_ms(&mut self) -> Option<u32> {
        crate::netwatch::direct_dns_ms()
    }

    fn region_of(&mut self, node: &str) -> Option<String> {
        REGIONS
            .iter()
            .find(|g| {
                self.get(&format!("/proxies/{}", enc(g)))
                    .and_then(|v| v.get("all").cloned())
                    .and_then(|a| a.as_array().map(|a| a.iter().any(|n| n.as_str() == Some(node))))
                    .unwrap_or(false)
            })
            .map(|g| g.to_string())
    }

    fn retest(&mut self, group: &str) -> Option<(String, bool)> {
        let path = format!("/group/{}/delay?url={}&timeout=3000", enc(group), enc(PROBE_URL));
        let Got::Json(200, delays) = self.call("GET", &path, None, true) else {
            return None;
        };
        let picked = self.group_now(group);
        let ok = delays.get(&picked).and_then(Value::as_u64).is_some_and(|d| d > 0);
        Some((picked, ok))
    }

    fn select_main(&mut self, member: &str) -> bool {
        let path = format!("/proxies/{}", enc(MAIN_GROUP));
        matches!(self.call("PUT", &path, Some(json!({"name": member})), false), Got::Json(c, _) | Got::Status(c) if c < 400)
    }

    fn drop_manual_conns(&mut self) {
        let Some(c) = self.get("/connections") else { return };
        for id in manual_conn_ids(&c) {
            let _ = self.call("DELETE", &format!("/connections/{}", enc(&id)), None, false);
        }
    }

    fn save(&mut self, p: &Persist) {
        if let Ok(s) = serde_json::to_vec(p) {
            let tmp = format!("{STATE_FILE}.tmp");
            if fs::write(&tmp, s).is_ok() {
                let _ = fs::rename(&tmp, STATE_FILE);
            }
        }
    }
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JP: &str = "🇯🇵 日本";
    const NODE: &str = "JP 03";

    #[derive(Default)]
    struct Fake {
        in_use: bool,
        probes: Vec<Option<Probe>>,
        cell: bool,
        direct: Option<u32>,
        region: Option<String>,
        retest: Option<(String, bool)>,
        selected: Vec<String>,
        dropped: u32,
        saved: u32,
        probed: u32,
    }

    impl Io for Fake {
        fn in_use(&mut self) -> bool {
            self.in_use
        }
        fn probe(&mut self, _n: &str) -> Option<Probe> {
            self.probed += 1;
            if self.probes.is_empty() { Some(Probe::Ms(100)) } else { self.probes.remove(0) }
        }
        fn cell_alive(&mut self) -> bool {
            self.cell
        }
        fn direct_ms(&mut self) -> Option<u32> {
            self.direct
        }
        fn region_of(&mut self, _n: &str) -> Option<String> {
            self.region.clone()
        }
        fn retest(&mut self, _g: &str) -> Option<(String, bool)> {
            self.retest.clone()
        }
        fn select_main(&mut self, m: &str) -> bool {
            self.selected.push(m.to_string());
            true
        }
        fn drop_manual_conns(&mut self) {
            self.dropped += 1;
        }
        fn save(&mut self, _p: &Persist) {
            self.saved += 1;
        }
    }

    fn healthy() -> Fake {
        Fake {
            in_use: true,
            cell: true,
            direct: Some(60),
            region: Some(JP.into()),
            retest: Some(("JP 01".into(), true)),
            ..Default::default()
        }
    }

    fn obs(now: u64, main: &str) -> Obs {
        Obs { now, active: true, main_now: main.into(), manual_now: NODE.into() }
    }

    /// Runs one tick per minute from `t`, returning the time after.
    fn run(m: &mut Machine, io: &mut Fake, t: u64, n: u32, main: &str) -> u64 {
        let mut t = t;
        for _ in 0..n {
            m.tick(&obs(t, main), io);
            t += 60;
        }
        t
    }

    fn left(m: &Machine) -> bool {
        m.p.backup == JP
    }

    #[test]
    fn two_timeouts_switch_to_region_and_drop_conns() {
        let mut m = Machine::default();
        let mut io = healthy();
        io.probes = vec![Some(Probe::Timeout), Some(Probe::Timeout)];
        run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
        assert!(left(&m));
        assert_eq!(io.selected, vec![JP]);
        assert_eq!(io.dropped, 1);
        assert_eq!(io.saved, 1);
        assert!(m.p.notice.contains("JP 03") && m.p.notice.contains(JP));
    }

    #[test]
    fn three_slow_switch_but_two_do_not() {
        let mut m = Machine::default();
        let mut io = healthy();
        io.probes = vec![Some(Probe::Ms(900)), Some(Probe::Ms(1200)), Some(Probe::Ms(100)), Some(Probe::Ms(900))];
        run(&mut m, &mut io, 1000, 4, MANUAL_GROUP);
        assert!(!left(&m), "a good probe in between resets the count");
        io.probes = vec![Some(Probe::Ms(900)), Some(Probe::Ms(950))];
        run(&mut m, &mut io, 2000, 2, MANUAL_GROUP);
        assert!(left(&m));
    }

    #[test]
    fn unreachable_mihomo_is_not_a_failure() {
        let mut m = Machine::default();
        let mut io = healthy();
        io.probes = vec![Some(Probe::Timeout), None, None, Some(Probe::Ms(100)), Some(Probe::Timeout)];
        run(&mut m, &mut io, 1000, 5, MANUAL_GROUP);
        assert!(!left(&m));
    }

    #[test]
    fn idle_means_no_probe() {
        let mut m = Machine::default();
        let mut io = healthy();
        io.in_use = false;
        run(&mut m, &mut io, 1000, 10, MANUAL_GROUP);
        assert_eq!(io.probed, 0);
    }

    #[test]
    fn probes_at_most_once_a_minute() {
        let mut m = Machine::default();
        let mut io = healthy();
        for t in (1000..1060).step_by(10) {
            m.tick(&obs(t, MANUAL_GROUP), &mut io);
        }
        assert_eq!(io.probed, 1);
    }

    #[test]
    fn no_switch_when_cell_down_or_direct_slow() {
        for (cell, direct) in [(false, Some(50)), (true, Some(2000)), (true, None)] {
            let mut m = Machine::default();
            let mut io = healthy();
            io.cell = cell;
            io.direct = direct;
            io.probes = vec![Some(Probe::Timeout); 6];
            run(&mut m, &mut io, 1000, 6, MANUAL_GROUP);
            assert!(!left(&m), "cell={cell} direct={direct:?}");
            assert!(io.selected.is_empty());
        }
    }

    #[test]
    fn backup_that_picks_the_dead_node_or_fails_is_not_used() {
        for r in [Some((NODE.to_string(), true)), Some(("JP 01".to_string(), false)), None] {
            let mut m = Machine::default();
            let mut io = healthy();
            io.retest = r.clone();
            io.probes = vec![Some(Probe::Timeout); 2];
            run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
            assert!(!left(&m), "{r:?}");
            assert!(m.p.notice.contains("都不通"));
        }
    }

    #[test]
    fn unknown_region_uses_last_auto_region() {
        let mut m = Machine::default();
        let mut io = healthy();
        m.tick(&obs(500, "🇸🇬 新加坡"), &mut io);
        io.region = None;
        io.probes = vec![Some(Probe::Timeout); 2];
        run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
        assert_eq!(m.p.backup, "🇸🇬 新加坡");
    }

    #[test]
    fn returns_after_ten_good_minutes_only() {
        let mut m = Machine::default();
        let mut io = healthy();
        io.probes = vec![Some(Probe::Timeout); 2];
        let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
        assert!(left(&m));
        // On backup: probes every 2 min. One failure restarts the 10 minutes.
        io.probes = vec![Some(Probe::Ms(100)), Some(Probe::Ms(100)), Some(Probe::Timeout)];
        let t = run(&mut m, &mut io, t + 120, 6, JP);
        assert!(left(&m));
        io.probes = vec![];
        let t = run(&mut m, &mut io, t, 10, JP); // 9 minutes of good probes
        assert!(left(&m), "not 10 minutes yet");
        run(&mut m, &mut io, t, 4, JP);
        assert!(!left(&m));
        assert_eq!(io.selected.last().map(String::as_str), Some(MANUAL_GROUP));
        assert!(m.p.notice.contains("恢复"));
    }

    #[test]
    fn owner_changing_main_stops_takeover() {
        let mut m = Machine::default();
        let mut io = healthy();
        io.probes = vec![Some(Probe::Timeout); 2];
        let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
        assert!(left(&m));
        m.tick(&obs(t, "DIRECT"), &mut io);
        assert!(m.p.backup.is_empty());
        assert!(m.p.notice.is_empty());
        let n = io.selected.len();
        run(&mut m, &mut io, t + 60, 20, "DIRECT");
        assert_eq!(io.selected.len(), n, "never switches back on its own afterwards");
    }

    #[test]
    fn new_hand_pick_while_on_backup_goes_back_to_manual() {
        let mut m = Machine::default();
        let mut io = healthy();
        io.probes = vec![Some(Probe::Timeout); 2];
        let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
        let o = Obs { now: t, active: true, main_now: JP.into(), manual_now: "JP 07".into() };
        m.tick(&o, &mut io);
        assert!(m.p.backup.is_empty());
        assert_eq!(io.selected.last().map(String::as_str), Some(MANUAL_GROUP));
        assert!(m.p.notice.contains("JP 07"));
    }

    #[test]
    fn inactive_chill_does_nothing_and_keeps_state() {
        let mut m = Machine::default();
        let mut io = healthy();
        io.probes = vec![Some(Probe::Timeout); 2];
        let t = run(&mut m, &mut io, 1000, 2, MANUAL_GROUP);
        m.tick(&Obs { now: t, ..Default::default() }, &mut io);
        assert!(left(&m));
    }

    #[test]
    fn at_most_two_switches_an_hour() {
        let mut m = Machine::default();
        let mut io = healthy();
        let mut t = 1000;
        for _ in 0..2 {
            io.probes = vec![Some(Probe::Timeout); 2];
            t = run(&mut m, &mut io, t, 2, MANUAL_GROUP);
            assert!(left(&m));
            m.tick(&obs(t, MANUAL_GROUP), &mut io); // owner put it back by hand
            t += 60;
        }
        io.probes = vec![Some(Probe::Timeout); 2];
        run(&mut m, &mut io, t, 2, MANUAL_GROUP);
        assert!(!left(&m));
        assert!(m.p.notice.contains("这次不换"));
    }

    #[test]
    fn restart_resumes_waiting_to_return() {
        let p = Persist { backup: JP.into(), manual: NODE.into(), switched_at: 900, ..Default::default() };
        let mut m = Machine::new(p);
        let mut io = healthy();
        run(&mut m, &mut io, 1000, 14, JP);
        assert!(!left(&m));
        assert_eq!(io.selected, vec![MANUAL_GROUP]);
    }

    #[test]
    fn exit_switch_remembers_manual_while_on_backup() {
        publish(&Persist { backup: JP.into(), ..Default::default() });
        assert_eq!(main_to_remember(JP), MANUAL_GROUP);
        assert_eq!(main_to_remember("🇹🇼 台湾"), "🇹🇼 台湾");
        publish(&Persist::default());
        assert_eq!(main_to_remember(JP), JP);
    }

    #[test]
    fn provider_map_skips_groups() {
        let all = json!({"providers": {
            "oix": {"vehicleType": "File", "proxies": [{"name": "JP 03"}]},
            "🚀 节点选择": {"vehicleType": "Compatible", "proxies": [{"name": "JP 03"}]},
            "shouhou": {"vehicleType": "HTTP", "proxies": [{"name": "US 1"}]},
        }});
        let m = node_providers(&all);
        assert_eq!(m.get("JP 03").map(String::as_str), Some("oix"));
        assert_eq!(m.get("US 1").map(String::as_str), Some("shouhou"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn connection_filters() {
        let c = json!({"connections": [
            {"id": "a", "upload": 10, "chains": [NODE, MANUAL_GROUP, MAIN_GROUP], "metadata": {"sourceIP": "192.168.0.5"}},
            {"id": "b", "upload": 5, "chains": [NODE, JP, "🤖 AI"], "metadata": {"sourceIP": "192.168.0.5"}},
            {"id": "c", "upload": 7, "chains": ["X", JP, MAIN_GROUP], "metadata": {"sourceIP": "127.0.0.1"}},
        ]});
        assert_eq!(manual_conn_ids(&c), vec!["a"], "AI connection on the same node is left alone");
        let up = main_uploads(&c);
        assert_eq!(up.len(), 1, "agent's own 127.0.0.1 lookups don't count");
        assert!(sent_since(&HashMap::new(), &up));
        assert!(!sent_since(&up, &up));
        let mut more = up.clone();
        more.insert("a".into(), 11);
        assert!(sent_since(&up, &more));
    }
}
