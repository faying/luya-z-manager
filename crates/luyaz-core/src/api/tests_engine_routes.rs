//! Every path the engines send through u60d's own routes is a route (Phase 10 X4, review D12):
//! the scenario actions (`ALLOWED_PATHS`), their readbacks (`verify_job`, `verify_field`) and the
//! actions in the scenario file stored on the owner's device (read 2026-10-14). A path that is
//! not served answers 404 `not_in_compat_yet` (a known path with another method too), which the
//! engines record as a failed or unsupported step.
//!
//! The body is not JSON, so a handler that reads one stops before doing anything.

use super::tests::{call, start};
use super::*;

/// `/api/wifi/radio` never goes over HTTP: the scenario engine applies it in process
/// (`luyaz_scenario::Engine::run_action`).
const IN_PROCESS: &[&str] = &["/api/wifi/radio"];

/// The method each scenario action is sent with.
fn method(path: &str) -> &'static str {
    match path {
        "/api/wifi/guest" | "/api/device/power-save" => "PUT",
        p => panic!("{p} is in ALLOWED_PATHS: add its method here"),
    }
}

/// Readbacks and the stored scenarios' actions (method, path).
const OTHER: &[(&str, &str)] = &[
];

#[tokio::test]
async fn every_path_the_engines_send_is_a_route() {
    let root = std::env::temp_dir().join(format!("u60d-compat-engine-routes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let addr = start(Compat::new(root.to_str().unwrap(), Sources::none()).with_writes(true)).await;
    let mut sent = Vec::new();
    for p in luyaz_policy::scenario::ALLOWED_PATHS {
        if !IN_PROCESS.contains(p) {
            sent.push((method(p), *p));
        }
    }
    sent.extend_from_slice(OTHER);
    for (m, p) in sent {
        let (st, b) = call(addr, m, p, None, "{").await;
        assert!(
            !(st == 404 && b["error"] == "not_in_compat_yet"),
            "{m} {p} is not a route: {b}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
