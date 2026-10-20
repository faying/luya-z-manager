use super::*;

/// A remote on/off of an AP with two clients, everything read, revert measured.
fn base() -> Evidence {
    Evidence {
        change: Some(Change::OnOff),
        origin: Some(Origin::Remote),
        clients_on_ap: Some(2),
        other_entry: Some(true),
        old_values: Some(true),
        revert_measured: Some(true),
    }
}

#[test]
fn the_evidence_matrix() {
    type Edit = fn(&mut Evidence);
    let cases: &[(&str, Edit, Advice)] = &[
        (
            "remote trial",
            |_| {},
            Advice::RemoteTrial { clients: Some(2) },
        ),
        (
            "change unknown",
            |e| e.change = None,
            Advice::Unknown {
                why: "change_unknown",
            },
        ),
        (
            "clients unread, remote",
            |e| e.clients_on_ap = None,
            Advice::RemoteTrial { clients: None },
        ),
        (
            "clients unread, remote, unmeasured",
            |e| {
                e.clients_on_ap = None;
                e.revert_measured = Some(false);
            },
            Advice::OnSiteOnly {
                why: "revert_unmeasured",
            },
        ),
        (
            "clients unread, lan",
            |e| {
                e.clients_on_ap = None;
                e.origin = Some(Origin::LanOther);
            },
            Advice::Unknown {
                why: "clients_unknown",
            },
        ),
        (
            "no old values",
            |e| e.old_values = None,
            Advice::OnSiteOnly {
                why: "no_old_values",
            },
        ),
        (
            "revert unmeasured",
            |e| e.revert_measured = Some(false),
            Advice::OnSiteOnly {
                why: "revert_unmeasured",
            },
        ),
        (
            "revert unread",
            |e| e.revert_measured = None,
            Advice::OnSiteOnly {
                why: "revert_unmeasured",
            },
        ),
        (
            "settings, no other way in",
            |e| {
                e.change = Some(Change::Settings);
                e.other_entry = Some(false);
            },
            Advice::OnSiteOnly {
                why: "no_other_entry",
            },
        ),
        (
            "settings, other entry unread",
            |e| {
                e.change = Some(Change::Settings);
                e.other_entry = None;
            },
            Advice::OnSiteOnly {
                why: "no_other_entry",
            },
        ),
        (
            "settings with another way in",
            |e| e.change = Some(Change::Settings),
            Advice::RemoteTrial { clients: Some(2) },
        ),
        (
            "origin unknown",
            |e| e.origin = None,
            Advice::Unknown {
                why: "origin_unknown",
            },
        ),
        (
            "origin unknown, unmeasured",
            |e| {
                e.origin = None;
                e.revert_measured = None;
            },
            Advice::OnSiteOnly {
                why: "origin_unknown_revert_unmeasured",
            },
        ),
        (
            "lan elsewhere, empty ap",
            |e| {
                e.origin = Some(Origin::LanOther);
                e.clients_on_ap = Some(0);
            },
            Advice::Low,
        ),
        (
            "lan elsewhere, clients",
            |e| e.origin = Some(Origin::LanOther),
            Advice::Clients {
                clients: 2,
                requester_drops: false,
            },
        ),
        (
            "lan on the ap, count missed the requester",
            |e| {
                e.origin = Some(Origin::LanSameAp);
                e.clients_on_ap = Some(0);
            },
            Advice::Clients {
                clients: 1,
                requester_drops: true,
            },
        ),
    ];
    for (name, edit, want) in cases {
        let mut e = base();
        edit(&mut e);
        assert_eq!(&replay(&e), want, "{name}");
    }
}

#[test]
fn never_acts_and_receipt_follows_remote_risk() {
    let a = replay(&base());
    assert_eq!(a.action(), None);
    assert!(a.needs_receipt());
    let mut e = base();
    e.origin = None;
    assert!(replay(&e).needs_receipt());
    e.origin = Some(Origin::LanOther);
    assert!(!replay(&e).needs_receipt());
}

#[test]
fn serializes_tagged() {
    let v = serde_json::to_value(replay(&base())).unwrap();
    assert_eq!(v["risk"], "remote_trial");
}
