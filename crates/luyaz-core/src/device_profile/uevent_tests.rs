//! Fixtures are synthetic, shaped like kernel uevents; not new device evidence.
use super::*;

fn ev(header: &str, keys: &[&str]) -> Vec<u8> {
    let mut v = header.as_bytes().to_vec();
    v.push(0);
    for k in keys {
        v.extend_from_slice(k.as_bytes());
        v.push(0);
    }
    v
}

fn udc(action: &str, seq: u64) -> Vec<u8> {
    ev(
        &format!("{action}@/devices/platform/soc/a600000.ssusb/a600000.dwc3/udc/a600000.dwc3"),
        &[
            &format!("ACTION={action}"),
            "DEVPATH=/devices/platform/soc/a600000.ssusb/a600000.dwc3/udc/a600000.dwc3",
            "SUBSYSTEM=udc",
            &format!("SEQNUM={seq}"),
        ],
    )
}

fn other(seq: u64) -> Vec<u8> {
    ev(
        "change@/devices/virtual/thermal/thermal_zone3",
        &[
            "ACTION=change",
            "SUBSYSTEM=thermal",
            &format!("SEQNUM={seq}"),
        ],
    )
}

fn live() -> Tracker {
    let mut t = Tracker::default();
    t.snapshot();
    t
}

#[test]
fn add_change_remove_are_relevant() {
    for (i, action) in ["add", "change", "remove"].into_iter().enumerate() {
        assert_eq!(
            parse(&udc(action, 10 + i as u64)),
            Ok(Msg::Kernel {
                seqnum: 10 + i as u64,
                relevant: true
            })
        );
    }
    for s in ["android_usb", "usb", "typec", "power_supply"] {
        let m = ev("change@/x", &[&format!("SUBSYSTEM={s}"), "SEQNUM=1"]);
        assert_eq!(
            parse(&m),
            Ok(Msg::Kernel {
                seqnum: 1,
                relevant: true
            }),
            "{s}"
        );
    }
    // The ZTE hook's USB_STATE key counts whatever the subsystem.
    let m = ev(
        "change@/devices/virtual/android_usb/android0",
        &["SUBSYSTEM=misc", "USB_STATE=CONFIGURED", "SEQNUM=7"],
    );
    assert_eq!(
        parse(&m),
        Ok(Msg::Kernel {
            seqnum: 7,
            relevant: true
        })
    );
}

#[test]
fn unrelated_subsystem_is_ignored_but_advances_the_sequence() {
    let mut t = live();
    assert_eq!(t.on(parse(&other(100))), Step::Nothing);
    assert_eq!(t.on(parse(&udc("change", 101))), Step::Bump);
    assert_eq!(t.on(parse(&other(102))), Step::Nothing);
    assert_eq!(t.on(parse(&udc("change", 103))), Step::Bump);
    // A prefix of a relevant name is not relevant.
    let m = ev("change@/x", &["SUBSYSTEM=usbmisc", "SEQNUM=104"]);
    assert_eq!(t.on(parse(&m)), Step::Nothing);
}

#[test]
fn a_sequence_gap_revokes_even_on_an_unrelated_uevent() {
    let mut t = live();
    assert_eq!(t.on(parse(&other(5))), Step::Nothing);
    assert_eq!(t.on(parse(&other(7))), Step::Lost);
    let mut t = live();
    assert_eq!(t.on(parse(&udc("add", 5))), Step::Bump);
    assert_eq!(t.on(parse(&udc("remove", 9))), Step::Lost);
}

#[test]
fn a_duplicate_or_backwards_seqnum_revokes() {
    let mut t = live();
    assert_eq!(t.on(parse(&udc("change", 20))), Step::Bump);
    assert_eq!(t.on(parse(&udc("change", 20))), Step::Lost);
    let mut t = live();
    assert_eq!(t.on(parse(&other(20))), Step::Nothing);
    assert_eq!(t.on(parse(&other(19))), Step::Lost);
    let mut t = live();
    assert_eq!(t.on(parse(&other(u64::MAX))), Step::Nothing);
    assert_eq!(t.on(parse(&other(0))), Step::Lost, "no wraparound");
}

#[test]
fn truncated_oversized_and_malformed_revoke() {
    let full = udc("change", 30);
    let cut = &full[..full.len() - 5];
    assert_eq!(parse(cut), Err(()));
    let mut t = live();
    assert_eq!(t.on(parse(cut)), Step::Lost);

    let mut big = ev("change@/x", &["SUBSYSTEM=udc", "SEQNUM=1"]);
    big.splice(0..0, std::iter::repeat_n(b'a', MAX_MSG));
    assert_eq!(parse(&big), Err(()));

    for bad in [
        ev("change@/x", &["SUBSYSTEM=udc"]),        // no SEQNUM
        ev("change@/x", &["SEQNUM=abc"]),           // not a number
        ev("change@/x", &["SEQNUM=1", "SEQNUM=2"]), // two
        ev("change@/x", &["SEQNUM=1", "NOEQUALS"]), // not KEY=VALUE
        ev("no-at-sign", &["SEQNUM=1"]),            // no action@devpath
        Vec::new(),
    ] {
        assert_eq!(parse(&bad), Err(()), "{bad:?}");
    }
    let mut utf = ev("change@/x", &["SEQNUM=1"]);
    utf.splice(0..0, [0xff]);
    assert_eq!(
        parse(&utf),
        Ok(Msg::Kernel {
            seqnum: 1,
            relevant: false
        }),
        "header bytes are not interpreted"
    );
}

#[test]
fn libudev_messages_are_ignored_and_do_not_touch_the_sequence() {
    let mut m = b"libudev\0".to_vec();
    m.extend_from_slice(&[0xfe, 0xed, 0xca, 0xfe, 1, 2, 3]);
    assert_eq!(parse(&m), Ok(Msg::Libudev));
    let mut t = live();
    assert_eq!(t.on(parse(&udc("change", 40))), Step::Bump);
    assert_eq!(t.on(parse(&m)), Step::Nothing);
    assert_eq!(t.on(parse(&udc("change", 41))), Step::Bump);
}

#[test]
fn nothing_is_minted_before_the_snapshot() {
    let mut t = Tracker::default();
    assert!(!t.live);
    assert_eq!(t.on(parse(&udc("change", 50))), Step::Nothing);
    // The sequence is already tracked before the snapshot.
    t.snapshot();
    assert_eq!(t.on(parse(&udc("change", 51))), Step::Bump);
    let mut t = Tracker::default();
    assert_eq!(t.on(parse(&udc("change", 50))), Step::Nothing);
    assert_eq!(t.on(parse(&udc("change", 52))), Step::Lost);
}

#[test]
fn one_plug_cycle_mints_on_every_related_uevent() {
    // Shape of the 10-24 B31 observation: unplug then replug, mixed with unrelated uevents.
    let mut t = live();
    let mut seq = 200;
    let mut bumps = 0;
    for state in ["DISCONNECTED", "CONFIGURED"] {
        for m in [
            ev(
                "change@/devices/virtual/android_usb/android0",
                &[
                    "SUBSYSTEM=android_usb",
                    &format!("USB_STATE={state}"),
                    &format!("SEQNUM={seq}"),
                ],
            ),
            other(seq + 1),
            ev(
                "change@/devices/platform/soc/usb/power_supply/usb",
                &["SUBSYSTEM=power_supply", &format!("SEQNUM={}", seq + 2)],
            ),
        ] {
            if t.on(parse(&m)) == Step::Bump {
                bumps += 1;
            }
        }
        seq += 3;
    }
    assert_eq!(bumps, 4);
}
