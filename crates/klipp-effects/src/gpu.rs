//! GPU-accelerated effects via wgpu compute shaders.
//!
//! Each effect runs as a compute shader dispatch on the GPU.
//! Frames are uploaded, processed through the effect chain, and downloaded.
//! Falls back to CPU if GPU is unavailable.

/// GPU power preference set from application preferences.
static GPU_POWER_PREF: std::sync::Mutex<Option<wgpu::PowerPreference>> = std::sync::Mutex::new(None);

/// Set the GPU power preference before pipeline initialization.
/// Options: HighPerformance (discrete), LowPower (integrated).
/// None uses HighPerformance as default.
pub fn set_gpu_power_preference(pref: Option<wgpu::PowerPreference>) {
    *GPU_POWER_PREF.lock().unwrap() = pref;
}

use crate::BuiltinEffect;
use tracing::{debug, info, warn};

/// Definition of a single mask for GPU dispatch.
pub struct GpuMaskDef {
    /// 0=circle, 1=rectangle, 2=linear_gradient
    pub shape_type: u32,
    pub cx: f32,
    pub cy: f32,
    pub p1: f32,  // circle: radius, rect: width, grad: end_x
    pub p2: f32,  // rect: height, grad: end_y
    pub p3: f32,  // rect: rotation (degrees)
    pub feather: f32,
    /// bit 0 = invert, bits 8-15 = opacity (0-255)
    pub flags: u32,
}

impl GpuMaskDef {
    pub fn circle(cx: f32, cy: f32, radius: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self { shape_type: 0, cx, cy, p1: radius, p2: 0.0, p3: 0.0, feather, flags: Self::make_flags(invert, opacity) }
    }
    pub fn rectangle(cx: f32, cy: f32, w: f32, h: f32, rotation: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self { shape_type: 1, cx, cy, p1: w, p2: h, p3: rotation, feather, flags: Self::make_flags(invert, opacity) }
    }
    pub fn linear_gradient(sx: f32, sy: f32, ex: f32, ey: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self { shape_type: 2, cx: sx, cy: sy, p1: ex, p2: ey, p3: 0.0, feather, flags: Self::make_flags(invert, opacity) }
    }
    fn make_flags(invert: bool, opacity: f32) -> u32 {
        let opacity_byte = (opacity.clamp(0.0, 1.0) * 255.0) as u32;
        (if invert { 1 } else { 0 }) | (opacity_byte << 8)
    }
}

/// GPU effects pipeline — holds device, queue, compute pipelines, and reusable buffers.
pub struct GpuEffectsPipeline {
    device: wgpu::Device,
    queue: wgpu::Queue,
    // Compute pipelines — effects (2-buffer: input, output, params)
    opacity_pipeline: wgpu::ComputePipeline,
    blur_h_pipeline: wgpu::ComputePipeline,
    blur_v_pipeline: wgpu::ComputePipeline,
    sharpen_pipeline: wgpu::ComputePipeline,
    crop_pipeline: wgpu::ComputePipeline,
    transform_pipeline: wgpu::ComputePipeline,
    chroma_key_pipeline: wgpu::ComputePipeline,
    // Compute pipelines — light effects
    bloom_threshold_pipeline: wgpu::ComputePipeline,
    bloom_blend_pipeline: wgpu::ComputePipeline,
    glow_blend_pipeline: wgpu::ComputePipeline,
    light_rays_pipeline: wgpu::ComputePipeline,
    vignette_pipeline: wgpu::ComputePipeline,
    lens_flare_pipeline: wgpu::ComputePipeline,
    anamorphic_streak_pipeline: wgpu::ComputePipeline,
    film_halation_blend_pipeline: wgpu::ComputePipeline,
    // Compute pipelines — new effects
    invert_pipeline: wgpu::ComputePipeline,
    posterize_pipeline: wgpu::ComputePipeline,
    mosaic_pipeline: wgpu::ComputePipeline,
    edge_detect_pipeline: wgpu::ComputePipeline,
    channel_mixer_pipeline: wgpu::ComputePipeline,
    directional_blur_pipeline: wgpu::ComputePipeline,
    emboss_pipeline: wgpu::ComputePipeline,
    mirror_pipeline: wgpu::ComputePipeline,
    lens_distortion_pipeline: wgpu::ComputePipeline,
    dilate_pipeline: wgpu::ComputePipeline,
    erode_pipeline: wgpu::ComputePipeline,
    denoise_pipeline: wgpu::ComputePipeline,
    gamma_pipeline: wgpu::ComputePipeline,
    hsv_adjust_pipeline: wgpu::ComputePipeline,
    noise_generator_pipeline: wgpu::ComputePipeline,
    median_filter_pipeline: wgpu::ComputePipeline,
    radial_blur_pipeline: wgpu::ComputePipeline,
    levels_pipeline: wgpu::ComputePipeline,
    equalize_pipeline: wgpu::ComputePipeline,
    letterbox_pipeline: wgpu::ComputePipeline,
    chromatic_aberration_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — rolling shutter correction (uses huesat 4-binding layout)
    rsc_pipeline: wgpu::ComputePipeline,
    rsc_row_buf: Option<wgpu::Buffer>,
    rsc_row_buf_size: u64,
    // Compute pipeline — video stabilization (uses huesat 4-binding layout)
    stabilization_pipeline: wgpu::ComputePipeline,
    stabilization_buf: Option<wgpu::Buffer>,
    stabilization_buf_size: u64,
    // Compute pipelines — color grading (standalone, not part of effect chain)
    color_grade_pipeline: wgpu::ComputePipeline,
    huesat_pipeline: wgpu::ComputePipeline,
    huesat_bgl: wgpu::BindGroupLayout,
    huesat_lut_buf: wgpu::Buffer,
    // Compute pipeline — 3D LUT (reuses huesat_bgl: input, output, params, lut storage)
    lut3d_pipeline: wgpu::ComputePipeline,
    lut3d_buf: Option<wgpu::Buffer>,
    lut3d_buf_size: u64,
    // Compute pipeline — color balance (temperature/tint/hue)
    color_balance_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — wheel sat/bri (per-wheel saturation & brightness)
    wheel_sat_bri_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — RGB curves (LUT-based, reuses huesat_bgl 4-binding layout)
    rgb_curves_pipeline: wgpu::ComputePipeline,
    rgb_curves_lut_buf: wgpu::Buffer,
    // Compute pipeline — vibrance
    vibrance_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — color warper (12-sector hue/sat)
    color_warper_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — input transform (log-to-sRGB via 1D LUT, reuses huesat_bgl)
    input_transform_pipeline: wgpu::ComputePipeline,
    input_transform_lut_buf: wgpu::Buffer,
    // Compute pipelines — clarity (3-pass: blur_h, blur_v, apply)
    clarity_blur_h_pipeline: wgpu::ComputePipeline,
    clarity_blur_v_pipeline: wgpu::ComputePipeline,
    clarity_apply_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — mask matte generation (SDF power windows)
    mask_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — qualifier (secondary color correction)
    qualifier_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — film grain overlay
    grain_pipeline: wgpu::ComputePipeline,
    // Compute pipelines — alpha mask operations (for title renderer)
    alpha_blur_h_pipeline: wgpu::ComputePipeline,
    alpha_blur_v_pipeline: wgpu::ComputePipeline,
    alpha_dilate_pipeline: wgpu::ComputePipeline,
    // Compute pipelines — transitions (3-buffer: from, to, output, params)
    transition_pipeline: wgpu::ComputePipeline,
    // ── Scope rendering (GPU-accelerated visualization) ──
    scope_histogram_pipeline: wgpu::ComputePipeline,
    scope_waveform_pipeline: wgpu::ComputePipeline,
    scope_parade_pipeline: wgpu::ComputePipeline,
    scope_render_wf_pipeline: wgpu::ComputePipeline,
    scope_render_parade_pipeline: wgpu::ComputePipeline,
    scope_render_hist_pipeline: wgpu::ComputePipeline,
    scope_bgl: wgpu::BindGroupLayout,                    // 3-binding: storage_read, storage_rw, uniform
    scope_bins_buf: Option<wgpu::Buffer>,
    scope_bins_size: u64,
    scope_img_buf: Option<wgpu::Buffer>,
    scope_img_size: u64,
    scope_staging_buf: Option<wgpu::Buffer>,
    scope_staging_size: u64,
    scope_params_buf: wgpu::Buffer,
    // Bind group layouts
    bind_group_layout: wgpu::BindGroupLayout,         // effects: input, output, params
    transition_bgl: wgpu::BindGroupLayout,             // transitions: from, to, output, params
    // Reusable buffers — resized when frame dimensions change
    buf_a: Option<wgpu::Buffer>,
    buf_b: Option<wgpu::Buffer>,
    buf_c: Option<wgpu::Buffer>,  // third buffer for transition "to" frame
    staging_read: Option<wgpu::Buffer>,
    params_buf: wgpu::Buffer,
    current_buf_size: u64,
}

/// Maximum params size (padded to 256 bytes for alignment)
const PARAMS_SIZE: u64 = 256;
const WORKGROUP_SIZE: u32 = 256;

impl GpuEffectsPipeline {
    /// Create a new GPU effects pipeline. Returns `None` if no suitable GPU is found.
    pub fn new() -> Option<Self> {
        // Ensure Vulkan ICD is discoverable (some distros put it in /usr/share not /etc)
        if std::env::var("VK_DRIVER_FILES").is_err() {
            let icd_path = "/usr/share/vulkan/icd.d/nvidia_icd.json";
            if std::path::Path::new(icd_path).exists() {
                std::env::set_var("VK_DRIVER_FILES", icd_path);
                debug!("GPU effects: set VK_DRIVER_FILES={icd_path}");
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::from_build_config() & !wgpu::InstanceFlags::VALIDATION,
            ..Default::default()
        });

        let power_pref = GPU_POWER_PREF.lock().ok()
            .and_then(|g| *g)
            .unwrap_or(wgpu::PowerPreference::HighPerformance);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: power_pref,
            force_fallback_adapter: false,
            compatible_surface: None,
        }));

        let adapter = match adapter {
            Some(a) => a,
            None => {
                warn!("GPU effects: no suitable adapter found — falling back to CPU");
                return None;
            }
        };

        let adapter_info = adapter.get_info();
        info!(
            "GPU effects: {} ({:?})",
            adapter_info.name, adapter_info.backend
        );

        let (device, queue) = match pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("klipp-effects"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            },
            None,
        )) {
            Ok(dq) => dq,
            Err(e) => {
                warn!("GPU effects: device creation failed: {e} — falling back to CPU");
                return None;
            }
        };

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effects_bgl"),
            entries: &[
                // binding 0: input (read-only storage)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 1: output (read-write storage)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 2: params (uniform)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Transition bind group layout: from (read), to (read), output (write), params (uniform)
        let transition_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transition_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let transition_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transition_pl"),
            bind_group_layouts: &[&transition_bgl],
            push_constant_ranges: &[],
        });

        // Push error handler to capture shader validation errors
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let pipeline_compile_start = std::time::Instant::now();

        let create_pipeline = |dev: &wgpu::Device, layout: &wgpu::PipelineLayout, label: &str, shader_src: &str| -> wgpu::ComputePipeline {
            let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(shader_src.into()),
            });
            dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            })
        };

        let opacity_pipeline = create_pipeline(&device, &pipeline_layout, "opacity", SHADER_OPACITY);
        let blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "blur_h", SHADER_BLUR_H);
        let blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "blur_v", SHADER_BLUR_V);
        let sharpen_pipeline = create_pipeline(&device, &pipeline_layout, "sharpen", SHADER_SHARPEN);
        let crop_pipeline = create_pipeline(&device, &pipeline_layout, "crop", SHADER_CROP);
        let transform_pipeline = create_pipeline(&device, &pipeline_layout, "transform", SHADER_TRANSFORM);
        let chroma_key_pipeline = create_pipeline(&device, &pipeline_layout, "chroma_key", SHADER_CHROMA_KEY);
        let bloom_threshold_pipeline = create_pipeline(&device, &pipeline_layout, "bloom_threshold", SHADER_BLOOM_THRESHOLD);
        // Bloom/glow blend uses 4-binding layout (input, bright/blurred, output, params) — same as transitions
        let bloom_blend_pipeline = create_pipeline(&device, &transition_pl, "bloom_blend", SHADER_BLOOM_BLEND);
        let glow_blend_pipeline = create_pipeline(&device, &transition_pl, "glow_blend", SHADER_GLOW_BLEND);
        let light_rays_pipeline = create_pipeline(&device, &pipeline_layout, "light_rays", SHADER_LIGHT_RAYS);
        let vignette_pipeline = create_pipeline(&device, &pipeline_layout, "vignette", SHADER_VIGNETTE);
        let lens_flare_pipeline = create_pipeline(&device, &pipeline_layout, "lens_flare", SHADER_LENS_FLARE);
        let anamorphic_streak_pipeline = create_pipeline(&device, &pipeline_layout, "anamorphic_streak", SHADER_ANAMORPHIC_STREAK);
        let film_halation_blend_pipeline = create_pipeline(&device, &transition_pl, "film_halation_blend", SHADER_FILM_HALATION);
        let invert_pipeline = create_pipeline(&device, &pipeline_layout, "invert", SHADER_INVERT);
        let posterize_pipeline = create_pipeline(&device, &pipeline_layout, "posterize", SHADER_POSTERIZE);
        let mosaic_pipeline = create_pipeline(&device, &pipeline_layout, "mosaic", SHADER_MOSAIC);
        let edge_detect_pipeline = create_pipeline(&device, &pipeline_layout, "edge_detect", SHADER_EDGE_DETECT);
        let channel_mixer_pipeline = create_pipeline(&device, &pipeline_layout, "channel_mixer", SHADER_CHANNEL_MIXER);
        let directional_blur_pipeline = create_pipeline(&device, &pipeline_layout, "directional_blur", SHADER_DIRECTIONAL_BLUR);
        let emboss_pipeline = create_pipeline(&device, &pipeline_layout, "emboss", SHADER_EMBOSS);
        let mirror_pipeline = create_pipeline(&device, &pipeline_layout, "mirror", SHADER_MIRROR);
        let lens_distortion_pipeline = create_pipeline(&device, &pipeline_layout, "lens_distortion", SHADER_LENS_DISTORTION);
        let dilate_pipeline = create_pipeline(&device, &pipeline_layout, "dilate", SHADER_DILATE);
        let erode_pipeline = create_pipeline(&device, &pipeline_layout, "erode", SHADER_ERODE);
        let denoise_pipeline = create_pipeline(&device, &pipeline_layout, "denoise", SHADER_DENOISE);
        let gamma_pipeline = create_pipeline(&device, &pipeline_layout, "gamma", SHADER_GAMMA);
        let hsv_adjust_pipeline = create_pipeline(&device, &pipeline_layout, "hsv_adjust", SHADER_HSV_ADJUST);
        let noise_generator_pipeline = create_pipeline(&device, &pipeline_layout, "noise_generator", SHADER_NOISE_GENERATOR);
        let median_filter_pipeline = create_pipeline(&device, &pipeline_layout, "median_filter", SHADER_MEDIAN_FILTER);
        let radial_blur_pipeline = create_pipeline(&device, &pipeline_layout, "radial_blur", SHADER_RADIAL_BLUR);
        let levels_pipeline = create_pipeline(&device, &pipeline_layout, "levels", SHADER_LEVELS);
        let letterbox_pipeline = create_pipeline(&device, &pipeline_layout, "letterbox", SHADER_LETTERBOX);
        let chromatic_aberration_pipeline = create_pipeline(&device, &pipeline_layout, "chromatic_aberration", SHADER_CHROMATIC_ABERRATION);
        let color_grade_pipeline = create_pipeline(&device, &pipeline_layout, "color_grade", SHADER_COLOR_GRADE);

        // Hue/Sat pipeline: 4 bindings (input, output, params, LUT storage)
        let huesat_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("huesat_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let huesat_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("huesat_pl"),
            bind_group_layouts: &[&huesat_bgl],
            push_constant_ranges: &[],
        });
        let huesat_pipeline = create_pipeline(&device, &huesat_pl, "huesat", SHADER_HUESAT);
        let lut3d_pipeline = create_pipeline(&device, &huesat_pl, "lut3d", SHADER_LUT3D);
        let equalize_pipeline = create_pipeline(&device, &huesat_pl, "equalize", SHADER_EQUALIZE);
        let rsc_pipeline = create_pipeline(&device, &huesat_pl, "rolling_shutter", SHADER_ROLLING_SHUTTER);
        let stabilization_pipeline = create_pipeline(&device, &huesat_pl, "stabilization", SHADER_STABILIZATION);
        let vibrance_pipeline = create_pipeline(&device, &pipeline_layout, "vibrance", SHADER_VIBRANCE);
        let color_warper_pipeline = create_pipeline(&device, &pipeline_layout, "color_warper", SHADER_COLOR_WARPER);
        let input_transform_pipeline = create_pipeline(&device, &huesat_pl, "input_transform", SHADER_INPUT_TRANSFORM);
        let clarity_blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "clarity_blur_h", SHADER_CLARITY_BLUR_H);
        let clarity_blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "clarity_blur_v", SHADER_CLARITY_BLUR_V);
        let clarity_apply_pipeline = create_pipeline(&device, &huesat_pl, "clarity_apply", SHADER_CLARITY_APPLY);
        let mask_pipeline = create_pipeline(&device, &pipeline_layout, "mask", SHADER_MASK);
        let color_balance_pipeline = create_pipeline(&device, &pipeline_layout, "color_balance", SHADER_COLOR_BALANCE);
        let wheel_sat_bri_pipeline = create_pipeline(&device, &pipeline_layout, "wheel_sat_bri", SHADER_WHEEL_SAT_BRI);
        let rgb_curves_pipeline = create_pipeline(&device, &huesat_pl, "rgb_curves", SHADER_RGB_CURVES);
        let qualifier_pipeline = create_pipeline(&device, &pipeline_layout, "qualifier", SHADER_QUALIFIER);
        let grain_pipeline = create_pipeline(&device, &pipeline_layout, "grain", SHADER_GRAIN);
        let alpha_blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_blur_h", SHADER_ALPHA_BLUR_H);
        let alpha_blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_blur_v", SHADER_ALPHA_BLUR_V);
        let alpha_dilate_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_dilate", SHADER_ALPHA_DILATE);

        // LUT buffer: 6 curves × 256 entries × 4 bytes = 6144 bytes
        let huesat_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("huesat_lut"),
            size: 6 * 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // RGB curves LUT buffer: 4 channels × 256 entries × 4 bytes = 4096 bytes
        let rgb_curves_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rgb_curves_lut"),
            size: 4 * 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Input transform LUT buffer: 256 entries × 4 bytes = 1024 bytes
        let input_transform_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("input_transform_lut"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let transition_pipeline = create_pipeline(&device, &transition_pl, "transition", SHADER_TRANSITION);

        // ── Scope rendering pipelines ──
        // Scope accumulation: input(read), bins(read_write atomic), params(uniform)
        let scope_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scope_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let scope_accum_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scope_accum_pl"), bind_group_layouts: &[&scope_bgl], push_constant_ranges: &[],
        });
        // Scope render uses same 3-binding layout (bins_read, output_write, params)
        let scope_render_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scope_render_pl"), bind_group_layouts: &[&scope_bgl], push_constant_ranges: &[],
        });

        let scope_waveform_pipeline = create_pipeline(&device, &scope_accum_pl, "scope_wf_accum", SHADER_SCOPE_WAVEFORM);
        let scope_parade_pipeline = create_pipeline(&device, &scope_accum_pl, "scope_parade_accum", SHADER_SCOPE_PARADE);
        let scope_histogram_pipeline = create_pipeline(&device, &scope_accum_pl, "scope_hist_accum", SHADER_SCOPE_HISTOGRAM);
        let scope_render_wf_pipeline = create_pipeline(&device, &scope_render_pl, "scope_render_wf", SHADER_SCOPE_RENDER_WF);
        let scope_render_parade_pipeline = create_pipeline(&device, &scope_render_pl, "scope_render_parade", SHADER_SCOPE_RENDER_PARADE);
        let scope_render_hist_pipeline = create_pipeline(&device, &scope_render_pl, "scope_render_hist", SHADER_SCOPE_RENDER_HIST);

        let scope_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scope_params"), size: PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });

        let pipeline_count = 61;
        info!(
            "GPU effects: compiled {} pipelines in {:.0}ms",
            pipeline_count,
            pipeline_compile_start.elapsed().as_secs_f64() * 1000.0,
        );

        // Check if any shader compilation failed
        if let Some(err) = pollster::block_on(device.pop_error_scope()) {
            warn!("GPU effects: shader compilation failed: {err} — falling back to CPU");
            return None;
        }

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        info!("GPU effects pipeline initialized");

        Some(Self {
            device,
            queue,
            opacity_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            sharpen_pipeline,
            crop_pipeline,
            transform_pipeline,
            chroma_key_pipeline,
            bloom_threshold_pipeline,
            bloom_blend_pipeline,
            glow_blend_pipeline,
            light_rays_pipeline,
            vignette_pipeline,
            lens_flare_pipeline,
            anamorphic_streak_pipeline,
            film_halation_blend_pipeline,
            invert_pipeline,
            posterize_pipeline,
            mosaic_pipeline,
            edge_detect_pipeline,
            channel_mixer_pipeline,
            directional_blur_pipeline,
            emboss_pipeline,
            mirror_pipeline,
            lens_distortion_pipeline,
            dilate_pipeline,
            erode_pipeline,
            denoise_pipeline,
            gamma_pipeline,
            hsv_adjust_pipeline,
            noise_generator_pipeline,
            median_filter_pipeline,
            radial_blur_pipeline,
            levels_pipeline,
            equalize_pipeline,
            letterbox_pipeline,
            chromatic_aberration_pipeline,
            rsc_pipeline,
            rsc_row_buf: None,
            rsc_row_buf_size: 0,
            stabilization_pipeline,
            stabilization_buf: None,
            stabilization_buf_size: 0,
            color_grade_pipeline,
            huesat_pipeline,
            huesat_bgl,
            huesat_lut_buf,
            lut3d_pipeline,
            lut3d_buf: None,
            lut3d_buf_size: 0,
            vibrance_pipeline,
            color_warper_pipeline,
            input_transform_pipeline,
            input_transform_lut_buf,
            clarity_blur_h_pipeline,
            clarity_blur_v_pipeline,
            clarity_apply_pipeline,
            mask_pipeline,
            color_balance_pipeline,
            wheel_sat_bri_pipeline,
            rgb_curves_pipeline,
            rgb_curves_lut_buf,
            qualifier_pipeline,
            grain_pipeline,
            alpha_blur_h_pipeline,
            alpha_blur_v_pipeline,
            alpha_dilate_pipeline,
            transition_pipeline,
            scope_histogram_pipeline,
            scope_waveform_pipeline,
            scope_parade_pipeline,
            scope_render_wf_pipeline,
            scope_render_parade_pipeline,
            scope_render_hist_pipeline,
            scope_bgl,
            scope_bins_buf: None,
            scope_bins_size: 0,
            scope_img_buf: None,
            scope_img_size: 0,
            scope_staging_buf: None,
            scope_staging_size: 0,
            scope_params_buf,
            bind_group_layout,
            transition_bgl,
            buf_a: None,
            buf_b: None,
            buf_c: None,
            staging_read: None,
            params_buf,
            current_buf_size: 0,
        })
    }

    /// Ensure GPU buffers are large enough for the frame.
    fn ensure_buffers(&mut self, byte_size: u64) {
        if byte_size <= self.current_buf_size {
            return;
        }
        debug!("GPU effects: resizing buffers to {byte_size} bytes");
        self.buf_a = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_a"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.buf_b = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_b"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.buf_c = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_c"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.staging_read = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_staging_read"),
            size: byte_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.current_buf_size = byte_size;
    }

    /// Create a bind group with the given input/output buffers.
    fn bind_group(&self, input: &wgpu::Buffer, output: &wgpu::Buffer) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params_buf.as_entire_binding(),
                },
            ],
        })
    }

    /// Apply per-clip color grading on the GPU. Returns the processed RGBA data.
    /// Accepts the pre-converted lift/gamma/gain RGB tuples from wheel_offset_to_rgb.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn apply_color_grade(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        exposure: f32,
        contrast: f32,
        saturation: f32,
        temperature: f32,
        tint: f32,
        highlights: f32,
        shadows: f32,
        whites: f32,
        blacks: f32,
        lift: (f32, f32, f32),
        gamma: (f32, f32, f32),
        gain: (f32, f32, f32),
        offset: (f32, f32, f32),
        hue: f32,
        global_mix: f32,
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuColorGradeParams {
            width,
            height,
            exposure,
            contrast,
            saturation,
            temperature,
            tint,
            highlights,
            shadows,
            whites,
            blacks,
            lift_r: lift.0,
            lift_g: lift.1,
            lift_b: lift.2,
            gamma_r: gamma.0,
            gamma_g: gamma.1,
            gamma_b: gamma.2,
            gain_r: gain.0,
            gain_g: gain.1,
            gain_b: gain.2,
            offset_r: offset.0,
            offset_g: offset.1,
            offset_b: offset.2,
            hue,
            global_mix,
            _pad0: 0,
        });
        self.dispatch(&self.color_grade_pipeline, buf_a, buf_b, workgroups);

        self.download(buf_b, byte_size)
    }

    /// Apply color balance (temperature, tint, hue rotation, global mix) on GPU.
    pub fn apply_color_balance_gpu(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        temperature: f32,
        tint: f32,
        hue: f32,
        global_mix: f32,
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuColorBalanceParams {
            width, height, temperature, tint, hue, global_mix,
            _pad0: 0, _pad1: 0,
        });
        self.dispatch(&self.color_balance_pipeline, buf_a, buf_b, workgroups);

        self.download(buf_b, byte_size)
    }

    /// Apply per-wheel saturation & brightness adjustments on GPU.
    pub fn apply_wheel_sat_bri_gpu(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        lift_sat: f32, lift_bri: f32,
        gamma_sat: f32, gamma_bri: f32,
        gain_sat: f32, gain_bri: f32,
        offset_sat: f32, offset_bri: f32,
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuWheelSatBriParams {
            width, height,
            lift_sat, lift_bri, gamma_sat, gamma_bri,
            gain_sat, gain_bri, offset_sat, offset_bri,
            _pad0: 0, _pad1: 0,
        });
        self.dispatch(&self.wheel_sat_bri_pipeline, buf_a, buf_b, workgroups);

        self.download(buf_b, byte_size)
    }

    /// Apply RGB curves on GPU using pre-computed LUTs.
    /// `lut_data` must be 4×256 f32 values (master, red, green, blue).
    pub fn apply_rgb_curves_gpu(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        lut_data: &[f32; 1024], // 4 × 256
        preserve_luma: bool,
        mix: f32,
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        // Upload curve LUT data
        self.queue.write_buffer(&self.rgb_curves_lut_buf, 0, bytemuck::cast_slice(lut_data));

        self.write_params(&GpuRgbCurvesParams {
            width, height,
            preserve_luma: if preserve_luma { 1 } else { 0 },
            mix,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        // Create bind group with 4 bindings (reuse huesat_bgl layout)
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rgb_curves_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.rgb_curves_lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.rgb_curves_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        self.download(buf_b, byte_size)
    }

    // ══════════════════════════════════════════════════════════════════════════
    // GPU Scope Rendering — produces ready-to-display RGBA scope textures
    // ══════════════════════════════════════════════════════════════════════════

    fn ensure_scope_bufs(&mut self, bins_count: u64, img_pixels: u64) {
        let bins_bytes = bins_count * 4;
        let img_bytes = img_pixels * 4;
        if self.scope_bins_size < bins_bytes {
            self.scope_bins_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scope_bins"), size: bins_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.scope_bins_size = bins_bytes;
        }
        if self.scope_img_size < img_bytes {
            self.scope_img_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scope_img"), size: img_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.scope_img_size = img_bytes;
        }
        if self.scope_staging_size < img_bytes {
            self.scope_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scope_staging"), size: img_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.scope_staging_size = img_bytes;
        }
    }

    fn scope_download(&self, pixels: u64) -> Vec<u8> {
        let byte_size = pixels * 4;
        let img_buf = self.scope_img_buf.as_ref().unwrap();
        let staging = self.scope_staging_buf.as_ref().unwrap();
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(img_buf, 0, staging, 0, byte_size);
        self.queue.submit(std::iter::once(encoder.finish()));
        let slice = staging.slice(..byte_size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().ok().and_then(|r| r.ok());
        let data = slice.get_mapped_range();
        let result = data.to_vec();
        drop(data);
        staging.unmap();
        result
    }

    /// Render a waveform scope as RGBA image. Returns (rgba, width, height).
    pub fn render_scope_waveform(&mut self, rgba: &[u8], frame_w: u32, frame_h: u32, out_w: u32, out_h: u32) -> Vec<u8> {
        let pixel_count = (frame_w * frame_h) as u64;
        let display_cols = out_w.min(frame_w);
        let num_bins = (display_cols as u64) * 256;
        let out_pixels = (out_w * out_h) as u64;
        let step = if frame_w > 1280 { 2u32 } else { 1 };

        self.ensure_buffers((pixel_count * 4).max(out_pixels * 4));
        self.ensure_scope_bufs(num_bins, out_pixels);

        let buf_a = self.buf_a.as_ref().unwrap();
        let bins_buf = self.scope_bins_buf.as_ref().unwrap();
        let img_buf = self.scope_img_buf.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, rgba);

        // 1. Clear bins
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.clear_buffer(bins_buf, 0, None);

        // 2. Accumulate waveform bins
        self.queue.write_buffer(&self.scope_params_buf, 0, bytemuck::bytes_of(&GpuScopeAccumParams {
            width: frame_w, height: frame_h, display_columns: display_cols, step,
        }));
        let bg1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.scope_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.scope_params_buf.as_entire_binding() },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.scope_waveform_pipeline);
            pass.set_bind_group(0, &bg1, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        // 3. Find max bin count for normalization
        // (Read back bins to find max — small data, fast)
        let max_count = {
            let bins_bytes = num_bins * 4;
            // Ensure staging is big enough for bins too
            if self.scope_staging_size < bins_bytes {
                self.scope_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("scope_staging"), size: bins_bytes.max(out_pixels * 4),
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.scope_staging_size = bins_bytes.max(out_pixels * 4);
            }
            let staging = self.scope_staging_buf.as_ref().unwrap();
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            enc.copy_buffer_to_buffer(bins_buf, 0, staging, 0, bins_bytes);
            self.queue.submit(std::iter::once(enc.finish()));
            let slice = staging.slice(..bins_bytes);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
            self.device.poll(wgpu::Maintain::Wait);
            rx.recv().ok().and_then(|r| r.ok());
            let data = slice.get_mapped_range();
            let bins: &[u32] = bytemuck::cast_slice(&data);
            let mx = bins.iter().copied().max().unwrap_or(1).max(1);
            drop(data);
            staging.unmap();
            mx
        };

        // 4. Render visualization
        self.queue.write_buffer(&self.scope_params_buf, 0, bytemuck::bytes_of(&GpuScopeRenderParams {
            out_w, out_h, num_cols: display_cols, max_count,
        }));
        let bg2 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.scope_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: img_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.scope_params_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.scope_render_wf_pipeline);
            pass.set_bind_group(0, &bg2, &[]);
            pass.dispatch_workgroups((out_pixels as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        self.scope_download(out_pixels)
    }

    /// Render a parade scope as RGBA image.
    pub fn render_scope_parade(&mut self, rgba: &[u8], frame_w: u32, frame_h: u32, out_w: u32, out_h: u32) -> Vec<u8> {
        let pixel_count = (frame_w * frame_h) as u64;
        let gap = 3u32;
        let section_w = (out_w - gap * 2) / 3;
        let display_cols = section_w.min(frame_w);
        let num_bins = (display_cols as u64) * 256 * 3;
        let out_pixels = (out_w * out_h) as u64;
        let step = if frame_w > 1280 { 2u32 } else { 1 };

        self.ensure_buffers((pixel_count * 4).max(out_pixels * 4));
        self.ensure_scope_bufs(num_bins, out_pixels);

        let buf_a = self.buf_a.as_ref().unwrap();
        let bins_buf = self.scope_bins_buf.as_ref().unwrap();
        let img_buf = self.scope_img_buf.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, rgba);

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.clear_buffer(bins_buf, 0, None);
        self.queue.write_buffer(&self.scope_params_buf, 0, bytemuck::bytes_of(&GpuScopeAccumParams {
            width: frame_w, height: frame_h, display_columns: display_cols, step,
        }));
        let bg1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.scope_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.scope_params_buf.as_entire_binding() },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.scope_parade_pipeline);
            pass.set_bind_group(0, &bg1, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        // Find max count
        let max_count = {
            let bins_bytes = num_bins * 4;
            if self.scope_staging_size < bins_bytes {
                self.scope_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("scope_staging"), size: bins_bytes.max(out_pixels * 4),
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.scope_staging_size = bins_bytes.max(out_pixels * 4);
            }
            let staging = self.scope_staging_buf.as_ref().unwrap();
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            enc.copy_buffer_to_buffer(bins_buf, 0, staging, 0, bins_bytes);
            self.queue.submit(std::iter::once(enc.finish()));
            let slice = staging.slice(..bins_bytes);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
            self.device.poll(wgpu::Maintain::Wait);
            rx.recv().ok().and_then(|r| r.ok());
            let data = slice.get_mapped_range();
            let bins: &[u32] = bytemuck::cast_slice(&data);
            let mx = bins.iter().copied().max().unwrap_or(1).max(1);
            drop(data);
            staging.unmap();
            mx
        };

        // Render
        self.queue.write_buffer(&self.scope_params_buf, 0, bytemuck::bytes_of(&GpuScopeRenderParams {
            out_w, out_h, num_cols: display_cols, max_count,
        }));
        let bg2 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.scope_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: img_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.scope_params_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.scope_render_parade_pipeline);
            pass.set_bind_group(0, &bg2, &[]);
            pass.dispatch_workgroups((out_pixels as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        self.scope_download(out_pixels)
    }

    /// Render a histogram scope as RGBA image.
    pub fn render_scope_histogram(&mut self, rgba: &[u8], frame_w: u32, frame_h: u32, out_w: u32, out_h: u32) -> Vec<u8> {
        let pixel_count = (frame_w * frame_h) as u64;
        let num_bins: u64 = 4 * 256;
        let out_pixels = (out_w * out_h) as u64;
        let step = if frame_w > 1280 { 2u32 } else { 1 };

        self.ensure_buffers((pixel_count * 4).max(out_pixels * 4));
        self.ensure_scope_bufs(num_bins, out_pixels);

        let buf_a = self.buf_a.as_ref().unwrap();
        let bins_buf = self.scope_bins_buf.as_ref().unwrap();
        let img_buf = self.scope_img_buf.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, rgba);

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.clear_buffer(bins_buf, 0, None);
        self.queue.write_buffer(&self.scope_params_buf, 0, bytemuck::bytes_of(&GpuScopeAccumParams {
            width: frame_w, height: frame_h, display_columns: 0, step,
        }));
        let bg1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.scope_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.scope_params_buf.as_entire_binding() },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.scope_histogram_pipeline);
            pass.set_bind_group(0, &bg1, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        // Find max
        let max_count = {
            let bins_bytes = num_bins * 4;
            if self.scope_staging_size < bins_bytes {
                self.scope_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("scope_staging"), size: bins_bytes.max(out_pixels * 4),
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
                self.scope_staging_size = bins_bytes.max(out_pixels * 4);
            }
            let staging = self.scope_staging_buf.as_ref().unwrap();
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            enc.copy_buffer_to_buffer(bins_buf, 0, staging, 0, bins_bytes);
            self.queue.submit(std::iter::once(enc.finish()));
            let slice = staging.slice(..bins_bytes);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
            self.device.poll(wgpu::Maintain::Wait);
            rx.recv().ok().and_then(|r| r.ok());
            let data = slice.get_mapped_range();
            let bins: &[u32] = bytemuck::cast_slice(&data);
            let mx = bins.iter().copied().max().unwrap_or(1).max(1);
            drop(data);
            staging.unmap();
            mx
        };

        // Render
        self.queue.write_buffer(&self.scope_params_buf, 0, bytemuck::bytes_of(&GpuScopeRenderParams {
            out_w, out_h, num_cols: 0, max_count,
        }));
        let bg2 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &self.scope_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: bins_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: img_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.scope_params_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.scope_render_hist_pipeline);
            pass.set_bind_group(0, &bg2, &[]);
            pass.dispatch_workgroups((out_pixels as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        self.scope_download(out_pixels)
    }

    /// Apply vibrance on GPU.
    pub fn apply_vibrance_gpu(&mut self, rgba: Vec<u8>, width: u32, height: u32, amount: f32) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.queue.write_buffer(buf_a, 0, &rgba);
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuVibranceParams { width, height, amount, _pad: 0 });
        self.dispatch(&self.vibrance_pipeline, buf_a, buf_b, workgroups);
        self.download(buf_b, byte_size)
    }

    /// Apply 12-sector color warper on GPU.
    pub fn apply_color_warper_gpu(&mut self, rgba: Vec<u8>, width: u32, height: u32, hue_shifts: &[f32; 12], sat_shifts: &[f32; 12]) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.queue.write_buffer(buf_a, 0, &rgba);
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuColorWarperParams {
            width, height, _pad0: 0, _pad1: 0,
            hue_shifts: *hue_shifts, sat_shifts: *sat_shifts,
        });
        self.dispatch(&self.color_warper_pipeline, buf_a, buf_b, workgroups);
        self.download(buf_b, byte_size)
    }

    /// Apply input transform (log-to-sRGB) on GPU using a pre-computed 1D LUT.
    pub fn apply_input_transform_gpu(&mut self, rgba: Vec<u8>, width: u32, height: u32, lut_data: &[f32; 256]) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.queue.write_buffer(buf_a, 0, &rgba);
        self.queue.write_buffer(&self.input_transform_lut_buf, 0, bytemuck::cast_slice(lut_data));
        self.write_params(&GpuInputTransformParams { width, height, _pad0: 0, _pad1: 0 });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("input_transform_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.input_transform_lut_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.input_transform_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.download(buf_b, byte_size)
    }

    /// Apply clarity (midtone unsharp mask) on GPU using 3-pass approach.
    pub fn apply_clarity_gpu(&mut self, rgba: Vec<u8>, width: u32, height: u32, amount: f32) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        let _luma_size = (width * height * 4) as u64; // store luma as u32 per pixel
        self.ensure_buffers(byte_size);
        // Need a third buffer for blur intermediate
        if self.buf_c.is_none() || self.current_buf_size < byte_size {
            self.buf_c = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("buf_c"), size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let buf_c = self.buf_c.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let radius = 20u32;

        // Pass 1: horizontal blur (input RGBA → buf_b luma)
        self.write_params(&GpuClarityBlurParams { width, height, radius, _pad: 0 });
        self.dispatch(&self.clarity_blur_h_pipeline, buf_a, buf_b, workgroups);

        // Pass 2: vertical blur (buf_b luma → buf_c blurred luma)
        self.dispatch(&self.clarity_blur_v_pipeline, buf_b, buf_c, workgroups);

        // Pass 3: apply unsharp mask (input RGBA + blurred luma → output RGBA)
        self.write_params(&GpuClarityApplyParams { width, height, amount, _pad: 0 });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clarity_apply_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: buf_c.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.clarity_apply_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.download(buf_b, byte_size)
    }

    /// Apply bilateral denoise on GPU. Used by the color grading Denoise node.
    pub fn apply_denoise_gpu(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        spatial_sigma: f32,
        color_sigma: f32,
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&DenoiseParams {
            width, height, spatial_sigma, color_sigma,
        });
        self.dispatch(&self.denoise_pipeline, buf_a, buf_b, workgroups);

        self.download(buf_b, byte_size)
    }

    /// Generate a mask matte on GPU. Supports up to 4 masks (circle, rect, linear gradient).
    /// Returns greyscale RGBA (R=G=B=matte, A=255).
    pub fn generate_mask_gpu(
        &mut self,
        width: u32,
        height: u32,
        masks: &[GpuMaskDef],
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let mut p = GpuMaskParams {
            width, height, mask_count: masks.len().min(4) as u32, _pad: 0,
            m0_type: 3, m0_cx: 0.0, m0_cy: 0.0, m0_p1: 0.0, m0_p2: 0.0, m0_p3: 0.0, m0_feather: 0.0, m0_flags: 0,
            m1_type: 3, m1_cx: 0.0, m1_cy: 0.0, m1_p1: 0.0, m1_p2: 0.0, m1_p3: 0.0, m1_feather: 0.0, m1_flags: 0,
            m2_type: 3, m2_cx: 0.0, m2_cy: 0.0, m2_p1: 0.0, m2_p2: 0.0, m2_p3: 0.0, m2_feather: 0.0, m2_flags: 0,
            m3_type: 3, m3_cx: 0.0, m3_cy: 0.0, m3_p1: 0.0, m3_p2: 0.0, m3_p3: 0.0, m3_feather: 0.0, m3_flags: 0,
        };

        let set = |slot: usize, p: &mut GpuMaskParams, m: &GpuMaskDef| {
            let (ty, cx, cy, p1, p2, p3, feather, flags) = (m.shape_type, m.cx, m.cy, m.p1, m.p2, m.p3, m.feather, m.flags);
            match slot {
                0 => { p.m0_type = ty; p.m0_cx = cx; p.m0_cy = cy; p.m0_p1 = p1; p.m0_p2 = p2; p.m0_p3 = p3; p.m0_feather = feather; p.m0_flags = flags; }
                1 => { p.m1_type = ty; p.m1_cx = cx; p.m1_cy = cy; p.m1_p1 = p1; p.m1_p2 = p2; p.m1_p3 = p3; p.m1_feather = feather; p.m1_flags = flags; }
                2 => { p.m2_type = ty; p.m2_cx = cx; p.m2_cy = cy; p.m2_p1 = p1; p.m2_p2 = p2; p.m2_p3 = p3; p.m2_feather = feather; p.m2_flags = flags; }
                3 => { p.m3_type = ty; p.m3_cx = cx; p.m3_cy = cy; p.m3_p1 = p1; p.m3_p2 = p2; p.m3_p3 = p3; p.m3_feather = feather; p.m3_flags = flags; }
                _ => {}
            }
        };

        for (i, m) in masks.iter().enumerate().take(4) {
            set(i, &mut p, m);
        }

        self.write_params(&p);
        self.dispatch(&self.mask_pipeline, buf_a, buf_b, workgroups);

        self.download(buf_b, byte_size)
    }

    /// Apply hue/sat curves on the GPU using pre-computed LUTs.
    /// `lut_data` must be 6×256 f32 values (6 curves, 256 entries each).
    pub fn apply_huesat_gpu(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        mix: f32,
        lut_data: &[f32; 1536], // 6 × 256
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        // Upload LUT data
        self.queue.write_buffer(&self.huesat_lut_buf, 0, bytemuck::cast_slice(lut_data));

        // Write params
        self.write_params(&GpuHueSatParams {
            width,
            height,
            mix,
            _pad0: 0,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        // Create bind group with 4 bindings
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("huesat_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.huesat_lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.huesat_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        self.download(buf_b, byte_size)
    }

    /// Apply a 3D LUT with trilinear interpolation on GPU.
    /// `lut_data` is RGB triples in B-major order, `lut_size` is the grid dimension (e.g. 33).
    pub fn apply_3d_lut_gpu(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        lut_data: &[f32],
        lut_size: u32,
        opacity: f32,
    ) -> Vec<u8> {
        if opacity <= 0.0 {
            return rgba;
        }

        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        // (Re-)create LUT buffer if needed
        let lut_byte_size = (lut_data.len() * 4) as u64;
        if self.lut3d_buf.is_none() || self.lut3d_buf_size < lut_byte_size {
            self.lut3d_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lut3d_data"),
                size: lut_byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.lut3d_buf_size = lut_byte_size;
        }
        let lut_buf = self.lut3d_buf.as_ref().unwrap();
        self.queue.write_buffer(lut_buf, 0, bytemuck::cast_slice(lut_data));

        self.write_params(&GpuLut3dParams {
            width,
            height,
            lut_size,
            opacity,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut3d_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.lut3d_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        self.download(buf_b, byte_size)
    }

    /// Apply qualifier (secondary color correction) on GPU.
    /// Generates HSL matte and applies secondary grade in a single pass.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_qualifier_gpu(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        hue_center: f32, hue_width: f32, hue_soft: f32,
        sat_low: f32, sat_high: f32, sat_soft: f32,
        lum_low: f32, lum_high: f32, lum_soft: f32,
        invert: bool,
        sec_exposure: f32, sec_saturation: f32, sec_hue_shift: f32, sec_temperature: f32,
        matte_preview: bool,
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        self.write_params(&GpuQualifierParams {
            width,
            height,
            hue_center: hue_center / 360.0,
            hue_width: hue_width / 360.0,
            hue_soft: hue_soft / 360.0,
            sat_low,
            sat_high,
            sat_soft,
            lum_low,
            lum_high,
            lum_soft,
            invert: if invert { 1 } else { 0 },
            sec_exposure,
            sec_saturation,
            sec_hue_shift: sec_hue_shift.to_radians(),
            sec_temperature: sec_temperature / 100.0 * 0.1,
            matte_preview: if matte_preview { 1 } else { 0 },
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.dispatch(&self.qualifier_pipeline, buf_a, buf_b, workgroups);
        self.download(buf_b, byte_size)
    }

    /// Apply film grain overlay on GPU.
    pub fn apply_grain_gpu(
        &mut self,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        amount: f32,
        size: f32,
        shadows: f32,
        highlights: f32,
        monochrome: bool,
        frame_seed: u32,
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, &rgba);

        self.write_params(&GpuGrainParams {
            width,
            height,
            amount,
            size,
            shadows,
            highlights,
            frame_seed,
            monochrome: if monochrome { 1 } else { 0 },
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.dispatch(&self.grain_pipeline, buf_a, buf_b, workgroups);
        self.download(buf_b, byte_size)
    }

    /// Apply a chain of effects on the GPU. Returns the processed RGBA data.
    pub fn apply_effects(
        &mut self,
        effects: &[BuiltinEffect],
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        if effects.is_empty() {
            return rgba;
        }

        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        // Upload frame data to buf_a
        self.queue.write_buffer(buf_a, 0, &rgba);

        // Track which buffer has the current data (true = buf_a, false = buf_b)
        let mut current_in_a = true;
        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        for effect in effects {
            let (input, output) = if current_in_a {
                (buf_a, buf_b)
            } else {
                (buf_b, buf_a)
            };

            match effect {
                BuiltinEffect::Opacity { value } => {
                    if (*value - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&OpacityParams {
                        width,
                        height,
                        value: *value,
                        _pad: 0,
                    });
                    self.dispatch(&self.opacity_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::GaussianBlur { radius } => {
                    if *radius < 0.5 {
                        continue;
                    }
                    // 3-pass box blur to approximate Gaussian
                    let boxes = boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        if box_r == 0 {
                            continue;
                        }
                        let (inp, out) = if current_in_a {
                            (buf_a, buf_b)
                        } else {
                            (buf_b, buf_a)
                        };
                        // Horizontal pass
                        self.write_params(&BlurParams {
                            width,
                            height,
                            radius: box_r as i32,
                            _pad: 0,
                        });
                        self.dispatch(&self.blur_h_pipeline, inp, out, workgroups);
                        current_in_a = !current_in_a;

                        // Vertical pass
                        let (inp, out) = if current_in_a {
                            (buf_a, buf_b)
                        } else {
                            (buf_b, buf_a)
                        };
                        self.dispatch(&self.blur_v_pipeline, inp, out, workgroups);
                        current_in_a = !current_in_a;
                    }
                }
                BuiltinEffect::Sharpen { amount } => {
                    if amount.abs() < f32::EPSILON {
                        continue;
                    }
                    // Single-pass 3x3 unsharp mask — the shader samples neighbors directly
                    self.write_params(&SharpenParams {
                        width,
                        height,
                        amount: *amount,
                        _pad: 0,
                    });
                    self.dispatch(&self.sharpen_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Crop {
                    left,
                    right,
                    top,
                    bottom,
                    feather,
                } => {
                    if *left <= 0.0 && *right <= 0.0 && *top <= 0.0 && *bottom <= 0.0 {
                        continue;
                    }
                    self.write_params(&CropParams {
                        width,
                        height,
                        crop_left: *left * width as f32,
                        crop_right: (1.0 - *right) * width as f32,
                        crop_top: *top * height as f32,
                        crop_bottom: (1.0 - *bottom) * height as f32,
                        feather: *feather,
                        _pad: 0,
                    });
                    self.dispatch(&self.crop_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Transform {
                    position_x,
                    position_y,
                    scale_x,
                    scale_y,
                    rotation,
                    anchor_x,
                    anchor_y,
                    rotation_x,
                    rotation_y,
                    perspective,
                } => {
                    if *position_x == 0.0
                        && *position_y == 0.0
                        && (*scale_x - 1.0).abs() < f32::EPSILON
                        && (*scale_y - 1.0).abs() < f32::EPSILON
                        && rotation.abs() < f32::EPSILON
                        && rotation_x.abs() < f32::EPSILON
                        && rotation_y.abs() < f32::EPSILON
                    {
                        continue;
                    }
                    let rot_rad = rotation.to_radians();
                    let cos_r = rot_rad.cos();
                    let sin_r = rot_rad.sin();
                    let inv_sx = if scale_x.abs() > f32::EPSILON {
                        1.0 / scale_x
                    } else {
                        1.0
                    };
                    let inv_sy = if scale_y.abs() > f32::EPSILON {
                        1.0 / scale_y
                    } else {
                        1.0
                    };
                    let rx_rad = rotation_x.to_radians();
                    let ry_rad = rotation_y.to_radians();
                    let focal = if *perspective > 0.001 { width as f32 / *perspective } else { 0.0 };
                    self.write_params(&TransformParams {
                        width,
                        height,
                        anchor_x: *anchor_x * width as f32,
                        anchor_y: *anchor_y * height as f32,
                        position_x: *position_x,
                        position_y: *position_y,
                        cos_r,
                        sin_r,
                        inv_sx,
                        inv_sy,
                        cos_rx: rx_rad.cos(),
                        sin_rx: rx_rad.sin(),
                        cos_ry: ry_rad.cos(),
                        sin_ry: ry_rad.sin(),
                        focal,
                        _pad: [0; 3],
                    });
                    self.dispatch(&self.transform_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::ChromaKey {
                    key_color,
                    tolerance,
                    spill_suppression,
                } => {
                    // Convert key color to CbCr
                    let rf = key_color[0] as f32;
                    let gf = key_color[1] as f32;
                    let bf = key_color[2] as f32;
                    let key_cb = 128.0 + (-0.168736 * rf - 0.331264 * gf + 0.5 * bf);
                    let key_cr = 128.0 + (0.5 * rf - 0.418688 * gf - 0.081312 * bf);

                    self.write_params(&ChromaKeyParams {
                        width,
                        height,
                        key_cb,
                        key_cr,
                        tolerance: *tolerance,
                        spill_suppression: *spill_suppression,
                        _pad: [0; 2],
                    });
                    self.dispatch(&self.chroma_key_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Bloom { threshold, radius, intensity } => {
                    // Step 1: Extract bright pixels to buf_c
                    let buf_c = self.buf_c.as_ref().unwrap();
                    self.write_params(&BloomThresholdParams {
                        width, height,
                        threshold: *threshold,
                        _pad: 0,
                    });
                    self.dispatch(&self.bloom_threshold_pipeline, input, buf_c, workgroups);

                    // Step 2: Blur the bright pixels (3-pass box blur in buf_c)
                    let boxes = crate::blur::boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }

                    // Step 3: Additive blend blurred bright (buf_c) onto input
                    self.write_params(&BloomBlendParams {
                        width, height,
                        intensity: *intensity,
                        _pad: 0,
                    });
                    // bloom_blend reads input + buf_c, writes output
                    self.dispatch_bloom_blend(input, buf_c, output, workgroups, *intensity, width, height);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Glow { radius, intensity } => {
                    // Step 1: Blur entire image to buf_c
                    let buf_c = self.buf_c.as_ref().unwrap();
                    // Copy input to buf_c first
                    let byte_size = (width * height * 4) as u64;
                    {
                        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                        enc.copy_buffer_to_buffer(input, 0, buf_c, 0, byte_size);
                        self.queue.submit(Some(enc.finish()));
                    }

                    let boxes = crate::blur::boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }

                    // Step 2: Screen blend blurred (buf_c) with original (input) → output
                    self.dispatch_bloom_blend_screen(input, buf_c, output, workgroups, *intensity, width, height);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
                    self.write_params(&LightRaysParams {
                        width, height,
                        center_x: *center_x,
                        center_y: *center_y,
                        threshold: *threshold,
                        samples: (*samples).clamp(4, 128),
                        decay: *decay,
                        intensity: *intensity,
                    });
                    self.dispatch(&self.light_rays_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Vignette { amount, size, softness, roundness } => {
                    self.write_params(&VignetteParams {
                        width, height,
                        amount: *amount, size: *size, softness: *softness, roundness: *roundness,
                        _pad: 0, _pad2: 0,
                    });
                    self.dispatch(&self.vignette_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Diffusion { amount, radius, threshold, desaturation: _ } => {
                    // Same as bloom: threshold → blur → screen blend
                    let buf_c = self.buf_c.as_ref().unwrap();
                    self.write_params(&BloomThresholdParams { width, height, threshold: *threshold, _pad: 0 });
                    self.dispatch(&self.bloom_threshold_pipeline, input, buf_c, workgroups);
                    let boxes = crate::blur::boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }
                    self.dispatch_bloom_blend_screen(input, buf_c, output, workgroups, *amount, width, height);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::LensFlare { position_x, position_y, brightness, flare_size, ghost_count, ghost_intensity, chromatic_aberration } => {
                    self.write_params(&LensFlareParams {
                        width, height,
                        pos_x: *position_x, pos_y: *position_y,
                        brightness: *brightness, flare_size: *flare_size,
                        ghost_count: *ghost_count, ghost_intensity: *ghost_intensity,
                        chromatic_aberration: *chromatic_aberration,
                        _pad: 0, _pad2: 0, _pad3: 0,
                    });
                    self.dispatch(&self.lens_flare_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::LightWrap { width: wrap_w, intensity, softness } => {
                    // Blur → blend where dark
                    let buf_c = self.buf_c.as_ref().unwrap();
                    let byte_size = (width * height * 4) as u64;
                    {
                        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                        enc.copy_buffer_to_buffer(input, 0, buf_c, 0, byte_size);
                        self.queue.submit(Some(enc.finish()));
                    }
                    let boxes = crate::blur::boxes_for_gaussian(*wrap_w * *softness);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }
                    self.dispatch_bloom_blend_screen(input, buf_c, output, workgroups, *intensity, width, height);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::AnamorphicStreak { threshold, length, intensity, tint } => {
                    self.write_params(&AnamorphicStreakParams {
                        width, height,
                        threshold: *threshold,
                        half_len: (*length * 0.5) as u32,
                        intensity: *intensity,
                        tint: *tint,
                        _pad: 0, _pad2: 0,
                    });
                    self.dispatch(&self.anamorphic_streak_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
                    // Extract bright with halation color → blur → screen blend
                    let buf_c = self.buf_c.as_ref().unwrap();
                    let hal_r = 1.0f32;
                    let hal_g = *hue * 0.6;
                    let hal_b = *hue * *hue * 0.2;
                    // Use bloom threshold to extract brights (with color tint applied after)
                    self.write_params(&BloomThresholdParams { width, height, threshold: *threshold, _pad: 0 });
                    self.dispatch(&self.bloom_threshold_pipeline, input, buf_c, workgroups);
                    let boxes = crate::blur::boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }
                    // Screen blend with halation tint
                    self.write_params(&FilmHalationBlendParams {
                        width, height, threshold: *threshold, intensity: *intensity,
                        hal_r, hal_g, hal_b, _pad: 0,
                    });
                    let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("halation_bg"),
                        layout: &self.transition_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: buf_c.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
                        ],
                    });
                    let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    { let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                      pass.set_pipeline(&self.film_halation_blend_pipeline);
                      pass.set_bind_group(0, &bind_group, &[]);
                      pass.dispatch_workgroups(workgroups, 1, 1);
                    }
                    self.queue.submit(Some(enc.finish()));
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Invert { mix } => {
                    if (*mix).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&InvertParams {
                        width, height,
                        mix: *mix,
                        _pad: 0,
                    });
                    self.dispatch(&self.invert_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Posterize { levels } => {
                    self.write_params(&PosterizeParams {
                        width, height,
                        levels: *levels,
                        _pad: 0,
                    });
                    self.dispatch(&self.posterize_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Mosaic { block_size } => {
                    if *block_size <= 1.0 {
                        continue;
                    }
                    self.write_params(&MosaicParams {
                        width, height,
                        block_size: *block_size,
                        _pad: 0,
                    });
                    self.dispatch(&self.mosaic_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::EdgeDetect { strength, mix } => {
                    self.write_params(&EdgeDetectParams {
                        width, height,
                        strength: *strength,
                        mix: *mix,
                    });
                    self.dispatch(&self.edge_detect_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::ChannelMixer {
                    red_red, red_green, red_blue,
                    green_red, green_green, green_blue,
                    blue_red, blue_green, blue_blue,
                } => {
                    self.write_params(&ChannelMixerParams {
                        width, height,
                        rr: *red_red, rg: *red_green, rb: *red_blue,
                        gr: *green_red, gg: *green_green, gb: *green_blue,
                        br: *blue_red, bg: *blue_green, bb: *blue_blue,
                        _pad: 0,
                    });
                    self.dispatch(&self.channel_mixer_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::DirectionalBlur { angle, distance, samples } => {
                    if *distance < 0.5 {
                        continue;
                    }
                    let samples_clamped = (*samples).clamp(3, 32);
                    let angle_rad = angle.to_radians();
                    self.write_params(&DirectionalBlurParams {
                        width, height,
                        dx: angle_rad.cos() * *distance / samples_clamped as f32,
                        dy: angle_rad.sin() * *distance / samples_clamped as f32,
                        samples: samples_clamped,
                        _pad: 0, _pad2: 0, _pad3: 0,
                    });
                    self.dispatch(&self.directional_blur_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Emboss { angle, strength, mix } => {
                    let angle_rad = angle.to_radians();
                    self.write_params(&EmbossParams {
                        width, height,
                        dx: angle_rad.cos(),
                        dy: angle_rad.sin(),
                        strength: *strength,
                        mix: *mix,
                        _pad0: 0, _pad1: 0,
                    });
                    self.dispatch(&self.emboss_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Mirror { horizontal, vertical } => {
                    if *horizontal == 0 && *vertical == 0 {
                        continue;
                    }
                    self.write_params(&MirrorParams {
                        width, height,
                        horizontal: *horizontal,
                        vertical: *vertical,
                    });
                    self.dispatch(&self.mirror_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::LensDistortion { distortion, scale } => {
                    if distortion.abs() < f32::EPSILON && (*scale - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&LensDistortionParams {
                        width, height,
                        distortion: *distortion,
                        scale: *scale,
                    });
                    self.dispatch(&self.lens_distortion_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Dilate { radius } => {
                    self.write_params(&DilateErodeParams {
                        width, height,
                        radius: (*radius).clamp(1, 10),
                        _pad: 0,
                    });
                    self.dispatch(&self.dilate_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Erode { radius } => {
                    self.write_params(&DilateErodeParams {
                        width, height,
                        radius: (*radius).clamp(1, 10),
                        _pad: 0,
                    });
                    self.dispatch(&self.erode_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Denoise { spatial_sigma, color_sigma, iterations } => {
                    let iters = (*iterations).clamp(1, 3);
                    self.write_params(&DenoiseParams {
                        width, height,
                        spatial_sigma: *spatial_sigma,
                        color_sigma: *color_sigma,
                    });
                    // Multi-pass: ping-pong between buffers
                    for _iter in 0..iters {
                        let (inp, out) = if current_in_a {
                            (buf_a, buf_b)
                        } else {
                            (buf_b, buf_a)
                        };
                        self.dispatch(&self.denoise_pipeline, inp, out, workgroups);
                        current_in_a = !current_in_a;
                    }
                }
                BuiltinEffect::Gamma { gamma } => {
                    if (*gamma - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&GammaParams {
                        width, height,
                        inv_gamma: 1.0 / *gamma,
                        _pad: 0,
                    });
                    self.dispatch(&self.gamma_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::HSVAdjust { hue_shift, saturation, value } => {
                    if hue_shift.abs() < f32::EPSILON && (*saturation - 1.0).abs() < f32::EPSILON && (*value - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&HSVAdjustParams {
                        width, height,
                        hue_shift: *hue_shift,
                        saturation: *saturation,
                        value: *value,
                        _pad: 0, _pad2: 0, _pad3: 0,
                    });
                    self.dispatch(&self.hsv_adjust_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::NoiseGenerator { amount, monochrome, seed } => {
                    if *amount < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&NoiseGeneratorParams {
                        width, height,
                        amount: *amount,
                        monochrome: *monochrome,
                        seed: *seed,
                        _pad: 0, _pad2: 0, _pad3: 0,
                    });
                    self.dispatch(&self.noise_generator_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::MedianFilter { radius } => {
                    let r = (*radius).clamp(1, 2); // clamp to 2 on GPU for performance
                    self.write_params(&MedianFilterParams {
                        width, height,
                        radius: r,
                        _pad: 0,
                    });
                    self.dispatch(&self.median_filter_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } => {
                    if *amount < 0.01 {
                        continue;
                    }
                    let s = (*samples).clamp(4, 32);
                    self.write_params(&RadialBlurParams {
                        width, height,
                        center_x: *center_x,
                        center_y: *center_y,
                        amount: *amount,
                        samples: s,
                        mode: *mode,
                        _pad: 0,
                    });
                    self.dispatch(&self.radial_blur_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Levels { in_black, in_white, gamma, out_black, out_white } => {
                    if *in_black <= 0.0 && (*in_white - 1.0).abs() < f32::EPSILON
                        && (*gamma - 1.0).abs() < f32::EPSILON
                        && *out_black <= 0.0 && (*out_white - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&LevelsParams {
                        width, height,
                        in_black: *in_black,
                        in_white: *in_white,
                        inv_gamma: if gamma.abs() > f32::EPSILON { 1.0 / *gamma } else { 1.0 },
                        out_black: *out_black,
                        out_white: *out_white,
                        _pad: 0,
                    });
                    self.dispatch(&self.levels_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Equalize { strength } => {
                    // Hybrid GPU: build histogram/CDF on CPU (fast), remap on GPU (parallel).
                    // Step 1: Download current frame to compute histogram
                    let current_buf = if current_in_a { buf_a } else { buf_b };
                    let frame_data = self.download(current_buf, byte_size);

                    // Step 2: Build luminance histogram (256 bins)
                    let mut histogram = [0u32; 256];
                    let pixel_count = (width * height) as usize;
                    for i in 0..pixel_count {
                        let idx = i * 4;
                        let r = frame_data[idx] as f32;
                        let g = frame_data[idx + 1] as f32;
                        let b = frame_data[idx + 2] as f32;
                        let luma = (0.2126 * r + 0.7152 * g + 0.0722 * b) as u8;
                        histogram[luma as usize] += 1;
                    }

                    // Step 3: Build CDF and normalize to 0.0-1.0
                    let mut cdf = [0.0f32; 256];
                    let mut cumulative = 0u32;
                    let total = pixel_count as f32;
                    // Find first non-zero bin for CDF normalization
                    let cdf_min = histogram.iter().position(|&h| h > 0).unwrap_or(0);
                    let min_cumulative = histogram[..=cdf_min].iter().sum::<u32>();
                    for i in 0..256 {
                        cumulative += histogram[i];
                        // Standard histogram equalization formula
                        cdf[i] = if total > min_cumulative as f32 {
                            ((cumulative - min_cumulative) as f32 / (total - min_cumulative as f32)).clamp(0.0, 1.0)
                        } else {
                            i as f32 / 255.0
                        };
                    }

                    // Step 4: Upload CDF as storage buffer and dispatch GPU remap shader
                    let cdf_bytes = bytemuck::cast_slice(&cdf);
                    // Reuse huesat_lut_buf (1024 bytes = 256 × 4 fits in 6144 byte huesat buf)
                    self.queue.write_buffer(&self.huesat_lut_buf, 0, cdf_bytes);

                    self.write_params(&EqualizeParams {
                        width,
                        height,
                        strength: *strength,
                        _pad0: 0,
                    });

                    let input = if current_in_a { buf_a } else { buf_b };
                    let output = if current_in_a { buf_b } else { buf_a };

                    // Create bind group with 4 bindings (like huesat)
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("equalize_bg"),
                        layout: &self.huesat_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: self.huesat_lut_buf.as_entire_binding() },
                        ],
                    });

                    let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.equalize_pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(workgroups, 1, 1);
                    }
                    self.queue.submit(std::iter::once(encoder.finish()));

                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Letterbox { aspect_ratio, bar_color, opacity } => {
                    self.write_params(&LetterboxParams {
                        width, height,
                        aspect_ratio: *aspect_ratio,
                        bar_color: *bar_color,
                        opacity: *opacity,
                        _pad0: 0,
                        _pad1: 0,
                        _pad2: 0,
                    });
                    self.dispatch(&self.letterbox_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
                    if red_shift.abs() < f32::EPSILON && blue_shift.abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&ChromaticAberrationParams {
                        width, height,
                        red_shift: *red_shift,
                        blue_shift: *blue_shift,
                        center_x: *center_x,
                        center_y: *center_y,
                        _pad0: 0,
                        _pad1: 0,
                    });
                    self.dispatch(&self.chromatic_aberration_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::RollingShutterCorrection { strength, .. } => {
                    if *strength < f32::EPSILON || !self.has_rsc_data() {
                        continue; // No correction or no motion data ready
                    }
                    self.write_params(&RscParams { width, height, strength: *strength, _pad: 0 });
                    let input = if current_in_a { buf_a } else { buf_b };
                    let output = if current_in_a { buf_b } else { buf_a };
                    let row_buf = self.rsc_row_buf.as_ref().unwrap();
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("rsc_bg"),
                        layout: &self.huesat_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: row_buf.as_entire_binding() },
                        ],
                    });
                    let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    {
                        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.rsc_pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(workgroups, 1, 1);
                    }
                    self.queue.submit(std::iter::once(enc.finish()));
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::VideoStabilization { crop_ratio, zoom, .. } => {
                    if !self.has_stabilization_data() { continue; }
                    let crop_zoom = if *zoom { 1.0 / (1.0 - 2.0 * crop_ratio) } else { 1.0 };
                    self.write_params(&StabilizationParams { width, height, crop_zoom, _pad: 0 });
                    let input = if current_in_a { buf_a } else { buf_b };
                    let output = if current_in_a { buf_b } else { buf_a };
                    let hom_buf = self.stabilization_buf.as_ref().unwrap();
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("stabilization_bg"),
                        layout: &self.huesat_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: hom_buf.as_entire_binding() },
                        ],
                    });
                    let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    {
                        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.stabilization_pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(workgroups, 1, 1);
                    }
                    self.queue.submit(std::iter::once(enc.finish()));
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Vibrance { .. } => {
                    // Vibrance handled by CPU fallback. Pass through GPU chain.
                }
                BuiltinEffect::SpeedRamp { .. } => {
                    // SpeedRamp is handled at the engine level (frame queue + GPU blend),
                    // not in the per-frame GPU effects chain. Pass through.
                }
            }
        }

        // Download result from GPU
        self.download(
            if current_in_a { buf_a } else { buf_b },
            byte_size,
        )
    }

    /// Blur an alpha mask on the GPU. Input/output are single-channel u8 buffers.
    /// Uses 3-pass box blur to approximate Gaussian, same as the RGBA blur.
    pub fn blur_alpha_gpu(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        let pixel_count = (w * h) as u64;
        // Each alpha value stored as one u32
        let byte_size = pixel_count * 4;
        self.ensure_buffers(byte_size);

        // Convert u8 alpha -> u32 for GPU
        let input_u32: Vec<u32> = mask.iter().map(|&a| a as u32).collect();

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&input_u32));

        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);

        // 3-pass box blur for gaussian approximation
        let boxes = boxes_for_gaussian(radius);
        let mut current_in_a = true;

        for &box_r in &boxes {
            if box_r == 0 { continue; }

            // Horizontal pass
            let (inp, out) = if current_in_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
            self.write_params(&AlphaBlurParams { width: w, height: h, radius: box_r as u32, _pad: 0 });
            self.dispatch(&self.alpha_blur_h_pipeline, inp, out, workgroups);
            current_in_a = !current_in_a;

            // Vertical pass
            let (inp, out) = if current_in_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
            self.dispatch(&self.alpha_blur_v_pipeline, inp, out, workgroups);
            current_in_a = !current_in_a;
        }

        let result_buf = if current_in_a { buf_a } else { buf_b };
        let result_bytes = self.download(result_buf, byte_size);

        // Convert u32 back to u8
        let result_u32: &[u32] = bytemuck::cast_slice(&result_bytes);
        result_u32.iter().map(|&v| v.min(255) as u8).collect()
    }

    /// Dilate an alpha mask on the GPU using circular distance.
    pub fn dilate_alpha_gpu(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        let r = radius.ceil() as u32;
        if r == 0 { return mask.to_vec(); }

        let pixel_count = (w * h) as u64;
        let byte_size = pixel_count * 4;
        self.ensure_buffers(byte_size);

        let input_u32: Vec<u32> = mask.iter().map(|&a| a as u32).collect();

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&input_u32));

        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);

        self.write_params(&AlphaDilateParams { width: w, height: h, radius: r, _pad: 0 });
        self.dispatch(&self.alpha_dilate_pipeline, buf_a, buf_b, workgroups);

        let result_bytes = self.download(buf_b, byte_size);
        let result_u32: &[u32] = bytemuck::cast_slice(&result_bytes);
        result_u32.iter().map(|&v| v.min(255) as u8).collect()
    }

    fn write_params<T: bytemuck::Pod>(&self, data: &T) {
        let bytes = bytemuck::bytes_of(data);
        self.queue.write_buffer(&self.params_buf, 0, bytes);
    }

    fn dispatch(
        &self,
        pipeline: &wgpu::ComputePipeline,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        workgroups: u32,
    ) {
        let bind_group = self.bind_group(input, output);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    pub fn upload_rsc_row_transforms(&mut self, row_transforms: &[crate::rolling_shutter::RowTransform]) {
        let byte_size = (row_transforms.len() * std::mem::size_of::<crate::rolling_shutter::RowTransform>()) as u64;
        if self.rsc_row_buf.as_ref().map_or(true, |_| self.rsc_row_buf_size < byte_size) {
            self.rsc_row_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rsc_row_transforms"),
                size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.rsc_row_buf_size = byte_size;
        }
        self.queue.write_buffer(self.rsc_row_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(row_transforms));
    }

    pub fn has_rsc_data(&self) -> bool {
        self.rsc_row_buf.is_some() && self.rsc_row_buf_size > 0
    }

    pub fn upload_stabilization_homography(&mut self, h: &[f32; 12]) {
        let byte_size = 48u64; // 12 x f32
        if self.stabilization_buf.is_none() {
            self.stabilization_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stabilization_homography"),
                size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.stabilization_buf_size = byte_size;
        }
        self.queue.write_buffer(self.stabilization_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(h));
    }

    pub fn has_stabilization_data(&self) -> bool {
        self.stabilization_buf.is_some()
    }

    fn download(&self, source: &wgpu::Buffer, byte_size: u64) -> Vec<u8> {
        let staging = self.staging_read.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(source, 0, staging, 0, byte_size);
        self.queue.submit(std::iter::once(encoder.finish()));

        let buffer_slice = staging.slice(..byte_size);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);

        match rx.recv() {
            Ok(Ok(())) => {
                let data = buffer_slice.get_mapped_range();
                let result = data.to_vec();
                drop(data);
                staging.unmap();
                result
            }
            _ => {
                warn!("GPU effects: download failed, returning zeros");
                vec![0u8; byte_size as usize]
            }
        }
    }

    /// Render a transition between two frames on the GPU.
    /// `kind_id`: 0=CrossDissolve, 1=WipeLeft, 2=WipeRight, 3=WipeUp, 4=WipeDown, 5=DipToBlack, 6=DipToWhite
    pub fn render_transition(
        &mut self,
        from_rgba: &[u8],
        to_rgba: &[u8],
        kind_id: u32,
        progress: f32,
        width: u32,
        height: u32,
        user_param0: f32,
        user_param1: f32,
        user_param2: f32,
        user_param3: f32,
        user_color: [f32; 4],
    ) -> Vec<u8> {
        let byte_size = (width * height * 4) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let buf_c = self.buf_c.as_ref().unwrap();

        // Upload: buf_a = from, buf_c = to, buf_b = output
        self.queue.write_buffer(buf_a, 0, from_rgba);
        self.queue.write_buffer(buf_c, 0, to_rgba);

        self.write_params(&TransitionParams {
            width,
            height,
            progress,
            kind_id,
            param0: user_param0,
            param1: user_param1,
            param2: user_param2,
            param3: user_param3,
            color_r: user_color[0],
            color_g: user_color[1],
            color_b: user_color[2],
            color_a: user_color[3],
        });

        // Create transition bind group (from, to, output, params)
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_c.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
            ],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.transition_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        self.download(buf_b, byte_size)
    }

    /// Dispatch bloom blend: additive blend of bright (buf_c) onto original (input) → output
    fn dispatch_bloom_blend(
        &self, input: &wgpu::Buffer, bright: &wgpu::Buffer, output: &wgpu::Buffer,
        workgroups: u32, intensity: f32, width: u32, height: u32,
    ) {
        self.write_params(&BloomBlendParams { width, height, intensity, _pad: 0 });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bloom_blend_bg"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: bright.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.bloom_blend_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(Some(enc.finish()));
    }

    /// Dispatch glow blend: screen blend of blurred (buf_c) onto original (input) → output
    fn dispatch_bloom_blend_screen(
        &self, input: &wgpu::Buffer, blurred: &wgpu::Buffer, output: &wgpu::Buffer,
        workgroups: u32, intensity: f32, width: u32, height: u32,
    ) {
        self.write_params(&BloomBlendParams { width, height, intensity, _pad: 0 });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glow_blend_bg"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: blurred.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.glow_blend_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(Some(enc.finish()));
    }
}

// ─── Param structs (must be Pod for bytemuck) ────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OpacityParams {
    width: u32,
    height: u32,
    value: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SharpenParams {
    width: u32,
    height: u32,
    amount: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CropParams {
    width: u32,
    height: u32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    feather: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformParams {
    width: u32,
    height: u32,
    anchor_x: f32,
    anchor_y: f32,
    position_x: f32,
    position_y: f32,
    cos_r: f32,
    sin_r: f32,
    inv_sx: f32,
    inv_sy: f32,
    cos_rx: f32,
    sin_rx: f32,
    cos_ry: f32,
    sin_ry: f32,
    focal: f32,
    _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaKeyParams {
    width: u32,
    height: u32,
    key_cb: f32,
    key_cr: f32,
    tolerance: f32,
    spill_suppression: f32,
    _pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomThresholdParams {
    width: u32,
    height: u32,
    threshold: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomBlendParams {
    width: u32,
    height: u32,
    intensity: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LightRaysParams {
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    threshold: f32,
    samples: u32,
    decay: f32,
    intensity: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VignetteParams {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    softness: f32,
    roundness: f32,
    _pad: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LensFlareParams {
    width: u32,
    height: u32,
    pos_x: f32,
    pos_y: f32,
    brightness: f32,
    flare_size: f32,
    ghost_count: u32,
    ghost_intensity: f32,
    chromatic_aberration: f32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AnamorphicStreakParams {
    width: u32,
    height: u32,
    threshold: f32,
    half_len: u32,
    intensity: f32,
    tint: f32,
    _pad: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FilmHalationBlendParams {
    width: u32,
    height: u32,
    threshold: f32,
    intensity: f32,
    hal_r: f32,
    hal_g: f32,
    hal_b: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransitionParams {
    width: u32,
    height: u32,
    progress: f32,
    kind_id: u32, // 0=CrossDissolve, 1=WipeLeft, 2=WipeRight, 3=WipeUp, 4=WipeDown, 5=DipToBlack, 6=DipToWhite
    // Generic per-kind user params. Meaning is a per-kind contract — see the
    // WGSL shader branches. Keep field layout identical to the WGSL struct.
    param0: f32,
    param1: f32,
    param2: f32,
    param3: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuColorGradeParams {
    width: u32,
    height: u32,
    exposure: f32,
    contrast: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    lift_r: f32,
    lift_g: f32,
    lift_b: f32,
    gamma_r: f32,
    gamma_g: f32,
    gamma_b: f32,
    gain_r: f32,
    gain_g: f32,
    gain_b: f32,
    offset_r: f32,
    offset_g: f32,
    offset_b: f32,
    hue: f32,
    global_mix: f32,
    _pad0: u32,
}
// 26 fields × 4 bytes = 104 bytes (well under 256-byte PARAMS_SIZE)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuHueSatParams {
    width: u32,
    height: u32,
    mix: f32,
    _pad0: u32,
}
// 4 fields × 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuColorBalanceParams {
    width: u32,
    height: u32,
    temperature: f32,
    tint: f32,
    hue: f32,
    global_mix: f32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuWheelSatBriParams {
    width: u32,
    height: u32,
    lift_sat: f32,
    lift_bri: f32,
    gamma_sat: f32,
    gamma_bri: f32,
    gain_sat: f32,
    gain_bri: f32,
    offset_sat: f32,
    offset_bri: f32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuRgbCurvesParams {
    width: u32,
    height: u32,
    preserve_luma: u32,
    mix: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVibranceParams {
    width: u32, height: u32, amount: f32, _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuColorWarperParams {
    width: u32, height: u32, _pad0: u32, _pad1: u32,
    hue_shifts: [f32; 12],
    sat_shifts: [f32; 12],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuInputTransformParams {
    width: u32, height: u32, _pad0: u32, _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuClarityBlurParams {
    width: u32, height: u32, radius: u32, _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuClarityApplyParams {
    width: u32, height: u32, amount: f32, _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuScopeAccumParams {
    width: u32, height: u32, display_columns: u32, step: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuScopeRenderParams {
    out_w: u32, out_h: u32, num_cols: u32, max_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuMaskParams {
    width: u32,
    height: u32,
    mask_count: u32,
    _pad: u32,
    // 4 masks × 8 fields = 32 u32s
    m0_type: u32, m0_cx: f32, m0_cy: f32, m0_p1: f32, m0_p2: f32, m0_p3: f32, m0_feather: f32, m0_flags: u32,
    m1_type: u32, m1_cx: f32, m1_cy: f32, m1_p1: f32, m1_p2: f32, m1_p3: f32, m1_feather: f32, m1_flags: u32,
    m2_type: u32, m2_cx: f32, m2_cy: f32, m2_p1: f32, m2_p2: f32, m2_p3: f32, m2_feather: f32, m2_flags: u32,
    m3_type: u32, m3_cx: f32, m3_cy: f32, m3_p1: f32, m3_p2: f32, m3_p3: f32, m3_feather: f32, m3_flags: u32,
}
// 36 fields × 4 bytes = 144 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLut3dParams {
    width: u32,
    height: u32,
    lut_size: u32,
    opacity: f32,
}
// 4 fields × 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuQualifierParams {
    width: u32,
    height: u32,
    // Qualifier selection
    hue_center: f32,     // 0-1 (normalized from 0-360°)
    hue_width: f32,      // 0-1
    hue_soft: f32,       // 0-1
    sat_low: f32,
    sat_high: f32,
    sat_soft: f32,
    lum_low: f32,
    lum_high: f32,
    lum_soft: f32,
    invert: u32,         // 0 or 1
    // Secondary grade
    sec_exposure: f32,
    sec_saturation: f32,
    sec_hue_shift: f32,  // radians
    sec_temperature: f32,
    // Matte preview
    matte_preview: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}
// 20 fields × 4 bytes = 80 bytes (well under 256-byte PARAMS_SIZE)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuGrainParams {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    shadows: f32,
    highlights: f32,
    frame_seed: u32,
    monochrome: u32,
}
// 8 fields × 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AlphaBlurParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AlphaDilateParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertParams {
    width: u32,
    height: u32,
    mix: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PosterizeParams {
    width: u32,
    height: u32,
    levels: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MosaicParams {
    width: u32,
    height: u32,
    block_size: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeDetectParams {
    width: u32,
    height: u32,
    strength: f32,
    mix: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChannelMixerParams {
    width: u32,
    height: u32,
    rr: f32,
    rg: f32,
    rb: f32,
    gr: f32,
    gg: f32,
    gb: f32,
    br: f32,
    bg: f32,
    bb: f32,
    _pad: u32,
}
// 12 fields × 4 bytes = 48 bytes (multiple of 16)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DirectionalBlurParams {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    samples: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}
// 8 fields × 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EmbossParams {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    strength: f32,
    mix: f32,
    _pad0: u32,
    _pad1: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MirrorParams {
    width: u32,
    height: u32,
    horizontal: u32,
    vertical: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LensDistortionParams {
    width: u32,
    height: u32,
    distortion: f32,
    scale: f32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DilateErodeParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DenoiseParams {
    width: u32,
    height: u32,
    spatial_sigma: f32,
    color_sigma: f32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GammaParams {
    width: u32,
    height: u32,
    inv_gamma: f32,
    _pad: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HSVAdjustParams {
    width: u32,
    height: u32,
    hue_shift: f32,
    saturation: f32,
    value: f32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NoiseGeneratorParams {
    width: u32,
    height: u32,
    amount: f32,
    monochrome: u32,
    seed: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MedianFilterParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RadialBlurParams {
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    amount: f32,
    samples: u32,
    mode: u32,
    _pad: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LevelsParams {
    width: u32,
    height: u32,
    in_black: f32,
    in_white: f32,
    inv_gamma: f32,
    out_black: f32,
    out_white: f32,
    _pad: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EqualizeParams {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LetterboxParams {
    width: u32,
    height: u32,
    aspect_ratio: f32,
    bar_color: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RscParams {
    width: u32,
    height: u32,
    strength: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StabilizationParams {
    width: u32,
    height: u32,
    crop_zoom: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaticAberrationParams {
    width: u32,
    height: u32,
    red_shift: f32,
    blue_shift: f32,
    center_x: f32,
    center_y: f32,
    _pad0: u32,
    _pad1: u32,
}
// 8 fields x 4 bytes = 32 bytes

// ─── Box blur Gaussian approximation (same as CPU version) ───────────────────

fn boxes_for_gaussian(sigma: f32) -> [usize; 3] {
    let w_ideal = ((12.0 * sigma * sigma / 3.0) + 1.0).sqrt();
    let mut wl = w_ideal.floor() as usize;
    if wl.is_multiple_of(2) {
        wl = wl.saturating_sub(1).max(1);
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma
        - 3.0 * (wl * wl) as f32
        - 12.0 * wl as f32
        - 9.0)
        / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as usize;
    let r = |size: usize| (size - 1) / 2;
    [
        r(if 0 < m { wl } else { wu }),
        r(if 1 < m { wl } else { wu }),
        r(if 2 < m { wl } else { wu }),
    ]
}

// ─── WGSL Compute Shaders ────────────────────────────────────────────────────

const SHADER_OPACITY: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    value: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = unpack(input[idx]);
    output[idx] = pack(pixel * params.value);
}
"
);

const SHADER_BLUR_H: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    var sum = vec4<f32>(0.0);
    let diam = 2 * params.radius + 1;
    let inv = 1.0 / f32(diam);
    let w_i32 = i32(params.width);

    for (var dx = -params.radius; dx <= params.radius; dx++) {
        let sx = clamp(i32(x) + dx, 0, w_i32 - 1);
        sum += unpack(input[y * params.width + u32(sx)]);
    }
    output[idx] = pack(sum * inv);
}
"
);

const SHADER_BLUR_V: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    var sum = vec4<f32>(0.0);
    let diam = 2 * params.radius + 1;
    let inv = 1.0 / f32(diam);
    let h_i32 = i32(params.height);

    for (var dy = -params.radius; dy <= params.radius; dy++) {
        let sy = clamp(i32(y) + dy, 0, h_i32 - 1);
        sum += unpack(input[u32(sy) * params.width + x]);
    }
    output[idx] = pack(sum * inv);
}
"
);

const SHADER_SHARPEN: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let orig = unpack(input[idx]);

    // 3x3 average of neighbors (excluding center)
    var avg = vec4<f32>(0.0);
    let w_i32 = i32(params.width);
    let h_i32 = i32(params.height);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let sx = clamp(i32(x) + dx, 0, w_i32 - 1);
            let sy = clamp(i32(y) + dy, 0, h_i32 - 1);
            avg += unpack(input[u32(sy) * params.width + u32(sx)]);
        }
    }
    avg = avg / 9.0;

    // Unsharp mask: original + amount * (original - blurred)
    let sharpened = vec4<f32>(
        orig.x + params.amount * (orig.x - avg.x),
        orig.y + params.amount * (orig.y - avg.y),
        orig.z + params.amount * (orig.z - avg.z),
        orig.w,  // preserve alpha
    );
    output[idx] = pack(sharpened);
}
"
);

const SHADER_CROP: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    feather: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let xf = f32(x);
    let yf = f32(y);

    if xf < params.crop_left || xf >= params.crop_right || yf < params.crop_top || yf >= params.crop_bottom {
        output[idx] = 0u;
        return;
    }

    var pixel = unpack(input[idx]);

    if params.feather > 0.0 {
        let dist = min(
            min(xf - params.crop_left, params.crop_right - 1.0 - xf),
            min(yf - params.crop_top, params.crop_bottom - 1.0 - yf),
        );
        if dist < params.feather {
            let factor = dist / params.feather;
            pixel.w = pixel.w * factor;
        }
    }

    output[idx] = pack(pixel);
}
"
);

const SHADER_TRANSFORM: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    anchor_x: f32,
    anchor_y: f32,
    position_x: f32,
    position_y: f32,
    cos_r: f32,
    sin_r: f32,
    inv_sx: f32,
    inv_sy: f32,
    cos_rx: f32,
    sin_rx: f32,
    cos_ry: f32,
    sin_ry: f32,
    focal: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let dx = f32(x) - params.anchor_x - params.position_x;
    let dy = f32(y) - params.anchor_y - params.position_y;

    // Inverse Z-rotation (2D)
    let rx = dx * params.cos_r + dy * params.sin_r;
    let ry = -dx * params.sin_r + dy * params.cos_r;

    // Inverse scale
    var sx = rx * params.inv_sx;
    var sy = ry * params.inv_sy;

    // 3D rotation + perspective
    if params.cos_rx != 1.0 || params.cos_ry != 1.0 {
        let y1 = sy * params.cos_rx;
        let z1 = sy * params.sin_rx;
        let x2 = sx * params.cos_ry + z1 * params.sin_ry;
        let z2 = -sx * params.sin_ry + z1 * params.cos_ry;
        if params.focal > 0.0 {
            let w_f = 1.0 + z2 / params.focal;
            if w_f > 0.01 {
                sx = x2 / w_f;
                sy = y1 / w_f;
            }
        } else {
            sx = x2;
            sy = y1;
        }
    }

    sx = sx + params.anchor_x;
    sy = sy + params.anchor_y;

    let src_x = i32(round(sx));
    let src_y = i32(round(sy));

    if src_x >= 0 && src_x < i32(params.width) && src_y >= 0 && src_y < i32(params.height) {
        let src_idx = u32(src_y) * params.width + u32(src_x);
        output[idx] = input[src_idx];
    } else {
        output[idx] = 0u;
    }
}
"
);

const SHADER_CHROMA_KEY: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    key_cb: f32,
    key_cr: f32,
    tolerance: f32,
    spill_suppression: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}
",
    "
fn rgb_to_cbcr(r: f32, g: f32, b: f32) -> vec2<f32> {
    let cb = 128.0 + (-0.168736 * r - 0.331264 * g + 0.5 * b);
    let cr = 128.0 + (0.5 * r - 0.418688 * g - 0.081312 * b);
    return vec2<f32>(cb, cr);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    var pixel = unpack(input[idx]);
    let cbcr = rgb_to_cbcr(pixel.x, pixel.y, pixel.z);

    let dcb = (cbcr.x - params.key_cb) / 255.0;
    let dcr = (cbcr.y - params.key_cr) / 255.0;
    let dist = sqrt(dcb * dcb + dcr * dcr);

    if dist < params.tolerance {
        pixel = vec4<f32>(0.0);
    } else if dist < params.tolerance + params.spill_suppression {
        let factor = (dist - params.tolerance) / params.spill_suppression;
        pixel.w = pixel.w * factor;
    }

    output[idx] = pack(pixel);
}
"
);

const SHADER_TRANSITION: &str = "
struct Params {
    width: u32,
    height: u32,
    progress: f32,
    kind_id: u32,
    param0: f32,
    param1: f32,
    param2: f32,
    param3: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
}

@group(0) @binding(0) var<storage, read> from_buf: array<u32>;
@group(0) @binding(1) var<storage, read> to_buf: array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;
@group(0) @binding(3) var<uniform> params: Params;

fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu),
        f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu),
        f32((v >> 24u) & 0xFFu),
    );
}

fn pack(v: vec4<f32>) -> u32 {
    let c = clamp(v, vec4<f32>(0.0), vec4<f32>(255.0));
    return u32(c.x) | (u32(c.y) << 8u) | (u32(c.z) << 16u) | (u32(c.w) << 24u);
}

fn lerp_pixel(a: vec4<f32>, b: vec4<f32>, t: f32) -> vec4<f32> {
    return a * (1.0 - t) + b * t;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let y = idx / params.width;
    let x = idx % params.width;
    let p = params.progress;

    let from_px = unpack(from_buf[idx]);
    let to_px = unpack(to_buf[idx]);

    var result: vec4<f32>;

    if params.kind_id == 0u {
        // Cross Dissolve
        result = lerp_pixel(from_px, to_px, p);
    } else if params.kind_id == 1u {
        // Wipe Left (reveals to-frame from left to right)
        // param0 = feather radius in pixels (0 = hard edge)
        // threshold expanded by ±feather so endpoints fully clear/cover the frame
        let feather = max(params.param0, 0.001);
        let threshold = p * (f32(params.width) + 2.0 * feather) - feather;
        let t = 1.0 - smoothstep(threshold - feather, threshold + feather, f32(x));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 2u {
        // Wipe Right (reveals to-frame from right to left)
        let feather = max(params.param0, 0.001);
        let threshold = (1.0 - p) * (f32(params.width) + 2.0 * feather) - feather;
        let t = smoothstep(threshold - feather, threshold + feather, f32(x));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 3u {
        // Wipe Up (reveals to-frame from top to bottom)
        let feather = max(params.param0, 0.001);
        let threshold = p * (f32(params.height) + 2.0 * feather) - feather;
        let t = 1.0 - smoothstep(threshold - feather, threshold + feather, f32(y));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 4u {
        // Wipe Down (reveals to-frame from bottom to top)
        let feather = max(params.param0, 0.001);
        let threshold = (1.0 - p) * (f32(params.height) + 2.0 * feather) - feather;
        let t = smoothstep(threshold - feather, threshold + feather, f32(y));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 5u || params.kind_id == 6u {
        // Dip to colour. param0 = hold duration (0..0.45) at the midpoint.
        // colour = dip colour (default black for kind 5, white for kind 6).
        let dip_color = vec4<f32>(
            params.color_r * 255.0,
            params.color_g * 255.0,
            params.color_b * 255.0,
            255.0,
        );
        let hold = clamp(params.param0, 0.0, 0.45);
        let mid_start = 0.5 - hold * 0.5;
        let mid_end = 0.5 + hold * 0.5;
        if p <= mid_start {
            let t = p / max(mid_start, 0.001);
            result = lerp_pixel(from_px, dip_color, t);
        } else if p >= mid_end {
            let t = (p - mid_end) / max(1.0 - mid_end, 0.001);
            result = lerp_pixel(dip_color, to_px, t);
        } else {
            result = dip_color;
        }
    } else if params.kind_id == 7u {
        // Push: outgoing slides off, incoming slides on. Hard slide, no blend.
        // param0 = direction (0=Left, 1=Right, 2=Up, 3=Down)
        let dir = u32(params.param0 + 0.5);
        if dir == 1u {
            // Push Right: outgoing slides right, incoming enters from left
            let offset = i32(p * f32(params.width));
            let src_x = i32(x) - offset;
            if src_x >= 0 {
                result = unpack(from_buf[y * params.width + u32(src_x)]);
            } else {
                let in_x = u32(src_x + i32(params.width));
                result = unpack(to_buf[y * params.width + in_x]);
            }
        } else if dir == 2u {
            // Push Up: outgoing slides up, incoming enters from bottom
            let offset = u32(p * f32(params.height));
            let src_y = y + offset;
            if src_y < params.height {
                result = unpack(from_buf[src_y * params.width + x]);
            } else {
                let in_y = src_y - params.height;
                result = unpack(to_buf[in_y * params.width + x]);
            }
        } else if dir == 3u {
            // Push Down: outgoing slides down, incoming enters from top
            let offset = i32(p * f32(params.height));
            let src_y = i32(y) - offset;
            if src_y >= 0 {
                result = unpack(from_buf[u32(src_y) * params.width + x]);
            } else {
                let in_y = u32(src_y + i32(params.height));
                result = unpack(to_buf[in_y * params.width + x]);
            }
        } else {
            // Push Left (default): outgoing slides left, incoming enters from right
            let offset = u32(p * f32(params.width));
            let src_x = x + offset;
            if src_x < params.width {
                result = unpack(from_buf[y * params.width + src_x]);
            } else {
                let in_x = src_x - params.width;
                result = unpack(to_buf[y * params.width + in_x]);
            }
        }
    } else if params.kind_id == 8u {
        // Film Dissolve: perceptual gamma-aware dissolve (sqrt blending)
        let f_lin = from_px / 255.0;
        let t_lin = to_px / 255.0;
        let blended = sqrt(f_lin * f_lin * (1.0 - p) + t_lin * t_lin * p);
        result = blended * 255.0;
        result.w = from_px.w * (1.0 - p) + to_px.w * p;
    } else if params.kind_id == 9u {
        // Blur Dissolve: dissolve + horizontal box blur peaking at midpoint
        // param0 = max blur radius in pixels
        let max_radius = max(params.param0, 1.0);
        let blur_r = i32(p * (1.0 - p) * 4.0 * max_radius);
        var acc = vec4<f32>(0.0);
        var count = 0.0;
        let ix = i32(x);
        let step_sz = max(blur_r / 4, 1);
        for (var dx = -blur_r; dx <= blur_r; dx += step_sz) {
            let sx = clamp(ix + dx, 0, i32(params.width) - 1);
            let si = y * params.width + u32(sx);
            let fpx = unpack(from_buf[si]);
            let tpx = unpack(to_buf[si]);
            acc += lerp_pixel(fpx, tpx, p);
            count += 1.0;
        }
        if count > 0.0 {
            result = acc / count;
        } else {
            result = lerp_pixel(from_px, to_px, p);
        }
    } else if params.kind_id == 10u {
        // Flash: bright colour flash with parameterized peak.
        // param0 = intensity, param1 = peak position, color = flash colour.
        let flash_color = vec4<f32>(
            params.color_r * 255.0,
            params.color_g * 255.0,
            params.color_b * 255.0,
            255.0,
        );
        let intensity = clamp(params.param0, 0.0, 2.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        var flash_t: f32;
        if p < ramp_up {
            flash_t = p / max(ramp_up, 0.001);
        } else if p < peak {
            flash_t = 1.0;
        } else if p < ramp_down {
            flash_t = 1.0 - (p - peak) / max(ramp_down - peak, 0.001);
        } else {
            flash_t = 0.0;
        }
        flash_t = clamp(flash_t * intensity, 0.0, 1.0);
        let dissolve_start = peak * 0.8;
        let dissolve_t = clamp((p - dissolve_start) / max(1.0 - dissolve_start, 0.001), 0.0, 1.0);
        let base = lerp_pixel(from_px, to_px, dissolve_t);
        result = vec4<f32>(
            min(base.x * (1.0 - flash_t) + flash_color.x * flash_t, 255.0),
            min(base.y * (1.0 - flash_t) + flash_color.y * flash_t, 255.0),
            min(base.z * (1.0 - flash_t) + flash_color.z * flash_t, 255.0),
            base.w,
        );
    } else if params.kind_id == 11u {
        // Spin: rotation dissolve around center
        // param0 = rotations (1.0 = one full turn), param1 = zoom bloom amount
        let rotations = max(params.param0, 0.05);
        let zoom_amount = clamp(params.param1, 0.0, 1.0);
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let angle = p * rotations * 6.283185; // 2*PI
        let scale = 1.0 + p * (1.0 - p) * 4.0 * zoom_amount;
        let inv_s = 1.0 / scale;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let rx = (dx * cos(angle) + dy * sin(angle)) * inv_s + cx;
        let ry = (-dx * sin(angle) + dy * cos(angle)) * inv_s + cy;
        let sx = u32(clamp(rx, 0.0, f32(params.width) - 1.0));
        let sy = u32(clamp(ry, 0.0, f32(params.height) - 1.0));
        let src_idx = sy * params.width + sx;
        let rotated_from = unpack(from_buf[src_idx]);
        result = lerp_pixel(rotated_from, to_px, p);
    } else if params.kind_id == 12u {
        // Light Leak: additive coloured glow peaking at midpoint.
        // param0 = intensity, color = leak colour (default warm amber).
        let leak_color = vec3<f32>(params.color_r, params.color_g, params.color_b) * 255.0;
        let intensity = clamp(params.param0, 0.0, 2.0);
        let leak_t = pow(p * (1.0 - p) * 4.0, 0.7) * intensity;
        let base = lerp_pixel(from_px, to_px, p);
        // Pure additive — leak colour added uniformly so non-amber leaks look right.
        let add = leak_color * leak_t;
        result = vec4<f32>(
            min(base.x + add.x, 255.0),
            min(base.y + add.y, 255.0),
            min(base.z + add.z, 255.0),
            base.w,
        );
    } else {
        result = lerp_pixel(from_px, to_px, p);
    }

    output[idx] = pack(result);
}
";

const SHADER_COLOR_GRADE: &str = "
struct Params {
    width: u32,
    height: u32,
    exposure: f32,
    contrast: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    lift_r: f32,
    lift_g: f32,
    lift_b: f32,
    gamma_r: f32,
    gamma_g: f32,
    gamma_b: f32,
    gain_r: f32,
    gain_g: f32,
    gain_b: f32,
    offset_r: f32,
    offset_g: f32,
    offset_b: f32,
    hue: f32,
    global_mix: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let pixel = input[idx];
    let orig_r = f32(pixel & 0xFFu) / 255.0;
    let orig_g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let orig_b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = (pixel >> 24u) & 0xFFu;

    var r = orig_r;
    var g = orig_g;
    var b = orig_b;

    // 1. Exposure (2^stops)
    let exp_factor = pow(2.0, params.exposure);
    r = r * exp_factor;
    g = g * exp_factor;
    b = b * exp_factor;

    // 2. Tone controls (highlights/shadows/whites/blacks)
    let luma1 = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let hi_w = max(luma1 - 0.5, 0.0) * 2.0;
    let lo_w = max(0.5 - luma1, 0.0) * 2.0;
    let white_w = max(luma1 - 0.8, 0.0) * 5.0;
    let black_w = max(0.2 - luma1, 0.0) * 5.0;
    let tone_adj = params.highlights * hi_w + params.shadows * lo_w + params.whites * white_w + params.blacks * black_w;
    r = r + tone_adj;
    g = g + tone_adj;
    b = b + tone_adj;

    // 3. Lift/Gamma/Gain/Offset color wheels
    let luma2 = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let lift_w = max(1.0 - luma2, 0.0);
    let gain_w = max(luma2, 0.0);
    let gamma_w = max(sin(clamp(luma2, 0.0, 1.0) * 3.14159265), 0.0);
    r = r + params.lift_r * lift_w + params.gamma_r * gamma_w + params.gain_r * gain_w + params.offset_r;
    g = g + params.lift_g * lift_w + params.gamma_g * gamma_w + params.gain_g * gain_w + params.offset_g;
    b = b + params.lift_b * lift_w + params.gamma_b * gamma_w + params.gain_b * gain_w + params.offset_b;

    // 4. Contrast (S-curve around 0.5)
    let cs = 1.0 + params.contrast;
    r = (r - 0.5) * cs + 0.5;
    g = (g - 0.5) * cs + 0.5;
    b = (b - 0.5) * cs + 0.5;

    // 5. Saturation (BT.709 luma lerp)
    let luma3 = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    r = luma3 + (r - luma3) * params.saturation;
    g = luma3 + (g - luma3) * params.saturation;
    b = luma3 + (b - luma3) * params.saturation;

    // 6. Temperature / Tint
    r = r + params.temperature * 0.001;
    b = b - params.temperature * 0.001;
    g = g + params.tint * 0.001;

    // 7. Hue rotation (simplified: skip on GPU for now, CPU handles it)
    // Full HSL conversion in WGSL is expensive; apply on CPU path for hue != 0

    // 8. Global mix (blend with original)
    r = orig_r + (r - orig_r) * params.global_mix;
    g = orig_g + (g - orig_g) * params.global_mix;
    b = orig_b + (b - orig_b) * params.global_mix;

    // Pack result
    let ro = u32(clamp(r, 0.0, 1.0) * 255.0);
    let go = u32(clamp(g, 0.0, 1.0) * 255.0);
    let bo = u32(clamp(b, 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

const SHADER_HUESAT: &str = "
struct Params {
    width: u32,
    height: u32,
    mix: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
// LUT layout: 6 curves × 256 entries = indices 0..1535
// Curve 0: hue_vs_hue   (0..255)
// Curve 1: hue_vs_sat   (256..511)
// Curve 2: hue_vs_lum   (512..767)
// Curve 3: lum_vs_sat   (768..1023)
// Curve 4: sat_vs_sat   (1024..1279)
// Curve 5: orange_vs_sat (1280..1535)

fn lut_lookup(curve: u32, t: f32) -> f32 {
    let base = curve * 256u;
    let idx_f = t * 255.0;
    let idx0 = u32(clamp(idx_f, 0.0, 254.0));
    let idx1 = idx0 + 1u;
    let frac = idx_f - f32(idx0);
    return lut[base + idx0] * (1.0 - frac) + lut[base + idx1] * frac;
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> vec3<f32> {
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    if delta < 0.0001 {
        return vec3<f32>(0.0, 0.0, l);
    }
    let s = delta / (1.0 - abs(2.0 * l - 1.0));
    var h: f32;
    if mx == r {
        h = ((g - b) / delta) % 6.0;
        if h < 0.0 { h = h + 6.0; }
    } else if mx == g {
        h = (b - r) / delta + 2.0;
    } else {
        h = (r - g) / delta + 4.0;
    }
    h = h / 6.0;
    return vec3<f32>(h, clamp(s, 0.0, 1.0), l);
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> vec3<f32> {
    if s < 0.0001 {
        return vec3<f32>(l, l, l);
    }
    let c = (1.0 - abs(2.0 * l - 1.0)) * s;
    let x = c * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = l - c * 0.5;
    let h6 = u32(h * 6.0);
    var rgb: vec3<f32>;
    switch h6 {
        case 0u: { rgb = vec3<f32>(c, x, 0.0); }
        case 1u: { rgb = vec3<f32>(x, c, 0.0); }
        case 2u: { rgb = vec3<f32>(0.0, c, x); }
        case 3u: { rgb = vec3<f32>(0.0, x, c); }
        case 4u: { rgb = vec3<f32>(x, 0.0, c); }
        default: { rgb = vec3<f32>(c, 0.0, x); }
    }
    return rgb + vec3<f32>(m, m, m);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let pixel = input[idx];
    let orig_r = f32(pixel & 0xFFu) / 255.0;
    let orig_g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let orig_b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = (pixel >> 24u) & 0xFFu;

    let hsl = rgb_to_hsl(orig_r, orig_g, orig_b);
    var h = hsl.x;
    var s = hsl.y;
    var l = hsl.z;

    // Curve 0: Hue vs Hue (0.5 = no change)
    let hvh = lut_lookup(0u, h);
    h = (h + (hvh - 0.5) * 2.0) % 1.0;
    if h < 0.0 { h = h + 1.0; }

    // Curve 1: Hue vs Sat (0.5 = identity)
    let hvs = lut_lookup(1u, hsl.x);
    s = clamp(s * hvs * 2.0, 0.0, 1.0);

    // Curve 2: Hue vs Lum (0.5 = no change)
    let hvl = lut_lookup(2u, hsl.x);
    l = clamp(l + (hvl - 0.5), 0.0, 1.0);

    // Curve 3: Lum vs Sat (0.5 = identity)
    let lvs = lut_lookup(3u, hsl.z);
    s = clamp(s * lvs * 2.0, 0.0, 1.0);

    // Curve 4: Sat vs Sat
    let svs = lut_lookup(4u, hsl.y);
    s = clamp(svs, 0.0, 1.0);

    // Curve 5: Orange vs Sat (targeted at ~30°/360° ≈ 0.083)
    let ovs = lut_lookup(5u, hsl.y);
    let orange_dist = min(abs(hsl.x - 0.083), min(abs(hsl.x - 0.083 + 1.0), abs(hsl.x - 0.083 - 1.0)));
    let weight = clamp(1.0 - orange_dist * 8.0, 0.0, 1.0);
    s = s + (ovs - s) * weight;

    let rgb = hsl_to_rgb(h, s, l);

    // Mix with original
    let r = orig_r + (rgb.x - orig_r) * params.mix;
    let g = orig_g + (rgb.y - orig_g) * params.mix;
    let b = orig_b + (rgb.z - orig_b) * params.mix;

    let ro = u32(clamp(r, 0.0, 1.0) * 255.0);
    let go = u32(clamp(g, 0.0, 1.0) * 255.0);
    let bo = u32(clamp(b, 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

// ─── Qualifier (Secondary Color Correction) GPU Shader ──────────────────────

const SHADER_QUALIFIER: &str = "
struct Params {
    width: u32,
    height: u32,
    hue_center: f32,
    hue_width: f32,
    hue_soft: f32,
    sat_low: f32,
    sat_high: f32,
    sat_soft: f32,
    lum_low: f32,
    lum_high: f32,
    lum_soft: f32,
    invert: u32,
    sec_exposure: f32,
    sec_saturation: f32,
    sec_hue_shift: f32,
    sec_temperature: f32,
    matte_preview: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn circular_distance(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn soft_range_centered(dist: f32, width: f32, soft: f32) -> f32 {
    let half_w = width * 0.5;
    if dist <= half_w { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    let t = (dist - half_w) / soft;
    return clamp(1.0 - t, 0.0, 1.0);
}

fn soft_range(val: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    if val >= lo && val <= hi { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    if val < lo {
        return clamp(1.0 - (lo - val) / soft, 0.0, 1.0);
    }
    return clamp(1.0 - (val - hi) / soft, 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let pixel = input[idx];
    let r = f32(pixel & 0xFFu) / 255.0;
    let g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = pixel & 0xFF000000u;

    // RGB -> HSL
    let mx = max(r, max(g, b));
    let mn = min(r, min(g, b));
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    var s = 0.0;
    if delta > 0.0001 {
        s = clamp(delta / (1.0 - abs(2.0 * l - 1.0)), 0.0, 1.0);
    }
    var h = 0.0;
    if delta > 0.0001 {
        if mx == r {
            h = ((g - b) / delta) % 6.0;
            if h < 0.0 { h += 6.0; }
            h = h / 6.0;
        } else if mx == g {
            h = ((b - r) / delta + 2.0) / 6.0;
        } else {
            h = ((r - g) / delta + 4.0) / 6.0;
        }
    }

    // Qualifier matte
    let hue_dist = circular_distance(h, params.hue_center);
    let hue_m = soft_range_centered(hue_dist, params.hue_width, params.hue_soft);
    let sat_m = soft_range(s, params.sat_low, params.sat_high, params.sat_soft);
    let lum_m = soft_range(l, params.lum_low, params.lum_high, params.lum_soft);
    var matte = hue_m * sat_m * lum_m;
    if params.invert != 0u { matte = 1.0 - matte; }

    // Matte preview mode: output greyscale matte
    if params.matte_preview != 0u {
        let v = u32(clamp(matte * 255.0, 0.0, 255.0));
        output[idx] = v | (v << 8u) | (v << 16u) | a;
        return;
    }

    // Skip untouched pixels
    if matte < 0.001 {
        output[idx] = pixel;
        return;
    }

    // Secondary adjustments
    var gr = r;
    var gg = g;
    var gb = b;

    // Exposure
    if abs(params.sec_exposure) > 0.001 {
        let factor = pow(2.0, params.sec_exposure);
        gr = gr * factor;
        gg = gg * factor;
        gb = gb * factor;
    }

    // Temperature (R += t, B -= t)
    if abs(params.sec_temperature) > 0.0001 {
        gr = gr + params.sec_temperature;
        gb = gb - params.sec_temperature;
    }

    // Saturation
    if abs(params.sec_saturation - 1.0) > 0.001 {
        let luma = 0.2126 * gr + 0.7152 * gg + 0.0722 * gb;
        gr = luma + (gr - luma) * params.sec_saturation;
        gg = luma + (gg - luma) * params.sec_saturation;
        gb = luma + (gb - luma) * params.sec_saturation;
    }

    // Hue shift (rotation matrix)
    if abs(params.sec_hue_shift) > 0.001 {
        let cos_h = cos(params.sec_hue_shift);
        let sin_h = sin(params.sec_hue_shift);
        let ir = gr; let ig = gg; let ib = gb;
        gr = ir * (0.213 + cos_h * 0.787 - sin_h * 0.213)
           + ig * (0.715 - cos_h * 0.715 - sin_h * 0.715)
           + ib * (0.072 - cos_h * 0.072 + sin_h * 0.928);
        gg = ir * (0.213 - cos_h * 0.213 + sin_h * 0.143)
           + ig * (0.715 + cos_h * 0.285 + sin_h * 0.140)
           + ib * (0.072 - cos_h * 0.072 - sin_h * 0.283);
        gb = ir * (0.213 - cos_h * 0.213 - sin_h * 0.787)
           + ig * (0.715 - cos_h * 0.715 + sin_h * 0.715)
           + ib * (0.072 + cos_h * 0.928 + sin_h * 0.072);
    }

    // Blend through matte
    let ro = u32(clamp((r * (1.0 - matte) + gr * matte) * 255.0, 0.0, 255.0));
    let go2 = u32(clamp((g * (1.0 - matte) + gg * matte) * 255.0, 0.0, 255.0));
    let bo2 = u32(clamp((b * (1.0 - matte) + gb * matte) * 255.0, 0.0, 255.0));
    output[idx] = ro | (go2 << 8u) | (bo2 << 16u) | a;
}
";

// ─── Film Grain GPU Shader ──────────────────────────────────────────────────

const SHADER_GRAIN: &str = "
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    shadows: f32,
    highlights: f32,
    frame_seed: u32,
    monochrome: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn wang_hash(seed: u32) -> f32 {
    var x = seed;
    x = (x ^ 61u) ^ (x >> 16u);
    x = x * 9u;
    x = x ^ (x >> 4u);
    x = x * 0x27d4eb2du;
    x = x ^ (x >> 15u);
    return f32(x) / 4294967295.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let pixel = input[idx];
    let r = f32(pixel & 0xFFu) / 255.0;
    let g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = pixel & 0xFF000000u;
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

    // Pixel coordinates (for grain size variation)
    let px = idx % params.width;
    let py = idx / params.width;
    // Quantize by grain size to create larger particles
    let qx = u32(f32(px) / params.size);
    let qy = u32(f32(py) / params.size);
    let pixel_seed = qx * 1973u + qy * 9277u + params.frame_seed * 26699u;

    // Generate grain noise
    let grain_r = (wang_hash(pixel_seed) - 0.5) * params.amount;
    var grain_g = grain_r;
    var grain_b = grain_r;
    if params.monochrome == 0u {
        grain_g = (wang_hash(pixel_seed + 1u) - 0.5) * params.amount;
        grain_b = (wang_hash(pixel_seed + 2u) - 0.5) * params.amount;
    }

    // Weight grain by tonal region
    let shadow_w = (1.0 - luma) * params.shadows;
    let highlight_w = luma * params.highlights;
    let weight = clamp(shadow_w + highlight_w, 0.0, 1.5);

    // Apply grain
    let ro = u32(clamp((r + grain_r * weight) * 255.0, 0.0, 255.0));
    let go = u32(clamp((g + grain_g * weight) * 255.0, 0.0, 255.0));
    let bo = u32(clamp((b + grain_b * weight) * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | a;
}
";

// ─── Alpha Mask GPU Shaders ─────────────────────────────────────────────────

const SHADER_ALPHA_BLUR_H: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = idx % params.width;
    var sum: u32 = 0u;
    var count: u32 = 0u;
    let r = i32(params.radius);
    for (var dx = -r; dx <= r; dx++) {
        let nx = i32(x) + dx;
        if nx >= 0 && nx < i32(params.width) {
            sum += input[y * params.width + u32(nx)];
            count += 1u;
        }
    }
    output[idx] = sum / count;
}
";

const SHADER_ALPHA_BLUR_V: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = idx % params.width;
    var sum: u32 = 0u;
    var count: u32 = 0u;
    let r = i32(params.radius);
    for (var dy = -r; dy <= r; dy++) {
        let ny = i32(y) + dy;
        if ny >= 0 && ny < i32(params.height) {
            sum += input[u32(ny) * params.width + x];
            count += 1u;
        }
    }
    output[idx] = sum / count;
}
";

const SHADER_ALPHA_DILATE: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = idx % params.width;
    var max_val: u32 = 0u;
    let r = i32(params.radius);
    let r_sq = f32(r * r);
    for (var dy = -r; dy <= r; dy++) {
        let ny = i32(y) + dy;
        if ny < 0 || ny >= i32(params.height) { continue; }
        for (var dx = -r; dx <= r; dx++) {
            let nx = i32(x) + dx;
            if nx < 0 || nx >= i32(params.width) { continue; }
            let dist_sq = f32(dx * dx + dy * dy);
            if dist_sq <= r_sq {
                let val = input[u32(ny) * params.width + u32(nx)];
                if val > max_val { max_val = val; }
            }
        }
    }
    output[idx] = max_val;
}
";

// ─── Light effect shaders ──────────────────────────────────────────────────────

const SHADER_BLOOM_THRESHOLD: &str = "
struct Params { width: u32, height: u32, threshold: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = input[idx];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = f32((v >> 24u) & 0xFFu) / 255.0;
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if luma > params.threshold {
        let factor = (luma - params.threshold) / (1.0 - params.threshold + 1e-6);
        let br = u32(clamp(r * factor * 255.0, 0.0, 255.0));
        let bg = u32(clamp(g * factor * 255.0, 0.0, 255.0));
        let bb = u32(clamp(b * factor * 255.0, 0.0, 255.0));
        let ba = u32(a * 255.0);
        output[idx] = br | (bg << 8u) | (bb << 16u) | (ba << 24u);
    } else {
        output[idx] = 0u;
    }
}
";

const SHADER_BLOOM_BLEND: &str = "
struct Params { width: u32, height: u32, intensity: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> original: array<u32>;
@group(0) @binding(1) var<storage, read> bright: array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;
@group(0) @binding(3) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let ov = original[idx];
    let bv = bright[idx];
    let or = f32(ov & 0xFFu);
    let og = f32((ov >> 8u) & 0xFFu);
    let ob = f32((ov >> 16u) & 0xFFu);
    let oa = (ov >> 24u) & 0xFFu;
    let br = f32(bv & 0xFFu) * params.intensity;
    let bg = f32((bv >> 8u) & 0xFFu) * params.intensity;
    let bb = f32((bv >> 16u) & 0xFFu) * params.intensity;
    let rr = u32(clamp(or + br, 0.0, 255.0));
    let rg = u32(clamp(og + bg, 0.0, 255.0));
    let rb = u32(clamp(ob + bb, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (oa << 24u);
}
";

const SHADER_GLOW_BLEND: &str = "
struct Params { width: u32, height: u32, intensity: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> original: array<u32>;
@group(0) @binding(1) var<storage, read> blurred: array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;
@group(0) @binding(3) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let ov = original[idx];
    let bv = blurred[idx];
    let or = f32(ov & 0xFFu) / 255.0;
    let og = f32((ov >> 8u) & 0xFFu) / 255.0;
    let ob = f32((ov >> 16u) & 0xFFu) / 255.0;
    let oa = (ov >> 24u) & 0xFFu;
    let br = f32(bv & 0xFFu) / 255.0 * params.intensity;
    let bg = f32((bv >> 8u) & 0xFFu) / 255.0 * params.intensity;
    let bb = f32((bv >> 16u) & 0xFFu) / 255.0 * params.intensity;
    // Screen blend: 1 - (1-a)(1-b)
    let sr = 1.0 - (1.0 - or) * (1.0 - br);
    let sg = 1.0 - (1.0 - og) * (1.0 - bg);
    let sb = 1.0 - (1.0 - ob) * (1.0 - bb);
    let rr = u32(clamp(sr * 255.0, 0.0, 255.0));
    let rg = u32(clamp(sg * 255.0, 0.0, 255.0));
    let rb = u32(clamp(sb * 255.0, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (oa << 24u);
}
";

const SHADER_LIGHT_RAYS: &str = "
struct Params {
    width: u32, height: u32,
    center_x: f32, center_y: f32,
    threshold: f32, samples: u32,
    decay: f32, intensity: f32,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let cx = params.center_x * f32(params.width);
    let cy = params.center_y * f32(params.height);
    let dx = x - cx;
    let dy = y - cy;
    let step = params.decay / f32(params.samples);

    var r_sum: f32 = 0.0;
    var g_sum: f32 = 0.0;
    var b_sum: f32 = 0.0;
    var weight: f32 = 1.0;
    let decay_per_sample = 1.0 - step;

    for (var s: u32 = 0u; s < params.samples; s = s + 1u) {
        let t = f32(s) * step;
        let sx = clamp(u32(x - dx * t), 0u, params.width - 1u);
        let sy = clamp(u32(y - dy * t), 0u, params.height - 1u);
        let si = sy * params.width + sx;
        let sv = input[si];
        let sr = f32(sv & 0xFFu) / 255.0;
        let sg = f32((sv >> 8u) & 0xFFu) / 255.0;
        let sb = f32((sv >> 16u) & 0xFFu) / 255.0;
        let luma = 0.2126 * sr + 0.7152 * sg + 0.0722 * sb;
        if luma > params.threshold {
            r_sum += sr * weight;
            g_sum += sg * weight;
            b_sum += sb * weight;
        }
        weight *= decay_per_sample;
    }

    let inv_samples = 1.0 / f32(params.samples);
    let ray_r = r_sum * inv_samples * params.intensity;
    let ray_g = g_sum * inv_samples * params.intensity;
    let ray_b = b_sum * inv_samples * params.intensity;

    let ov = input[idx];
    let or = f32(ov & 0xFFu) / 255.0;
    let og = f32((ov >> 8u) & 0xFFu) / 255.0;
    let ob = f32((ov >> 16u) & 0xFFu) / 255.0;
    let oa = (ov >> 24u) & 0xFFu;

    let rr = u32(clamp((or + ray_r) * 255.0, 0.0, 255.0));
    let rg = u32(clamp((og + ray_g) * 255.0, 0.0, 255.0));
    let rb = u32(clamp((ob + ray_b) * 255.0, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (oa << 24u);
}
";

const SHADER_VIGNETTE: &str = "
struct Params { width: u32, height: u32, amount: f32, size: f32, softness: f32, roundness: f32, _pad: u32, _pad2: u32 }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let cx = f32(params.width) * 0.5;
    let cy = f32(params.height) * 0.5;
    let nx = (x - cx) / cx;
    let ny = (y - cy) / cy;
    let rect_dist = max(abs(nx), abs(ny));
    let circ_dist = sqrt(nx * nx + ny * ny);
    let dist = rect_dist * (1.0 - params.roundness) + circ_dist * params.roundness;
    let falloff = clamp((dist - params.size) / max(params.softness, 0.01), 0.0, 1.0);
    let factor = 1.0 + params.amount * falloff;
    let v = input[idx];
    let r = clamp(f32(v & 0xFFu) * factor, 0.0, 255.0);
    let g = clamp(f32((v >> 8u) & 0xFFu) * factor, 0.0, 255.0);
    let b = clamp(f32((v >> 16u) & 0xFFu) * factor, 0.0, 255.0);
    let a = (v >> 24u) & 0xFFu;
    output[idx] = u32(r) | (u32(g) << 8u) | (u32(b) << 16u) | (a << 24u);
}
";

const SHADER_LENS_FLARE: &str = "
struct Params {
    width: u32, height: u32, pos_x: f32, pos_y: f32,
    brightness: f32, flare_size: f32, ghost_count: u32, ghost_intensity: f32,
    chromatic_aberration: f32, _pad: u32, _pad2: u32, _pad3: u32,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = f32(idx % params.width);
    let py = f32(idx / params.width);
    let w = f32(params.width);
    let h = f32(params.height);
    let fx = params.pos_x * w;
    let fy = params.pos_y * h;
    let cx = w * 0.5;
    let cy = h * 0.5;
    let max_dim = max(w, h);
    let disc_r = params.flare_size * max_dim;
    var add_r: f32 = 0.0;
    var add_g: f32 = 0.0;
    var add_b: f32 = 0.0;
    let dist = sqrt((px - fx) * (px - fx) + (py - fy) * (py - fy));
    if dist < disc_r {
        let falloff = 1.0 - dist / disc_r;
        let glow = falloff * falloff * params.brightness;
        add_r += glow;
        add_g += glow * 0.9;
        add_b += glow * 0.7;
    }
    for (var gi: u32 = 0u; gi < params.ghost_count; gi = gi + 1u) {
        let t = (f32(gi) + 1.0) / (f32(params.ghost_count) + 1.0);
        let gx = fx + (cx - fx) * t * 2.0;
        let gy = fy + (cy - fy) * t * 2.0;
        let gr = disc_r * (0.3 + t * 0.4);
        let gdist = sqrt((px - gx) * (px - gx) + (py - gy) * (py - gy));
        if gdist < gr {
            let gf = 1.0 - gdist / gr;
            let glow = gf * gf * params.ghost_intensity * params.brightness;
            let ca = params.chromatic_aberration * t;
            add_r += glow * (1.0 + ca);
            add_g += glow;
            add_b += glow * (1.0 - ca * 0.5);
        }
    }
    let v = input[idx];
    let rr = u32(clamp(f32(v & 0xFFu) + add_r * 255.0, 0.0, 255.0));
    let rg = u32(clamp(f32((v >> 8u) & 0xFFu) + add_g * 255.0, 0.0, 255.0));
    let rb = u32(clamp(f32((v >> 16u) & 0xFFu) + add_b * 255.0, 0.0, 255.0));
    let a = (v >> 24u) & 0xFFu;
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (a << 24u);
}
";

const SHADER_ANAMORPHIC_STREAK: &str = "
struct Params { width: u32, height: u32, threshold: f32, half_len: u32, intensity: f32, tint: f32, _pad: u32, _pad2: u32 }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let tint_r = 1.0 + (1.0 - params.tint) * 0.3;
    let tint_b = 1.0 + params.tint * 0.5;
    var sr: f32 = 0.0; var sg: f32 = 0.0; var sb: f32 = 0.0;
    let x_start = max(i32(x) - i32(params.half_len), 0);
    let x_end = min(x + params.half_len + 1u, params.width);
    let count = f32(x_end - u32(x_start));
    for (var sx: u32 = u32(x_start); sx < x_end; sx = sx + 1u) {
        let si = y * params.width + sx;
        let sv = input[si];
        let r = f32(sv & 0xFFu) / 255.0;
        let g = f32((sv >> 8u) & 0xFFu) / 255.0;
        let b = f32((sv >> 16u) & 0xFFu) / 255.0;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        if luma > params.threshold {
            let factor = (luma - params.threshold) / (1.0 - params.threshold + 1e-6);
            sr += r * factor; sg += g * factor; sb += b * factor;
        }
    }
    sr = sr / count; sg = sg / count; sb = sb / count;
    let v = input[idx];
    let rr = u32(clamp(f32(v & 0xFFu) + sr * tint_r * params.intensity * 255.0, 0.0, 255.0));
    let rg = u32(clamp(f32((v >> 8u) & 0xFFu) + sg * params.intensity * 255.0, 0.0, 255.0));
    let rb = u32(clamp(f32((v >> 16u) & 0xFFu) + sb * tint_b * params.intensity * 255.0, 0.0, 255.0));
    let a = (v >> 24u) & 0xFFu;
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (a << 24u);
}
";

const SHADER_FILM_HALATION: &str = "
struct Params { width: u32, height: u32, threshold: f32, intensity: f32, hal_r: f32, hal_g: f32, hal_b: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> original: array<u32>;
@group(0) @binding(1) var<storage, read> halation: array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;
@group(0) @binding(3) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let ov = original[idx];
    let hv = halation[idx];
    let or = f32(ov & 0xFFu) / 255.0;
    let og = f32((ov >> 8u) & 0xFFu) / 255.0;
    let ob = f32((ov >> 16u) & 0xFFu) / 255.0;
    let oa = (ov >> 24u) & 0xFFu;
    let hr = f32(hv & 0xFFu) / 255.0 * params.intensity;
    let hg = f32((hv >> 8u) & 0xFFu) / 255.0 * params.intensity;
    let hb = f32((hv >> 16u) & 0xFFu) / 255.0 * params.intensity;
    let sr = 1.0 - (1.0 - or) * (1.0 - hr);
    let sg = 1.0 - (1.0 - og) * (1.0 - hg);
    let sb = 1.0 - (1.0 - ob) * (1.0 - hb);
    let rr = u32(clamp(sr * 255.0, 0.0, 255.0));
    let rg = u32(clamp(sg * 255.0, 0.0, 255.0));
    let rb = u32(clamp(sb * 255.0, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (oa << 24u);
}
";

const SHADER_LUT3D: &str = "
struct Params {
    width: u32,
    height: u32,
    lut_size: u32,
    opacity: f32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;

fn lut_sample(ri: u32, gi: u32, bi: u32) -> vec3<f32> {
    let n = params.lut_size;
    let idx = (bi * n * n + gi * n + ri) * 3u;
    return vec3<f32>(lut[idx], lut[idx + 1u], lut[idx + 2u]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let pixel_idx = gid.x;
    if pixel_idx >= params.width * params.height { return; }

    let packed = input[pixel_idx];
    let r_in = f32(packed & 0xFFu) / 255.0;
    let g_in = f32((packed >> 8u) & 0xFFu) / 255.0;
    let b_in = f32((packed >> 16u) & 0xFFu) / 255.0;
    let a = packed >> 24u;

    let n1 = f32(params.lut_size - 1u);

    let r_idx = clamp(r_in * n1, 0.0, n1);
    let g_idx = clamp(g_in * n1, 0.0, n1);
    let b_idx = clamp(b_in * n1, 0.0, n1);

    let r0 = min(u32(r_idx), params.lut_size - 2u);
    let g0 = min(u32(g_idx), params.lut_size - 2u);
    let b0 = min(u32(b_idx), params.lut_size - 2u);
    let r1 = r0 + 1u;
    let g1 = g0 + 1u;
    let b1 = b0 + 1u;

    let rf = r_idx - f32(r0);
    let gf = g_idx - f32(g0);
    let bf = b_idx - f32(b0);

    // Trilinear interpolation
    let c000 = lut_sample(r0, g0, b0);
    let c100 = lut_sample(r1, g0, b0);
    let c010 = lut_sample(r0, g1, b0);
    let c110 = lut_sample(r1, g1, b0);
    let c001 = lut_sample(r0, g0, b1);
    let c101 = lut_sample(r1, g0, b1);
    let c011 = lut_sample(r0, g1, b1);
    let c111 = lut_sample(r1, g1, b1);

    let c00 = mix(c000, c100, rf);
    let c01 = mix(c001, c101, rf);
    let c10 = mix(c010, c110, rf);
    let c11 = mix(c011, c111, rf);

    let c0 = mix(c00, c10, gf);
    let c1 = mix(c01, c11, gf);

    let graded = mix(c0, c1, bf);

    // Blend with original by opacity
    let orig = vec3<f32>(r_in, g_in, b_in);
    let result = mix(orig, graded, params.opacity);

    let out_r = u32(clamp(result.x * 255.0, 0.0, 255.0));
    let out_g = u32(clamp(result.y * 255.0, 0.0, 255.0));
    let out_b = u32(clamp(result.z * 255.0, 0.0, 255.0));
    output[pixel_idx] = out_r | (out_g << 8u) | (out_b << 16u) | (a << 24u);
}
";

// ─── New effect shaders ─────────────────────────────────────────────────────

const SHADER_INVERT: &str = "
struct Params {
    width: u32,
    height: u32,
    mix_val: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = input[idx];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;
    let m = params.mix_val;
    let or = r + (1.0 - r - r) * m;
    let og = g + (1.0 - g - g) * m;
    let ob = b + (1.0 - b - b) * m;
    let rr = u32(clamp(or * 255.0, 0.0, 255.0));
    let rg = u32(clamp(og * 255.0, 0.0, 255.0));
    let rb = u32(clamp(ob * 255.0, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (a << 24u);
}
";

const SHADER_POSTERIZE: &str = "
struct Params {
    width: u32,
    height: u32,
    levels: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = input[idx];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;
    let lvl = params.levels;
    let div = lvl - 1.0;
    let or = floor(r * lvl) / div;
    let og = floor(g * lvl) / div;
    let ob = floor(b * lvl) / div;
    let rr = u32(clamp(or * 255.0, 0.0, 255.0));
    let rg = u32(clamp(og * 255.0, 0.0, 255.0));
    let rb = u32(clamp(ob * 255.0, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (a << 24u);
}
";

const SHADER_MOSAIC: &str = "
struct Params {
    width: u32,
    height: u32,
    block_size: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let bx = u32(floor(f32(x) / params.block_size) * params.block_size);
    let by = u32(floor(f32(y) / params.block_size) * params.block_size);
    let cbx = min(bx, params.width - 1u);
    let cby = min(by, params.height - 1u);
    let src_idx = cby * params.width + cbx;
    output[idx] = input[src_idx];
}
";

const SHADER_EDGE_DETECT: &str = "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    mix_val: f32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn get_lum(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    let v = input[u32(cy) * params.width + u32(cx)];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    return 0.299 * r + 0.587 * g + 0.114 * b;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);

    // Sobel Gx
    let gx = -1.0 * get_lum(x - 1, y - 1) + 1.0 * get_lum(x + 1, y - 1)
           + -2.0 * get_lum(x - 1, y)     + 2.0 * get_lum(x + 1, y)
           + -1.0 * get_lum(x - 1, y + 1) + 1.0 * get_lum(x + 1, y + 1);
    // Sobel Gy
    let gy = -1.0 * get_lum(x - 1, y - 1) + -2.0 * get_lum(x, y - 1) + -1.0 * get_lum(x + 1, y - 1)
           +  1.0 * get_lum(x - 1, y + 1) +  2.0 * get_lum(x, y + 1) +  1.0 * get_lum(x + 1, y + 1);

    let edge = clamp(sqrt(gx * gx + gy * gy) * params.strength, 0.0, 1.0);

    let v = input[idx];
    let or = f32(v & 0xFFu) / 255.0;
    let og = f32((v >> 8u) & 0xFFu) / 255.0;
    let ob = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;
    let m = params.mix_val;
    let rr = u32(clamp((or * (1.0 - m) + edge * m) * 255.0, 0.0, 255.0));
    let rg = u32(clamp((og * (1.0 - m) + edge * m) * 255.0, 0.0, 255.0));
    let rb = u32(clamp((ob * (1.0 - m) + edge * m) * 255.0, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (a << 24u);
}
";

const SHADER_CHANNEL_MIXER: &str = "
struct Params {
    width: u32,
    height: u32,
    rr: f32,
    rg: f32,
    rb: f32,
    gr: f32,
    gg: f32,
    gb: f32,
    br: f32,
    bg: f32,
    bb: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = input[idx];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;
    let out_r = params.rr * r + params.rg * g + params.rb * b;
    let out_g = params.gr * r + params.gg * g + params.gb * b;
    let out_b = params.br * r + params.bg * g + params.bb * b;
    let cr = u32(clamp(out_r * 255.0, 0.0, 255.0));
    let cg = u32(clamp(out_g * 255.0, 0.0, 255.0));
    let cb = u32(clamp(out_b * 255.0, 0.0, 255.0));
    output[idx] = cr | (cg << 8u) | (cb << 16u) | (a << 24u);
}
";

const SHADER_DIRECTIONAL_BLUR: &str = "
struct Params {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    samples: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var sum_a = 0.0;
    let w = i32(params.width);
    let h = i32(params.height);
    for (var i = 0u; i < params.samples; i = i + 1u) {
        let sx = clamp(i32(round(x + params.dx * f32(i))), 0, w - 1);
        let sy = clamp(i32(round(y + params.dy * f32(i))), 0, h - 1);
        let si = u32(sy) * params.width + u32(sx);
        let v = input[si];
        sum_r += f32(v & 0xFFu);
        sum_g += f32((v >> 8u) & 0xFFu);
        sum_b += f32((v >> 16u) & 0xFFu);
        sum_a += f32(v >> 24u);
    }
    let inv = 1.0 / f32(params.samples);
    let rr = u32(clamp(sum_r * inv, 0.0, 255.0));
    let rg = u32(clamp(sum_g * inv, 0.0, 255.0));
    let rb = u32(clamp(sum_b * inv, 0.0, 255.0));
    let ra = u32(clamp(sum_a * inv, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (ra << 24u);
}
";

const SHADER_EMBOSS: &str = "
struct Params {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    strength: f32,
    mix: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }

    let x = idx % w;
    let y = idx / w;

    let pixel = input[idx];
    let orig_r = f32(pixel & 0xFFu) / 255.0;
    let orig_g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let orig_b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = (pixel >> 24u) & 0xFFu;

    // Skip border pixels
    if x == 0u || y == 0u || x >= w - 1u || y >= h - 1u {
        output[idx] = pixel;
        return;
    }

    // Sample in light direction and opposite direction
    let sx1 = clamp(i32(round(f32(x) + params.dx)), 0, i32(w) - 1);
    let sy1 = clamp(i32(round(f32(y) + params.dy)), 0, i32(h) - 1);
    let sx2 = clamp(i32(round(f32(x) - params.dx)), 0, i32(w) - 1);
    let sy2 = clamp(i32(round(f32(y) - params.dy)), 0, i32(h) - 1);

    let p1 = input[u32(sy1) * w + u32(sx1)];
    let p2 = input[u32(sy2) * w + u32(sx2)];

    let r1 = f32(p1 & 0xFFu) / 255.0;
    let g1 = f32((p1 >> 8u) & 0xFFu) / 255.0;
    let b1 = f32((p1 >> 16u) & 0xFFu) / 255.0;

    let r2 = f32(p2 & 0xFFu) / 255.0;
    let g2 = f32((p2 >> 8u) & 0xFFu) / 255.0;
    let b2 = f32((p2 >> 16u) & 0xFFu) / 255.0;

    // Emboss: difference scaled around 0.5 (neutral gray)
    let er = clamp(0.5 + (r1 - r2) * params.strength, 0.0, 1.0);
    let eg = clamp(0.5 + (g1 - g2) * params.strength, 0.0, 1.0);
    let eb = clamp(0.5 + (b1 - b2) * params.strength, 0.0, 1.0);

    // Mix with original
    let rr = orig_r + (er - orig_r) * params.mix;
    let rg = orig_g + (eg - orig_g) * params.mix;
    let rb = orig_b + (eb - orig_b) * params.mix;

    let ro = u32(clamp(rr, 0.0, 1.0) * 255.0);
    let go = u32(clamp(rg, 0.0, 1.0) * 255.0);
    let bo = u32(clamp(rb, 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

const SHADER_MIRROR: &str = "
struct Params {
    width: u32,
    height: u32,
    horizontal: u32,
    vertical: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    var sx = x;
    var sy = y;
    if params.horizontal != 0u {
        sx = params.width - 1u - x;
    }
    if params.vertical != 0u {
        sy = params.height - 1u - y;
    }
    output[idx] = input[sy * params.width + sx];
}
";

const SHADER_LENS_DISTORTION: &str = "
struct Params {
    width: u32,
    height: u32,
    distortion: f32,
    scale: f32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn sample_pixel(sx: f32, sy: f32, w: u32, h: u32) -> vec4<f32> {
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);
    let cx0 = u32(clamp(x0, 0, i32(w) - 1));
    let cy0 = u32(clamp(y0, 0, i32(h) - 1));
    let cx1 = u32(clamp(x1, 0, i32(w) - 1));
    let cy1 = u32(clamp(y1, 0, i32(h) - 1));
    let p00 = input[cy0 * w + cx0];
    let p10 = input[cy0 * w + cx1];
    let p01 = input[cy1 * w + cx0];
    let p11 = input[cy1 * w + cx1];
    let c00 = vec4<f32>(f32(p00 & 0xFFu), f32((p00 >> 8u) & 0xFFu), f32((p00 >> 16u) & 0xFFu), f32((p00 >> 24u) & 0xFFu));
    let c10 = vec4<f32>(f32(p10 & 0xFFu), f32((p10 >> 8u) & 0xFFu), f32((p10 >> 16u) & 0xFFu), f32((p10 >> 24u) & 0xFFu));
    let c01 = vec4<f32>(f32(p01 & 0xFFu), f32((p01 >> 8u) & 0xFFu), f32((p01 >> 16u) & 0xFFu), f32((p01 >> 24u) & 0xFFu));
    let c11 = vec4<f32>(f32(p11 & 0xFFu), f32((p11 >> 8u) & 0xFFu), f32((p11 >> 16u) & 0xFFu), f32((p11 >> 24u) & 0xFFu));
    let top = mix(c00, c10, fx);
    let bot = mix(c01, c11, fx);
    return mix(top, bot, fy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;
    let cx = f32(w) / 2.0;
    let cy = f32(h) / 2.0;
    let nx = (f32(x) - cx) / cx;
    let ny = (f32(y) - cy) / cy;
    let r2 = nx * nx + ny * ny;
    let factor = 1.0 + params.distortion * r2;
    let sx = (nx * factor / params.scale) * cx + cx;
    let sy = (ny * factor / params.scale) * cy + cy;
    let c = sample_pixel(sx, sy, w, h);
    let rr = u32(clamp(c.x, 0.0, 255.0));
    let rg = u32(clamp(c.y, 0.0, 255.0));
    let rb = u32(clamp(c.z, 0.0, 255.0));
    let ra = u32(clamp(c.w, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (ra << 24u);
}
";

const SHADER_DILATE: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);
    let r = i32(params.radius);
    var max_r = 0u;
    var max_g = 0u;
    var max_b = 0u;
    var max_a = 0u;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            let v = input[sy * w + sx];
            max_r = max(max_r, v & 0xFFu);
            max_g = max(max_g, (v >> 8u) & 0xFFu);
            max_b = max(max_b, (v >> 16u) & 0xFFu);
            max_a = max(max_a, (v >> 24u) & 0xFFu);
        }
    }
    output[idx] = max_r | (max_g << 8u) | (max_b << 16u) | (max_a << 24u);
}
";

const SHADER_ERODE: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);
    let r = i32(params.radius);
    var min_r = 255u;
    var min_g = 255u;
    var min_b = 255u;
    var min_a = 255u;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            let v = input[sy * w + sx];
            min_r = min(min_r, v & 0xFFu);
            min_g = min(min_g, (v >> 8u) & 0xFFu);
            min_b = min(min_b, (v >> 16u) & 0xFFu);
            min_a = min(min_a, (v >> 24u) & 0xFFu);
        }
    }
    output[idx] = min_r | (min_g << 8u) | (min_b << 16u) | (min_a << 24u);
}
";

const SHADER_DENOISE: &str = "
struct Params {
    width: u32,
    height: u32,
    spatial_sigma: f32,
    color_sigma: f32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);

    let cv = input[idx];
    let cr = f32(cv & 0xFFu) / 255.0;
    let cg = f32((cv >> 8u) & 0xFFu) / 255.0;
    let cb = f32((cv >> 16u) & 0xFFu) / 255.0;
    let ca = (cv >> 24u) & 0xFFu;
    let center_luma = 0.299 * cr + 0.587 * cg + 0.114 * cb;

    let kernel_size = min(i32(ceil(params.spatial_sigma * 2.0)), 10);
    let spatial_denom = 2.0 * params.spatial_sigma * params.spatial_sigma;
    let color_denom = 2.0 * params.color_sigma * params.color_sigma;

    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var total_weight = 0.0;

    for (var dy = -kernel_size; dy <= kernel_size; dy = dy + 1) {
        for (var dx = -kernel_size; dx <= kernel_size; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            let nv = input[sy * w + sx];
            let nr = f32(nv & 0xFFu) / 255.0;
            let ng = f32((nv >> 8u) & 0xFFu) / 255.0;
            let nb = f32((nv >> 16u) & 0xFFu) / 255.0;
            let n_luma = 0.299 * nr + 0.587 * ng + 0.114 * nb;

            let spatial_dist = f32(dx * dx + dy * dy);
            let spatial_w = exp(-spatial_dist / spatial_denom);
            let color_diff = center_luma - n_luma;
            let color_w = exp(-color_diff * color_diff / color_denom);
            let weight = spatial_w * color_w;

            sum_r += nr * weight;
            sum_g += ng * weight;
            sum_b += nb * weight;
            total_weight += weight;
        }
    }

    let inv = 1.0 / total_weight;
    let rr = u32(clamp(sum_r * inv * 255.0, 0.0, 255.0));
    let rg = u32(clamp(sum_g * inv * 255.0, 0.0, 255.0));
    let rb = u32(clamp(sum_b * inv * 255.0, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (ca << 24u);
}
";

const SHADER_GAMMA: &str = "
struct Params {
    width: u32,
    height: u32,
    inv_gamma: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = input[idx];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;
    let or = pow(r, params.inv_gamma);
    let og = pow(g, params.inv_gamma);
    let ob = pow(b, params.inv_gamma);
    let rr = u32(clamp(or * 255.0, 0.0, 255.0));
    let rg = u32(clamp(og * 255.0, 0.0, 255.0));
    let rb = u32(clamp(ob * 255.0, 0.0, 255.0));
    output[idx] = rr | (rg << 8u) | (rb << 16u) | (a << 24u);
}
";

const SHADER_HSV_ADJUST: &str = "
struct Params {
    width: u32,
    height: u32,
    hue_shift: f32,
    saturation: f32,
    value_mult: f32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = input[idx];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;

    // RGB to HSV
    let cmax = max(r, max(g, b));
    let cmin = min(r, min(g, b));
    let delta = cmax - cmin;

    var h = 0.0;
    if delta > 0.001 {
        if abs(cmax - r) < 0.001 {
            h = ((g - b) / delta) % 6.0;
        } else if abs(cmax - g) < 0.001 {
            h = (b - r) / delta + 2.0;
        } else {
            h = (r - g) / delta + 4.0;
        }
        h = h * 60.0;
        if h < 0.0 { h += 360.0; }
    }
    let s = select(0.0, delta / cmax, cmax > 0.001);
    let val = cmax;

    // Apply adjustments
    h = (h + params.hue_shift + 360.0) % 360.0;
    let new_s = clamp(s * params.saturation, 0.0, 1.0);
    let new_v = clamp(val * params.value_mult, 0.0, 1.0);

    // HSV to RGB
    let c = new_v * new_s;
    let x = c * (1.0 - abs((h / 60.0) % 2.0 - 1.0));
    let m = new_v - c;

    var rr = 0.0;
    var gg = 0.0;
    var bb = 0.0;
    if h < 60.0 { rr = c; gg = x; bb = 0.0; }
    else if h < 120.0 { rr = x; gg = c; bb = 0.0; }
    else if h < 180.0 { rr = 0.0; gg = c; bb = x; }
    else if h < 240.0 { rr = 0.0; gg = x; bb = c; }
    else if h < 300.0 { rr = x; gg = 0.0; bb = c; }
    else { rr = c; gg = 0.0; bb = x; }

    let ro = u32(clamp((rr + m) * 255.0, 0.0, 255.0));
    let go = u32(clamp((gg + m) * 255.0, 0.0, 255.0));
    let bo = u32(clamp((bb + m) * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

const SHADER_NOISE_GENERATOR: &str = "
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    monochrome: u32,
    seed: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hash(p: u32) -> f32 {
    var x = p;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = (x >> 16u) ^ x;
    return f32(x) / 4294967295.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = input[idx];
    var r = f32(v & 0xFFu) / 255.0;
    var g = f32((v >> 8u) & 0xFFu) / 255.0;
    var b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;

    let noise_seed = idx * 7919u + params.seed;
    if params.monochrome != 0u {
        let n = (hash(noise_seed) - 0.5) * 2.0 * params.amount;
        r = r + n;
        g = g + n;
        b = b + n;
    } else {
        r = r + (hash(noise_seed) - 0.5) * 2.0 * params.amount;
        g = g + (hash(noise_seed + 1u) - 0.5) * 2.0 * params.amount;
        b = b + (hash(noise_seed + 2u) - 0.5) * 2.0 * params.amount;
    }

    let ro = u32(clamp(r * 255.0, 0.0, 255.0));
    let go = u32(clamp(g * 255.0, 0.0, 255.0));
    let bo = u32(clamp(b * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

const SHADER_MEDIAN_FILTER: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);
    let r = i32(min(params.radius, 2u));

    // Collect neighborhood values (max 5x5 = 25)
    var vals_r: array<f32, 25>;
    var vals_g: array<f32, 25>;
    var vals_b: array<f32, 25>;
    var n = 0u;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            let p = input[sy * w + sx];
            vals_r[n] = f32(p & 0xFFu);
            vals_g[n] = f32((p >> 8u) & 0xFFu);
            vals_b[n] = f32((p >> 16u) & 0xFFu);
            n = n + 1u;
        }
    }

    // Partial selection sort to find median
    let mid = n / 2u;
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_r[j] < vals_r[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_r[i]; vals_r[i] = vals_r[min_idx]; vals_r[min_idx] = tmp;
        }
    }
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_g[j] < vals_g[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_g[i]; vals_g[i] = vals_g[min_idx]; vals_g[min_idx] = tmp;
        }
    }
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_b[j] < vals_b[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_b[i]; vals_b[i] = vals_b[min_idx]; vals_b[min_idx] = tmp;
        }
    }

    let orig = input[idx];
    let a = orig >> 24u;
    let ro = u32(clamp(vals_r[mid], 0.0, 255.0));
    let go = u32(clamp(vals_g[mid], 0.0, 255.0));
    let bo = u32(clamp(vals_b[mid], 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

const SHADER_RADIAL_BLUR: &str = "
struct Params {
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    amount: f32,
    samples: u32,
    mode: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn sample_pixel(sx: i32, sy: i32, w: u32, h: u32) -> vec3<f32> {
    let cx = u32(clamp(sx, 0, i32(w) - 1));
    let cy = u32(clamp(sy, 0, i32(h) - 1));
    let p = input[cy * w + cx];
    return vec3<f32>(
        f32(p & 0xFFu) / 255.0,
        f32((p >> 8u) & 0xFFu) / 255.0,
        f32((p >> 16u) & 0xFFu) / 255.0
    );
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = f32(idx % w);
    let y = f32(idx / w);

    let cx = params.center_x * f32(w);
    let cy = params.center_y * f32(h);
    let dx = x - cx;
    let dy = y - cy;
    let s = params.samples;

    var sum = vec3<f32>(0.0, 0.0, 0.0);

    if params.mode == 0u {
        // Zoom blur
        let dist = max(sqrt(dx * dx + dy * dy), 1.0);
        let scale = params.amount / f32(s) / dist;
        for (var i = 0u; i < s; i = i + 1u) {
            let t = (f32(i) / f32(s) - 0.5) * 2.0;
            let sx = i32(round(x + dx * t * scale));
            let sy = i32(round(y + dy * t * scale));
            sum = sum + sample_pixel(sx, sy, w, h);
        }
    } else {
        // Spin blur
        let angle_step = params.amount * 3.14159265 / 180.0 / f32(s);
        for (var i = 0u; i < s; i = i + 1u) {
            let t = (f32(i) / f32(s) - 0.5) * 2.0;
            let ang = t * angle_step;
            let cos_a = cos(ang);
            let sin_a = sin(ang);
            let sx = i32(round(cx + dx * cos_a - dy * sin_a));
            let sy = i32(round(cy + dx * sin_a + dy * cos_a));
            sum = sum + sample_pixel(sx, sy, w, h);
        }
    }

    let inv = 1.0 / f32(s);
    let orig = input[idx];
    let a = orig >> 24u;
    let ro = u32(clamp(sum.x * inv * 255.0, 0.0, 255.0));
    let go = u32(clamp(sum.y * inv * 255.0, 0.0, 255.0));
    let bo = u32(clamp(sum.z * inv * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

const SHADER_LEVELS: &str = "
struct Params {
    width: u32,
    height: u32,
    in_black: f32,
    in_white: f32,
    inv_gamma: f32,
    out_black: f32,
    out_white: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = input[idx];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;

    let in_range = max(params.in_white - params.in_black, 0.001);

    let mr = clamp((r - params.in_black) / in_range, 0.0, 1.0);
    let mg = clamp((g - params.in_black) / in_range, 0.0, 1.0);
    let mb = clamp((b - params.in_black) / in_range, 0.0, 1.0);

    let gr = pow(mr, params.inv_gamma);
    let gg = pow(mg, params.inv_gamma);
    let gb = pow(mb, params.inv_gamma);

    let fr = params.out_black + gr * (params.out_white - params.out_black);
    let fg = params.out_black + gg * (params.out_white - params.out_black);
    let fb = params.out_black + gb * (params.out_white - params.out_black);

    let ro = u32(clamp(fr * 255.0, 0.0, 255.0));
    let go = u32(clamp(fg * 255.0, 0.0, 255.0));
    let bo = u32(clamp(fb * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

const SHADER_EQUALIZE: &str = "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> cdf: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let v = input[idx];
    let r = f32(v & 0xFFu) / 255.0;
    let g = f32((v >> 8u) & 0xFFu) / 255.0;
    let b = f32((v >> 16u) & 0xFFu) / 255.0;
    let a = v >> 24u;

    // Compute luminance and look up equalized value from CDF
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let luma_idx = u32(clamp(luma * 255.0, 0.0, 255.0));
    let eq_luma = cdf[luma_idx];

    // Scale RGB proportionally to match new luminance
    let scale = select(eq_luma / luma, 1.0, luma < 0.001);
    let eq_r = clamp(r * scale, 0.0, 1.0);
    let eq_g = clamp(g * scale, 0.0, 1.0);
    let eq_b = clamp(b * scale, 0.0, 1.0);

    // Blend with original based on strength
    let fr = r + (eq_r - r) * params.strength;
    let fg = g + (eq_g - g) * params.strength;
    let fb = b + (eq_b - b) * params.strength;

    let ro = u32(clamp(fr * 255.0, 0.0, 255.0));
    let go = u32(clamp(fg * 255.0, 0.0, 255.0));
    let bo = u32(clamp(fb * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

const SHADER_LETTERBOX: &str = "
struct Params {
    width: u32,
    height: u32,
    aspect_ratio: f32,
    bar_color: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;

    let frame_aspect = f32(w) / f32(h);
    let target_aspect = params.aspect_ratio;

    var bar_height = 0.0;
    var bar_width = 0.0;
    if frame_aspect > target_aspect {
        let visible_w = f32(h) * target_aspect;
        bar_width = (f32(w) - visible_w) / 2.0;
    } else {
        let visible_h = f32(w) / target_aspect;
        bar_height = (f32(h) - visible_h) / 2.0;
    }

    let in_bar = f32(y) < bar_height || f32(y) >= f32(h) - bar_height
              || f32(x) < bar_width || f32(x) >= f32(w) - bar_width;

    let v = input[idx];
    if in_bar {
        let bar_val = select(0u, 255u, params.bar_color != 0u);
        let orig_r = f32(v & 0xFFu);
        let orig_g = f32((v >> 8u) & 0xFFu);
        let orig_b = f32((v >> 16u) & 0xFFu);
        let orig_a = f32(v >> 24u);
        let op = params.opacity;
        let bv = f32(bar_val);
        let nr = u32(clamp(orig_r * (1.0 - op) + bv * op, 0.0, 255.0));
        let ng = u32(clamp(orig_g * (1.0 - op) + bv * op, 0.0, 255.0));
        let nb = u32(clamp(orig_b * (1.0 - op) + bv * op, 0.0, 255.0));
        let na = u32(clamp(orig_a * (1.0 - op) + 255.0 * op, 0.0, 255.0));
        output[idx] = nr | (ng << 8u) | (nb << 16u) | (na << 24u);
    } else {
        output[idx] = v;
    }
}
";

const SHADER_CHROMATIC_ABERRATION: &str = "
struct Params {
    width: u32,
    height: u32,
    red_shift: f32,
    blue_shift: f32,
    center_x: f32,
    center_y: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;

    let cx = params.center_x * f32(w);
    let cy = params.center_y * f32(h);
    let dx = f32(x) - cx;
    let dy = f32(y) - cy;
    let dist = sqrt(dx * dx + dy * dy);
    let max_dist = max(sqrt(cx * cx + cy * cy), 1.0);
    let norm_dist = dist / max_dist;

    // Red channel: sample shifted
    let r_scale = 1.0 + params.red_shift * norm_dist / max_dist;
    let r_sx = clamp(i32(round(cx + dx * r_scale)), 0, i32(w) - 1);
    let r_sy = clamp(i32(round(cy + dy * r_scale)), 0, i32(h) - 1);
    let r_pixel = input[u32(r_sy) * w + u32(r_sx)];
    let r_val = r_pixel & 0xFFu;

    // Green channel: original position
    let g_pixel = input[idx];
    let g_val = (g_pixel >> 8u) & 0xFFu;
    let a_val = g_pixel >> 24u;

    // Blue channel: sample shifted
    let b_scale = 1.0 + params.blue_shift * norm_dist / max_dist;
    let b_sx = clamp(i32(round(cx + dx * b_scale)), 0, i32(w) - 1);
    let b_sy = clamp(i32(round(cy + dy * b_scale)), 0, i32(h) - 1);
    let b_pixel = input[u32(b_sy) * w + u32(b_sx)];
    let b_val = (b_pixel >> 16u) & 0xFFu;

    output[idx] = r_val | (g_val << 8u) | (b_val << 16u) | (a_val << 24u);
}
";

const SHADER_ROLLING_SHUTTER: &str = "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad: u32,
}

struct RowXform {
    a: f32, b: f32, tx: f32,
    c: f32, d: f32, ty: f32,
    _pad0: f32, _pad1: f32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> row_xforms: array<RowXform>;

fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu), f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu), f32((v >> 24u) & 0xFFu)
    );
}

fn sample_bilinear(fx: f32, fy: f32) -> vec4<f32> {
    let w = params.width;
    let h = params.height;
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;

    let xf = row_xforms[y];
    let fx = f32(x);
    let fy = f32(y);

    let src_x = xf.a * fx + xf.b * fy + xf.tx;
    let src_y = xf.c * fx + xf.d * fy + xf.ty;

    let color = sample_bilinear(src_x, src_y);
    let ro = u32(clamp(color.x, 0.0, 255.0));
    let go = u32(clamp(color.y, 0.0, 255.0));
    let bo = u32(clamp(color.z, 0.0, 255.0));
    let ao = u32(clamp(color.w, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (ao << 24u);
}
";

const SHADER_STABILIZATION: &str = "
struct Params {
    width: u32,
    height: u32,
    crop_zoom: f32,
    _pad: u32,
}

struct Homography3x3 {
    h0: f32, h1: f32, h2: f32,
    h3: f32, h4: f32, h5: f32,
    h6: f32, h7: f32, h8: f32,
    _p0: f32, _p1: f32, _p2: f32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> homography: Homography3x3;

fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xFFu), f32((v >> 8u) & 0xFFu),
        f32((v >> 16u) & 0xFFu), f32((v >> 24u) & 0xFFu)
    );
}

fn sample_bilinear(fx: f32, fy: f32) -> vec4<f32> {
    let w = params.width;
    let h = params.height;
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = idx % params.width;
    let py = idx / params.width;

    // Apply crop/zoom: center and scale
    let cx = f32(params.width) * 0.5;
    let cy = f32(params.height) * 0.5;
    let x = (f32(px) - cx) / params.crop_zoom + cx;
    let y = (f32(py) - cy) / params.crop_zoom + cy;

    // Apply homography (perspective divide for completeness, but h6/h7 ~ 0 for affine)
    let w_div = homography.h6 * x + homography.h7 * y + homography.h8;
    let src_x = (homography.h0 * x + homography.h1 * y + homography.h2) / w_div;
    let src_y = (homography.h3 * x + homography.h4 * y + homography.h5) / w_div;

    let color = sample_bilinear(src_x, src_y);
    let ro = u32(clamp(color.x, 0.0, 255.0));
    let go = u32(clamp(color.y, 0.0, 255.0));
    let bo = u32(clamp(color.z, 0.0, 255.0));
    let ao = u32(clamp(color.w, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (ao << 24u);
}
";

// ── Color Balance (temperature / tint / hue rotation / global mix) ────────────
const SHADER_COLOR_BALANCE: &str = "
struct Params {
    width: u32,
    height: u32,
    temperature: f32,
    tint: f32,
    hue: f32,
    global_mix: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let pixel = input[idx];
    let orig_r = f32(pixel & 0xFFu) / 255.0;
    let orig_g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let orig_b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = (pixel >> 24u) & 0xFFu;

    let t = params.temperature / 100.0 * 0.1;
    let g_tint = params.tint / 100.0 * 0.1;

    var r = orig_r + t;
    var g = orig_g + g_tint;
    var b = orig_b - t;

    // Hue rotation in RGB (Rodrigues rotation around (1,1,1)/sqrt(3) axis)
    if abs(params.hue) > 0.01 {
        let angle = params.hue * 3.14159265 / 180.0;
        let cos_a = cos(angle);
        let sin_a = sin(angle);
        let k = 1.0 / 3.0;
        let new_r = r * (cos_a + k * (1.0 - cos_a)) + g * (k * (1.0 - cos_a) - 0.57735 * sin_a) + b * (k * (1.0 - cos_a) + 0.57735 * sin_a);
        let new_g = r * (k * (1.0 - cos_a) + 0.57735 * sin_a) + g * (cos_a + k * (1.0 - cos_a)) + b * (k * (1.0 - cos_a) - 0.57735 * sin_a);
        let new_b = r * (k * (1.0 - cos_a) - 0.57735 * sin_a) + g * (k * (1.0 - cos_a) + 0.57735 * sin_a) + b * (cos_a + k * (1.0 - cos_a));
        r = new_r;
        g = new_g;
        b = new_b;
    }

    // Global mix
    r = orig_r + (r - orig_r) * params.global_mix;
    g = orig_g + (g - orig_g) * params.global_mix;
    b = orig_b + (b - orig_b) * params.global_mix;

    let ro = u32(clamp(r, 0.0, 1.0) * 255.0);
    let go = u32(clamp(g, 0.0, 1.0) * 255.0);
    let bo = u32(clamp(b, 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

// ── WheelSatBri (per-wheel saturation and brightness adjustments) ─────────────
const SHADER_WHEEL_SAT_BRI: &str = "
struct Params {
    width: u32,
    height: u32,
    lift_sat: f32,
    lift_bri: f32,
    gamma_sat: f32,
    gamma_bri: f32,
    gain_sat: f32,
    gain_bri: f32,
    offset_sat: f32,
    offset_bri: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let pixel = input[idx];
    var r = f32(pixel & 0xFFu) / 255.0;
    var g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    var b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = (pixel >> 24u) & 0xFFu;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

    let lift_w = max(1.0 - luma, 0.0);
    let gamma_w = max(sin(clamp(luma, 0.0, 1.0) * 3.14159265), 0.0);
    let gain_w = luma;

    let bri_adj = params.lift_bri * lift_w + params.gamma_bri * gamma_w + params.gain_bri * gain_w + params.offset_bri;

    r = r + bri_adj;
    g = g + bri_adj;
    b = b + bri_adj;

    let sat_adj = params.lift_sat * lift_w + params.gamma_sat * gamma_w + params.gain_sat * gain_w + params.offset_sat;
    let sat_factor = 1.0 + sat_adj;
    let luma2 = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    r = luma2 + (r - luma2) * sat_factor;
    g = luma2 + (g - luma2) * sat_factor;
    b = luma2 + (b - luma2) * sat_factor;

    let ro = u32(clamp(r, 0.0, 1.0) * 255.0);
    let go = u32(clamp(g, 0.0, 1.0) * 255.0);
    let bo = u32(clamp(b, 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

// ── RGB Curves (LUT-based curve application) ──────────────────────────────────
const SHADER_RGB_CURVES: &str = "
struct Params {
    width: u32,
    height: u32,
    preserve_luma: u32,
    mix: f32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
// LUT layout: 4 channels x 256 entries = 1024 floats
// 0..255   = master
// 256..511 = red
// 512..767 = green
// 768..1023 = blue

fn lut_lookup(channel: u32, val: f32) -> f32 {
    let base = channel * 256u;
    let idx_f = val * 255.0;
    let idx0 = u32(clamp(idx_f, 0.0, 254.0));
    let idx1 = idx0 + 1u;
    let frac = idx_f - f32(idx0);
    return lut[base + idx0] * (1.0 - frac) + lut[base + idx1] * frac;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let pixel = input[idx];
    let orig_r = f32(pixel & 0xFFu) / 255.0;
    let orig_g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let orig_b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = (pixel >> 24u) & 0xFFu;

    // Apply per-channel curves, then master
    var r = lut_lookup(1u, orig_r);
    var g = lut_lookup(2u, orig_g);
    var b = lut_lookup(3u, orig_b);
    r = lut_lookup(0u, r);
    g = lut_lookup(0u, g);
    b = lut_lookup(0u, b);

    // Preserve luma: scale output to match input luma
    if params.preserve_luma != 0u {
        let orig_luma = 0.2126 * orig_r + 0.7152 * orig_g + 0.0722 * orig_b;
        let new_luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        if new_luma > 0.0001 {
            let scale = orig_luma / new_luma;
            r = r * scale;
            g = g * scale;
            b = b * scale;
        }
    }

    // Mix with original
    r = orig_r + (r - orig_r) * params.mix;
    g = orig_g + (g - orig_g) * params.mix;
    b = orig_b + (b - orig_b) * params.mix;

    let ro = u32(clamp(r, 0.0, 1.0) * 255.0);
    let go = u32(clamp(g, 0.0, 1.0) * 255.0);
    let bo = u32(clamp(b, 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

// ── Mask matte generation (SDF-based power windows) ───────────────────────────
const SHADER_MASK: &str = "
struct Params {
    width: u32,
    height: u32,
    mask_count: u32,
    _pad: u32,
    m0_type: u32, m0_cx: f32, m0_cy: f32, m0_p1: f32, m0_p2: f32, m0_p3: f32, m0_feather: f32, m0_flags: u32,
    m1_type: u32, m1_cx: f32, m1_cy: f32, m1_p1: f32, m1_p2: f32, m1_p3: f32, m1_feather: f32, m1_flags: u32,
    m2_type: u32, m2_cx: f32, m2_cy: f32, m2_p1: f32, m2_p2: f32, m2_p3: f32, m2_feather: f32, m2_flags: u32,
    m3_type: u32, m3_cx: f32, m3_cy: f32, m3_p1: f32, m3_p2: f32, m3_p3: f32, m3_feather: f32, m3_flags: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn smooth_step_sdf(dist: f32, feather: f32) -> f32 {
    if dist <= 0.0 { return 1.0; }
    if feather > 0.0001 && dist < feather {
        let t = dist / feather;
        return 1.0 - t * t * (3.0 - 2.0 * t);
    }
    return 0.0;
}

fn eval_mask(nx: f32, ny: f32, aspect: f32,
    shape_type: u32, cx: f32, cy: f32, p1: f32, p2: f32, p3: f32,
    feather: f32, flags: u32) -> f32
{
    var dist: f32;
    switch shape_type {
        case 0u: {
            let dx = nx - cx;
            let dy = (ny - cy) / aspect;
            dist = sqrt(dx * dx + dy * dy) - p1;
        }
        case 1u: {
            let rad = p3 * 3.14159265 / 180.0;
            let cos_r = cos(rad);
            let sin_r = sin(rad);
            let dx = nx - cx;
            let dy = (ny - cy) / aspect;
            let lx = abs(dx * cos_r + dy * sin_r);
            let ly = abs(-dx * sin_r + dy * cos_r);
            let hw = p1 / 2.0;
            let hh = p2 / 2.0;
            let ox = lx - hw;
            let oy = ly - hh;
            let outside = sqrt(max(ox, 0.0) * max(ox, 0.0) + max(oy, 0.0) * max(oy, 0.0));
            let inside = min(max(ox, oy), 0.0);
            dist = outside + inside;
        }
        case 2u: {
            let gx = p1 - cx;
            let gy = (p2 - cy) / aspect;
            let glen = sqrt(gx * gx + gy * gy);
            if glen < 0.0001 { dist = 0.0; } else {
                let dx = nx - cx;
                let dy = (ny - cy) / aspect;
                let t = (dx * gx + dy * gy) / (glen * glen);
                dist = t - 0.5;
            }
        }
        default: { dist = 1.0; }
    }
    var m = smooth_step_sdf(dist, feather);
    if (flags & 1u) != 0u { m = 1.0 - m; }
    let opacity = f32((flags >> 8u) & 0xFFu) / 255.0;
    return m * opacity;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let x = idx % params.width;
    let y = idx / params.width;
    let nx = f32(x) / f32(max(params.width - 1u, 1u));
    let ny = f32(y) / f32(max(params.height - 1u, 1u));
    let aspect = f32(params.height) / f32(params.width);

    var matte: f32 = 0.0;
    if params.mask_count >= 1u { matte = max(matte, eval_mask(nx, ny, aspect, params.m0_type, params.m0_cx, params.m0_cy, params.m0_p1, params.m0_p2, params.m0_p3, params.m0_feather, params.m0_flags)); }
    if params.mask_count >= 2u { matte = max(matte, eval_mask(nx, ny, aspect, params.m1_type, params.m1_cx, params.m1_cy, params.m1_p1, params.m1_p2, params.m1_p3, params.m1_feather, params.m1_flags)); }
    if params.mask_count >= 3u { matte = max(matte, eval_mask(nx, ny, aspect, params.m2_type, params.m2_cx, params.m2_cy, params.m2_p1, params.m2_p2, params.m2_p3, params.m2_feather, params.m2_flags)); }
    if params.mask_count >= 4u { matte = max(matte, eval_mask(nx, ny, aspect, params.m3_type, params.m3_cx, params.m3_cy, params.m3_p1, params.m3_p2, params.m3_p3, params.m3_feather, params.m3_flags)); }

    let b = u32(clamp(matte, 0.0, 1.0) * 255.0);
    output[idx] = b | (b << 8u) | (b << 16u) | (255u << 24u);
}
";

// ── Vibrance (saturation-weighted boost) ──────────────────────────────────────
const SHADER_VIBRANCE: &str = "
struct Params { width: u32, height: u32, amount: f32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = input[idx];
    let r = f32(pixel & 0xFFu) / 255.0;
    let g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = (pixel >> 24u) & 0xFFu;
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let sat = mx - mn;
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let weight = 1.0 - clamp(sat, 0.0, 1.0);
    let boost = 1.0 + params.amount * weight;
    let ro = u32(clamp(luma + (r - luma) * boost, 0.0, 1.0) * 255.0);
    let go = u32(clamp(luma + (g - luma) * boost, 0.0, 1.0) * 255.0);
    let bo = u32(clamp(luma + (b - luma) * boost, 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

// ── Color Warper (12-sector hue/sat shifting with cosine interpolation) ───────
const SHADER_COLOR_WARPER: &str = "
struct Params {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
    hue0: f32, hue1: f32, hue2: f32, hue3: f32,
    hue4: f32, hue5: f32, hue6: f32, hue7: f32,
    hue8: f32, hue9: f32, hue10: f32, hue11: f32,
    sat0: f32, sat1: f32, sat2: f32, sat3: f32,
    sat4: f32, sat5: f32, sat6: f32, sat7: f32,
    sat8: f32, sat9: f32, sat10: f32, sat11: f32,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn rgb2hsl(r: f32, g: f32, b: f32) -> vec3<f32> {
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let d = mx - mn;
    let l = (mx + mn) * 0.5;
    if d < 0.0001 { return vec3<f32>(0.0, 0.0, l); }
    let s = d / (1.0 - abs(2.0 * l - 1.0));
    var h: f32;
    if mx == r { h = ((g - b) / d) % 6.0; if h < 0.0 { h += 6.0; } }
    else if mx == g { h = (b - r) / d + 2.0; }
    else { h = (r - g) / d + 4.0; }
    return vec3<f32>(h / 6.0, clamp(s, 0.0, 1.0), l);
}

fn hsl2rgb(h: f32, s: f32, l: f32) -> vec3<f32> {
    if s < 0.0001 { return vec3<f32>(l, l, l); }
    let c = (1.0 - abs(2.0 * l - 1.0)) * s;
    let x = c * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = l - c * 0.5;
    let h6 = u32(h * 6.0);
    var rgb: vec3<f32>;
    switch h6 {
        case 0u: { rgb = vec3<f32>(c, x, 0.0); }
        case 1u: { rgb = vec3<f32>(x, c, 0.0); }
        case 2u: { rgb = vec3<f32>(0.0, c, x); }
        case 3u: { rgb = vec3<f32>(0.0, x, c); }
        case 4u: { rgb = vec3<f32>(x, 0.0, c); }
        default: { rgb = vec3<f32>(c, 0.0, x); }
    }
    return rgb + vec3<f32>(m, m, m);
}

fn get_hue_shift(sector: u32) -> f32 {
    switch sector {
        case 0u: { return params.hue0; } case 1u: { return params.hue1; }
        case 2u: { return params.hue2; } case 3u: { return params.hue3; }
        case 4u: { return params.hue4; } case 5u: { return params.hue5; }
        case 6u: { return params.hue6; } case 7u: { return params.hue7; }
        case 8u: { return params.hue8; } case 9u: { return params.hue9; }
        case 10u: { return params.hue10; } default: { return params.hue11; }
    }
}
fn get_sat_shift(sector: u32) -> f32 {
    switch sector {
        case 0u: { return params.sat0; } case 1u: { return params.sat1; }
        case 2u: { return params.sat2; } case 3u: { return params.sat3; }
        case 4u: { return params.sat4; } case 5u: { return params.sat5; }
        case 6u: { return params.sat6; } case 7u: { return params.sat7; }
        case 8u: { return params.sat8; } case 9u: { return params.sat9; }
        case 10u: { return params.sat10; } default: { return params.sat11; }
    }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = input[idx];
    let r = f32(pixel & 0xFFu) / 255.0;
    let g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let a = (pixel >> 24u) & 0xFFu;
    let hsl = rgb2hsl(r, g, b);
    let h_deg = hsl.x * 360.0;
    let sector_f = h_deg / 30.0;
    let sector_low = u32(floor(sector_f)) % 12u;
    let sector_high = (sector_low + 1u) % 12u;
    let t = fract(sector_f);
    let blend = (1.0 - cos(t * 3.14159265)) * 0.5;
    let hue_shift = get_hue_shift(sector_low) * (1.0 - blend) + get_hue_shift(sector_high) * blend;
    let sat_shift = get_sat_shift(sector_low) * (1.0 - blend) + get_sat_shift(sector_high) * blend;
    let sat_weight = clamp(hsl.y, 0.0, 1.0);
    let new_h = ((h_deg + hue_shift * sat_weight) % 360.0 + 360.0) % 360.0 / 360.0;
    let new_s = clamp(hsl.y + sat_shift * sat_weight, 0.0, 1.0);
    let rgb = hsl2rgb(new_h, new_s, hsl.z);
    let ro = u32(clamp(rgb.x, 0.0, 1.0) * 255.0);
    let go = u32(clamp(rgb.y, 0.0, 1.0) * 255.0);
    let bo = u32(clamp(rgb.z, 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

// ── Input Transform (log-to-linear + sRGB gamma) ─────────────────────────────
// Uses a 256-entry 1D LUT per channel (pre-computed on CPU from the log format).
const SHADER_INPUT_TRANSFORM: &str = "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32, }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
// LUT layout: 256 entries (input 0..255 → output 0..1 sRGB)

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = input[idx];
    let ri = pixel & 0xFFu;
    let gi = (pixel >> 8u) & 0xFFu;
    let bi = (pixel >> 16u) & 0xFFu;
    let a = (pixel >> 24u) & 0xFFu;
    let ro = u32(clamp(lut[ri], 0.0, 1.0) * 255.0);
    let go = u32(clamp(lut[gi], 0.0, 1.0) * 255.0);
    let bo = u32(clamp(lut[bi], 0.0, 1.0) * 255.0);
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

// ── Clarity (midtone unsharp mask — horizontal blur pass) ─────────────────────
const SHADER_CLARITY_BLUR_H: &str = "
struct Params { width: u32, height: u32, radius: u32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    let r = i32(params.radius);
    var sum = 0.0;
    var count = 0u;
    for (var dx = -r; dx <= r; dx = dx + 1) {
        let sx = clamp(x + dx, 0, i32(params.width) - 1);
        let pix = input[u32(y) * params.width + u32(sx)];
        let pr = f32(pix & 0xFFu) / 255.0;
        let pg = f32((pix >> 8u) & 0xFFu) / 255.0;
        let pb = f32((pix >> 16u) & 0xFFu) / 255.0;
        sum += 0.2126 * pr + 0.7152 * pg + 0.0722 * pb;
        count += 1u;
    }
    let luma = sum / f32(count);
    output[idx] = u32(clamp(luma * 255.0, 0.0, 255.0));
}
";

// ── Clarity (midtone unsharp mask — vertical blur pass) ───────────────────────
const SHADER_CLARITY_BLUR_V: &str = "
struct Params { width: u32, height: u32, radius: u32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    let r = i32(params.radius);
    var sum = 0.0;
    var count = 0u;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        let sy = clamp(y + dy, 0, i32(params.height) - 1);
        let val = f32(input[u32(sy) * params.width + u32(x)]) / 255.0;
        sum += val;
        count += 1u;
    }
    output[idx] = u32(clamp(sum / f32(count) * 255.0, 0.0, 255.0));
}
";

// ── Clarity (apply unsharp mask with midtone weighting) ───────────────────────
const SHADER_CLARITY_APPLY: &str = "
struct Params { width: u32, height: u32, amount: f32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> blurred: array<u32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = input[idx];
    let r = f32(pixel & 0xFFu);
    let g = f32((pixel >> 8u) & 0xFFu);
    let b = f32((pixel >> 16u) & 0xFFu);
    let a = (pixel >> 24u) & 0xFFu;
    let luma = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0;
    let blurred_luma = f32(blurred[idx]) / 255.0;
    let detail = luma - blurred_luma;
    let midtone_weight = 1.0 - abs(2.0 * luma - 1.0);
    let adj = detail * params.amount * midtone_weight * 255.0;
    let ro = u32(clamp(r + adj, 0.0, 255.0));
    let go = u32(clamp(g + adj, 0.0, 255.0));
    let bo = u32(clamp(b + adj, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (a << 24u);
}
";

// ══════════════════════════════════════════════════════════════════════════════
// GPU Scope Rendering Shaders
// ══════════════════════════════════════════════════════════════════════════════

// ── Waveform bin accumulation (atomic histogram per column) ───────────────────
const SHADER_SCOPE_WAVEFORM: &str = "
struct Params { width: u32, height: u32, display_columns: u32, step: u32, }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    if (x % params.step != 0u) { return; }
    let pixel = input[idx];
    let r = f32(pixel & 0xFFu) / 255.0;
    let g = f32((pixel >> 8u) & 0xFFu) / 255.0;
    let b = f32((pixel >> 16u) & 0xFFu) / 255.0;
    let luma = clamp(0.2126 * r + 0.7152 * g + 0.0722 * b, 0.0, 1.0);
    let bin = u32(luma * 255.0);
    let col = x * params.display_columns / params.width;
    atomicAdd(&bins[col * 256u + bin], 1u);
}
";

// ── Parade bin accumulation (atomic per-channel per-column) ───────────────────
const SHADER_SCOPE_PARADE: &str = "
struct Params { width: u32, height: u32, display_columns: u32, step: u32, }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    if (x % params.step != 0u) { return; }
    let pixel = input[idx];
    let r = pixel & 0xFFu;
    let g = (pixel >> 8u) & 0xFFu;
    let b = (pixel >> 16u) & 0xFFu;
    let col = x * params.display_columns / params.width;
    let base = col * 256u;
    let dc256 = params.display_columns * 256u;
    atomicAdd(&bins[base + r], 1u);
    atomicAdd(&bins[dc256 + base + g], 1u);
    atomicAdd(&bins[dc256 * 2u + base + b], 1u);
}
";

// ── Histogram bin accumulation (global R/G/B/Luma) ────────────────────────────
const SHADER_SCOPE_HISTOGRAM: &str = "
struct Params { width: u32, height: u32, display_columns: u32, step: u32, }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    if (x % params.step != 0u) || (y % params.step != 0u) { return; }
    let pixel = input[idx];
    let r = pixel & 0xFFu;
    let g = (pixel >> 8u) & 0xFFu;
    let b = (pixel >> 16u) & 0xFFu;
    atomicAdd(&bins[r], 1u);
    atomicAdd(&bins[256u + g], 1u);
    atomicAdd(&bins[512u + b], 1u);
    let luma = u32(clamp(0.2126 * f32(r) + 0.7152 * f32(g) + 0.0722 * f32(b), 0.0, 255.0));
    atomicAdd(&bins[768u + luma], 1u);
}
";

// ── Waveform visualization renderer ──────────────────────────────────────────
// Reads bin data, produces RGBA image with DaVinci-style glow and white-hot cores.
const SHADER_SCOPE_RENDER_WF: &str = "
struct Params {
    out_w: u32, out_h: u32, num_cols: u32, max_count: u32,
}
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.out_w * params.out_h { return; }
    let px = idx % params.out_w;
    let py = idx / params.out_w;

    let col = px * params.num_cols / params.out_w;
    let bin = 255u - (py * 256u / params.out_h);

    let count = f32(bins[col * 256u + bin]);
    if count < 0.5 {
        output[idx] = 0x04u | (0x04u << 8u) | (0x07u << 16u) | (0xFFu << 24u);
        return;
    }

    // DaVinci log tone mapping
    let gain = 300.0;
    let brightness = log(1.0 + count * gain) / log(1.0 + gain);
    let out = pow(clamp(brightness, 0.0, 1.0), 1.0 / 2.2);

    // Green trace with subtle secondaries
    let ro = u32(clamp(out * 0.15 * 255.0, 0.0, 255.0));
    let go = u32(clamp(out * 255.0, 0.0, 255.0));
    let bo = u32(clamp(out * 0.15 * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (0xFFu << 24u);
}
";

// ── Parade visualization renderer ────────────────────────────────────────────
const SHADER_SCOPE_RENDER_PARADE: &str = "
struct Params {
    out_w: u32, out_h: u32, num_cols: u32, max_count: u32,
}
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.out_w * params.out_h { return; }
    let px = idx % params.out_w;
    let py = idx / params.out_w;
    let bin = 255u - (py * 256u / params.out_h);

    let gap = 2u;
    let section_w = (params.out_w - gap * 2u) / 3u;
    var channel = 3u;
    var local_x = 0u;
    if px < section_w {
        channel = 0u; local_x = px;
    } else if px >= section_w + gap && px < section_w * 2u + gap {
        channel = 1u; local_x = px - section_w - gap;
    } else if px >= section_w * 2u + gap * 2u {
        channel = 2u; local_x = px - section_w * 2u - gap * 2u;
    }

    if channel >= 3u {
        output[idx] = 0x04u | (0x04u << 8u) | (0x07u << 16u) | (0xFFu << 24u);
        return;
    }

    let col = local_x * params.num_cols / section_w;
    let dc256 = params.num_cols * 256u;
    let count = f32(bins[channel * dc256 + col * 256u + bin]);

    if count < 0.5 {
        output[idx] = 0x04u | (0x04u << 8u) | (0x07u << 16u) | (0xFFu << 24u);
        return;
    }

    // DaVinci log tone mapping: log(1 + count * gain) / log(1 + gain)
    let gain = 300.0;
    let brightness = log(1.0 + count * gain) / log(1.0 + gain);
    let out = pow(clamp(brightness, 0.0, 1.0), 1.0 / 2.2);

    // Pure channel color
    var r = 0.0; var g = 0.0; var b = 0.0;
    if channel == 0u { r = out; g = out * 0.06; b = out * 0.03; }
    else if channel == 1u { r = out * 0.03; g = out; b = out * 0.06; }
    else { r = out * 0.06; g = out * 0.08; b = out; }

    let ro = u32(clamp(r * 255.0, 0.0, 255.0));
    let go = u32(clamp(g * 255.0, 0.0, 255.0));
    let bo = u32(clamp(b * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (0xFFu << 24u);
}
";

// ── Histogram visualization renderer ─────────────────────────────────────────
const SHADER_SCOPE_RENDER_HIST: &str = "
struct Params {
    out_w: u32, out_h: u32, num_cols: u32, max_count: u32,
}
@group(0) @binding(0) var<storage, read> bins: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.out_w * params.out_h { return; }
    let px = idx % params.out_w;
    let py = idx / params.out_w;
    let max_c = f32(params.max_count);

    let bin = px * 256u / params.out_w;

    // Stack channels: luma (back), then R, G, B
    var r_acc = 0.0; var g_acc = 0.0; var b_acc = 0.0;
    let channels: array<vec4<f32>, 4> = array<vec4<f32>, 4>(
        vec4<f32>(0.35, 0.35, 0.35, f32(bins[768u + bin])),  // luma
        vec4<f32>(1.0, 0.08, 0.05, f32(bins[bin])),           // red
        vec4<f32>(0.05, 0.9, 0.08, f32(bins[256u + bin])),    // green
        vec4<f32>(0.08, 0.2, 1.0, f32(bins[512u + bin])),     // blue
    );

    for (var ch = 0u; ch < 4u; ch = ch + 1u) {
        let c = channels[ch];
        let count = c.w;
        if count < 1.0 { continue; }
        let height_frac = sqrt(count / max_c);
        let height_px = u32(height_frac * f32(params.out_h));
        let from_bottom = params.out_h - 1u - py;
        if from_bottom < height_px {
            let fade = 1.0 - f32(from_bottom) / f32(max(height_px, 1u)) * 0.5;
            r_acc += c.x * fade * 0.6;
            g_acc += c.y * fade * 0.6;
            b_acc += c.z * fade * 0.6;
        }
    }

    if r_acc < 0.01 && g_acc < 0.01 && b_acc < 0.01 {
        output[idx] = 0x04u | (0x04u << 8u) | (0x07u << 16u) | (0xFFu << 24u);
        return;
    }

    // Log tone mapping (same as parade/waveform)
    let gain = 300.0;
    let log_gain = log(1.0 + gain);
    let r_out = pow(log(1.0 + r_acc * gain) / log_gain, 1.0 / 2.2);
    let g_out = pow(log(1.0 + g_acc * gain) / log_gain, 1.0 / 2.2);
    let b_out = pow(log(1.0 + b_acc * gain) / log_gain, 1.0 / 2.2);
    let ro = u32(clamp(r_out * 255.0, 0.0, 255.0));
    let go = u32(clamp(g_out * 255.0, 0.0, 255.0));
    let bo = u32(clamp(b_out * 255.0, 0.0, 255.0));
    output[idx] = ro | (go << 8u) | (bo << 16u) | (0xFFu << 24u);
}
";
