//! u60d — U60 Pro (MU5250) core daemon. Phase 1: the wake ledger, read-only
//! (source/manager/docs/designs/u60-ledger.md).
//!
//!   u60d [--dir /data/luyaz/core/ledger] [--listen 127.0.0.1:9470] [--wan rmnet_data0]
//!   u60d --probe SECS      two reads SECS apart, print the partial minute as JSON, exit
//!   u60d --once [-i MS]    one read round of the device (moved from data-service), print the
//!                          snapshot as JSON, exit — the same output as data-service's `--once`
//!   u60d --evlog-stats N [-i MS]   N read rounds into an in-memory event log, print what it
//!                          wrote per block (the log never goes to /data before T7)
//!   u60d --ubus-compare OBJECT:METHOD ...   read-only: socket vs `ubus call`, see ubus/compare.rs
//!   u60d serve <arguments>   Phase 3: the core's server (serve.rs). When argv[0]'s file name
//!                          starts with `luyaz-core`, the whole command line is the server's.
//!
//! Ledger mode: one `sleep_until` in the whole process; the HTTP listener only wakes us when
//! someone connects. Reads only; the one thing it writes is its own directory.

mod crashwatch;
mod diag;
mod doctor;
mod device_profile;
mod envelope;
#[allow(dead_code)]
mod evlog;
mod ledger;
mod longterm;
mod nl;
mod o2;
mod paths;
mod peer_pid;
mod procfs;
mod record;
mod sample;
mod scorecard;
#[allow(dead_code)]
mod sens;

// Phase 3: HTTP and the write path moved from data-service (serve.rs)
mod at;
mod auth;
mod conn;
mod control;
mod ops;
mod serve;
mod svcwatch;
mod server;
mod v2;
mod versions;
mod watchdog;
mod wifi;

// Phase 4: the policy host and the policies moved from zte-agent (plugin.rs)
mod plugin;

// Phase 2: the read path moved from data-service (read.rs)
// Shadow mode uses only part of it; the rest (heartbeat, watchdog hooks, write scope, /v2/screen)
// comes into use with the comparator and Phase 3, so dead code is allowed here until then.
mod api;
#[allow(dead_code)]
mod block;
#[allow(dead_code)]
mod cell_window;
#[allow(dead_code)]
mod command;
#[allow(dead_code)]
mod contract;
mod engines;
#[allow(dead_code)]
mod executor;
#[allow(dead_code)]
mod model;
#[allow(dead_code)]
mod project;
#[allow(dead_code)]
mod qos;
mod read;
#[allow(dead_code)]
mod sms;
#[allow(dead_code)]
mod state;
#[allow(dead_code)]
mod ubus;
#[allow(dead_code)]
mod uci;

use record::*;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Instant, sleep_until};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const TICK_SCREEN_OFF: Duration = Duration::from_secs(60);
const TICK_SCREEN_ON: Duration = Duration::from_secs(5);
const RING: usize = 180;
const FLUSH_AT: usize = 64 * 1024;
const SEG_MAX: u64 = 4 << 20;
const KEEP_BYTES: u64 = 16 << 20;
const DAY_BUDGET: u64 = 2 << 20;
/// upper bound for one compact minute plus the names it may introduce
const MINUTE_EST: usize = 16 << 10;
/// netlink event socket buffers (u60-ledger.md §5)
const EVENT_BUF: i32 = 4 << 20;

struct Cfg {
    dir: String,
    listen: String,
    wan: String,
    probe: Option<u64>,
    once: bool,
    evlog_stats: Option<u32>,
    interval_ms: Option<u64>,
}

fn parse_args() -> Cfg {
    let mut c = Cfg {
        dir: "/data/luyaz/core/ledger".into(),
        listen: "127.0.0.1:9470".into(),
        wan: "rmnet_data0".into(),
        probe: None,
        once: false,
        evlog_stats: None,
        interval_ms: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut v = || it.next().unwrap_or_else(|| usage(&a));
        match a.as_str() {
            "--dir" => c.dir = v(),
            "--listen" => c.listen = v(),
            "--wan" => c.wan = v(),
            "--probe" => c.probe = Some(v().parse().unwrap_or_else(|_| usage("--probe"))),
            "--once" => c.once = true,
            "--evlog-stats" => {
                c.evlog_stats = Some(v().parse().unwrap_or_else(|_| usage("--evlog-stats")))
            }
            "-i" => c.interval_ms = Some(v().parse().unwrap_or_else(|_| usage("-i"))),
            // data-service's option; the read path keeps no files, so it is accepted and ignored
            "--data-dir" => drop(v()),
            _ => usage(&a),
        }
    }
    c
}

fn usage(a: &str) -> ! {
    eprintln!(
        "luyaz-core: bad argument {a}\nusage: luyaz-core [--dir D] [--listen ADDR] [--wan IFACE] | --probe SECS | --once [-i MS] | --evlog-stats N [-i MS] | --ubus-compare OBJ:METHOD..."
    );
    std::process::exit(2)
}

struct State {
    col: sample::Collector,
    ledger: ledger::Ledger,
    store: envelope::Store,
    ring: VecDeque<Minute>,
    diag: Option<diag::Window>,
    last_diag: Option<Diag>,
    day_start_ms: u64,
    day_bytes: u64,
    started_mono: u64,
}

impl State {
    /// Queues a record that uses no names (start, diag).
    fn push(&mut self, kind: u16, payload: &[u8]) {
        if let Err(e) = self.store.begin(envelope::HEADER + payload.len()) {
            eprintln!("luyaz-core: write: {e}");
        }
        let before = self.store.pending_len();
        self.store.push(SCHEMA, kind, payload);
        self.day_bytes += self.store.pending_len().saturating_sub(before) as u64;
    }

    /// Queues a minute in its compact form; names go into the segment table.
    fn push_minute(&mut self, m: &Minute) {
        if let Err(e) = self.store.begin(MINUTE_EST) {
            eprintln!("luyaz-core: write: {e}");
        }
        let c = MinuteC::pack(m, &mut self.store);
        let before = self.store.pending_len();
        self.store.push(SCHEMA, KIND_MINUTE, &to_cbor(&c));
        self.day_bytes += self.store.pending_len().saturating_sub(before) as u64;
    }

    /// One resident tick; returns the next interval.
    fn tick(&mut self) -> Duration {
        let s = self.col.snapshot();
        if s.mono_ms.saturating_sub(self.day_start_ms) >= 86_400_000 {
            self.day_start_ms = s.mono_ms;
            self.day_bytes = 0;
        }
        if let Some(mut m) = self.ledger.tick(&s) {
            if self.day_bytes > DAY_BUDGET {
                m.trimmed = true;
                m.procs.truncate(5);
                m.flows.truncate(3);
                m.forks.truncate(3);
                m.irqs.clear();
            }
            self.push_minute(&m);
            if self.ring.len() == RING {
                self.ring.pop_front();
            }
            self.ring.push_back(m);
            if self.store.pending_len() >= FLUSH_AT
                && let Err(e) = self.store.flush(false)
            {
                eprintln!("luyaz-core: write: {e}");
            }
        }
        if s.screen_on {
            TICK_SCREEN_ON
        } else {
            TICK_SCREEN_OFF
        }
    }

    fn diag_second(&mut self) {
        let mut x = self.col.second();
        let (local_addrs, wan_addrs) = sample::addrs(&self.col.wan);
        let ctx = ledger::Snapshot {
            wan_addrs,
            local_addrs,
            owners: std::mem::take(&mut x.owners),
            peers: std::mem::take(&mut x.peers),
            ..Default::default()
        };
        let Some(w) = &mut self.diag else { return };
        w.push(&diag::Second {
            wan_pkts: x.wan_pkts,
            flows: &x.flows,
            destroyed: &x.destroyed,
            power_uw: x.power_uw,
            screen_on: x.screen_on,
            self_ctxt: x.self_ctxt,
            ctx: &ctx,
        });
        if w.done() {
            self.end_diag("time");
        }
    }

    fn end_diag(&mut self, how: &str) {
        let Some(w) = self.diag.take() else { return };
        let mono = self.col.mono_ms();
        let d = w.result(ledger::meta(&self.col.boot, mono, wall()), how);
        let b = to_cbor(&d);
        self.push(KIND_DIAG, &b);
        if let Err(e) = self.store.flush(true) {
            eprintln!("luyaz-core: write: {e}");
        }
        self.last_diag = Some(d);
    }
}

impl Intern for envelope::Store {
    fn intern(&mut self, s: &str) -> u32 {
        envelope::Store::intern(self, s)
    }
}

fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn probe(cfg: &Cfg, secs: u64) {
    let mut col = sample::Collector::new(&cfg.wan, EVENT_BUF);
    let mut l = ledger::Ledger::new();
    let s0 = col.snapshot();
    l.tick(&s0);
    std::thread::sleep(Duration::from_secs(secs));
    let s1 = col.snapshot();
    l.tick(&s1);
    let out = serde_json::json!({
        "conntrack": col.ct.is_some(), "taskstats": col.ts.is_some(), "inet_diag": col.diag.is_some(),
        "procs": s1.procs.len(), "threads": s1.threads.len(), "flows": s1.flows.as_ref().map(|f| f.len()),
        "owners": s1.owners.len(), "wan_addrs": s1.wan_addrs.len(), "collect_ms": [s0.collect_ms, s1.collect_ms],
        "sock_stats": col.sock_stats,
        "sample_flows": s1.flows.iter().flatten().filter(|f| ledger::classify(f, &s1).0 == ledger::Class::Cellular).take(6)
            .map(|f| format!("{:?} {}:{} -> {}:{} | reply {}:{} -> {}:{} own_s={} own_d={}", f.orig.proto, f.orig.src, f.orig.sport, f.orig.dst, f.orig.dport,
                f.reply.src, f.reply.sport, f.reply.dst, f.reply.dport,
                s1.owners.contains_key(&(f.orig.proto, f.orig.sport)), s1.owners.contains_key(&(f.orig.proto, f.orig.dport)))).collect::<Vec<_>>(),
        "minute": l.partial().map(|m| m.redacted()),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

/// Read from the file (the install kit's versions.json), never by running the program.
#[used]
static VERSION_MARK: &str = concat!("LUYAZ_VERSION=", env!("CARGO_PKG_VERSION"), ";");

/// `<name> <version> (<commit>)`: the commit from LUYAZ_COMMIT (tools/luyaz ship passes it)
fn version_line(name: &str) -> String {
    match option_env!("LUYAZ_COMMIT").filter(|c| !c.is_empty()) {
        Some(c) => format!(
            "{name} {} ({})",
            env!("CARGO_PKG_VERSION"),
            &c[..c.len().min(7)]
        ),
        None => format!("{name} {}", env!("CARGO_PKG_VERSION")),
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).map(String::as_str) == Some("--version") {
        println!("{}", version_line("luyaz-core"));
        return;
    }
    // X11: `<core> doctor …` (any argv[0]) is a one-shot check: before the runtime and before
    // serve's first side effect (svcwatch, flock, pid file, crash markers, migration, core log)
    if raw.get(1).map(String::as_str) == Some("doctor") {
        std::process::exit(doctor::main(&raw[2..]));
    }
    // `<core> o2-check <iface> <source-ip>`: one read-only bound HTTPS run (O2)
    if raw.get(1).map(String::as_str) == Some("o2-check") {
        std::process::exit(o2::main(&raw[2..]));
    }
    // `<core> plan-reboot [why]`: mark the coming shell `reboot` as asked for (ledger S1)
    if raw.get(1).map(String::as_str) == Some("plan-reboot") {
        let why = raw.get(2).map_or("shell", String::as_str);
        match longterm::boot::plan(why) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("plan-reboot: {e}");
                std::process::exit(1);
            }
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("runtime");
    // serve mode (serve.rs): `luyaz-core serve ARGS` or argv[0] = luyaz-core*
    let core_argv = if raw.first().is_some_and(|a0| serve::invoked_as_core(a0)) {
        Some(raw.clone())
    } else if raw.get(1).map(String::as_str) == Some("serve") {
        Some(
            std::iter::once("luyaz-core".to_owned())
                .chain(raw[2..].iter().cloned())
                .collect(),
        )
    } else {
        None
    };
    if let Some(argv) = core_argv {
        if let Err(error) = rt.block_on(serve::main(argv)) {
            eprintln!("Error: {error:?}");
            std::process::exit(1);
        }
        return;
    }
    if raw.get(1).map(String::as_str) == Some("--scorecard-replay") {
        // T9a: one fixture left by doctor (DOC_REPORT_KEEP) through the Rust scorecard; prints
        // what doctor would and exits with its code (tools/scorecard-parity.sh compares them)
        let dir = raw.get(2).unwrap_or_else(|| usage("--scorecard-replay"));
        let (text, rc) = scorecard::replay::run(std::path::Path::new(dir));
        print!("{text}");
        std::process::exit(rc);
    }
    if raw.get(1).map(String::as_str) == Some("--longterm-once") {
        // T9c: one look, one power reading and the key.log backfill on the real device, printed;
        // writes only to the scratch dir (the side run before a ship)
        let dir = raw.get(2).unwrap_or_else(|| usage("--longterm-once"));
        print!("{}", longterm::once(std::path::Path::new(dir)));
        std::process::exit(0);
    }
    if raw.get(1).map(String::as_str) == Some("--scorecard-replay-core") {
        // T9c, the second leg: the same fixture's side inputs over the core's long-term layer
        let dir = raw.get(2).unwrap_or_else(|| usage("--scorecard-replay-core"));
        let (text, rc) = scorecard::replay::run_core(std::path::Path::new(dir));
        print!("{text}");
        std::process::exit(rc);
    }
    if raw.get(1).map(String::as_str) == Some("--scorecard-replay-stitch") {
        // T9d: the guard ledger before <cut> (wall clock), the core's long-term layer from it
        let dir = raw.get(2).unwrap_or_else(|| usage("--scorecard-replay-stitch"));
        let cut = raw
            .get(3)
            .and_then(|c| c.parse::<f64>().ok())
            .unwrap_or_else(|| usage("--scorecard-replay-stitch"));
        let (text, rc) = scorecard::replay::run_stitch(std::path::Path::new(dir), cut);
        print!("{text}");
        std::process::exit(rc);
    }
    if raw.get(1).map(String::as_str) == Some("--scorecard-core") {
        // T9a: the core's long-term layer (<dir>/hours.jsonl, events.jsonl) as a scorecard,
        // without doctor's side inputs (T9b supplies them)
        let dir = raw.get(2).unwrap_or_else(|| usage("--scorecard-core"));
        let (w, label) = match raw.get(3).map(String::as_str) {
            Some("24h") => (86_400.0, "24 小时"),
            _ => (604_800.0, "7 天"),
        };
        let inp = scorecard::Inputs {
            w,
            label: label.into(),
            ..Default::default()
        };
        let (text, rc) = scorecard::replay::core(std::path::Path::new(dir), &inp);
        print!("{text}");
        std::process::exit(rc);
    }
    if raw.get(1).map(String::as_str) == Some("--scorecard-live") {
        // T9b: what /v2/scorecard answers (the server runs this as its child): 24h|7d [--summary]
        // [--body F|-] (doctor's request body); prints the card and the rc= line
        let w = match raw.get(2).map(String::as_str) {
            Some("24h") => 86_400.0,
            Some("7d") => 604_800.0,
            _ => usage("--scorecard-live"),
        };
        let summary = raw.iter().any(|a| a == "--summary");
        let body = raw
            .iter()
            .position(|a| a == "--body")
            .and_then(|i| raw.get(i + 1))
            .map(|f| {
                if f == "-" {
                    std::io::read_to_string(std::io::stdin()).unwrap_or_default()
                } else {
                    std::fs::read_to_string(f).unwrap_or_default()
                }
            });
        let p = scorecard::live::Paths::host();
        print!("{}", scorecard::live::card(&p, w, summary, body.as_deref()));
        std::process::exit(0);
    }
    if raw.get(1).map(String::as_str) == Some("--scorecard-inputs") {
        // T9b: the side inputs as /v2/scorecard reads them, written as doctor's DOC_REPORT_KEEP
        // leaves them (cap, alerts, THR and GAVEUP in vars) so the two can be diffed;
        // [crashcap=D] [alerts=F] [uid=D] [base=F] override the device paths
        let out = raw.get(2).unwrap_or_else(|| usage("--scorecard-inputs"));
        let mut p = scorecard::live::Paths::host();
        for a in &raw[3..] {
            let (k, v) = a
                .split_once('=')
                .unwrap_or_else(|| usage("--scorecard-inputs"));
            let v = std::path::PathBuf::from(v);
            match k {
                "crashcap" => p.crashcap = v,
                "alerts" => p.alerts_queue = v,
                "uid" => p.uid_state = v,
                "base" => p.standby_base = v,
                _ => usage("--scorecard-inputs"),
            }
        }
        let inp = scorecard::live::inputs(&p, 86_400.0, false);
        let out = std::path::Path::new(out);
        let _ = std::fs::create_dir_all(out);
        let w = |n: &str, t: &str| {
            std::fs::write(out.join(n), t).unwrap_or_else(|e| {
                eprintln!("{n}: {e}");
                std::process::exit(2)
            })
        };
        w("cap", &inp.cap);
        if p.alerts_queue.exists() {
            w("alerts", &inp.alerts);
        }
        w(
            "vars",
            &format!("GAVEUP={}\nTHR={}\n", u8::from(inp.gaveup), inp.thr),
        );
        std::process::exit(0);
    }
    if raw.get(1).map(String::as_str) == Some("--crashwatch-replay") {
        // T10: a device timeline through the crash watcher; events as JSON lines on stdout
        // (tools/crashwatch-parity.sh compared them with the guard's watcher until Phase 10, X2)
        let path = raw.get(2).unwrap_or_else(|| usage("--crashwatch-replay"));
        let boot = raw
            .get(3)
            .map_or("feedc0de-0000-4000-8000-000000000000", String::as_str);
        let first_live = raw.iter().any(|a| a == "--first-live");
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("{path}: {e}");
            std::process::exit(2)
        });
        let items = crashwatch::replay::parse(&text).unwrap_or_else(|e| {
            eprintln!("{path}: {e}");
            std::process::exit(2)
        });
        let o = crashwatch::replay::run(&items, boot, first_live);
        for e in &o.events {
            println!("{e}");
        }
        eprintln!("wakes {} (timer {})", o.wakes, o.timer_wakes);
        return;
    }
    if raw.get(1).map(String::as_str) == Some("--ubus-compare") {
        std::process::exit(rt.block_on(ubus::compare::run(&raw[2..])));
    }
    let cfg = parse_args();
    if let Some(secs) = cfg.probe {
        probe(&cfg, secs);
        return;
    }
    // same clamp as data-service's -i
    let interval =
        |default: u64| Duration::from_millis(cfg.interval_ms.unwrap_or(default).clamp(500, 5000));
    if let Some(n) = cfg.evlog_stats {
        let interval = interval(1000);
        println!("{}", rt.block_on(read::evlog_stats(interval, n)));
        return;
    }
    if cfg.once {
        let interval = interval(1000);
        let snap = rt.block_on(read::once(interval));
        println!("{}", serde_json::to_string(&snap).expect("snapshot json"));
        return;
    }
    rt.block_on(serve(cfg));
}

async fn serve(cfg: Cfg) {
    let store = envelope::Store::open(std::path::Path::new(&cfg.dir), SEG_MAX, KEEP_BYTES)
        .unwrap_or_else(|e| {
            eprintln!("luyaz-core: cannot open {}: {e}", cfg.dir);
            std::process::exit(1)
        });
    let col = sample::Collector::new(&cfg.wan, EVENT_BUF);
    let mono = col.mono_ms();
    let mut st = State {
        ledger: ledger::Ledger::new(),
        store,
        ring: VecDeque::with_capacity(RING),
        diag: None,
        last_diag: None,
        day_start_ms: mono,
        day_bytes: 0,
        started_mono: mono,
        col,
    };
    let start = Start {
        meta: ledger::meta(&st.col.boot, mono, wall()),
        version: VERSION.into(),
        conntrack: st.col.ct.is_some(),
        taskstats: st.col.ts.is_some(),
        inet_diag: st.col.diag.is_some(),
    };
    if let Ok((groups, _)) = st.store.read_all() {
        st.last_diag = groups
            .iter()
            .flatten()
            .rev()
            .find(|r| r.kind == KIND_DIAG)
            .and_then(|r| from_cbor(&r.payload));
    }
    st.push(KIND_START, &to_cbor(&start));
    let _ = st.store.flush(true);

    let listener = TcpListener::bind(&cfg.listen).await.unwrap_or_else(|e| {
        eprintln!("luyaz-core: cannot listen on {}: {e}", cfg.listen);
        std::process::exit(1)
    });
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("signal");
    let mut int = signal(SignalKind::interrupt()).expect("signal");

    let mut next_tick = Instant::now();
    let mut next_sec: Option<Instant> = None;
    loop {
        let deadline = next_sec.map_or(next_tick, |s| s.min(next_tick));
        tokio::select! {
            _ = sleep_until(deadline) => {
                let now = Instant::now();
                if let Some(s) = next_sec
                    && now >= s {
                        st.diag_second();
                        next_sec = st.diag.as_ref().map(|_| s + Duration::from_secs(1));
                    }
                if now >= next_tick {
                    next_tick = now + st.tick();
                }
            }
            r = listener.accept() => {
                if let Ok((sock, _)) = r {
                    let had = st.diag.is_some();
                    let _ = tokio::time::timeout(Duration::from_secs(3), handle(&mut st, sock)).await;
                    if !had && st.diag.is_some() {
                        next_sec = Some(Instant::now());
                    } else if st.diag.is_none() {
                        next_sec = None;
                    }
                }
            }
            _ = term.recv() => break,
            _ = int.recv() => break,
        }
    }
    st.end_diag("restarted");
    let _ = st.store.flush(true);
}

// ── HTTP (localhost only; one request per connection) ───────────────────────

async fn handle(st: &mut State, mut sock: TcpStream) {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.split_ascii_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let q = Query(
        query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    let full = q.get("full") == Some("1");
    let (code, body) = route(st, method, path, &q, full);
    let resp = format!(
        "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.write_all(body.as_bytes()).await;
    let _ = sock.shutdown().await;
}

struct Query(Vec<(String, String)>);

impl Query {
    fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
}

fn route(
    st: &mut State,
    method: &str,
    path: &str,
    q: &Query,
    full: bool,
) -> (&'static str, String) {
    use serde_json::json;
    let view = |m: &Minute| if full { m.clone() } else { m.redacted() };
    let dview = |d: &Diag| if full { d.clone() } else { d.redacted() };
    let ok = |v: serde_json::Value| ("200 OK", v.to_string());
    match (method, path) {
        ("GET", "/healthz") => ok(json!({
            "ok": true, "version": VERSION,
            "up_s": st.col.mono_ms().saturating_sub(st.started_mono) / 1000,
            "conntrack": st.col.ct.is_some(), "taskstats": st.col.ts.is_some(), "inet_diag": st.col.diag.is_some(),
            "minutes": st.ring.len(), "written": st.store.written, "pending": st.store.pending_len(),
            "day_bytes": st.day_bytes, "self": st.ring.back().map(|m| m.self_cost.clone()),
            "diag": st.diag.as_ref().map(|w| json!({"elapsed": w.elapsed(), "target": w.target_secs})),
        })),
        ("GET", "/ledger/now") => ok(json!({
            "partial": st.ledger.partial().map(|m| view(&m)),
            "last": st.ring.back().map(view),
        })),
        ("GET", "/ledger") => {
            let n: usize = q
                .get("last")
                .and_then(|v| v.parse().ok())
                .unwrap_or(60)
                .min(10_080);
            match st.store.read_all() {
                Ok((groups, dropped)) => {
                    let mut ms: Vec<Minute> = Vec::new();
                    for g in &groups {
                        let names = envelope::segment_names(g);
                        ms.extend(
                            g.iter()
                                .filter(|r| r.kind == KIND_MINUTE)
                                .filter_map(|r| from_cbor::<MinuteC>(&r.payload)?.unpack(&names)),
                        );
                    }
                    let skip = ms.len().saturating_sub(n);
                    let ms: Vec<Minute> = ms.drain(skip..).map(|m| view(&m)).collect();
                    ok(json!({"minutes": ms, "dropped_bytes": dropped}))
                }
                Err(e) => (
                    "500 Internal Server Error",
                    json!({"error": e.to_string()}).to_string(),
                ),
            }
        }
        ("POST", "/diag") => {
            if st.diag.is_some() {
                return (
                    "409 Conflict",
                    json!({"error": "a window is running"}).to_string(),
                );
            }
            let secs: u32 = q.get("secs").and_then(|v| v.parse().ok()).unwrap_or(1200);
            let w = diag::Window::new(&st.col.wan, secs);
            let target = w.target_secs;
            st.diag = Some(w);
            ok(json!({"started": true, "secs": target}))
        }
        ("POST", "/diag/stop") => {
            st.end_diag("stopped");
            ok(json!({"stopped": true, "result": st.last_diag.as_ref().map(dview)}))
        }
        ("GET", "/diag") => ok(json!({
            "active": st.diag.as_ref().map(|w| json!({"elapsed": w.elapsed(), "target": w.target_secs})),
            "last": st.last_diag.as_ref().map(dview),
        })),
        _ => ("404 Not Found", json!({"error": "not found"}).to_string()),
    }
}
