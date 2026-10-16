//! A fake device for the engine (this crate's tests and u60d's, feature `testing`): scans, the
//! SIM, the Wi-Fi switch, the routes and both clocks are the test's, files live under a temporary
//! root, and every wait advances the fake clock instead of sleeping.

use crate::*;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

#[derive(Default)]
pub struct Dev {
    pub now: i64,
    pub mono: f64,
    /// What the next scans return, in order (then the last one, again and again).
    pub scans: VecDeque<Result<Vec<Network>, String>>,
    pub last_scan: Option<Result<Vec<Network>, String>>,
    pub mcc: Option<String>,
    pub uci: BTreeMap<String, String>,
    /// Wi-Fi switch calls and other writes, in order.
    pub calls: Vec<String>,
    /// `wifi_apply` fails while this is set.
    pub wifi_fails: bool,
    /// GET answers by path.
    pub gets: HashMap<String, Value>,
    /// exec fails for these paths.
    pub exec_fails: Vec<String>,
    pub records: Vec<Value>,
    pub auto_sleep: Option<bool>,
}

pub struct Fake {
    pub root: String,
    pub dev: Mutex<Dev>,
    /// The host's "still ours" flag, when a test hands the engine over and back.
    pub active: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
}

fn ap_on(uci: &BTreeMap<String, String>, key: &str) -> bool {
    uci.get(key).map(String::as_str) != Some("1")
}

impl Fake {
    pub fn new(name: &str) -> Arc<Fake> {
        let root = std::env::temp_dir().join(format!("u60-scenario-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("data/scenario")).unwrap();
        fs::create_dir_all(root.join("tmp")).unwrap();
        fs::create_dir_all(root.join("sys/class/rtc/rtc0")).unwrap();
        fs::write(root.join("sys/class/rtc/rtc0/since_epoch"), "1000\n").unwrap();
        let f = Arc::new(Fake {
            root: root.to_string_lossy().into_owned(),
            dev: Mutex::new(Dev::default()),
            active: Mutex::new(None),
        });
        {
            let mut d = f.d();
            d.now = 1_700_000_000;
            d.uci.insert(luyaz_wifi::AP_2G.into(), "0".into());
            d.uci.insert(luyaz_wifi::AP_5G.into(), "0".into());
        }
        f
    }

    pub fn d(&self) -> std::sync::MutexGuard<'_, Dev> {
        lk(&self.dev)
    }

    pub fn file(&self, p: &str) -> String {
        format!("{}{p}", self.root)
    }

    pub fn write_config(&self, cfg: &Config) {
        fs::write(self.file(CONFIG_FILE), serde_json::to_string_pretty(cfg).unwrap()).unwrap();
    }

    pub fn calls(&self) -> Vec<String> {
        std::mem::take(&mut self.d().calls)
    }

    pub fn records(&self) -> Vec<Value> {
        std::mem::take(&mut self.d().records)
    }

    pub fn scan_returns(&self, nets: Vec<Network>) {
        let mut d = self.d();
        d.scans.clear();
        d.last_scan = Some(Ok(nets));
    }

    pub fn log(&self) -> String {
        fs::read_to_string(self.file(LOG_FILE)).unwrap_or_default()
    }
}

impl luyaz_wifi::Host for Fake {
    fn uci_get(&self, key: &str) -> Option<String> {
        self.d().uci.get(key).cloned()
    }
    fn write_aps(&self, _: bool, _: bool) -> Result<Option<String>, String> {
        unreachable!("the fake overrides every Wi-Fi method")
    }
    fn vendor_on(&self) -> Result<(), String> {
        unreachable!()
    }
    fn reload(&self) -> Result<Value, String> {
        unreachable!()
    }
}

impl Io for Fake {
    fn path(&self, p: &str) -> String {
        self.file(p)
    }
    fn now(&self) -> i64 {
        self.d().now
    }
    fn mono(&self) -> f64 {
        self.d().mono
    }
    fn sleep(&self, d: Duration) {
        let mut dev = self.d();
        dev.now += d.as_secs() as i64;
        dev.mono += d.as_secs_f64();
    }
    fn active(&self) -> bool {
        lk(&self.active).as_ref().is_none_or(|a| a.load(std::sync::atomic::Ordering::SeqCst))
    }
    /// The wait between ticks takes a little real time when the engine runs on its own thread (a
    /// test that hands it over), so the loop does not spin.
    fn idle(&self, d: Duration) {
        if lk(&self.active).is_some() {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.sleep(d);
    }
    fn uptime_secs(&self) -> Option<u64> {
        Some(self.d().mono as u64 + 100)
    }
    fn scan(&self) -> Result<Vec<Network>, String> {
        let mut d = self.d();
        let next = d.scans.pop_front().or_else(|| d.last_scan.clone()).unwrap_or(Ok(Vec::new()));
        d.calls.push("scan".into());
        next
    }
    fn wifi(&self) -> &dyn luyaz_wifi::Host {
        self
    }
    fn beaconing(&self) -> bool {
        let d = self.d();
        ap_on(&d.uci, luyaz_wifi::AP_2G) || ap_on(&d.uci, luyaz_wifi::AP_5G)
    }
    fn wifi_fully_on(&self) -> bool {
        let d = self.d();
        ap_on(&d.uci, luyaz_wifi::AP_2G) && ap_on(&d.uci, luyaz_wifi::AP_5G)
    }
    fn wifi_snapshot(&self) -> (bool, bool) {
        let d = self.d();
        (ap_on(&d.uci, luyaz_wifi::AP_2G), ap_on(&d.uci, luyaz_wifi::AP_5G))
    }
    fn wifi_apply(&self, a: bool, b: bool) -> Result<(), String> {
        let mut d = self.d();
        d.calls.push(format!("wifi {a} {b}"));
        if d.wifi_fails {
            return Err("AP state not observable within deadline".into());
        }
        d.uci.insert(luyaz_wifi::AP_2G.into(), if a { "0" } else { "1" }.into());
        d.uci.insert(luyaz_wifi::AP_5G.into(), if b { "0" } else { "1" }.into());
        Ok(())
    }
    fn wifi_lock(&self, wait: Duration) -> Result<luyaz_wifi::WifiLock, String> {
        luyaz_wifi::lock_at(&self.file(luyaz_wifi::LOCK_FILE), wait)
    }
    fn wifi_apply_locked(&self, _lk: &luyaz_wifi::WifiLock, a: bool, b: bool) -> Result<(), String> {
        let mut d = self.d();
        d.calls.push(format!("wifi-locked {a} {b}"));
        d.uci.insert(luyaz_wifi::AP_2G.into(), if a { "0" } else { "1" }.into());
        d.uci.insert(luyaz_wifi::AP_5G.into(), if b { "0" } else { "1" }.into());
        Ok(())
    }
    fn wifi_put_back(&self, set: serde_json::Map<String, Value>) {
        self.d().calls.push(format!("put-back {}", Value::Object(set)));
    }
    fn exec(&self, method: &str, path: &str, body: &[u8]) -> Result<(), String> {
        let mut d = self.d();
        d.calls.push(format!("{method} {path} {}{}", String::from_utf8_lossy(body), if in_restore() { " (restore)" } else { "" }));
        if d.exec_fails.iter().any(|p| p == path) {
            return Err("proxy unreachable".into());
        }
        Ok(())
    }
    fn get(&self, path: &str) -> (u16, Value) {
        match self.d().gets.get(path) {
            Some(v) => (200, v.clone()),
            None => (404, json!({"ok": false})),
        }
    }
    fn set_auto_sleep(&self, enabled: bool) {
        let mut d = self.d();
        d.auto_sleep = Some(enabled);
        d.calls.push(format!("auto-sleep {enabled}"));
    }
    fn charging(&self) -> Option<bool> {
        Some(false)
    }
    fn sim_mcc(&self) -> Option<String> {
        self.d().mcc.clone()
    }
    fn publish(&self, ring: &str) {
        let v: Value = serde_json::from_str(ring).unwrap();
        self.d().records.push(v.as_array().unwrap().last().unwrap().clone());
    }
}

pub fn net(ssid: &str, bssid: &str, signal: f64) -> Network {
    Network { ssid: ssid.into(), bssid: bssid.into(), signal }
}

