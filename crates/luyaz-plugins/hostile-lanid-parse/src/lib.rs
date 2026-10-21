//! Test only (luyaz-core src/lanid/tests.rs): a lanid-parse that misbehaves on frames carrying a
//! marker: `HOSTILE-SPIN` spins until the fuel runs out, `HOSTILE-MEM` grows memory past the cap,
//! `HOSTILE-PANIC` panics, `HOSTILE-BIG` answers 20 facts of 100 bytes. Any other frame gets
//! one fact, `netbios-name` "plain", so a test can see which frames survived. Never installed.

wit_bindgen::generate!({ path: "../../luyaz-policy/wit/lanid-parse.wit", world: "lanid-parse" });

struct Hostile;

fn has(f: &[u8], m: &[u8]) -> bool {
    f.windows(m.len()).any(|w| w == m)
}

impl Guest for Hostile {
    fn parse(frames: Vec<Vec<u8>>) -> Vec<Vec<Fact>> {
        let mut out = Vec::new();
        for f in &frames {
            if has(f, b"HOSTILE-SPIN") {
                let mut x: u64 = 1;
                loop {
                    x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
            }
            if has(f, b"HOSTILE-MEM") {
                let mut keep: Vec<Vec<u8>> = Vec::new();
                loop {
                    keep.push(core::hint::black_box(vec![1u8; 1 << 20]));
                }
            }
            if has(f, b"HOSTILE-PANIC") {
                panic!("hostile");
            }
            if has(f, b"HOSTILE-BIG") {
                out.push(
                    (0..20)
                        .map(|_| Fact {
                            kind: FactKind::MdnsService,
                            value: "z".repeat(100),
                        })
                        .collect(),
                );
                continue;
            }
            out.push(vec![Fact {
                kind: FactKind::NetbiosName,
                value: "plain".into(),
            }]);
        }
        out
    }
}

export!(Hostile);
