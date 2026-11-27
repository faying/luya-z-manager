//! Connection hardening for the accept loop (ext-1117 A1).
//!
//! The header timeout in `main.rs` only covers the request line and headers. After that, nothing
//! timed the request body, and the sockets had no TCP keepalive: a phone that left Wi-Fi in the
//! middle of a POST left a connection (and its slot, see `guard.rs`) open until the process
//! restarted. Two fixes, both the same as the core's (`luyaz-core/src/conn.rs`):
//!
//! - [`tune`]: TCP keepalive (idle 60 s, every 10 s, 3 probes) and `TCP_USER_TIMEOUT` 60 s, so a
//!   peer that is gone is noticed on any connection, push streams included.
//! - [`guard_body`]: a request body that sends nothing for [`BODY_STALL`] ends with an error; the
//!   handler answers (or the connection closes) and the slot is freed. It is a stall timer, reset
//!   by every chunk, so a slow but moving upload (a config backup over weak Wi-Fi) is not cut.
//!   Requests without a body (GET, the CHILL WebSocket upgrade) are passed through untouched.

use std::future::Future as _;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, header};
use futures_util::Stream;

/// A request body that delivers nothing for this long is cut off.
pub const BODY_STALL: Duration = Duration::from_secs(15);

/// Keepalive and user timeout on an accepted socket. Failures are ignored: the connection works
/// as before, just without the early dead-peer check.
#[cfg(unix)]
pub fn tune(stream: &tokio::net::TcpStream) {
    use std::os::fd::AsRawFd;
    fn opt(fd: libc::c_int, level: libc::c_int, name: libc::c_int, value: libc::c_int) {
        // SAFETY: setsockopt on our own socket with a correctly sized int.
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
    opt(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
    #[cfg(target_os = "linux")]
    {
        opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 60);
        opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 10);
        opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3);
        opt(fd, libc::IPPROTO_TCP, libc::TCP_USER_TIMEOUT, 60_000);
    }
}

#[cfg(not(unix))]
pub fn tune(_stream: &tokio::net::TcpStream) {}

/// The request says it carries a body: a non-zero Content-Length, or chunked.
fn has_body(h: &HeaderMap) -> bool {
    h.contains_key(header::TRANSFER_ENCODING)
        || h
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()?.trim().parse::<u64>().ok())
            .is_some_and(|n| n > 0)
}

/// The request with its body under a stall timer (see the module note). No body: unchanged.
pub fn guard_body<B>(req: Request<B>, stall: Duration) -> Request<Body>
where
    B: axum::body::HttpBody<Data = Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    if !has_body(req.headers()) {
        return req.map(Body::new);
    }
    req.map(|b| {
        Body::from_stream(Stalled {
            inner: Body::new(b).into_data_stream(),
            stall,
            sleep: Box::pin(tokio::time::sleep(stall)),
            done: false,
        })
    })
}

struct Stalled<S> {
    inner: S,
    stall: Duration,
    sleep: Pin<Box<tokio::time::Sleep>>,
    done: bool,
}

impl<S> Stream for Stalled<S>
where
    S: Stream<Item = Result<Bytes, axum::Error>> + Unpin,
{
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(chunk)) => {
                let next = tokio::time::Instant::now() + self.stall;
                self.sleep.as_mut().reset(next);
                Poll::Ready(Some(chunk.map_err(std::io::Error::other)))
            }
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => match self.sleep.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    self.done = true;
                    Poll::Ready(Some(Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "request body stalled",
                    ))))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn req(headers: &[(&str, &str)]) -> Request<Body> {
        let mut b = Request::builder().method("POST").uri("/");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).unwrap()
    }

    #[test]
    fn only_requests_with_a_body_are_guarded() {
        assert!(!has_body(req(&[]).headers()));
        assert!(!has_body(req(&[("content-length", "0")]).headers()));
        assert!(!has_body(req(&[("content-length", "x")]).headers()));
        assert!(has_body(req(&[("content-length", "12")]).headers()));
        assert!(has_body(req(&[("transfer-encoding", "chunked")]).headers()));
    }

    #[tokio::test]
    async fn a_moving_body_is_not_cut_and_a_stalled_one_is() {
        use futures_util::StreamExt;
        let stall = Duration::from_millis(300);
        // moving: a chunk every 150 ms with a 300 ms stall timer, 4 chunks = 600 ms in all
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);
        tokio::spawn(async move {
            for _ in 0..4 {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let _ = tx.send(Ok(Bytes::from_static(b"ab"))).await;
            }
        });
        let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
        let r = Request::builder().header("content-length", "8").body(body).unwrap();
        let got = axum::body::to_bytes(guard_body(r, stall).into_body(), 1024).await;
        assert_eq!(got.unwrap().len(), 8);

        // stalled: one chunk, then nothing
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);
        tx.send(Ok(Bytes::from_static(b"ab"))).await.unwrap();
        let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
        let r = Request::builder().header("content-length", "8").body(body).unwrap();
        let mut s = guard_body(r, stall).into_body().into_data_stream();
        assert!(s.next().await.unwrap().is_ok());
        let started = std::time::Instant::now();
        assert!(s.next().await.unwrap().is_err());
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(2), "{took:?}");
        drop(tx);
    }
}
