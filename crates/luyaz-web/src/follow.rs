//! Following one u60d transaction from a page's write area (batch 3: cell lock, APN, network
//! mode), the way /bandlock does it (bandlock.rs `set`): after the send, watch the `op` block for
//! this `op_id`, put u60d's `say`/`next` into the area while it runs, and end with u60d's own words.
//! A plain write (u60d answered without an `op`: an APN change it judged not to need a transaction,
//! or an older u60d) has no transaction to follow: the page's block readback decides.

use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::Sender;

use crate::changes::Seen;
use crate::hub::St;
use crate::i18n::Lang;
use crate::pages::{Shared, patch};
use crate::write;

/// A transaction running in u60d now whose item starts with one of `prefixes`: its words.
pub fn running(st: &St, prefixes: &[&str], l: Lang) -> Option<String> {
    let a = &st.blocks.get("op")?.data["active"];
    let item = a["item"].as_str()?;
    if !prefixes.iter().any(|p| item.starts_with(p)) {
        return None;
    }
    Some(crate::bandlock::progress(a, l))
}

/// How the follow ended.
pub struct Followed {
    pub seen: Seen,
    /// The page's readback holds at the end.
    pub read: bool,
}

/// Follow `op_id` until it ends, the readback holds with no transaction seen (after `quiet`), or
/// `window` passes. `pending` renders the area with a progress line. None: the tab went away.
#[allow(clippy::too_many_arguments)]
pub async fn follow(
    app: &Shared,
    tx: &Sender<axum::response::sse::Event>,
    op_id: &str,
    mut changes: tokio::sync::broadcast::Receiver<crate::hub::Change>,
    window: Duration,
    quiet: Duration,
    l: Lang,
    pending: impl Fn(String) -> String,
    read: impl Fn(&St) -> bool,
) -> Option<Followed> {
    let t0 = tokio::time::Instant::now();
    let mut deadline = t0 + window;
    let mut last = Seen::Nothing;
    let mut receipted = false;
    let mut said = String::new();
    loop {
        match app.hub.read(|st| crate::changes::seen(st, op_id)) {
            f @ Seen::Final(_) => {
                last = f;
                break;
            }
            Seen::Live(v) => {
                // O3: the AP is up and the core waits for this page. Reaching the browser and
                // getting its POST back is the receipt; a cut-off page never sends it.
                if !receipted && v["receipt"]["waiting"].as_bool() == Some(true) {
                    receipted = true;
                    deadline = deadline.max(tokio::time::Instant::now() + RECEIPT_FOLLOW);
                    if tx.send(receipt_patch(op_id)).await.is_err() {
                        return None;
                    }
                }
                let now = crate::bandlock::progress(&v, l);
                if now != said {
                    said = now.clone();
                    if tx.send(patch(&pending(now))).await.is_err() {
                        return None;
                    }
                }
                last = Seen::Live(v);
            }
            Seen::Nothing => {
                if matches!(last, Seen::Nothing) && t0.elapsed() >= quiet && app.hub.read(&read) {
                    break;
                }
            }
        }
        let wake = (tokio::time::Instant::now() + Duration::from_secs(1)).min(deadline);
        tokio::select! {
            r = tokio::time::timeout_at(wake, changes.recv()) => match r {
                Ok(Ok(_)) | Ok(Err(RecvError::Lagged(_))) | Err(_) => {}
                Ok(Err(RecvError::Closed)) => break,
            },
            _ = tx.closed() => return None,
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    let read = app.hub.read(&read);
    Some(Followed { seen: last, read })
}

/// O3: say in the confirm that a change from off the LAN waits for this page and reverts without it.
pub fn receipt_note(cf: &mut write::Confirm, via: crate::topbar::Via, l: Lang) {
    if via == crate::topbar::Via::Local {
        return;
    }
    let note = l.pick(
        "You are not on the device's LAN: once Wi-Fi is back up, this page reports in by itself. If it cannot within 60 s, the device puts the old settings back.",
        "你不在设备的局域网里：Wi-Fi 起来后，这个页面会自动回报；60 秒内回报不了，设备会自己改回原来的设置。",
    );
    cf.recovery = Some(match cf.recovery.take() {
        Some(r) => format!("{r} {note}"),
        None => note.to_string(),
    });
}

/// §2 `remote.wifi_change`: from off the LAN, say whether this change may cut the page off for
/// good. Advice only: the write stays allowed (the receipt still reverts it), and a tailnet page
/// rides the mobile link, so only a subnet route or an unknown origin gets a line.
pub fn remote_risk_note(
    cf: &mut write::Confirm,
    via: crate::topbar::Via,
    change: luyaz_api::execution::wifi_change::Change,
    clients: Option<u32>,
    l: Lang,
) {
    use crate::topbar::Via;
    use luyaz_api::execution::wifi_change::{Advice, Evidence, Origin, replay};
    let origin = match via {
        Via::Local | Via::Tailnet => return,
        Via::SubnetRoute => Some(Origin::Remote),
        Via::Unknown => None,
    };
    let e = Evidence {
        change: Some(change),
        origin,
        clients_on_ap: clients,
        // a subnet route's other ways in are not visible from here
        other_entry: None,
        // the core reads the old values before it writes
        old_values: Some(true),
        // O3 has not been checked on the device yet (§4.5)
        revert_measured: Some(false),
    };
    let note = match replay(&e) {
        Advice::OnSiteOnly {
            why: "origin_unknown_revert_unmeasured",
        } => l.pick(
            "Where this page comes from cannot be told, and reverting from afar has not been tested on this device yet: if you are not on site, this change may lose the device for good.",
            "看不出这个页面从哪里连过来，远程退回也还没在这台设备上实测过：如果你不在现场，改完可能再也连不上设备。",
        ),
        Advice::OnSiteOnly { .. } => l.pick(
            "This page reaches the device through another device's route. Reverting from afar has not been tested on this device yet: if you can, make this change on site.",
            "这个页面是经别的设备的路由连到设备的。远程退回还没在这台设备上实测过：能的话，到现场再改。",
        ),
        Advice::Unknown { .. } => l.pick(
            "Where this page comes from cannot be told, so it is treated as remote: it may lose the device until the change is reverted.",
            "看不出这个页面从哪里连过来，按远程处理：改完可能连不上设备，直到设置被改回。",
        ),
        _ => return,
    };
    cf.recovery = Some(match cf.recovery.take() {
        Some(r) => format!("{r} {note}"),
        None => note.to_string(),
    });
}

/// O3: how much longer to follow once the core waits for a receipt (its 60 s and a margin).
const RECEIPT_FOLLOW: Duration = Duration::from_secs(70);

/// An invisible element that POSTs the receipt as soon as the browser gets it.
fn receipt_patch(op_id: &str) -> axum::response::sse::Event {
    let op: String = op_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect();
    axum::response::sse::Event::default()
        .event("datastar-patch-elements")
        .data(format!(
            "selector body\nmode append\nelements <span hidden data-init=\"@post('/router/wifi/receipt?op={op}')\"></span>"
        ))
}

/// The core reads `op.*` targets from `params`, not the top level.
fn receipt_body(op: &str) -> serde_json::Value {
    serde_json::json!({"action": "op.receipt", "params": {"op_id": op}, "source": "web"})
}

#[derive(serde::Deserialize)]
pub struct ReceiptQuery {
    op: String,
}

/// O3: the page that made a remote Wi-Fi change is back. Pass it on; nothing to show.
pub async fn receipt(
    axum::extract::State(app): axum::extract::State<Shared>,
    axum::extract::Query(q): axum::extract::Query<ReceiptQuery>,
) -> axum::http::StatusCode {
    match app.http.post(&app.control_url).json(&receipt_body(&q.op)).send().await {
        Ok(r) if r.status().is_success() => axum::http::StatusCode::NO_CONTENT,
        _ => axum::http::StatusCode::CONFLICT,
    }
}

/// The page's words for a followed write: u60d's for a finished transaction; `applied` when the
/// readback shows it and no transaction was seen.
pub fn outcome(f: &Followed, sent: write::Sent, applied: String, l: Lang) -> write::Outcome {
    match &f.seen {
        Seen::Final(_) => crate::changes::outcome(&f.seen, sent, l),
        Seen::Live(v) => {
            let say = crate::changes::pick(v, "say", l);
            write::Outcome::AcceptedSaid(match l {
                Lang::En => format!("Accepted by the device · {say}; not confirmed yet"),
                Lang::Zh => format!("设备已接受 · {say}，还没确认"),
            })
        }
        Seen::Nothing if f.read => write::Outcome::Applied(applied),
        Seen::Nothing => sent.not_read(),
    }
}


#[cfg(test)]
mod receipt_tests {
    use super::*;
    use crate::topbar::Via;

    fn cf(recovery: Option<&str>) -> write::Confirm {
        write::Confirm {
            tier: write::Tier::Dialog,
            title: String::new(),
            what: String::new(),
            downtime: None,
            recovery: recovery.map(str::to_string),
            action: String::new(),
            cuts_uplink: false,
            word: None,
            danger: false,
            revert: write::Revert::No,
        }
    }

    #[test]
    fn the_receipt_element_posts_back_for_this_op_only() {
        let e = format!("{:?}", receipt_patch("w-12.a_b\"<x>"));
        assert!(e.contains("mode append"), "{e}");
        assert!(e.contains("/router/wifi/receipt?op=w-12.a_bx"), "{e}");
        assert!(!e.contains("<x>"), "{e}");
    }

    #[test]
    fn remote_risk_only_for_subnet_routes_and_unknown_origins() {
        use luyaz_api::execution::wifi_change::Change;
        for via in [Via::Local, Via::Tailnet] {
            let mut c = cf(None);
            remote_risk_note(&mut c, via, Change::OnOff, Some(2), Lang::En);
            assert_eq!(c.recovery, None, "{via:?}");
        }
        let mut c = cf(Some("Reconnect."));
        remote_risk_note(
            &mut c,
            Via::SubnetRoute,
            Change::Settings,
            Some(2),
            Lang::En,
        );
        let r = c.recovery.unwrap();
        assert!(r.starts_with("Reconnect. ") && r.contains("on site"), "{r}");
        // the guest page reads no client count: still the on-site line
        let mut c = cf(None);
        remote_risk_note(&mut c, Via::SubnetRoute, Change::OnOff, None, Lang::En);
        assert!(c.recovery.unwrap().contains("another device's route"));
        // unknown origin, nothing measured: its own on-site line
        let mut c = cf(None);
        remote_risk_note(&mut c, Via::Unknown, Change::OnOff, Some(0), Lang::En);
        let r = c.recovery.unwrap();
        assert!(r.contains("cannot be told") && r.contains("not on site"), "{r}");
    }

    #[test]
    fn the_receipt_names_the_op_where_the_core_reads_it() {
        assert_eq!(receipt_body("w-1")["params"]["op_id"], "w-1");
    }

    #[test]
    fn only_off_lan_confirms_mention_the_receipt() {
        for via in [Via::Tailnet, Via::SubnetRoute, Via::Unknown] {
            let mut c = cf(Some("Reconnect."));
            receipt_note(&mut c, via, Lang::En);
            let r = c.recovery.unwrap();
            assert!(r.starts_with("Reconnect. ") && r.contains("60 s"), "{r}");
        }
        let mut c = cf(None);
        receipt_note(&mut c, Via::Local, Lang::Zh);
        assert_eq!(c.recovery, None);
        receipt_note(&mut c, Via::Tailnet, Lang::Zh);
        assert!(c.recovery.unwrap().contains("60 秒"));
    }
}
