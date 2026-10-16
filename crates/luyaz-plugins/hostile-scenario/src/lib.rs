//! Test only: every export spins until the host's fuel runs out.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/scenario.wit", world: "scenario-policy" });

struct Hostile;

fn spin() -> ! {
    let mut x: u64 = 1;
    loop {
        x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}

impl Guest for Hostile {
    fn step(_i: TickInput) -> TickOutcome {
        spin()
    }
    fn detect(_c: Config, _n: Vec<Network>, _m: Option<String>) -> Option<String> {
        spin()
    }
    fn scan_interval(_c: Config, _cur: String, _cand: String, _ch: bool) -> u64 {
        spin()
    }
    fn after_switch(
        _s: RunState,
        _c: Config,
        _id: String,
        _now: i64,
        _r: Result<(), String>,
    ) -> RunState {
        spin()
    }
}

export!(Hostile);
