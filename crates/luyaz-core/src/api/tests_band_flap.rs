use super::band_flap::*;
use luyaz_api::execution::band_flap::{Advice, WINDOW_S, replay};
use serde_json::json;

const MIN: u64 = 60_000;

fn d(at_min: u64, cell: &str, sinr: f64) -> Drop {
    Drop {
        at_ms: at_min * MIN,
        band: Some("n78".into()),
        cell: Some(cell.into()),
        sinr: Some(sinr),
        ours: Some(false),
    }
}

/// Live since minute 0, three drops on n78, one cell, good SINR; looked at minute 40.
fn flapping() -> Ring {
    let mut r = Ring::default();
    r.live(true, 0);
    for m in [12, 20, 31] {
        r.drop_seen(d(m, "7", 8.0));
    }
    r
}

#[test]
fn three_drops_on_one_band_over_a_full_window_explain() {
    let e = flapping().evidence(40 * MIN, Some(0));
    assert_eq!(e.drops, Some(3));
    assert_eq!(e.epochs_authoritative, Some(true));
    assert_eq!(e.stock_recovery, Some(false));
    assert_eq!(
        replay(&e),
        Advice::Explain {
            band: "n78".into(),
            drops: 3
        }
    );
}

#[test]
fn ipv4_and_ipv6_leaving_together_count_once() {
    let mut r = Ring::default();
    r.live(true, 0);
    r.drop_seen(d(12, "7", 8.0));
    let mut v6 = d(12, "7", 8.0);
    v6.at_ms += 4_000;
    r.drop_seen(v6);
    assert_eq!(r.evidence(40 * MIN, Some(0)).drops, Some(1));
}

#[test]
fn old_drops_leave_the_window() {
    let e = flapping().evidence(13 * MIN + WINDOW_S * 1000, Some(0));
    assert_eq!(e.drops, Some(2));
}

#[test]
fn a_young_subscription_is_not_authoritative() {
    let mut r = flapping();
    let e = r.evidence(29 * MIN, Some(0));
    assert_eq!(e.epochs_authoritative, Some(false));
    assert!(matches!(replay(&e), Advice::Unknown { .. }));
    // lost and back: the window starts again, drops gone
    r.live(false, 41 * MIN);
    assert_eq!(r.evidence(42 * MIN, Some(0)).drops, None);
    r.live(true, 42 * MIN);
    let e = r.evidence(43 * MIN, Some(0));
    assert_eq!((e.drops, e.covered_s), (Some(0), Some(60)));
}

#[test]
fn drops_before_live_are_not_counted() {
    let mut r = Ring::default();
    r.drop_seen(d(1, "7", 8.0));
    r.live(true, 2 * MIN);
    assert_eq!(r.evidence(3 * MIN, Some(0)).drops, Some(0));
}

#[test]
fn a_card_event_restarts_the_window() {
    let mut r = flapping();
    r.sim_changed(35 * MIN);
    let e = r.evidence(40 * MIN, Some(0));
    assert_eq!((e.drops, e.covered_s), (Some(0), Some(300)));
}

#[test]
fn alternatives_read_from_the_drops() {
    // crashwatch off = unknown; an SSR in the window = the stock recovery
    let mut r = flapping();
    assert_eq!(r.evidence(40 * MIN, None).stock_recovery, None);
    assert_eq!(
        r.evidence(40 * MIN, Some(20 * MIN)).stock_recovery,
        Some(true)
    );
    assert_eq!(
        r.evidence(40 * MIN, Some(5 * MIN)).stock_recovery,
        Some(false)
    );
    // one drop with weak SINR, one during our own transaction, one unread cell
    let mut r = flapping();
    r.drop_seen(Drop {
        sinr: Some(-3.0),
        ours: Some(true),
        cell: None,
        ..d(36, "7", 0.0)
    });
    let e = r.evidence(40 * MIN, Some(0));
    assert_eq!(e.weak_sinr, Some(true));
    assert_eq!(e.expected_overlap, Some(true));
    assert_eq!(e.moving, None);
    // three cells = moving; bands disagree = no band
    let mut r = Ring::default();
    r.live(true, 0);
    r.drop_seen(d(12, "1", 8.0));
    r.drop_seen(d(20, "2", 8.0));
    r.drop_seen(Drop {
        band: Some("B3".into()),
        ..d(31, "3", 8.0)
    });
    let e = r.evidence(40 * MIN, Some(0));
    assert_eq!((e.moving, e.band), (Some(true), None));
}

#[test]
fn a_drop_reads_band_cell_and_sinr_from_the_signal_block() {
    let nr = json!({"band":"B3","nr_band":"78","nr_cell_id":7,"nr_snr":"12.5","cell_id":"x","lte_snr":"1"});
    let x = drop_from(Some(&nr), Some(&json!({"active": null})), 5);
    assert_eq!(
        (x.band.as_deref(), x.cell.as_deref(), x.sinr, x.ours),
        (Some("n78"), Some("7"), Some(12.5), Some(false))
    );
    let lte = json!({"band":"B3","cell_id":"abc","lte_snr":"-2"});
    let x = drop_from(Some(&lte), Some(&json!({"active": {"item": "band"}})), 5);
    assert_eq!(
        (x.band.as_deref(), x.cell.as_deref(), x.sinr, x.ours),
        (Some("B3"), Some("abc"), Some(-2.0), Some(true))
    );
    let x = drop_from(None, None, 5);
    assert_eq!((x.band, x.cell, x.sinr, x.ours), (None, None, None, None));
}
