//! Which execution provider each model's session runs on (#2250).
//!
//! ort falls back to its CPU provider when the CUDA one can't be
//! registered, and says so only in its own log: with a CPU-only
//! libonnxruntime on a machine with an NVIDIA GPU, every model ran on the
//! CPU and nothing showed it. A session here is built through
//! [`on_device`], which tries the GPU's providers in turn (CUDA, then
//! WebGPU, #2245) so a failure is an error, keeps the session on the CPU
//! when none loads, and records what it got and why: read by the AI
//! models panel, the analysis status and the log.

use std::sync::Mutex;

#[cfg(feature = "ort")]
use klipp_ml::DevicePreference;

/// The provider a model's session runs on, and why not the GPU when it
/// is the CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUse {
    pub model: String,
    /// "CUDA", "WebGPU" or "CPU".
    pub provider: &'static str,
    /// Why the CPU, in plain words: asked for, no GPU, or what kept the
    /// GPU's provider from loading. `None` on the GPU.
    pub why_cpu: Option<String>,
}

impl ProviderUse {
    /// Whether a GPU is present that the model doesn't run on.
    pub fn misses_a_gpu(&self) -> bool {
        self.provider == "CPU" && self.why_cpu.as_deref().is_some_and(|w| !w.starts_with(CPU_ASKED) && !w.starts_with(NO_GPU))
    }

    /// One line for a status or a log: "rf_detr_seg on CPU: …".
    pub fn line(&self) -> String {
        match &self.why_cpu {
            Some(why) => format!("{} on {}: {why}", self.model, self.provider),
            None => format!("{} on {}", self.model, self.provider),
        }
    }
}

const CPU_ASKED: &str = "the CPU was asked for";
const NO_GPU: &str = "no GPU found";

static USED: Mutex<Vec<ProviderUse>> = Mutex::new(Vec::new());

/// What each model's latest session runs on, in the order first built.
pub fn used() -> Vec<ProviderUse> {
    USED.lock().map(|u| u.clone()).unwrap_or_default()
}

/// What `model`'s latest session runs on, if one was built.
pub fn used_by(model: &str) -> Option<ProviderUse> {
    used().into_iter().find(|u| u.model == model)
}

/// `u` kept as its model's, said in the log, loudly when it misses a GPU.
pub fn record(u: ProviderUse) {
    if u.misses_a_gpu() {
        tracing::warn!("{}", u.line());
    } else {
        tracing::info!("{}", u.line());
    }
    if let Ok(mut used) = USED.lock() {
        match used.iter_mut().find(|e| e.model == u.model) {
            Some(e) => *e = u,
            None => used.push(u),
        }
    }
}

/// Why a session wanting the GPU is on the CPU, from what the machine has
/// and what kept the GPU's providers from loading (`failure`).
#[cfg(feature = "ort")]
fn why_not_gpu(failure: &str) -> String {
    use crate::runtime_resolve::{detect_gpu_vendor, GpuVendor};
    match detect_gpu_vendor() {
        GpuVendor::None => NO_GPU.to_string(),
        v @ (GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::Intel) => format!("an {} GPU is present, but {failure}", v.display_label()),
        GpuVendor::Unknown => format!("no GPU provider loaded ({failure})"),
    }
}

/// What to install for this machine's GPU, said when the onnxruntime in
/// use has no provider for it.
#[cfg(feature = "ort")]
fn gpu_runtime_hint() -> &'static str {
    use crate::runtime_resolve::{detect_gpu_vendor, GpuVendor};
    match detect_gpu_vendor() {
        GpuVendor::Nvidia => "a GPU build (onnxruntime-cuda, or ORT_DYLIB_PATH to a CUDA or WebGPU build) runs it on the GPU",
        _ => "a WebGPU build of onnxruntime, set through ORT_DYLIB_PATH, runs it on the GPU",
    }
}

/// Whether the onnxruntime library loaded was built with the WebGPU
/// provider; `None` before it can be asked.
#[cfg(feature = "ort")]
pub fn runtime_has_webgpu() -> Option<bool> {
    use ort::execution_providers::ExecutionProvider;
    crate::runtime_resolve::ensure_runtime_loadable().ok()?;
    ort::ep::WebGPU::default().is_available().ok()
}

/// The GPU provider a session would get here: CUDA when this build has
/// it and the onnxruntime loaded does, else WebGPU when the onnxruntime
/// does, else none.
#[cfg(feature = "ort")]
pub fn gpu_provider() -> Option<&'static str> {
    if cfg!(feature = "cuda") && runtime_has_cuda() == Some(true) {
        Some("CUDA")
    } else if runtime_has_webgpu() == Some(true) {
        Some("WebGPU")
    } else {
        None
    }
}

/// Whether the onnxruntime library loaded was built with the CUDA
/// provider; `None` before it can be asked.
#[cfg(feature = "ort")]
pub fn runtime_has_cuda() -> Option<bool> {
    use ort::execution_providers::ExecutionProvider;
    crate::runtime_resolve::ensure_runtime_loadable().ok()?;
    ort::execution_providers::CUDAExecutionProvider::default().is_available().ok()
}

/// `builder` with the GPU's provider where `device` wants it and it
/// loads, else as it was, on the CPU: what it got recorded as `model`'s.
/// An error only when `device` requires the GPU and it didn't load.
#[cfg(feature = "ort")]
pub fn on_device(builder: ort::session::builder::SessionBuilder, device: DevicePreference, model: &str) -> Result<ort::session::builder::SessionBuilder, String> {
    let cpu = |why: String| ProviderUse { model: model.to_string(), provider: "CPU", why_cpu: Some(why) };
    if device == DevicePreference::Cpu {
        record(cpu(CPU_ASKED.to_string()));
        return Ok(builder);
    }
    let (builder, cuda_failure) = with_cuda(builder);
    let (builder, failure) = match cuda_failure {
        None => (builder, Ok("CUDA")),
        Some(cuda_failure) => match with_webgpu(builder) {
            (b, None) => (b, Ok("WebGPU")),
            (b, Some(webgpu_failure)) => (b, Err(format!("{cuda_failure}; {webgpu_failure}"))),
        },
    };
    match failure {
        Ok(provider) => {
            record(ProviderUse { model: model.to_string(), provider, why_cpu: None });
            Ok(builder)
        }
        Err(failure) => {
            let why = why_not_gpu(&failure);
            record(cpu(why.clone()));
            if device == DevicePreference::GpuRequired {
                return Err(format!("{model} needs the GPU: {why}"));
            }
            Ok(builder)
        }
    }
}

/// `builder` with CUDA registered, or as it was with why it isn't.
#[cfg(all(feature = "ort", feature = "cuda"))]
fn with_cuda(builder: ort::session::builder::SessionBuilder) -> (ort::session::builder::SessionBuilder, Option<String>) {
    if runtime_has_cuda() == Some(false) {
        let lib = crate::runtime_resolve::find_libonnxruntime().map(|p| p.display().to_string()).unwrap_or_else(|| "libonnxruntime".into());
        return (builder, Some(format!("the onnxruntime in use ({lib}) has no CUDA provider")));
    }
    match builder.with_execution_providers([ort::execution_providers::CUDAExecutionProvider::default().build().error_on_failure()]) {
        Ok(b) => (b, None),
        Err(e) => {
            let why = format!("its CUDA provider didn't load: {e}");
            (e.recover(), Some(why))
        }
    }
}

#[cfg(all(feature = "ort", not(feature = "cuda")))]
fn with_cuda(builder: ort::session::builder::SessionBuilder) -> (ort::session::builder::SessionBuilder, Option<String>) {
    (builder, Some("this build of Klipp has no CUDA support (the cuda feature is off)".into()))
}

/// `builder` with WebGPU registered, or as it was with why it isn't, and
/// what to install.
#[cfg(feature = "ort")]
fn with_webgpu(builder: ort::session::builder::SessionBuilder) -> (ort::session::builder::SessionBuilder, Option<String>) {
    if runtime_has_webgpu() == Some(false) {
        return (builder, Some(format!("nor a WebGPU provider: {}", gpu_runtime_hint())));
    }
    match builder.with_execution_providers([ort::ep::WebGPU::default().build().error_on_failure()]) {
        Ok(b) => (b, None),
        Err(e) => {
            let why = format!("its WebGPU provider didn't load: {e}");
            (e.recover(), Some(why))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cpu_session_beside_a_gpu_is_said_loudly() {
        let missed = ProviderUse { model: "m".into(), provider: "CPU", why_cpu: Some("an NVIDIA GPU is present, but x".into()) };
        assert!(missed.misses_a_gpu());
        assert_eq!(missed.line(), "m on CPU: an NVIDIA GPU is present, but x");
        for why in [CPU_ASKED, NO_GPU] {
            assert!(!ProviderUse { model: "m".into(), provider: "CPU", why_cpu: Some(why.into()) }.misses_a_gpu());
        }
        assert!(!ProviderUse { model: "m".into(), provider: "CUDA", why_cpu: None }.misses_a_gpu());
    }

    #[test]
    fn a_models_latest_session_is_kept() {
        record(ProviderUse { model: "provider-test".into(), provider: "CUDA", why_cpu: None });
        record(ProviderUse { model: "provider-test".into(), provider: "CPU", why_cpu: Some(NO_GPU.into()) });
        assert_eq!(used_by("provider-test").map(|u| u.provider), Some("CPU"));
        assert_eq!(used().iter().filter(|u| u.model == "provider-test").count(), 1);
    }
}
