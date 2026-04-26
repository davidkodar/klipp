//! Shared wgpu device + queue for the entire engine's GPU pipeline.
//!
//! Historically every GPU-using type in klipp-effects
//! (`GpuNv12Converter`, `GpuP010Converter`, `GpuEffectsPipeline`) owned
//! its own `wgpu::Device`. That worked but forced every cross-type
//! handoff through a CPU round-trip — buffers are tied to the device
//! that allocated them and cannot be bound on a different device.
//!
//! Phase 4 of the end-to-end GPU-residency plan unifies all three on a
//! single `GpuContext`. Decoder output written on `ctx.device` can be
//! fed directly as the grade chain's first input, no download + no
//! re-upload. The engine builds one `GpuContext` at startup and hands
//! it to the converters and the effects pipeline.
//!
//! The existing `*::new()` constructors still work by creating a
//! private `GpuContext` internally, so pre-phase-4 call sites are
//! unaffected.

use tracing::info;

/// Bundled wgpu device + queue. Held behind `Arc` so handles can be
/// shared across the decoder thread, the engine thread, and the UI
/// thread without cloning the underlying wgpu resources.
///
/// The `wgpu::Instance` is deliberately not retained — device /
/// queue carry their own refcount to the underlying driver-level
/// resources, and eframe-wgpu (when injecting its device via
/// `from_existing`) doesn't expose its instance publicly.
#[derive(Clone)]
pub struct GpuContext {
    pub device: std::sync::Arc<wgpu::Device>,
    pub queue: std::sync::Arc<wgpu::Queue>,
}

impl GpuContext {
    /// Create a fresh `GpuContext`. Returns `None` on systems where
    /// no usable wgpu backend is available (pre-Vulkan-1.2 Linux,
    /// macOS without Metal, headless servers without llvmpipe, etc.)
    /// so callers can fall back to their CPU paths.
    ///
    /// Applies the same CachyOS `VK_DRIVER_FILES` shim the existing
    /// converters use — this path is the canonical one for future
    /// code, so the shim lives here.
    pub fn new() -> Option<Self> {
        if std::env::var("VK_DRIVER_FILES").is_err() {
            let icd_path = "/usr/share/vulkan/icd.d/nvidia_icd.json";
            if std::path::Path::new(icd_path).exists() {
                // SAFETY: called early, before other threads read env.
                unsafe { std::env::set_var("VK_DRIVER_FILES", icd_path) };
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::from_build_config() & !wgpu::InstanceFlags::VALIDATION,
            ..Default::default()
        });

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))?;

        let adapter_info = adapter.get_info();
        info!("GpuContext: {} ({:?})", adapter_info.name, adapter_info.backend);

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("klipp-shared"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            },
            None,
        ))
        .ok()?;

        // Diagnostic: route uncaptured wgpu errors (validation,
        // out-of-memory, lost device) into tracing instead of letting
        // them surface as opaque storage-lookup panics inside
        // wgpu-core. Without this the GPU-COMPOUND-PANIC class shows
        // up as a bare `TextureView[Id(N,vk)] does not exist` panic
        // with no upstream context; with it, the original wgpu
        // validation message that names the offending resource
        // appears in klipp-debug.log immediately before the abort.
        device.on_uncaptured_error(Box::new(|err| {
            tracing::error!("WGPU_UNCAPTURED on engine device: {err}");
            tracing::error!("WGPU_UNCAPTURED debug:\n{err:#?}");
        }));

        // `instance` is dropped at scope exit — wgpu's device /
        // queue keep the underlying driver resources alive via their
        // own refcounts.
        drop(instance);
        Some(Self {
            device: std::sync::Arc::new(device),
            queue: std::sync::Arc::new(queue),
        })
    }

    /// Wrap existing `Arc<wgpu::Device>` / `Arc<wgpu::Queue>`
    /// handles — typically from eframe-wgpu's `RenderState` so the
    /// engine and the UI run on the same device. Pair with
    /// `install_shared` before any consumer calls `shared()`.
    pub fn from_existing(
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
    ) -> Self {
        Self { device, queue }
    }

    /// Cheap clone of the Arc-wrapped device.
    pub fn device(&self) -> std::sync::Arc<wgpu::Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Cheap clone of the Arc-wrapped queue.
    pub fn queue(&self) -> std::sync::Arc<wgpu::Queue> {
        std::sync::Arc::clone(&self.queue)
    }

    /// Return the process-wide shared context, lazily initialised on
    /// first call. Once the context exists, every subsequent
    /// `GpuNv12Converter::new`, `GpuP010Converter::new`, and
    /// `GpuEffectsPipeline::new` re-uses the same `wgpu::Device` —
    /// the prerequisite for GPU-resident decode → grade hand-off,
    /// and for viewer-side GPU handoff when the UI shares this same
    /// device.
    ///
    /// Returns `None` if the first initialisation fails (no adapter,
    /// etc.) and caches that failure for the process lifetime so we
    /// don't retry a broken driver on every decoder spawn. On such
    /// systems the `*::new()` paths fall back to creating private
    /// devices (the pre-phase-4a behaviour).
    pub fn shared() -> Option<&'static GpuContext> {
        SHARED.get_or_init(GpuContext::new).as_ref()
    }

    /// Install a pre-built `GpuContext` as the process-wide shared
    /// one. Must be called before any `shared()` caller or it's a
    /// no-op — the first `get_or_init` wins. Used by the UI host
    /// (eframe-wgpu) to inject its own device so the engine, the
    /// converters, and the viewer all render on a single device.
    ///
    /// Returns `Ok(())` when installation succeeded. Returns
    /// `Err(ctx)` if the shared slot was already populated (either
    /// by an earlier `install_shared` or by `shared()` triggering
    /// its lazy init); the caller can decide whether to log and
    /// proceed with two devices or abort.
    pub fn install_shared(ctx: GpuContext) -> Result<(), GpuContext> {
        SHARED.set(Some(ctx)).map_err(|opt| {
            // OnceLock::set returns the value back on Err; unwrap
            // the Option layer. The slot is only ever stored as
            // Some here, so opt is always Some.
            opt.expect("install_shared always stores Some")
        })
    }
}

// File-level so `shared()` and `install_shared()` cooperate on one
// slot. `None` in the `Some(...)` position means init was attempted
// and failed — `shared()` returns None until end of process.
static SHARED: std::sync::OnceLock<Option<GpuContext>> = std::sync::OnceLock::new();
