//! Test only: spins until the host's fuel runs out.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/sms-forward.wit", world: "sms-forward" });

struct Hostile;

fn spin() -> ! {
    let mut x: u64 = 1;
    loop {
        x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}

impl Guest for Hostile {
    fn replay_pass(_input: PassIn) -> PassOut {
        spin()
    }
    fn settle(_held: Option<Held>, _sms_id: u64, _failures: Vec<bool>, _delivered: Vec<u32>, _wall: i64) -> Settled {
        spin()
    }
}

export!(Hostile);
