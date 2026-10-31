//! Home's 情景 card and the agent's health: luyaz-core's GET /api/public/status
//! (no login), every 30 s on any page (the status bar's 「后台失联」 must be right everywhere),
//! at once when Home comes into view unless the agent is not answering.
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::core_api::CoreApi;
use super::util::{fnv, jstr, jtrue, mono_ms, FNV0};
use crate::json;
use crate::lang;
use ui_model::{agent_health_t, scenario_status_t};

/// a local request takes tens of ms; a stuck agent must not hold Home up
const SC_IO_MS: i32 = 800;
/// the engine switches no faster than every second scan: 30 s is enough
const SC_TTL_MS: i64 = 30000;
const SC_RESP_MAX: usize = 16384;
/// lost only after 2 minutes: an agent restart takes seconds
const SC_LOST_MS: i64 = 120000;
/// GET /api/scenario (the matched Wi-Fi, the judging dots): at most once a minute while Home is
/// up on a lit screen, sooner when the public summary changed
const SC_DETAIL_MS: i64 = 60000;
/// its data carries the whole config: room for it
const SC_DETAIL_MAX: usize = 65536;

/// What the card takes from GET /api/scenario's data (core 1.50); None = not the shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub current: String,
    pub candidate: String,
    pub hits: i32,
    pub enter_hits: i32,
    pub matched_id: String,
    pub matched_ssid: String,
    pub matched_since: i64,
    pub misses: i32,
    pub exit_misses: i32,
    pub cand_name: String,
    /// the SSID last matched for the scenario in force (sticky across reads)
    pub cur_ssid: String,
}

/// The display name of scenario `id` in the config (English from names_en when asked).
fn scenario_name(d: &[u8], id: &str, en: bool) -> String {
    if id.is_empty() {
        return String::new();
    }
    if en
        && let Some(ne) = json::get(d, "names_en", 4096)
    {
        let n = jstr(&ne, id, 48);
        if !n.is_empty() {
            return n;
        }
    }
    let Some(cfg) = json::get(d, "config", SC_DETAIL_MAX) else { return String::new() };
    let Some(arr) = json::get(&cfg, "scenarios", SC_DETAIL_MAX) else { return String::new() };
    let mut p = 0;
    while let Some((s, np)) = json::arr_next(&arr, p, 16384) {
        p = np;
        if jstr(&s, "id", 48) == id {
            return jstr(&s, "name", 48);
        }
    }
    String::new()
}

pub fn parse_detail(body: &[u8]) -> Option<Detail> {
    let d = json::get(body, "data", SC_DETAIL_MAX)?;
    // an older core has no enter_hits: nothing here it can say
    json::get(&d, "enter_hits", 16).filter(|v| v.first().is_some_and(u8::is_ascii_digit))?;
    let mut o = Detail {
        current: jstr(&d, "current", 48),
        candidate: jstr(&d, "candidate", 48),
        hits: json::get_int(&d, "hits", 0) as i32,
        enter_hits: json::get_int(&d, "enter_hits", 0) as i32,
        misses: json::get_int(&d, "misses", 0) as i32,
        exit_misses: json::get_int(&d, "exit_misses", 0) as i32,
        ..Default::default()
    };
    o.cand_name = scenario_name(&d, &o.candidate, lang::is_en());
    if let Some(m) = json::get(&d, "matched", 512)
        && m.first() == Some(&b'{')
    {
        o.matched_id = jstr(&m, "id", 48);
        o.matched_ssid = jstr(&m, "ssid", 96);
        o.matched_since = json::get_int(&m, "since", 0);
    }
    Some(o)
}

#[derive(Default)]
#[derive(Clone)]
pub struct Scenario {
    ok: bool,
    configured: bool,
    enabled: bool,
    wifi_off: bool,
    name: String,
    pin: String,
    last_switch: i64,
    abroad: bool,
    chill_on: i32,
    chill_back: bool,
    auto_direct: bool,
    mf_notice: String,
    mf_kind: String,
    mf_at: i64,
    mf_seen: i64,
    mf_stale: bool,
    home_on: i32,
    home_until: i64,
    home_fallback: bool,
    ai_gate: i32,
    ai_region: String,
    /// services.chill as last read (C keeps the buffer: a reply without it reads the old one)
    ch: Vec<u8>,
    /// changed locally (a switch from the screen): redraw on the next round
    dirty: bool,
    was_active: bool,
    poll_ms: i64,
    sig: u32,
    /// the last good read; 0 = never, counted from first_ms
    ok_ms: i64,
    first_ms: i64,
    unread: i32,
    h_checked: bool,
    h_bad: i32,
    h_warn: i32,
    /// GET /api/scenario's part, None = never read or the core has none
    detail: Option<Detail>,
    detail_ms: i64,
    detail_sig: u32,
    was_lit: bool,
}

impl Scenario {
    pub fn new() -> Scenario {
        Scenario {
            chill_on: -1,
            ..Default::default()
        }
    }

    /// Every refresh; active = Home is up, lit = and the screen is lit (the logged-in read).
    /// true = what shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool, lit: bool) -> bool {
        let just_shown = active && !self.was_active;
        // the screen lit again on Home: the logged-in part is read at once
        let just_lit = active && lit && !self.was_lit;
        self.was_lit = active && lit;
        let t = mono_ms();
        if self.first_ms == 0 {
            self.first_ms = t;
        }
        self.was_active = active;
        if self.poll_ms != 0 && t - self.poll_ms < SC_TTL_MS && !((just_shown || just_lit) && self.ok) {
            if !self.dirty {
                return false;
            }
            self.dirty = false;
            return true;
        }
        self.dirty = false;
        self.poll_ms = t;

        self.ok = false;
        let (code, b) = agent.http("GET", "/api/public/status", None, SC_RESP_MAX, SC_IO_MS);
        let data = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", SC_RESP_MAX)).unwrap_or_default();
        if !data.is_empty() {
            self.ok_ms = t;
            self.unread = json::get(&data, "alerts", 128).map_or(0, |al| json::get_int(&al, "unread", 0) as i32);
            match json::get(&data, "health", 128) {
                Some(h) => {
                    self.h_checked = jtrue(&h, "checked");
                    self.h_bad = json::get_int(&h, "bad", 0) as i32;
                    self.h_warn = json::get_int(&h, "warn", 0) as i32;
                }
                None => self.h_checked = false,
            }
            // services.chill.on: the master switch (an old agent has no such key: -1); the
            // manual-first notice too (notice / notice_en / notice_at / notice_kind)
            self.chill_on = -1;
            self.mf_notice.clear();
            self.mf_kind.clear();
            self.mf_at = 0;
            self.mf_stale = false;
            self.mf_seen = super::util::wall();
            if let Some(svc) = json::get(&data, "services", 4096)
                && let Some(ch) = json::get(&svc, "chill", 2048)
            {
                self.ch = ch;
                let ch = &self.ch;
                if let Some(v) = json::get(ch, "on", 8) {
                    self.chill_on = (v == b"true") as i32;
                }
                let zh = jstr(ch, "notice", 256);
                let en = jstr(ch, "notice_en", 256);
                // a flag in a node name: the device font has none, the country code instead
                if !zh.is_empty() {
                    self.mf_notice = super::chill::sanitize_name(lang::pick(&zh, &en), 192);
                }
                self.mf_kind = jstr(ch, "notice_kind", 12);
                self.mf_at = json::get_int(ch, "notice_at", 0);
            }
            self.home_on = 0;
            self.home_fallback = false;
            self.home_until = 0;
            if let Some(he) = json::get(&self.ch, "home_exit", 256)
                && he.first() == Some(&b'{')
            {
                self.home_on = jtrue(&he, "media") as i32 | (jtrue(&he, "ip") as i32) << 1 | (jtrue(&he, "rest") as i32) << 2;
                self.home_until = json::get_int(&he, "rest_until", 0);
                self.home_fallback = jtrue(&he, "fallback");
            }
            // the AI gate (luyaz-core engines/chill_ai_gate.rs): null when off
            self.ai_gate = 0;
            self.ai_region.clear();
            // way "ai" (rule mode): AI goes through 🤖 AI, the gate has nothing to say, but the
            // region still names where AI lands
            if let Some(g) = json::get(&self.ch, "ai_gate", 256)
                && g.first() == Some(&b'{')
            {
                if jstr(&g, "way", 12) != "ai" {
                    self.ai_gate = if jtrue(&g, "open") { 2 } else { 1 };
                }
                self.ai_region = jstr(&g, "region", 4);
            }
        }
        if data.is_empty() {
            self.mf_stale = !self.mf_notice.is_empty(); // the notice stays, dimmed, with its time
        }
        if !data.is_empty()
            && let Some(sc) = json::get(&data, "scenario", 1024)
        {
            self.ok = true;
            self.configured = jtrue(&sc, "configured");
            self.enabled = jtrue(&sc, "enabled");
            self.wifi_off = jtrue(&sc, "wifi_off");
            // English: name_en for the factory scenarios not renamed; display only
            self.name = if lang::is_en() { jstr(&sc, "name_en", 48) } else { String::new() };
            if self.name.is_empty() {
                self.name = jstr(&sc, "name", 48);
            }
            self.pin = jstr(&sc, "pin", 48);
            self.last_switch = json::get_int(&sc, "last_switch", 0);
            self.abroad = jtrue(&sc, "abroad");
            self.chill_back = jtrue(&sc, "chill_on_when_home");
            self.auto_direct = jtrue(&sc, "auto_direct");
        }

        // the logged-in read: on a lit Home only, once a minute or when the summary moved
        let pub_sig = fnv(fnv(FNV0, self.name.as_bytes()), format!("{}/{}/{}", self.last_switch, self.pin, self.enabled as i32).as_bytes());
        if active && lit && self.ok && (just_shown || just_lit || pub_sig != self.detail_sig || self.detail_ms == 0 || t - self.detail_ms >= SC_DETAIL_MS) {
            self.detail_sig = pub_sig;
            self.detail_ms = t;
            let (code, b) = agent.api_ms("GET", "/api/scenario", None, SC_DETAIL_MAX, SC_IO_MS);
            // 503 (engine not up), 404 / no new keys (older core), unreachable: the card as before
            let prev = self.detail.take();
            self.detail = b.filter(|_| code == 200).and_then(|b| parse_detail(&b)).map(|mut d| {
                // the Wi-Fi that brought it here, kept while the scans miss it
                d.cur_ssid = if d.matched_id == d.current && !d.matched_ssid.is_empty() {
                    d.matched_ssid.clone()
                } else {
                    prev.filter(|p| p.current == d.current).map(|p| p.cur_ssid).unwrap_or_default()
                };
                d
            });
        } else if pub_sig != self.detail_sig {
            // the scenario moved while the detail could not be read: it no longer belongs to it
            self.detail = None;
        }

        let nums = format!(
            "{}/{}/{}/{}/{}/{}/{}{}{}",
            self.ok as i32,
            self.configured as i32,
            self.enabled as i32,
            self.wifi_off as i32,
            self.last_switch,
            self.unread,
            self.abroad as i32,
            self.chill_on,
            self.chill_back as i32
        );
        let mut h = fnv(FNV0, nums.as_bytes());
        h = fnv(h, self.name.as_bytes());
        h = fnv(h, self.pin.as_bytes());
        h = fnv(h, self.mf_notice.as_bytes());
        h = fnv(h, self.mf_kind.as_bytes());
        h = fnv(h, if self.mf_stale { b"stale" } else { b"live" });
        h = fnv(h, format!("{}/{}/{}", self.home_on, self.home_until, self.home_fallback as i32).as_bytes());
        h = fnv(h, format!("{}/{}", self.ai_gate, self.ai_region).as_bytes());
        if let Some(d) = &self.detail {
            h = fnv(h, format!("{}/{}/{}/{}/{}/{}/{}", d.current, d.candidate, d.hits, d.enter_hits, d.matched_id, d.matched_ssid, d.matched_since).as_bytes());
            h = fnv(h, format!("{}/{}/{}/{}", d.misses, d.exit_misses, d.cand_name, d.cur_ssid).as_bytes());
        }
        if h == self.sig {
            return false;
        }
        self.sig = h;
        true
    }

    pub fn status(&self) -> scenario_status_t {
        scenario_status_t {
            available: (self.ok && self.configured) as i32,
            enabled: self.enabled as i32,
            name: self.name.clone(),
            wifi_off: self.wifi_off as i32,
            pin: self.pin.clone(),
            last_switch: self.last_switch,
            abroad: self.abroad as i32,
            chill_on: self.chill_on,
            chill_back: self.chill_back as i32,
            auto_direct: self.auto_direct as i32,
            mf_notice: self.mf_notice.clone(),
            mf_kind: self.mf_kind.clone(),
            mf_at: self.mf_at,
            mf_seen: self.mf_seen,
            mf_stale: self.mf_stale as i32,
            home_on: self.home_on,
            home_until: self.home_until,
            home_fallback: self.home_fallback as i32,
            ai_gate: self.ai_gate,
            ai_region: self.ai_region.clone(),
            ..self.detail_status()
        }
    }

    fn detail_status(&self) -> scenario_status_t {
        let Some(d) = &self.detail else { return scenario_status_t::default() };
        scenario_status_t {
            detail: 1,
            current: d.current.clone(),
            candidate: d.candidate.clone(),
            hits: d.hits,
            enter_hits: d.enter_hits,
            matched_id: d.matched_id.clone(),
            matched_ssid: d.matched_ssid.clone(),
            matched_since: d.matched_since,
            misses: d.misses,
            exit_misses: d.exit_misses,
            cand_name: d.cand_name.clone(),
            cur_ssid: d.cur_ssid.clone(),
            ..Default::default()
        }
    }

    /// CHILL's master switch through luyaz-core (logged in). Shown as asked at once; read
    /// again in 4 s (chill.sh takes a few seconds).
    pub fn chill_set(&mut self, agent: &mut CoreApi, on: bool) -> bool {
        let code = agent.post(if on { "/api/services/chill/enable" } else { "/api/services/chill/disable" });
        if !(200..300).contains(&code) {
            return false;
        }
        self.chill_on = on as i32;
        if on {
            self.chill_back = false;
        } else if self.abroad {
            self.chill_back = true;
        }
        self.dirty = true;
        self.sig = 0;
        self.poll_ms = mono_ms() - SC_TTL_MS + 4000;
        true
    }

    /// Read again on the next round (after pinning a scenario).
    pub fn kick(&mut self) {
        self.poll_ms = 0;
        self.sig = 0;
    }

    /// The agent lost (more than 2 minutes without a read, counted from the start if it never
    /// answered), its unread alerts and the device check's counts.
    pub fn health(&self) -> agent_health_t {
        let t = mono_ms();
        let since = if self.ok_ms != 0 { self.ok_ms } else { self.first_ms };
        let lost = if since != 0 && t - since > SC_LOST_MS { (t - since) / 1000 } else { 0 };
        agent_health_t {
            lost_secs: lost,
            unread: if lost != 0 { 0 } else { self.unread },
            checked: (lost == 0 && self.h_checked) as i32,
            bad: self.h_bad,
            warn: self.h_warn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_detail() {
        // the real reply carries the whole config (scenarios, actions, a nested "current"-like key)
        let mut cfg = String::from(r#"{"scenarios":["#);
        for i in 0..80 {
            cfg.push_str(&format!(r#"{{"id":"s{i}","name":"情景{i}","detect":{{"wifi":{{"ssids":["Net{i}"]}}}},"actions":[{{"path":"/api/wifi/radio","body":{{"current":"x","matched":null}}}}]}},"#));
        }
        cfg.push_str(r#"{"id":"home"}],"params":{"enter_hits":9}}"#);
        let b = format!(
            // serde_json's order (alphabetical): the config, with its own enter_hits, comes first
            r#"{{"ok":true,"data":{{"allowed_paths":["/api/wifi/radio"],"applied":true,"candidate":"away","config":{cfg},"current":"home","enabled":true,"enter_hits":2,"exit_misses":2,"hits":1,"matched":{{"id":"home","since":1790000000,"ssid":"Home-5G"}},"misses":0,"pin":null}}}}"#
        );
        assert!(b.len() > 8000);
        let d = parse_detail(b.as_bytes()).unwrap();
        assert_eq!((d.current.as_str(), d.candidate.as_str(), d.hits, d.enter_hits), ("home", "away", 1, 2));
        assert_eq!((d.matched_id.as_str(), d.matched_ssid.as_str(), d.matched_since), ("home", "Home-5G", 1_790_000_000));
        assert_eq!((d.misses, d.exit_misses), (0, 2));
        // the candidate's name from the config (s0..s79 then home; ask for one in the middle)
        let b2 = b.replace(r#""candidate":"away""#, r#""candidate":"s41""#);
        assert_eq!(parse_detail(b2.as_bytes()).unwrap().cand_name, "情景41");
        // nothing matched
        let d = parse_detail(br#"{"ok":true,"data":{"current":"away","candidate":null,"hits":0,"enter_hits":2,"matched":null}}"#).unwrap();
        assert!(d.matched_ssid.is_empty() && d.candidate.is_empty());
        // an older core: no enter_hits
        assert!(parse_detail(br#"{"ok":true,"data":{"current":"home","candidate":null,"hits":0}}"#).is_none());
        assert!(parse_detail(br#"{"ok":false,"error":"x"}"#).is_none());
    }
}
