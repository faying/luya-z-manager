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
    let deadline = t0 + window;
    let mut last = Seen::Nothing;
    let mut said = String::new();
    loop {
        match app.hub.read(|st| crate::changes::seen(st, op_id)) {
            f @ Seen::Final(_) => {
                last = f;
                break;
            }
            Seen::Live(v) => {
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

