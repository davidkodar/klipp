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
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUse {
    pub model: String,
    /// "CUDA", "WebGPU" or "CPU".
    pub provider: &'static str,
    /// Why the CPU, in plain words: asked for, no GPU, or what kept the
    /// GPU's provider from loading. `None` on the GPU.
    pub why_cpu: Option<String>,
    /// Milliseconds a frame (or a text) took in the latest run, where the
    /// model reports it ([`timed`]), so the GPU's gain shows.
    pub ms_per_frame: Option<f64>,
}

impl ProviderUse {
    /// Whether a GPU is present that the model doesn't run on.
    pub fn misses_a_gpu(&self) -> bool {
        self.provider == "CPU" && self.why_cpu.as_deref().is_some_and(|w| !w.starts_with(CPU_ASKED) && !w.starts_with(NO_GPU))
    }

    /// One line for a status or a log: "rf_detr_seg on CPU: …", or
    /// "clip_vit_b32_vision on WebGPU, 0.8 ms a frame".
    pub fn line(&self) -> String {
        let timing = self.ms_per_frame.map(|ms| format!(", {ms:.1} ms a frame")).unwrap_or_default();
        match &self.why_cpu {
            Some(why) => format!("{} on {}{timing}: {why}", self.model, self.provider),
            None => format!("{} on {}{timing}", self.model, self.provider),
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

/// `frames` frames run through `model` in `took`: kept as its time a
/// frame.
pub fn timed(model: &str, frames: usize, took: std::time::Duration) {
    if frames == 0 {
        return;
    }
    if let Ok(mut used) = USED.lock() {
        if let Some(e) = used.iter_mut().find(|e| e.model == model) {
            e.ms_per_frame = Some(took.as_secs_f64() * 1000.0 / frames as f64);
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
        _ => "the WebGPU add-on (Preferences > AI, a 7 MB download) runs the vision analysis on the GPU",
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
    let cpu = |why: String| ProviderUse { model: model.to_string(), provider: "CPU", why_cpu: Some(why), ms_per_frame: None };
    if device == DevicePreference::Cpu {
        record(cpu(CPU_ASKED.to_string()));
        return Ok(builder);
    }
    let (builder, cuda_failure) = with_cuda(builder);
    let (builder, failure) = match cuda_failure {
        None => (builder, Ok("CUDA")),
        Some(cuda_failure) => match with_webgpu(builder) {
            (b, None) => (b, Ok("WebGPU")),
            (b, Some(webgpu_failure)) if plugin_serves(model) => match with_webgpu_plugin(b) {
                (b, None) => (b, Ok("WebGPU")),
                (b, Some(plugin_failure)) => (b, Err(format!("{cuda_failure}; {webgpu_failure}; {plugin_failure}"))),
            },
            (b, Some(webgpu_failure)) => (b, Err(format!("{cuda_failure}; {webgpu_failure}"))),
        },
    };
    match failure {
        Ok(provider) => {
            record(ProviderUse { model: model.to_string(), provider, why_cpu: None, ms_per_frame: None });
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

/// Whether this process may run models on the WebGPU plug-in: set by an
/// analysis worker, so a fault in the plug-in or the GPU driver costs
/// that one analysis, never the editor.
static PLUGIN_ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Let this process run models on the WebGPU plug-in ([`webgpu_plugin`](crate::webgpu_plugin)).
pub fn allow_gpu_plugin() {
    PLUGIN_ALLOWED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether `model` is taken to the WebGPU plug-in here: in a process that
/// allows it, and only the models measured faster on it than on the CPU
/// (11-07, through Vulkan: RF-DETR 119 to 17 ms, PP-OCR 54 to 7 and 14 to
/// 3, CLIP 11 to 6). The face model ran slower (2 to 5 ms) and stays.
#[cfg(feature = "ort")]
fn plugin_serves(model: &str) -> bool {
    PLUGIN_ALLOWED.load(std::sync::atomic::Ordering::Relaxed) && plugin_helps(model)
}

#[cfg(feature = "ort")]
fn plugin_helps(model: &str) -> bool {
    model.starts_with("rf_detr_seg") || model == "ppocr_en_det_v3" || model == "ppocr_en_rec_v4" || model == "clip_vit_b32_vision"
}

/// `builder` on the WebGPU plug-in, or as it was with why it isn't. A
/// device has to be found for it: given none, onnxruntime builds the
/// session on the CPU without a word.
#[cfg(feature = "ort")]
fn with_webgpu_plugin(builder: ort::session::builder::SessionBuilder) -> (ort::session::builder::SessionBuilder, Option<String>) {
    let library = match crate::webgpu_plugin::library() {
        Ok(Some(path)) => path,
        Ok(None) => return (builder, Some("the WebGPU add-on isn't downloaded (Preferences > AI)".into())),
        Err(e) => return (builder, Some(e)),
    };
    let registered = REGISTERED.get_or_init(|| {
        let env = ort::environment::Environment::current().map_err(|e| e.to_string())?;
        let lib = env.register_ep_library("klipp_webgpu", &library).map_err(|e| e.to_string())?;
        // Kept for the life of the process: sessions use it.
        std::mem::forget(lib);
        Ok(())
    });
    if let Err(e) = registered {
        return (builder, Some(format!("the WebGPU add-on didn't load: {e}")));
    }
    let env = match ort::environment::Environment::current() {
        Ok(env) => env,
        Err(e) => return (builder, Some(format!("the WebGPU add-on: {e}"))),
    };
    let devices: Vec<_> = env.devices().filter(|d| d.ep().is_ok_and(|ep| ep == WEBGPU_EP)).collect();
    if devices.is_empty() {
        return (builder, Some("the WebGPU add-on found no GPU to run on (it needs a Vulkan driver)".into()));
    }
    match builder.with_devices(devices, None) {
        Ok(b) => (b, None),
        Err(e) => {
            let why = format!("the WebGPU add-on didn't take the model: {e}");
            (e.recover(), Some(why))
        }
    }
}

/// Whether the WebGPU plug-in was loaded, and why not when it failed.
#[cfg(feature = "ort")]
static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();

/// Whether this process has loaded the WebGPU plug-in. Such a process
/// must end without onnxruntime's clean-up at exit: there it releases
/// the plug-in, which tears Vulkan down after the GPU driver's own code is
/// gone, and the process crashed after its work was done (11-07, NVIDIA's
/// Vulkan driver, every framing and screen-text worker).
pub fn gpu_plugin_loaded() -> bool {
    #[cfg(feature = "ort")]
    {
        matches!(REGISTERED.get(), Some(Ok(())))
    }
    #[cfg(not(feature = "ort"))]
    {
        false
    }
}

/// The name the WebGPU plug-in gives its devices.
#[cfg(feature = "ort")]
const WEBGPU_EP: &str = "WebGpuExecutionProvider";

/// `builder` with CUDA registered, or as it was with why it isn't.
#[cfg(all(feature = "ort", feature = "cuda"))]
fn with_cuda(builder: ort::session::builder::SessionBuilder) -> (ort::session::builder::SessionBuilder, Option<String>) {
    if runtime_has_cuda() == Some(false) {
        let lib = crate::runtime_resolve::find_libonnxruntime().map(|p| p.display().to_string()).unwrap_or_else(|| "libonnxruntime".into());
        return (builder, Some(format!("the onnxruntime in use ({lib}) has no CUDA provider")));
    }
    load_cudnn();
    // Full float precision, as on the CPU: onnxruntime's default TF32 read
    // about 300 more lines of noise ("O") on a six-minute screen recording
    // than the CPU did; with it off the lines matched (one label spaced
    // differently) and it was no slower.
    match builder.with_execution_providers([ort::execution_providers::CUDAExecutionProvider::default().with_tf32(false).build().error_on_failure()]) {
        Ok(b) => (b, None),
        Err(e) => {
            let why = format!("its CUDA provider didn't load: {e}");
            (e.recover(), Some(why))
        }
    }
}

/// cuDNN, loaded where the CUDA provider can see it before the provider
/// loads. The provider calls cuDNN, but a build can leave it off the
/// libraries it asks for (Arch's onnxruntime-cuda 1.29.0-3 does): the
/// provider then failed on an undefined cuDNN symbol, and every model ran
/// on the CPU beside the GPU. Loaded once, for the life of the process;
/// where there is no cuDNN 9 nothing changes and the provider says why.
#[cfg(all(feature = "ort", feature = "cuda"))]
fn load_cudnn() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(unix)]
        {
            use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
            // SAFETY: a system library opened by name, whose start-up code
            // is cuDNN's own; it stays loaded, so nothing it gave is freed.
            match unsafe { Library::open(Some("libcudnn.so.9"), RTLD_NOW | RTLD_GLOBAL) } {
                Ok(lib) => {
                    tracing::info!("cuDNN 9 loaded for the CUDA provider");
                    std::mem::forget(lib);
                }
                Err(e) => tracing::debug!("no cuDNN 9 to load for the CUDA provider: {e}"),
            }
        }
    });
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

    /// The CUDA provider finds cuDNN's functions once Klipp has loaded
    /// it, whether or not the provider asks for the library itself: the
    /// symbol Arch's provider failed on is then there for any library
    /// loaded after. Skipped where there is no cuDNN 9.
    #[cfg(all(feature = "ort", feature = "cuda", unix))]
    #[test]
    fn cudnn_is_there_for_the_cuda_provider() {
        // SAFETY: as in `load_cudnn`.
        if unsafe { libloading::os::unix::Library::open(Some("libcudnn.so.9"), libloading::os::unix::RTLD_LAZY | libloading::os::unix::RTLD_LOCAL) }.is_err() {
            eprintln!("skipping: no cuDNN 9");
            return;
        }
        let global = || {
            // SAFETY: looking a name up in what is already loaded.
            unsafe { libloading::os::unix::Library::this().get::<unsafe extern "C" fn()>(b"cudnnGetConvolutionBackwardDataAlgorithm_v7\0").is_ok() }
        };
        assert!(!global(), "only loaded on its own, not for everyone");
        load_cudnn();
        assert!(global(), "loaded for every library after it");
    }

    #[test]
    fn a_cpu_session_beside_a_gpu_is_said_loudly() {
        let missed = ProviderUse { model: "m".into(), provider: "CPU", why_cpu: Some("an NVIDIA GPU is present, but x".into()), ms_per_frame: None };
        assert!(missed.misses_a_gpu());
        assert_eq!(missed.line(), "m on CPU: an NVIDIA GPU is present, but x");
        for why in [CPU_ASKED, NO_GPU] {
            assert!(!ProviderUse { model: "m".into(), provider: "CPU", why_cpu: Some(why.into()), ms_per_frame: None }.misses_a_gpu());
        }
        assert!(!ProviderUse { model: "m".into(), provider: "CUDA", why_cpu: None, ms_per_frame: None }.misses_a_gpu());
    }

    #[test]
    fn a_models_time_a_frame_is_said() {
        record(ProviderUse { model: "provider-timed".into(), provider: "WebGPU", why_cpu: None, ms_per_frame: None });
        timed("provider-timed", 16, std::time::Duration::from_millis(12));
        assert_eq!(used_by("provider-timed").map(|u| u.line()).as_deref(), Some("provider-timed on WebGPU, 0.8 ms a frame"));
    }

    #[test]
    fn a_models_latest_session_is_kept() {
        record(ProviderUse { model: "provider-test".into(), provider: "CUDA", why_cpu: None, ms_per_frame: None });
        record(ProviderUse { model: "provider-test".into(), provider: "CPU", why_cpu: Some(NO_GPU.into()), ms_per_frame: None });
        assert_eq!(used_by("provider-test").map(|u| u.provider), Some("CPU"));
        assert_eq!(used().iter().filter(|u| u.model == "provider-test").count(), 1);
    }
}
