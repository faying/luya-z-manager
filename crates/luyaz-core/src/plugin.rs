//! Phase 4: the policy host (u60-platform.md §1, Phase 4 step 1).
//!
//! A policy is plain Rust in `u60-policy`, built into u60d as the built-in default. A wasm
//! component of the same policy (crates/luyaz-plugins, precompiled to `.cwasm` by `plugc`) replaces
//! the built-in when the plugin directory has one that checks out:
//!
//! - `plugins.json` lists `{name, abi, file, sha256}`; `abi` must equal the WIT package version
//!   u60d was built against, the file's sha256 must match, and wasmtime's deserialize must accept
//!   it (it refuses artifacts from another wasmtime version or Config). Loading a `.cwasm` is
//!   `unsafe` in wasmtime (it trusts the file completely), which is what the hash is for.
//! - Any failure (no manifest, mismatch, deserialize error, a trap or fuel exhaustion in a call)
//!   leaves or puts the policy on the built-in; a component that failed a call stays off until
//!   u60d restarts. The status says which one runs and why.
//! - Each call gets a fresh instance (about 5 µs), fuel worth about 10 ms on the device (D8;
//!   u60-phase0.md: 2.36 M fuel/ms), 8 MiB of linear memory. No epoch timer (it would add wakeups).
//! - Nothing here may panic: u60d's device build is `panic = "abort"`.
//!
//! Phase 10 (X2b): only the battery estimate and the charge policy are left here; the other
//! policies run in their engines (compat), and their shadows that paired u60d with the agent are
//! gone with the agent.

pub mod battery_eta;
pub mod charge;
pub mod charge_policy;
pub mod config;

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use wasmtime::{Engine, ResourceLimiter, Store};

/// Where the manifest and the `.cwasm` files are (`LUYAZ_PLUGINS`).
pub const ENV_DIR: &str = "LUYAZ_PLUGINS";
pub const DEFAULT_DIR: &str = "/data/luyaz/core/plugins";
/// About 10 ms of guest code on the device (D8: one call ≤ 10 ms).
pub const FUEL_PER_CALL: u64 = 23_600_000;

pub fn dir() -> PathBuf {
    std::env::var_os(ENV_DIR)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.into_string().ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR))
}

/// Which implementation a policy runs, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "runs", rename_all = "snake_case")]
pub enum Status {
    /// The built-in default; `why` is empty when there simply is no component for it.
    Builtin {
        why: String,
    },
    Component {
        sha256: String,
    },
}

/// Linear-memory cap per instance.
pub struct Limits;
impl ResourceLimiter for Limits {
    fn memory_growing(
        &mut self,
        _cur: usize,
        desired: usize,
        _max: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= config::MEM_LIMIT)
    }
    fn table_growing(
        &mut self,
        _cur: usize,
        desired: usize,
        _max: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 10_000)
    }
}

/// A fresh store for one call.
pub fn store(engine: &Engine) -> wasmtime::Result<Store<Limits>> {
    let mut s = Store::new(engine, Limits);
    s.limiter(|l| l);
    s.set_fuel(FUEL_PER_CALL)?;
    Ok(s)
}

pub fn engine() -> Result<Engine, String> {
    Engine::new(&config::config()).map_err(|e| format!("engine: {e}"))
}

/// The bytes of component `name` from `dir`, checked against the manifest (name, abi, sha256).
pub fn load_checked(dir: &Path, name: &str, abi: &str) -> Result<(Vec<u8>, String), String> {
    let manifest = dir.join("plugins.json");
    let text = match std::fs::read_to_string(&manifest) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(String::new()),
        Err(e) => return Err(format!("{}: {e}", manifest.display())),
    };
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("plugins.json: {e}"))?;
    let Some(entry) = v["plugins"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["name"] == name))
    else {
        return Err(String::new());
    };
    let want_abi = entry["abi"].as_str().unwrap_or_default();
    if want_abi != abi {
        return Err(format!("abi {want_abi}, the core has {abi}"));
    }
    let file = entry["file"].as_str().unwrap_or_default();
    // a bare file name in the plugin directory, nothing else
    if file.is_empty() || file.contains('/') || file.starts_with('.') {
        return Err(format!("bad file name {file:?}"));
    }
    let bytes = std::fs::read(dir.join(file)).map_err(|e| format!("{file}: {e}"))?;
    let sha = hex(&Sha256::digest(&bytes));
    if entry["sha256"].as_str() != Some(sha.as_str()) {
        return Err(format!("{file}: sha256 {sha} does not match the manifest"));
    }
    Ok((bytes, sha))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One policy's implementation: the component when one loaded, the built-in otherwise, and what
/// happened to the calls. Shared by every policy so the fallback is written (and tested) once.
pub struct Slot<C> {
    name: &'static str,
    pub component: Option<C>,
    pub status: Status,
    pub calls: u64,
    /// calls answered by the built-in after the component failed
    pub fallbacks: u64,
    pub max_call_us: u64,
}

impl<C> Slot<C> {
    /// Load component `name` from `dir` (checked against the manifest), or stay on the built-in.
    pub fn load(
        dir: &Path,
        name: &'static str,
        abi: &str,
        new: impl FnOnce(&Engine, &[u8]) -> Result<C, String>,
    ) -> Self {
        let loaded = load_checked(dir, name, abi)
            .and_then(|(bytes, sha)| engine().and_then(|e| new(&e, &bytes)).map(|c| (c, sha)));
        let (component, status) = match loaded {
            Ok((c, sha256)) => (Some(c), Status::Component { sha256 }),
            Err(why) => (None, Status::Builtin { why }),
        };
        if let Status::Builtin { why } = &status
            && !why.is_empty()
        {
            eprintln!("plugin {name}: using the built-in ({why})");
        }
        Slot {
            name,
            component,
            status,
            calls: 0,
            fallbacks: 0,
            max_call_us: 0,
        }
    }

    /// One policy call: the component's answer, or the built-in's when there is no component or
    /// the call failed. A component that failed a call stays off until u60d restarts (it only
    /// stops itself, DR5).
    pub fn run<T>(
        &mut self,
        call: impl FnOnce(&C) -> Result<T, String>,
        builtin: impl FnOnce() -> T,
    ) -> T {
        self.calls += 1;
        let Some(c) = self.component.as_ref() else {
            return builtin();
        };
        let t = std::time::Instant::now();
        let out = call(c);
        self.max_call_us = self.max_call_us.max(t.elapsed().as_micros() as u64);
        match out {
            Ok(v) => v,
            Err(why) => {
                eprintln!(
                    "plugin {}: call failed, back to the built-in ({why})",
                    self.name
                );
                self.fallbacks += 1;
                self.component = None;
                self.status = Status::Builtin {
                    why: format!("call failed: {why}"),
                };
                builtin()
            }
        }
    }

    #[cfg(test)]
    pub fn runs_component(&self) -> bool {
        matches!(self.status, Status::Component { .. })
    }

    /// The runtime part of a policy's status.
    #[cfg(test)]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "runtime": self.status,
            "calls": self.calls,
            "fallbacks": self.fallbacks,
            "max_call_us": self.max_call_us,
        })
    }
}

/// Short text for a wasmtime error (a trap's name, or the message).
pub fn short(e: &wasmtime::Error) -> String {
    let s = match e.downcast_ref::<wasmtime::Trap>() {
        Some(t) => format!("trap: {t}"),
        None => e.to_string(),
    };
    s.chars().take(120).collect()
}

/// `LUYAZ_POLICIES=0` turns the shadow policies off: no hook on the hub, nothing taken from
/// `/control`.
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("LUYAZ_POLICIES").as_deref() != Ok("0"))
}

/// Install the policies on the hub: they run after every successful read of the blocks they
/// follow (battery and charger, 5 s), so they add no timer. Serve mode only.
pub fn install(hub: &crate::block::Hub) -> bool {
    hub.set_after_read(Box::new(|hub, name| {
        match name {
            "battery" => {
                battery_eta::tick(hub);
                charge_policy::tick(hub);
            }
            "charger" => {
                battery_eta::charger_read(hub);
                charge_policy::tick(hub);
            }
            _ => {}
        }
    }))
}

#[cfg(test)]
mod tests;
