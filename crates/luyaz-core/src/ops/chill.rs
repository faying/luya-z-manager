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
    if slot
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        return Err("another CHILL job is running".into());
    }
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{} profile {want}", ch::CHILL_SH))
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    slot.store(false, Ordering::SeqCst);
    match out {
        Ok(o) if o.status.success() => Ok(json!({"profile": want})),
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            Err(format!(
                "chill.sh profile {want} failed: {}",
                ch::tail(&s, 4)
            ))
        }
        Err(e) => Err(format!("exec failed: {e}")),
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
    #[test]
    fn only_job_subcommands_count() {
        use super::is_job;
        assert!(!is_job(b"/bin/sh\0/data/chill/chill.sh\0run\0"));
        assert!(is_job(b"/bin/sh\0/data/chill/chill.sh\0profile\0eco\0"));
        assert!(is_job(b"sh\0-c\0/data/chill/chill.sh profile perf\0"));
        assert!(is_job(b"/bin/sh\0/data/chill/chill.sh\0reload\0"));
        assert!(!is_job(b"/usr/bin/mihomo\0-d\0/data/chill\0"));
    }
}
