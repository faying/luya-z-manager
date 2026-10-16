//! Test only: spins until the host's fuel runs out.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/battery-eta.wit", world: "battery-eta" });

struct Hostile;

impl Guest for Hostile {
    fn estimate(_input: Input) -> Estimate {
        let mut x: u64 = 1;
        loop {
            x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
    }
}

export!(Hostile);
