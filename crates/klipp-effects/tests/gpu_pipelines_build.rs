//! If this machine has a GPU, every GPU pipeline must build.
//!
//! This is a canary, and it exists because of a hole the fixture audit
//! fell into by accident. Around ninety places across the test suite are
//! written like this:
//!
//! ```ignore
//! let Some(mut conv) = GpuP010Converter::new() else {
//!     eprintln!("skipping: P010 GPU converter unavailable on this system");
//!     return;
//! };
//! ```
//!
//! which is right on a machine with no GPU. But a constructor also
//! returns `None` when its **shader fails to compile** — and then the
//! test prints "skipping" and reports `ok`. Found the hard way: a
//! deliberate one-line WGSL error, introduced to check whether a test
//! could detect a bug, instead made that test pass while saying
//! "P010 GPU converter unavailable on this system" on a machine with a
//! working GPU. A broken shader can turn the entire GPU suite green.
//!
//! Hardening all ninety sites would be a lot of churn for the same
//! protection this one test gives: if a shader stops compiling, this
//! fails, whatever the rest of the suite decides to skip.
//!
//! The GPU is detected by enumerating adapters, which compiles no
//! shaders — so "an adapter exists but a pipeline will not build" is
//! unambiguous, and it is a regression rather than a platform.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_context::GpuContext;
use klipp_effects::nv12_gpu::GpuNv12Converter;
use klipp_effects::p010_gpu::GpuP010Converter;

/// Whether any wgpu adapter exists, without building a device or
/// compiling anything. This has to stay shader-free: asking a pipeline
/// whether the GPU works is the circularity this test exists to break.
fn an_adapter_exists() -> bool {
    let instance = klipp_effects::gpu_context::instance();
    let adapters = instance.enumerate_adapters(klipp_effects::gpu_context::backends());
    for a in &adapters {
        let info = a.get_info();
        eprintln!("  adapter: {} ({:?}, {:?})", info.name, info.device_type, info.backend);
    }
    !adapters.is_empty()
}

#[test]
fn every_gpu_pipeline_builds_when_an_adapter_is_present() {
    if !an_adapter_exists() {
        eprintln!(
            "skipping: no wgpu adapter on this host, so nothing can be expected to \
             build. This is the one skip in the GPU suite that is allowed to be \
             silent — every other one should be read as suspicious.",
        );
        return;
    }

    // Each of these gates a slice of the suite. The counts are how many
    // call sites currently skip on that constructor returning None, i.e.
    // how many tests would quietly pass if its shader stopped compiling.
    let mut failed: Vec<&str> = Vec::new();

    // GpuContext first: the others can share it, and it is the cheapest
    // thing that can fail.
    let ctx = GpuContext::new();
    if ctx.is_none() {
        failed.push("GpuContext::new (gates ~25 call sites)");
    }

    if GpuEffectsPipeline::new().is_none() {
        failed.push("GpuEffectsPipeline::new (gates ~12 call sites)");
    }
    if let Some(ctx) = ctx.as_ref() {
        if GpuEffectsPipeline::with_context(ctx).is_none() {
            failed.push("GpuEffectsPipeline::with_context (gates ~22 call sites)");
        }
        if GpuNv12Converter::with_context(ctx).is_none() {
            failed.push("GpuNv12Converter::with_context");
        }
        if GpuP010Converter::with_context(ctx).is_none() {
            failed.push("GpuP010Converter::with_context");
        }
    }
    if GpuNv12Converter::new().is_none() {
        failed.push("GpuNv12Converter::new (gates ~11 call sites)");
    }
    if GpuP010Converter::new().is_none() {
        failed.push("GpuP010Converter::new (gates ~18 call sites)");
    }

    assert!(
        failed.is_empty(),
        "an adapter is present but these would not build:\n  - {}\n\n\
         A constructor returning None here almost always means its WGSL failed \
         to compile. Run with RUST_LOG=warn to see naga's error. Until it is \
         fixed, every test gated on it is skipping and reporting ok — the GPU \
         suite is green and proving nothing.",
        failed.join("\n  - "),
    );
    eprintln!("all GPU pipelines built");
}
