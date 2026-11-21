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
/// `chill.sh reload` 只重渲染配置和让 mihomo 重读。
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

/// Run a reload off the async runtime. Kill the whole process group if the deadline expires.
pub(crate) fn chill_reload_within(cmd: &str, limit: Duration) -> Result<(), String> {
    let mut c = std::process::Command::new("sh");
    c.arg("-c").arg(cmd);
    match luyaz_api::health::output_within_group(&mut c, limit) {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let s = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            Err(format!("reload failed: {}", ch::tail(&s, 6)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            Err(format!("reload timed out after {} s", limit.as_secs()))
        }
        Err(e) => Err(format!("reload failed: {e}")),
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
            return matches!(
                w.next(),
                Some("start" | "stop" | "profile" | "reload" | "profile-switch" | "profile-refresh" | "profile-validate")
            );
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
        .timeout(Duration::from_secs(5))
        .build()
    else {
        return Probe::Unknown;
    };
    match cl.get(ch::PROBE_URL).send().await {
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
async fn exit_now() -> Result<(Option<Value>, String), String> {
    let url = format!("{}/proxies/{}", mihomo(), ch::enc(ch::MAIN_GROUP));
    let res = auth(client(Duration::from_secs(3))?.get(&url), &secret())
        .send()
        .await
        .map_err(|_| "mihomo group request failed".to_owned())?;
    let group = match res.status().as_u16() {
        200 => Some(serde_json::from_slice(&res.bytes().await.map_err(|_| "cannot read mihomo group")?)
            .map_err(|_| "invalid mihomo group response")?),
        404 => None,
        code => return Err(format!("mihomo group request failed (HTTP {code})")),
    };
    let cfg = get_json("/configs").await?;
    let mode = cfg["mode"].as_str()
        .filter(|mode| ch::valid_mode(mode))
        .ok_or("invalid mihomo mode")?;
    Ok((group, mode.to_owned()))
}

/// 现在的出口。
pub async fn read_exit() -> Result<Value, String> {
    let (group, mode) = exit_now().await?;
    let now = group.as_ref().and_then(|g| g["now"].as_str()).unwrap_or("");
    Ok(json!(ch::exit_state(&mode, now)))
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

pub(crate) async fn write_mode(
    client: &reqwest::Client,
    api: &str,
    secret: &str,
    file: &std::path::Path,
    want: &str,
) -> Result<(), String> {
    if !ch::valid_mode(want) {
        return Err("mode must be rule, global or direct".into());
    }
    let url = format!("{api}/configs");
    let response = auth(client.patch(&url), secret)
        .header("Content-Type", "application/json")
        .body(json!({"mode": want}).to_string())
        .send()
        .await
        .map_err(|_| "mode request failed".to_owned())?;
    if !response.status().is_success() {
        return Err(format!("mihomo rejected mode (HTTP {})", response.status().as_u16()));
    }
    let response = auth(client.get(&url), secret).send().await.map_err(|_| "mode readback failed")?;
    if !response.status().is_success() {
        return Err(format!("mode readback failed (HTTP {})", response.status().as_u16()));
    }
    let bytes = response.bytes().await.map_err(|_| "cannot read mihomo mode")?;
    let readback: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid mihomo mode response")?;
    if readback.get("mode").and_then(Value::as_str) != Some(want) {
        return Err("mihomo mode readback did not match".into());
    }
    crate::api::ops::atomic_write(file, format!("{want}\n").as_bytes())
        .map_err(|_| "cannot save mihomo mode".to_owned())
}

/// 写出口：组、模式两步（和兼容层的 `chill_exit` 同一个计划）；模式那步失败把组换回原来的成员（O5）。
pub async fn write_exit(want: &str) -> Result<Value, String> {
    let (group, _) = exit_now().await?;
    let cl = client(Duration::from_secs(3))?;
    let api = mihomo();
    let secret = secret();
    let file = std::path::Path::new(ch::CHILL_MODE_FILE);
    let Some(group) = group else {
            let mode = ch::exit_mode(want).ok_or("invalid CHILL exit state")?;
            write_mode(&cl, &api, &secret, file, mode).await?;
            crate::api::ops::note_exit_abroad(
                "",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64),
            );
            return Ok(json!({"exit": ch::exit_state(mode, "")}));
    };
    let now = group["now"].as_str().unwrap_or("");
    let opts: Vec<&str> = group["all"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let remembered = std::fs::read_to_string(ch::LAST_MAIN).ok();
    let backup = std::fs::read_to_string(ch::MANUAL_FIRST_STATE)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["backup"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let plan = ch::exit_plan(
        want,
        now,
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
    if let Some(r) = &plan.remember {
        let _ = std::fs::write(ch::LAST_MAIN, r);
    }
    if let Some(m) = &plan.member {
        put_member(&cl, &secret, m).await?;
    }
    let mode = write_mode(&cl, &api, &secret, file, plan.mode).await;
    let err = match mode {
        Ok(_) => {
            // 和兼容层一样：在国外改了出口，回国由情景改回代理
            crate::api::ops::note_exit_abroad(
                "",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64),
            );
            return Ok(json!({"exit": want}));
        }
        Err(e) => e,
    };
    if plan.member.is_some() && !now.is_empty() {
        // O5：组已经换了、模式没换上：换回去，不留半截
        if let Err(e) = put_member(&cl, &secret, now).await {
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
    use std::time::{Duration, Instant};

    #[test]
    fn a_hung_reload_returns_at_its_deadline() {
        let t0 = Instant::now();
        let e = super::chill_reload_within("sleep 3600", Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(t0.elapsed() < Duration::from_secs(2));
        // a later reload still runs
        assert_eq!(
            super::chill_reload_within("true", Duration::from_secs(5)),
            Ok(())
        );
        assert!(
            super::chill_reload_within("echo no; exit 1", Duration::from_secs(5))
                .unwrap_err()
                .contains("no")
        );
    }

    #[tokio::test]
    async fn a_hung_chill_sh_is_killed_at_its_deadline() {
        // R7: `sleep 3600` stands in for a wedged chill.sh; its background child dies with it.
        let mark = format!("/tmp/luyaz-chill-r7-{}", std::process::id());
        let t0 = Instant::now();
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
        assert!(is_job(b"/bin/sh\0/data/chill/chill.sh\0profile-switch\0p-x\0"));
        assert!(is_job(b"/bin/sh\0/data/chill/chill.sh\0profile-refresh\0p-x\0"));
        assert!(!is_job(b"/usr/bin/mihomo\0-d\0/data/chill\0"));
    }

    #[tokio::test]
    async fn canonical_mode_persists_only_after_a_matching_readback() {
        use axum::{Router, Json, extract::State, routing::get};
        use serde_json::{Value, json};
        use std::sync::{Arc, Mutex};

        let state = Arc::new(Mutex::new("rule".to_owned()));
        let app = Router::new().route("/configs", get(
            |State(mode): State<Arc<Mutex<String>>>| async move { Json(json!({"mode": *mode.lock().unwrap()})) },
        ).patch(
            |State(mode): State<Arc<Mutex<String>>>, Json(want): Json<Value>| async move {
                // Simulate a successful PATCH that does not honor a mode.
                if want["mode"] != "direct" {
                    *mode.lock().unwrap() = want["mode"].as_str().unwrap().to_owned();
                }
                axum::http::StatusCode::NO_CONTENT
            },
        )).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let path = std::env::temp_dir().join(format!("luyaz-mode-test-{}", std::process::id()));
        let cl = super::client(Duration::from_secs(3)).unwrap();
        assert_eq!(super::write_mode(&cl, &url, "", &path, "global").await, Ok(()));
        assert!(super::write_mode(&cl, &url, "", &path, "direct").await.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "global\n");
        std::fs::remove_file(path).unwrap();
    }
}
