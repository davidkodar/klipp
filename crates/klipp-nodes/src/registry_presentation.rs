//! How the inspector shows a grade node's parameters (M3 steps 0a, 0c),
//! with the effects' schema ([`klipp_effects::registry::presentation`]):
//! this catalog's table, by `(type id, key)`, transcribed from today's
//! node inspector (`klipp-ui` `panels/node_inspector.rs`; recorded in
//! `klipp-ui/tests/fixtures/old_rows.json`). A parameter the table names
//! nothing for is shown as its description implies
//! ([`P::derived`]).
//!
//! Each type keeps the page it's drawn on today: the Corrector its tabbed
//! page, every other kind the per-node layout. A grade slider drags by a
//! 800th of its shown range. A wheel's parts (its x, y and master, and
//! its saturation and brightness where the type has them) share a
//! [`Widget::WheelPart`] group; the Colour Warper's 24 values share one
//! editor and group. Rows a switch turns on (a qualifier's, a key's)
//! show only while it's on, and the log wheels only in log mode, as
//! today.

use crate::registry::NodeType;
use klipp_color::grade_model::{decode, encode, Encoding, MAX_CONTRAST_PIVOT, MIN_CONTRAST_PIVOT};
use klipp_effects::registry::presentation::{Cond, DisplayMap, Gradient, ParamEntry, Presentation as P, Suffix, TypeEntry, TypePresentation, Widget};
use klipp_effects::registry::ParamDesc;

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

/// A grade slider dragging by `step` (an 800th of its shown range), to
/// two decimals.
const fn slider(step: f32) -> P {
    P::scrub(step, 2).widget(Widget::Slider)
}

const fn toggle() -> P {
    P::shown_as(Widget::Toggle)
}

const fn combo() -> P {
    P::shown_as(Widget::Combo)
}

const fn editor(id: &'static str) -> P {
    P::shown_as(Widget::Custom(id))
}

const fn wheel(group: &'static str) -> P {
    P::scrub(0.0025, 2).widget(Widget::WheelPart(group))
}

const fn on(key: &'static str) -> Cond {
    Cond::ParamIs { key, value: 1.0 }
}

const fn off(key: &'static str) -> Cond {
    Cond::ParamIs { key, value: 0.0 }
}

/// A qualifier row of mode `index` (#895): 0 HSL, 1 RGB, 2 3D.
const fn mode_is(index: u8) -> Cond {
    Cond::ParamIs { key: "qual_mode", value: index as f32 }
}

/// The contrast pivot is stored linear and shown in the grade encoding.
fn pivot_shown(linear: f32, enc: Encoding) -> f32 {
    encode(linear.clamp(MIN_CONTRAST_PIVOT, MAX_CONTRAST_PIVOT), enc)
}

fn pivot_stored(shown: f32, enc: Encoding) -> f32 {
    decode(shown, enc).clamp(MIN_CONTRAST_PIVOT, MAX_CONTRAST_PIVOT)
}

const PIVOT: DisplayMap = DisplayMap { to_shown: pivot_shown, to_stored: pivot_stored };

const CORRECTOR: &str = "klipp.grade.corrector";
const WHEELS: &str = "klipp.grade.color_wheels";
const SAT_BRI: &str = "klipp.grade.wheel_sat_bri";
const QUALIFIER: &str = "klipp.grade.qualifier";
const LUMA_KEY: &str = "klipp.grade.luma_key";
const SOFT_CLIP: &str = "klipp.grade.soft_clip";
const HDR_ZONES: &str = "klipp.grade.hdr_zones";
const DIFF_KEY: &str = "klipp.grade.difference_key";
const GRAIN: &str = "klipp.grade.film_grain";
const LUT: &str = "klipp.grade.lut_3d";
const COLOR_MATCH: &str = "klipp.grade.color_match";

/// A measured value, to two decimals: shown, written by its node's tool.
const fn readout() -> P {
    P::scrub(0.0, 2).widget(Widget::Readout)
}

/// The wheel rows of a type that has them: the log switch, the luma mix,
/// the log ranges, then the four primary wheels or the three log ones.
macro_rules! wheels {
    ($t:expr) => {
        [
            at($t, "wheels_log_mode", combo().label("Linear / Log (HDR)").labels(&["Linear", "Log (HDR)"]).order(10)),
            at($t, "luma_mix", slider(0.00125).label("Luma Mix").order(11)),
            at($t, "log_low_range", slider(0.000_511_796_4).label("Low Range").order(12).visible_if(on("wheels_log_mode"))),
            at($t, "log_high_range", slider(0.000_788_203_7).label("High Range").order(13).visible_if(on("wheels_log_mode"))),
            at($t, "lift_x", wheel("Lift").order(20).visible_if(off("wheels_log_mode"))),
            at($t, "lift_y", wheel("Lift").order(20).visible_if(off("wheels_log_mode"))),
            at($t, "lift_master", wheel("Lift").order(20).visible_if(off("wheels_log_mode"))),
            at($t, "gamma_x", wheel("Gamma").order(21).visible_if(off("wheels_log_mode"))),
            at($t, "gamma_y", wheel("Gamma").order(21).visible_if(off("wheels_log_mode"))),
            at($t, "gamma_master", wheel("Gamma").order(21).visible_if(off("wheels_log_mode"))),
            at($t, "gain_x", wheel("Gain").order(22).visible_if(off("wheels_log_mode"))),
            at($t, "gain_y", wheel("Gain").order(22).visible_if(off("wheels_log_mode"))),
            at($t, "gain_master", wheel("Gain").order(22).visible_if(off("wheels_log_mode"))),
            at($t, "offset_x", wheel("Offset").order(23).visible_if(off("wheels_log_mode"))),
            at($t, "offset_y", wheel("Offset").order(23).visible_if(off("wheels_log_mode"))),
            at($t, "offset_master", wheel("Offset").order(23).visible_if(off("wheels_log_mode"))),
            at($t, "log_low_x", wheel("Low").order(24).visible_if(on("wheels_log_mode"))),
            at($t, "log_low_y", wheel("Low").order(24).visible_if(on("wheels_log_mode"))),
            at($t, "log_low_master", wheel("Low").order(24).visible_if(on("wheels_log_mode"))),
            at($t, "log_mid_x", wheel("Mid").order(25).visible_if(on("wheels_log_mode"))),
            at($t, "log_mid_y", wheel("Mid").order(25).visible_if(on("wheels_log_mode"))),
            at($t, "log_mid_master", wheel("Mid").order(25).visible_if(on("wheels_log_mode"))),
            at($t, "log_high_x", wheel("High").order(26).visible_if(on("wheels_log_mode"))),
            at($t, "log_high_y", wheel("High").order(26).visible_if(on("wheels_log_mode"))),
            at($t, "log_high_master", wheel("High").order(26).visible_if(on("wheels_log_mode"))),
        ]
    };
}

/// The Colour Warper's 24 values, drawn by its one editor.
macro_rules! warper {
    ($t:expr, $order:expr) => {{
        const W: P = editor("colour_warper").group("Colour Warper").order($order);
        [
            at($t, "warper_hue_0", W), at($t, "warper_hue_1", W), at($t, "warper_hue_2", W), at($t, "warper_hue_3", W),
            at($t, "warper_hue_4", W), at($t, "warper_hue_5", W), at($t, "warper_hue_6", W), at($t, "warper_hue_7", W),
            at($t, "warper_hue_8", W), at($t, "warper_hue_9", W), at($t, "warper_hue_10", W), at($t, "warper_hue_11", W),
            at($t, "warper_sat_0", W), at($t, "warper_sat_1", W), at($t, "warper_sat_2", W), at($t, "warper_sat_3", W),
            at($t, "warper_sat_4", W), at($t, "warper_sat_5", W), at($t, "warper_sat_6", W), at($t, "warper_sat_7", W),
            at($t, "warper_sat_8", W), at($t, "warper_sat_9", W), at($t, "warper_sat_10", W), at($t, "warper_sat_11", W),
        ]
    }};
}

const CORRECTOR_WHEELS: [ParamEntry; 25] = wheels!(CORRECTOR);
const NODE_WHEELS: [ParamEntry; 25] = wheels!(WHEELS);
const CORRECTOR_WARPER: [ParamEntry; 24] = warper!(CORRECTOR, 60);
const NODE_WARPER: [ParamEntry; 24] = warper!("klipp.grade.color_warper", 0);

/// The Corrector's tabbed page: Adjustments, Wheels, Curves, Hue/Sat,
/// Warper.
const CORRECTOR_PAGE: [ParamEntry; 27] = [
    at(CORRECTOR, "exposure", slider(0.0075).suffix(Suffix::Text("stops")).order(0)),
    at(CORRECTOR, "contrast", slider(0.0025).order(1)),
    at(CORRECTOR, "contrast_pivot", slider(0.001_066_525).display(PIVOT).order(2)),
    at(CORRECTOR, "clarity", slider(0.0025).order(3)),
    at(CORRECTOR, "saturation", slider(0.0025).suffix(Suffix::Text("×")).order(4)),
    at(CORRECTOR, "vibrance", slider(0.0025).order(5)),
    at(CORRECTOR, "highlights", slider(0.0025).order(6)),
    at(CORRECTOR, "shadows", slider(0.0025).order(7)),
    at(CORRECTOR, "whites", slider(0.0025).order(8)),
    at(CORRECTOR, "blacks", slider(0.0025).order(9)),
    at(CORRECTOR, "temperature", slider(0.125).widget(Widget::Gradient(Gradient::Temperature)).order(30)),
    at(CORRECTOR, "tint", slider(0.125).widget(Widget::Gradient(Gradient::Tint)).order(31)),
    at(CORRECTOR, "hue", slider(0.45).widget(Widget::HueRainbow).suffix(Suffix::None).order(32)),
    at(CORRECTOR, "global_mix", slider(0.00125).label("Global Mix").order(33)),
    at(CORRECTOR, "lift_sat", wheel("Lift").label("S").order(20).visible_if(off("wheels_log_mode"))),
    at(CORRECTOR, "lift_bri", wheel("Lift").label("B").order(20).visible_if(off("wheels_log_mode"))),
    at(CORRECTOR, "gamma_sat", wheel("Gamma").label("S").order(21).visible_if(off("wheels_log_mode"))),
    at(CORRECTOR, "gamma_bri", wheel("Gamma").label("B").order(21).visible_if(off("wheels_log_mode"))),
    at(CORRECTOR, "gain_sat", wheel("Gain").label("S").order(22).visible_if(off("wheels_log_mode"))),
    at(CORRECTOR, "gain_bri", wheel("Gain").label("B").order(22).visible_if(off("wheels_log_mode"))),
    at(CORRECTOR, "offset_sat", wheel("Offset").label("S").order(23).visible_if(off("wheels_log_mode"))),
    at(CORRECTOR, "offset_bri", wheel("Offset").label("B").order(23).visible_if(off("wheels_log_mode"))),
    at(CORRECTOR, "curves", editor("curves").order(40)),
    at(CORRECTOR, "curves_preserve_luma", toggle().label("Preserve Luma").order(41)),
    at(CORRECTOR, "curves_mix", slider(0.00125).order(42)),
    at(CORRECTOR, "huesat", editor("huesat_curves").order(50)),
    at(CORRECTOR, "huesat_mix", slider(0.00125).order(51)),
];

/// The per-node layout of every other kind.
const NODES: &[ParamEntry] = &[
    at("klipp.grade.input_transform", "log_format", editor("camera_encoding").label("Input Colour Space")),
    at("klipp.grade.exposure", "exposure", slider(0.0075).suffix(Suffix::Text("stops"))),
    at("klipp.grade.contrast", "contrast", slider(0.0025)),
    at("klipp.grade.contrast", "contrast_pivot", slider(0.001_066_525).display(PIVOT)),
    at("klipp.grade.contrast", "clarity", slider(0.0025)),
    at("klipp.grade.saturation", "saturation", slider(0.0025)),
    at("klipp.grade.saturation", "vibrance", slider(0.0025)),
    at("klipp.grade.tone_controls", "highlights", slider(0.0025)),
    at("klipp.grade.tone_controls", "shadows", slider(0.0025)),
    at("klipp.grade.tone_controls", "whites", slider(0.0025)),
    at("klipp.grade.tone_controls", "blacks", slider(0.0025)),
    at(SAT_BRI, "lift_sat", slider(0.0025).label("Saturation").group("Lift")),
    at(SAT_BRI, "lift_bri", slider(0.0025).label("Brightness").group("Lift")),
    at(SAT_BRI, "gamma_sat", slider(0.0025).label("Saturation").group("Gamma")),
    at(SAT_BRI, "gamma_bri", slider(0.0025).label("Brightness").group("Gamma")),
    at(SAT_BRI, "gain_sat", slider(0.0025).label("Saturation").group("Gain")),
    at(SAT_BRI, "gain_bri", slider(0.0025).label("Brightness").group("Gain")),
    at(SAT_BRI, "offset_sat", slider(0.0025).label("Saturation").group("Offset")),
    at(SAT_BRI, "offset_bri", slider(0.0025).label("Brightness").group("Offset")),
    at("klipp.grade.rgb_curves", "curves", editor("curves")),
    at("klipp.grade.rgb_curves", "curves_preserve_luma", toggle().label("Preserve Luma")),
    at("klipp.grade.rgb_curves", "curves_mix", slider(0.00125)),
    at("klipp.grade.hue_sat_curves", "huesat", editor("huesat_curves")),
    at("klipp.grade.hue_sat_curves", "huesat_mix", slider(0.00125)),
    at("klipp.grade.color_balance", "temperature", slider(0.125)),
    at("klipp.grade.color_balance", "tint", slider(0.125)),
    at("klipp.grade.color_balance", "hue", slider(0.45).suffix(Suffix::None)),
    at("klipp.grade.color_balance", "global_mix", slider(0.00125).label("Global Mix")),
    at(QUALIFIER, "qual_enabled", toggle()),
    // The key's mode (#895): HSL's rows or RGB's.
    at(QUALIFIER, "qual_mode", combo().label("Mode")),
    at(QUALIFIER, "qual_hue_center", slider(0.45).label("Hue Center").suffix(Suffix::Text("deg")).visible_if(mode_is(0))),
    at(QUALIFIER, "qual_hue_width", slider(0.225).label("Hue Width").suffix(Suffix::Text("deg")).visible_if(mode_is(0))),
    at(QUALIFIER, "qual_hue_soft", slider(0.1125).label("Hue Soft").suffix(Suffix::Text("deg")).visible_if(mode_is(0))),
    at(QUALIFIER, "qual_sat_low", slider(0.00125).label("Sat Low").visible_if(mode_is(0))),
    at(QUALIFIER, "qual_sat_high", slider(0.00125).label("Sat High").visible_if(mode_is(0))),
    at(QUALIFIER, "qual_sat_soft", slider(0.00125).label("Sat Soft").visible_if(mode_is(0))),
    at(QUALIFIER, "qual_lum_low", slider(0.00125).label("Lum Low").visible_if(mode_is(0))),
    at(QUALIFIER, "qual_lum_high", slider(0.00125).label("Lum High").visible_if(mode_is(0))),
    at(QUALIFIER, "qual_lum_soft", slider(0.00125).label("Lum Soft").visible_if(mode_is(0))),
    at(QUALIFIER, "qual_red_low", slider(0.00125).label("Red Low").visible_if(mode_is(1))),
    at(QUALIFIER, "qual_red_high", slider(0.00125).label("Red High").visible_if(mode_is(1))),
    at(QUALIFIER, "qual_green_low", slider(0.00125).label("Green Low").visible_if(mode_is(1))),
    at(QUALIFIER, "qual_green_high", slider(0.00125).label("Green High").visible_if(mode_is(1))),
    at(QUALIFIER, "qual_blue_low", slider(0.00125).label("Blue Low").visible_if(mode_is(1))),
    at(QUALIFIER, "qual_blue_high", slider(0.00125).label("Blue High").visible_if(mode_is(1))),
    at(QUALIFIER, "qual_rgb_soft", slider(0.00125).label("RGB Soft").visible_if(mode_is(1))),
    // The 3D mode's reach (#895): how far round its strokes it keys.
    at(QUALIFIER, "qual_key_radius", slider(0.01).label("Reach").visible_if(mode_is(2))),
    at(QUALIFIER, "qual_key_soft", slider(0.01).label("Softness").visible_if(mode_is(2))),
    at(QUALIFIER, "qual_invert", toggle().visible_if(on("qual_enabled"))),
    at(QUALIFIER, "qual_clean_black", slider(0.00125).label("Clean Black").visible_if(on("qual_enabled"))),
    at(QUALIFIER, "qual_clean_white", slider(0.00125).label("Clean White").visible_if(on("qual_enabled"))),
    at(QUALIFIER, "qual_denoise", slider(0.025).label("Denoise").visible_if(on("qual_enabled"))),
    at(QUALIFIER, "qual_shrink_grow", slider(0.05).label("Shrink / Grow").visible_if(on("qual_enabled"))),
    at(QUALIFIER, "qual_blur", slider(0.0625).visible_if(on("qual_enabled"))),
    at(LUMA_KEY, "luma_key_enabled", toggle()),
    at(LUMA_KEY, "luma_key_low", slider(0.00125).visible_if(on("luma_key_enabled"))),
    at(LUMA_KEY, "luma_key_high", slider(0.00125).visible_if(on("luma_key_enabled"))),
    at(LUMA_KEY, "luma_key_soft", slider(0.000625).label("Soft").visible_if(on("luma_key_enabled"))),
    at(LUMA_KEY, "luma_key_invert", toggle().visible_if(on("luma_key_enabled"))),
    at(SOFT_CLIP, "soft_clip.enabled", toggle()),
    // Colour Match's correction is what Apply match measured, shown beside
    // its Capture reference, Apply match and Reset: a hand edit would be
    // replaced by the next match, and a correction by hand is the
    // Corrector's (#1421).
    at(COLOR_MATCH, "color_match.exposure", readout()),
    at(COLOR_MATCH, "color_match.temperature", readout()),
    at(COLOR_MATCH, "color_match.tint", readout()),
    at(COLOR_MATCH, "color_match.saturation", readout()),
    at(SOFT_CLIP, "soft_clip.threshold", slider(0.00125).visible_if(on("soft_clip.enabled"))),
    at(SOFT_CLIP, "soft_clip.knee", slider(0.000625).visible_if(on("soft_clip.enabled"))),
    // HDR zones (#240): the picked zone's wheel (its puck the colour, its
    // Y the exposure), then its rows, one zone at a time from the zone
    // menu, so each row is labelled as every zone's is.
    at(HDR_ZONES, "hdr_global_exposure", wheel("Global")),
    at(HDR_ZONES, "hdr_global_colour_x", wheel("Global")),
    at(HDR_ZONES, "hdr_global_colour_y", wheel("Global")),
    at(HDR_ZONES, "hdr_global_saturation", slider(0.0025).label("Saturation").suffix(Suffix::Text("×"))),
    at(HDR_ZONES, "hdr_black_exposure", wheel("Black")),
    at(HDR_ZONES, "hdr_black_colour_x", wheel("Black")),
    at(HDR_ZONES, "hdr_black_colour_y", wheel("Black")),
    at(HDR_ZONES, "hdr_black_saturation", slider(0.0025).label("Saturation").suffix(Suffix::Text("×"))),
    at(HDR_ZONES, "hdr_black_boundary", slider(0.01).label("Boundary").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_black_falloff", slider(0.01).label("Falloff").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_dark_exposure", wheel("Dark")),
    at(HDR_ZONES, "hdr_dark_colour_x", wheel("Dark")),
    at(HDR_ZONES, "hdr_dark_colour_y", wheel("Dark")),
    at(HDR_ZONES, "hdr_dark_saturation", slider(0.0025).label("Saturation").suffix(Suffix::Text("×"))),
    at(HDR_ZONES, "hdr_dark_boundary", slider(0.01).label("Boundary").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_dark_falloff", slider(0.01).label("Falloff").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_shadow_exposure", wheel("Shadow")),
    at(HDR_ZONES, "hdr_shadow_colour_x", wheel("Shadow")),
    at(HDR_ZONES, "hdr_shadow_colour_y", wheel("Shadow")),
    at(HDR_ZONES, "hdr_shadow_saturation", slider(0.0025).label("Saturation").suffix(Suffix::Text("×"))),
    at(HDR_ZONES, "hdr_shadow_boundary", slider(0.01).label("Boundary").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_shadow_falloff", slider(0.01).label("Falloff").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_light_exposure", wheel("Light")),
    at(HDR_ZONES, "hdr_light_colour_x", wheel("Light")),
    at(HDR_ZONES, "hdr_light_colour_y", wheel("Light")),
    at(HDR_ZONES, "hdr_light_saturation", slider(0.0025).label("Saturation").suffix(Suffix::Text("×"))),
    at(HDR_ZONES, "hdr_light_boundary", slider(0.01).label("Boundary").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_light_falloff", slider(0.01).label("Falloff").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_highlight_exposure", wheel("Highlight")),
    at(HDR_ZONES, "hdr_highlight_colour_x", wheel("Highlight")),
    at(HDR_ZONES, "hdr_highlight_colour_y", wheel("Highlight")),
    at(HDR_ZONES, "hdr_highlight_saturation", slider(0.0025).label("Saturation").suffix(Suffix::Text("×"))),
    at(HDR_ZONES, "hdr_highlight_boundary", slider(0.01).label("Boundary").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_highlight_falloff", slider(0.01).label("Falloff").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_specular_exposure", wheel("Specular")),
    at(HDR_ZONES, "hdr_specular_colour_x", wheel("Specular")),
    at(HDR_ZONES, "hdr_specular_colour_y", wheel("Specular")),
    at(HDR_ZONES, "hdr_specular_saturation", slider(0.0025).label("Saturation").suffix(Suffix::Text("×"))),
    at(HDR_ZONES, "hdr_specular_boundary", slider(0.01).label("Boundary").suffix(Suffix::Text("stops"))),
    at(HDR_ZONES, "hdr_specular_falloff", slider(0.01).label("Falloff").suffix(Suffix::Text("stops"))),
    at("klipp.grade.mask", "masks", editor("mask_stack").label("Masks")),
    at("klipp.grade.combine_mattes", "matte_combine_op", combo().label("Combine")),
    at("klipp.grade.external_matte", "matte_image_path", editor("matte_file")),
    at("klipp.grade.external_matte", "matte_channel", combo().label("Source")),
    at("klipp.grade.external_matte", "matte_invert", toggle()),
    at("klipp.grade.external_matte", "matte_subject", combo().label("Tracked subject")),
    at(DIFF_KEY, "difference_key.enabled", toggle().order(0)),
    at(DIFF_KEY, "difference_key.plate_path", editor("plate_file").label("Plate").order(1).visible_if(on("difference_key.enabled"))),
    at(DIFF_KEY, "difference_key.match_space", combo().label("Match").order(2).visible_if(on("difference_key.enabled"))),
    at(DIFF_KEY, "difference_key.tolerance", slider(0.00125).order(3).visible_if(on("difference_key.enabled"))),
    at(DIFF_KEY, "difference_key.soft", slider(0.000625).label("Soft").order(4).visible_if(on("difference_key.enabled"))),
    at(DIFF_KEY, "difference_key.invert", toggle().order(5).visible_if(on("difference_key.enabled"))),
    at("klipp.grade.blend", "mix", slider(0.00125)),
    at("klipp.grade.apply_matte", "mix", slider(0.00125)),
    at("klipp.grade.key_mixer", "key_mixer", editor("key_mixer_inputs")),
    at("klipp.grade.key_mixer", "key_mixer_output_gain", slider(0.0025)),
    at("klipp.grade.key_mixer", "key_mixer_output_offset", slider(0.0025)),
    // A gate taking its key inverted (#898): set by Add Outside Node, over
    // MCP and by undo; no row of its own yet (the tabbed page edits the
    // first Corrector, not an outside one).
    at("klipp.grade.corrector", "key_invert", P::shown_as(Widget::Hidden)),
    at("klipp.grade.color_balance", "key_invert", P::shown_as(Widget::Hidden)),
    at("klipp.grade.exposure", "key_invert", P::shown_as(Widget::Hidden)),
    at("klipp.grade.contrast", "key_invert", P::shown_as(Widget::Hidden)),
    at("klipp.grade.saturation", "key_invert", P::shown_as(Widget::Hidden)),
    at("klipp.grade.color_wheels", "key_invert", P::shown_as(Widget::Hidden)),
    at("klipp.grade.tone_controls", "key_invert", P::shown_as(Widget::Hidden)),
    at("klipp.grade.rgb_curves", "key_invert", P::shown_as(Widget::Hidden)),
    at("klipp.grade.layer_mixer", "layer_mixer", editor("layer_mixer_inputs")),
    at(LUT, "lut_path", editor("lut_file").label("LUT")),
    at(LUT, "lut_opacity", slider(0.00125)),
    at(LUT, "lut_input_space", combo()),
    at(LUT, "lut_output_space", combo()),
    at(GRAIN, "grain.enabled", toggle()),
    at(GRAIN, "grain.amount", slider(0.00125).visible_if(on("grain.enabled"))),
    at(GRAIN, "grain.size", slider(0.003125).suffix(Suffix::None).visible_if(on("grain.enabled"))),
    at(GRAIN, "grain.shadows", slider(0.00125).visible_if(on("grain.enabled"))),
    at(GRAIN, "grain.highlights", slider(0.00125).visible_if(on("grain.enabled"))),
    at(GRAIN, "grain.monochrome", toggle().visible_if(on("grain.enabled"))),
    at("klipp.grade.denoise", "denoise_luma", slider(0.00125).label("Luma Strength")),
    at("klipp.grade.denoise", "denoise_chroma", slider(0.00125).label("Chroma Strength")),
    at("klipp.grade.denoise", "denoise_radius", slider(0.005)),
    at("klipp.grade.compound", "compound_subgraph", editor("compound_body")),
];

/// The grade nodes' entries.
pub static PARAMS: &[&[ParamEntry]] = &[&CORRECTOR_PAGE, &CORRECTOR_WHEELS, &CORRECTOR_WARPER, &NODE_WHEELS, &NODE_WARPER, NODES];
pub static TYPES: &[TypeEntry] = &[];

/// Every grade entry.
pub fn params() -> impl Iterator<Item = &'static ParamEntry> {
    PARAMS.iter().flat_map(|f| f.iter())
}

impl NodeType {
    /// How parameter `p` of this node type is shown.
    pub fn presentation(&self, p: &ParamDesc) -> P {
        P::find(params(), self.desc.id, p)
    }

    /// How this node type is shown, beyond its rows.
    pub fn type_presentation(&self) -> TypePresentation {
        TypePresentation::of(TYPES, self.desc.id)
    }
}
