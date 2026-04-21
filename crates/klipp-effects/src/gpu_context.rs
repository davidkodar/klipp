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

/// Bundled wgpu device + queue + instance. All three are held behind
/// `Arc` so handles can be shared across the decoder thread, the
/// engine thread, and any phase-4+ consumers without cloning the
/// underlying handles.
#[derive(Clone)]
pub struct GpuContext {
    pub instance: std::sync::Arc<wgpu::Instance>,
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

        Some(Self {
            instance: std::sync::Arc::new(instance),
            device: std::sync::Arc::new(device),
            queue: std::sync::Arc::new(queue),
        })
    }

    /// Cheap clone of the Arc-wrapped device.
    pub fn device(&self) -> std::sync::Arc<wgpu::Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Cheap clone of the Arc-wrapped queue.
    pub fn queue(&self) -> std::sync::Arc<wgpu::Queue> {
        std::sync::Arc::clone(&self.queue)
    }
}
