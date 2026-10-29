//! The regress layer's IO, on the long-term thread: read what the minute check already has
//! (plus the alert queue when it changed and the core's own blocks), run [`Regress`], save
//! `incidents.json` (fsynced) before queueing a transition, queue it, keep evidence, and hand
//! the readers ([`view`]) a copy.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::{At, File, Regress, Said, VERSION, base, detect, ship};
use crate::longterm::Clock;

/// Evidence kept (CEO review X5): 8 KB each, 30 days, 50 at most; memory snapshots (X2) 4 KB,
/// 10 at most.
pub const EVIDENCE_MAX: usize = 8 * 1024;
pub const EVIDENCE_KEEP: usize = 50;
pub const EVIDENCE_DAYS: i64 = 30;
pub const MEMSNAP_MAX: usize = 4 * 1024;
pub const MEMSNAP_KEEP: usize = 10;
/// The baseline is recomputed this often (and at start).
const BASE_EVERY_S: i64 = 86_400;
/// Blind runs moving alone are saved at most this often.
const BLIND_SAVE_S: i64 = 3600;

static HUB: OnceLock<Arc<crate::block::Hub>> = OnceLock::new();

/// The core's hub, for the sim / signal / interfaces / op blocks (the cellular detector).
pub fn set_hub(hub: Arc<crate::block::Hub>) {
    let _ = HUB.set(hub);
}

/// What readers get, rebuilt after every check.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub file: File,
    pub list: Value,
    pub block: Value,
    pub long: PathBuf,
    pub ready: bool,
}

fn view_cell() -> &'static Mutex<Arc<View>> {
    static V: OnceLock<Mutex<Arc<View>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(Arc::new(View::default())))
}

pub fn view() -> Arc<View> {
    view_cell()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn publish(v: View) {
    *view_cell().lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(v);
}

/// The `regress` block now (`null` until the layer ran once: the block stays unread, then
/// stale, and the screen says it has no incident data).
pub fn block_now() -> Option<Value> {
    let v = view();
    v.ready.then(|| v.block.clone())
}

pub struct Paths {
    pub long: PathBuf,
    pub alerts: PathBuf,
    pub manifest: PathBuf,
    pub txn: PathBuf,
    pub route: PathBuf,
    pub proc_self: PathBuf,
    /// The decision loop's real-device test switch: a drill while it is on.
    pub rx_quiet: PathBuf,
}

impl Paths {
    pub fn device(long: &Path) -> Paths {
        let h = |p: &str| PathBuf::from(crate::state::host_path(p));
        Paths {
            long: long.to_path_buf(),
            alerts: h(luyaz_api::alerts::DIR),
            manifest: h(ship::MANIFEST),
            txn: h(ship::TXN),
            route: h("/proc/net/route"),
            proc_self: h("/proc/self"),
            rx_quiet: h(crate::api::finding::sampling::RX_QUIET_FILE),
        }
    }
}

fn write_synced(p: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = p.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, p)
}

/// `incidents.json` as saved; another version or a broken file is renamed aside
/// (`incidents.json.v<N>.bak`) and the layer starts over.
pub fn load_file(long: &Path) -> Option<File> {
    let p = long.join(super::FILE);
    let text = std::fs::read_to_string(&p).ok()?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let n = v["v"].as_u64().unwrap_or(0);
    if n == u64::from(VERSION)
        && let Ok(f) = serde_json::from_value::<File>(v)
    {
        return Some(f);
    }
    let aside = long.join(format!("{}.v{n}.bak", super::FILE));
    eprintln!(
        "[regress] {} is version {n} or broken: moved to {}",
        p.display(),
        aside.display()
    );
    let _ = std::fs::rename(&p, &aside);
    None
}

pub fn load_base(long: &Path) -> base::Base {
    std::fs::read_to_string(long.join(base::FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The 7 days of hour lines a baseline reads (only `hour_proc` and `hour_power` are parsed).
pub fn hour_lines(long: &Path, since: i64) -> Vec<Value> {
    use std::io::BufRead;
    let Ok(f) = std::fs::File::open(long.join("hours.jsonl")) else {
        return Vec::new();
    };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter(|l| l.contains("\"k\":\"hour_proc\"") || l.contains("\"k\":\"hour_power\""))
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .filter(|v| v["tt"].as_i64().is_some_and(|t| t >= since))
        .collect()
}

pub struct Engine {
    pub r: Regress,
    p: Paths,
    boot: String,
    started_up: i64,
    queue_sig: Option<(u64, std::time::SystemTime)>,
    base_at: Option<i64>,
    blind_saved: i64,
    fw: Option<String>,
    limits_at: Option<std::time::SystemTime>,
    /// This boot's reboot code and plan mark (the evidence names them).
    boot_facts: Option<(Option<i64>, Option<String>)>,
    drill_on: bool,
    /// Open incidents whose evidence was written again once the clock was trusted.
    rewritten: std::collections::HashSet<String>,
}

fn queue_sig(p: &Path) -> Option<(u64, std::time::SystemTime)> {
    let m = std::fs::metadata(p.join("queue")).ok()?;
    Some((m.len(), m.modified().ok()?))
}

impl Engine {
    pub fn start(p: Paths, build: &str, c: &Clock) -> Engine {
        let a = luyaz_api::alerts::Alerts::at(&p.alerts);
        let head = a
            .events()
            .last()
            .map_or(0, |e| e.seq)
            .max(a.read_u64("seq"));
        let now = At::of(c);
        let r = Regress::load(
            load_file(&p.long),
            load_base(&p.long),
            build,
            &now,
            head,
            &c.boot,
        );
        let mut e = Engine {
            r,
            boot: c.boot.clone(),
            started_up: c.boottime,
            queue_sig: None,
            base_at: None,
            blind_saved: c.boottime,
            fw: None,
            limits_at: None,
            boot_facts: None,
            drill_on: false,
            rewritten: Default::default(),
            p,
        };
        // evidence written by an older core only when they opened: once more, with the end
        let old: Vec<String> =
            e.r.file
                .closed
                .iter()
                .filter(|i| i.evidence)
                .map(|i| i.id.clone())
                .collect();
        e.r.ended.extend(old);
        e.flush(&now);
        e.publish(&now);
        e
    }

    /// The boot facts arrived (`boot::gather`).
    pub fn boot(&mut self, c: &Clock, facts: &Value) {
        self.fw = facts["fw"].as_str().map(str::to_string);
        self.boot_facts = Some((
            facts["code"].as_i64(),
            facts["planned"].as_str().map(str::to_string),
        ));
        let now = At::of(c);
        let rows = self.ship_rows();
        let sh = ship::find(&rows, &now, now.up, luyaz_api::alerts::current_offset());
        self.r.boot(facts, &now, &self.boot, sh);
        self.after(&now);
    }

    /// One minute check, with the power reading the fold just took and the hour lines it wrote.
    pub fn check(&mut self, c: &Clock, read: &crate::longterm::power::Read, hours: &[Value]) {
        let now = At::of(c);
        #[allow(unused_mut)]
        let mut k = detect::Check {
            at: now.clone(),
            core_age: c.boottime - self.started_up,
            heap_kb: read.mem.map(|m| m.1),
            anon_kb: read.mem.and_then(|m| m.0),
            cell: self.cell(),
            chill: chill_state(
                read.chill_on,
                read.chill_ok,
                crate::engines::chill_probe::now(),
            ),
            ts: if read.ts_on { read.ts_ok } else { None },
            observed: observed_net(),
            ..Default::default()
        };
        k.heat = crate::heat::critical();
        let lm = std::fs::metadata(self.p.long.join(LIMITS))
            .and_then(|m| m.modified())
            .ok();
        if lm != self.limits_at {
            self.limits_at = lm;
            self.r.base.limits = read_limits(&self.p.long);
        }
        let drill = crate::api::finding::sampling::rx_quiet_at(&self.p.rx_quiet);
        if drill && !self.drill_on {
            self.r.drill("rx-quiet", &now);
        }
        self.drill_on = drill;
        self.r.check(&k);
        for h in hours {
            self.r.hour(h, &now);
        }
        let sig = queue_sig(&self.p.alerts);
        if sig != self.queue_sig {
            self.queue_sig = sig;
            let events = luyaz_api::alerts::Alerts::at(&self.p.alerts).events();
            self.r.ingest(&events, &now);
        }
        if now.ok && self.base_at.is_none_or(|t| now.up - t >= BASE_EVERY_S) {
            self.base_at = Some(now.up);
            self.rebase(&now);
        }
        self.after(&now);
    }

    fn after(&mut self, now: &At) {
        self.fresh(now);
        if self.r.det.blind_moved && now.up - self.blind_saved >= BLIND_SAVE_S {
            self.r.dirty = true;
        }
        self.flush(now);
        for l in self.r.log.drain(..) {
            eprintln!("{l}");
        }
        self.publish(now);
    }

    fn rebase(&mut self, now: &At) {
        let lines = hour_lines(&self.p.long, now.wall - base::WINDOW_S - 3600);
        let spans = |fam: &str| -> Vec<(i64, i64)> {
            self.r
                .file
                .open
                .iter()
                .chain(&self.r.file.closed)
                .filter(|i| i.kind.starts_with(fam))
                .filter_map(|i| {
                    let a = i.from.wall_ok()?;
                    let b = i.to.as_ref().map_or(Some(now.wall), At::wall_ok)?;
                    Some((a, b))
                })
                .collect()
        };
        let (mem_ex, power_ex) = (spans("regress-mem"), spans("regress-power"));
        let before = self.r.base.clone();
        if let Some(fw) = &self.fw {
            let home = HUB
                .get()
                .and_then(|h| h.view("sim"))
                .filter(|v| !v.stale)
                .and_then(|v| crate::crashwatch::Net::from_blocks(None, Some(&v.data), None).home)
                .unwrap_or_default();
            self.r.base.power_under(&format!("{fw}|{home}"));
        }
        self.r
            .base
            .recompute(lines.iter(), now.wall, &mem_ex, &power_ex);
        let known = self.r.base.power_known != Some(false);
        self.r.blind("power", known, now);
        if self.r.base != before {
            match serde_json::to_string(&self.r.base) {
                Ok(t) => {
                    if let Err(e) = write_synced(&self.p.long.join(base::FILE), &t) {
                        eprintln!("[regress] baseline: {e}");
                    }
                }
                Err(e) => eprintln!("[regress] baseline: {e}"),
            }
        }
    }

    /// The cellular detector's reading from the core's blocks and the route table (eng review
    /// R11, CEO R3-7): None when it cannot be judged.
    fn cell(&self) -> Option<detect::Cell> {
        let hub = HUB.get()?;
        let get = |n: &str| hub.view(n).filter(|v| !v.stale).map(|v| v.data);
        let sim = get("sim")?;
        let signal = get("signal")?;
        let ifs = get("interfaces")?;
        let cell = &ifs["cellular"];
        let roaming = signal["roaming"]
            .as_str()
            .is_some_and(|r| !r.is_empty() && r != "Home");
        let ready = sim["state"] == "sim ready"
            && sim["phase"] == "online"
            && cell["enable"].as_i64() == Some(1)
            && (!roaming || cell["roam_enable"].as_i64() == Some(1));
        let busy = get("op").is_some_and(|o| !o["active"].is_null());
        if !ready || busy {
            return None;
        }
        let t = signal["type"].as_str().unwrap_or("").trim();
        if t.is_empty() {
            return None;
        }
        let route = std::fs::read_to_string(&self.p.route)
            .is_ok_and(|t| crate::crashwatch::io::route_up(&t, "rmnet_data0"));
        Some(detect::Cell {
            registered: crate::ops::registered(t),
            route_up: route,
        })
    }

    fn ship_rows(&self) -> Vec<ship::Row> {
        ship::rows(
            &std::fs::read_to_string(&self.p.manifest).unwrap_or_default(),
            &std::fs::read_to_string(&self.p.txn).unwrap_or_default(),
        )
    }

    /// New incidents: the ship and drill they may relate to, their evidence, a memory snapshot.
    /// Ended ones: their evidence again (the end and the recovery time). Open ones that began
    /// before the clock was trusted: again once it is (the hours before them).
    fn fresh(&mut self, now: &At) {
        let ids = std::mem::take(&mut self.r.fresh);
        let ended = std::mem::take(&mut self.r.ended);
        let late: Vec<String> = if now.ok {
            self.r
                .file
                .open
                .iter()
                .filter(|i| {
                    !i.from.ok && i.from.boot == now.boot && !self.rewritten.contains(&i.id)
                })
                .map(|i| i.id.clone())
                .collect()
        } else {
            Vec::new()
        };
        if ids.is_empty() && ended.is_empty() && late.is_empty() {
            return;
        }
        let rows = if ids.is_empty() {
            Vec::new()
        } else {
            self.ship_rows()
        };
        let offset = luyaz_api::alerts::current_offset();
        let this_boot = Facts {
            boot_code: self.boot_facts.as_ref().and_then(|f| f.0),
            planned: self.boot_facts.as_ref().and_then(|f| f.1.clone()),
        };
        for id in &ids {
            let drill = self
                .r
                .file
                .open
                .iter()
                .chain(&self.r.file.closed)
                .find(|i| &i.id == id)
                .and_then(|i| self.r.drill_for(&i.from));
            let Some(i) = self
                .r
                .file
                .open
                .iter_mut()
                .chain(self.r.file.closed.iter_mut())
                .find(|i| &i.id == id)
            else {
                continue;
            };
            if i.ship_clock.is_none()
                && let Some((s, clock)) = ship::find(&rows, &i.from, now.up, offset)
            {
                i.ship = Some(s);
                i.ship_clock = Some(clock);
            }
            if i.drill.is_none() {
                i.drill = drill;
            }
            // the snapshot is of the moment it opened: one taken after the end would mislead
            if i.kind.starts_with("regress-mem")
                && i.to.is_none()
                && let Err(e) = memsnap(&self.p.long, &self.p.proc_self, &i.id)
            {
                eprintln!("[regress] memsnap {}: {e}", i.id);
            }
            self.r.dirty = true;
        }
        for id in ids.iter().chain(&ended).chain(&late) {
            let Some(i) = self
                .r
                .file
                .open
                .iter_mut()
                .chain(self.r.file.closed.iter_mut())
                .find(|i| &i.id == id)
            else {
                continue;
            };
            let path = self.p.long.join(format!("incident-{}.json", i.id));
            // the reboot code is the boot it began in: kept from the first writing after that
            let facts = if i.from.boot == now.boot {
                this_boot.clone()
            } else {
                let was: Value = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(Value::Null);
                Facts {
                    boot_code: was["boot_code"].as_i64(),
                    planned: was["planned"].as_str().map(str::to_string),
                }
            };
            let ev = evidence(i, &self.p.long, now, &facts);
            match write_synced(&path, &ev) {
                Ok(()) => {
                    if !i.evidence {
                        i.evidence = true;
                        self.r.dirty = true;
                    }
                }
                Err(e) => eprintln!("[regress] evidence {}: {e}", i.id),
            }
        }
        self.rewritten.extend(late);
        prune_files(
            &self.p.long,
            "incident-",
            ".json",
            EVIDENCE_KEEP,
            Some(EVIDENCE_DAYS),
        );
        prune_files(&self.p.long, "memsnap-", ".txt", MEMSNAP_KEEP, None);
    }

    /// Save (the transitions still pending in it), queue the transitions, save again.
    fn flush(&mut self, now: &At) {
        if !self.r.dirty {
            return;
        }
        if self.save(now.up).is_err() {
            return;
        }
        let pending = self.r.pending();
        if pending.is_empty() {
            return;
        }
        let a = luyaz_api::alerts::Alerts::at(&self.p.alerts);
        for t in pending {
            match a.add_once(&t.id, &t.kind, &t.text, now.wall, now.up.max(0) as u64) {
                Ok((seq, _)) => self.r.queued(&t.id, seq),
                Err(e) => eprintln!("[regress] queue {}: {e}", t.id),
            }
        }
        let _ = self.save(now.up);
        // the queue moved: read it back next check
        self.queue_sig = None;
    }

    fn save(&mut self, up: i64) -> std::io::Result<()> {
        let text = serde_json::to_string(&self.r.file).map_err(std::io::Error::other)?;
        match write_synced(&self.p.long.join(super::FILE), &text) {
            Ok(()) => {
                self.r.dirty = false;
                self.r.det.blind_moved = false;
                self.blind_saved = up;
                Ok(())
            }
            Err(e) => {
                eprintln!("[regress] save: {e}");
                Err(e)
            }
        }
    }

    fn publish(&self, now: &At) {
        publish(View {
            file: self.r.file.clone(),
            list: super::list(&self.r, now),
            block: super::block(&self.r, now),
            long: self.p.long.clone(),
            ready: true,
        });
    }
}

/// The owner's commands over the core's control route (design §4). `limit` is not one: only the
/// device's command line writes [`LIMITS`] (CEO review S3), and the layer reads it back.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Accept { kind: String, source: String },
}

/// The owner's absolute bars (`<core> regress limit …`), read back when it changes.
pub const LIMITS: &str = "regress-limits.json";

/// The limits file as written; missing = the starting values.
pub fn read_limits(long: &Path) -> base::Limits {
    std::fs::read_to_string(long.join(LIMITS))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// `<core> regress limit mem <heap MiB> <anon MiB>` / `limit power <mW>`: write the bars and an
/// event saying so (source `cli`). The running core reads them at its next check.
pub fn set_limits(
    long: &Path,
    mem: Option<(f64, f64)>,
    power: Option<f64>,
) -> Result<String, String> {
    let mut l = read_limits(long);
    let mut what = json!({"what": "limit", "source": "cli"});
    if let Some((h, a)) = mem {
        if !(1.0..=4096.0).contains(&h) || !(1.0..=4096.0).contains(&a) {
            return Err("memory limits are MiB between 1 and 4096".into());
        }
        l.heap_mib = Some(h);
        l.anon_mib = Some(a);
        what["heap_mib"] = json!(h);
        what["anon_mib"] = json!(a);
    }
    if let Some(mw) = power {
        if !(100.0..=10_000.0).contains(&mw) {
            return Err("the power limit is mW between 100 and 10000".into());
        }
        l.power_mw = Some(mw);
        what["power_mw"] = json!(mw);
    }
    let t = serde_json::to_string(&l).map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(long);
    write_synced(&long.join(LIMITS), &t).map_err(|e| format!("{}: {e}", LIMITS))?;
    let c = crate::longterm::clock_now(&crate::evlog::boot_id());
    let now = At::of(&c);
    let mut ev = json!({"t": now.wall_ok(), "raw_wall": now.wall, "up": now.up, "boot": now.boot, "k": "regress"});
    if let (Some(o), Some(w)) = (ev.as_object_mut(), what.as_object()) {
        for (k, v) in w {
            o.insert(k.clone(), v.clone());
        }
    }
    crate::longterm::Store::at(long)
        .append_events(&[ev])
        .map_err(|e| format!("event: {e}"))?;
    Ok(format!(
        "limits: heap {} MiB, anon {} MiB, standby power {} mW (the core reads them within a minute)",
        l.heap_mib.unwrap_or(base::HEAP_LIMIT_MIB),
        l.anon_mib.unwrap_or(base::ANON_LIMIT_MIB),
        l.power_mw.unwrap_or(base::POWER_LIMIT_MW)
    ))
}

/// Hand a command to the long-term thread and wait for its answer.
pub fn request(cmd: Command) -> Result<String, String> {
    if !crate::longterm::enabled() {
        return Err("the long-term layer is off (LUYAZ_LONGTERM)".into());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    crate::longterm::send(crate::longterm::Msg::Regress(cmd, tx));
    rx.recv_timeout(std::time::Duration::from_secs(20))
        .map_err(|_| "the long-term thread did not answer".to_string())?
}

impl Engine {
    pub fn command(&mut self, c: &Clock, cmd: Command) -> Result<String, String> {
        let now = At::of(c);
        let (said, what, source) = match &cmd {
            Command::Accept { kind, source } => {
                let lines = hour_lines(&self.p.long, now.wall - 4 * 86_400);
                let said = self.r.accept(kind, lines.iter(), &now)?;
                (
                    said,
                    json!({"what": "accept", "kind": kind}),
                    source.clone(),
                )
            }
        };
        let t = serde_json::to_string(&self.r.base).map_err(|e| e.to_string())?;
        write_synced(&self.p.long.join(base::FILE), &t).map_err(|e| format!("baseline: {e}"))?;
        // an owner's command is on record with where it came from
        let mut ev = json!({"t": now.wall_ok(), "raw_wall": now.wall, "up": now.up, "boot": now.boot,
                            "k": "regress", "source": source});
        if let (Some(o), Some(w)) = (ev.as_object_mut(), what.as_object()) {
            for (k, v) in w {
                o.insert(k.clone(), v.clone());
            }
        }
        if let Err(e) = crate::longterm::Store::at(&self.p.long).append_events(&[ev]) {
            eprintln!("[regress] event: {e}");
        }
        self.r.log.push(format!("[regress] {said} (from {source})"));
        self.after(&now);
        Ok(said)
    }
}

/// CHILL as the exits detector reads it (eng review R14): not meant on → cannot tell; meant on
/// and its process not running → failing; running → what the forwarding probe last said (no
/// verdict yet → cannot tell).
pub fn chill_state(meant: bool, running: bool, probe: Option<bool>) -> Option<bool> {
    if !meant {
        return None;
    }
    if !running {
        return Some(false);
    }
    probe
}

/// What else is seen now, for cellular and exit incidents (CEO review X3r): the home screens'
/// finding and the no-service ledger's stock recovery.
fn observed_net() -> Option<Said> {
    let mut zh = Vec::new();
    let mut en = Vec::new();
    if let Some(f) = crate::api::finding::home_now() {
        zh.push(f.symptom.clone());
        en.push(f.symptom_en.clone());
    }
    let r = crate::stock_anomaly::report();
    if let Some(seen) = r["no_service"]["open"]["seen"].as_array()
        && !seen.is_empty()
    {
        let s: Vec<&str> = seen.iter().filter_map(Value::as_str).collect();
        zh.push(format!("原厂动作：{}", s.join("、")));
        en.push(format!("stock firmware: {}", s.join(", ")));
    }
    (!zh.is_empty()).then(|| Said {
        zh: zh.join("；"),
        en: en.join("; "),
    })
}

/// What the evidence names beside the incident: this boot's reboot code and plan mark.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub boot_code: Option<i64>,
    pub planned: Option<String>,
}

/// When an incident began by the wall clock: as it was, or (begun before the clock was trusted)
/// counted back from a trusted clock now in the same boot.
pub fn start_wall(from: &At, now: &At) -> Option<i64> {
    from.wall_ok()
        .or_else(|| (now.ok && from.boot == now.boot).then(|| now.wall - (now.up - from.up)))
}

/// An incident's evidence (CEO review X5; design §4): what it is, how it ended and how long it
/// took, this boot's reboot code, the ship and drill before it, and the 6 hours before it from
/// `hours.jsonl` in short (memory, the exits, standby power), cut from the oldest hour to
/// [`EVIDENCE_MAX`]. Written when it opens and again when it ends.
pub fn evidence(i: &super::Incident, long: &Path, now: &At, f: &Facts) -> String {
    let start = start_wall(&i.from, now);
    let mut hours: Vec<Value> = match start {
        Some(w) => hour_lines(long, w - 6 * 3600)
            .into_iter()
            .filter(|l| l["ft"].as_i64().is_some_and(|t| t <= w))
            .map(|l| {
                if l["k"] == "hour_proc" {
                    json!({"k": "proc", "ft": l["ft"], "heap": l["heap_live"], "anon": l["anon_core"],
                           "rss_core": l["rss_core"], "rss_mihomo": l["rss_mihomo"], "cpu": l["cpu_core"],
                           "chill": [l["chill_on_s"], l["chill_ok_s"]],
                           "ts": [l["ts_on_s"], l["ts_chk_s"], l["ts_ok_s"]]})
                } else {
                    json!({"k": "power", "ft": l["ft"], "e": l["e"], "off_home_s": l["off_home_s"],
                           "off_home_e": l["off_home_e"], "chg_s": l["chg_s"], "pkts": l["off_home_wan_pkts"]})
                }
            })
            .collect(),
        None => Vec::new(),
    };
    let to_wall = i.to.as_ref().and_then(|t| start_wall(t, now));
    loop {
        let v = json!({
            "id": i.id, "kind": i.kind, "sub": i.sub, "from": i.from, "to": i.to, "end": i.end,
            "from_wall": start, "to_wall": to_wall,
            "count": i.count, "last": i.last, "build": i.build, "detail": i.detail, "observed": i.observed,
            "ship": i.ship, "ship_clock": i.ship_clock, "drill": i.drill,
            "boot_code": f.boot_code, "planned": f.planned,
            // only a signal-backed end measures a recovery; "quiet" is just the window passing
            "recovery_s": i.to.as_ref()
                .filter(|_| i.end.as_deref() == Some("recovered"))
                .and_then(|t| i.from.until(t)),
            "alert_seqs": i.alert_seqs, "alert_more": i.alert_more,
            "written": now, "hours_note": "exits: chill [on_s, ok_s], ts [on_s, checked_s, ok_s]",
            "hours": hours,
        });
        let s = v.to_string();
        if s.len() <= EVIDENCE_MAX || hours.is_empty() {
            return s;
        }
        hours.remove(0);
    }
}

/// A memory incident's snapshot (CEO review X2): smaps_rollup (else the anonymous sizes summed
/// from smaps), the thread count, the heap counters; at most [`MEMSNAP_MAX`].
pub fn memsnap(long: &Path, proc_self: &Path, id: &str) -> std::io::Result<()> {
    let mut s = String::new();
    match std::fs::read_to_string(proc_self.join("smaps_rollup")) {
        Ok(t) => s.push_str(&t),
        Err(_) => {
            let t = std::fs::read_to_string(proc_self.join("smaps")).unwrap_or_default();
            let anon: i64 = t
                .lines()
                .filter_map(|l| l.strip_prefix("Anonymous:"))
                .filter_map(|v| v.split_whitespace().next()?.parse::<i64>().ok())
                .sum();
            s.push_str(&format!("Anonymous (smaps sum): {anon} kB\n"));
        }
    }
    let status = std::fs::read_to_string(proc_self.join("status")).unwrap_or_default();
    for l in status.lines() {
        if l.starts_with("Threads:") || l.starts_with("VmRSS:") || l.starts_with("RssAnon:") {
            s.push_str(l);
            s.push('\n');
        }
    }
    let (live, peak) = crate::mem::stats();
    s.push_str(&format!("heap live: {live} kB\nheap peak: {peak} kB\n"));
    let s: String = s.chars().take(MEMSNAP_MAX).collect();
    write_synced(&long.join(format!("memsnap-{id}.txt")), &s)
}

/// Keep the newest `keep` of `<prefix>*<suffix>` (by mtime), none older than `days`.
pub fn prune_files(dir: &Path, prefix: &str, suffix: &str, keep: usize, days: Option<i64>) {
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix))
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.0));
    let now = std::time::SystemTime::now();
    for (k, (t, p)) in v.iter().enumerate() {
        let old = days.is_some_and(|d| {
            now.duration_since(*t)
                .is_ok_and(|a| a.as_secs() as i64 > d * 86_400)
        });
        if k >= keep || old {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// `/v2/incidents`: the list with `unread` from the alert queue and its read cursor
/// (eng review round 3 D10); `{v, open: [], closed: [], error}` before the layer ran.
pub fn incidents(alerts: &Path) -> Value {
    let v = view();
    if !v.ready {
        return json!({"v": VERSION, "open": [], "closed": [], "error": "the regress layer has not run yet (LUYAZ_LONGTERM off, or the core just started)"});
    }
    let a = luyaz_api::alerts::Alerts::at(alerts);
    let read = a.read_u64("read");
    let queued: std::collections::HashSet<u64> = a.events().iter().map(|e| e.seq).collect();
    let mut out = v.list.clone();
    for side in ["open", "closed"] {
        if let Some(list) = out[side].as_array_mut() {
            for it in list {
                let unread = it["alert_seqs"].as_array().is_some_and(|s| {
                    s.iter()
                        .filter_map(Value::as_u64)
                        .any(|q| q > read && queued.contains(&q))
                });
                it["unread"] = json!(unread);
                if let Some(o) = it.as_object_mut() {
                    o.remove("alert_seqs");
                }
            }
        }
    }
    out
}

/// An incident id as readers may ask for it.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `/v2/incidents/<id>`: the incident, its evidence and memory snapshot, and its alerts from the
/// whole queue. Err(()) = not known (pruned, or never was): 404.
pub fn incident(id: &str, alerts: &Path) -> Result<Value, ()> {
    let v = view();
    incident_in(&v.file, &v.long, id, alerts)
}

/// [`incident`] from a file and a long-term directory. An incident past the 8 days
/// `incidents.json` keeps is read back from its evidence (kept 30 days): `archived`.
pub fn incident_in(file: &File, long: &Path, id: &str, alerts: &Path) -> Result<Value, ()> {
    if !valid_id(id) {
        return Err(());
    }
    let read_small = |name: &str, max: usize| -> Option<String> {
        let t = std::fs::read_to_string(long.join(name)).ok()?;
        Some(t.chars().take(max).collect())
    };
    let evidence = read_small(&format!("incident-{id}.json"), EVIDENCE_MAX)
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let kept = file.open.iter().chain(&file.closed).find(|i| i.id == id);
    let (i, archived) = match kept {
        Some(i) => (i.clone(), false),
        None => {
            let e = evidence.as_ref().ok_or(())?;
            let i = serde_json::from_value::<super::Incident>(e.clone()).map_err(|_| ())?;
            if i.id != id {
                return Err(());
            }
            (i, true)
        }
    };
    let a = luyaz_api::alerts::Alerts::at(alerts);
    let read = a.read_u64("read");
    let list: Vec<Value> = a
        .events()
        .iter()
        .filter(|e| i.alert_seqs.contains(&e.seq))
        .map(|e| {
            json!({
                "seq": e.seq,
                "time": (e.wall >= 1_704_067_200).then_some(e.wall),
                "kind": e.kind,
                "text": e.text,
                "unread": e.seq > read,
            })
        })
        .collect();
    let memsnap = read_small(&format!("memsnap-{id}.txt"), MEMSNAP_MAX);
    let mut out = super::listed(&i);
    if let Some(o) = out.as_object_mut() {
        o.remove("alert_seqs");
        let unread = list.iter().any(|x| x["unread"] == true);
        o.insert("unread".into(), json!(unread));
        o.insert("alert_more".into(), json!(i.alert_more));
        o.insert("alerts".into(), json!(list));
        o.insert("archived".into(), json!(archived));
        if archived {
            o.insert("evidence".into(), json!(true));
        }
        let ev = |k: &str| evidence.as_ref().map_or(Value::Null, |e| e[k].clone());
        o.insert("evidence_hours".into(), ev("hours"));
        o.insert("recovery_s".into(), ev("recovery_s"));
        o.insert("boot_code".into(), ev("boot_code"));
        o.insert("memsnap".into(), json!(memsnap));
        let (zh, en) = (
            luyaz_api::alerts::kind_label(&i.kind),
            luyaz_api::alerts::kind_label_en(&i.kind),
        );
        o.insert("label".into(), json!(zh));
        o.insert("label_en".into(), json!(en));
    }
    Ok(out)
}

/// The incident each alert in an `/api/alerts` answer belongs to (eng review round 3 D2).
pub fn tag_alerts(v: &mut Value) {
    let view = view();
    if !view.ready {
        return;
    }
    if let Some(list) = v["events"].as_array_mut() {
        for e in list {
            if let Some(seq) = e["seq"].as_u64()
                && let Some(i) = view.file.incident_of(seq)
            {
                e["incident"] = json!(i.id);
            }
        }
    }
}
