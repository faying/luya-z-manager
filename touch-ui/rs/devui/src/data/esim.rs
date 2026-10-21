//! The eSIM page. luyaz-core does the work (/api/esim/*: it switches profiles and gets the
//! modem to read the new card); the screen only lists the profiles, shows the core's job and
//! asks for a switch, which takes two taps on the same row.

use super::core_api::CoreApi;
use super::util::{bytes_cap, find, find_byte, jstr, jtrue, mono_ms, objects, text, trunc};
use crate::cf;
use crate::json;
use crate::lang::tr;
use crate::source::{ESIM_SEL_ARMED, ESIM_SEL_BUSY, ESIM_SEL_COOLDOWN, ESIM_SEL_CURRENT, ESIM_SEL_FAIL, ESIM_SEL_STARTED};
use devui_model::esim_profile_t;

/// how often the job is read while the page is open
const ES_IDLE_MS: i64 = 2000;
/// … and while a switch started here is running
const ES_JOB_MS: i64 = 1000;
/// the core did not answer: ask less often
const ES_BACKOFF_MS: i64 = 5000;
/// the second tap has to come within this
const ES_ARM_MS: i64 = 4000;
/// longer than all of the core's switch steps together (luyaz-esim: enable 30 s, simreset 15 s,
/// mdm restart 30 s, IDENTITY_WAIT 45 s, DATA_WAIT 60 s, and the reads); the web waits as long
const ES_GIVEUP_MS: i64 = 240000;
const ES_MAX: usize = 16;
/// profiles can carry a base64 icon
const ES_RESP_MAX: usize = 65536;

#[derive(Clone, Default, PartialEq)]
struct Prof {
    iccid: String,
    name: String,
    sub: String,
    enabled: bool,
}

pub struct Esim {
    prof: Vec<Prof>,
    loaded: bool,
    err: String,
    offline: bool,
    seen_id: i64,
    seen_status: String,
    busy: bool,
    busy_kind: String,
    /// the core is showing its last list while the card settles (`stale`): listed, not switchable
    settling: bool,
    /// the job of a switch asked for here that has not finished; 0 = none
    my_job: i64,
    target_iccid: String,
    target_name: String,
    t0: i64,
    msg: String,
    /// the last switch to this card ended in catBusy: only a restart or reseating the card helps
    busy_iccid: String,
    msg_ms: i64,
    arm_iccid: String,
    arm_ms: i64,
    was_active: bool,
    poll_ms: i64,
    poll_gap: i64,
    sig: Option<String>,
    prefetched: String,
}

impl Default for Esim {
    fn default() -> Self {
        Esim {
            prof: Vec::new(),
            loaded: false,
            err: String::new(),
            offline: false,
            seen_id: -1,
            seen_status: String::new(),
            busy: false,
            busy_kind: String::new(),
            settling: false,
            my_job: 0,
            target_iccid: String::new(),
            target_name: String::new(),
            t0: 0,
            msg: String::new(),
            busy_iccid: String::new(),
            msg_ms: 0,
            arm_iccid: String::new(),
            arm_ms: 0,
            was_active: false,
            poll_ms: 0,
            poll_gap: ES_IDLE_MS,
            sig: None,
            prefetched: String::new(),
        }
    }
}

impl Prof {
    /// One profile object; None without an ICCID.
    fn read(o: &[u8]) -> Option<Prof> {
        let iccid = jstr(o, "iccid", 24);
        if iccid.is_empty() {
            return None;
        }
        let official = jstr(o, "profileName", 96);
        let nick = jstr(o, "profileNickname", 96);
        let carrier = jstr(o, "serviceProviderName", 64);
        let name = [&nick, &official].into_iter().find(|s| !s.is_empty()).unwrap_or(&iccid);
        // second line: carrier, the real name when a nickname hides it, the last 4 digits
        let mut sub = String::new();
        for part in [&carrier, if nick.is_empty() { &nick } else { &official }] {
            if !part.is_empty() {
                sub += part;
                sub += " · ";
            }
        }
        let sub = trunc(&sub, 192);
        let digits = iccid.as_bytes();
        let last4 = text(&digits[digits.len().saturating_sub(4)..]);
        let room = 192 - sub.len();
        Some(Prof {
            name: trunc(name, 96),
            sub: sub + &bytes_cap(&cf!(tr("尾号 %s"), &last4), room),
            enabled: jstr(o, "profileState", 16) == "enabled",
            iccid,
        })
    }
}

/// The profiles in a list text, at most ES_MAX of those that have an ICCID.
fn parse_profiles(list: &[u8]) -> Vec<Prof> {
    let Some(open) = find_byte(list, b'[', 0) else { return Vec::new() };
    objects(&list[open..]).into_iter().filter_map(Prof::read).take(ES_MAX).collect()
}

/// What GET /api/esim/profiles came back with.
enum Listing {
    Unreachable,
    BadLogin,
    NoCard,
    NotInstalled,
    /// the core is busy with the card and did not read it
    Busy,
    Got(Vec<Prof>, bool),
}

fn listing(code: i32, body: Option<&[u8]>) -> Listing {
    let Some(body) = body.filter(|_| code != 0) else { return Listing::Unreachable };
    if code == 401 {
        return Listing::BadLogin;
    }
    // a 502 is lpac failing to read a card, which mostly means an ordinary SIM
    let Some(data) = json::get(body, "data", ES_RESP_MAX).filter(|_| code == 200) else { return Listing::NoCard };
    if json::get(&data, "installed", 16).is_some_and(|v| v == b"false") {
        return Listing::NotInstalled;
    }
    if jtrue(&data, "busy") {
        return Listing::Busy;
    }
    match json::get(&data, "profiles", ES_RESP_MAX) {
        Some(list) => Listing::Got(parse_profiles(&list), jtrue(&data, "stale")),
        None => Listing::NoCard,
    }
}

/// The core's job slot: one job at a time, the last one stays until the next.
struct Job {
    id: i64,
    status: String,
    message: String,
    kind: String,
    rebooting: bool,
    /// set by cores from 10-04 on; older ones only say it in the message
    reason: String,
}

impl Job {
    fn read(data: &[u8]) -> Job {
        Job {
            id: json::get_int(data, "id", 0),
            status: jstr(data, "status", 16),
            message: jstr(data, "message", 256),
            kind: jstr(data, "kind", 16),
            rebooting: jstr(data, "rebooting", 8) == "true",
            reason: jstr(data, "reason", 24),
        }
    }
}

impl Esim {
    /// Read the profile list. false when the core did not answer.
    fn load_list(&mut self, agent: &mut CoreApi) -> bool {
        let (code, body) = agent.api("GET", "/api/esim/profiles", None, ES_RESP_MAX);
        let problem = match listing(code, body.as_deref()) {
            Listing::Unreachable => {
                self.err = trunc(tr("连不上管理后台"), 96);
                return false;
            }
            Listing::BadLogin => {
                self.err = trunc(tr("登录管理后台失败（密码不对？）"), 96);
                return true;
            }
            // the core will not touch the card now; the old list stands
            Listing::Busy => return true,
            Listing::Got(prof, settling) => {
                self.prof = prof;
                self.settling = settling;
                self.err.clear();
                self.loaded = true;
                return true;
            }
            Listing::NoCard => tr("未读到 eSIM 卡"),
            Listing::NotInstalled => tr("未安装 eSIM 组件"),
        };
        self.err = trunc(problem, 96);
        self.prof.clear();
        true
    }

    /// How a switch asked for here ended, from the job slot; None while it still runs.
    fn switch_outcome(&mut self, job: &Job) -> Option<String> {
        let secs = (mono_ms() - self.t0) / 1000;
        let switched = || bytes_cap(&cf!(tr("已切换到 %s（%ld 秒）"), &self.target_name, secs), 160);
        if job.id > self.my_job && job.kind == "notifications" {
            // the slot moved on to the notifications the core sends after a switch that worked
            // without a reboot: so ours worked
            return Some(switched());
        }
        if job.id != self.my_job {
            // the core restarted and our job went with it
            return Some(trunc(tr("切换结果未知，请看当前配置"), 160));
        }
        match job.status.as_str() {
            "done" if job.rebooting => Some(trunc(tr("已切换，设备即将重启"), 160)),
            // card and modem are on the new profile but the data call is not back (no reboot)
            "done" if job.reason == "no_data" => Some(trunc(tr("已切换，数据还没恢复"), 160)),
            "done" => Some(switched()),
            "error" if job.reason == "card_busy" || job.message.contains("card is busy") => {
                // catBusy: waiting is no use, the card needs power cycling (its row says so)
                self.busy_iccid = self.target_iccid.clone();
                Some(trunc(tr("卡忙没切成"), 160))
            }
            "error" => Some(bytes_cap(&cf!(tr("切换失败：%s"), &job.message), 160)),
            _ => None,
        }
    }

    /// Read the job slot. `reload` is set when a job just ended, since the list may have changed.
    /// false when the core did not answer.
    fn poll_job(&mut self, agent: &mut CoreApi, reload: &mut bool) -> bool {
        let (code, body) = agent.api("GET", "/api/esim/job", None, ES_RESP_MAX);
        let Some(data) = body.filter(|_| code == 200).and_then(|b| json::get(&b, "data", 1024)) else { return false };
        let job = Job::read(&data);
        self.busy_kind = job.kind.clone();
        self.busy = job.status == "running";
        let changed = job.id != self.seen_id || job.status != self.seen_status;
        if !self.busy && self.seen_id >= 0 && changed {
            // whatever ended (a download or delete from the web changes the list too)
            *reload = true;
            // somebody else's job makes our last result old news; the notifications the core
            // sends after our own switch do not
            if job.id != self.my_job && job.kind != "notifications" {
                self.msg.clear();
                self.busy_iccid.clear();
            }
        }
        self.seen_id = job.id;
        self.seen_status = trunc(&job.status, 16);

        if self.my_job == 0 {
            return true;
        }
        let Some(msg) = self.switch_outcome(&job) else { return true };
        let secs = (mono_ms() - self.t0) / 1000;
        eprintln!("esim: job {} {} after {secs} s: {}", self.my_job, job.status, job.message);
        self.msg = msg;
        self.msg_ms = mono_ms();
        self.my_job = 0;
        *reload = true;
        true
    }

    /// Everything the page draws, to tell whether it needs drawing again.
    fn display_sig(&self, now: i64) -> String {
        let running_s = if self.my_job != 0 { (now - self.t0) / 1000 } else { 0 };
        let armed = !self.arm_iccid.is_empty() && now - self.arm_ms <= ES_ARM_MS;
        let flags = [self.busy, self.settling, self.offline].map(|f| (f as i32).to_string()).join("/");
        let mut s = format!("{}/{flags}/{}/{running_s}/{}", self.prof.len(), self.my_job, armed as i32);
        for line in [&self.err, &self.msg, &self.busy_kind, &self.arm_iccid] {
            s += "\u{1}";
            s += line;
        }
        for p in &self.prof {
            s += &format!("\u{2}{}\u{1}{}\u{1}{}\u{1}{}", p.iccid, p.name, p.sub, p.enabled as i32);
        }
        s
    }

    /// Opening the page: no armed row, and a result from more than a minute ago is not shown
    /// again.
    fn on_open(&mut self, now: i64) {
        self.arm_iccid.clear();
        if self.my_job == 0 && now - self.msg_ms > 60000 {
            self.msg.clear();
            self.busy_iccid.clear();
        }
    }

    /// active: the eSIM page is on a lit screen. With the page closed only a switch started here
    /// is followed; opening it reads the list straight away. true when what shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        if !active {
            self.was_active = false;
            if self.my_job == 0 {
                return false;
            }
        }
        let now = mono_ms();
        let opening = active && !self.was_active;
        if active {
            self.was_active = true;
        }
        if opening {
            self.on_open(now);
        }
        let gap = if self.my_job != 0 { ES_JOB_MS } else { self.poll_gap };
        if opening || now - self.poll_ms >= gap {
            self.poll_ms = now;
            let mut reload = false;
            let mut ok = self.poll_job(agent, &mut reload);
            if self.my_job != 0 && now - self.t0 > ES_GIVEUP_MS {
                self.my_job = 0;
                self.msg = trunc(tr("切换超时，请看当前配置"), 160);
                self.msg_ms = now;
                reload = true;
            }
            // the list costs an lpac run: read it on opening, after a job, or once the core is
            // back, never every round (a plain SIM would be asked every 2 s for nothing)
            if ok && active && (opening || reload || self.offline) {
                ok = self.load_list(agent);
            }
            if !ok {
                self.err = trunc(tr("连不上管理后台"), 96);
                self.busy = false;
            }
            self.offline = !ok;
            self.poll_gap = if ok { ES_IDLE_MS } else { ES_BACKOFF_MS };
        }
        let sig = self.display_sig(now);
        if self.sig.as_deref() == Some(sig.as_str()) {
            return false;
        }
        self.sig = Some(sig);
        true
    }

    pub fn loaded(&self) -> bool {
        self.loaded
    }

    fn enabled(&self) -> Option<&Prof> {
        self.prof.iter().find(|p| p.enabled)
    }

    pub fn enabled_iccid(&self) -> &str {
        self.enabled().map_or("", |p| &p.iccid)
    }

    /// Read the list once per card with the page closed (key: the modem's ICCID, or its IMSI),
    /// so 蜂窝 can tell a plain SIM from an eSIM. Once only, whatever comes back: lpac holds the
    /// screen up and pokes the card.
    pub fn prefetch(&mut self, agent: &mut CoreApi, key: &str) -> bool {
        let key = trunc(key, 32);
        if key.is_empty() || key == self.prefetched || self.my_job != 0 {
            return false;
        }
        self.prefetched = key;
        self.load_list(agent)
    }

    pub fn current(&self) -> &str {
        self.enabled().map_or("-", |p| &p.name)
    }

    /// The status line in the screen's language; for showing only.
    pub fn state(&self) -> String {
        if self.my_job != 0 {
            return bytes_cap(&cf!(tr("切换中 · 已 %ld 秒"), (mono_ms() - self.t0) / 1000), 192);
        }
        let line = if self.busy {
            // one sentence per kind, so each language can order it its own way
            match self.busy_kind.as_str() {
                "switch" => tr("网页端正在切换"),
                "download" => tr("网页端正在下载"),
                "delete" => tr("网页端正在删除"),
                "notifications" => tr("正在发送 eSIM 通知"),
                _ => tr("网页端正在操作"),
            }
        } else if !self.err.is_empty() {
            self.err.as_str()
        } else if !self.msg.is_empty() {
            self.msg.as_str()
        } else if self.settling {
            tr("卡片还在稳定，列表稍后更新")
        } else {
            tr("就绪")
        };
        line.to_string()
    }

    pub fn ready(&self) -> bool {
        self.my_job == 0 && !self.busy && !self.settling && self.err.is_empty() && self.msg.is_empty()
    }

    pub fn profiles(&self) -> Vec<esim_profile_t> {
        let now = mono_ms();
        let armed = (now - self.arm_ms <= ES_ARM_MS).then_some(self.arm_iccid.as_str()).filter(|a| !a.is_empty());
        let going = (self.my_job != 0).then_some(self.target_iccid.as_str());
        let busy_card = Some(self.busy_iccid.as_str()).filter(|b| !b.is_empty());
        self.prof
            .iter()
            .map(|p| {
                let here = Some(p.iccid.as_str());
                esim_profile_t {
                    name: p.name.clone(),
                    sub: p.sub.clone(),
                    enabled: p.enabled as i32,
                    armed: (armed == here) as i32,
                    going: (going == here) as i32,
                    card_busy: (going != here && busy_card == here) as i32,
                }
            })
            .collect()
    }

    pub fn switching(&self) -> bool {
        self.my_job != 0
    }

    /// The seconds in the core's cooldown answer ("… wait 480s and retry"), 0 if it gives none.
    fn cooldown_secs(body: Option<&[u8]>) -> i64 {
        let err = body.map(|b| jstr(b, "error", 128)).unwrap_or_default();
        find(err.as_bytes(), b"wait ", 0).map_or(0, |at| json::atol(&err.as_bytes()[at + 5..]))
    }

    /// A tap on row i. The first tap arms the row, a second one on it within 4 s asks for the
    /// switch: a switch drops the data and may end in a reboot. Returns an ESIM_SEL_* code.
    pub fn select(&mut self, agent: &mut CoreApi, i: usize) -> i32 {
        let now = mono_ms();
        let Some(p) = self.prof.get(i).cloned() else { return ESIM_SEL_FAIL };
        if self.busy || self.settling || self.my_job != 0 {
            return ESIM_SEL_BUSY;
        }
        if p.enabled {
            self.arm_iccid.clear();
            return ESIM_SEL_CURRENT;
        }
        // armed by ICCID, so a list read between the taps does not move it to another row
        let second_tap = self.arm_iccid == p.iccid && now - self.arm_ms <= ES_ARM_MS;
        if !second_tap {
            self.arm_iccid = p.iccid.clone();
            self.arm_ms = now;
            return ESIM_SEL_ARMED;
        }
        self.arm_iccid.clear();
        let ask = trunc(&format!(r#"{{"iccid":"{}"}}"#, p.iccid), 64);
        let (code, body) = agent.api("POST", "/api/esim/switch", Some(&ask), ES_RESP_MAX);
        match code {
            409 => {
                self.busy = true;
                return ESIM_SEL_BUSY;
            }
            429 => {
                // the core holds off after the card really answered catBusy, and says how long
                let wait = Self::cooldown_secs(body.as_deref());
                self.msg = if wait > 0 {
                    bytes_cap(&cf!(tr("卡刚报过忙，还要等 %d 秒再试"), wait), 160)
                } else {
                    trunc(tr("卡刚报过忙，稍后再试"), 160)
                };
                self.msg_ms = mono_ms();
                return ESIM_SEL_COOLDOWN;
            }
            _ => {}
        }
        let job = body.filter(|_| code == 200).and_then(|b| json::get(&b, "data", 256)).map_or(0, |d| json::get_int(&d, "job_id", 0));
        if job <= 0 {
            self.my_job = 0;
            eprintln!("esim: switch request failed (HTTP {code})");
            return ESIM_SEL_FAIL;
        }
        eprintln!("esim: switch to {} started (job {job})", p.iccid);
        self.my_job = job;
        self.target_iccid = p.iccid;
        self.target_name = p.name;
        self.t0 = now;
        self.poll_ms = now;
        self.busy = true;
        self.msg.clear();
        self.busy_iccid.clear();
        ESIM_SEL_STARTED
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::testsrv::canned;

    #[test]
    fn profiles_and_two_taps() {
        crate::lang::set_en(false);
        let p = parse_profiles(
            r#"[{"iccid":"89860000000000000001","profileName":"CU","profileNickname":"主号","serviceProviderName":"中国联通","profileState":"enabled"},{"iccid":"","profileName":"x"},{"iccid":"89852000000000004567","profileName":"Ubigi","profileNickname":null,"serviceProviderName":"","profileState":"disabled"}]"#.as_bytes(),
        );
        assert_eq!(p.len(), 2, "no iccid: skipped");
        assert!(p[0].name == "主号" && p[0].sub == "中国联通 · CU · 尾号 0001" && p[0].enabled);
        assert!(p[1].name == "Ubigi" && p[1].sub == "尾号 4567" && !p[1].enabled);

        // the switch: first tap arms, second sends; 429 says how long to wait
        let (port, seen, h) = canned(
            vec![
                "HTTP/1.0 429 X\r\n\r\n{\"error\":\"card busy recently: wait 480s and retry\"}".into(),
                "HTTP/1.0 200 OK\r\n\r\n{\"ok\":true,\"data\":{\"job_id\":12}}".into(),
            ],
            2,
        );
        let mut a = CoreApi::default();
        a.config(port);
        let mut e = Esim { prof: p, ..Default::default() };
        assert_eq!(e.select(&mut a, 0), ESIM_SEL_CURRENT);
        assert_eq!(e.select(&mut a, 1), ESIM_SEL_ARMED);
        assert_eq!(e.profiles()[1].armed, 1);
        assert_eq!(e.select(&mut a, 1), ESIM_SEL_COOLDOWN);
        assert_eq!(e.state(), "卡刚报过忙，还要等 480 秒再试");
        assert_eq!(e.select(&mut a, 1), ESIM_SEL_ARMED);
        assert_eq!(e.select(&mut a, 1), ESIM_SEL_STARTED);
        assert!(e.switching() && e.profiles()[1].going == 1 && e.state().starts_with("切换中"));
        assert_eq!(e.select(&mut a, 0), ESIM_SEL_BUSY);
        h.join().unwrap();
        assert!(seen.lock().unwrap()[1].ends_with(r#"{"iccid":"89852000000000004567"}"#));
    }

    #[test]
    fn a_settling_list_shows_and_refuses_a_switch() {
        crate::lang::set_en(false);
        let list = r#"{"iccid":"89860000000000000001","profileName":"CU","profileState":"enabled"},{"iccid":"89852000000000004567","profileName":"Ubigi","profileState":"disabled"}"#;
        let (port, _seen, h) = canned(
            vec![
                format!("HTTP/1.0 200 OK\r\n\r\n{{\"ok\":true,\"data\":{{\"installed\":true,\"stale\":true,\"updating\":true,\"age_s\":4,\"profiles\":[{list}]}}}}"),
                format!("HTTP/1.0 200 OK\r\n\r\n{{\"ok\":true,\"data\":{{\"installed\":true,\"profiles\":[{list}]}}}}"),
            ],
            2,
        );
        let mut a = CoreApi::default();
        a.config(port);
        let mut e = Esim::default();
        assert!(e.load_list(&mut a));
        assert_eq!(e.profiles().len(), 2, "the list still shows");
        assert_eq!(e.state(), "卡片还在稳定，列表稍后更新");
        assert!(!e.ready());
        assert_eq!(e.select(&mut a, 1), ESIM_SEL_BUSY, "no switch on a settling list");
        assert!(e.load_list(&mut a));
        assert!(e.ready() && e.state() == "就绪", "a fresh list clears it");
        h.join().unwrap();
    }
}
