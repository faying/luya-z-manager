//! Test only: spins until the host's fuel runs out.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/scheduler.wit", world: "scheduler" });

struct Hostile;

impl Guest for Hostile {
    fn due(_jobs: Vec<Job>, _now_ts: i64, _hh: u8, _mm: u8, _dow: u8) -> Vec<Fire> {
        let mut x: u64 = 1;
        loop {
            x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
    }
}

export!(Hostile);
