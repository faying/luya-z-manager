//! Which ship may be related to an incident (CEO review X1/G1, eng review A1/O2, OV-C3): the
//! ship manifest's `kind:ship` lines and the transaction in flight, on the device's clocks only.
//! The moment that counts is the promote (`act_boot`/`act_uptime`, written by luyaz-ship.sh);
//! an older line without it counts only in this boot and not after now. In the same boot a ship
//! up to an hour before the start is "may be related"; across a boot the clocks cannot order them
//! ("time uncertain", never "unrelated").

use super::{At, Ship};

pub const MANIFEST: &str = "/data/luyaz/ship/manifest.jsonl";
pub const TXN: &str = "/data/luyaz/ship/txn";
pub const RELATED_S: i64 = 3600;
/// Across a boot: a ship this long before (by the Mac's clock against ours) is still named.
pub const ACROSS_S: i64 = 6 * 3600;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub comp: String,
    pub txn: String,
    /// The full boot id and BOOTTIME s of the promote (or of the manifest line when older).
    pub boot: String,
    pub up: i64,
    /// `act_*` known.
    pub act: bool,
    pub mac_time: Option<i64>,
}

fn boot8(b: &str) -> String {
    super::super::boot8(b)
}

/// The ship lines (oldest first), then the transaction in flight.
pub fn rows(manifest: &str, txn: &str) -> Vec<Row> {
    let mut v: Vec<Row> = manifest
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|j| j["kind"] == "ship")
        .filter_map(|j| {
            let s = |k: &str| j[k].as_str().map(str::to_string);
            let (boot, up, act) = match (s("act_boot"), j["act_uptime"].as_i64()) {
                (Some(b), Some(u)) => (b, u, true),
                _ => (s("boot_id")?, j["uptime"].as_i64()?, false),
            };
            Some(Row {
                comp: s("comp")?,
                txn: s("txn").unwrap_or_default(),
                boot,
                up,
                act,
                mac_time: j["mac_time"].as_i64(),
            })
        })
        .collect();
    let kv = |k: &str| {
        txn.lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
            .map(str::to_string)
    };
    if kv("phase").as_deref() != Some("done")
        && let (Some(comp), Some(b), Some(u)) = (
            kv("comp"),
            kv("act_boot"),
            kv("act_uptime").and_then(|u| u.parse().ok()),
        )
    {
        let t = kv("txn").unwrap_or_default();
        if !v.iter().any(|r| r.txn == t) {
            v.push(Row {
                comp,
                txn: t,
                boot: b,
                up: u,
                act: true,
                mac_time: None,
            });
        }
    }
    v
}

/// The ship an incident starting at `at` may be related to, and how sure the clocks are
/// (`same_boot` / `uncertain`). `now_up`: BOOTTIME now; `offset`: the device clock's lead over
/// UTC (s), for the Mac's `mac_time` across a boot.
pub fn find(rows: &[Row], at: &At, now_up: i64, offset: Option<i64>) -> Option<(Ship, String)> {
    let same = rows
        .iter()
        .filter(|r| boot8(&r.boot) == at.boot && (r.act || r.up <= now_up))
        .filter(|r| r.up <= at.up && at.up - r.up <= RELATED_S)
        .max_by_key(|r| r.up);
    if let Some(r) = same {
        return Some((
            Ship {
                comp: r.comp.clone(),
                txn: r.txn.clone(),
                ago_s: Some(at.up - r.up),
            },
            "same_boot".into(),
        ));
    }
    // the last ship was in another boot: name it early in this one, time uncertain
    let last = rows.last()?;
    if boot8(&last.boot) == at.boot || at.up > RELATED_S {
        return None;
    }
    let ago = match (at.wall_ok(), offset, last.mac_time) {
        (Some(w), Some(o), Some(m)) => Some(w - o - m),
        _ => None,
    };
    if ago.is_some_and(|a| !(0..=ACROSS_S).contains(&a)) {
        return None;
    }
    Some((
        Ship {
            comp: last.comp.clone(),
            txn: last.txn.clone(),
            ago_s: ago,
        },
        "uncertain".into(),
    ))
}
