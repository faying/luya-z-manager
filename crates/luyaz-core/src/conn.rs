//! 每条 HTTP 连接的底层设置（P2-4）：写超时 + TCP keepalive。
//!
//! SSE（`/events`、`/v2/events`）一共 16 个名额。局域网客户端断电、离开 Wi-Fi 之后不会发 FIN，
//! 它的连接会一直占着名额：hyper 写不出去就停在 `poll_write`，不再拉 SSE 流，流不结束，名额不还。
//! - 写超时（[`TimedIo`]）：一次写连续卡住超过 [`write_timeout`] 就报错，hyper 关掉这条连接，
//!   SSE 流跟着被丢掉，名额还回来。只看「卡住多久」，慢但在读的客户端不受影响。
//! - keepalive + `TCP_USER_TIMEOUT`（Linux）：对端没了时由内核在约一分钟内断开。SSE 一直有数据在发，
//!   keepalive 只管空闲的连接；有未确认数据时靠 `TCP_USER_TIMEOUT`，不等默认约 15 分钟的重传上限。

use axum::{
    extract::connect_info::Connected,
    serve::{IncomingStream, Listener},
};
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    time::Sleep,
};

/// 默认写超时。
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// 写超时：默认 30 秒；`LUYAZ_WRITE_TIMEOUT_MS` 只给测试缩短用（100 ms～10 分钟）。
pub fn write_timeout() -> Duration {
    std::env::var("LUYAZ_WRITE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|ms| Duration::from_millis(ms.clamp(100, 600_000)))
        .unwrap_or(WRITE_TIMEOUT)
}

/// datad 的监听：接受连接时设 keepalive，并套上写超时。
pub struct DatadListener {
    inner: TcpListener,
    write_timeout: Duration,
    /// 只给测试用：`LUYAZ_TEST_SNDBUF`（字节）把发送缓冲压小，「不读的客户端」才能很快写满它。
    /// 回环口的发送缓冲会自动长到几 MB，按 SSE 每秒约 10 KB 要好几分钟才满。
    test_sndbuf: Option<i32>,
}

impl DatadListener {
    pub fn new(inner: TcpListener) -> Self {
        Self {
            inner,
            write_timeout: write_timeout(),
            test_sndbuf: std::env::var("LUYAZ_TEST_SNDBUF")
                .ok()
                .and_then(|v| v.trim().parse::<i32>().ok())
                .filter(|v| *v > 0),
        }
    }
}

impl Listener for DatadListener {
    type Io = TimedIo<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        // axum 对 TcpListener 的 accept 自带出错重试（如 EMFILE 时稍等）。
        let (stream, addr) = Listener::accept(&mut self.inner).await;
        tune(&stream, self.test_sndbuf);
        (TimedIo::new(stream, self.write_timeout), addr)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// 对端地址（`ConnectInfo<Peer>`）。axum 只给它自己的监听类型实现了 `ConnectInfo<SocketAddr>`，
/// 自己的监听要用本地类型。
#[derive(Clone, Copy, Debug)]
pub struct Peer(pub SocketAddr);

impl Connected<IncomingStream<'_, DatadListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, DatadListener>) -> Self {
        Peer(*stream.remote_addr())
    }
}

#[cfg(unix)]
fn tune(stream: &TcpStream, test_sndbuf: Option<i32>) {
    use std::os::fd::AsRawFd;
    fn opt(fd: libc::c_int, level: libc::c_int, name: libc::c_int, value: libc::c_int) {
        // SAFETY: fd 是活着的 socket，value 是栈上的 c_int，长度如实给出。
        unsafe {
            libc::setsockopt(
                fd,
                level,
                name,
                (&value as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
    let fd = stream.as_raw_fd();
    if let Some(bytes) = test_sndbuf {
        opt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, bytes);
    }
    opt(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
    #[cfg(target_os = "linux")]
    {
        // 空闲 60 秒开始探，每 10 秒一次，3 次不回就断。
        opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 60);
        opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 10);
        opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3);
        // 发出去的数据 60 秒没被确认就断（对端已经不在时）。
        opt(fd, libc::IPPROTO_TCP, libc::TCP_USER_TIMEOUT, 60_000);
    }
}

#[cfg(not(unix))]
fn tune(_stream: &TcpStream, _test_sndbuf: Option<i32>) {}

/// 带写超时的 IO：一次写（含 flush/shutdown）从第一次 `Pending` 起连续卡住超过 `timeout`，
/// 就回 `TimedOut`。任何一次写成功都重新计时。读不受影响。
pub struct TimedIo<T> {
    inner: T,
    timeout: Duration,
    stall: Option<Pin<Box<Sleep>>>,
}

impl<T> TimedIo<T> {
    pub fn new(inner: T, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            stall: None,
        }
    }

    fn check<R>(
        &mut self,
        cx: &mut Context<'_>,
        result: Poll<io::Result<R>>,
    ) -> Poll<io::Result<R>> {
        match result {
            Poll::Ready(v) => {
                self.stall = None;
                Poll::Ready(v)
            }
            Poll::Pending => {
                let timeout = self.timeout;
                let stall = self
                    .stall
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)));
                if stall.as_mut().poll(cx).is_ready() {
                    self.stall = None;
                    Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "write timed out: peer is not reading",
                    )))
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for TimedIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for TimedIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let r = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.check(cx, r)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let r = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        this.check(cx, r)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let r = Pin::new(&mut this.inner).poll_flush(cx);
        this.check(cx, r)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let r = Pin::new(&mut this.inner).poll_shutdown(cx);
        this.check(cx, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn write_times_out_when_peer_stops_reading() {
        let (a, mut b) = tokio::io::duplex(64);
        let mut io = TimedIo::new(a, Duration::from_secs(30));
        // 对端不读：64 字节缓冲写满后卡住，30 秒后报 TimedOut。
        let started = tokio::time::Instant::now();
        let err = io.write_all(&[0u8; 1024]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(31));
        // 写进去的那部分对端仍读得到。
        let mut got = [0u8; 64];
        b.read_exact(&mut got).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn slow_but_reading_peer_is_not_cut() {
        let (a, mut b) = tokio::io::duplex(64);
        let mut io = TimedIo::new(a, Duration::from_secs(30));
        // 对端每 20 秒读一次：每次卡住都不到 30 秒，整段写完不报错。
        let reader = tokio::spawn(async move {
            let mut total = 0;
            let mut buf = [0u8; 64];
            while total < 512 {
                tokio::time::sleep(Duration::from_secs(20)).await;
                total += b.read(&mut buf).await.unwrap();
            }
            total
        });
        io.write_all(&[0u8; 512]).await.unwrap();
        assert_eq!(reader.await.unwrap(), 512);
    }
}
