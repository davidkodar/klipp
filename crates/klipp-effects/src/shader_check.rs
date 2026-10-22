//! GPU shader modules that would not compile (#2344).
//!
//! A module whose shaders fail leaves its pipelines out: its work falls
//! back to the CPU or declines, and the GPU tests take the missing
//! pipeline for a machine without a GPU and skip. So a failure is said
//! once per module, loudly, and kept for the perf line, and in a debug
//! build (every test run) it panics: a shader that does not compile is
//! a bug, never a machine without a GPU.

use std::sync::Mutex;

static FAILED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Record that `module`'s shaders did not compile, with the error.
/// Panics in a debug build.
pub fn shader_failed(module: &str, error: &str) {
    let first = {
        let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
        let first = !failed.iter().any(|m| m == module);
        if first {
            failed.push(module.to_owned());
        }
        first
    };
    if first {
        tracing::error!("GPU {module}: shader compilation failed, its work runs without the GPU: {error}");
    }
    if cfg!(debug_assertions) {
        panic!("GPU {module}: shader compilation failed: {error}");
    }
}

/// Parse and validate a WGSL module as the device would, with no GPU:
/// the first error, rendered against the source, or `Ok`. Every crate's
/// tests run their shaders through this, so a shader that does not
/// compile fails on any machine.
pub fn validate(source: &str) -> Result<(), String> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map(|_| ())
        .map_err(|e| e.emit_to_string(source))
}

/// The modules whose shaders did not compile, in the order they failed.
pub fn failed_shader_modules() -> Vec<String> {
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
