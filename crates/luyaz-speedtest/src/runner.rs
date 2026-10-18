//! HTTP measurement for both endpoint protocols, sharing one budget across all attempts.
use super::*;
pub(super) struct Budget {
    deadline: Instant,
    max_bytes: u64,
    used: u64,
}
impl Budget {
    pub(super) fn new(lim: Limits) -> Self {
        Self {
            deadline: Instant::now()
                + if lim.upload {
                    Duration::from_secs(60)
                } else {
                    lim.dl_time
                },
            max_bytes: lim.dl_max_bytes.unwrap_or(512_000_000),
            used: 0,
        }
    }
    pub(super) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
    fn room(&self) -> usize {
        self.max_bytes
            .saturating_sub(self.used)
            .min(usize::MAX as u64) as usize
    }
    fn timeout(&self, max: Duration) -> Result<Duration, Failure> {
        if self.remaining().is_zero() || self.room() == 0 {
            return Err(exhausted());
        }
        Ok(self.remaining().min(max))
    }
}
fn exhausted() -> Failure {
    Failure {
        code: "budget_exhausted",
        zh: "测速已达到本次时间或流量上限，请重试或切换来源。".into(),
        en: "The test reached its time or data limit. Retry or switch provider.".into(),
    }
}
fn connection(e: &ureq::Error, provider: Provider) -> Failure {
    let timed = matches!(e, ureq::Error::Timeout(_));
    Failure::service(
        if timed {
            "provider_timeout"
        } else {
            "provider_unreachable"
        },
        provider,
        if timed { "timed out" } else { "is unreachable" },
    )
}
fn transfer(provider: Provider) -> Failure {
    Failure::service(
        "transfer_failed",
        provider,
        "could not complete the transfer",
    )
}
fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}
fn fresh(url: &str, n: u64) -> String {
    // Defeat caching: repeated static downloads must reach the measurement server.
    format!(
        "{url}{}luyaz={n}-{}",
        if url.contains('?') { '&' } else { '?' },
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
pub(super) fn run(
    server: &TestServer,
    progress: &Arc<Mutex<SpeedTestProgress>>,
    cancel: &AtomicBool,
    lim: Limits,
    budget: &mut Budget,
) -> Result<(), Failure> {
    budget.timeout(Duration::from_secs(3))?;
    let agent: ureq::Agent = ureq::Agent::config_builder().build().into();
    if lim.ping {
        progress.lock().unwrap().phase = Phase::Latency;
        let mut rtts = Vec::new();
        let mut failed = 0;
        for i in 0..PING_COUNT {
            if cancelled(cancel) {
                return Ok(());
            }
            let t = Instant::now();
            let sample = agent
                .get(fresh(&server.ping_url, i as u64).as_str())
                .config()
                .timeout_global(Some(budget.timeout(Duration::from_secs(3))?))
                .build()
                .call();
            match sample {
                Ok(r) => {
                    if r.into_body()
                        .with_config()
                        .limit(1024)
                        .read_to_vec()
                        .is_ok()
                    {
                        rtts.push(t.elapsed().as_secs_f64() * 1000.0);
                    } else {
                        failed += 1;
                    }
                }
                Err(e) => {
                    failed += 1;
                    if failed >= 2 && rtts.is_empty() {
                        return Err(connection(&e, server.provider));
                    }
                }
            }
            if failed >= 2 && rtts.is_empty() {
                return Err(transfer(server.provider));
            }
            progress.lock().unwrap().progress = ((i + 1) * 20 / PING_COUNT) as u8;
        }
        if rtts.is_empty() {
            return Err(transfer(server.provider));
        }
        // Jitter is variation in successive samples, not in sorted samples.
        let jitter = if rtts.len() > 1 {
            rtts.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (rtts.len() - 1) as f64
        } else {
            0.0
        };
        rtts.sort_by(f64::total_cmp);
        let mut p = progress.lock().unwrap();
        p.ping_ms = Some(round2(rtts[rtts.len() / 2]));
        p.jitter_ms = Some(round2(jitter));
    }
    if cancelled(cancel) {
        return Ok(());
    }
    progress.lock().unwrap().phase = Phase::Download;
    let start = Instant::now();
    let deadline = (start + lim.dl_time).min(budget.deadline);
    let mut bytes = 0u64;
    let mut buf = [0u8; BUF_SIZE];
    let (from, span) = if lim.upload { (20, 40) } else { (0, 100) };
    let mut requests = 0u64;
    while Instant::now() < deadline
        && budget.room()
            > if lim.upload {
                UPLOAD_SIZE * UPLOAD_ROUNDS
            } else {
                0
            }
    {
        if cancelled(cancel) {
            return Ok(());
        }
        requests += 1;
        let request_start = Instant::now();
        let request_timeout = budget.timeout(Duration::from_secs(3))?;
        let r = agent
            .get(fresh(&server.download_url, requests).as_str())
            .config()
            .timeout_global(Some(request_timeout))
            .build()
            .call()
            .map_err(|e| connection(&e, server.provider))?;
        if r.headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .is_some_and(|t| t.starts_with("text/html"))
        {
            return Err(transfer(server.provider));
        }
        let mut body = r.into_body();
        let mut reader = body.as_reader();
        let before = bytes;
        while Instant::now() < deadline && budget.room() > 0 {
            if cancelled(cancel) {
                return Ok(());
            }
            let cap = BUF_SIZE.min(budget.room());
            match reader.read(&mut buf[..cap]) {
                Ok(0) => break,
                Ok(n) => {
                    bytes += n as u64;
                    budget.used += n as u64;
                    let mut p = progress.lock().unwrap();
                    p.download_bytes += n as u64;
                    p.live_speed_mbps = round2(
                        bytes as f64 * 8.0 / start.elapsed().as_secs_f64().max(0.000001) / 1e6,
                    );
                    p.progress = from
                        + ((start.elapsed().as_secs_f64() / lim.dl_time.as_secs_f64())
                            * span as f64)
                            .min(span as f64) as u8;
                }
                // Streaming endpoints can outlive our short I/O deadline. Start a new request,
                // retaining bytes and elapsed time; other early read errors are real failures.
                Err(_)
                    if request_start.elapsed() + Duration::from_millis(50) >= request_timeout
                        && bytes > before =>
                {
                    break
                }
                Err(_) => return Err(transfer(server.provider)),
            }
        }
        if bytes == before {
            return Err(transfer(server.provider));
        }
    }
    if bytes == 0 {
        return Err(transfer(server.provider));
    }
    {
        let mut p = progress.lock().unwrap();
        p.download_mbps = Some(round2(
            bytes as f64 * 8.0 / start.elapsed().as_secs_f64().max(0.000001) / 1e6,
        ));
        p.live_speed_mbps = 0.0;
    }
    if !lim.upload {
        return Ok(());
    }
    if cancelled(cancel) {
        return Ok(());
    }
    progress.lock().unwrap().phase = Phase::Upload;
    let payload = vec![0x5au8; UPLOAD_SIZE];
    let start = Instant::now();
    let mut bytes = 0u64;
    for i in 0..UPLOAD_ROUNDS {
        if cancelled(cancel) {
            return Ok(());
        }
        if budget.room() < UPLOAD_SIZE {
            return Err(exhausted());
        }
        let timeout = budget.timeout(Duration::from_secs(10))?;
        // Count attempted upload payloads even when the peer fails after receiving them.
        budget.used += UPLOAD_SIZE as u64;
        progress.lock().unwrap().upload_bytes += UPLOAD_SIZE as u64;
        agent
            .post(fresh(&server.url, i as u64).as_str())
            .config()
            .timeout_global(Some(timeout))
            .build()
            .send(&payload[..])
            .map_err(|e| connection(&e, server.provider))?
            .into_body()
            .with_config()
            .limit(1024)
            .read_to_vec()
            .map_err(|_| transfer(server.provider))?;
        bytes += UPLOAD_SIZE as u64;
        let mut p = progress.lock().unwrap();
        p.progress = 60 + ((i + 1) * 40 / UPLOAD_ROUNDS) as u8;
        p.live_speed_mbps =
            round2(bytes as f64 * 8.0 / start.elapsed().as_secs_f64().max(0.000001) / 1e6);
    }
    progress.lock().unwrap().upload_mbps = Some(round2(
        bytes as f64 * 8.0 / start.elapsed().as_secs_f64().max(0.000001) / 1e6,
    ));
    Ok(())
}
