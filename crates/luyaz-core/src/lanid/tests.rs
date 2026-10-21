//! lanid's host side (rate limit, trap handling, output limits, storage, merge) with a test
//! double, and (with components built for this machine, crates/luyaz-plugins/build.sh test:
//! `LUYAZ_TEST_PLUGINS`, `LUYAZ_TEST_HOSTILE`) the real parser and a hostile one.

use super::*;

#[path = "../../../luyaz-plugins/lanid-parse/src/fixtures.rs"]
mod fixtures;
use fixtures::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lanid-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A parser double: frames containing `BAD` trap, the rest answer what `answer` says.
struct Fake<F: Fn(&[u8]) -> Vec<Fact>>(F);
impl<F: Fn(&[u8]) -> Vec<Fact>> Parse for Fake<F> {
    fn parse(&self, frames: &[Vec<u8>]) -> Result<Vec<Vec<Fact>>, String> {
        if frames.iter().any(|f| f.windows(3).any(|w| w == b"BAD")) {
            return Err("trap: unreachable".into());
        }
        Ok(frames.iter().map(|f| (self.0)(f)).collect())
    }
}

fn host(v: &str) -> Vec<Fact> {
    vec![Fact {
        kind: Kind::DhcpHostname,
        value: v.into(),
    }]
}

fn frame(mac: Mac, tail: &[u8]) -> Vec<u8> {
    ipv4_udp(mac, IP_A, BCAST, 68, 67, tail)
}

fn mac(n: u8) -> Mac {
    [0x00, 0x00, 0x5e, 0x00, 0x53, n]
}

#[test]
fn rate_limit_per_mac_and_table_cap() {
    let mut g = Gate::default();
    let p = Fake(|_| host("test-host"));
    let t = Instant::now();
    let frames: Vec<_> = (0..40).map(|_| frame(mac(1), b"x")).collect();
    let (_, n) = g.run(&p, frames, t);
    // 40 frames, the queue takes them all; 30 pass the bucket
    assert_eq!((n.frames, n.rate_dropped, n.facts), (40, 10, 30));
    // a minute later the bucket is full again
    let (_, n) = g.run(&p, vec![frame(mac(1), b"x")], t + Duration::from_secs(60));
    assert_eq!(n.rate_dropped, 0);
    // never more than MAX_MACS buckets
    for i in 0..600u32 {
        let m = [0x00, 0x00, 0x5e, (i >> 8) as u8, 0x54, i as u8];
        g.run(&p, vec![frame(m, b"x")], t);
    }
    assert!(g.rate.len() <= MAX_MACS);
    // a batch never takes more than the queue
    let (_, n) = g.run(&p, (0..100).map(|i| frame(mac(i), b"x")).collect(), t);
    assert_eq!(n.frames, QUEUE as u64);
}

#[test]
fn short_and_multicast_sources_are_invalid() {
    let mut g = Gate::default();
    let p = Fake(|_| host("x"));
    let mut mc = frame(mac(2), b"x");
    mc[6] = 0x01;
    let (out, n) = g.run(&p, vec![vec![1, 2, 3], mc], Instant::now());
    assert!(out.is_empty());
    assert_eq!((n.invalid, n.batches), (2, 0));
}

#[test]
fn a_trap_reruns_per_frame_and_blocks_the_sender() {
    let mut g = Gate::default();
    let p = Fake(|_| host("test-host"));
    let t = Instant::now();
    let frames = vec![
        frame(mac(1), b"ok"),
        frame(mac(2), b"BAD"),
        frame(mac(3), b"ok"),
    ];
    let (out, n) = g.run(&p, frames, t);
    let who: Vec<Mac> = out.iter().map(|(m, _)| *m).collect();
    assert_eq!(who, [mac(1), mac(3)]);
    // the batch and the one frame on its own
    assert_eq!((n.traps, n.bad_frames, n.batches), (2, 1, 1));
    assert_eq!(g.blocked_now(t), 1);
    // blocked for an hour, even with a good frame
    let (out, n) = g.run(&p, vec![frame(mac(2), b"ok")], t + Duration::from_secs(10));
    assert!(out.is_empty());
    assert_eq!(n.blocked_dropped, 1);
    let (out, _) = g.run(&p, vec![frame(mac(2), b"ok")], t + BLOCK_FOR);
    assert_eq!(out.len(), 1);
}

#[test]
fn answers_over_the_limits_are_dropped() {
    let mut g = Gate::default();
    let t = Instant::now();
    let p = Fake(|f| match f.last() {
        Some(b'9') => (0..9)
            .map(|i| Fact {
                kind: Kind::MdnsService,
                value: format!("_s{i}._tcp"),
            })
            .collect(),
        Some(b'L') => host(&"y".repeat(65)),
        Some(b'C') => host("a\u{1}b"),
        Some(b'E') => host(""),
        _ => host("fine"),
    });
    let frames = b"9LCEk"
        .iter()
        .enumerate()
        .map(|(i, b)| frame(mac(i as u8 + 1), &[*b]))
        .collect();
    let (out, n) = g.run(&p, frames, t);
    assert_eq!(out.len(), 1);
    assert_eq!(n.invalid, 4);
    // an answer of the wrong length: the whole batch is invalid
    struct Short;
    impl Parse for Short {
        fn parse(&self, _: &[Vec<u8>]) -> Result<Vec<Vec<Fact>>, String> {
            Ok(vec![])
        }
    }
    let (out, n) = g.run(&Short, vec![frame(mac(9), b"a"), frame(mac(9), b"b")], t);
    assert!(out.is_empty());
    assert_eq!(n.invalid, 2);
}

#[test]
fn storage_writes_on_change_at_most_hourly() {
    let d = tmp("store");
    let mut s = Shared::open(&d, true);
    let t = s.last_write;
    let m = mac(1);
    assert!(s.learn(&m, &host("test-host"), 1_800_000_123));
    // the same facts again: not a change (only `last` moves)
    assert!(!s.learn(&m, &host("test-host"), 1_800_000_999));
    s.maybe_flush(t + Duration::from_secs(60), false);
    assert_eq!(s.writes, 0);
    s.maybe_flush(t + FLUSH_EVERY, false);
    assert_eq!(s.writes, 1);
    // nothing new: no write, even on exit
    s.maybe_flush(t + 3 * FLUSH_EVERY, true);
    assert_eq!(s.writes, 1);
    let v: Value = serde_json::from_slice(&std::fs::read(d.join("clients.json")).unwrap()).unwrap();
    assert_eq!(v["version"], 1);
    let r = &v["records"]["00:00:5e:00:53:01"];
    assert_eq!(r["facts"]["dhcp-hostname"], "test-host");
    // `last` written rounded down to the hour
    assert_eq!(r["last"], 1_800_000_999 - 1_800_000_999 % 3600);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(d.join("clients.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // read back by the next core
    let again = Shared::open(&d, true);
    assert_eq!(again.records.len(), 1);
    // exit writes without waiting for the hour
    let mut s = again;
    s.learn(&mac(2), &host("test-other"), crate::qmi_nas::now());
    s.maybe_flush(s.last_write, true);
    assert_eq!(Shared::open(&d, true).records.len(), 2);
}

#[test]
fn a_bad_or_unknown_file_is_empty() {
    let d = tmp("bad");
    std::fs::write(d.join("clients.json"), "{not json").unwrap();
    std::fs::write(
        d.join("aliases.json"),
        r#"{"version":2,"aliases":{"00:00:5e:00:53:01":"x"}}"#,
    )
    .unwrap();
    let s = Shared::open(&d, true);
    assert!(s.records.is_empty() && s.aliases.is_empty());
    std::fs::write(d.join("clients.json"), r#"{"version":9,"records":{}}"#).unwrap();
    assert!(Shared::open(&d, true).records.is_empty());
}

#[test]
fn records_capped_and_evicted_after_30_days() {
    let d = tmp("evict");
    let mut s = Shared::open(&d, true);
    let now = 1_800_000_000u64;
    for i in 0..300u32 {
        let m = [0x00, 0x00, 0x5e, (i >> 8) as u8, 0x53, i as u8];
        s.learn(&m, &host("h"), now + u64::from(i));
    }
    assert_eq!(s.records.len(), MAX_RECORDS);
    // the oldest went
    assert!(!s.records.contains_key("00:00:5e:00:53:00"));
    s.records.insert(
        "00:00:5e:00:53:fe".into(),
        Record {
            first: 1,
            last: now - EVICT_AFTER - 1,
            ..Default::default()
        },
    );
    s.evict(now);
    assert!(!s.records.contains_key("00:00:5e:00:53:fe"));
}

#[test]
fn aliases_separate_uncapped_and_read_back() {
    let d = tmp("alias");
    let mut s = Shared::open(&d, true);
    for i in 0..300u32 {
        let m = format!("00:00:5e:{:02x}:53:{:02x}", i >> 8, i & 0xff);
        s.set_alias(&m, &format!("Test {i}")).unwrap();
    }
    assert_eq!(Shared::open(&d, true).aliases.len(), 300);
    assert_eq!(
        s.set_alias("00-00-5E-00-53-01", " Test Phone ").unwrap(),
        ("00:00:5e:00:53:01".into(), "Test Phone".into())
    );
    assert_eq!(s.set_alias("00:00:5e:00:53:01", "").unwrap().1, "");
    assert!(s.set_alias("zz", "x").is_err());
    assert!(s.set_alias("00:00:5e:00:53:01", &"a".repeat(65)).is_err());
    // a side-by-side core does not write
    assert!(
        Shared::open(&d, false)
            .set_alias("00:00:5e:00:53:01", "x")
            .is_err()
    );
}

fn client(mac: &str, name: &str) -> c::Client {
    c::Client {
        ip: "10.9.0.21".into(),
        mac: mac.into(),
        name: name.into(),
        ..Default::default()
    }
}

#[test]
fn merge_priorities() {
    let d = tmp("merge");
    std::fs::write(d.join("oui.tsv"), "00005e\tTest Vendor\nnot a line\n").unwrap();
    let mut s = Shared::open(&d, true);
    let m = mac(1);
    let facts = |k: Kind, v: &str| Fact {
        kind: k,
        value: v.into(),
    };
    // off and no alias: as before
    let mut cl = client("00:00:5e:00:53:01", "dhcp-name");
    s.enrich(&mut cl);
    assert_eq!(cl, client("00:00:5e:00:53:01", "dhcp-name"));
    s.loaded = true;
    s.learn(
        &m,
        &[
            facts(Kind::NetbiosName, "TESTPC"),
            facts(Kind::SsdpServer, "Linux/5.0 UPnP/1.0 Sonos/80.1"),
            facts(Kind::DhcpVendorClass, "MSFT 5.0"),
        ],
        1,
    );
    // DHCP name beats NetBIOS; vendor SSDP beats OUI; type from option 60
    let mut cl = client("00:00:5e:00:53:01", "dhcp-name");
    let sources = s.enrich(&mut cl);
    assert_eq!(
        (cl.name.as_str(), cl.vendor.as_str(), cl.kind.as_str()),
        ("dhcp-name", "Sonos", "computer")
    );
    let srcs: Vec<(&str, &str)> = sources
        .iter()
        .map(|x| (x.field.as_str(), x.source.as_str()))
        .collect();
    assert_eq!(
        srcs,
        [
            ("name", "dhcp"),
            ("name", "netbios"),
            ("vendor", "ssdp"),
            ("vendor", "oui"),
            ("kind", "dhcp")
        ]
    );
    // no DHCP name (the projection's MAC fallback): NetBIOS
    let mut cl = client("00:00:5e:00:53:01", "00:00:5e:00:53:01");
    s.enrich(&mut cl);
    assert_eq!(cl.name, "TESTPC");
    // mDNS instance and model beat them; the alias beats everything
    s.learn(
        &m,
        &[
            facts(Kind::MdnsInstance, "Test Laptop"),
            facts(Kind::MdnsModel, "MacBookPro18,1"),
        ],
        2,
    );
    s.set_alias("00:00:5e:00:53:01", "My Test").unwrap();
    let mut cl = client("00:00:5e:00:53:01", "dhcp-name");
    s.enrich(&mut cl);
    assert_eq!(
        (
            cl.name.as_str(),
            cl.alias.as_str(),
            cl.vendor.as_str(),
            cl.kind.as_str()
        ),
        ("My Test", "My Test", "Apple", "laptop")
    );
    assert!(!cl.private);
    // a private MAC: flagged, no OUI
    let p = [0x02, 0x00, 0x5e, 0x00, 0x53, 0x07];
    s.learn(&p, &[facts(Kind::DhcpHostname, "test-phone")], 3);
    let mut cl = client("02:00:5e:00:53:07", "02:00:5e:00:53:07");
    s.enrich(&mut cl);
    assert!(cl.private);
    assert_eq!((cl.name.as_str(), cl.vendor.as_str()), ("test-phone", ""));
    // component off again: only the alias
    s.loaded = false;
    let mut cl = client("00:00:5e:00:53:01", "dhcp-name");
    let sources = s.enrich(&mut cl);
    assert_eq!((cl.name.as_str(), cl.vendor.as_str()), ("My Test", ""));
    assert_eq!(sources.len(), 2);
    // what the touch UI reads stays short (it skips client objects of 160 bytes or more)
    let mut cl = client("00:00:5e:00:53:01", "dhcp-name");
    s.loaded = true;
    s.enrich(&mut cl);
    let v = serde_json::to_string(&cl).unwrap();
    assert!(v.len() < 160, "{v}");
    let r = s.report();
    assert_eq!(
        r["merged"]["00:00:5e:00:53:01"]["client"]["name"],
        "My Test"
    );
    assert_eq!(
        r["merged"]["00:00:5e:00:53:01"]["sources"][0]["source"],
        "alias"
    );
}

#[test]
fn status_is_published_at_most_every_10_minutes() {
    let mut s = Shared::open(&tmp("pub"), false);
    s.loaded = true;
    s.running = true;
    let t = Instant::now();
    let a = s.published(t);
    assert!(a.sources.mdns && a.why.is_empty());
    s.counters.batches = 5;
    assert_eq!(s.published(t + Duration::from_secs(60)).batches, 0);
    assert_eq!(s.published(t + PUBLISH_EVERY).batches, 5);
    // turning off shows at once
    s.running = false;
    s.why = "socket: test".into();
    let b = s.published(t + PUBLISH_EVERY + Duration::from_secs(1));
    assert!(!b.sources.dhcp && b.why == "socket: test");
}

#[test]
fn oui_table_and_classify() {
    let o = classify::Oui::parse("00005E\tTest Vendor\n00005\tshort\nzzzzzz\tbad\n");
    assert_eq!(o.prefixes(), 1);
    assert_eq!(o.get(&mac(1)), Some("Test Vendor"));
    let mut r = Record::default();
    r.services.push("_googlecast._tcp".into());
    assert_eq!(classify::kinds(Some(&r)), [("mdns", "tv")]);
}

#[test]
fn no_component_means_off() {
    let d = tmp("nocomp");
    assert_eq!(Component::load(&d).err().as_deref(), Some("no component"));
    let bytes = b"not a cwasm";
    std::fs::write(d.join("l.cwasm"), bytes).unwrap();
    use sha2::Digest;
    let sha: String = sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let m = json!({"plugins": [{"name": NAME, "abi": ABI, "file": "l.cwasm", "sha256": sha}]});
    std::fs::write(d.join("plugins.json"), m.to_string()).unwrap();
    assert!(
        Component::load(&d)
            .err()
            .unwrap()
            .starts_with("deserialize")
    );
    let m = json!({"plugins": [{"name": NAME, "abi": ABI, "file": "l.cwasm", "sha256": "00"}]});
    std::fs::write(d.join("plugins.json"), m.to_string()).unwrap();
    assert!(Component::load(&d).err().unwrap().contains("sha256"));
}

// ── with components built for this machine ──────────────────────────────────

fn built(var: &str) -> Option<Component> {
    let Some(dir) = std::env::var_os(var) else {
        assert!(
            std::env::var_os("LUYAZ_REQUIRE_PLUGINS").is_none(),
            "LUYAZ_REQUIRE_PLUGINS set without {var}"
        );
        eprintln!("no {var}: lanid component tests not run");
        return None;
    };
    Some(Component::load(Path::new(&dir)).unwrap_or_else(|e| panic!("{var}: {e}")))
}

#[test]
fn component_parses_the_fixtures() {
    let Some(c) = built("LUYAZ_TEST_PLUGINS") else {
        return;
    };
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
    let out = c.parse(&frames).unwrap();
    let kinds: Vec<Vec<Kind>> = out
        .iter()
        .map(|v| v.iter().map(|f| f.kind).collect())
        .collect();
    assert_eq!(
        kinds,
        [
            vec![Kind::DhcpHostname, Kind::DhcpVendorClass, Kind::DhcpParams],
            vec![Kind::MdnsInstance, Kind::MdnsModel, Kind::MdnsService],
            vec![Kind::SsdpServer],
            vec![Kind::NetbiosName],
        ]
    );
    assert_eq!(out[1][0].value, "Test Laptop");
    // the whole path: learned and merged
    let mut g = Gate::default();
    let mut s = Shared::open(&tmp("comp"), false);
    s.loaded = true;
    let (learned, n) = g.run(&c, frames, Instant::now());
    assert_eq!((n.traps, n.invalid, n.facts), (0, 0, 8));
    for (m, f) in &learned {
        s.learn(m, f, 1);
    }
    let mut cl = client(&mac_str(&MAC_B), &mac_str(&MAC_B));
    s.enrich(&mut cl);
    assert_eq!(
        (cl.name.as_str(), cl.vendor.as_str(), cl.kind.as_str()),
        ("Test Laptop", "Apple", "computer")
    );
}

#[test]
fn component_survives_malformed_frames() {
    let Some(c) = built("LUYAZ_TEST_PLUGINS") else {
        return;
    };
    let good = mdns_laptop(MAC_B, IP_A, "Test Laptop", "Mac14,2");
    let mut frames = vec![
        mdns_pointer_loop(MAC_B, IP_A),
        mdns_oversize(MAC_B, IP_A),
        vec![0u8; 14],
        good[..good.len() / 2].to_vec(),
        vec![0xffu8; 1514],
    ];
    for i in (14..good.len()).step_by(7) {
        let mut g = good.clone();
        g[i] = g[i].wrapping_add(0x41);
        frames.push(g);
    }
    for chunk in frames.chunks(QUEUE) {
        let out = c.parse(chunk).expect("the parser never traps");
        assert_eq!(out.len(), chunk.len());
        assert!(out[0].is_empty() && out[1].is_empty());
        assert!(out.iter().all(|v| v.len() <= MAX_FACTS));
    }
}

#[test]
fn hostile_component_traps_only_cost_their_frames() {
    let Some(c) = built("LUYAZ_TEST_HOSTILE") else {
        return;
    };
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
    let (out, n) = g.run(&c, frames, t);
    let who: Vec<Mac> = out.iter().map(|(m, _)| *m).collect();
    assert_eq!(who, [mac(1), mac(6)]);
    assert_eq!(out[0].1[0].value, "plain");
    // the batch, then spin / mem / panic on their own
    assert_eq!((n.traps, n.bad_frames, n.invalid), (4, 3, 1));
    assert_eq!(g.blocked_now(t), 3);
    // fuel and the memory cap end it: well under a second per instance here
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    // the component is still there for the next batch
    let (out, n) = g.run(&c, vec![frame(mac(7), b"fine")], t);
    assert_eq!((out.len(), n.traps), (1, 0));
}

#[test]
fn ids_are_not_names() {
    use super::classify::readable_name;
    assert!(!readable_name("12FE7FED-D51B-46ED-94F7-01E5ADF04520"));
    assert!(!readable_name("a4cf12b3c4d5"));
    assert!(!readable_name("A4:CF:12:00:00:D5"));
    assert!(!readable_name("  "));
    assert!(readable_name("Test-MBP"));
    assert!(readable_name("living-room-tv"));
    assert!(readable_name("cafe"));
}

// ── watch.rs: alias takeover and strangers ──────────────────────────────────

fn on(d: &Path) -> Shared {
    let mut s = Shared::open(d, true);
    s.started = true;
    s.loaded = true;
    s
}

fn private(n: u8) -> String {
    mac_str(&[0x02, 0x00, 0x5e, 0x00, 0x53, n])
}

fn rec(facts: &[(Kind, &str)]) -> Record {
    let wall = crate::qmi_nas::now();
    Record {
        first: wall,
        last: wall,
        facts: facts.iter().map(|(k, v)| (*k, v.to_string())).collect(),
        ..Default::default()
    }
}

const PHONE: &[(Kind, &str)] = &[
    (Kind::DhcpHostname, "Sams-iPhone"),
    (Kind::DhcpParams, "1,121,3,6,15,108,114,119,252"),
];

#[test]
fn same_device_rules() {
    assert!(watch::same_device(&rec(PHONE), &rec(PHONE)));
    let host_only = rec(&[(Kind::DhcpHostname, "Sams-iPhone")]);
    assert!(!watch::same_device(&host_only, &host_only));
    let other_params = rec(&[
        (Kind::DhcpHostname, "Sams-iPhone"),
        (Kind::DhcpParams, "1,3,6"),
    ]);
    assert!(!watch::same_device(&rec(PHONE), &other_params));
    let generic = rec(&[(Kind::DhcpHostname, "iPhone"), (Kind::DhcpParams, "1,3,6")]);
    assert!(!watch::same_device(&generic, &generic));
    let mdns = rec(&[(Kind::MdnsInstance, "Sam's MacBook Air")]);
    assert!(watch::same_device(&mdns, &mdns));
    let uuid = rec(&[(Kind::MdnsInstance, "0123456789abcdef0123")]);
    assert!(!watch::same_device(&uuid, &uuid));
}

#[test]
fn takeover_when_the_old_mac_is_offline_and_unique() {
    let d = tmp("takeover");
    let mut s = on(&d);
    let (old, new) = (private(1), private(2));
    s.records.insert(old.clone(), rec(PHONE));
    s.records.insert(new.clone(), rec(PHONE));
    s.set_alias(&old, "Sam 的手机").unwrap();
    s.known = Some(BTreeMap::new());
    let t = Instant::now();
    // the old MAC online too: two devices, no takeover
    s.watch(&[old.clone(), new.clone()], t, 100);
    assert!(!s.aliases.contains_key(&new));
    s.watch(std::slice::from_ref(&new), t, 100);
    assert_eq!(s.aliases[&new], "Sam 的手机");
    assert_eq!(s.records[&new].from, old);
    // persisted, and shown
    let back = Shared::open(&d, false);
    assert_eq!(back.aliases[&new], "Sam 的手机");
    assert_eq!(back.records[&new].from, old);
    let mut cl = client(&new, "*");
    s.enrich(&mut cl);
    assert_eq!(
        (cl.name.as_str(), cl.alias_from.as_str()),
        ("Sam 的手机", old.as_str())
    );
    // the owner renames it: no longer "taken over"
    s.set_alias(&new, "Sam").unwrap();
    assert!(s.records[&new].from.is_empty());
}

#[test]
fn no_takeover_with_two_candidates_or_a_global_mac() {
    let d = tmp("takeover2");
    let mut s = on(&d);
    for n in [1, 2, 3] {
        s.records.insert(private(n), rec(PHONE));
    }
    s.set_alias(&private(1), "A").unwrap();
    s.set_alias(&private(2), "B").unwrap();
    s.known = Some(BTreeMap::new());
    s.watch(&[private(3)], Instant::now(), 100);
    assert!(!s.aliases.contains_key(&private(3)));
    let global = mac_str(&mac(9));
    s.records.insert(global.clone(), rec(PHONE));
    s.watch(std::slice::from_ref(&global), Instant::now(), 100);
    assert!(!s.aliases.contains_key(&global));
}

#[test]
fn strangers_seed_settle_once() {
    let d = tmp("strangers");
    let mut s = on(&d);
    let (a, b, c) = (private(1), private(2), private(3));
    let t = Instant::now();
    // first round: everything present is known, no alert
    assert!(s.watch(&[a.clone(), b.clone()], t, 100).is_empty());
    assert!(s.known.as_ref().unwrap().contains_key(&b));
    // a new MAC waits SETTLE, then one alert
    assert!(s.watch(&[a.clone(), c.clone()], t, 100).is_empty());
    assert!(
        s.watch(std::slice::from_ref(&c), t + Duration::from_secs(30), 100)
            .is_empty()
    );
    assert_eq!(
        s.watch(std::slice::from_ref(&c), t + watch::SETTLE, 190),
        vec![c.clone()]
    );
    assert!(
        s.watch(std::slice::from_ref(&c), t + watch::SETTLE * 2, 280)
            .is_empty()
    );
    // known survives a restart
    let mut again = on(&d);
    assert!(
        again
            .watch(std::slice::from_ref(&c), t + watch::SETTLE * 3, 400)
            .is_empty()
    );
    assert!(again.pending.is_empty());
}

#[test]
fn stranger_that_left_before_settling_is_forgotten() {
    let d = tmp("strangers-left");
    let mut s = on(&d);
    let t = Instant::now();
    s.watch(&[], t, 100);
    s.watch(&[private(5)], t, 100);
    s.watch(&[], t + Duration::from_secs(10), 110);
    assert!(s.watch(&[private(5)], t + watch::SETTLE, 200).is_empty());
}

#[test]
fn rotated_mac_with_late_facts_takes_over_instead_of_alerting() {
    let d = tmp("late-facts");
    let mut s = on(&d);
    let (old, new) = (private(1), private(2));
    s.records.insert(old.clone(), rec(PHONE));
    s.set_alias(&old, "Sam").unwrap();
    let t = Instant::now();
    s.watch(&[], t, 100);
    // the lease shows up first, the DHCP facts a bit later
    assert!(s.watch(std::slice::from_ref(&new), t, 100).is_empty());
    s.learn(&parse_mac(&new).unwrap(), &[], 110);
    s.records.insert(new.clone(), rec(PHONE));
    assert!(
        s.watch(std::slice::from_ref(&new), t + watch::SETTLE, 200)
            .is_empty()
    );
    assert_eq!(s.aliases[&new], "Sam");
}

#[test]
fn watch_is_off_on_a_side_core_or_without_the_component() {
    let d = tmp("watch-off");
    let mut side = Shared::open(&d, false);
    side.started = true;
    side.loaded = true;
    side.watch(&[private(1)], Instant::now(), 100);
    assert!(side.known.is_none());
    let mut unloaded = on(&d);
    unloaded.loaded = false;
    unloaded.watch(&[private(1)], Instant::now(), 100);
    assert!(unloaded.known.is_none());
    assert!(!d.join("clients.json").exists());
}

#[test]
fn alert_ids_and_text() {
    let m = [0x02, 0xab, 0xcd, 0xef, 0xff, 0xff];
    let id = watch::event_id(&m);
    assert_eq!(id, "02abcdef-65535");
    assert!(luyaz_api::alerts::valid_event_id(&id));
    assert!(luyaz_api::alerts::valid_kind(watch::KIND));
    let mac = mac_str(&m);
    assert_eq!(
        watch::alert_text(&mac, "Sams-iPhone"),
        format!("Sams-iPhone {mac}")
    );
    assert_eq!(watch::alert_text(&mac, "小米"), mac);
    assert_eq!(watch::alert_text(&mac, &mac), mac);
}
