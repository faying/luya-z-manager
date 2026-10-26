//! The AI gate (`CHILL_AI_STRICT=1` in chill.env): AI's traffic only ever leaves through the region
//! 🤖 AI is set to. While it does not go through 🤖 AI — CHILL not running (crashed, hot, paused,
//! idle, switched off) or mihomo's mode not `rule` (`direct` and `global` skip the rules) — the AI
//! domains do not resolve, until the way out is checked to land in that region: the cellular exit
//! itself (a Taiwan SIM with 🤖 AI on 🇹🇼 台湾), or in global mode GLOBAL's node.
//!
//! chill.sh owns the block file and closes it in the same dnsmasq restart that hands DNS back
//! (`restore_dns`), so a stop never waits on this loop. This loop does the rest, from local reads
//! every [`TICK_S`]:
//! - what 🤖 AI is set to (asked from mihomo every [`SEL_S`] while it runs), kept in
//!   [`REGION_FILE`] for when it does not (default TW);
//! - which way AI goes now ([`way`]);
//! - when it is not 🤖 AI: the exit's country, from [`LOOKUPS`] (only with somebody connected,
//!   backing off; a definite elsewhere is asked again only after [`ELSEWHERE_S`] or a new route or
//!   region), [`judge`]d: at least two answers and every one the region. Then the verdict
//!   goes to [`OK_FILE`] (cellular: tied to this boot and the default route's interface and
//!   address, which a redial or another SIM changes) and the gate opens
//!   (`chill.sh ai-gate off`); anything else keeps it closed (`… on`).
//!
//! Not covered: a client with its own encrypted DNS, or one going to an address it already knows.

use luyaz_api::alerts::{clock::BootTime, deadline::Deadline};
use luyaz_api::chill as ch;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// chill.sh's `$AI_OK`: `v2 <boot> <from> <until> direct <iface> <ipv4> <CC>`.
pub const OK_FILE: &str = "/tmp/chill.ai-ok";
/// chill.sh's `$AI_BLOCK` (dnsmasq's conf-dir): there = closed.
pub const BLOCK_FILE: &str = "/tmp/dnsmasq.d/luyaz-ai-block.conf";
/// 🤖 AI's region as last seen (`TW`), for while mihomo is not there to ask.
pub const REGION_FILE: &str = "/data/chill/ai-region";
const CHILL_SH: &str = "/data/chill/chill.sh";
const DEFAULT_REGION: &str = "TW";
pub const TICK_S: u64 = 5;
/// 🤖 AI's selection and mihomo's real mode (the panel can change it without the mode file): asked
/// this often while CHILL runs. `/configs` is large, hence the slower one, and only with somebody
/// connected.
const SEL_S: u64 = 60;
const MODE_S: u64 = 600;
/// A verdict holds this long; it is asked again (gate still open) in its last [`RECHECK_S`].
pub const OK_S: u64 = 12 * 3600;
pub const RECHECK_S: u64 = 3600;
/// Retry after an inconclusive lookup: doubling from the first, capped.
const RETRY_MIN_S: u64 = 30;
const RETRY_MAX_S: u64 = 600;
/// After a definite elsewhere: the same route only this much later (a new route or region at once).
const ELSEWHERE_S: u64 = 6 * 3600;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Three services that answer with a country code (IPv4 only, like the exit lookups).
pub const LOOKUPS: [&str; 3] = [
    "https://www.cloudflare.com/cdn-cgi/trace",
    "http://ip-api.com/json/?fields=status,countryCode",
    "https://ipinfo.io/json",
];
const MIXED_PORT: u16 = 7894;
const GLOBAL_GROUP: &str = "GLOBAL";

/// Which way AI's traffic leaves now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// through 🤖 AI: nothing to do
    Ai,
    /// straight out of the cellular link (CHILL not running, or mode `direct`)
    Cellular,
    /// through GLOBAL's node (mode `global`)
    Global,
}

pub fn way(running: bool, mode: &str) -> Way {
    match (running, mode) {
        (false, _) => Way::Cellular,
        (true, "direct") => Way::Cellular,
        (true, "global") => Way::Global,
        _ => Way::Ai,
    }
}

/// mihomo's mode as the mode file should say it, when it does not already (None: leave it).
pub fn mode_to_save<'a>(file: &str, api: Option<&'a str>) -> Option<&'a str> {
    let api = api.filter(|m| matches!(*m, "rule" | "global" | "direct"))?;
    let file = if file.is_empty() { "rule" } else { file };
    (api != file).then_some(api)
}

/// 🤖 AI's member → its country code.
pub fn region_code(group: &str) -> Option<&'static str> {
    [
        ("台湾", "TW"),
        ("日本", "JP"),
        ("新加坡", "SG"),
        ("美国", "US"),
    ]
    .into_iter()
    .find(|(name, _)| group.contains(name))
    .map(|(_, cc)| cc)
}

/// A country code out of one lookup's answer: Cloudflare's `loc=TW` line, or JSON's
/// `countryCode` / `country_code` / `country`. Two capital letters or nothing.
pub fn parse_cc(body: &str) -> Option<String> {
    let cc = if let Ok(v) = serde_json::from_str::<Value>(body) {
        if v["status"] == "fail" {
            return None;
        }
        ["countryCode", "country_code", "country"]
            .iter()
            .find_map(|k| v[*k].as_str().map(str::to_owned))?
    } else {
        body.lines()
            .find_map(|l| l.trim().strip_prefix("loc="))?
            .to_owned()
    };
    (cc.len() == 2 && cc.bytes().all(|b| b.is_ascii_uppercase())).then_some(cc)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    /// some service placed the exit elsewhere
    Other(String),
    /// fewer than two answers
    Unknown,
}

/// Every answer the region and at least two of them; one other answer is enough to say no.
pub fn judge(answers: &[Option<String>], region: &str) -> Verdict {
    let got: Vec<&String> = answers.iter().flatten().collect();
    if let Some(o) = got.iter().find(|c| c.as_str() != region) {
        return Verdict::Other((*o).clone());
    }
    if got.len() >= 2 {
        Verdict::Allowed
    } else {
        Verdict::Unknown
    }
}

/// The cellular verdict line chill.sh reads ([`OK_FILE`]).
pub fn ok_line(issued: BootTime, route_key: &str, cc: &str) -> Option<String> {
    let d = Deadline::new(issued, OK_S)?;
    Some(format!(
        "{} direct {route_key} {cc}\n",
        d.encode().trim_end()
    ))
}

/// [`OK_FILE`] still says yes for this route and region: the deadline left, if so.
pub fn ok_left(text: &str, now: Option<&BootTime>, route_key: &str, region: &str) -> Option<u64> {
    let f: Vec<&str> = text.split_ascii_whitespace().collect();
    if f.len() != 8
        || f[4] != "direct"
        || format!("{} {}", f[5], f[6]) != route_key
        || f[7] != region
    {
        return None;
    }
    let d = Deadline::decode(&f[..4].join(" "))?;
    let now = now?;
    d.active(Some(now)).then(|| d.until - now.seconds)
}

/// When to ask again after an inconclusive (or negative) answer.
#[derive(Debug, Default)]
pub struct Retry {
    key: String,
    next: u64,
    gap: u64,
}

impl Retry {
    /// A lookup may go out now for `key` (a new key goes at once).
    pub fn due(&self, key: &str, now: u64) -> bool {
        self.key != key || now >= self.next
    }

    pub fn failed(&mut self, key: &str, now: u64) {
        if self.key != key {
            self.gap = 0;
        }
        self.gap = (self.gap * 2).clamp(RETRY_MIN_S, RETRY_MAX_S);
        self.key = key.to_owned();
        self.next = now + self.gap;
    }

    /// A definite elsewhere: hold for [`ELSEWHERE_S`].
    pub fn elsewhere(&mut self, key: &str, now: u64) {
        self.key = key.to_owned();
        self.gap = 0;
        self.next = now + ELSEWHERE_S;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// ── what the screens see ────────────────────────────────────────────────────

fn shown() -> &'static Mutex<Value> {
    static S: OnceLock<Mutex<Value>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Value::Null))
}

/// `services.chill.ai_gate`: null when the gate is off, else
/// `{region, way: "ai"|"cellular"|"global", open, seen, checked_at}` (`seen`: the country the
/// last lookup found, `checked_at`: unix seconds).
pub fn view() -> Value {
    shown().lock().map(|v| v.clone()).unwrap_or(Value::Null)
}

// ── the loop ────────────────────────────────────────────────────────────────

struct Gate {
    region: String,
    sel_at: Option<u64>,
    mode_at: Option<u64>,
    file_mode: String,
    retry: Retry,
    /// GLOBAL's verdict: (node, until uptime)
    global_ok: Option<(String, u64)>,
    seen: Option<String>,
    checked_at: Option<u64>,
}

fn host(p: &str) -> PathBuf {
    PathBuf::from(crate::state::host_path(p))
}

fn read(p: &str) -> String {
    std::fs::read_to_string(host(p)).unwrap_or_default()
}

/// dnsmasq's running config (written at its every start) as chill.sh's apply_dns leaves it.
const DNSMASQ_CONF: &str = "/var/etc/dnsmasq.conf.lan_dns";

/// DNS goes to mihomo now: what decides whether AI can leave any other way (its answers are fake
/// addresses only mihomo's TUN routes). chill.state lags both ways (written after apply_dns, not
/// rewritten by every flush); it is only the fallback when dnsmasq's config cannot be read.
pub fn dns_on_mihomo(conf: &str) -> bool {
    conf.lines().any(|l| l.trim() == "server=127.0.0.1")
}

fn chill_running() -> bool {
    match std::fs::read_to_string(host(DNSMASQ_CONF)) {
        Ok(conf) => dns_on_mihomo(&conf),
        Err(_) => serde_json::from_str::<Value>(&read(ch::CHILL_STATE))
            .is_ok_and(|v| v["state"] == "running"),
    }
}

fn mihomo_get(secret: &str, path: &str) -> Option<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .into();
    let mut req = agent.get(format!("http://127.0.0.1:9999{path}"));
    if !secret.is_empty() {
        req = req.header("Authorization", format!("Bearer {secret}"));
    }
    req.call()
        .ok()
        .filter(|r| r.status().as_u16() < 400)
        .and_then(|mut r| r.body_mut().read_json::<Value>().ok())
}

/// GLOBAL's selection followed down to a node.
fn global_node(secret: &str) -> Option<String> {
    let mut at = GLOBAL_GROUP.to_owned();
    for _ in 0..4 {
        let v = mihomo_get(secret, &format!("/proxies/{}", ch::enc(&at)))?;
        match v["now"].as_str() {
            Some(n) if !n.is_empty() && n != at => at = n.to_owned(),
            _ => break,
        }
    }
    Some(at)
}

/// One country code per service, through mihomo's mixed port when `proxy`.
fn lookup(proxy: bool) -> Vec<Option<String>> {
    let proxy = proxy
        .then(|| ureq::Proxy::new(&format!("http://127.0.0.1:{MIXED_PORT}")).ok())
        .flatten();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(LOOKUP_TIMEOUT))
        .ip_family(ureq::config::IpFamily::Ipv4Only)
        .proxy(proxy)
        .build()
        .into();
    LOOKUPS
        .iter()
        .map(|url| {
            agent
                .get(*url)
                .header("User-Agent", "curl/8")
                .call()
                .ok()
                .and_then(|mut r| r.body_mut().read_to_string().ok())
                .and_then(|b| parse_cc(&b))
        })
        .collect()
}

/// `chill.sh ai-gate on|off`, at most 30 s.
fn set_gate(close: bool) {
    let arg = if close { "on" } else { "off" };
    let child = std::process::Command::new("sh")
        .arg(host(CHILL_SH))
        .args(["ai-gate", arg])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("[ai-gate] chill.sh ai-gate {arg}: could not start");
        return;
    };
    for _ in 0..300 {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    eprintln!("[ai-gate] chill.sh ai-gate {arg}: timed out");
}

impl Gate {
    fn tick(&mut self, hub: &crate::block::Hub) {
        let env = read(ch::CHILL_ENV);
        let closed = host(BLOCK_FILE).exists();
        if ch::env_var(&env, "CHILL_AI_STRICT") != "1" {
            if closed {
                set_gate(false);
            }
            *shown().lock().unwrap_or_else(|e| e.into_inner()) = Value::Null;
            self.retry.reset();
            return;
        }
        let secret = ch::env_var(&env, "CHILL_SECRET");
        let t = luyaz_netwatch::now_unix();
        let running = chill_running();
        let saved = read(REGION_FILE);
        self.region = match saved.trim() {
            cc @ ("TW" | "JP" | "SG" | "US") => cc,
            _ => DEFAULT_REGION,
        }
        .to_owned();
        if !running {
            self.sel_at = None;
            self.mode_at = None;
        } else if self.sel_at.is_none_or(|a| t.saturating_sub(a) >= SEL_S) {
            self.sel_at = Some(t);
            let sel = mihomo_get(&secret, &format!("/proxies/{}", ch::enc(ch::AI_GROUP)));
            if let Some(cc) = sel
                .as_ref()
                .and_then(|v| v["now"].as_str())
                .and_then(region_code)
                && cc != self.region
            {
                eprintln!("[ai-gate] 🤖 AI now on {cc} (was {})", self.region);
                let _ =
                    crate::api::ops::atomic_write(&host(REGION_FILE), format!("{cc}\n").as_bytes());
                self.region = cc.to_owned();
                // a verdict was for the old region: chill.sh must not trust it at the next stop
                let _ = std::fs::remove_file(host(OK_FILE));
                self.global_ok = None;
            }
        }
        let file_mode = read(ch::CHILL_MODE_FILE).trim().to_owned();
        // our own switches write the mode file first: ask mihomo again at once when it changes
        if self.file_mode != file_mode {
            self.file_mode = file_mode.clone();
            self.mode_at = None;
        }
        let clients = hub
            .view("clients")
            .filter(|v| !v.stale)
            .and_then(|v| v.data["total"].as_i64());
        // unknown is somebody (as in chill_idle)
        let somebody = clients.is_none_or(|n| n > 0);
        if running && somebody && self.mode_at.is_none_or(|a| t.saturating_sub(a) >= MODE_S) {
            self.mode_at = Some(t);
            let api =
                mihomo_get(&secret, "/configs").and_then(|v| v["mode"].as_str().map(str::to_owned));
            // a switch from the panel does not write the mode file: write it as chill.sh's
            // save_mode would, so chill.sh's minute tick and this loop read the same mode
            if let Some(m) = mode_to_save(&file_mode, api.as_deref()) {
                eprintln!("[ai-gate] mihomo's mode is {m} (mode file said {file_mode:?})");
                let _ = crate::api::ops::atomic_write(
                    &host(ch::CHILL_MODE_FILE),
                    format!("{m}\n").as_bytes(),
                );
                self.file_mode = m.to_owned();
            }
        }
        let mode = self.file_mode.clone();
        let w = way(running, &mode);
        let open = match w {
            // Opening for rule mode is chill.sh's alone (apply_dns, its minute tick), in the same
            // dnsmasq restart that puts DNS on mihomo.
            Way::Ai => {
                self.retry.reset();
                self.global_ok = None;
                !closed
            }
            Way::Cellular => self.cellular(t, somebody),
            Way::Global => self.global(t, somebody, &secret),
        };
        // Closing after the lookups (seconds): chill.sh may have taken DNS over and opened in the
        // meantime (10-24 on u60g). Skipping is safe: chill.sh closes by itself whenever it hands
        // DNS back.
        let overtaken = !open && way(chill_running(), read(ch::CHILL_MODE_FILE).trim()) == Way::Ai;
        if open == closed && !overtaken {
            set_gate(!open);
            eprintln!(
                "[ai-gate] {} ({w:?}, region {}, seen {:?})",
                if open { "open" } else { "closed" },
                self.region,
                self.seen
            );
        }
        let way_s = match w {
            Way::Ai => "ai",
            Way::Cellular => "cellular",
            Way::Global => "global",
        };
        *shown().lock().unwrap_or_else(|e| e.into_inner()) = json!({
            "region": self.region, "way": way_s, "open": open,
            "seen": self.seen, "checked_at": self.checked_at,
        });
    }

    /// Ask, judge, remember. Some(true) allowed, Some(false) elsewhere, None unknown.
    fn ask(&mut self, key: &str, t: u64, proxy: bool) -> Option<bool> {
        let answers = lookup(proxy);
        let v = judge(&answers, &self.region);
        self.checked_at = Some(t);
        match &v {
            Verdict::Allowed => self.seen = Some(self.region.clone()),
            Verdict::Other(cc) => self.seen = Some(cc.clone()),
            Verdict::Unknown => {}
        }
        eprintln!(
            "[ai-gate] {key}: {answers:?} → {v:?} (region {})",
            self.region
        );
        match v {
            Verdict::Allowed => {
                self.retry.reset();
                Some(true)
            }
            Verdict::Other(_) => {
                self.retry.elsewhere(key, t);
                Some(false)
            }
            Verdict::Unknown => {
                self.retry.failed(key, t);
                None
            }
        }
    }

    fn cellular(&mut self, t: u64, somebody: bool) -> bool {
        self.global_ok = None;
        let route = std::fs::read_to_string(host("/proc/net/route")).unwrap_or_default();
        let key = crate::api::exits::route_key(&route, crate::api::exits::ipv4_of);
        if key.is_empty() || key.ends_with(' ') {
            return false;
        }
        let now = BootTime::read();
        let left = ok_left(&read(OK_FILE), now.as_ref(), &key, &self.region);
        let want_ask = left.is_none_or(|l| l <= RECHECK_S);
        let rkey = format!("{key} {}", self.region);
        if want_ask && somebody && self.retry.due(&rkey, t) {
            match self.ask(&rkey, t, false) {
                Some(true) => {
                    if let Some(line) = now.and_then(|n| ok_line(n, &key, &self.region)) {
                        let _ = crate::api::ops::atomic_write(&host(OK_FILE), line.as_bytes());
                    }
                    return true;
                }
                Some(false) => {
                    let _ = std::fs::remove_file(host(OK_FILE));
                    return false;
                }
                None => {}
            }
        }
        left.is_some()
    }

    fn global(&mut self, t: u64, somebody: bool, secret: &str) -> bool {
        let Some(node) = global_node(secret) else {
            return false;
        };
        let up = BootTime::read().map(|b| b.seconds).unwrap_or(0);
        let left = self
            .global_ok
            .as_ref()
            .filter(|(n, until)| *n == node && up < *until)
            .map(|(_, until)| until - up);
        let key = format!("global {node} {}", self.region);
        if left.is_none_or(|l| l <= RECHECK_S) && somebody && self.retry.due(&key, t) {
            match self.ask(&key, t, true) {
                Some(true) => {
                    self.global_ok = Some((node, up + OK_S));
                    return true;
                }
                Some(false) => {
                    self.global_ok = None;
                    return false;
                }
                None => {}
            }
        }
        left.is_some()
    }
}

pub fn spawn(hub: Arc<crate::block::Hub>) {
    let _ = std::thread::Builder::new()
        .name("chill-ai-gate".into())
        .spawn(move || {
            let mut g = Gate {
                region: DEFAULT_REGION.into(),
                sel_at: None,
                mode_at: None,
                file_mode: String::new(),
                retry: Retry::default(),
                global_ok: None,
                seen: None,
                checked_at: None,
            };
            loop {
                g.tick(&hub);
                std::thread::sleep(Duration::from_secs(TICK_S));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bt(s: u64) -> BootTime {
        BootTime::from_uptime("boot-a", &format!("{s}.00 0.00")).unwrap()
    }

    #[test]
    fn ai_goes_through_its_group_only_in_rule_mode_with_chill_running() {
        assert_eq!(way(true, "rule"), Way::Ai);
        assert_eq!(way(true, ""), Way::Ai, "no mode file: rule");
        assert_eq!(way(true, "direct"), Way::Cellular);
        assert_eq!(way(true, "global"), Way::Global);
        assert_eq!(way(false, "rule"), Way::Cellular);
        assert_eq!(way(false, "global"), Way::Cellular);
    }

    #[test]
    fn dns_on_mihomo_from_dnsmasqs_running_config() {
        assert!(dns_on_mihomo(
            "conf-file=/etc/dnsmasq.conf\nno-resolv\nserver=127.0.0.1\n"
        ));
        assert!(!dns_on_mihomo(
            "resolv-file=/etc/resolv.conf\nconf-dir=/tmp/dnsmasq.d\n"
        ));
        assert!(
            !dns_on_mihomo("server=127.0.0.1#5353\n"),
            "another upstream on loopback"
        );
    }

    #[test]
    fn a_panel_switch_is_written_to_the_mode_file() {
        assert_eq!(mode_to_save("rule", Some("direct")), Some("direct"));
        assert_eq!(
            mode_to_save("direct", Some("rule")),
            Some("rule"),
            "back from the panel too"
        );
        assert_eq!(mode_to_save("", Some("global")), Some("global"));
        assert_eq!(
            mode_to_save("", Some("rule")),
            None,
            "no file reads as rule"
        );
        assert_eq!(mode_to_save("global", Some("global")), None);
        assert_eq!(mode_to_save("rule", None), None, "mihomo did not answer");
        assert_eq!(
            mode_to_save("rule", Some("script")),
            None,
            "not one of ours"
        );
    }

    #[test]
    fn region_from_the_group_name() {
        assert_eq!(region_code("🇹🇼 台湾"), Some("TW"));
        assert_eq!(region_code("🇯🇵 日本"), Some("JP"));
        assert_eq!(region_code("🇸🇬 新加坡"), Some("SG"));
        assert_eq!(region_code("🇺🇸 美国"), Some("US"));
        assert_eq!(
            region_code("TW"),
            None,
            "the region file holds codes, checked apart"
        );
        assert_eq!(region_code("DIRECT"), None);
    }

    #[test]
    fn country_codes_from_each_service() {
        assert_eq!(
            parse_cc("fl=1\nip=1.2.3.4\nloc=TW\ntls=TLSv1.3\n").as_deref(),
            Some("TW")
        );
        assert_eq!(
            parse_cc(r#"{"status":"success","countryCode":"JP"}"#).as_deref(),
            Some("JP")
        );
        assert_eq!(parse_cc(r#"{"status":"fail","countryCode":"JP"}"#), None);
        assert_eq!(
            parse_cc(r#"{"ip":"1.2.3.4","country":"TW"}"#).as_deref(),
            Some("TW")
        );
        assert_eq!(parse_cc(r#"{"country_code":"HK"}"#).as_deref(), Some("HK"));
        assert_eq!(
            parse_cc(r#"{"country":"Taiwan"}"#),
            None,
            "names are not codes"
        );
        assert_eq!(parse_cc("<html>blocked</html>"), None);
        assert_eq!(parse_cc("loc=tw"), None);
    }

    #[test]
    fn two_answers_all_the_region_and_one_other_is_no() {
        let s = |x: &str| Some(x.to_string());
        assert_eq!(judge(&[s("TW"), s("TW"), None], "TW"), Verdict::Allowed);
        assert_eq!(judge(&[s("TW"), None, None], "TW"), Verdict::Unknown);
        assert_eq!(judge(&[None, None, None], "TW"), Verdict::Unknown);
        assert_eq!(
            judge(&[s("TW"), s("HK"), s("TW")], "TW"),
            Verdict::Other("HK".into())
        );
        assert_eq!(
            judge(&[s("CN"), None, None], "TW"),
            Verdict::Other("CN".into())
        );
        assert_eq!(judge(&[s("JP"), s("JP")], "JP"), Verdict::Allowed);
    }

    #[test]
    fn the_verdict_line_is_what_chill_sh_reads_and_holds_for_its_route_and_region() {
        let line = ok_line(bt(100), "rmnet_data0 10.20.30.40", "TW").unwrap();
        assert_eq!(
            line,
            format!(
                "v2 boot-a 100 {} direct rmnet_data0 10.20.30.40 TW\n",
                100 + OK_S
            )
        );
        let k = "rmnet_data0 10.20.30.40";
        assert_eq!(ok_left(&line, Some(&bt(200)), k, "TW"), Some(OK_S - 100));
        assert_eq!(
            ok_left(&line, Some(&bt(200)), "rmnet_data0 10.20.30.41", "TW"),
            None,
            "redial"
        );
        assert_eq!(ok_left(&line, Some(&bt(200)), k, "JP"), None, "🤖 AI moved");
        assert_eq!(
            ok_left(&line, Some(&bt(100 + OK_S)), k, "TW"),
            None,
            "expired"
        );
        assert_eq!(
            ok_left(&line, Some(&bt(50)), k, "TW"),
            None,
            "before it was written"
        );
        let other_boot = BootTime::from_uptime("boot-b", "200.00 0.00").unwrap();
        assert_eq!(
            ok_left(&line, Some(&other_boot), k, "TW"),
            None,
            "another boot"
        );
        assert_eq!(ok_left(&line, None, k, "TW"), None, "no clock");
        assert_eq!(ok_left("garbage", Some(&bt(200)), k, "TW"), None);
    }

    #[test]
    fn retries_back_off_and_a_new_route_goes_at_once() {
        let mut r = Retry::default();
        assert!(r.due("a", 0));
        r.failed("a", 0);
        assert!(!r.due("a", RETRY_MIN_S - 1));
        assert!(r.due("a", RETRY_MIN_S));
        r.failed("a", RETRY_MIN_S);
        assert!(!r.due("a", RETRY_MIN_S + 2 * RETRY_MIN_S - 1), "doubled");
        assert!(r.due("b", RETRY_MIN_S + 1), "another route");
        for i in 0..20 {
            r.failed("a", i);
        }
        assert!(r.due("a", 19 + RETRY_MAX_S), "capped");
        r.elsewhere("a", 1000);
        assert!(!r.due("a", 1000 + ELSEWHERE_S - 1), "a definite no holds");
        assert!(r.due("a", 1000 + ELSEWHERE_S));
        assert!(r.due("a TW→JP", 1001), "another route or region at once");
    }
}
