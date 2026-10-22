//! `<core> lanid-selftest real|hostile <plugins-dir>`: the lanid component checks of tests.rs, run
//! by the device build itself (`release-device`, panic = abort, which `cargo test` can't use), so a
//! host-side panic on a hostile answer would end this process here instead of passing in a test.
//! `real`: the parser on the fixtures and on malformed frames; `hostile`: crates/luyaz-plugins
//! hostile-lanid-parse (compiled for the device: plugc's default target) under the real name.
//! One-shot, read-only: nothing is stored, no socket. Exit 0 = passed.

use super::*;

#[path = "../../../luyaz-plugins/lanid-parse/src/fixtures.rs"]
mod fixtures;
use fixtures::*;

fn mac(n: u8) -> Mac {
    [0x00, 0x00, 0x5e, 0x00, 0x53, n]
}

fn frame(m: Mac, tail: &[u8]) -> Vec<u8> {
    ipv4_udp(m, IP_A, BCAST, 68, 67, tail)
}

fn check(ok: bool, what: &str, fails: &mut u32) {
    println!("{} {what}", if ok { "ok  " } else { "FAIL" });
    if !ok {
        *fails += 1;
    }
}

fn real(c: &Component, fails: &mut u32) {
    let frames = vec![
        dhcp_frame(
            MAC_A,
            Some("test-phone"),
            Some("android-dhcp-14"),
            &[1, 3, 6],
        ),
        mdns_laptop(MAC_B, IP_A, "Test Laptop", "Mac14,2"),
        ssdp_notify(MAC_B, IP_A, "Linux/5.0 UPnP/1.0 TestCo/2.1"),
        netbios_register(MAC_B, IP_A, "TESTPC", 0x20, false),
    ];
    let (_, n) = Gate::default().run(c, frames, Instant::now());
    check(
        (n.traps, n.invalid, n.facts) == (0, 0, 8),
        &format!(
            "fixtures: traps {} invalid {} facts {} (want 0 0 8)",
            n.traps, n.invalid, n.facts
        ),
        fails,
    );
    let good = mdns_laptop(MAC_B, IP_A, "Test Laptop", "Mac14,2");
    let mut bad = vec![
        mdns_pointer_loop(MAC_B, IP_A),
        mdns_oversize(MAC_B, IP_A),
        vec![0u8; 14],
        good[..good.len() / 2].to_vec(),
        vec![0xffu8; 1514],
    ];
    for i in (14..good.len()).step_by(7) {
        let mut g = good.clone();
        g[i] = g[i].wrapping_add(0x41);
        bad.push(g);
    }
    let mut traps = 0;
    for chunk in bad.chunks(QUEUE) {
        match c.parse(chunk) {
            Ok(out) => traps += u32::from(out.len() != chunk.len()),
            Err(_) => traps += 1,
        }
    }
    check(
        traps == 0,
        &format!("{} malformed frames: {traps} bad batches", bad.len()),
        fails,
    );
}

fn hostile(c: &Component, fails: &mut u32) {
    let mut g = Gate::default();
    let t = Instant::now();
    let frames = vec![
        frame(mac(1), b"fine"),
        frame(mac(2), b"HOSTILE-SPIN"),
        frame(mac(3), b"HOSTILE-MEM"),
        frame(mac(4), b"HOSTILE-PANIC"),
        frame(mac(5), b"HOSTILE-BIG"),
        frame(mac(6), b"fine"),
    ];
    let started = Instant::now();
    let (out, n) = g.run(c, frames, t);
    let took = started.elapsed();
    let who: Vec<Mac> = out.iter().map(|(m, _)| *m).collect();
    check(
        who == [mac(1), mac(6)],
        "only the two fine frames answered",
        fails,
    );
    check(
        (n.traps, n.bad_frames, n.invalid) == (4, 3, 1),
        &format!(
            "traps {} bad {} invalid {} (want 4 3 1: spin, mem, panic trap; big over the limits)",
            n.traps, n.bad_frames, n.invalid
        ),
        fails,
    );
    check(
        g.blocked_now(t) == 3,
        &format!("blocked {} MACs (want 3)", g.blocked_now(t)),
        fails,
    );
    check(
        took < Duration::from_secs(20),
        &format!("batch took {took:?}"),
        fails,
    );
    let (out, n) = g.run(c, vec![frame(mac(7), b"fine")], t);
    check(
        (out.len(), n.traps) == (1, 0),
        "component still answers afterwards",
        fails,
    );
}

pub fn main(args: &[String]) -> i32 {
    let (Some(mode), Some(dir)) = (args.first(), args.get(1)) else {
        eprintln!("usage: lanid-selftest real|hostile <plugins-dir>");
        return 2;
    };
    let c = match Component::load(Path::new(dir)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lanid-selftest: load {dir}: {e}");
            return 1;
        }
    };
    let mut fails = 0;
    match mode.as_str() {
        "real" => real(&c, &mut fails),
        "hostile" => hostile(&c, &mut fails),
        _ => {
            eprintln!("usage: lanid-selftest real|hostile <plugins-dir>");
            return 2;
        }
    }
    println!(
        "lanid-selftest {mode}: {}",
        if fails == 0 { "passed" } else { "FAILED" }
    );
    i32::from(fails > 0)
}
