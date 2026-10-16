//! Test only: spins until the host's fuel runs out.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/home-exit.wit", world: "home-exit" });

struct Hostile;

impl Guest for Hostile {
    fn replay(_before: Machine, _s: Step, _recorded: Vec<Event>) -> Outcome {
        let mut x: u64 = 1;
        loop {
            x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
    }
}

export!(Hostile);
