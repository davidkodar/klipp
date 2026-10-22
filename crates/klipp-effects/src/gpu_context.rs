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

/// The limits Klipp asks a device for, the engine's and the UI's alike
/// (#2341): wgpu's defaults, but the adapter's own largest buffer and
/// storage binding. The defaults cap a binding at 128 MiB and a buffer at
/// 256 MiB, under an 8K frame of four floats a pixel (~510 MiB); what an
/// adapter offers is granted, so asking for it can't fail. The GL backend
/// starts from the WebGL2 defaults, as egui's own descriptor did, and
/// textures go to 8192 for a 4K+ surface's depth buffer.
pub fn device_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let base = if adapter.get_info().backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    let offered = adapter.limits();
    wgpu::Limits {
        max_texture_dimension_2d: base.max_texture_dimension_2d.max(8192),
        max_storage_buffer_binding_size: offered.max_storage_buffer_binding_size,
        max_buffer_size: offered.max_buffer_size,
        ..base
    }
}

/// Log the buffer limits `device` was granted, so a frame too large for
/// them can be told from a bug.
fn log_granted_limits(device: &wgpu::Device) {
    let l = device.limits();
    info!(
        "GpuContext limits: storage binding {} MiB, buffer {} MiB, texture 2D {}",
        l.max_storage_buffer_binding_size >> 20,
        l.max_buffer_size >> 20,
        l.max_texture_dimension_2d
    );
}

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
    /// Flipped by the device's uncaptured-error / device-lost
    /// callbacks. Owned here — NOT per consumer — because wgpu keeps
    /// exactly one callback slot per device and replacing it from two
    /// threads races inside wgpu-core (the loser's closure is dropped
    /// unconsumed → panic in `DeviceLostClosureRust::drop`). Register
    /// once per device, share the flag.
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The adapter the device was made on, when this context made it
    /// ([`Self::new`]); a device handed in ([`Self::from_existing`])
    /// doesn't say.
    adapter_info: Option<wgpu::AdapterInfo>,
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

        // Starting the driver must not overlap a plug-in's library
        // loading on another thread: the two deadlock inside NVIDIA's
        // driver (#1032). Held until the device is made.
        let _loading = klipp_load_lock::library_loading();
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
                required_limits: device_limits(&adapter),
                ..Default::default()
            },
            None,
        ))
        .ok()?;
        drop(_loading);
        log_granted_limits(&device);

        let device_lost = register_device_callbacks(&device);

        // `instance` is dropped at scope exit — wgpu's device /
        // queue keep the underlying driver resources alive via their
        // own refcounts.
        drop(instance);
        Some(Self {
            device: std::sync::Arc::new(device),
            queue: std::sync::Arc::new(queue),
            device_lost,
            adapter_info: Some(adapter_info),
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
        log_granted_limits(&device);
        let device_lost = register_device_callbacks(&device);
        Self { device, queue, device_lost, adapter_info: None }
    }

    /// The adapter this context's device was made on: its name, driver
    /// and backend, so a result tied to one GPU and driver can say which.
    /// `None` for a device handed in by [`Self::from_existing`].
    pub fn adapter_info(&self) -> Option<&wgpu::AdapterInfo> {
        self.adapter_info.as_ref()
    }

    /// Cheap clone of the Arc-wrapped device.
    pub fn device(&self) -> std::sync::Arc<wgpu::Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Cheap clone of the Arc-wrapped queue.
    pub fn queue(&self) -> std::sync::Arc<wgpu::Queue> {
        std::sync::Arc::clone(&self.queue)
    }

    /// Shared "this device errored / was lost" flag. Consumers (the
    /// effects pipeline) read it to short-circuit GPU work; they may
    /// reset it when rebuilding after a transient error.
    pub fn device_lost_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.device_lost)
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
        })?;
        SHARED_FROM_UI.store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// Wait, up to `limit`, for the shared device to be decided: the UI's
    /// installed, or another caller's first [`Self::shared`] (#2108). A
    /// pipeline built ahead waits here, so it never makes the device
    /// itself and leaves the UI's install to fail. Whether it was decided.
    pub fn wait_for_shared(limit: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + limit;
        while SHARED.get().is_none() {
            if std::time::Instant::now() >= until {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        true
    }

    /// Whether [`Self::shared`] is the UI's own device, installed by
    /// [`Self::install_shared`]. Only then can a texture the engine
    /// writes be sampled by the UI's renderer: wgpu tracks resources
    /// per instance, and a view from another instance panics on
    /// registration.
    pub fn shared_is_ui_device() -> bool {
        SHARED_FROM_UI.load(std::sync::atomic::Ordering::Acquire)
    }
}

// File-level so `shared()` and `install_shared()` cooperate on one
// slot. `None` in the `Some(...)` position means init was attempted
// and failed — `shared()` returns None until end of process.
static SHARED: std::sync::OnceLock<Option<GpuContext>> = std::sync::OnceLock::new();
static SHARED_FROM_UI: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Register the device's single uncaptured-error + device-lost
/// callback pair, routing both into tracing and a shared flag.
///
/// Called exactly once per `wgpu::Device`, from the `GpuContext`
/// constructors. Nothing else in the codebase may call
/// `on_uncaptured_error` / `set_device_lost_callback` — wgpu holds
/// one callback slot per device, and concurrent replacement races
/// inside wgpu-core 22 (`device_set_device_lost_closure` drops the
/// life-tracker lock mid-swap; the losing thread's closure is dropped
/// unconsumed and panics). This was the source of flaky
/// `DeviceLostClosureRust must be consumed` panics under parallel
/// `cargo test`.
///
/// Routing errors into tracing (rather than wgpu's default panicking
/// handler) also keeps the GPU-COMPOUND-PANIC class diagnosable: the
/// original validation message that names the offending resource
/// lands in klipp-debug.log immediately before any downstream abort.
/// Validation errors seen on the device this session, and how many are
/// put up with before GPU effects are switched off.
static VALIDATION_ERRORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const VALIDATION_ERRORS_TOLERATED: u64 = 32;

/// How many validation errors the device has reported this session.
pub fn validation_errors() -> u64 {
    VALIDATION_ERRORS.load(std::sync::atomic::Ordering::Relaxed)
}

fn register_device_callbacks(
    device: &wgpu::Device,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let device_lost = std::sync::Arc::new(AtomicBool::new(false));
    {
        let flag = device_lost.clone();
        device.on_uncaptured_error(Box::new(move |err| {
            // A validation error is one dropped command on a device that
            // is otherwise fine: the frame it was part of is wrong, the
            // next frame records afresh. It used to take the GPU away
            // for the rest of the session, and a 4K timeline then ran on
            // the CPU at five frames a second with nothing on screen to
            // say why. Only a run of them, or memory or the device going,
            // does that now.
            if matches!(err, wgpu::Error::Validation { .. }) {
                let n = VALIDATION_ERRORS.fetch_add(1, Ordering::SeqCst) + 1;
                if n <= VALIDATION_ERRORS_TOLERATED {
                    tracing::error!("WGPU_UNCAPTURED validation error {n} of {VALIDATION_ERRORS_TOLERATED} tolerated: {err}");
                    tracing::error!("WGPU_UNCAPTURED debug:\n{err:#?}");
                    return;
                }
                tracing::error!("WGPU_UNCAPTURED: {err} — the {n}th validation error, disabling GPU effects");
            } else {
                tracing::error!("WGPU_UNCAPTURED: {err} — disabling GPU effects");
                tracing::error!("WGPU_UNCAPTURED debug:\n{err:#?}");
            }
            flag.store(true, Ordering::SeqCst);
        }));
    }
    {
        let flag = device_lost.clone();
        device.set_device_lost_callback(Box::new(move |reason, msg| {
            // `ReplacedCallback` fires if a newer callback ever takes
            // over this slot — the device itself is fine.
            if matches!(reason, wgpu::DeviceLostReason::ReplacedCallback) {
                return;
            }
            tracing::error!("WGPU device lost ({reason:?}): {msg} — disabling GPU effects");
            flag.store(true, Ordering::SeqCst);
        }));
    }
    device_lost
}
