//! Building the GPU effects pipeline (#2890): every compute pipeline and
//! its bind group layouts, compiled once a device and shared by its
//! renderers (#2688), and the buffers each renderer starts with.

use super::*;

impl EffectPipelines {
    /// Every pipeline and layout, compiled on `device`; `None` when a
    /// shader fails to build.
    pub(super) fn build(device: &std::sync::Arc<wgpu::Device>) -> Option<Self> {
        let device = std::sync::Arc::clone(device);
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

        // Posed pieces (#985): the transition's four bindings and the plan.
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let pieces_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pieces_bgl"),
            entries: &[
                storage(0, true),
                storage(1, true),
                storage(2, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage(4, true),
                // The subject's matte (#985 phase 3).
                storage(5, true),
            ],
        });
        let pieces_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pieces_pl"),
            bind_group_layouts: &[&pieces_bgl],
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
        let color_fade_pipeline = create_pipeline(&device, &pipeline_layout, "color_fade", SHADER_COLOR_FADE);
        let blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "blur_h", SHADER_BLUR_H);
        let blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "blur_v", SHADER_BLUR_V);
        let sharpen_pipeline = create_pipeline(&device, &pipeline_layout, "sharpen", SHADER_SHARPEN);
        let crop_pipeline = create_pipeline(&device, &pipeline_layout, "crop", SHADER_CROP);
        let transform_pipeline = create_pipeline(&device, &pipeline_layout, "transform", SHADER_TRANSFORM);
        let corner_pin_pipeline = create_pipeline(&device, &pipeline_layout, "corner_pin", SHADER_CORNER_PIN);
        let chroma_key_pipeline = create_pipeline(&device, &pipeline_layout, "chroma_key", SHADER_CHROMA_KEY);
        let bloom_threshold_pipeline = create_pipeline(&device, &pipeline_layout, "bloom_threshold", SHADER_BLOOM_THRESHOLD);
        // Bloom/glow blend uses 4-binding layout (input, bright/blurred, output, params) — same as transitions
        let bloom_blend_pipeline = create_pipeline(&device, &transition_pl, "bloom_blend", SHADER_BLOOM_BLEND);
        let glow_blend_pipeline = create_pipeline(&device, &transition_pl, "glow_blend", SHADER_GLOW_BLEND);
        let light_rays_pipeline = create_pipeline(&device, &pipeline_layout, "light_rays", SHADER_LIGHT_RAYS);
        let vignette_pipeline = create_pipeline(&device, &pipeline_layout, "vignette", SHADER_VIGNETTE);
        let living_gradient_pipeline = create_pipeline(&device, &pipeline_layout, "living_gradient", SHADER_LIVING_GRADIENT);
        let grain_generator_pipeline = create_pipeline(&device, &pipeline_layout, "grain_generator", SHADER_GRAIN_GENERATOR);
        let segments_pipeline = create_pipeline(&device, &pipeline_layout, "segments", SHADER_SEGMENTS);
        let generator_pipeline = create_pipeline(&device, &pipeline_layout, "generator", SHADER_GENERATOR);
        let film_grain_pipeline = create_pipeline(&device, &pipeline_layout, "film_grain", SHADER_FILM_GRAIN);
        let tilt_shift_blend_pipeline = create_pipeline(&device, &transition_pl, "tilt_shift_blend", SHADER_TILT_SHIFT_BLEND);
        let focus_blur_blend_pipeline = create_pipeline(&device, &transition_pl, "focus_blur_blend", SHADER_FOCUS_BLUR_BLEND);
        let focus_blur_premultiply_pipeline = create_pipeline(&device, &pipeline_layout, "focus_blur_premultiply", SHADER_FOCUS_BLUR_PREMULTIPLY);
        let old_film_pipeline = create_pipeline(&device, &pipeline_layout, "old_film", SHADER_OLD_FILM);
        let vhs_pipeline = create_pipeline(&device, &pipeline_layout, "vhs", SHADER_VHS);
        let lens_flare_pipeline = create_pipeline(&device, &pipeline_layout, "lens_flare", SHADER_LENS_FLARE);
        let anamorphic_streak_pipeline = create_pipeline(&device, &pipeline_layout, "anamorphic_streak", SHADER_ANAMORPHIC_STREAK);
        let film_halation_blend_pipeline = create_pipeline(&device, &transition_pl, "film_halation_blend", SHADER_FILM_HALATION);
        let invert_pipeline = create_pipeline(&device, &pipeline_layout, "invert", SHADER_INVERT);
        let mosaic_pipeline = create_pipeline(&device, &pipeline_layout, "mosaic", SHADER_MOSAIC);
        let edge_detect_pipeline = create_pipeline(&device, &pipeline_layout, "edge_detect", SHADER_EDGE_DETECT);
        let channel_mixer_pipeline = create_pipeline(&device, &pipeline_layout, "channel_mixer", SHADER_CHANNEL_MIXER);
        let directional_blur_pipeline = create_pipeline(&device, &pipeline_layout, "directional_blur", SHADER_DIRECTIONAL_BLUR);
        let emboss_pipeline = create_pipeline(&device, &pipeline_layout, "emboss", SHADER_EMBOSS);
        let mirror_pipeline = create_pipeline(&device, &pipeline_layout, "mirror", SHADER_MIRROR);
        let lens_distortion_pipeline = create_pipeline(&device, &pipeline_layout, "lens_distortion", SHADER_LENS_DISTORTION);
        let displace_pipeline = create_pipeline(&device, &pipeline_layout, "displace", SHADER_DISPLACE);
        let warp_pipeline = create_pipeline(&device, &pipeline_layout, "warp", SHADER_WARP);
        let gradient_map_pipeline = create_pipeline(&device, &pipeline_layout, "gradient_map", SHADER_GRADIENT_MAP);
        let reflection_pipeline = create_pipeline(&device, &pipeline_layout, "reflection", SHADER_REFLECTION);
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
        let binocular_vignette_pipeline = create_pipeline(&device, &pipeline_layout, "binocular_vignette", SHADER_BINOCULAR_VIGNETTE);
        let particles_pipeline = create_pipeline(&device, &pipeline_layout, "particles", SHADER_PARTICLES);
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

        // ApplyMatte needs the graded frame, the frame before the
        // secondary, and the matte, all three at once — one binding
        // more than any other chain op.
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let apply_matte_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("apply_matte_bgl"),
            entries: &[
                storage(0, true),
                storage(1, false),
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
                storage(3, true),
                storage(4, true),
            ],
        });
        let apply_matte_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("apply_matte_pl"),
            bind_group_layouts: &[&apply_matte_bgl],
            push_constant_ranges: &[],
        });

        // Difference key: the effects layout plus the clean plate.
        //   0: input  (storage, read)  — the kept picture, linear
        //   1: output (storage, write) — the matte
        //   2: params (uniform)
        //   3: plate  (storage, read)  — the plate, already resized to
        //                                the frame on the host and
        //                                linear like the input
        let difference_key_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("difference_key_bgl"),
            entries: &[
                storage(0, true),
                storage(1, false),
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
                storage(3, true),
            ],
        });
        let difference_key_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("difference_key_pl"),
            bind_group_layouts: &[&difference_key_bgl],
            push_constant_ranges: &[],
        });
        let difference_key_pipeline = create_pipeline(
            &device,
            &difference_key_pl,
            "difference_key",
            SHADER_DIFFERENCE_KEY,
        );

        // Temporal noise reduction (#1988): the effects layout plus the
        // two neighbour frames, linear working-space like the input.
        let temporal_denoise_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("temporal_denoise_bgl"),
            entries: &[
                storage(0, true),
                storage(1, false),
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
                storage(3, true),
                storage(4, true),
            ],
        });
        let temporal_denoise_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("temporal_denoise_pl"),
            bind_group_layouts: &[&temporal_denoise_bgl],
            push_constant_ranges: &[],
        });
        let temporal_denoise_pipeline = create_pipeline(&device, &temporal_denoise_pl, "temporal_denoise", SHADER_TEMPORAL_DENOISE);

        // Mask v2 bind group layout — purpose-built for the
        // storage-buffer mask architecture. 4 bindings:
        //   0: output (storage, write) — the matte vec4<f32> buffer
        //   1: params (uniform) — frame constants only (width,
        //                          height, mask_count, _pad)
        //   2: records (storage, read) — array<MaskRecord> with
        //                                 per-mask shape data + bezier
        //                                 offsets into binding 3
        //   3: points  (storage, read) — pooled bezier control points
        //                                 (vec2<f32>) across all masks
        // No "input" buffer (the original mask shader reused the
        // 3-binding effects layout and ignored its input slot — we
        // drop the slot entirely here for clarity).
        let mask_v2_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask_v2_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
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
        let mask_v2_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask_v2_pl"),
            bind_group_layouts: &[&mask_v2_bgl],
            push_constant_ranges: &[],
        });
        let huesat_pipeline = create_pipeline(&device, &huesat_pl, "huesat", SHADER_HUESAT);
        let lut3d_pipeline = create_pipeline(&device, &huesat_pl, "lut3d", SHADER_LUT3D);
        // Posterize reads its ladder's band edges from a table (#1443).
        let posterize_pipeline = create_pipeline(&device, &huesat_pl, "posterize", SHADER_POSTERIZE);
        let cst_pipeline = create_pipeline(&device, &huesat_pl, "colour_space_transform", SHADER_CST);
        let equalize_pipeline = create_pipeline(&device, &huesat_pl, "equalize", SHADER_EQUALIZE);
        let equalize_histogram_pipeline = create_pipeline(&device, &huesat_pl, "equalize_histogram", SHADER_EQUALIZE_HISTOGRAM);
        let equalize_cdf_pipeline = create_pipeline(&device, &huesat_pl, "equalize_cdf", SHADER_EQUALIZE_CDF);
        let rsc_pipeline = create_pipeline(&device, &huesat_pl, "rolling_shutter", SHADER_ROLLING_SHUTTER);
        let stabilization_pipeline = create_pipeline(&device, &huesat_pl, "stabilization", SHADER_STABILIZATION);
        let vibrance_pipeline = create_pipeline(&device, &pipeline_layout, "vibrance", SHADER_VIBRANCE);
        let scene_to_display_pipeline = create_pipeline(&device, &pipeline_layout, "scene_to_display", shader_scene_to_display());
        let color_warper_pipeline = create_pipeline(&device, &pipeline_layout, "color_warper", SHADER_COLOR_WARPER);
        let clarity_blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "clarity_blur_h", SHADER_CLARITY_BLUR_H);
        let clarity_blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "clarity_blur_v", SHADER_CLARITY_BLUR_V);
        let clarity_apply_pipeline = create_pipeline(&device, &huesat_pl, "clarity_apply", SHADER_CLARITY_APPLY);
        let mask_cutout_pipeline = create_pipeline(&device, &huesat_pl, "mask_cutout", SHADER_MASK_CUTOUT);
        let grade_mix_pipeline = create_pipeline(&device, &huesat_pl, "grade_mix", SHADER_GRADE_MIX);
        let apply_matte_pipeline = create_pipeline(&device, &apply_matte_pl, "apply_matte", SHADER_APPLY_MATTE_CHAIN);
        let deflicker_pipeline = create_pipeline(&device, &pipeline_layout, "deflicker", SHADER_DEFLICKER);
        let blend_mix_pipeline = create_pipeline(&device, &huesat_pl, "blend_mix", SHADER_BLEND_MIX);
        let merge_over_pipeline = create_pipeline(&device, &huesat_pl, "merge_over", SHADER_MERGE_OVER);
        let layer_over_pipeline = create_pipeline(&device, &huesat_pl, "layer_over", SHADER_LAYER_OVER);
        let mask_pipeline = create_pipeline(&device, &mask_v2_pl, "mask_v2", SHADER_MASK_V2);
        let color_balance_pipeline = create_pipeline(&device, &pipeline_layout, "color_balance", SHADER_COLOR_BALANCE);
        let hdr_zones_pipeline = create_pipeline(&device, &pipeline_layout, "hdr_zones", SHADER_HDR_ZONES);
        let wheel_sat_bri_pipeline = create_pipeline(&device, &pipeline_layout, "wheel_sat_bri", SHADER_WHEEL_SAT_BRI);
        let rgb_curves_pipeline = create_pipeline(&device, &huesat_pl, "rgb_curves", SHADER_RGB_CURVES);
        let qualifier_pipeline = create_pipeline(&device, &pipeline_layout, "qualifier", SHADER_QUALIFIER);
        let luma_key_pipeline = create_pipeline(&device, &pipeline_layout, "luma_key", SHADER_LUMA_KEY);
        let soft_clip_pipeline = create_pipeline(&device, &pipeline_layout, "soft_clip", SHADER_SOFT_CLIP);
        let grain_pipeline = create_pipeline(&device, &pipeline_layout, "grain", SHADER_GRAIN);
        let alpha_blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_blur_h", SHADER_ALPHA_BLUR_H);
        let alpha_blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_blur_v", SHADER_ALPHA_BLUR_V);
        let alpha_dilate_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_dilate", SHADER_ALPHA_DILATE);
        let title_style_pipeline = create_pipeline(&device, &pipeline_layout, "title_styles", gpu_shader!("title/title_styles.wgsl"));
        let title_metal_shot_pipeline = create_pipeline(&device, &pipeline_layout, "title_metal_shot", SHADER_TITLE_METAL_SHOT);
        let contrast_protect_pipeline = create_pipeline(&device, &pipeline_layout, "contrast_protect", contrast_protect::SHADER_CONTRAST_PROTECT);
        let surface_light_pipeline = create_pipeline(&device, &pipeline_layout, "surface_light", surface_light::SHADER_SURFACE_LIGHT);

        let transition_pipeline = create_pipeline(&device, &transition_pl, "transition", SHADER_TRANSITION);
        let pieces_pipeline = create_pipeline(&device, &pieces_pl, "pieces", SHADER_PIECES);
        let shutter_pipeline = create_pipeline(&device, &transition_pl, "shutter", SHADER_SHUTTER);
        // Linear-to-u8 bridge — uses the same 3-binding effects BGL.
        // Input: array<vec4<f32>>. Output: array<u32> (packed u8 RGBA).
        // Output buffer is 4× smaller than input; readback dominates
        // wall time on this op so the smaller output is the win.
        let bridge_u8_pipeline = create_pipeline(&device, &pipeline_layout, "bridge_u8", SHADER_LINEAR_TO_U8);
        // The 4-binding layout (input, output, params, then the 8-bit
        // codes' decode table), shader flipped to read u8 and write linear.
        let bridge_u8_to_linear_pipeline = create_pipeline(
            &device, &huesat_pl, "bridge_u8_to_linear", SHADER_U8_TO_LINEAR,
        );
        let bridge_u8_fit_pipeline = create_pipeline(
            &device, &huesat_pl, "bridge_u8_fit_to_linear", SHADER_U8_FIT_TO_LINEAR,
        );
        // Each 8-bit code's linear value, decoded once here as every CPU
        // route decodes it (`srgb8_to_linear`), so a
        // frame put on the GPU as 8-bit codes is the CPU's bit for bit.
        // The shaders' `pow` only approximates it, and a step in the chain
        // (Posterize's band edge, at a code that sits on one) then went
        // one way on the GPU and the other on the CPU.
        let srgb8_lut = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("srgb8_to_linear"),
            size: 512 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        srgb8_lut.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(&srgb8_to_linear()));
        srgb8_lut.unmap();
        // Same again at 16 bits a sample, for the export's software-decoded
        // frames — two u32 of input per pixel instead of one — with the
        // CPU's own 16-bit decode table (512 KiB), for the same reason
        // (#1451).
        let bridge_rgba64_to_linear_pipeline = create_pipeline(
            &device, &huesat_pl, "bridge_rgba64_to_linear", SHADER_RGBA64_TO_LINEAR,
        );
        let srgb16_table = srgb16_decode_table();
        let srgb16_lut = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("srgb16_decode"),
            size: std::mem::size_of_val(srgb16_table) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        srgb16_lut.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(srgb16_table));
        srgb16_lut.unmap();
        // A display transform whose numbers are baked on the CPU — ACES'
        // matrices and tone curves, BT.2020's gamut reach tables — reads
        // them from a fourth binding. All three pipelines below share
        // this layout because each wants that same slot, and the WGSL
        // calls it `tables` rather than naming one occupant.
        let display_tables_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("display_tables_bgl"),
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
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Per-workgroup light, for a PQ delivery's MaxCLL and
                // MaxFALL. Only the Y'CbCr writer declares it; the other
                // two pipelines bind the dummy and never read it, which
                // a layout permits.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let display_tables_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("display_tables_pl"),
            bind_group_layouts: &[&display_tables_bgl],
            push_constant_ranges: &[],
        });
        let aces_rec709_pipeline =
            create_pipeline(&device, &display_tables_pl, "aces_rec709", SHADER_ACES_REC709_U8);
        // The export's last step: linear frame in, packed Y/Cb/Cr out,
        // on the same layout so it can reach whichever tables its
        // delivery's display transform needs.
        let linear_to_yuv420_pipeline = create_pipeline(
            &device, &display_tables_pl, "linear_to_yuv420", SHADER_LINEAR_TO_YUV420,
        );
        let bt2020_display_pipeline = create_pipeline(
            &device, &display_tables_pl, "bt2020_display", SHADER_BT2020_DISPLAY_U8,
        );
        // Bound at slot 3 when the delivery's transform needs no baked
        // tables. Never read; a bind group still has to be complete.
        let no_display_tables = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("no_display_tables"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // The same for slot 4, which measures light. A *separate* buffer
        // and not the one above: slot 3 is read-only and slot 4 writable,
        // and one buffer bound as both in a single bind group is an
        // aliasing violation — which wgpu reports as the encode simply
        // failing, so it looks like "this frame cannot take the GPU path".
        let no_light = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("no_light"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // Both HDR kernels share the colourspace prelude with the P010
        // decoder, so the PQ/HLG curves on either side are one definition.
        let bridge_hdr10_pipeline = create_pipeline(
            &device, &pipeline_layout, "bridge_hdr10_to_linear", shader_hdr10_to_linear(),
        );
        let bridge_hdr16_pipeline = create_pipeline(
            &device, &pipeline_layout, "bridge_hdr16_to_linear", shader_hdr16_to_linear(),
        );
        let display_u8_pipeline = create_pipeline(
            &device, &pipeline_layout, "linear_to_display_u8", SHADER_LINEAR_TO_DISPLAY_U8,
        );
        let u8_to_texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("u8_to_texture_bgl"),
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
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
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
        let u8_to_texture_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("u8_to_texture_pl"),
            bind_group_layouts: &[&u8_to_texture_bgl],
            push_constant_ranges: &[],
        });
        let u8_to_texture_pipeline = create_pipeline(&device, &u8_to_texture_pl, "u8_to_texture", SHADER_U8_TO_TEXTURE);
        let blend_layer_pipeline = create_pipeline(&device, &pipeline_layout, "blend_layer", SHADER_BLEND_LAYER);
        let premultiply_pipeline = create_pipeline(&device, &pipeline_layout, "premultiply", SHADER_PREMULTIPLY);
        let view_channel_pipeline = create_pipeline(&device, &pipeline_layout, "view_channel", SHADER_VIEW_CHANNEL);
        let alpha_form_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_form", SHADER_ALPHA_FORM);
        let sprite_rows_pipeline = create_pipeline(&device, &pipeline_layout, "sprite_rows", SHADER_SPRITE_ROWS);
        let sprite_rows_f16_pipeline = create_pipeline(&device, &pipeline_layout, "sprite_rows_f16", SHADER_SPRITE_ROWS_F16);
        let sprite_box_f16_pipeline = create_pipeline(&device, &pipeline_layout, "sprite_box_f16", SHADER_SPRITE_BOX_F16);
        let sprite_place_pipeline = create_pipeline(&device, &pipeline_layout, "sprite_place", SHADER_SPRITE_PLACE);
        let storage_entry = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let sprite_glyphs_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite_glyphs_bgl"),
            entries: &[
                storage_entry(0, true),
                storage_entry(1, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage_entry(3, true),
                storage_entry(4, true),
            ],
        });
        let sprite_glyphs_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite_glyphs_pl"),
            bind_group_layouts: &[&sprite_glyphs_bgl],
            push_constant_ranges: &[],
        });
        let sprite_glyphs_pipeline = create_pipeline(&device, &sprite_glyphs_pl, "sprite_glyphs", SHADER_SPRITE_GLYPHS);
        let matte_extract_pipeline = create_pipeline(&device, &pipeline_layout, "matte_extract", SHADER_MATTE_EXTRACT);
        let mask_min_h_pipeline = create_pipeline(&device, &pipeline_layout, "mask_min_h", SHADER_MASK_MIN_H);
        let mask_min_v_pipeline = create_pipeline(&device, &pipeline_layout, "mask_min_v", SHADER_MASK_MIN_V);
        let matte_apply_pipeline = create_pipeline(&device, &pipeline_layout, "matte_apply", SHADER_MATTE_APPLY);
        let scale_alpha_pipeline = create_pipeline(&device, &pipeline_layout, "scale_alpha", SHADER_SCALE_ALPHA);
        // Same three-binding shape as every effect — in, out, params —
        // so it needs no layout of its own; its `out` is f32 rather than
        // packed bytes, which a bind group layout does not distinguish.
        let hdr_nit_map_pipeline = create_pipeline(
            &device, &pipeline_layout, "hdr_nit_map", SHADER_HDR_NIT_MAP,
        );

        let mask_mix_gpu = mask_mix::MaskMixGpu::new(&device);
        let layer = Arc::new(layer::LayerPipelines::new(&device, &pipeline_layout, &transition_pl));
        let pipeline_count = 68;
        info!(
            "GPU effects: compiled {} pipelines in {:.0}ms",
            pipeline_count,
            pipeline_compile_start.elapsed().as_secs_f64() * 1000.0,
        );

        // Check if any shader compilation failed
        if let Some(err) = pollster::block_on(device.pop_error_scope()) {
            crate::shader_check::shader_failed("effects", &err.to_string());
            return None;
        }

        Some(EffectPipelines {
            opacity_pipeline,
            color_fade_pipeline,
            mask_mix_gpu,
            blur_h_pipeline,
            blur_v_pipeline,
            sharpen_pipeline,
            crop_pipeline,
            transform_pipeline,
            corner_pin_pipeline,
            chroma_key_pipeline,
            bloom_threshold_pipeline,
            bloom_blend_pipeline,
            glow_blend_pipeline,
            light_rays_pipeline,
            vignette_pipeline,
            generator_pipeline,
            living_gradient_pipeline,
            grain_generator_pipeline,
            segments_pipeline,
            film_grain_pipeline,
            tilt_shift_blend_pipeline,
            focus_blur_blend_pipeline,
            focus_blur_premultiply_pipeline,
            old_film_pipeline,
            vhs_pipeline,
            lens_flare_pipeline,
            anamorphic_streak_pipeline,
            film_halation_blend_pipeline,
            invert_pipeline,
            posterize_pipeline,
            cst_pipeline,
            mosaic_pipeline,
            edge_detect_pipeline,
            channel_mixer_pipeline,
            directional_blur_pipeline,
            emboss_pipeline,
            mirror_pipeline,
            lens_distortion_pipeline,
            displace_pipeline,
            warp_pipeline,
            gradient_map_pipeline,
            reflection_pipeline,
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
            equalize_histogram_pipeline,
            equalize_cdf_pipeline,
            letterbox_pipeline,
            chromatic_aberration_pipeline,
            binocular_vignette_pipeline,
            particles_pipeline,
            rsc_pipeline,
            stabilization_pipeline,
            color_grade_pipeline,
            huesat_pipeline,
            huesat_bgl,
            apply_matte_bgl,
            lut3d_pipeline,
            color_balance_pipeline,
            hdr_zones_pipeline,
            wheel_sat_bri_pipeline,
            rgb_curves_pipeline,
            vibrance_pipeline,
            scene_to_display_pipeline,
            color_warper_pipeline,
            clarity_blur_h_pipeline,
            clarity_blur_v_pipeline,
            clarity_apply_pipeline,
            mask_pipeline,
            mask_cutout_pipeline,
            grade_mix_pipeline,
            apply_matte_pipeline,
            deflicker_pipeline,
            blend_mix_pipeline,
            merge_over_pipeline,
            layer_over_pipeline,
            qualifier_pipeline,
            luma_key_pipeline,
            difference_key_pipeline,
            difference_key_bgl,
            temporal_denoise_pipeline,
            temporal_denoise_bgl,
            soft_clip_pipeline,
            grain_pipeline,
            alpha_blur_h_pipeline,
            alpha_blur_v_pipeline,
            alpha_dilate_pipeline,
            title_style_pipeline,
            title_metal_shot_pipeline,
            contrast_protect_pipeline,
            surface_light_pipeline,
            transition_pipeline,
            pieces_pipeline,
            shutter_pipeline,
            bridge_u8_pipeline,
            bridge_u8_to_linear_pipeline,
            bridge_u8_fit_pipeline,
            srgb8_lut,
            bridge_rgba64_to_linear_pipeline,
            srgb16_lut,
            linear_to_yuv420_pipeline,
            aces_rec709_pipeline,
            display_tables_bgl,
            no_display_tables,
            no_light,
            bt2020_display_pipeline,
            bridge_hdr10_pipeline,
            bridge_hdr16_pipeline,
            display_u8_pipeline,
            hdr_nit_map_pipeline,
            u8_to_texture_pipeline,
            u8_to_texture_bgl,
            sprite_place_pipeline,
            sprite_glyphs_bgl,
            sprite_glyphs_pipeline,
            blend_layer_pipeline,
            premultiply_pipeline,
            view_channel_pipeline,
            alpha_form_pipeline,
            sprite_rows_pipeline,
            sprite_rows_f16_pipeline,
            sprite_box_f16_pipeline,
            matte_extract_pipeline,
            mask_min_h_pipeline,
            mask_min_v_pipeline,
            matte_apply_pipeline,
            scale_alpha_pipeline,
            bind_group_layout,
            transition_bgl,
            pieces_bgl,
            layer,
        })
    }
}

impl GpuEffectsPipeline {
    pub(super) fn build_from_device_queue(
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
        device_lost: Arc<AtomicBool>,
    ) -> Option<Self> {
        let pipelines = EffectPipelines::shared(&device)?;
        let layer_gpu = layer::LayerGpu::new(&device, Arc::clone(&pipelines.layer));

        // What a renderer writes as it draws, its own.
        //
        // The hue/sat LUT buffer, sized from the same constant the
        // uploads are. A buffer left a curve short would not fail to
        // compile: the binding is a runtime-sized `array<f32>`. It would
        // fail at the upload instead — `queue.write_buffer` validates the
        // write and an oversized one reaches the uncaptured-error handler —
        // so the symptom is a logged validation error and a LUT that never
        // arrives, not a silent out-of-bounds read.
        let huesat_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("huesat_lut"),
            size: HUESAT_LUT_LEN as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // The equalize CDF: 256 f32. Its own buffer — see the field.
        let equalize_cdf_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("equalize_cdf"),
            size: EQUALIZE_CDF_LEN as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let equalize_hist_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("equalize_histogram"),
            size: EQUALIZE_CDF_LEN as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        // RGB curves LUT buffer: 4 channels × 256 entries × 4 bytes = 4096 bytes
        let rgb_curves_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rgb_curves_lut"),
            size: 4 * 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Ring of per-dispatch params buffers for the chain path. Each
        // chain dispatch claims its own slot so the params write lands
        // in a buffer no later dispatch in the same submit reads from.
        let stack_params_ring: Vec<wgpu::Buffer> = (0..STACK_PARAMS_RING_SIZE)
            .map(|i| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("stack_params_{i}")),
                    size: PARAMS_SIZE,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let chain_params = ParamSlots::new(&device, CHAIN_PARAMS_RING_SIZE, PARAMS_SIZE, "chain_params");
        let effect_params = Some(ParamSlots::new(&device, EFFECT_PARAMS_RING_SIZE, PARAMS_SIZE, "effect_params"));

        info!("GPU effects pipeline initialized");

        Some(Self {
            readbacks: readbacks::Counters::default(),
            pipelines,
            layer_gpu,
            flow_gpu: None,
            device,
            queue,
            effect_luts: std::cell::RefCell::new(Vec::new()),
            rsc_row_buf: None,
            rsc_row_buf_size: 0,
            stabilization_buf: None,
            stabilization_buf_size: 0,
            huesat_lut_buf,
            equalize_cdf_buf,
            equalize_hist_buf,
            lut3d_buf: None,
            lut3d_buf_size: 0,
            grade_mix_src: None,
            rgb_curves_lut_buf,
            plate_buf: None,
            plate_key: None,
            plate_read_in_chain: false,
            title_style_scratch: None,
            title_metal_shots: std::collections::HashMap::new(),
            title_metal_shot_scratch: None,
            contrast_protect_scratch: None,
            surface_light_scratch: None,
            title_post_scratch: None,
            title_scene_bounds: None,
            title_outlines: Vec::new(),
            aces_rec709_data: None,
            bt2020_gamut_data: None,
            hdr_encode_data: None,
            hdr_nit_buf: None,
            hdr_nit_staging: None,
            display_u8_staging: None,
            stack_layers: Vec::new(),
            stack_title_slot: 0,
            stack_acc: None,
            stack_sprite_u8: None,
            stack_sprite_f16: None,
            stack_rects: None,
            stack_sprites: Vec::new(),
            stack_sprite_tick: 0,
            sprite_glyph_table: None,
            stack_buf_size: 0,
            stack_acc_touched: false,
            stack_dims: (0, 0),
            bridge_u8_out_buf: None,
            bridge_u8_out_size: 0,
            bridge_u8_staging_buf: None,
            bridge_u8_staging_size: 0,
            buf_a: None,
            buf_b: None,
            buf_c: None,
            shutter_sum: None,
            matte_buf: None,
            mask_records_buf: None,
            mask_points_buf: None,
            staging_read: None,
            params_buf,
            chain_params,
            effect_params,
            effects_incomplete: std::cell::Cell::new(false),
            slot_pool: SlotPool::new(Self::MAX_SLOTS),
            refuse_recording: false,
            // Start empty; populated by `rebuild_chain_bind_group_pools`
            // after the first `ensure_buffers` call (buf_a/b/c don't
            // exist until then).
            chain_bg_3binding: (0..CHAIN_PARAMS_RING_SIZE * 4).map(|_| None).collect(),
            chain_bg_huesat: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_rgb_curves: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_clarity_apply: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_mask_cutout: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_build_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            chain_encoder: None,
            current_buf_size: 0,
            memory_gauges: MemoryGauges::default(),
            neighbour_pool: std::sync::Mutex::new(crate::nv12_gpu::ResidentPool::with_cap("temporal_neighbour_kept", NEIGHBOURS_KEPT, crate::gpu_memory::Held::Neighbours, crate::gpu_memory::Made::Neighbour)),
            neighbours_in_flight: Default::default(),
            pressure_seen: crate::gpu_context::memory_pressure(),
            device_lost,
            submit_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            frame_upload_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            batch: std::cell::RefCell::new(None),
            batching: std::cell::Cell::new(false),
            stack_params_ring,
            stack_params_slot: std::cell::Cell::new(0),
            scopes: None,
            luma_coefs: [0.2126, 0.7152, 0.0722],
            authored_to_working: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            grade_encoding: 0,
        })
    }
}


