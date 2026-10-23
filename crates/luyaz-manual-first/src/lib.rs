//! CHILL "manual first" runtime. The decisions are u60-policy's [`Machine`]; this is everything
//! around it: mihomo as the [`Io`], the 10 s loop, the state file, the status for the
//! screens and the shadow records. What the host provides goes through [`Host`].
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
//! The state file (`/data/chill/manual-first.json` on the device) is written here, on switch /
//! return / clear, and is what the other side reads once it no longer runs the loop:
//! [`read_persist`], then [`summary_of`] / [`backup_of`].

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

pub use luyaz_api::chill::PROBE_URL;
pub use luyaz_policy::manual_first::{Io, Machine, Obs, Persist, Probe, MAIN_GROUP, MANUAL_GROUP, REGIONS};

use luyaz_api::chill::{enc, node_providers, Got};
use luyaz_api::diagnose::NodeProbe;

const TICK: Duration = Duration::from_secs(10);

/// What the loop needs from the side running it.
pub trait Host: Send + Sync + 'static {
    /// mihomo's controller, e.g. `http://127.0.0.1:9999`.
    fn mihomo_api(&self) -> String;
    /// CHILL is running (mihomo up).
    fn chill_running(&self) -> bool;
    /// The cellular link is alive (netwatch).
    fn cell_alive(&self) -> bool;
    /// A direct DNS query's time, `None` = no answer (netwatch).
    fn direct_dns_ms(&self) -> Option<u32>;
    /// A shadow record for u60d (the host keeps the ring).
    fn tick(&self, record: Value);
    /// The host still owns manual-first; false = handed over (the loop stops before the next
    /// observation).
    fn active(&self) -> bool;
    fn sleep(&self, d: Duration);
}

/* -------------------------------------------------------------- *
 *  The state file, read by whichever side does not run the loop
 * -------------------------------------------------------------- */

/// The saved state; missing or unparsable = default (nothing taken over).
pub fn read_persist(path: impl AsRef<Path>) -> Persist {
    fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// manual-first's backup region while it holds one (else empty): the exit
/// switch remembers the manual group instead of it (luyaz_api::chill::main_to_remember).
pub fn backup_of(p: &Persist) -> String {
    p.backup.clone()
}

/// For `/api/services/chill` and `/api/public/status`.
pub fn summary_of(p: &Persist, now: u64) -> Value {
    luyaz_api::chill::manual_first_summary(&view(p), now)
}

/// What the summary reads (luyaz_api::chill renders it, so u60d's touch layer answers alike).
pub fn view(p: &Persist) -> luyaz_api::chill::ManualFirst<'_> {
    luyaz_api::chill::ManualFirst {
        backup: &p.backup,
        manual: &p.manual,
        notice: &p.notice,
        notice_en: &p.notice_en,
        notice_at: p.notice_at,
        notice_kind: &p.notice_kind,
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

/// Upload bytes of LAN connections through 🚀, by id. The device's own lookups
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

fn conn_list(conns: &Value) -> impl Iterator<Item = &Value> {
    conns.get("connections").and_then(Value::as_array).into_iter().flatten()
}

fn chains(c: &Value) -> impl Iterator<Item = &str> {
    c.get("chains").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str)
}

/* -------------------------------------------------------------- *
 *  Runtime: the loop, mihomo I/O, status for the screens
 * -------------------------------------------------------------- */

/// The lease u60d holds while it runs the loop (`luyaz_api::lease`; on /tmp, gone at boot).
pub const LEASE_FILE: &str = "/tmp/manual-first.owner";

pub struct ManualFirst {
    host: Arc<dyn Host>,
    state_file: PathBuf,
    /// What the loop last published (`None` until it starts). Kept apart from the machine so the
    /// exit switch can read the backup without waiting for a tick's network calls.
    public: Mutex<Option<Persist>>,
    /// [`ManualFirst::saving_every_change`]
    save_changes: bool,
}

impl ManualFirst {
    /// `state_file`: where the state is read at start and saved on switch / return / clear.
    pub fn new(host: Arc<dyn Host>, state_file: impl Into<PathBuf>) -> Self {
        ManualFirst { host, state_file: state_file.into(), public: Mutex::new(None), save_changes: false }
    }

    /// Save the state file on every change, not only on switch / return / clear: for a host whose
    /// state others read from the file (screens and the web read the summary from there).
    pub fn saving_every_change(mut self) -> Self {
        self.save_changes = true;
        self
    }

    /// [`ManualFirst::run`] on its own thread, once no other live process holds `lease` (u60d,
    /// while it runs the loop: another owner taking it back must not start before u60d's loop ended,
    /// nor read a state file u60d is about to write). At most [`luyaz_api::lease::WAIT`].
    pub fn start_after_lease(self: &Arc<Self>, lease: PathBuf) {
        let me = Arc::clone(self);
        thread::Builder::new()
            .name("manual-first".into())
            .spawn(move || {
                luyaz_api::lease::wait_free(&lease, "manual_first", &|d| me.host.sleep(d), luyaz_api::lease::WAIT);
                me.run()
            })
            .ok();
    }

    /// [`ManualFirst::run`] on its own thread.
    pub fn start(self: &Arc<Self>) {
        let me = Arc::clone(self);
        thread::Builder::new().name("manual-first".into()).spawn(move || me.run()).ok();
    }

    /// The loop on the calling thread: observe → tick → publish → shadow record → sleep, until
    /// [`Host::active`] says no.
    pub fn run(&self) {
        let p = read_persist(&self.state_file);
        self.publish(&p);
        let mut m = Machine::new(p);
        let mut io = Mihomo::new(&self.host, &self.state_file);
        let mut quiet = 0u64;
        while self.host.active() {
            let detail = std::cell::Cell::new(None);
            let o = io.observe(|main| {
                let d = m.needs_detail(main);
                detail.set(Some((main.to_string(), d)));
                d
            });
            let before = m.clone();
            let mut rec = Recorder { inner: &mut io, events: Vec::new() };
            m.tick(&o, &mut rec);
            let events = rec.events;
            log_notice(&before.p, &m.p);
            if m.p != before.p {
                self.publish(&m.p);
                if self.save_changes {
                    io.save(&m.p);
                }
            }
            if worth_a_record(&before, &m, &events) {
                self.host.tick(tick_record(&o, detail.take(), &before, &events, &m, quiet));
                quiet = 0;
            } else {
                quiet += 1;
            }
            self.host.sleep(TICK);
        }
    }

    fn publish(&self, p: &Persist) {
        *self.public.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
    }

    fn published(&self) -> Persist {
        self.public.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
    }

    /// What `last-main` should remember for 🚀's current value: while we hold the
    /// backup, the owner's real choice is still 🎯 ([`backup_of`] of the published state).
    pub fn backup(&self) -> String {
        backup_of(&self.published())
    }

    /// For `/api/services/chill` and `/api/public/status` ([`summary_of`] of the published state).
    pub fn summary(&self) -> Value {
        summary_of(&self.published(), now_unix())
    }

    /// Deep diagnosis' proxy layer (D11): u60-api's `probe_exit` on this mihomo.
    pub fn probe_exit(&self) -> (String, Option<NodeProbe>) {
        let m = Mihomo::new(&self.host, &self.state_file);
        luyaz_api::chill::probe_exit(&|path, slow| m.call("GET", path, None, slow))
    }
}

/// The machine's `say` logged each notice as it was set (u60-policy's
/// leaves it to the host): a tick that set one has a new notice tuple, and a cleared one is not
/// logged.
fn log_notice(before: &Persist, after: &Persist) {
    if let Some(text) = notice_set(before, after) {
        eprintln!("[manual_first] {text}");
    }
}

fn notice_set<'a>(before: &Persist, after: &'a Persist) -> Option<&'a str> {
    let changed = (&before.notice, &before.notice_en, before.notice_at, &before.notice_kind)
        != (&after.notice, &after.notice_en, after.notice_at, &after.notice_kind);
    (changed && !after.notice.is_empty()).then_some(after.notice.as_str())
}

/* -------------------------------------------------------------- *
 *  Shadow records for u60d
 * -------------------------------------------------------------- */

// u60d replays each tick through its copy of [`Machine`] (it must not probe nodes itself), so
// the tick's inputs, every `Io` answer in order, and the whole machine before and after go to
// [`Host::tick`]. Ticks that changed nothing and asked nothing but an idle `in_use` are left out
// and counted in the next record's `quiet`: replaying them could not differ.

/// Wraps the real `Io` and writes down each call and its answer (u60-policy's `Event` shape).
struct Recorder<'a> {
    inner: &'a mut dyn Io,
    events: Vec<Value>,
}

fn probe_json(p: Option<Probe>) -> Value {
    match p {
        Some(Probe::Ms(ms)) => json!({"ms": ms}),
        Some(Probe::Timeout) => json!("timeout"),
        None => Value::Null,
    }
}

impl Io for Recorder<'_> {
    fn in_use(&mut self) -> bool {
        let got = self.inner.in_use();
        self.events.push(json!({"call": "in_use", "got": got}));
        got
    }
    fn probe(&mut self, node: &str) -> Option<Probe> {
        let got = self.inner.probe(node);
        self.events.push(json!({"call": "probe", "node": node, "got": probe_json(got)}));
        got
    }
    fn cell_alive(&mut self) -> bool {
        let got = self.inner.cell_alive();
        self.events.push(json!({"call": "cell_alive", "got": got}));
        got
    }
    fn direct_ms(&mut self) -> Option<u32> {
        let got = self.inner.direct_ms();
        self.events.push(json!({"call": "direct_ms", "got": got}));
        got
    }
    fn region_of(&mut self, node: &str) -> Option<String> {
        let got = self.inner.region_of(node);
        self.events.push(json!({"call": "region_of", "node": node, "got": got}));
        got
    }
    fn retest(&mut self, group: &str) -> Option<(String, bool)> {
        let got = self.inner.retest(group);
        self.events.push(json!({"call": "retest", "group": group, "got": got}));
        got
    }
    fn select_main(&mut self, member: &str) -> bool {
        let ok = self.inner.select_main(member);
        self.events.push(json!({"call": "select_main", "member": member, "ok": ok}));
        ok
    }
    fn drop_manual_conns(&mut self) {
        self.inner.drop_manual_conns();
        self.events.push(json!({"call": "drop_manual_conns"}));
    }
    fn save(&mut self, p: &Persist) {
        self.inner.save(p);
        self.events.push(json!({"call": "save"}));
    }
}

/// Built by hand (not `to_value(Machine)`): this is the record shape u60d parses, and the
/// contract test below holds it to u60-policy's.
fn machine_json(m: &Machine) -> Value {
    json!({
        "p": m.p,
        "manual": m.manual,
        "timeouts": m.timeouts,
        "bad": m.bad,
        "good_since": m.good_since,
        "last_probe": m.last_probe,
    })
}

/// The machine changed, or the tick asked more than an idle `in_use`.
fn worth_a_record(before: &Machine, after: &Machine, events: &[Value]) -> bool {
    before != after || events.iter().any(|e| *e != json!({"call": "in_use", "got": false}))
}

fn tick_record(o: &Obs, detail: Option<(String, bool)>, before: &Machine, events: &[Value], after: &Machine, quiet: u64) -> Value {
    json!({
        "kind": "tick",
        "obs": {"now": o.now, "active": o.active, "main_now": o.main_now, "manual_now": o.manual_now},
        // what `needs_detail` said about 🚀's value (null: CHILL not running, not asked)
        "detail": detail.map(|(main, d)| json!({"main": main, "needs": d})),
        "before": machine_json(before),
        "io": events,
        "after": machine_json(after),
        "quiet": quiet,
    })
}

struct Mihomo {
    api: String,
    fast: ureq::Agent,
    slow: ureq::Agent,
    uploads: HashMap<String, u64>,
    providers: HashMap<String, String>,
    host: Arc<dyn Host>,
    state_file: PathBuf,
}

impl Mihomo {
    fn new(host: &Arc<dyn Host>, state_file: &Path) -> Self {
        let agent = |secs| -> ureq::Agent {
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(secs)))
                .http_status_as_error(false)
                .build()
                .into()
        };
        Mihomo {
            api: host.mihomo_api(),
            fast: agent(3),
            // A 3 s delay test needs more than 3 s of HTTP, or every probe
            // would look like a node timeout.
            slow: agent(15),
            uploads: HashMap::new(),
            providers: HashMap::new(),
            host: Arc::clone(host),
            state_file: state_file.to_path_buf(),
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
        let prov = self.provider_of(node)?;
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

    /// 🚀 first (~0.4 KB); the mode and 🎯 only when `needs_detail` says the
    /// tick can use them ([`Machine::needs_detail`]).
    fn observe(&mut self, needs_detail: impl Fn(&str) -> bool) -> Obs {
        let now = now_unix();
        if !self.host.chill_running() {
            return Obs { now, ..Default::default() };
        }
        let main_now = self.group_now(MAIN_GROUP);
        if !needs_detail(&main_now) {
            return Obs { now, active: true, main_now, manual_now: String::new() };
        }
        let rule = self.get("/configs").and_then(|c| c.get("mode").and_then(Value::as_str).map(str::to_string)).as_deref()
            == Some("rule");
        if !rule {
            return Obs { now, ..Default::default() };
        }
        Obs { now, active: true, main_now, manual_now: self.group_now(MANUAL_GROUP) }
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
        self.host.cell_alive()
    }

    fn direct_ms(&mut self) -> Option<u32> {
        self.host.direct_dns_ms()
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
            let _ = atomic_write(&self.state_file, &s);
        }
    }
}

/// Atomic write: write `<path>.tmp`, keeping the old file's mode, then rename.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let keep = fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o7777);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&tmp)?;
        // After open, not via OpenOptions::mode: that one is masked by the
        // umask and ignored for a .tmp left over from a crash.
        if let Some(m) = keep {
            f.set_permissions(fs::Permissions::from_mode(m))?;
        }
        f.write_all(data)?;
    }
    fs::rename(&tmp, path)
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests;
