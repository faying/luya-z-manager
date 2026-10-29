//! 一行一个 JSON 的帧（R3），读和写两边都有上限（R31）。
//!
//! 读：先找换行再收，收到 `max` 字节还没换行就报 [`ReadError::TooLong`]，不会为一条坏帧一直涨内存。
//! 写：一个写任务按顺序送出；排队的字节超过上限就把连线标成断开（对端不读的时候）。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

#[derive(Debug)]
pub enum ReadError {
    /// 超过单帧上限还没换行。
    TooLong,
    Io(std::io::Error),
}

/// 读一帧（不含换行）到 `buf`。对端关了返回 `Ok(false)`；关之前没写完的半帧丢掉。
pub async fn read_line<R: AsyncBufRead + Unpin>(
    r: &mut R,
    max: usize,
    buf: &mut Vec<u8>,
) -> Result<bool, ReadError> {
    buf.clear();
    loop {
        let avail = r.fill_buf().await.map_err(ReadError::Io)?;
        if avail.is_empty() {
            return Ok(false);
        }
        if let Some(i) = avail.iter().position(|&b| b == b'\n') {
            if buf.len() + i > max {
                return Err(ReadError::TooLong);
            }
            buf.extend_from_slice(&avail[..i]);
            r.consume(i + 1);
            return Ok(true);
        }
        let n = avail.len();
        if buf.len() + n > max {
            return Err(ReadError::TooLong);
        }
        buf.extend_from_slice(avail);
        r.consume(n);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    /// 排队的字节超过上限：连线已标成断开。
    Overflow,
    /// 连线已经断了。
    Closed,
}

/// 往一条连线写帧的一端。克隆共用同一个队列、计数和断开状态。
#[derive(Clone)]
pub struct Sender {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    max: usize,
    /// 断开了没有；读循环和写任务都等它。
    state: Arc<watch::Sender<bool>>,
}

impl Sender {
    pub fn send(&self, v: &Value) -> Result<(), SendError> {
        if self.is_closed() {
            return Err(SendError::Closed);
        }
        let mut bytes = serde_json::to_vec(v).unwrap_or_default();
        bytes.push(b'\n');
        let n = bytes.len();
        if self.queued.fetch_add(n, Ordering::AcqRel) + n > self.max {
            self.queued.fetch_sub(n, Ordering::AcqRel);
            self.close();
            return Err(SendError::Overflow);
        }
        self.tx.send(bytes).map_err(|_| {
            self.close();
            SendError::Closed
        })
    }

    /// 标成断开：写任务停下并关掉写端，读循环醒来。
    pub fn close(&self) {
        self.state.send_replace(true);
    }

    pub fn is_closed(&self) -> bool {
        *self.state.borrow()
    }

    /// 连线被标成断开（队列满、写失败、[`Sender::close`]）时醒来。
    pub async fn closed(&self) {
        let mut rx = self.state.subscribe();
        let _ = rx.wait_for(|closed| *closed).await;
    }
}

/// 起写任务；`max` 是排队字节上限。
pub fn spawn_writer<W: AsyncWrite + Unpin + Send + 'static>(mut w: W, max: usize) -> Sender {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let sender = Sender {
        tx,
        queued: Arc::new(AtomicUsize::new(0)),
        max,
        state: Arc::new(watch::channel(false).0),
    };
    let s = sender.clone();
    tokio::spawn(async move {
        loop {
            let bytes = tokio::select! {
                b = rx.recv() => match b {
                    Some(b) => b,
                    None => break,
                },
                _ = s.closed() => break,
            };
            let n = bytes.len();
            let wrote = tokio::select! {
                r = w.write_all(&bytes) => r.is_ok(),
                _ = s.closed() => false,
            };
            if !wrote {
                s.close();
                break;
            }
            s.queued.fetch_sub(n, Ordering::AcqRel);
        }
        let _ = w.shutdown().await;
    });
    sender
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, BufReader};

    #[tokio::test]
    async fn lines_split_and_a_half_line_at_close_is_dropped() {
        let data: &[u8] = b"{\"a\":1}\n{\"b\":2}\n{\"c\"";
        let mut r = BufReader::with_capacity(4, data);
        let mut buf = Vec::new();
        assert!(read_line(&mut r, 64, &mut buf).await.unwrap());
        assert_eq!(buf, b"{\"a\":1}");
        assert!(read_line(&mut r, 64, &mut buf).await.unwrap());
        assert_eq!(buf, b"{\"b\":2}");
        assert!(!read_line(&mut r, 64, &mut buf).await.unwrap());
    }

    #[tokio::test]
    async fn a_line_without_newline_past_the_limit_is_refused() {
        // 对端一直送、不换行：读到上限就停，不会等到对端送完
        let (mut a, b) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            let chunk = vec![b'x'; 512];
            while a.write_all(&chunk).await.is_ok() {}
        });
        let mut r = BufReader::new(b);
        let mut buf = Vec::new();
        assert!(matches!(
            read_line(&mut r, 4096, &mut buf).await,
            Err(ReadError::TooLong)
        ));
        assert!(buf.len() <= 4096);
        // 刚好等于上限的行可以
        let line = [vec![b'y'; 16], vec![b'\n']].concat();
        let mut r = BufReader::new(&line[..]);
        assert!(read_line(&mut r, 16, &mut buf).await.unwrap());
    }

    #[tokio::test]
    async fn a_peer_that_stops_reading_overflows_the_queue_and_closes() {
        // 对端完全不读：duplex 缓冲 256 字节塞满后写任务卡住，队列涨到上限就断开
        let (a, _b_kept_but_never_read) = tokio::io::duplex(256);
        let s = spawn_writer(a, 2048);
        let big = json!({"pad": "z".repeat(200)});
        let mut overflow = false;
        for _ in 0..100 {
            match s.send(&big) {
                Ok(()) => tokio::task::yield_now().await,
                Err(SendError::Overflow) => {
                    overflow = true;
                    break;
                }
                Err(SendError::Closed) => panic!("closed before the limit"),
            }
        }
        assert!(overflow);
        assert!(s.is_closed());
        s.closed().await;
        assert_eq!(s.send(&json!({})), Err(SendError::Closed));
    }

    #[tokio::test]
    async fn frames_arrive_in_order_one_per_line() {
        let (a, mut b) = tokio::io::duplex(4096);
        let s = spawn_writer(a, 4096);
        s.send(&json!({"n":1})).unwrap();
        s.send(&json!({"n":2})).unwrap();
        let mut got = vec![0u8; 16];
        b.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"{\"n\":1}\n{\"n\":2}\n");
    }
}
