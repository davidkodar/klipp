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

/// The graphics APIs Klipp's GPU work runs on (#2990), for the engine's
/// device, the UI's and export's alike: the platform's own (Vulkan
/// here), and GL only where they have no adapter. `WGPU_BACKEND` still
/// chooses. Asked for every API, the GL backend's adapter probe brought
/// up NVIDIA's GL driver beside its Vulkan one; at exit the driver's
/// teardown raced its own background shader compile and aborted the
/// process ("corrupted size vs. prev_size"), one test run in five.
pub fn backends() -> wgpu::Backends {
    let _loading = klipp_load_lock::library_loading();
    backends_unlocked()
}

/// [`backends`] with the driver-loading lock already held.
fn backends_unlocked() -> wgpu::Backends {
    static PICKED: std::sync::OnceLock<wgpu::Backends> = std::sync::OnceLock::new();
    *PICKED.get_or_init(|| {
        if let Some(chosen) = wgpu::util::backend_bits_from_env() {
            return chosen;
        }
        let primary = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, flags: wgpu::InstanceFlags::from_build_config() & !wgpu::InstanceFlags::VALIDATION, ..Default::default() });
        if primary.enumerate_adapters(wgpu::Backends::PRIMARY).is_empty() {
            info!("GpuContext: no Vulkan adapter; GL instead");
            wgpu::Backends::GL
        } else {
            wgpu::Backends::PRIMARY
        }
    })
}

/// An instance on [`backends`], made as the app's devices make theirs:
/// for a test or a tool that wants a device of its own, so it never
/// brings up NVIDIA's GL driver beside its Vulkan one (#3000).
pub fn instance() -> wgpu::Instance {
    let _loading = klipp_load_lock::library_loading();
    instance_unlocked()
}

/// [`instance`] with the driver-loading lock already held.
fn instance_unlocked() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: backends_unlocked(),
        flags: wgpu::InstanceFlags::from_build_config() & !wgpu::InstanceFlags::VALIDATION,
        ..Default::default()
    })
}

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
        let instance = instance_unlocked();

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
    /// Returns `None` if initialisation fails (no adapter, a driver
    /// refusing new work, etc.), and keeps answering `None` for
    /// `SHARED_RETRY_AFTER` so a broken driver isn't retried on every
    /// decoder spawn; after that the next caller tries again. It used to
    /// keep that answer for the life of the process, so a driver that
    /// recovered was never used (#2394). Meanwhile the `*::new()` paths
    /// fall back to creating private devices (the pre-phase-4a
    /// behaviour).
    pub fn shared() -> Option<&'static GpuContext> {
        if let Some(ctx) = SHARED.get() {
            return Some(ctx);
        }
        let mut failed_at = SHARED_FAILED_AT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ctx) = SHARED.get() {
            return Some(ctx);
        }
        if failed_at.is_some_and(|at| at.elapsed() < SHARED_RETRY_AFTER) {
            return None;
        }
        match GpuContext::new() {
            Some(ctx) => {
                *failed_at = None;
                // An install racing this one wins; either is the device.
                let _ = SHARED.set(ctx);
                SHARED.get()
            }
            None => {
                *failed_at = Some(std::time::Instant::now());
                None
            }
        }
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
        SHARED.set(ctx)?;
        SHARED_FROM_UI.store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// Wait, up to `limit`, for the shared device to be decided: the UI's
    /// installed, or another caller's first [`Self::shared`] (#2108). A
    /// pipeline built ahead waits here, so it never makes the device
    /// itself and leaves the UI's install to fail. Whether it was decided.
    pub fn wait_for_shared(limit: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + limit;
        while SHARED.get().is_none() && SHARED_FAILED_AT.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
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
// slot.
static SHARED: std::sync::OnceLock<GpuContext> = std::sync::OnceLock::new();
/// When [`GpuContext::shared`] last failed to make the device, while
/// it has none. Its lock also keeps two callers from making it at once.
static SHARED_FAILED_AT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
/// How long after a failed [`GpuContext::shared`] the next caller tries
/// to make the device again.
const SHARED_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);
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

/// Out-of-memory errors the device reported this session (#2394).
static OUT_OF_MEMORY_ERRORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many out-of-memory errors the device has reported this session.
pub fn out_of_memory_errors() -> u64 {
    OUT_OF_MEMORY_ERRORS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Count an out-of-memory error, as the device reports one. Public for
/// tests, which can't make a driver run out.
#[doc(hidden)]
pub fn note_out_of_memory() {
    OUT_OF_MEMORY_ERRORS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Times the GPU has run out of memory this session (#3174): the caches
/// holding GPU memory let go of what they keep for reuse when it moves.
static MEMORY_PRESSURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times the GPU has run out of memory this session: a cache
/// that read another number when it last trimmed trims again.
pub fn memory_pressure() -> u64 {
    MEMORY_PRESSURE.load(std::sync::atomic::Ordering::Acquire)
}

/// The GPU ran out of memory: the caches trim. Public for tests.
#[doc(hidden)]
pub fn note_memory_pressure() {
    MEMORY_PRESSURE.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// How long after running out of memory the validation errors that follow
/// are its doing (#3174): a buffer that failed to be made is then invalid
/// wherever a pass binds it, for the frame or two still recording.
pub const OUT_OF_MEMORY_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// What an error from the device means for the GPU stack (#3174).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Ran out of memory: the caches trim and the next frame tries again.
    OutOfMemory,
    /// One dropped command, or out of memory's doing: put up with.
    Tolerated,
    /// The device is gone, or the errors don't stop: GPU effects off.
    Lost,
}

/// The device's errors as they come (#3174), kept apart by what they mean:
/// running out of memory is this frame's, and what it leaves invalid
/// for a moment after is too; a device that is gone is gone whatever
/// came before; any other validation error is put up with until a run
/// of them.
#[derive(Debug, Default)]
pub struct ErrorTally {
    last_out_of_memory: Option<std::time::Instant>,
    counted: u64,
}

/// Whether an error's text says the device itself is gone: wgpu reports
/// each call on a lost device as a validation error (#3174), as it does a
/// failed allocation wgpu-hal maps to a lost device.
pub fn says_device_lost(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("parent device is lost") || t.contains("device is lost") || t.contains("device lost")
}

impl ErrorTally {
    /// An error at `now`: out of memory, or a validation error saying
    /// `text`.
    pub fn on_error(&mut self, out_of_memory: bool, text: &str, now: std::time::Instant) -> Verdict {
        if says_device_lost(text) {
            return Verdict::Lost;
        }
        if out_of_memory {
            self.last_out_of_memory = Some(now);
            return Verdict::OutOfMemory;
        }
        if self.last_out_of_memory.is_some_and(|at| now.duration_since(at) <= OUT_OF_MEMORY_GRACE) {
            return Verdict::Tolerated;
        }
        self.counted += 1;
        if self.counted > VALIDATION_ERRORS_TOLERATED { Verdict::Lost } else { Verdict::Tolerated }
    }

    /// Validation errors counted toward switching GPU effects off.
    pub fn counted(&self) -> u64 {
        self.counted
    }
}

fn register_device_callbacks(
    device: &wgpu::Device,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let device_lost = std::sync::Arc::new(AtomicBool::new(false));
    {
        let flag = device_lost.clone();
        let tally = std::sync::Mutex::new(ErrorTally::default());
        device.on_uncaptured_error(Box::new(move |err| {
            // A validation error is one dropped command on a device that
            // is otherwise fine: the frame it was part of is wrong, the
            // next frame records afresh. It used to take the GPU away
            // for the rest of the session, and a 4K timeline then ran on
            // the CPU at five frames a second with nothing on screen to
            // say why. Only a run of them, or the device going, does that
            // now. Out of memory is this frame's (#2394): counted, said,
            // the caches told to trim, and the next frame tries the GPU
            // again; what it leaves invalid for a moment after is its
            // doing, not a run of errors (#3174). GPU work never stands
            // aside for it: the entries that pass a frame through would
            // put ungraded frames in an export.
            let internal = matches!(err, wgpu::Error::Internal { .. });
            let out_of_memory = matches!(err, wgpu::Error::OutOfMemory { .. });
            let text = err.to_string();
            let verdict = if internal { Verdict::Lost } else { tally.lock().unwrap_or_else(|e| e.into_inner()).on_error(out_of_memory, &text, std::time::Instant::now()) };
            if !out_of_memory && !internal {
                VALIDATION_ERRORS.fetch_add(1, Ordering::SeqCst);
            }
            match verdict {
                Verdict::OutOfMemory => {
                    note_out_of_memory();
                    note_memory_pressure();
                    tracing::error!("WGPU_UNCAPTURED: {err} — out of GPU memory ({} this session); the caches trim and the next frame tries the GPU again", out_of_memory_errors());
                }
                Verdict::Tolerated => {
                    tracing::error!("WGPU_UNCAPTURED validation error, tolerated ({} of {VALIDATION_ERRORS_TOLERATED} counted): {err}", tally.lock().unwrap_or_else(|e| e.into_inner()).counted());
                    tracing::error!("WGPU_UNCAPTURED debug:\n{err:#?}");
                }
                Verdict::Lost => {
                    tracing::error!("WGPU_UNCAPTURED: {err} — the device is lost or the errors don't stop, disabling GPU effects");
                    tracing::error!("WGPU_UNCAPTURED debug:\n{err:#?}");
                    flag.store(true, Ordering::SeqCst);
                }
            }
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


#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// #3174, the M1's case: out of memory, and the burst of validation
    /// errors what failed to be made leaves after it, is put up with, the
    /// caches told to trim; after the grace, a run of errors still turns
    /// GPU effects off past 32.
    #[test]
    fn out_of_memory_and_its_errors_keep_the_gpu() {
        let mut tally = ErrorTally::default();
        let t0 = Instant::now();
        assert_eq!(tally.on_error(true, "Not enough memory left.", t0), Verdict::OutOfMemory);
        for i in 0..84 {
            assert_eq!(tally.on_error(false, "In a set_bind_group command: Buffer with 'fx_a' label is invalid", t0 + Duration::from_millis(10 * i)), Verdict::Tolerated, "{i}");
        }
        assert_eq!(tally.counted(), 0, "none counted toward switching off");
        let later = t0 + OUT_OF_MEMORY_GRACE + Duration::from_millis(900);
        for i in 0..VALIDATION_ERRORS_TOLERATED {
            assert_eq!(tally.on_error(false, "a validation error", later), Verdict::Tolerated, "{i}");
        }
        assert_eq!(tally.on_error(false, "a validation error", later), Verdict::Lost, "the 33rd past the grace");
    }

    /// #3174: a device that is gone is lost at once, the grace after
    /// running out of memory or not: wgpu says each call on it is a
    /// validation error, and wgpu-hal makes a lost device of an
    /// allocation that fails for another reason than memory.
    #[test]
    fn a_lost_device_is_lost_even_just_after_out_of_memory() {
        let t0 = Instant::now();
        let mut tally = ErrorTally::default();
        assert_eq!(tally.on_error(false, "Parent device is lost", t0), Verdict::Lost, "with nothing before");
        let mut tally = ErrorTally::default();
        assert_eq!(tally.on_error(true, "Not enough memory left.", t0), Verdict::OutOfMemory);
        assert_eq!(tally.on_error(false, "In Queue::submit: Parent device is lost", t0 + Duration::from_millis(5)), Verdict::Lost, "within the grace");
        assert!(says_device_lost("Validation Error: Device is lost"));
        assert!(!says_device_lost("Buffer with 'fx_a' label is invalid"));
    }
}
