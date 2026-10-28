//! Every built-in effect and every registered shader has a CPU/GPU
//! parity check, or a stated reason it has none (#692).
//!
//! The CPU twins and the WGSL shaders are written by hand, side by side,
//! and drift between them is caught only where a test renders both and
//! compares. Nothing used to say where that was, so an effect or a shader
//! added without one could drift unseen: the viewer (GPU) and a CPU
//! render would then disagree, which has been the real bug more than
//! once. This file is that map, and it is checked:
//!
//! * every `BuiltinEffect` variant, read from the enum's source, has an
//!   entry in [`EFFECTS`], and every shader in `gpu::shader_sources()` an
//!   entry in [`SHADERS`]: a new one without an entry fails here;
//! * every `SHADER_*` constant in the crate is registered (directly, or
//!   assembled with the shared prelude), so none escapes the WGSL
//!   validation and pack lints either;
//! * every case and test named exists, so the map cannot rot when a test
//!   is renamed or removed;
//! * every exemption says why, in a sentence.
//!
//! The map does not prove a test exercises what it is listed for; it
//! makes that claim visible and reviewable next to the code, and stops
//! the silent case where nobody claimed anything at all.

use std::collections::BTreeSet;

/// What checks one effect or shader against its CPU twin.
#[derive(Debug, Clone, Copy)]
enum Cover {
    /// A labelled case in `cpu_gpu_effect_parity.rs`'s `cases()`, rendered
    /// both ways by `every_effect_matches_between_cpu_and_gpu`.
    Case(&'static str),
    /// A test function, by its file under `crates/` and its name.
    Test(&'static str, &'static str),
    /// No CPU/GPU comparison, and why.
    Exempt(&'static str),
}
use Cover::*;

const EFFECT_PARITY: &str = "klipp-effects/tests/cpu_gpu_effect_parity.rs";
const EVERY_EFFECT: Cover = Test(EFFECT_PARITY, "every_effect_matches_between_cpu_and_gpu");
const LAYER: &str = "klipp-effects/tests/layer_gpu_parity.rs";
const STACK: &str = "klipp-effects/tests/gpu_stack_parity.rs";
const COMPOSITE: &str = "klipp-compose/tests/gpu_composite.rs";
const GRADE: Cover = Test("klipp-compose/tests/grade_cpu_gpu_parity.rs", "every_grade_control_matches_between_cpu_and_gpu");
const SECONDARY: &str = "klipp-nodes/tests/secondary_gpu_parity.rs";
const GLOW: Cover = Test("klipp-compose/tests/vector_glow_parity.rs", "revealed_stroke_halo_matches_cpu_and_gpu");

/// The motion-compensation effects: the compositor works out their motion
/// data per frame and gives the same to both arms (#698).
const MOTION: Cover = Test("klipp-effects/tests/motion_warp_parity.rs", "stabilisation_and_rolling_shutter_match_between_cpu_and_gpu");

const EFFECTS: &[(&str, Cover)] = &[
    ("Crop", Case("Crop")),
    ("Transform", Case("Transform")),
    ("Opacity", Case("Opacity")),
    ("ColorFade", Case("ColorFade")),
    ("GaussianBlur", Case("GaussianBlur")),
    ("FocusBlur", Case("FocusBlur down")),
    ("Sharpen", Case("Sharpen")),
    ("ChromaKey", Case("ChromaKey")),
    ("Bloom", Case("Bloom")),
    ("Glow", Case("Glow")),
    ("LightRays", Case("LightRays")),
    ("Vignette", Case("Vignette")),
    ("Diffusion", Case("Diffusion")),
    ("LensFlare", Case("LensFlare")),
    ("RoundedCorners", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("DropShadow", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("FillBars", Test(LAYER, "fill_bars_matches_cpu_on_gpu")),
    ("OuterGlow", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LightWrap", Case("LightWrap")),
    ("AnamorphicStreak", Case("AnamorphicStreak")),
    ("FilmHalation", Case("FilmHalation")),
    ("Invert", Case("Invert")),
    ("Posterize", Case("Posterize")),
    ("Mosaic", Case("Mosaic")),
    ("EdgeDetect", Case("EdgeDetect")),
    ("ChannelMixer", Case("ChannelMixer")),
    ("DirectionalBlur", Case("DirectionalBlur")),
    ("Emboss", Case("Emboss")),
    ("Mirror", Case("Mirror")),
    ("LensDistortion", Case("LensDistortion")),
    ("Displace", Case("Displace")),
    ("Warp", Case("Warp wave")),
    ("GradientMap", Case("GradientMap")),
    ("Reflection", Case("Reflection")),
    ("Dilate", Case("Dilate")),
    ("Erode", Case("Erode")),
    ("Denoise", Case("Denoise")),
    ("Gamma", Case("Gamma")),
    ("HSVAdjust", Case("HSVAdjust")),
    ("Vibrance", Case("Vibrance")),
    ("NoiseGenerator", Case("NoiseGenerator")),
    ("MedianFilter", Case("MedianFilter")),
    ("RadialBlur", Case("RadialBlur zoom")),
    ("Levels", Case("Levels")),
    ("Equalize", Case("Equalize")),
    ("Letterbox", Case("Letterbox")),
    ("ChromaticAberration", Case("ChromaticAberration")),
    ("FilmGrain", Case("FilmGrain mono")),
    ("TiltShift", Case("TiltShift")),
    ("OldFilm", Case("OldFilm")),
    ("Vhs", Case("Vhs")),
    ("RollingShutterCorrection", MOTION),
    ("TemporalDenoise", Exempt("CPU-only by design: the compositor lifts it out of the chain and averages the source with its neighbours at the head of the clip's pipeline; both chain arms pass through, and the GPU paths hand such a clip to the CPU (klipp-compose tests/temporal_denoise_gpu.rs)")),
    ("Deflicker", Case("Deflicker")),
    ("VideoStabilization", MOTION),
    ("SpeedRamp", Exempt("not a picture pass: the engine blends frames for it in the frame queue, and both chain arms pass through")),
    ("Frei0r", Exempt("CPU-only plugins: frei0r 1.x has no GPU surface; a frei0r node runs on the CPU between the GPU nodes around it (M4 step 5)")),
    ("CornerPin", Case("CornerPin")),
    ("Lut3D", Case("Lut3D")),
    ("ColourSpaceTransform", Case("ColourSpaceTransform")),
    ("PictureTransform", Case("PictureTransform")),
    ("Flicker", Case("Flicker exposure")),
    ("BinocularVignette", Case("BinocularVignette")),
    ("Particles", Case("Particles sparks")),
    // Its alpha is all it changes, which `cases()` does not compare (#1638).
    ("LumaKey", Test("klipp-effects/tests/luma_key_effect.rs", "the_luma_key_effect_matches_between_cpu_and_gpu")),
    ("Generator", Case("Generator linear")),
    ("GradeCompound", Exempt("a marker: the plan runs the grade compound as the executor's compound node, held in klipp-compose's grade_compound_render")),
    ("ClipBelow", Exempt("its kernels pass the picture: the executor draws what the render composited below, held in klipp-compose's clip_below_render")),
    ("MediaSource", Exempt("its kernels pass the picture: the executor reads another media item's frame, held in klipp-compose's media_source_render")),
    ("SurfacePin", Exempt("it records and applies as the corner pin it is in the frame (corner_pin::surface_as_pin), whose parity is checked; its fit held in klipp-compose's surface_pin_node on both routes")),
    ("TitleSource", Exempt("its kernels pass the picture: the renderer draws the title as a title clip's and the executor reads it, held in klipp-compose's title_source_render")),
    ("Merge", Exempt("its own kernels pass the picture: the plan's layer mixer draws it with its foreground, held in klipp-compose's merge_render")),
];

const SHADERS: &[(&str, Cover)] = &[
    ("SHADER_LIVING_GRADIENT", Test("klipp-compose/tests/living_gradient_gpu.rs", "gpu_living_gradient_matches_cpu_at_several_times")),
    ("SHADER_GRAIN_GENERATOR", Test("klipp-compose/tests/animated_generators_gpu.rs", "gpu_animated_generators_match_the_cpu")),
    ("SHADER_SEGMENTS", Test("klipp-compose/tests/animated_generators_gpu.rs", "gpu_animated_generators_match_the_cpu")),
    ("SHADER_MASK_MIX", Test("klipp-effects/tests/mask_mix.rs", "gpu_mask_mix_matches_the_independent_oracle")),
    ("SHADER_MASK_MIX_WINDOW", Test("klipp-effects/tests/window_mix.rs", "a_window_inside_the_mix_is_the_window_drawn_into_a_matte")),
    ("SHADER_MASK_MIX_IN_PLACE", Test("klipp-nodes/tests/masked_effect_cost.rs", "a_mixed_effect_is_its_input_and_result_mixed_in_place_or_not")),
    ("SHADER_MASK_MIX_WINDOW_IN_PLACE", Test("klipp-nodes/tests/masked_effect_cost.rs", "a_masked_effect_costs_one_slot_and_nothing_on_the_cpu")),
    ("LAYER_ROWS", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_BOUNDS", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_CAST", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_SPILL_RESET", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_SPILL_BOUNDS", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_SPILL_REGION", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_SPILL_CAST", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_SPILL_BLUR_H", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_SPILL_BLUR_V", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_SPILL_LAY", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_SPILL_FINAL", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_CORNERS", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_OVER", Test(LAYER, "rounded_and_grouped_spills_match_cpu_on_gpu")),
    ("LAYER_FILL_SMALL", Test(LAYER, "fill_bars_matches_cpu_on_gpu")),
    ("LAYER_FILL_OVER", Test(LAYER, "fill_bars_matches_cpu_on_gpu")),
    ("SHADER_OPACITY", EVERY_EFFECT),
    ("SHADER_BINOCULAR_VIGNETTE", EVERY_EFFECT),
    ("SHADER_PARTICLES", EVERY_EFFECT),
    ("SHADER_COLOR_FADE", EVERY_EFFECT),
    ("SHADER_BLUR_H", EVERY_EFFECT),
    ("SHADER_BLUR_V", EVERY_EFFECT),
    ("SHADER_SHARPEN", EVERY_EFFECT),
    ("SHADER_CROP", EVERY_EFFECT),
    ("SHADER_TRANSFORM", EVERY_EFFECT),
    ("SHADER_CORNER_PIN", EVERY_EFFECT),
    ("SHADER_CHROMA_KEY", EVERY_EFFECT),
    ("SHADER_LINEAR_TO_DISPLAY_U8", Test("klipp-engine/tests/hdr_monitor_gpu_matches_cpu.rs", "the_preview_encode_matches_the_cpu")),
    ("SHADER_HDR_NIT_MAP", Test("klipp-engine/tests/hdr_monitor_gpu_matches_cpu.rs", "the_nit_map_matches_the_cpu")),
    ("SHADER_LINEAR_TO_U8", Test("klipp-engine/tests/grade_to_texture_identity.rs", "encode_grade_to_texture_matches_cpu")),
    ("SHADER_U8_TO_LINEAR", Test("klipp-engine/tests/gpu_u8_to_linear_bridge.rs", "u8_to_linear_gpu_matches_cpu_exactly")),
    ("SHADER_RGBA64_TO_LINEAR", Test("klipp-engine/tests/gpu_rgba64_to_linear_bridge.rs", "rgba64_to_linear_gpu_matches_cpu")),
    ("SHADER_LINEAR_TO_YUV420", Test("klipp-engine/tests/gpu_yuv420_matches_cpu.rs", "gpu_yuv_matches_cpu_with_no_tone_map")),
    ("SHADER_ACES_REC709_U8", Test("klipp-engine/tests/aces_gpu_matches_cpu.rs", "aces_rec709_matches_the_cpu_on_noise")),
    ("SHADER_BT2020_DISPLAY_U8", Test("klipp-engine/tests/bt2020_gamut_gpu_matches_cpu.rs", "bt2020_display_matches_the_cpu_on_saturated_colour")),
    ("SHADER_TRANSITION", Test("klipp-effects/tests/cpu_gpu_transition_parity.rs", "every_transition_matches_between_cpu_and_gpu")),
    ("SHADER_PIECES", Test("klipp-effects/tests/piece_transitions.rs", "posed_pieces_match_between_cpu_and_gpu")),
    ("SHADER_SHUTTER", Test("klipp-compose/tests/gpu_composite.rs", "a_moving_clip_is_smeared_on_the_stack_as_the_cpu_smears_it")),
    ("SHADER_COLOR_GRADE", GRADE),
    ("SHADER_HUESAT", Test("klipp-effects/tests/equalize_and_huesat_share_nothing.rs", "a_huesat_grade_followed_by_equalize_matches_the_cpu")),
    ("SHADER_QUALIFIER", Test(SECONDARY, "a_secondary_matches_the_cpu_and_stays_on_the_gpu")),
    ("SHADER_LUMA_KEY", Test("klipp-effects/tests/luma_key_gpu_parity.rs", "gpu_matches_cpu_oracle_across_fixtures")),
    ("SHADER_SOFT_CLIP", Test("klipp-effects/tests/soft_clip_gpu_parity.rs", "gpu_matches_cpu_oracle_across_fixtures")),
    ("SHADER_HDR_ZONES", Test("klipp-effects/tests/hdr_zones_gpu_parity.rs", "the_standalone_pass_draws_the_cpu_zones")),
    ("SHADER_GRAIN", Test("klipp-nodes/tests/grain_denoise_parity.rs", "colour_grain_matches_the_shader")),
    ("TITLE_STYLES", Test("klipp-effects/tests/resident_title_styles.rs", "resident_styles_match_authored_title_composition_without_downloads")),
    ("SHADER_CONTRAST_PROTECT", Test("klipp-compose/tests/title_contrast_protect.rs", "protected_text_reads_over_a_bright_picture")),
    ("SHADER_SURFACE_LIGHT", Test("klipp-compose/tests/surface_shade_render.rs", "a_pinned_layer_takes_the_surfaces_shade_on_both_routes")),
    ("SHADER_MERGE_OVER", Test("klipp-compose/tests/merge_on_the_stack.rs", "a_merge_and_its_sources_stay_on_the_gpu_stack")),
    ("SHADER_TITLE_METAL_SHOT", Exempt("GPU only by design (#2706): a metal title's reflection of the shot behind it has no CPU twin, a frame drawn without a GPU shows the title's colour; what it draws is held in klipp-compose's title_metal_gpu (a_metal_reflects_the_shot_behind_it)")),
    ("SHADER_ALPHA_BLUR_H", GLOW),
    ("SHADER_ALPHA_BLUR_V", GLOW),
    ("SHADER_ALPHA_DILATE", GLOW),
    ("SHADER_BLOOM_THRESHOLD", EVERY_EFFECT),
    ("SHADER_BLOOM_BLEND", EVERY_EFFECT),
    ("SHADER_GLOW_BLEND", EVERY_EFFECT),
    ("SHADER_LIGHT_RAYS", EVERY_EFFECT),
    ("SHADER_VIGNETTE", EVERY_EFFECT),
    ("SHADER_LENS_FLARE", EVERY_EFFECT),
    ("SHADER_ANAMORPHIC_STREAK", EVERY_EFFECT),
    ("SHADER_FILM_HALATION", EVERY_EFFECT),
    ("SHADER_LUT3D", Test(EFFECT_PARITY, "the_lut_matches_between_cpu_and_gpu")),
    ("SHADER_INVERT", EVERY_EFFECT),
    ("SHADER_POSTERIZE", EVERY_EFFECT),
    ("SHADER_CST", Case("ColourSpaceTransform")),
    ("SHADER_GENERATOR", Case("Generator linear")),
    ("SHADER_MOSAIC", EVERY_EFFECT),
    ("SHADER_EDGE_DETECT", EVERY_EFFECT),
    ("SHADER_CHANNEL_MIXER", EVERY_EFFECT),
    ("SHADER_DIRECTIONAL_BLUR", EVERY_EFFECT),
    ("SHADER_EMBOSS", EVERY_EFFECT),
    ("SHADER_MIRROR", EVERY_EFFECT),
    ("SHADER_LENS_DISTORTION", EVERY_EFFECT),
    ("SHADER_DISPLACE", EVERY_EFFECT),
    ("SHADER_WARP", EVERY_EFFECT),
    ("SHADER_GRADIENT_MAP", EVERY_EFFECT),
    ("SHADER_REFLECTION", EVERY_EFFECT),
    ("SHADER_DILATE", EVERY_EFFECT),
    ("SHADER_ERODE", EVERY_EFFECT),
    ("SHADER_DENOISE", Test("klipp-nodes/tests/grain_denoise_parity.rs", "denoise_matches_the_shader_for_every_slider_pair")),
    ("SHADER_GAMMA", EVERY_EFFECT),
    ("SHADER_HSV_ADJUST", EVERY_EFFECT),
    ("SHADER_NOISE_GENERATOR", EVERY_EFFECT),
    ("SHADER_MEDIAN_FILTER", EVERY_EFFECT),
    ("SHADER_RADIAL_BLUR", EVERY_EFFECT),
    ("SHADER_LEVELS", EVERY_EFFECT),
    ("SHADER_EQUALIZE", EVERY_EFFECT),
    ("SHADER_EQUALIZE_HISTOGRAM", Test("klipp-effects/tests/equalize_gpu.rs", "the_gpu_histogram_and_cdf_are_the_cpus")),
    ("SHADER_EQUALIZE_CDF", Test("klipp-effects/tests/equalize_gpu.rs", "the_gpu_histogram_and_cdf_are_the_cpus")),
    ("SHADER_LETTERBOX", EVERY_EFFECT),
    ("SHADER_CHROMATIC_ABERRATION", EVERY_EFFECT),
    ("SHADER_ROLLING_SHUTTER", MOTION),
    ("SHADER_STABILIZATION", MOTION),
    ("SHADER_COLOR_BALANCE", GRADE),
    ("SHADER_WHEEL_SAT_BRI", GRADE),
    ("SHADER_RGB_CURVES", GRADE),
    ("SHADER_MASK_V2", Test("klipp-effects/tests/mask_gpu_parity.rs", "gpu_matches_cpu_oracle_across_fixtures")),
    ("SHADER_VIBRANCE", EVERY_EFFECT),
    ("SHADER_COLOR_WARPER", GRADE),
    ("SHADER_CLARITY_BLUR_H", GRADE),
    ("SHADER_CLARITY_BLUR_V", GRADE),
    ("SHADER_MASK_CUTOUT", Test("klipp-nodes/tests/mask_cutout_gpu_parity.rs", "a_mask_cutout_matches_the_cpu")),
    ("SHADER_GRADE_MIX", Test(SECONDARY, "the_secondarys_mix_agrees_on_both_paths")),
    ("SHADER_APPLY_MATTE_CHAIN", Test(SECONDARY, "a_secondary_matches_the_cpu_and_stays_on_the_gpu")),
    ("SHADER_CLARITY_APPLY", GRADE),
    ("SHADER_BLEND_LAYER", Test(STACK, "every_blend_mode_matches_the_cpu_stack")),
    ("SHADER_PREMULTIPLY", Test(STACK, "the_finish_premultiplies_like_the_cpu_stack")),
    ("SHADER_VIEW_CHANNEL", Test(STACK, "the_channel_view_finishes_like_the_cpu_stack")),
    ("SHADER_ALPHA_FORM", Test("klipp-compose/tests/golden_corpus.rs", "every_path_draws_the_corpus_alike")),
    ("SHADER_SPRITE_ROWS", Test(STACK, "a_sprite_decodes_as_the_cpu_bridge_does")),
    ("SHADER_MATTE_EXTRACT", Test(COMPOSITE, "a_luma_matte_with_feather_and_choke_matches")),
    ("SHADER_MASK_MIN_H", Test(COMPOSITE, "a_luma_matte_with_feather_and_choke_matches")),
    ("SHADER_MASK_MIN_V", Test(COMPOSITE, "a_luma_matte_with_feather_and_choke_matches")),
    ("SHADER_MATTE_APPLY", Test(COMPOSITE, "a_luma_matte_with_feather_and_choke_matches")),
    ("SHADER_SCALE_ALPHA", Test(COMPOSITE, "a_title_with_an_effect_and_a_faded_layer_match")),
    // A byte copy into the viewer's texture, no arithmetic: what it is
    // held to is the readback of the same encode, not a CPU twin.
    ("SHADER_U8_TO_TEXTURE", Test("klipp-effects/tests/display_to_texture.rs", "the_texture_holds_the_bytes_the_readback_returns")),
    ("SHADER_SPRITE_PLACE", Test(STACK, "a_posed_sprite_is_the_cpu_pose_uploaded")),
    ("SHADER_SPRITE_GLYPHS", Test(STACK, "posed_glyphs_are_the_cpu_glyph_pose_uploaded")),
    ("SHADER_U8_FIT_TO_LINEAR", Test(COMPOSITE, "a_layer_of_another_size_is_fitted_on_the_gpu")),
    ("SHADER_HDR10_TO_LINEAR", Test("klipp-engine/tests/hdr_gpu_graded_path.rs", "a_chain_seeded_from_hdr_codes_on_the_gpu_grades_like_the_cpu_path")),
    ("SHADER_HDR16_TO_LINEAR", Test("klipp-engine/tests/hdr_gpu_graded_path.rs", "a_deep_source_seeded_on_the_gpu_grades_like_the_cpu_path")),
    ("SHADER_SCENE_TO_DISPLAY", Test("klipp-nodes/tests/scene_to_display_node.rs", "the_display_step_on_the_chain_draws_the_cpu_step")),
    ("SHADER_SPRITE_ROWS_F16", Test(COMPOSITE, "the_gpu_stack_is_the_renderers_frame")),
    ("SHADER_SPRITE_BOX_F16", Test("klipp-compose/tests/title_routes_f16.rs", "each_route_lands_within_a_level_of_the_byte_frame")),
    ("SHADER_DIFFERENCE_KEY", Test(SECONDARY, "the_other_matte_sources_ride_the_chain_too")),
    ("SHADER_TEMPORAL_DENOISE", Test("klipp-effects/tests/temporal_denoise_gpu.rs", "the_gpu_draws_what_the_cpu_draws")),
    ("SHADER_FILM_GRAIN", EVERY_EFFECT),
    ("SHADER_FOCUS_BLUR_BLEND", EVERY_EFFECT),
    ("SHADER_FOCUS_BLUR_PREMULTIPLY", EVERY_EFFECT),
    ("SHADER_TILT_SHIFT_BLEND", EVERY_EFFECT),
    ("SHADER_OLD_FILM", EVERY_EFFECT),
    ("SHADER_VHS", EVERY_EFFECT),
    ("SHADER_DEFLICKER", EVERY_EFFECT),
    ("SHADER_BLEND_MIX", Test("klipp-nodes/tests/composite_gpu_parity.rs", "a_blend_over_the_source_matches_the_cpu")),
    ("SHADER_LAYER_OVER", Test("klipp-nodes/tests/composite_gpu_parity.rs", "a_layer_over_the_source_matches_the_cpu")),
    ("SHADER_CHAIN_THUMB", Test("klipp-nodes/tests/chain_thumbnails.rs", "gpu_chain_thumbnails_are_the_cpu_thumbnails")),
    ("SHADER_NV12", Test("klipp-engine/tests/display_yuv_cpu_matches_gpu.rs", "the_display_converter_matches_the_nv12_shader")),
    ("SHADER_P010", Test("klipp-engine/tests/yuv420p10_cpu_matches_gpu.rs", "the_planar_10bit_converter_matches_the_p010_shader")),
];

/// Shader constants that are not registered under their own name
/// because they are a body, compiled only after the shared prelude, and
/// the assembled source is registered instead.
const ASSEMBLED: &[(&str, &str)] = &[
    ("SHADER_HDR10_TO_LINEAR_BODY", "SHADER_HDR10_TO_LINEAR"),
    ("SHADER_HDR16_TO_LINEAR_BODY", "SHADER_HDR16_TO_LINEAR"),
    ("SHADER_SCENE_TO_DISPLAY_BODY", "SHADER_SCENE_TO_DISPLAY"),
    ("SHADER_NV12_BODY", "SHADER_NV12"),
    ("SHADER_P010_BODY", "SHADER_P010"),
];

fn crates_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(crates_dir().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The names of `enum`'s variants in `src`: the identifiers that open a
/// line at one level of indent inside its braces.
fn variants(src: &str, header: &str) -> Vec<String> {
    let start = src.find(header).unwrap_or_else(|| panic!("no `{header}`"));
    let body = &src[start..];
    let open = body.find('{').unwrap();
    let mut depth = 0;
    let mut end = open;
    for (i, c) in body[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    body[open + 1..end]
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("    ")?;
            let first = rest.chars().next()?;
            if !first.is_ascii_uppercase() {
                return None;
            }
            Some(rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect())
        })
        .collect()
}

fn check_cover(what: &str, cover: Cover, failures: &mut Vec<String>) {
    match cover {
        Case(label) => {
            if !source(EFFECT_PARITY).contains(&format!("(\"{label}\",")) {
                failures.push(format!("{what}: no case \"{label}\" in {EFFECT_PARITY}"));
            }
        }
        Test(file, name) => {
            let src = source(file);
            if !src.contains(&format!("fn {name}(")) {
                failures.push(format!("{what}: no fn {name} in {file}"));
            } else if !src.contains(&format!("#[test]\nfn {name}(")) && !src.contains(&format!("#[test]\r\nfn {name}(")) {
                failures.push(format!("{what}: {file}::{name} is not a #[test]"));
            }
        }
        Exempt(why) => {
            if why.split_whitespace().count() < 8 {
                failures.push(format!("{what}: an exemption has to say why, in a sentence"));
            }
        }
    }
}

/// Every effect variant has an entry, and no entry names a variant that
/// is gone.
#[test]
fn every_effect_has_a_parity_check_or_a_reason() {
    let declared: BTreeSet<String> = variants(&source("klipp-effects/src/lib.rs"), "pub enum BuiltinEffect").into_iter().collect();
    assert!(declared.len() > 40, "the variant scan found only {declared:?}");
    let mapped: BTreeSet<String> = EFFECTS.iter().map(|(n, _)| n.to_string()).collect();
    let missing: Vec<_> = declared.difference(&mapped).collect();
    let stale: Vec<_> = mapped.difference(&declared).collect();
    assert!(missing.is_empty(), "effects with no parity entry in parity_coverage.rs (add a case to cpu_gpu_effect_parity.rs, or an exemption): {missing:?}");
    assert!(stale.is_empty(), "entries for effects that no longer exist: {stale:?}");
    let mut failures = Vec::new();
    for (name, cover) in EFFECTS {
        check_cover(name, *cover, &mut failures);
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Every registered shader has an entry, and no entry names one that is
/// gone.
#[test]
fn every_shader_has_a_parity_check_or_a_reason() {
    let registered: BTreeSet<String> = klipp_effects::gpu::shader_sources().into_iter().map(|(n, _)| n.to_string()).collect();
    let mapped: BTreeSet<String> = SHADERS.iter().map(|(n, _)| n.to_string()).collect();
    let missing: Vec<_> = registered.difference(&mapped).collect();
    let stale: Vec<_> = mapped.difference(&registered).collect();
    assert!(missing.is_empty(), "shaders with no parity entry in parity_coverage.rs: {missing:?}");
    assert!(stale.is_empty(), "entries for shaders that are not registered: {stale:?}");
    let mut failures = Vec::new();
    for (name, cover) in SHADERS {
        check_cover(name, *cover, &mut failures);
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Every shader constant in the crate is registered, under its own name
/// or assembled, so the WGSL validation and the pack lint see it too.
/// Eleven were not, until this test.
#[test]
fn every_shader_constant_is_registered() {
    let registered: BTreeSet<String> = klipp_effects::gpu::shader_sources().into_iter().map(|(n, _)| n.to_string()).collect();
    let mut unregistered = Vec::new();
    // gpu.rs and every module under gpu/, where the shaders' constants
    // move by area (#2890), and the decoders' converters.
    let mut files: Vec<String> = ["klipp-effects/src/gpu.rs", "klipp-effects/src/nv12_gpu.rs", "klipp-effects/src/p010_gpu.rs"].map(String::from).to_vec();
    for e in std::fs::read_dir(crates_dir().join("klipp-effects/src/gpu")).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".rs") {
            files.push(format!("klipp-effects/src/gpu/{name}"));
        }
    }
    files.sort();
    assert!(files.iter().any(|f| f.ends_with("gpu/transitions.rs")), "the area modules are scanned: {files:?}");
    for file in &files {
        let file = file.as_str();
        for line in source(file).lines() {
            // Whatever its visibility: `pub const`, `pub(crate) const` and
            // the rest count too.
            let decl = line.trim_start();
            let decl = match decl.strip_prefix("pub") {
                Some(after) => after.trim_start().strip_prefix('(').and_then(|r| r.split_once(')')).map_or(after, |(_, r)| r).trim_start(),
                None => decl,
            };
            let Some(rest) = decl.strip_prefix("const SHADER_") else { continue };
            let name = format!("SHADER_{}", rest.split(':').next().unwrap().trim());
            let listed = registered.contains(&name) || ASSEMBLED.iter().any(|(body, whole)| *body == name && registered.contains(*whole));
            if !listed {
                unregistered.push(format!("{file}: {name}"));
            }
        }
    }
    assert!(unregistered.is_empty(), "shader constants missing from gpu::shader_sources(): {unregistered:#?}");
}
