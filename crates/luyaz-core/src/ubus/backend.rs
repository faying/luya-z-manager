//! ubus 读取后端（docs/STATE_V2.md V2-18）：`LUYAZ_UBUS=cli|socket|auto` 选择，默认 `cli`。
//!
//! - `Cli`：默认，每次起一个 `ubus call`（和原来的 `state::ubus` 一样：同样的名字校验、8 秒超时、
//!   同样的错误文字，`LUYAZ_UBUS_BIN` 指定程序）。
//! - `Socket`：直连 ubusd（`client::UbusClient`），`LUYAZ_UBUS_SOCK` 指定 socket，
//!   `LUYAZ_UBUS_TIMEOUT_MS` 指定采集轮里的单请求超时（默认 2000）；采集轮之外（控制任务、
//!   内部任务）的请求用 `CONTROL_TIMEOUT`（8 秒，写操作可能要等较久；环境变量更大时取更大的）。
//! - `Auto`：走 socket；请求**肯定没送到**时（连不上 ubusd、没收到 HELLO、LOOKUP 失败、INVOKE 没写出去，
//!   见 `UbusClient::last_call_not_sent`）这一次改用 `ubus call`，之后 `FALLBACK_RETRY`（30 秒）内都走 CLI，
//!   到时再试 socket。INVOKE 写出去之后的超时、断开不退回、不重发。`socket` 则从不退回。
//!   退回的那一次 CLI 只给 CLI 超时（8 秒）减去 socket 已用掉的时间（至少 `FALLBACK_MIN`），
//!   所以一次调用总共仍不超过 8 秒左右，执行者的 `CALL_LIMIT`（10 秒）和看门狗的前提不变。
//!
//! 执行者（`executor.rs`）持有一个 `Backend`；`state::ubus` 经执行者调到这里（T4）。

use super::client::{DEFAULT_SOCKET, DEFAULT_TIMEOUT, UbusClient, UbusError};
use crate::command;
use serde_json::Value;
use std::{future::Future, time::Duration};
use tokio::time::Instant;

pub const ENV_BACKEND: &str = "LUYAZ_UBUS";
pub const ENV_SOCKET: &str = "LUYAZ_UBUS_SOCK";
pub const ENV_TIMEOUT_MS: &str = "LUYAZ_UBUS_TIMEOUT_MS";
pub const ENV_CLI_BIN: &str = "LUYAZ_UBUS_BIN";
/// cli 后端（和短信事件监听，`listen.rs`）用的 ubus 程序。
pub fn cli_bin() -> String {
    std::env::var(ENV_CLI_BIN).unwrap_or_else(|_| "/bin/ubus".into())
}

/// `state::ubus` 给 `ubus call` 的超时。
pub const CLI_TIMEOUT: Duration = Duration::from_secs(8);
/// socket 后端在采集轮之外（控制任务、内部任务）的单请求超时（V2-19）。
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(8);
/// `auto`：socket 不可用后多久内都走 CLI，再试 socket（同上游 v0.10.56）。
pub const FALLBACK_RETRY: Duration = Duration::from_secs(30);
/// 退回的那一次 CLI 至少给这么久。
pub const FALLBACK_MIN: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Cli,
    Socket,
    /// socket，肯定没送到时退回 CLI（`AutoBackend`）。
    Auto,
}

impl BackendKind {
    /// 解析 `LUYAZ_UBUS` 的值。未设置或空 → cli；非法值 → cli，并返回要写进日志的警告。
    pub fn parse(value: Option<&str>) -> (Self, Option<String>) {
        match value.map(str::trim) {
            None | Some("") | Some("cli") => (Self::Cli, None),
            Some("socket") => (Self::Socket, None),
            Some("auto") => (Self::Auto, None),
            Some(other) => (
                Self::Cli,
                Some(format!(
                    "{ENV_BACKEND}={other:?} 无效（只认 cli、socket 或 auto），改用 cli"
                )),
            ),
        }
    }

    pub fn from_env() -> Self {
        let value = std::env::var(ENV_BACKEND).ok();
        let (kind, warning) = Self::parse(value.as_deref());
        if let Some(w) = warning {
            eprintln!("luyaz-core: {w}");
        }
        kind
    }
}

/// 一次 ubus 读取。返回 `Send` 的 future，T4 的执行者可以放进 `tokio::spawn` 的任务里。
pub trait UbusBackend: Send {
    #[cfg(test)]
    fn kind(&self) -> BackendKind;
    fn call(
        &mut self,
        object: &str,
        method: &str,
        args: &Value,
    ) -> impl Future<Output = Result<Value, UbusError>> + Send;
    /// 接下来的调用是否在采集轮里（执行者每次调用前设置）。socket 后端据此选超时，其他后端不管。
    fn set_round(&mut self, _round: bool) {}

    /// Final USB write preflight only. No GET or periodic discovery uses this.
    fn usb_methods(&mut self, object: &str) -> impl Future<Output = Result<Vec<u8>, UbusError>> + Send {
        async move {
            if !matches!(object, "zwrt_bsp.usb" | "zwrt_bsp.typec" | "zwrt_bsp.powerbank") {
                return Err(UbusError::InvalidArgument("not a reviewed USB object".into()));
            }
            command::run(&cli_bin(), ["-v", "list", object], Duration::from_secs(5))
                .await.map_err(|e| UbusError::Io(e.to_string()))
        }
    }
}

/// 现状：每次调用起一个 `ubus call`。
#[derive(Debug, Clone)]
pub struct CliBackend {
    bin: String,
    timeout: Duration,
}

impl CliBackend {
    pub fn new(bin: impl Into<String>, timeout: Duration) -> Self {
        Self {
            bin: bin.into(),
            timeout,
        }
    }

    pub fn from_env() -> Self {
        Self::new(cli_bin(), CLI_TIMEOUT)
    }
}

impl UbusBackend for CliBackend {
    #[cfg(test)]
    fn kind(&self) -> BackendKind {
        BackendKind::Cli
    }

    // 原来 state::ubus 的实现（T4 起那边只转给执行者）。
    async fn call(&mut self, object: &str, method: &str, args: &Value) -> Result<Value, UbusError> {
        super::validate_name(object).map_err(UbusError::InvalidArgument)?;
        super::validate_name(method).map_err(UbusError::InvalidArgument)?;
        if !args.is_object() {
            return Err(UbusError::InvalidArgument("args must be an object".into()));
        }
        let body =
            serde_json::to_string(args).map_err(|e| UbusError::InvalidArgument(e.to_string()))?;
        let raw = command::run(&self.bin, ["call", object, method, &body], self.timeout)
            .await
            .map_err(|e| {
                if e.chain().any(|c| c.is::<tokio::time::error::Elapsed>()) {
                    UbusError::Timeout {
                        object: object.into(),
                        detail: e.to_string(),
                    }
                } else {
                    UbusError::Io(e.to_string())
                }
            })?;
        serde_json::from_slice(&raw).map_err(|e| {
            let detail = format!("invalid ubus JSON: {e}");
            if raw.iter().all(u8::is_ascii_whitespace) {
                UbusError::NoData {
                    object: object.into(),
                    detail,
                }
            } else {
                UbusError::Io(detail)
            }
        })
    }
}

/// 直连 ubusd。
pub struct SocketBackend {
    client: UbusClient,
    /// 采集轮里的超时（构造时 client 的超时）。
    round_timeout: Duration,
    /// 采集轮之外的超时。
    control_timeout: Duration,
}

impl SocketBackend {
    pub fn new(client: UbusClient) -> Self {
        let round_timeout = client.timeout();
        Self {
            client,
            round_timeout,
            control_timeout: CONTROL_TIMEOUT.max(round_timeout),
        }
    }

    pub fn from_env() -> Self {
        let path = std::env::var(ENV_SOCKET).unwrap_or_else(|_| DEFAULT_SOCKET.into());
        let timeout = std::env::var(ENV_TIMEOUT_MS)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        Self::new(UbusClient::with_timeout(path, timeout))
    }

    pub fn client(&mut self) -> &mut UbusClient {
        &mut self.client
    }
}

/// `ubus call` 什么都不打印时 `state::ubus` 给出的错误文字。
fn empty_output_error() -> String {
    match serde_json::from_slice::<Value>(b"") {
        Err(e) => format!("invalid ubus JSON: {e}"),
        Ok(_) => "invalid ubus JSON".into(),
    }
}

impl UbusBackend for SocketBackend {
    #[cfg(test)]
    fn kind(&self) -> BackendKind {
        BackendKind::Socket
    }

    fn set_round(&mut self, round: bool) {
        self.client.set_timeout(if round {
            self.round_timeout
        } else {
            self.control_timeout
        });
    }

    async fn call(&mut self, object: &str, method: &str, args: &Value) -> Result<Value, UbusError> {
        // 状态 OK 但没数据：和 CLI 后端一样算读失败（待确认：T4 的写操作可能更想要 Ok({})）。
        self.client
            .call(object, method, args)
            .await?
            .ok_or_else(|| UbusError::NoData {
                object: object.into(),
                detail: empty_output_error(),
            })
    }
}

/// socket 优先，请求肯定没送到时退回 CLI。
pub struct AutoBackend {
    socket: SocketBackend,
    cli: CliBackend,
    retry: Duration,
    /// 在这之前都走 CLI（socket 不可用）。
    cli_until: Option<Instant>,
    /// 退回过几次（测试和日志用）。
    pub fallbacks: u64,
}

impl AutoBackend {
    pub fn new(socket: SocketBackend, cli: CliBackend, retry: Duration) -> Self {
        Self {
            socket,
            cli,
            retry,
            cli_until: None,
            fallbacks: 0,
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            SocketBackend::from_env(),
            CliBackend::from_env(),
            FALLBACK_RETRY,
        )
    }

    /// 现在是否在走 CLI。
    pub fn on_cli(&self, now: Instant) -> bool {
        self.cli_until.is_some_and(|t| now < t)
    }
}

impl UbusBackend for AutoBackend {
    #[cfg(test)]
    fn kind(&self) -> BackendKind {
        BackendKind::Auto
    }

    fn set_round(&mut self, round: bool) {
        self.socket.set_round(round);
    }

    async fn call(&mut self, object: &str, method: &str, args: &Value) -> Result<Value, UbusError> {
        if self.on_cli(Instant::now()) {
            return self.cli.call(object, method, args).await;
        }
        if self.cli_until.take().is_some() {
            eprintln!("luyaz-core: trying the ubus socket again");
        }
        let start = Instant::now();
        match self.socket.call(object, method, args).await {
            Err(e)
                if matches!(
                    e,
                    UbusError::Io(_) | UbusError::Timeout { .. } | UbusError::Protocol(_)
                ) && self.socket.client().last_call_not_sent() =>
            {
                self.fallbacks += 1;
                self.cli_until = Some(Instant::now() + self.retry);
                eprintln!(
                    "luyaz-core: ubus socket unusable ({e}); using `ubus call` for {} s",
                    self.retry.as_secs()
                );
                // 这一次的总时间不超过 CLI 自己的超时（见模块说明）。
                let full = self.cli.timeout;
                self.cli.timeout = full.saturating_sub(start.elapsed()).max(FALLBACK_MIN);
                let r = self.cli.call(object, method, args).await;
                self.cli.timeout = full;
                r
            }
            r => r,
        }
    }
}

/// 按环境变量选出的后端。
pub enum Backend {
    Cli(CliBackend),
    Socket(SocketBackend),
    Auto(AutoBackend),
}

impl Backend {
    pub fn from_env() -> Self {
        match BackendKind::from_env() {
            BackendKind::Cli => Self::Cli(CliBackend::from_env()),
            BackendKind::Socket => Self::Socket(SocketBackend::from_env()),
            BackendKind::Auto => Self::Auto(AutoBackend::from_env()),
        }
    }
}

impl UbusBackend for Backend {
    #[cfg(test)]
    fn kind(&self) -> BackendKind {
        match self {
            Self::Cli(b) => b.kind(),
            Self::Socket(b) => b.kind(),
            Self::Auto(b) => b.kind(),
        }
    }

    async fn call(&mut self, object: &str, method: &str, args: &Value) -> Result<Value, UbusError> {
        match self {
            Self::Cli(b) => b.call(object, method, args).await,
            Self::Socket(b) => b.call(object, method, args).await,
            Self::Auto(b) => b.call(object, method, args).await,
        }
    }

    fn set_round(&mut self, round: bool) {
        match self {
            Self::Cli(b) => b.set_round(round),
            Self::Socket(b) => b.set_round(round),
            Self::Auto(b) => b.set_round(round),
        }
    }
}
