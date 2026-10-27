//! Which node types are mixable, and the mask input they declare (#1089,
//! M2). Declared only: nothing executes a mask or a mix yet.

use klipp_effects::registry::{effect_types, PortType, MASK_CHANNEL, MIXABLE_IN};

/// The single-picture effects whose result can be masked and mixed over
/// their input: per-pixel and neighbourhood operations. Not: geometry,
/// what draws outside the picture, the head, temporal and plug-in
/// effects, those with their own mix (invert, emboss, edge detect), and
/// the keys (chroma and luma), which are still open. By saved tag (HSV adjust is saved
/// as `h_s_v_adjust`).
const MIXABLE: [&str; 38] = [
    "color_fade", "posterize", "channel_mixer", "gamma", "h_s_v_adjust", "vibrance", "levels", "equalize",
    "film_grain", "old_film", "vhs", "noise_generator",
    "gaussian_blur", "focus_blur", "sharpen", "directional_blur", "radial_blur", "tilt_shift", "denoise",
    "median_filter", "dilate", "erode", "mosaic",
    "bloom", "glow", "light_rays", "vignette", "diffusion", "lens_flare", "light_wrap", "anamorphic_streak",
    "film_halation", "chromatic_aberration", "lut_3d", "colour_space_transform", "flicker", "binocular_vignette",
    "displace",
];

#[test]
fn exactly_the_single_picture_effects_are_mixable() {
    let mut mixable: Vec<_> = effect_types().filter(|t| t.desc.mixable).map(|t| t.tag).collect();
    mixable.sort_unstable();
    let mut expected = MIXABLE.to_vec();
    expected.sort_unstable();
    assert_eq!(mixable, expected);
    for tag in ["chroma_key", "luma_key", "invert", "transform", "opacity", "drop_shadow", "speed_ramp", "frei0r"] {
        assert!(!effect_types().any(|t| t.tag == tag && t.desc.mixable), "{tag} is not mixable");
    }
}

#[test]
fn a_mixable_type_takes_an_optional_mask_and_says_which_channel_is_the_mask() {
    assert_eq!(MIXABLE_IN.len(), 2);
    let (input, mask) = (MIXABLE_IN[0], MIXABLE_IN[1]);
    assert_eq!((input.key, input.ty, input.optional), ("in", PortType::Image, false));
    assert_eq!((mask.key, mask.ty, mask.optional), ("mask", PortType::Matte, true));
    assert_eq!(mask.params, &[MASK_CHANNEL]);
    assert_eq!(MASK_CHANNEL.options, &["red", "luminance", "alpha"]);
    assert_eq!((MASK_CHANNEL.default, MASK_CHANNEL.migrated), ("luminance", "red"));
    for t in effect_types() {
        let has_mask = t.desc.inputs.iter().any(|p| p.key == "mask");
        // A merge takes a mask limiting where it lays its picture (#2758),
        // its second port being that picture: not mixable, masked.
        assert_eq!(has_mask, t.desc.mixable || t.tag == "merge", "{}: a mask input exactly when mixable, or a merge", t.tag);
        if t.desc.mixable {
            assert_eq!(t.desc.inputs, MIXABLE_IN, "{}", t.tag);
        }
    }
    let merge = effect_types().find(|t| t.tag == "merge").expect("a merge");
    assert_eq!(merge.desc.inputs.iter().map(|p| p.key).collect::<Vec<_>>(), ["in", "fg", "mask"]);
    assert_eq!(merge.desc.inputs[2].params, &[MASK_CHANNEL]);
}
