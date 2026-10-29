//! Serve mode: the core's server (u60 platform Phase 3). This started as the old data service's
//! `main.rs` (data-service `68416eb`) with the same `Args` and ports.
//!
//!   luyaz-core serve <arguments>
//!   luyaz-core* <arguments>     (argv[0]'s file name starts with `luyaz-core`)
//!
//! Differences against data-service's `main.rs`, all deliberate (PORTING.md):
//! - a `current_thread` runtime (built in `main.rs`), not `#[tokio::main]` multi-thread;
//! - the SMS session key is registered (`state::SMS_SESSION`), like data-service;
//! - `--version` prints u60d's version.

use crate::{model, server, server::App, ubus};
use anyhow::Result;
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf, time::Duration};

fn validate_listener_security(addr: SocketAddr, require_auth: bool) -> Result<()> {
    if !addr.ip().is_loopback() && !require_auth {
        anyhow::bail!(
            "refusing unauthenticated non-loopback listener {addr}; use --auth-token-file or --lan-bind"
        );
    }
    Ok(())
}

#[derive(Parser, Debug)]
#[command(name = "luyaz-core", version = env!("CARGO_PKG_VERSION"))]
struct Args {
    #[arg(long)]
    once: bool,
    /// 已删除的 WebShell（和 2026-10 删掉的邻区采集）的旧开关，只为不让还带着它的旧启动脚本起不来，不起作用。
    #[arg(long, hide = true)]
    webshell: bool,
    #[arg(long, hide = true)]
    neighbor: bool,
    #[arg(long)]
    auth_token_file: Option<PathBuf>,
    #[arg(long)]
    lan_bind: Option<String>,
    #[arg(long, default_value_t = 9461)]
    lan_port: u16,
    #[arg(short = 'i', default_value_t = 1000)]
    interval: u64,
    #[arg(short = 'b', long = "bind", default_value = "127.0.0.1")]
    bind: String,
    #[arg(short = 'p', long = "port", default_value_t = 9460)]
    port: u16,
    #[arg(long, env = "LUYAZ_DATA_DIR", default_value = "/data/luyaz/core")]
    data_dir: PathBuf,
}

/// 构建标记：给打包脚本认「这是 Luya-Z 核心、不带外部更新源」用
/// （onboard/build-kit.sh、tools/luyaz、luyaz-ship.sh 用 `grep -a` 查；
/// tools/luyaz 按提交编老版本时也认改名前的标记，装机包和设备上的 luyaz-ship.sh 只认这个）。
/// 不参与任何 HTTP 输出；`#[used]` + main 里的 black_box 保证 LTO/strip 后仍在二进制里。
#[used]
static BUILD_MARKER: [u8; 31] = *b"LUYAZ_CORE_BUILD_SELF_CONTAINED";

/// Runs data-service's command line as data-service did; `raw[0]` is the program name.
/// Returns only for `--once`, `--compare-state-shape` and argument errors; serving ends the process.
pub async fn main(raw: Vec<String>) -> Result<()> {
    std::hint::black_box(&BUILD_MARKER);
    // X9: the core's log before this start (the last run's crash evidence), before anything is written
    crate::svcwatch::note_start();
    crate::state::SMS_SESSION.store(true, std::sync::atomic::Ordering::Relaxed);
    // reqwest（短信 HTTP 发送）用 rustls，进程里装一次 ring 作为默认加密实现。
    let _ = rustls::crypto::ring::default_provider().install_default();
    // 只读核对 socket 后端和 `ubus call` 的结果（ubus/compare.rs）；不起服务、不写文件。
    if raw.get(1).map(String::as_str) == Some("--ubus-compare") {
        std::process::exit(ubus::compare::run(&raw[2..]).await);
    }
    if raw.get(1).map(String::as_str) == Some("--compare-state-shape") {
        match model::compare_state_shape(&raw[2..]) {
            Ok(value) => {
                println!("{}", serde_json::to_string(&value)?);
                return Ok(());
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(64);
            }
        }
    }
    let argv0 = raw.first().cloned().unwrap_or_default();
    let args = Args::parse_from(raw);
    let interval = Duration::from_millis(args.interval.clamp(500, 5000));
    let _ = (
        &args.neighbor,
        &args.webshell,
        &args.auth_token_file,
        &args.lan_bind,
        args.lan_port,
    );
    let token = match args.auth_token_file {
        Some(path) => std::fs::read_to_string(path)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty()),
        None => None,
    };
    let local_requires_auth = args.lan_bind.is_none() && token.is_some();
    let app = App::new(args.data_dir, interval, token.clone()).await?;
    if args.once {
        app.start().await;
        println!("{}", serde_json::to_string(&app.snapshot().await)?);
        return Ok(());
    }
    let addr: SocketAddr = format!("{}:{}", args.bind, args.port).parse()?;
    validate_listener_security(addr, local_requires_auth)?;
    // 先监听、再采第一轮（P1-3）：第一轮期间 `/healthz` 就能回 503 `starting`，看门狗也已经在看。
    // 端口被占（另一个核心还在）时在这里就退出，不碰事务恢复。
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let lan = match &args.lan_bind {
        Some(lan_bind) => {
            let lan_addr: SocketAddr = format!("{}:{}", lan_bind, args.lan_port).parse()?;
            Some(tokio::net::TcpListener::bind(lan_addr).await?)
        }
        None => None,
    };
    // The screen's and luyaz-web's API (`/v2/api/...`, compat's routes) and its background duties.
    // The routes only on a loopback listener: never towards the LAN (M6).
    let mut ui = None;
    // the writes go to this process's own /control
    let control = (
        format!("http://127.0.0.1:{}/control", args.port),
        token.clone(),
    );
    let screen_app = app.clone();
    let screen: std::sync::Arc<dyn Fn() -> crate::api::ScreenReply + Send + Sync> =
        std::sync::Arc::new(move || {
            let a = screen_app.clone();
            Box::pin(async move { Some(a.screen_json().await) })
        });
    let c = crate::api::Compat::from_host(Some(app.hub()), control, screen);
    if addr.ip().is_loopback() {
        ui = Some(c.clone().v2_router());
    }
    crate::api::start(c);
    app.resume_loop();
    // X9 (supervise.sh's duties): both listeners are ours, so this is the one core
    crate::svcwatch::start(&argv0);
    // lan-client-id.md step 0: only with the opt-in flag file
    crate::lanid_capture::start(&argv0);
    // before the ledger's boot line (~60 s) takes the planned-reboot marker away
    crate::sim_alerts::boot();
    // 进程插件的 Unix socket（没有插件的专用用户就不开）
    crate::plug::start();
    server::write_pid_file();
    let starting = app.clone();
    tokio::spawn(async move { starting.start().await });
    let result = if let Some(lan) = lan {
        tokio::try_join!(
            app.clone().serve(listener, false, false, ui),
            app.serve(lan, true, true, None)
        )
        .map(|_| ())
    } else {
        app.serve(listener, local_requires_auth, false, ui).await
    };
    // 服务停了就直接退出进程（P2-4）：从 main 返回会析构运行时，而运行时要等阻塞线程池里还在跑的
    // 任务（如卡在串口上的 AT 读写），可能一直等到 procd 的 SIGKILL。
    match result {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("Error: {error:?}");
            std::process::exit(1)
        }
    }
}

/// argv[0] says luyaz-core: the installed binary or a side-by-side copy (`luyaz-core.test`, which
/// `tools/luyaz ship` and `luyaz-fallback.sh` recognise by the prefix). Only the file name counts,
/// not the directory.
pub fn invoked_as_core(argv0: &str) -> bool {
    std::path::Path::new(argv0)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(crate::paths::is_core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_unauthenticated_non_loopback_listener() {
        assert!(validate_listener_security("0.0.0.0:9460".parse().unwrap(), false).is_err());
        assert!(validate_listener_security("192.168.0.1:9460".parse().unwrap(), false).is_err());
        assert!(validate_listener_security("127.0.0.1:9460".parse().unwrap(), false).is_ok());
        assert!(validate_listener_security("[::1]:9460".parse().unwrap(), false).is_ok());
        assert!(validate_listener_security("0.0.0.0:9460".parse().unwrap(), true).is_ok());
    }

    #[test]
    fn argv0_dispatch() {
        for yes in [
            "luyaz-core",
            "/data/luyaz/luyaz-core",
            "luyaz-core.test",
        ] {
            assert!(invoked_as_core(yes), "{yes}");
        }
        for no in [
            "otherd",
            "/data/other/otherd",
            "/tmp/otherd.test",
            "/data/luyaz/core/otherd",
            "luyaz-web",
            "",
        ] {
            assert!(!invoked_as_core(no), "{no}");
        }
    }

    #[test]
    fn args_parse() {
        let a = Args::try_parse_from([
            "luyaz-core",
            "-i",
            "1000",
            "--lan-bind",
            "0.0.0.0",
            "--lan-port",
            "9461",
            "--auth-token-file",
            "/data/luyaz/core/auth.token",
            "--webshell",
            "--neighbor",
        ])
        .unwrap();
        assert_eq!(a.interval, 1000);
        assert_eq!(a.lan_bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(a.lan_port, 9461);
        assert_eq!((a.bind.as_str(), a.port), ("127.0.0.1", 9460));
        assert!(!a.once);
        let a = Args::try_parse_from(["luyaz-core", "--once"]).unwrap();
        assert!(a.once);
    }
}
