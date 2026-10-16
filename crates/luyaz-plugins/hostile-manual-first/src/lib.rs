//! Test only: spins until the host's fuel runs out.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/manual-first.wit", world: "manual-first" });

struct Hostile;

fn spin() -> ! {
    let mut x: u64 = 1;
    loop {
        x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}

impl Guest for Hostile {
    fn replay(_before: Machine, _o: Obs, _recorded: Vec<Event>) -> Outcome {
        spin()
    }
    fn needs_detail(_m: Machine, _main_now: String) -> bool {
        spin()
    }
}

export!(Hostile);
