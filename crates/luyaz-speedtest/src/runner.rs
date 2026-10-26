//! HTTP measurement for both endpoint protocols, sharing one budget across all attempts.
//!
//! Each stage runs N connection threads (each its own `ureq::Agent`, so its own kept-alive
//! connection), an optional latency probe thread, and a coordinator (the calling thread) that
//! merges the counters into the progress snapshot every 100 ms. Every byte is reserved from
//! the stage's and the run's pools before it is read, so caps are exact across threads. All
//! threads are joined before a stage returns: nothing outlives the run.
use super::*;
use crate::verdict::Loaded;
use std::io::Read;
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::thread;

/// Transfer threads' stack (the buffers are on the heap).
const STACK: usize = 512 * 1024; // TLS handshakes (https LibreSpeed) need headroom
/// How often the coordinator merges counters into the progress snapshot.
const TICK: Duration = Duration::from_millis(100);
/// An upload connection's first POST; it doubles while a POST takes under a second, up to
/// `Limits::upload_chunk`, so a slow uplink still gets acknowledged chunks inside the stage.
const UPLOAD_FIRST: usize = 512 * 1024;
/// A planned stage smaller than this share of its cap is cut (`budget`).
const MIN_SHARE: u64 = 4;

/// A byte pool several threads draw from: reserve, read, return what was not read.
pub(super) struct Pool {
    used: AtomicU64,
    cap: u64,
}
impl Pool {
    pub(super) fn new(cap: u64) -> Self {
        Self {
            used: AtomicU64::new(0),
            cap,
        }
    }
    /// Reserve up to `want`; 0 = the pool is empty.
    fn take(&self, want: u64) -> u64 {
        let mut got = 0;
        let _ = self
            .used
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |u| {
                got = want.min(self.cap.saturating_sub(u));
                (got > 0).then_some(u + got)
            });
        got
    }
    fn give(&self, n: u64) {
        let _ = self
            .used
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |u| {
                Some(u.saturating_sub(n))
            });
    }
    /// Counted, never refused (latency probes: a few hundred bytes each).
    fn add(&self, n: u64) {
        self.used.fetch_add(n, Ordering::SeqCst);
    }
    pub(super) fn room(&self) -> u64 {
        self.cap.saturating_sub(self.used.load(Ordering::SeqCst))
    }
}
/// A stage's own cap inside the run's pool.
struct Grant<'a> {
    stage: Pool,
    total: &'a Pool,
}
impl Grant<'_> {
    fn take(&self, want: u64) -> u64 {
        let s = self.stage.take(want);
        if s == 0 {
            return 0;
        }
        let t = self.total.take(s);
        if t < s {
            self.stage.give(s - t);
        }
        t
    }
    fn give(&self, n: u64) {
        self.stage.give(n);
        self.total.give(n);
    }
}
pub(super) struct Budget {
    deadline: Instant,
    pub(super) total: Pool,
    /// the CHILL leg's own pool (R5: +400 MB)
    chill: Pool,
}
impl Budget {
    pub(super) fn new(lim: Limits, chill: bool) -> Self {
        Self {
            deadline: Instant::now() + lim.time_cap(),
            total: Pool::new(lim.total_max),
            chill: Pool::new(if chill { lim.chill_max } else { 0 }),
        }
    }
    pub(super) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
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
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
fn mbps(bytes: u64, secs: f64) -> f64 {
    round2(bytes as f64 * 8.0 / secs.max(0.000001) / 1e6)
}
/// Direct agents ignore any proxy environment; a CHILL agent must get its proxy or nothing.
fn agent(proxy: Option<&str>) -> Option<ureq::Agent> {
    let p = match proxy {
        Some(addr) => Some(ureq::Proxy::new(&format!("http://{addr}")).ok()?),
        None => None,
    };
    Some(
        ureq::Agent::config_builder()
            .proxy(p)
            .max_idle_connections_per_host(2)
            .build()
            .into(),
    )
}
fn median(v: &mut [f64]) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied()
}

pub(super) struct Env<'a> {
    pub progress: &'a Mutex<SpeedTestProgress>,
    pub cancel: &'a AtomicBool,
    pub lim: Limits,
    pub budget: &'a Budget,
    pub chill: chill::Env<'a>,
}

/// One latency request on a kept-alive agent: its round trip, or the error (`None`: the body
/// failed).
fn probe_once(
    agent: &ureq::Agent,
    url: &str,
    timeout: Duration,
) -> Result<(Duration, u64), Option<ureq::Error>> {
    let t = Instant::now();
    let r = agent
        .get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()
        .map_err(Some)?;
    let body = r
        .into_body()
        .with_config()
        .limit(1024)
        .read_to_vec()
        .map_err(|_| None)?;
    Ok((t.elapsed(), body.len() as u64 + 256))
}
/// One loaded-stage sample (R8): answered → RTT; slower than the cap → the cap, not dropped;
/// cut short by the stage end or failed at once → none.
fn sample_rtt(
    agent: &ureq::Agent,
    url: &str,
    cap: Duration,
    left: Duration,
    total: &Pool,
) -> Option<f64> {
    let t = Instant::now();
    match probe_once(agent, url, cap.min(left)) {
        Ok((rtt, bytes)) => {
            total.add(bytes);
            Some(ms(rtt.min(cap)))
        }
        Err(_) if t.elapsed() + Duration::from_millis(10) >= cap => Some(ms(cap)),
        Err(_) => None,
    }
}

/// What one stage is.
struct Spec<'a> {
    /// `None`: a node trial (the run is still "discovering")
    stage: Option<(Stage, Phase, u8)>,
    url: &'a str,
    ping_url: &'a str,
    conns: u8,
    time: Duration,
    /// a trial ends here once it has bytes; otherwise it waits until `time`
    soft: Option<Duration>,
    cap: u64,
    total: &'a Pool,
    proxy: Option<&'a str>,
    probe: bool,
    cpu: bool,
    /// the CHILL leg: the first error ends the stage (never retried, never direct)
    fail_fast: bool,
    upload: bool,
    /// the progress bar's share: (from, span)
    pct: Option<(u8, u8)>,
}
#[derive(Default)]
struct StageOut {
    /// received; on upload: acknowledged
    bytes: u64,
    whole: Option<f64>,
    steady: Option<f64>,
    steady_secs: f64,
    errors: u32,
    first_err: Option<Failure>,
    loaded: Loaded,
    cpu_peak: Option<f64>,
}
/// What the threads of one stage share.
struct Worker<'a> {
    url: &'a str,
    provider: Provider,
    start: Instant,
    deadline: Instant,
    grant: &'a Grant<'a>,
    bytes: &'a AtomicU64,
    sent: &'a AtomicU64,
    /// ns since `start` of the last counted byte
    last_at: &'a AtomicU64,
    stop: &'a AtomicBool,
    cancel: &'a AtomicBool,
    errors: &'a AtomicU32,
    first_err: &'a Mutex<Option<Failure>>,
    fail_fast: bool,
}
impl Worker<'_> {
    fn over(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
            || self.cancel.load(Ordering::Relaxed)
            || Instant::now() >= self.deadline
    }
    /// An error now is the stage ending, not the node failing.
    fn late(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
            || self.cancel.load(Ordering::Relaxed)
            || Instant::now() + Duration::from_millis(50) >= self.deadline
    }
    fn error(&self, f: Failure) {
        self.errors.fetch_add(1, Ordering::SeqCst);
        let mut first = self.first_err.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_none() {
            *first = Some(f);
        }
        if self.fail_fast {
            self.stop.store(true, Ordering::SeqCst);
        }
    }
    fn count(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::SeqCst);
        let at = u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.last_at.fetch_max(at, Ordering::SeqCst);
    }
    /// One long connection: request after request on the same agent until the stage ends;
    /// each request's timeout is the stage's remaining time (no 3 s reconnects).
    fn download(&self, agent: &ureq::Agent) {
        let mut buf = vec![0u8; BUF_SIZE];
        let (mut n, mut fails) = (0u64, 0u32);
        'conn: while !self.over() {
            n += 1;
            let left = self.deadline.saturating_duration_since(Instant::now());
            let r = match agent
                .get(fresh(self.url, n).as_str())
                .config()
                .timeout_global(Some(left))
                .build()
                .call()
            {
                Ok(r) => r,
                Err(e) => {
                    if self.late() {
                        break;
                    }
                    self.error(connection(&e, self.provider));
                    fails += 1;
                    if self.fail_fast || fails >= 3 {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            if r.headers()
                .get("content-type")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|t| t.starts_with("text/html"))
            {
                self.error(transfer(self.provider));
                break;
            }
            let mut body = r.into_body();
            let mut reader = body.as_reader();
            let mut got = false;
            loop {
                if self.over() {
                    break 'conn;
                }
                let want = self.grant.take(BUF_SIZE as u64);
                if want == 0 {
                    break 'conn; // the stage's or the run's bytes are spent
                }
                let Some(dst) =
                    buf.get_mut(..usize::try_from(want).unwrap_or(BUF_SIZE).min(BUF_SIZE))
                else {
                    self.grant.give(want);
                    break 'conn;
                };
                match reader.read(dst) {
                    Ok(0) => {
                        self.grant.give(want);
                        break;
                    }
                    Ok(k) => {
                        let k = (k as u64).min(want);
                        self.grant.give(want - k);
                        self.count(k);
                        got = true;
                    }
                    Err(_) => {
                        self.grant.give(want);
                        if self.late() {
                            break 'conn;
                        }
                        self.error(transfer(self.provider));
                        if self.fail_fast {
                            break 'conn;
                        }
                        break;
                    }
                }
            }
            if got {
                fails = 0;
            } else {
                fails += 1;
                if fails >= 3 {
                    break;
                }
            }
        }
    }
    /// Fixed-size POSTs one after another on one connection (R7); only acknowledged ones
    /// count toward the rate. Returns (start s, end s, bytes) per acknowledged POST.
    fn upload(&self, agent: &ureq::Agent, payload: &[u8]) -> Vec<(f64, f64, u64)> {
        let mut acks = Vec::new();
        let mut chunk = UPLOAD_FIRST.min(payload.len());
        let (mut n, mut fails) = (0u64, 0u32);
        while !self.over() && chunk > 0 {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let got = self.grant.take(chunk as u64);
            if got < (64 * 1024).min(chunk as u64) {
                self.grant.give(got);
                break;
            }
            let size = usize::try_from(got).unwrap_or(chunk).min(payload.len());
            let Some(body) = payload.get(..size) else {
                break;
            };
            // what was sent is data used, acknowledged or not
            self.sent.fetch_add(size as u64, Ordering::SeqCst);
            n += 1;
            let t0 = self.start.elapsed().as_secs_f64();
            let r = agent
                .post(fresh(self.url, n).as_str())
                .config()
                .timeout_global(Some(left))
                .build()
                .send(body)
                .and_then(|r| r.into_body().with_config().limit(64 * 1024).read_to_vec());
            match r {
                Ok(_) => {
                    let t1 = self.start.elapsed().as_secs_f64();
                    acks.push((t0, t1, size as u64));
                    self.count(size as u64);
                    fails = 0;
                    if t1 - t0 < 1.0 && chunk < payload.len() {
                        chunk = (chunk * 2).min(payload.len());
                    }
                }
                Err(e) => {
                    if self.late() {
                        break;
                    }
                    self.error(connection(&e, self.provider));
                    fails += 1;
                    if self.fail_fast || fails >= 3 {
                        break;
                    }
                }
            }
        }
        acks
    }
}
/// The latency probe under load: its own kept-alive agent, one GET every `probe_every`.
fn probe_loop(
    agent: &ureq::Agent,
    url: &str,
    stop: &AtomicBool,
    cancel: &AtomicBool,
    deadline: Instant,
    lim: Limits,
    total: &Pool,
) -> Vec<f64> {
    let mut v = Vec::new();
    let mut n = 0u64;
    while !stop.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
        let t = Instant::now();
        let left = deadline.saturating_duration_since(t);
        if left.is_zero() {
            break;
        }
        n += 1;
        if let Some(s) = sample_rtt(agent, &fresh(url, n), lim.probe_cap, left, total) {
            v.push(s);
        }
        let next = t + lim.probe_every;
        while !stop.load(Ordering::Relaxed) && Instant::now() < next {
            thread::sleep(next.saturating_duration_since(Instant::now()).min(TICK));
        }
    }
    v
}
/// Whole-stage mean, the rate after the ramp, and how long that part is.
fn rates(samples: &[(f64, u64)], ramp: f64) -> (Option<f64>, Option<f64>, f64) {
    let Some(&(t_end, b_end)) = samples.last() else {
        return (None, None, 0.0);
    };
    let whole = (t_end > 0.0 && b_end > 0).then(|| mbps(b_end, t_end));
    match samples.iter().find(|(t, _)| *t >= ramp) {
        Some(&(t0, b0)) if t_end - t0 >= 0.05 && b_end > b0 => {
            (whole, Some(mbps(b_end - b0, t_end - t0)), t_end - t0)
        }
        _ => (whole, None, 0.0),
    }
}
/// Upload rates from acknowledged POSTs, each spread evenly over its own request time.
fn upload_rates(acks: &[(f64, f64, u64)], ramp: f64) -> (Option<f64>, Option<f64>, f64) {
    let Some(end) = acks.iter().map(|a| a.1).reduce(f64::max) else {
        return (None, None, 0.0);
    };
    let total: u64 = acks.iter().map(|a| a.2).sum();
    let whole = (end > 0.0 && total > 0).then(|| mbps(total, end));
    if end - ramp < 0.05 {
        return (whole, None, 0.0);
    }
    let inside: f64 = acks
        .iter()
        .map(|&(t0, t1, n)| {
            let len = t1 - t0;
            if len <= 0.0 {
                return if t1 >= ramp { n as f64 } else { 0.0 };
            }
            let overlap = (t1.min(end) - t0.max(ramp)).max(0.0);
            n as f64 * overlap / len
        })
        .sum();
    let steady = (inside > 0.0).then(|| round2(inside * 8.0 / (end - ramp) / 1e6));
    (whole, steady, end - ramp)
}
fn stage(env: &Env, server: &TestServer, s: &Spec) -> StageOut {
    let lim = env.lim;
    let start = Instant::now();
    let deadline = (start + s.time).min(env.budget.deadline);
    let grant = Grant {
        stage: Pool::new(s.cap),
        total: s.total,
    };
    let (bytes, sent, last_at) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let stop = AtomicBool::new(false);
    let errors = AtomicU32::new(0);
    let first_err = Mutex::new(None);
    let payload = if s.upload {
        vec![0x5au8; lim.upload_chunk]
    } else {
        Vec::new()
    };
    let (base_down, base_up) = {
        let mut p = env.progress.lock().unwrap();
        if let Some((st, ph, step)) = &s.stage {
            p.stage = Some(*st);
            p.phase = *ph;
            p.step = *step;
        }
        p.live_speed_mbps = 0.0;
        (p.download_bytes, p.upload_bytes)
    };
    let mut samples: Vec<(f64, u64)> = vec![(0.0, 0)];
    let mut acks: Vec<(f64, f64, u64)> = Vec::new();
    let mut probes: Vec<f64> = Vec::new();
    let mut cpu_peak: Option<f64> = None;
    let w = Worker {
        url: s.url,
        provider: server.provider,
        start,
        deadline,
        grant: &grant,
        bytes: &bytes,
        sent: &sent,
        last_at: &last_at,
        stop: &stop,
        cancel: env.cancel,
        errors: &errors,
        first_err: &first_err,
        fail_fast: s.fail_fast,
    };
    thread::scope(|sc| {
        let mut conns = Vec::new();
        for i in 0..s.conns.max(1) {
            let Some(agent) = agent(s.proxy) else {
                w.error(transfer(server.provider));
                break;
            };
            let (w, payload, upload) = (&w, &payload[..], s.upload);
            let spawned = thread::Builder::new()
                .name(format!("speed-{i}"))
                .stack_size(STACK)
                .spawn_scoped(sc, move || {
                    if upload {
                        w.upload(&agent, payload)
                    } else {
                        w.download(&agent);
                        Vec::new()
                    }
                });
            // a thread that cannot start: run with the ones that did
            match spawned {
                Ok(h) => conns.push(h),
                Err(_) => break,
            }
        }
        let prober = if s.probe {
            agent(s.proxy).and_then(|a| {
                let (stop, cancel, total, url) = (&stop, env.cancel, s.total, s.ping_url);
                thread::Builder::new()
                    .name("speed-probe".into())
                    .stack_size(STACK)
                    .spawn_scoped(sc, move || {
                        probe_loop(&a, url, stop, cancel, deadline, lim, total)
                    })
                    .ok()
            })
        } else {
            None
        };
        let mut cpu_last = if s.cpu { sys::cores() } else { None };
        let mut cpu_at = Instant::now();
        loop {
            thread::sleep(TICK.min(deadline.saturating_duration_since(Instant::now())));
            let now = start.elapsed().as_secs_f64();
            let b = bytes.load(Ordering::SeqCst);
            samples.push((now, b));
            let window = samples
                .iter()
                .find(|(t, _)| *t >= now - 1.0)
                .copied()
                .unwrap_or((0.0, 0));
            {
                let mut p = env.progress.lock().unwrap();
                if s.upload {
                    p.upload_bytes = base_up + sent.load(Ordering::SeqCst);
                } else {
                    p.download_bytes = base_down + b;
                }
                if s.stage.is_some() && now - window.0 >= 0.05 {
                    p.live_speed_mbps = mbps(b.saturating_sub(window.1), now - window.0);
                }
                if let Some((from, span)) = s.pct {
                    let f = (now / s.time.as_secs_f64().max(0.001)).min(1.0);
                    p.progress = from.saturating_add((f * span as f64) as u8);
                }
            }
            if let Some(prev) = &cpu_last {
                if cpu_at.elapsed() >= Duration::from_millis(500) {
                    if let Some(cur) = sys::cores() {
                        if let Some(pk) = sys::peak(prev, &cur) {
                            cpu_peak = Some(cpu_peak.map_or(pk, |c: f64| c.max(pk)));
                        }
                        cpu_last = Some(cur);
                        cpu_at = Instant::now();
                    }
                }
            }
            let done = conns.iter().all(|h| h.is_finished());
            let soft = s.soft.is_some_and(|t| start.elapsed() >= t && b > 0);
            if done || soft || Instant::now() >= deadline || env.cancel.load(Ordering::Relaxed) {
                break;
            }
        }
        stop.store(true, Ordering::SeqCst);
        for h in conns {
            if let Ok(v) = h.join() {
                acks.extend(v);
            }
        }
        if let Some(h) = prober {
            if let Ok(v) = h.join() {
                probes = v;
            }
        }
    });
    let total = bytes.load(Ordering::SeqCst);
    // the stage ends at its last counted byte (a byte cap can end it before the deadline)
    let end = last_at.load(Ordering::SeqCst) as f64 / 1e9;
    samples.retain(|(t, _)| *t < end);
    samples.push((end, total));
    let ramp = lim.ramp.as_secs_f64();
    let (whole, steady, steady_secs) = if s.upload {
        upload_rates(&acks, ramp)
    } else {
        rates(&samples, ramp)
    };
    {
        let mut p = env.progress.lock().unwrap();
        if s.upload {
            p.upload_bytes = base_up + sent.load(Ordering::SeqCst);
        } else {
            p.download_bytes = base_down + total;
        }
        p.live_speed_mbps = 0.0;
    }
    let n = probes.len();
    StageOut {
        bytes: total,
        whole,
        steady,
        steady_secs,
        errors: errors.load(Ordering::SeqCst),
        first_err: first_err.into_inner().unwrap_or_else(|e| e.into_inner()),
        loaded: Loaded {
            median: median(&mut probes),
            n,
        },
        cpu_peak,
    }
}

/// What the verdict reads (the rest went into the progress snapshot as it was measured).
pub(super) struct Outcome {
    multi: f64,
    single: Result<f64, &'static str>,
    chill: Result<f64, &'static str>,
    idle_ms: Option<f64>,
    loaded_down: Loaded,
    loaded_up: Loaded,
    min_probes: usize,
    cpu_peak: Option<f64>,
    mihomo_cpu: Option<f64>,
    pub sample_short: bool,
}
impl Outcome {
    pub(super) fn inputs<'a>(&self, radio: Option<&'a Radio>) -> verdict::Inputs<'a> {
        verdict::Inputs {
            multi: self.multi,
            single: self.single,
            chill: self.chill,
            idle_ms: self.idle_ms,
            loaded_down: self.loaded_down,
            loaded_up: self.loaded_up,
            min_probes: self.min_probes,
            radio,
            cpu_peak: self.cpu_peak,
            mihomo_cpu: self.mihomo_cpu,
        }
    }
}
/// The idle round: ten probes on the probe agent; two failures before any answer = the node
/// is not reachable (the old rule).
fn idle(
    env: &Env,
    server: &TestServer,
    agent: &ureq::Agent,
    pct: (u8, u8),
) -> Result<Option<(f64, f64)>, Failure> {
    {
        let mut p = env.progress.lock().unwrap();
        p.phase = Phase::Latency;
        p.stage = Some(Stage::IdleLatency);
        p.step = 1;
    }
    let lim = env.lim;
    let every = lim.idle_time / PING_COUNT as u32;
    let mut rtts = Vec::new();
    let mut failed = 0;
    for i in 0..PING_COUNT {
        if env.cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let t = Instant::now();
        let left = env.budget.remaining();
        if left.is_zero() {
            return Err(exhausted());
        }
        match probe_once(
            agent,
            &fresh(&server.ping_url, i as u64),
            lim.probe_cap.min(left),
        ) {
            Ok((rtt, bytes)) => {
                env.budget.total.add(bytes);
                rtts.push(ms(rtt));
            }
            Err(e) => {
                failed += 1;
                if failed >= 2 && rtts.is_empty() {
                    return Err(match e {
                        Some(e) => connection(&e, server.provider),
                        None => transfer(server.provider),
                    });
                }
            }
        }
        env.progress.lock().unwrap().progress =
            pct.0 + ((i + 1) * pct.1 as usize / PING_COUNT) as u8;
        thread::sleep((t + every).saturating_duration_since(Instant::now()));
    }
    if rtts.is_empty() {
        return Err(transfer(server.provider));
    }
    // Jitter is variation in successive samples, not in sorted samples.
    let jitter = if rtts.len() > 1 {
        rtts.windows(2)
            .map(|w| match w {
                [a, b] => (b - a).abs(),
                _ => 0.0,
            })
            .sum::<f64>()
            / (rtts.len() - 1) as f64
    } else {
        0.0
    };
    let med = median(&mut rtts).unwrap_or(0.0);
    Ok(Some((round2(med), round2(jitter))))
}

/// R10: the reachable nodes by latency (top 3), each tried for a second on 2 connections;
/// a node still at 0 bytes after `trial_zero` is dropped. Best throughput first.
pub(super) fn pick(list: Vec<TestServer>, env: &Env) -> Vec<TestServer> {
    let lim = env.lim;
    let mut reach: Vec<(f64, TestServer)> = thread::scope(|sc| {
        let hs: Vec<_> = list
            .into_iter()
            .filter_map(|s| {
                thread::Builder::new()
                    .stack_size(STACK)
                    .spawn_scoped(sc, move || {
                        let a = agent(None)?;
                        let (rtt, _) =
                            probe_once(&a, &fresh(&s.ping_url, 0), Duration::from_secs(1)).ok()?;
                        Some((ms(rtt), s))
                    })
                    .ok()
            })
            .collect();
        hs.into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .collect()
    });
    reach.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut tried: Vec<(f64, TestServer)> = Vec::new();
    for (_, server) in reach.into_iter().take(3) {
        if env.cancel.load(Ordering::Relaxed) || env.budget.remaining().is_zero() {
            break;
        }
        let out = stage(
            env,
            &server,
            &Spec {
                stage: None,
                url: &server.download_url,
                ping_url: &server.ping_url,
                conns: 2,
                time: lim.trial_zero.max(lim.trial_time),
                soft: Some(lim.trial_time),
                cap: lim.trial_max,
                total: &env.budget.total,
                proxy: None,
                probe: false,
                cpu: false,
                fail_fast: false,
                upload: false,
                pct: None,
            },
        );
        if out.bytes > 0 {
            tried.push((out.whole.unwrap_or(0.0), server));
        }
    }
    tried.sort_by(|a, b| b.0.total_cmp(&a.0));
    tried.into_iter().map(|(_, s)| s).collect()
}

/// A planned stage's cap, or `None` when the budget left cannot carry it.
fn share(cap: u64, left: &mut u64) -> Option<u64> {
    let c = cap.min(*left);
    *left -= c;
    (c > 0 && c >= cap / MIN_SHARE).then_some(c)
}
pub(super) fn run(server: &TestServer, env: &Env) -> Result<Outcome, Failure> {
    let lim = env.lim;
    let b = env.budget;
    if b.remaining().is_zero() || b.total.room() == 0 {
        return Err(exhausted());
    }
    // Cut order when the budget is short: upload, then single, the multi download last.
    let mut left = b.total.room();
    let Some(multi_cap) = share(lim.multi_max, &mut left) else {
        return Err(exhausted());
    };
    let single_cap = lim
        .single
        .then(|| share(lim.single_max, &mut left))
        .flatten();
    let upload_cap = lim
        .upload
        .then(|| share(lim.upload_max, &mut left))
        .flatten();
    let chill_run = env.chill.plan == chill::Plan::Run;
    let chill_cap = chill_run
        .then(|| b.chill.room())
        .filter(|c| *c > 0 && *c >= lim.chill_max / MIN_SHARE);
    // progress bar shares by planned time
    let parts: [(bool, Duration); 5] = [
        (lim.ping, lim.idle_time),
        (single_cap.is_some(), lim.single_time),
        (true, lim.multi_time),
        (chill_cap.is_some(), lim.multi_time),
        (upload_cap.is_some(), lim.upload_time),
    ];
    let planned: f64 = parts
        .iter()
        .filter(|p| p.0)
        .map(|p| p.1.as_secs_f64())
        .sum();
    let mut at = 0.0;
    let mut pct = |i: usize| -> (u8, u8) {
        let d = parts.get(i).map_or(0.0, |p| p.1.as_secs_f64());
        let from = (at / planned.max(0.001) * 100.0) as u8;
        at += d;
        let to = (at / planned.max(0.001) * 100.0).min(99.0) as u8;
        (from, to.saturating_sub(from))
    };
    let mut out = Outcome {
        multi: 0.0,
        single: Err("budget"),
        chill: Err(match env.chill.plan {
            chill::Plan::Off => "not_requested",
            chill::Plan::NotRunning => "chill_not_running",
            chill::Plan::Run => "budget",
        }),
        idle_ms: None,
        loaded_down: Loaded::default(),
        loaded_up: Loaded::default(),
        min_probes: lim.min_probes,
        cpu_peak: None,
        mihomo_cpu: None,
        sample_short: false,
    };
    let cancelled = || env.cancel.load(Ordering::Relaxed);
    let direct = agent(None).ok_or_else(|| transfer(server.provider))?;
    if lim.ping {
        let Some((ping, jitter)) = idle(env, server, &direct, pct(0))? else {
            return Ok(out);
        };
        let mut p = env.progress.lock().unwrap();
        p.ping_ms = Some(ping);
        p.jitter_ms = Some(jitter);
        p.idle_ms = Some(ping);
        out.idle_ms = Some(ping);
    }
    if cancelled() {
        return Ok(out);
    }
    let spec = |stage: Stage,
                phase: Phase,
                step: u8,
                conns: u8,
                time: Duration,
                cap: u64,
                pct: (u8, u8)| Spec {
        stage: Some((stage, phase, step)),
        url: &server.download_url,
        ping_url: &server.ping_url,
        conns,
        time,
        soft: None,
        cap,
        total: &b.total,
        proxy: None,
        probe: lim.ping,
        cpu: false,
        fail_fast: false,
        upload: false,
        pct: Some(pct),
    };
    if let Some(cap) = single_cap {
        let s = stage(
            env,
            server,
            &Spec {
                probe: false,
                ..spec(
                    Stage::SingleDownload,
                    Phase::Download,
                    2,
                    1,
                    lim.single_time,
                    cap,
                    pct(1),
                )
            },
        );
        if cancelled() {
            return Ok(out);
        }
        if s.bytes == 0 {
            return Err(s.first_err.unwrap_or_else(|| transfer(server.provider)));
        }
        let mut p = env.progress.lock().unwrap();
        p.single_mbps = s.steady.or(s.whole);
        p.single_whole_mbps = s.whole;
        out.single = p.single_mbps.ok_or("budget");
    } else {
        pct(1);
    }
    let m = stage(
        env,
        server,
        &Spec {
            cpu: true,
            ..spec(
                Stage::MultiDownload,
                Phase::Download,
                3,
                lim.connections,
                lim.multi_time,
                multi_cap,
                pct(2),
            )
        },
    );
    if cancelled() {
        return Ok(out);
    }
    if m.bytes == 0 {
        return Err(m.first_err.unwrap_or_else(|| transfer(server.provider)));
    }
    {
        let mut p = env.progress.lock().unwrap();
        // the headline: the stable part; a run too short for one shows the whole mean (R3)
        p.download_mbps = m.steady.or(m.whole);
        p.download_whole_mbps = m.whole;
        p.sample_short = m.steady_secs < lim.min_steady.as_secs_f64();
        p.cpu_peak_pct = m.cpu_peak.map(round2);
        if lim.ping {
            p.loaded_down_ms = m.loaded.median.map(round2);
        }
        out.multi = p.download_mbps.unwrap_or(0.0);
        out.sample_short = p.sample_short;
        out.loaded_down = m.loaded;
        out.cpu_peak = m.cpu_peak;
    }
    if let Some(cap) = chill_cap {
        out.chill = chill_leg(
            env,
            server,
            cap,
            &spec(
                Stage::ChillDownload,
                Phase::Download,
                4,
                lim.connections,
                lim.multi_time,
                cap,
                pct(3),
            ),
        );
        if cancelled() {
            return Ok(out);
        }
    } else {
        pct(3);
    }
    if let Some(cap) = upload_cap {
        let step = if chill_run { 5 } else { 4 };
        let u = stage(
            env,
            server,
            &Spec {
                url: &server.url,
                upload: true,
                ..spec(
                    Stage::MultiUpload,
                    Phase::Upload,
                    step,
                    lim.connections,
                    lim.upload_time,
                    cap,
                    pct(4),
                )
            },
        );
        if cancelled() {
            return Ok(out);
        }
        if u.bytes == 0 && u.errors > 0 {
            return Err(u.first_err.unwrap_or_else(|| transfer(server.provider)));
        }
        let mut p = env.progress.lock().unwrap();
        p.upload_mbps = u.steady.or(u.whole);
        p.upload_whole_mbps = u.whole;
        if lim.ping {
            p.loaded_up_ms = u.loaded.median.map(round2);
        }
        out.loaded_up = u.loaded;
    }
    Ok(out)
}
/// The CHILL download of the pair: the same node, right after the direct one. Any break is
/// `chill_stopped` (never 0 Mbps, never retried direct); a change of cell, exit or node
/// during it voids the comparison.
fn chill_leg(env: &Env, server: &TestServer, cap: u64, base: &Spec) -> Result<f64, &'static str> {
    let c = &env.chill;
    if !chill::running(c.proxy) {
        return Err("chill_stopped");
    }
    let exit_before = chill::exit(c.controller);
    if let Some(e) = &exit_before {
        env.progress.lock().unwrap().chill_region = Some(e.region.clone());
    }
    let cell_before = (c.context)().cell.or_else(|| c.cell.clone());
    let cpu_before = sys::mihomo();
    let s = stage(
        env,
        server,
        &Spec {
            cap,
            total: &env.budget.chill,
            proxy: Some(c.proxy),
            fail_fast: true,
            ..*base
        },
    );
    if env.cancel.load(Ordering::Relaxed) {
        return Err("chill_stopped");
    }
    let mihomo_cpu = cpu_before
        .zip(sys::mihomo())
        .and_then(|(a, b)| sys::cpu_pct(&a, &b))
        .map(round2);
    if s.errors > 0 || s.bytes == 0 {
        return Err("chill_stopped");
    }
    let rate = s.steady.or(s.whole).ok_or("chill_stopped")?;
    {
        let mut p = env.progress.lock().unwrap();
        p.chill_mbps = Some(rate);
        p.chill_whole_mbps = s.whole;
        p.mihomo_cpu_pct = mihomo_cpu;
        if env.lim.ping {
            p.loaded_chill_ms = s.loaded.median.map(round2);
        }
    }
    let cell_after = (c.context)().cell.or_else(|| c.cell.clone());
    if cell_before != cell_after {
        return Err("changed_cell");
    }
    let exit_after = chill::exit(c.controller);
    match (&exit_before, &exit_after) {
        (Some(a), Some(b)) if a.region != b.region => return Err("changed_exit"),
        (Some(a), Some(b)) if a != b => return Err("changed_node"),
        (Some(_), None) | (None, Some(_)) => return Err("changed_exit"),
        _ => {}
    }
    Ok(rate)
}
