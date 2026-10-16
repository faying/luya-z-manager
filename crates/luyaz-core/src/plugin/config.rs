// The wasmtime Config, shared by u60d's host and the precompiler (crates/luyaz-plugins/plugc, which
// includes this file by path). Every setting here is baked into a .cwasm, so both sides must
// build it the same way; u60-phase0.md §0b measured these on the device.
// No crate items here: plugc compiles this file on its own.

/// Linear memory per instance (StoreLimits; the host's, plugc does not need it).
#[allow(dead_code)]
pub const MEM_LIMIT: usize = 8 << 20;

pub fn config() -> wasmtime::Config {
    let mut c = wasmtime::Config::new();
    c.wasm_component_model(true);
    c.consume_fuel(true);
    // the device kernel has 39-bit VA (512 GiB); don't reserve 4 GiB per instance
    c.memory_reservation(16 << 20);
    c.memory_guard_size(64 << 10);
    c.memory_reservation_for_growth(0);
    c.max_wasm_stack(256 << 10);
    // a trap in a deep stack otherwise records thousands of frames each time
    c.wasm_backtrace_max_frames(None);
    c
}
