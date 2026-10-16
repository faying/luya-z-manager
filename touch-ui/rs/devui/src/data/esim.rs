//! The eSIM page (src/esim.c): zte-agent's /api/esim/* (the agent switches and makes the modem
//! read the new card; this only lists and asks): the profile list, the job, two-tap switching.
// SPDX-License-Identifier: MIT

use super::core_api::CoreApi;
use super::util::{bytes_cap, find, find_byte, jstr, mono_ms, objects, text, trunc};
use crate::cf;
use crate::json;
use crate::lang::tr;
use crate::source::{ESIM_SEL_ARMED, ESIM_SEL_BUSY, ESIM_SEL_COOLDOWN, ESIM_SEL_CURRENT, ESIM_SEL_FAIL, ESIM_SEL_STARTED};
use devui_model::esim_profile_t;

/// the job, while the page is up
const ES_IDLE_MS: i64 = 2000;
/// a switch from this screen is running
const ES_JOB_MS: i64 = 1000;
/// the agent did not answer: don't stall every round
const ES_BACKOFF_MS: i64 = 5000;
const ES_ARM_MS: i64 = 4000;
/// the agent itself takes at most ~60 s
const ES_GIVEUP_MS: i64 = 120000;
const ES_MAX: usize = 16;
/// a profile may carry a base64 icon
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
    /// this screen's switch, not answered yet; 0 = none
    my_job: i64,
    target_iccid: String,
    target_name: String,
    t0: i64,
    msg: String,
    /// the last switch to this card got catBusy: restart the device or reseat the card
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

/// The profiles of the list (each object cut out on its own).
fn parse_profiles(arr: &[u8]) -> Vec<Prof> {
    let Some(p) = find_byte(arr, b'[', 0) else { return Vec::new() };
    let mut out = Vec::new();
    for o in objects(&arr[p..]) {
        if out.len() >= ES_MAX {
            break;
        }
        let iccid = jstr(o, "iccid", 24);
        let pname = jstr(o, "profileName", 96);
        let nick = jstr(o, "profileNickname", 96);
        let sp = jstr(o, "serviceProviderName", 64);
        let state = jstr(o, "profileState", 16);
        if iccid.is_empty() {
            continue;
        }
        let name = trunc(if !nick.is_empty() { &nick } else if !pname.is_empty() { &pname } else { &iccid }, 96);
        let ib = iccid.as_bytes();
        let tail = text(&ib[ib.len().saturating_sub(4)..]); // bytes, as C: a cut character must not panic
        // with a nickname, the original name too, or which one is it
        let mut sub = String::new();
        if !sp.is_empty() {
            sub.push_str(&sp);
            sub.push_str(" · ");
        }
        if !nick.is_empty() && !pname.is_empty() {
            sub.push_str(&pname);
            sub.push_str(" · ");
        }
        let sub = trunc(&sub, 192);
        let rest = bytes_cap(&cf!(tr("尾号 %s"), &tail), 192 - sub.len());
        out.push(Prof { iccid, name, sub: sub + &rest, enabled: state == "enabled" });
    }
    out
}

impl Esim {
    /// false = the agent did not answer
    fn load_list(&mut self, agent: &mut CoreApi) -> bool {
        let (code, b) = agent.api("GET", "/api/esim/profiles", None, ES_RESP_MAX);
        let Some(b) = b.filter(|_| code != 0) else {
            self.err = trunc(tr("连不上管理后台"), 96);
            return false;
        };
        if code == 401 {
            self.err = trunc(tr("登录管理后台失败（密码不对？）"), 96);
            return true;
        }
        // 502 = lpac could not read a card: most likely a plain SIM
        let Some(data) = json::get(&b, "data", ES_RESP_MAX).filter(|_| code == 200) else {
            self.err = trunc(tr("未读到 eSIM 卡"), 96);
            self.prof.clear();
            return true;
        };
        if json::get(&data, "installed", 16).is_some_and(|v| v == b"false") {
            self.err = trunc(tr("未安装 eSIM 组件"), 96);
            self.prof.clear();
            return true;
        }
        if json::get(&data, "busy", 16).is_some_and(|v| v == b"true") {
            return true; // something runs, the agent does not read the card: keep the old list
        }
        let Some(arr) = json::get(&data, "profiles", ES_RESP_MAX) else {
            self.err = trunc(tr("未读到 eSIM 卡"), 96);
            self.prof.clear();
            return true;
        };
        self.prof = parse_profiles(&arr);
        self.err.clear();
        self.loaded = true;
        true
    }

    /// The job. reload = something just ended (the list may have changed). false = no answer.
    fn poll_job(&mut self, agent: &mut CoreApi, reload: &mut bool) -> bool {
        let (code, b) = agent.api("GET", "/api/esim/job", None, ES_RESP_MAX);
        let Some(data) = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", 1024)) else { return false };
        let id = json::get_int(&data, "id", 0);
        let status = jstr(&data, "status", 16);
        let msg = jstr(&data, "message", 256);
        self.busy_kind = jstr(&data, "kind", 16);
        let reboot = jstr(&data, "rebooting", 8);
        let reason = jstr(&data, "reason", 24); // agent 10-04 on; older: see message
        self.busy = status == "running";

        // any job that ends counts (the web's downloads and deletes change the list too)
        if !self.busy && self.seen_id >= 0 && (id != self.seen_id || status != self.seen_status) {
            *reload = true;
            // someone else's job ended: this screen's last result is stale. The agent's own
            // notifications after a switch do not count: that switch's result stays
            if id != self.my_job && self.busy_kind != "notifications" {
                self.msg.clear();
                self.busy_iccid.clear();
            }
        }
        self.seen_id = id;
        self.seen_status = trunc(&status, 16);

        if self.my_job != 0 {
            let secs = (mono_ms() - self.t0) / 1000;
            if id > self.my_job && self.busy_kind == "notifications" {
                // missed the switch's result; the slot already holds the notifications the
                // agent sends after a successful switch (no reboot): it worked
                self.msg = bytes_cap(&cf!(tr("已切换到 %s（%ld 秒）"), &self.target_name, secs), 160);
            } else if id != self.my_job {
                // the agent restarted: the job is gone
                self.msg = trunc(tr("切换结果未知，请看当前配置"), 160);
            } else if status == "done" {
                self.msg = if reboot == "true" {
                    trunc(tr("已切换，设备即将重启"), 160)
                } else {
                    bytes_cap(&cf!(tr("已切换到 %s（%ld 秒）"), &self.target_name, secs), 160)
                };
            } else if status == "error" && (reason == "card_busy" || msg.contains("card is busy")) {
                // catBusy: waiting does not help, only power to the card does (the row says so)
                self.msg = trunc(tr("卡忙没切成"), 160);
                self.busy_iccid = self.target_iccid.clone();
            } else if status == "error" {
                self.msg = bytes_cap(&cf!(tr("切换失败：%s"), &msg), 160);
            } else {
                return true; // still running
            }
            eprintln!("esim: job {} {status} after {secs} s: {msg}", self.my_job);
            self.msg_ms = mono_ms();
            self.my_job = 0;
            *reload = true;
        }
        true
    }

    fn display_sig(&self, t: i64) -> String {
        let mut s = format!(
            "{}/{}/{}/{}/{}/{}",
            self.prof.len(),
            self.busy as i32,
            self.offline as i32,
            self.my_job,
            if self.my_job != 0 { (t - self.t0) / 1000 } else { 0 },
            (!self.arm_iccid.is_empty() && t - self.arm_ms <= ES_ARM_MS) as i32
        );
        for x in [&self.err, &self.msg, &self.busy_kind, &self.arm_iccid] {
            s.push('\u{1}');
            s.push_str(x);
        }
        for p in &self.prof {
            s.push_str(&format!("\u{2}{}\u{1}{}\u{1}{}\u{1}{}", p.iccid, p.name, p.sub, p.enabled as i32));
        }
        s
    }

    /// active = the eSIM page is up on a lit screen. Nothing when closed and no switch from here
    /// runs; the list at once on opening. true = what shows changed.
    pub fn poll(&mut self, agent: &mut CoreApi, active: bool) -> bool {
        if !active {
            self.was_active = false;
        }
        if !active && self.my_job == 0 {
            return false;
        }
        let t = mono_ms();
        let entering = active && !self.was_active;
        if active {
            self.was_active = true;
        }
        if entering {
            self.arm_iccid.clear();
            if self.my_job == 0 && t - self.msg_ms > 60000 {
                // a result from long ago is not hung up again
                self.msg.clear();
                self.busy_iccid.clear();
            }
        }
        if entering || t - self.poll_ms >= if self.my_job != 0 { ES_JOB_MS } else { self.poll_gap } {
            let mut reload = false;
            self.poll_ms = t;
            let mut ok = self.poll_job(agent, &mut reload);
            if self.my_job != 0 && t - self.t0 > ES_GIVEUP_MS {
                self.my_job = 0;
                self.msg = trunc(tr("切换超时，请看当前配置"), 160);
                self.msg_ms = t;
                reload = true;
            }
            // the list runs lpac: only on entering, after a job, or when the agent is back; a
            // card that could not be read (a plain SIM) is not knocked on every 2 s
            if ok && active && (entering || reload || self.offline) {
                ok = self.load_list(agent);
            }
            if !ok {
                self.err = trunc(tr("连不上管理后台"), 96);
                self.busy = false;
            }
            self.offline = !ok;
            self.poll_gap = if ok { ES_IDLE_MS } else { ES_BACKOFF_MS };
        }
        let sig = self.display_sig(t);
        if self.sig.as_ref() == Some(&sig) {
            return false;
        }
        self.sig = Some(sig);
        true
    }

    pub fn loaded(&self) -> bool {
        self.loaded
    }

    pub fn enabled_iccid(&self) -> &str {
        self.prof.iter().find(|p| p.enabled).map_or("", |p| &p.iccid)
    }

    /// The list once for this card (key = the modem's ICCID, else its IMSI) with the page closed, so 蜂窝 can
    /// tell a plain SIM from an eSIM. Once per card, success or not (lpac stalls the screen and
    /// knocks on the card).
    pub fn prefetch(&mut self, agent: &mut CoreApi, key: &str) -> bool {
        if key.is_empty() || self.prefetched == trunc(key, 32) || self.my_job != 0 {
            return false;
        }
        self.prefetched = trunc(key, 32);
        self.load_list(agent)
    }

    pub fn current(&self) -> &str {
        self.prof.iter().find(|p| p.enabled).map_or("-", |p| &p.name)
    }

    /// The state line, in the screen's language (only shown, never compared).
    pub fn state(&self) -> String {
        if self.my_job != 0 {
            return bytes_cap(&cf!(tr("切换中 · 已 %ld 秒"), (mono_ms() - self.t0) / 1000), 192);
        }
        if self.busy {
            // whole sentences, so the English can put the verb where it goes
            return match self.busy_kind.as_str() {
                "switch" => tr("网页端正在切换"),
                "download" => tr("网页端正在下载"),
                "delete" => tr("网页端正在删除"),
                "notifications" => tr("正在发送 eSIM 通知"),
                _ => tr("网页端正在操作"),
            }
            .into();
        }
        if !self.err.is_empty() {
            return self.err.clone();
        }
        if !self.msg.is_empty() {
            return self.msg.clone();
        }
        tr("就绪").into()
    }

    pub fn ready(&self) -> bool {
        self.my_job == 0 && !self.busy && self.err.is_empty() && self.msg.is_empty()
    }

    pub fn profiles(&self) -> Vec<esim_profile_t> {
        let t = mono_ms();
        let armed_live = !self.arm_iccid.is_empty() && t - self.arm_ms <= ES_ARM_MS;
        self.prof
            .iter()
            .map(|e| {
                let going = self.my_job != 0 && self.target_iccid == e.iccid;
                esim_profile_t {
                    name: e.name.clone(),
                    sub: e.sub.clone(),
                    enabled: e.enabled as i32,
                    armed: (armed_live && self.arm_iccid == e.iccid) as i32,
                    going: going as i32,
                    card_busy: (!going && !self.busy_iccid.is_empty() && self.busy_iccid == e.iccid) as i32,
                }
            })
            .collect()
    }

    pub fn switching(&self) -> bool {
        self.my_job != 0
    }

    /// A tap on profile i: two taps within 4 s switch (a switch cuts the data, at worst
    /// reboots). ESIM_SEL_*.
    pub fn select(&mut self, agent: &mut CoreApi, i: usize) -> i32 {
        let t = mono_ms();
        let Some(e) = self.prof.get(i).cloned() else { return ESIM_SEL_FAIL };
        if self.busy || self.my_job != 0 {
            return ESIM_SEL_BUSY;
        }
        if e.enabled {
            self.arm_iccid.clear();
            return ESIM_SEL_CURRENT;
        }
        // which card is armed goes by ICCID: a list re-read in between does not move it
        if self.arm_iccid != e.iccid || t - self.arm_ms > ES_ARM_MS {
            self.arm_iccid = e.iccid.clone();
            self.arm_ms = t;
            return ESIM_SEL_ARMED;
        }
        self.arm_iccid.clear();
        let js = trunc(&format!("{{\"iccid\":\"{}\"}}", e.iccid), 64);
        let (code, b) = agent.api("POST", "/api/esim/switch", Some(&js), ES_RESP_MAX);
        if code == 409 {
            self.busy = true;
            return ESIM_SEL_BUSY;
        }
        if code == 429 {
            // the agent's cooldown, only after the card really said catBusy: "...wait 480s and
            // retry" gives the seconds (this follows the agent's wording)
            let err = b.as_deref().map(|b| jstr(b, "error", 128)).unwrap_or_default();
            let wait = find(err.as_bytes(), b"wait ", 0).map_or(0, |w| json::atol(&err.as_bytes()[w + 5..]));
            self.msg = if wait > 0 {
                bytes_cap(&cf!(tr("卡刚报过忙，还要等 %d 秒再试"), wait), 160)
            } else {
                trunc(tr("卡刚报过忙，稍后再试"), 160)
            };
            self.msg_ms = mono_ms();
            return ESIM_SEL_COOLDOWN;
        }
        let job = b.filter(|_| code == 200).and_then(|b| json::get(&b, "data", 256)).map_or(0, |d| json::get_int(&d, "job_id", 0));
        if job <= 0 {
            self.my_job = 0;
            eprintln!("esim: switch request failed (HTTP {code})");
            return ESIM_SEL_FAIL;
        }
        self.my_job = job;
        self.target_iccid = e.iccid.clone();
        self.target_name = e.name.clone();
        self.t0 = t;
        self.poll_ms = t;
        self.busy = true;
        self.msg.clear();
        self.busy_iccid.clear();
        eprintln!("esim: switch to {} started (job {job})", e.iccid);
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
}
