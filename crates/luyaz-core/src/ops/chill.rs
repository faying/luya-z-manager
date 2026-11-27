//! CHILL 的两项事务（u60-mcp.md「第二期 Decision ledger」D2、D10，Codex O5、O8、O10）：档位
//! （`chill.profile`，值 `eco`/`standard`/`perf`）和出口（`chill.exit`，值 `proxy`/`direct_keep_ai`/`direct_all`/`global`）。
//!
//! - 读：档位读 `chill.sh` 的档位文件（没有 = standard）；出口按 mihomo 的主代理组和模式算（和网页、触屏同一个
//!   `exit_state`）。读数里 `registered` = mihomo 的控制 API 答话；不看蜂窝数据、SIM 固定为空（换卡不影响 CHILL）。
//! - 写：档位跑 `chill.sh profile X`（和兼容层的 CHILL 工作同一个槽，[`slot`]，O10）；出口先换主代理组、再改模式，
//!   第二步失败时把组换回去（O5：不留半截，免得读回成第三个值被当成别人改的）。
//! - 确认（[`probe`]）：经本机入口 127.0.0.1:7894 真去取 generate_204——规则、模式、节点一起验（O8：不是 mihomo
//!   的延迟测试）。分三种：通、确定不通、不知道（控制 API 或入口连不上）。Agent 的写切之前先测：不知道就不做，
//!   确定不通照做但不带退回（D10）。
//!
//! 只有 Agent（经 `/control`）走这里；触屏、网页还是兼容层原来的入口。人在试用期间改了，读回变成第三个值，
//! 引擎记 manual_change、不退回（人优先）。
//! 出口写成功后和兼容层一样登记情景的「回国改回代理」（`api::ops::note_exit_abroad`）。

use luyaz_api::chill as ch;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const PROFILE: &str = "chill.profile";
pub const EXIT: &str = "chill.exit";
const PROFILE_FILE: &str = "/data/chill/profile";
/// mihomo 的本机混合入口（`template.yaml` 的 `127.0.0.1:7894`）。
pub const MIXED: &str = "http://127.0.0.1:7894";

/// 一次只跑一个 CHILL 工作（`chill.sh start|stop|profile|reload`）：兼容层的 `ChillJobs` 和这里共用。
pub fn slot() -> Arc<AtomicBool> {
    static S: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    S.get_or_init(Arc::default).clone()
}

/// `chill.sh start|stop|profile` 的截止时间（韧性审计 R7）。`start` 走 init 脚本起 mihomo、
/// 等它就绪（chill.sh 里最长的等待是就绪探测，几十秒内）；`profile` 改档后重启 mihomo。5 分钟
/// 远超正常用时，只用来截住卡死的那次：到点杀掉整个进程组、放开工作槽、报超时。
pub const SH_TIMEOUT: Duration = Duration::from_secs(300);
/// `chill.sh reload`（家里出口引擎每拍可能调用）：只重渲染配置 + 让 mihomo 重读，60 秒足够。
pub const RELOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// `sh -c cmd`，限时 `limit`：`Ok((退出成功, stdout + stderr))`，超时 `Err("… timed out after N s")`。
/// 子进程在自己的进程组里，不随核心一起死（核心重启时任务自己跑完，不留半启动的 mihomo）；
/// 超时才杀整个组（chill.sh 和它起的子进程）。
pub async fn sh_within(cmd: &str, limit: Duration) -> Result<(bool, String), String> {
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("exec failed: {e}"))?;
    let pid = child.id();
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(Ok(o)) => {
            let s = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            Ok((o.status.success(), s))
        }
        Ok(Err(e)) => Err(format!("exec failed: {e}")),
        Err(_) => {
            // 还没被回收（wait 没返回），进程号仍是它的组；绝不 kill(-0)。
            if let Some(pid) = pid.filter(|p| *p > 0) {
                unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
            }
            eprintln!(
                "ops: chill: `{cmd}` timed out after {} s, killed",
                limit.as_secs()
            );
            Err(format!("timed out after {} s", limit.as_secs()))
        }
    }
}

/// 有 `chill.sh` 在跑（核心重启之前起的子进程也算，O10：内存里的槽重启就没了）。
pub fn sh_running() -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    dir.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            && std::fs::read(e.path().join("cmdline")).is_ok_and(|c| is_job(&c))
    })
}

/// 一个进程的 cmdline（NUL 分隔，或 `sh -c` 的一整串）是不是 CHILL 工作（`start|stop|profile|reload`）。
/// 常驻的监督进程 `chill.sh run` 不算。
pub fn is_job(cmdline: &[u8]) -> bool {
    let s = String::from_utf8_lossy(cmdline).replace('\0', " ");
    let mut w = s.split_whitespace();
    while let Some(x) = w.next() {
        if x.ends_with("chill.sh") {
            return matches!(w.next(), Some("start" | "stop" | "profile" | "reload"));
        }
    }
    false
}

/// 真路径探测的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Up,
    Down,
    /// 控制 API 或入口连不上：说不清是 CHILL 的问题还是没在跑。
    Unknown,
}

fn mihomo() -> String {
    std::env::var("LUYAZ_MIHOMO_API").unwrap_or_else(|_| "http://127.0.0.1:9999".into())
}

fn secret() -> String {
    ch::env_var(
        &std::fs::read_to_string(ch::CHILL_ENV).unwrap_or_default(),
        "CHILL_SECRET",
    )
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

fn auth(r: reqwest::RequestBuilder, secret: &str) -> reqwest::RequestBuilder {
    if secret.is_empty() {
        r
    } else {
        r.header("Authorization", format!("Bearer {secret}"))
    }
}

async fn get_json(path: &str) -> Result<Value, String> {
    let r = auth(
        client(Duration::from_secs(3))?.get(format!("{}{path}", mihomo())),
        &secret(),
    )
    .send()
    .await
    .map_err(|e| format!("mihomo: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("mihomo: HTTP {}", r.status().as_u16()));
    }
    let b = r.bytes().await.map_err(|e| format!("mihomo: {e}"))?;
    serde_json::from_slice(&b).map_err(|e| format!("mihomo: {e}"))
}

/// 经 7894 取一次 generate_204（5 秒）。控制 API 不答 = 不知道；入口本身 TCP 连不上 = 不知道；
/// 入口连得上之后的任何失败（隧道建不起来、超时、不是 204）= 确定不通。
pub async fn probe() -> Probe {
    probe_at(ch::PROBE_URL, 5).await
}

/// The exit check's verdict (`engines::chill_probe`): a miss is tried twice more, 3 s apart, at
/// [`ch::PROBE_URL_ALT`] then [`ch::PROBE_URL`], 8 s each, and counts only if all three miss.
/// 11-13: twice a single 5 s miss, ten minutes apart, opened an incident while the owner's traffic
/// through CHILL went on. Only a miss costs the extra requests.
pub async fn probe_confirmed() -> Probe {
    let mut p = probe_at(ch::PROBE_URL, 8).await;
    for url in [ch::PROBE_URL_ALT, ch::PROBE_URL] {
        if p != Probe::Down {
            break;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        p = probe_at(url, 8).await;
    }
    p
}

/// What mihomo's own health check last said of the node the main group ends at, for an exit
/// incident's evidence: `(zh, en)`; None = the API did not say. One GET of `/proxies`, only when
/// the exit check turns to broken.
pub async fn exit_note() -> Option<(String, String)> {
    exit_note_from(
        &get_json("/proxies").await.ok()?,
        luyaz_netwatch::now_unix() as i64,
    )
}

/// Older than this, mihomo's last check says nothing about now (its checks are lazy: none while
/// nothing goes through the group; 11-14 the last one before a break was 4 hours old).
const NOTE_FRESH_S: i64 = 600;

/// [`exit_note`] from mihomo's `/proxies` answer at `now` (unix s, mihomo's clock): follow `now`
/// from the main group (at most 6 groups deep), then the last `history` entry of the deepest one
/// that has any (delay 0 = the check failed) and how old it is. A subscription's node is not in
/// `/proxies` (only in the 300 KB `/providers/proxies`); its URLTest group above it carries the
/// same checks.
pub fn exit_note_from(all: &Value, now: i64) -> Option<(String, String)> {
    let p = &all["proxies"];
    let mut chain = vec![ch::MAIN_GROUP];
    while let Some(n) = p[*chain.last()?]["now"].as_str().filter(|n| !n.is_empty()) {
        if chain.len() > 6 || chain.contains(&n) {
            return None;
        }
        chain.push(n);
    }
    if chain.len() < 2 {
        return None;
    }
    let via = chain[1..].join(" → ");
    let last = chain.iter().rev().find_map(|c| {
        let h = p[*c]["history"].as_array()?.last()?;
        Some((h["delay"].as_u64()?, h["time"].as_str().and_then(unix_of)))
    });
    let Some((delay, at)) = last else {
        return Some((
            format!("经 {via}，mihomo 还没健康检查过"),
            format!("via {via}, no health check by mihomo yet"),
        ));
    };
    let (zh, en) = if delay == 0 {
        ("失败".to_string(), "failed".to_string())
    } else {
        (format!("{delay} ms"), format!("{delay} ms"))
    };
    Some(match at.map(|t| (now - t).max(0)) {
        Some(age) if age > NOTE_FRESH_S => {
            let (az, ae) = ago(age);
            (
                format!("mihomo 对 {via} 最近一次健康检查是 {az}（{zh}），之后没再查过"),
                format!("mihomo last checked {via} {ae} ({en}), not since"),
            )
        }
        Some(age) => {
            let (az, ae) = ago(age);
            (
                format!("mihomo 对 {via} 最近一次健康检查：{zh}（{az}）"),
                format!("mihomo's last health check of {via}: {en} ({ae})"),
            )
        }
        None => (
            format!("mihomo 对 {via} 最近一次健康检查：{zh}（时间不明）"),
            format!("mihomo's last health check of {via}: {en} (time unknown)"),
        ),
    })
}

/// "N 分钟前" / "N min ago", hours from 2 h.
fn ago(s: i64) -> (String, String) {
    if s >= 7200 {
        (
            format!("{} 小时前", s / 3600),
            format!("{} h ago", s / 3600),
        )
    } else {
        (format!("{} 分钟前", s / 60), format!("{} min ago", s / 60))
    }
}

/// Unix seconds of an RFC 3339 time as mihomo writes it (`…Z` or `±hh:mm`, fraction allowed).
fn unix_of(t: &str) -> Option<i64> {
    let n = |a: usize, z: usize| t.get(a..z)?.parse::<i64>().ok();
    let b = t.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't') {
        return None;
    }
    let (y, mo, d, h, mi, se) = (
        n(0, 4)?,
        n(5, 7)?,
        n(8, 10)?,
        n(11, 13)?,
        n(14, 16)?,
        n(17, 19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = t.get(19..)?;
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let off = match rest {
        "Z" | "z" => 0,
        r if r.len() == 6 && matches!(&r[..1], "+" | "-") && &r[3..4] == ":" => {
            let v = r[1..3].parse::<i64>().ok()? * 3600 + r[4..6].parse::<i64>().ok()? * 60;
            if r.starts_with('-') { -v } else { v }
        }
        _ => return None,
    };
    // days from civil (H. Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + mi * 60 + se - off)
}

async fn probe_at(url: &str, secs: u64) -> Probe {
    if get_json("/version").await.is_err() {
        return Probe::Unknown;
    }
    let entry = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect("127.0.0.1:7894"),
    )
    .await;
    if !matches!(entry, Ok(Ok(_))) {
        return Probe::Unknown;
    }
    let Ok(proxy) = reqwest::Proxy::all(MIXED) else {
        return Probe::Unknown;
    };
    let Ok(cl) = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(secs))
        .build()
    else {
        return Probe::Unknown;
    };
    match cl.get(url).send().await {
        Ok(r) if r.status().as_u16() == 204 => Probe::Up,
        _ => Probe::Down,
    }
}

/// 现在的档位。
pub fn read_profile() -> Option<Value> {
    let p = std::fs::read_to_string(PROFILE_FILE).unwrap_or_default();
    let p = p.trim();
    Some(json!(if p.is_empty() { "standard" } else { p }))
}

/// 主代理组现在的成员、可选的成员；模式。
async fn exit_now() -> Result<(String, Vec<String>, String), String> {
    let group = get_json(&format!("/proxies/{}", ch::enc(ch::MAIN_GROUP))).await?;
    let cfg = get_json("/configs").await?;
    let now = group["now"].as_str().unwrap_or("").to_owned();
    let options = group["all"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok((
        now,
        options,
        cfg["mode"].as_str().unwrap_or("rule").to_owned(),
    ))
}

/// 现在的出口。
pub async fn read_exit() -> Result<Value, String> {
    let (now, _, mode) = exit_now().await?;
    Ok(json!(ch::exit_state(&mode, &now)))
}

/// mihomo 在跑（读数的 `registered`）。
pub async fn up() -> bool {
    get_json("/version").await.is_ok()
}

async fn put_member(cl: &reqwest::Client, secret: &str, member: &str) -> Result<(), String> {
    let url = format!("{}/proxies/{}", mihomo(), ch::enc(ch::MAIN_GROUP));
    match auth(cl.put(url), secret)
        .header("Content-Type", "application/json")
        .body(json!({"name": member}).to_string())
        .send()
        .await
    {
        Ok(r) if r.status().as_u16() < 400 => Ok(()),
        Ok(r) => Err(format!(
            "mihomo rejected switch (HTTP {})",
            r.status().as_u16()
        )),
        Err(e) => Err(format!("switch request failed: {e}")),
    }
}

/// 写出口：组、模式两步（和兼容层的 `chill_exit` 同一个计划）；模式那步失败把组换回原来的成员（O5）。
pub async fn write_exit(want: &str) -> Result<Value, String> {
    let (now, options, _) = exit_now().await?;
    let opts: Vec<&str> = options.iter().map(String::as_str).collect();
    let remembered = std::fs::read_to_string(ch::LAST_MAIN).ok();
    let backup = std::fs::read_to_string(ch::MANUAL_FIRST_STATE)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["backup"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let plan = ch::exit_plan(
        want,
        &now,
        &opts,
        remembered.as_deref().map(str::trim),
        &backup,
    )
    .map_err(|(_, v)| {
        v["error"]
            .as_str()
            .unwrap_or("cannot plan the exit")
            .to_owned()
    })?;
    let cl = client(Duration::from_secs(3))?;
    let secret = secret();
    if let Some(r) = &plan.remember {
        let _ = std::fs::write(ch::LAST_MAIN, r);
    }
    if let Some(m) = &plan.member {
        put_member(&cl, &secret, m).await?;
    }
    let mode = auth(cl.patch(format!("{}/configs", mihomo())), &secret)
        .header("Content-Type", "application/json")
        .body(json!({"mode": plan.mode}).to_string())
        .send()
        .await;
    let err = match mode {
        Ok(r) if r.status().as_u16() < 400 => {
            let _ = std::fs::write(ch::CHILL_MODE_FILE, format!("{}\n", plan.mode));
            // 和兼容层一样：在国外改了出口，回国由情景改回代理
            crate::api::ops::note_exit_abroad(
                "",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64),
            );
            return Ok(json!({"exit": want}));
        }
        Ok(r) => format!("mihomo rejected mode (HTTP {})", r.status().as_u16()),
        Err(e) => format!("mode request failed: {e}"),
    };
    if plan.member.is_some() && !now.is_empty() {
        // O5：组已经换了、模式没换上：换回去，不留半截
        if let Err(e) = put_member(&cl, &secret, &now).await {
            eprintln!("ops: chill exit: could not put the group back to {now}: {e}");
        }
    }
    Err(err)
}

/// 写档位：`chill.sh profile X`，占 CHILL 的工作槽（被占着 = 别的 CHILL 工作在跑，不写）。
pub async fn write_profile(want: &str) -> Result<Value, String> {
    let slot = slot();
    if slot.swap(true, Ordering::SeqCst) {
        return Err("another CHILL job is running".into());
    }
    let out = sh_within(&format!("{} profile {want}", ch::CHILL_SH), SH_TIMEOUT).await;
    slot.store(false, Ordering::SeqCst);
    match out {
        Ok((true, _)) => Ok(json!({"profile": want})),
        Ok((false, s)) => Err(format!(
            "chill.sh profile {want} failed: {}",
            ch::tail(&s, 4)
        )),
        Err(e) => Err(format!("chill.sh profile {want}: {e}")),
    }
}

/// 核心的「没人用就暂停」租约还没到期（`engines::chill_idle`）：mihomo 这时停着，改了也测不出通不通。
pub fn idle_paused() -> bool {
    std::fs::read_to_string(crate::engines::chill_idle::LEASE)
        .ok()
        .is_some_and(|s| {
            luyaz_api::alerts::deadline::active(
                &s,
                luyaz_api::alerts::clock::BootTime::read().as_ref(),
            )
        })
}

/// 现在能不能动 CHILL（Agent 的关用）：有 CHILL 工作在跑（这次开机的槽，或重启前起的 `chill.sh`）就不行。
pub fn busy() -> bool {
    slot().load(Ordering::SeqCst) || sh_running()
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_hung_chill_sh_is_killed_at_its_deadline() {
        use std::time::Duration;
        // R7: `sleep 3600` stands in for a wedged chill.sh; its background child dies with it.
        let mark = format!("/tmp/luyaz-chill-r7-{}", std::process::id());
        let t0 = std::time::Instant::now();
        let r = super::sh_within(
            &format!("(sleep 2; touch {mark}) & sleep 3600"),
            Duration::from_millis(300),
        )
        .await;
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        assert!(r.unwrap_err().contains("timed out"));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            !std::path::Path::new(&mark).exists(),
            "the group was not killed"
        );
        assert_eq!(
            super::sh_within("echo out; echo err >&2; exit 3", Duration::from_secs(5)).await,
            Ok((false, "out\nerr\n".into()))
        );
    }

    #[test]
    fn only_job_subcommands_count() {
        use super::is_job;
        assert!(!is_job(b"/bin/sh\0/data/chill/chill.sh\0run\0"));
        assert!(is_job(b"/bin/sh\0/data/chill/chill.sh\0profile\0eco\0"));
        assert!(is_job(b"sh\0-c\0/data/chill/chill.sh profile perf\0"));
        assert!(is_job(b"/bin/sh\0/data/chill/chill.sh\0reload\0"));
        assert!(!is_job(b"/usr/bin/mihomo\0-d\0/data/chill\0"));
    }

    #[test]
    fn exit_note_follows_the_main_group_to_its_node() {
        use super::{exit_note_from, unix_of};
        use serde_json::json;
        let now = unix_of("2026-11-14T18:20:00Z").unwrap();
        assert_eq!(unix_of("2026-11-14T18:13:40.847732098Z"), Some(now - 380));
        assert_eq!(unix_of("2026-11-14T18:20:00+08:00"), Some(now - 8 * 3600));
        assert_eq!(unix_of("nope"), None);
        let all = json!({"proxies": {
            "🚀 节点选择": {"now": "TW"},
            "TW": {"now": "tw-1", "history": [{"delay": 90, "time": "2026-11-14T18:10:00Z"}]},
            "tw-1": {"history": [{"delay": 120}, {"delay": 0, "time": "2026-11-14T18:17:00Z"}]}}});
        assert_eq!(
            exit_note_from(&all, now).unwrap().1,
            "mihomo's last health check of TW → tw-1: failed (3 min ago)"
        );
        // 11-14: the node is a provider's, its group's last check four hours before the break
        let stale = json!({"proxies": {"🚀 节点选择": {"now": "TW"},
            "TW": {"now": "tw-9", "history": [{"delay": 51, "time": "2026-11-14T14:16:31Z"}]}}});
        assert_eq!(
            exit_note_from(&stale, now).unwrap(),
            (
                "mihomo 对 TW → tw-9 最近一次健康检查是 4 小时前（51 ms），之后没再查过"
                    .to_string(),
                "mihomo last checked TW → tw-9 4 h ago (51 ms), not since".to_string()
            )
        );
        let fresh = json!({"proxies": {"🚀 节点选择": {"now": "jp-1"},
            "jp-1": {"history": [{"delay": 80, "time": "2026-11-14T18:19:00Z"}]}}});
        assert_eq!(
            exit_note_from(&fresh, now).unwrap().0,
            "mihomo 对 jp-1 最近一次健康检查：80 ms（1 分钟前）"
        );
        let untimed = json!({"proxies": {"🚀 节点选择": {"now": "jp-1"},
            "jp-1": {"history": [{"delay": 80}]}}});
        assert!(
            exit_note_from(&untimed, now)
                .unwrap()
                .1
                .ends_with("(time unknown)")
        );
        let none = json!({"proxies": {"🚀 节点选择": {"now": "jp-1"}, "jp-1": {}}});
        assert_eq!(
            exit_note_from(&none, now).unwrap().1,
            "via jp-1, no health check by mihomo yet"
        );
        // nothing chosen, a loop, no answer: nothing to say
        assert!(exit_note_from(&json!({"proxies": {"🚀 节点选择": {"now": ""}}}), now).is_none());
        let looped = json!({"proxies": {"🚀 节点选择": {"now": "a"}, "a": {"now": "🚀 节点选择"}}});
        assert!(exit_note_from(&looped, now).is_none());
        assert!(exit_note_from(&json!({}), now).is_none());
    }
}
