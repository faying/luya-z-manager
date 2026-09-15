use serde_json::{json, Value};

use crate::at_cmd;
use crate::handlers::AppState;
use crate::ubus;

/// GET /api/network/qos
///
/// Best-effort QoS / QCI surfacing. ZTE firmware does not expose 5QI/QCI directly
/// via ubus or AT (`+CGEQOS?` / `+CGEQOSRDP` come back empty for the default
/// bearer). We do what we can:
/// 1. List PDP contexts via `AT+CGCONTRDP` (cid, bearer_id, APN, addresses).
/// 2. Probe `AT+CGEQOSRDP=<cid>` per context — populated only for non-default
///    bearers (e.g. an active VoNR call brings up a 5QI=1 dedicated bearer).
/// 3. Infer 5QI heuristically from APN: IMS-bearing APN → 5 (signaling),
///    everything else → 9 (default best-effort internet). The inferred field is
///    explicitly labeled so callers don't confuse it with a measured value.
pub fn network_qos(state: &AppState) -> (u16, Value) {
    let cgcontrdp = match at_cmd::send(&state.at_port, "AT+CGCONTRDP", 5) {
        Ok(s) => s,
        Err(e) => return (503, json!({"ok": false, "error": format!("CGCONTRDP failed: {e}")})),
    };

    let mut contexts: Vec<Value> = Vec::new();
    for line in cgcontrdp.lines() {
        let line = line.trim();
        if !line.starts_with("+CGCONTRDP:") {
            continue;
        }
        let rest = line["+CGCONTRDP:".len()..].trim();
        let (cid, bearer_id, apn) = match parse_cgcontrdp_head(rest) {
            Some(v) => v,
            None => continue,
        };

        let qci_inferred = infer_5qi(&apn);

        // The firmware tracks the real default-bearer QCI per context in a runtime
        // uci section (`zwrt_data_tmp.wwaniface<cid>.qci`) — this is exactly what
        // the device's "About" screen shows (e.g. 6 for CMCC). Authoritative when
        // present, unlike the APN heuristic.
        let qci_device: Option<u32> = ubus::uci_get(&format!("zwrt_data_tmp.wwaniface{cid}.qci"))
            .ok()
            .and_then(|s| s.trim().parse().ok());

        // Per-context QoS probe. Empty (no `+CGEQOSRDP:` line) means default bearer.
        let mut qci: Option<u32> = None;
        let mut dl_gbr: Option<u32> = None;
        let mut ul_gbr: Option<u32> = None;
        let mut dl_mbr: Option<u32> = None;
        let mut ul_mbr: Option<u32> = None;
        if let Ok(resp) = at_cmd::send(&state.at_port, &format!("AT+CGEQOSRDP={cid}"), 3) {
            for ln in resp.lines() {
                let ln = ln.trim();
                if let Some(rest) = ln.strip_prefix("+CGEQOSRDP:") {
                    let parts: Vec<&str> = rest.split(',').map(|s| s.trim()).collect();
                    // <cid>,<QCI>,<DL_GBR>,<UL_GBR>,<DL_MBR>,<UL_MBR>
                    if parts.len() >= 2 {
                        qci = parts[1].parse().ok();
                    }
                    if parts.len() >= 6 {
                        dl_gbr = parts[2].parse().ok();
                        ul_gbr = parts[3].parse().ok();
                        dl_mbr = parts[4].parse().ok();
                        ul_mbr = parts[5].parse().ok();
                    }
                    break;
                }
            }
        }

        contexts.push(json!({
            "cid": cid,
            "bearer_id": bearer_id,
            "apn": apn,
            "qci": qci,
            "qci_device": qci_device,
            "qci_inferred": qci_inferred,
            "dl_gbr_kbps": dl_gbr,
            "ul_gbr_kbps": ul_gbr,
            "dl_mbr_kbps": dl_mbr,
            "ul_mbr_kbps": ul_mbr,
        }));
    }

    (
        200,
        json!({
            "ok": true,
            "data": {
                "contexts": contexts,
                "raw_cgcontrdp": cgcontrdp.trim(),
                "note": "QCI is read from the modem (qci_device, same value as the device's About screen) when available; dedicated bearers may also report a measured 5QI via AT; otherwise it is inferred from the APN.",
            }
        }),
    )
}

/// Parse the head of a CGCONTRDP record into (cid, bearer_id, apn).
/// Format: `<cid>,<bearer_id>,"<apn>",...`
fn parse_cgcontrdp_head(s: &str) -> Option<(u32, u32, String)> {
    let mut it = s.splitn(3, ',');
    let cid: u32 = it.next()?.trim().parse().ok()?;
    let bearer_id: u32 = it.next()?.trim().parse().ok()?;
    let tail = it.next()?.trim_start();
    // Pull the first quoted token as APN; fall back to first comma-separated.
    let apn = if let Some(rest) = tail.strip_prefix('"') {
        rest.split('"').next().unwrap_or("").to_string()
    } else {
        tail.split(',').next().unwrap_or("").trim().to_string()
    };
    Some((cid, bearer_id, apn))
}

fn infer_5qi(apn: &str) -> u8 {
    let lower = apn.to_ascii_lowercase();
    if lower.contains("ims") {
        5 // IMS signaling
    } else {
        9 // default best-effort
    }
}
