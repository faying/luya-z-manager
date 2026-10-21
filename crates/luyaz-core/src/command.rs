use anyhow::{Context, Result, bail};
use std::{
    ffi::OsStr,
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    time::timeout,
};

const MAX_OUTPUT: usize = 1024 * 1024;

/// 让子进程随核心一起死（SIGKILL 也一样）：Linux 上 fork 后、exec 前调
/// `prctl(PR_SET_PDEATHSIG, SIGKILL)`。
///
/// 细节：pdeathsig 按「fork 出它的那个**线程**」退出触发，不是按进程。核心的 spawn 发生在
/// tokio 工作线程上，工作线程和运行时同生共死，不会先于进程退出；但不要从 `spawn_blocking`
/// 里起长期子进程（阻塞池线程空闲 10 秒就退出，会误杀子进程）。
/// 另外 fork 之后、prctl 之前父进程可能已经死了（子进程已被过继给 init，prctl 不会再触发），
/// 所以设完再比一次 `getppid()` 和 fork 前记下的父进程号，不一样就立即 `_exit`。
pub fn die_with_parent(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: pre_exec 闭包只调 async-signal-safe 的 prctl / getppid / _exit，不分配内存。
        unsafe {
            cmd.pre_exec(move || {
                if libc::prctl(
                    libc::PR_SET_PDEATHSIG,
                    libc::SIGKILL as libc::c_ulong,
                    0,
                    0,
                    0,
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(0);
                }
                Ok(())
            });
        }
    }
    cmd
}

/// Run `program` directly (never through a shell) and return its stdout.
///
/// Fails when it cannot start, outlives `deadline`, prints more than
/// `MAX_OUTPUT` bytes, or exits non-zero. stdin is empty and stderr is
/// discarded (a pipe nobody drains would stall a chatty child).
pub async fn run<I, S>(program: &str, args: I, deadline: Duration) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(program);
    die_with_parent(&mut cmd);
    cmd.args(args);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    // Dropping the child (timeout or oversize below) kills it.
    cmd.kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return Err(anyhow::Error::new(e).context(format!("spawn {program}"))),
    };
    let waited = timeout(deadline, collect(&mut child)).await;
    // 子进程有自己的超时（V2-32）：在执行者里时，结束（含超时、超长）算一次前进。
    crate::executor::progress();
    // Keep the elapsed error in the chain: callers detect timeouts by downcasting.
    let collected = waited.with_context(|| format!("{program} timed out"))?;
    match collected? {
        (None, _) => bail!("{program} output exceeds limit"),
        (Some(status), _) if !status.success() => bail!("{program} exited with {status}"),
        (Some(_), stdout) => Ok(stdout),
    }
}

/// 边读边判 stdout（P2-6）：最多读 `MAX_OUTPUT + 1` 字节，超了不再读、不等退出，回 `None`；
/// 没超就读到 EOF 再等退出码。
async fn collect(child: &mut Child) -> Result<(Option<ExitStatus>, Vec<u8>)> {
    let mut stdout = child.stdout.take().context("stdout not piped")?;
    let mut buf = Vec::new();
    (&mut stdout)
        .take(MAX_OUTPUT as u64 + 1)
        .read_to_end(&mut buf)
        .await?;
    if buf.len() > MAX_OUTPUT {
        return Ok((None, buf));
    }
    drop(stdout);
    Ok((Some(child.wait().await?), buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn command_has_timeout_and_no_shell() {
        // Arguments reach the program verbatim: no shell expands `$(...)`.
        let literal = "$(id)";
        let echoed = run("printf", ["%s", literal], Duration::from_secs(1)).await;
        assert_eq!(echoed.unwrap(), literal.as_bytes());
        // A child that outlives its deadline is an error.
        let slow = run("sleep", ["2"], Duration::from_millis(10)).await;
        assert!(slow.is_err());
    }

    #[tokio::test]
    async fn output_is_capped_while_reading() {
        // `yes` 永远写不完：老写法读到超时才失败；现在读满上限就停、马上报超长（不是超时）。
        let started = std::time::Instant::now();
        let err = run("yes", std::iter::empty::<&str>(), Duration::from_secs(20))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exceeds limit"), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(10));
        // 刚好到上限的照常返回。
        let out = run(
            "head",
            ["-c", &MAX_OUTPUT.to_string(), "/dev/zero"],
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        assert_eq!(out.len(), MAX_OUTPUT);
        // 多一个字节就算超长。
        let err = run(
            "head",
            ["-c", &(MAX_OUTPUT + 1).to_string(), "/dev/zero"],
            Duration::from_secs(20),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("exceeds limit"), "{err:#}");
    }
}
