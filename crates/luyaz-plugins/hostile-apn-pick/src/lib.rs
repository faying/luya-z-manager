//! Test only: spins until the host's fuel runs out.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/apn-pick.wit", world: "apn-pick" });

struct Hostile;

impl Guest for Hostile {
    fn decide(_s: Store, _before: Watch, _iccid: String, _busy: bool, _apn: ApnRead) -> Look {
        let mut x: u64 = 1;
        loop {
            x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
    }
}

export!(Hostile);
