//! The scenario component: `luyaz_policy::scenario::{step, detect, scan_interval, after_switch}`
//! behind `wit/scenario.wit`. Only type conversion here; the policy and its tests live in
//! u60-policy. The config arrives without names and actions (see the wit header): they are filled
//! in empty, which the decisions never look at.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/scenario.wit", world: "scenario-policy" });

use luyaz_policy::scenario as p;

fn config(c: Config) -> p::Config {
    p::Config {
        version: c.version,
        home_mcc: c.home_mcc,
        params: p::Params {
            scan_interval_home_secs: c.params.scan_interval_home_secs,
            scan_interval_away_battery_secs: c.params.scan_interval_away_battery_secs,
            scan_interval_away_charging_secs: c.params.scan_interval_away_charging_secs,
            enter_hits: c.params.enter_hits,
            exit_misses: c.params.exit_misses,
            min_rssi_dbm: c.params.min_rssi_dbm,
        },
        scenarios: c
            .scenarios
            .into_iter()
            .map(|s| p::Scenario {
                id: s.id,
                name: String::new(),
                detect: match s.detect {
                    Detect::Ssid(entries) => p::Detect::Ssid {
                        entries: entries
                            .into_iter()
                            .map(|e| p::SsidEntry {
                                ssid: e.ssid,
                                bssid: e.bssid,
                            })
                            .collect(),
                    },
                    Detect::Mcc(mccs) => p::Detect::Mcc { mccs },
                    Detect::Abroad => p::Detect::Abroad,
                    Detect::Fallback => p::Detect::Fallback,
                },
                actions: Vec::new(),
                inhibit_sleep: s.inhibit_sleep,
            })
            .collect(),
    }
}

fn state_in(s: RunState) -> p::RunState {
    p::RunState {
        current: s.current,
        candidate: s.candidate,
        hits: s.hits,
        misses: s.misses,
        applied: s.applied.into_iter().collect(),
        last_switch: s.last_switch,
        last_error: s.last_error,
        last_scan: s.last_scan,
        scan_failures: s.scan_failures,
    }
}

fn state_out(s: p::RunState) -> RunState {
    RunState {
        current: s.current,
        candidate: s.candidate,
        hits: s.hits,
        misses: s.misses,
        applied: s.applied.into_iter().collect(),
        last_switch: s.last_switch,
        last_error: s.last_error,
        last_scan: s.last_scan,
        scan_failures: s.scan_failures,
    }
}

fn nets(n: Vec<Network>) -> Vec<p::Network> {
    n.into_iter()
        .map(|n| p::Network {
            ssid: n.ssid,
            bssid: n.bssid,
            signal: n.signal,
        })
        .collect()
}

struct Policy;

impl Guest for Policy {
    fn step(i: TickInput) -> TickOutcome {
        let cfg = config(i.config);
        let st = state_in(i.state);
        let (scanned, error) = match i.scan {
            Scan::NotScanned => (None, String::new()),
            Scan::Failed(f) => (Some((f.at, Vec::new(), true)), f.error),
            Scan::Networks(s) => (Some((s.at, nets(s.networks), false)), String::new()),
        };
        let recheck = i.recheck.map(|r| (r.at, nets(r.networks)));
        let o = p::step(&p::TickInput {
            cfg: &cfg,
            state: &st,
            enabled: i.enabled,
            takeover: i.takeover,
            pin: i.pin.as_deref(),
            now: i.now,
            pin_retry_at: i.pin_retry_at,
            charging: i.charging,
            scan: match &scanned {
                None => p::Scan::NotScanned,
                Some((at, _, true)) => p::Scan::Failed {
                    at: *at,
                    error: &error,
                },
                Some((at, n, false)) => p::Scan::Networks {
                    at: *at,
                    networks: n,
                },
            },
            recheck: recheck.as_ref().map(|(at, n)| p::Recheck {
                at: *at,
                networks: n,
            }),
            mcc: i.mcc.as_deref(),
            beaconing: i.beaconing,
        });
        TickOutcome {
            branch: match o.branch {
                p::Branch::Takeover => Branch::Takeover,
                p::Branch::Disabled => Branch::Disabled,
                p::Branch::Unconfigured => Branch::Unconfigured,
                p::Branch::Pinned => Branch::Pinned,
                p::Branch::NotDue => Branch::NotDue,
                p::Branch::Due => Branch::Due,
                p::Branch::ScanFailed => Branch::ScanFailed,
                p::Branch::Scanned => Branch::Scanned,
            },
            state: state_out(o.state),
            intent: match o.intent {
                p::Intent::None => Intent::None,
                p::Intent::Switch { to } => Intent::Switch(to),
                p::Intent::Bootsafe => Intent::Bootsafe,
                p::Intent::NeedScan => Intent::NeedScan,
                p::Intent::NeedRecheck => Intent::NeedRecheck,
            },
            restores: o.restores.map(|r| Restores { next: r.next }),
            keep_awake: o.keep_awake,
            pin_retry_at: o.pin_retry_at,
        }
    }

    fn detect(c: Config, networks: Vec<Network>, mcc: Option<String>) -> Option<String> {
        p::detect(&config(c), &nets(networks), mcc.as_deref())
    }

    fn scan_interval(c: Config, current: String, candidate: String, charging: bool) -> u64 {
        p::scan_interval(&config(c), &current, &candidate, charging)
    }

    fn after_switch(
        state: RunState,
        c: Config,
        id: String,
        now: i64,
        outcome: Result<(), String>,
    ) -> RunState {
        let r = outcome.as_ref().map(|_| ()).map_err(String::as_str);
        state_out(p::after_switch(&state_in(state), &config(c), &id, now, r))
    }
}

export!(Policy);
