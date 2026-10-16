//! `plugc <component.wasm> <out.cwasm> <name> <abi> [target]`: precompiles a component with u60d's
//! Config (default target aarch64-unknown-linux-musl, the device) and prints its manifest entry
//! (`plugins.json`, read by crates/luyaz-core/src/plugin.rs). The wasmtime version is in the .cwasm
//! itself; u60d's deserialize refuses one made by another version.

#[path = "../../../luyaz-core/src/plugin/config.rs"]
mod config;

use sha2::{Digest, Sha256};

fn main() -> wasmtime::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("usage: plugc <component.wasm> <out.cwasm> <name> <abi> [target]");
        std::process::exit(2);
    }
    let (input, output, name, abi) = (&a[1], &a[2], &a[3], &a[4]);
    let target = a
        .get(5)
        .map(String::as_str)
        .unwrap_or("aarch64-unknown-linux-musl");
    let mut cfg = config::config();
    cfg.target(target)?;
    let engine = wasmtime::Engine::new(&cfg)?;
    let out = engine.precompile_component(&std::fs::read(input)?)?;
    std::fs::write(output, &out)?;
    let sha: String = Sha256::digest(&out)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let file = std::path::Path::new(output)
        .file_name()
        .unwrap()
        .to_string_lossy();
    println!("{{\"name\":\"{name}\",\"abi\":\"{abi}\",\"file\":\"{file}\",\"sha256\":\"{sha}\"}}");
    Ok(())
}
