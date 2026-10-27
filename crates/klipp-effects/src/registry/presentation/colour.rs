//! The colour family's rows (M3 step 0b), as the effects tab draws them
//! today (`klipp-ui` `inspector/effects_tab.rs`; recorded in
//! `klipp-ui/tests/fixtures/old_rows.json`). Today's labels are kept where
//! they differ from the catalog's.

use super::{ParamEntry, Presentation as P, TypeEntry, TypePresentation, Widget};

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

const OPACITY: &str = "klipp.fx.opacity";
const COLOR_FADE: &str = "klipp.fx.color_fade";
const MIXER: &str = "klipp.fx.channel_mixer";
const HSV: &str = "klipp.fx.hsv_adjust";
const LEVELS: &str = "klipp.fx.levels";
const LUT: &str = "klipp.fx.lut_3d";
const CST: &str = "klipp.fx.colour_space_transform";
const GRADIENT_MAP: &str = "klipp.fx.gradient_map";

pub(super) static PARAMS: &[ParamEntry] = &[
    // Its ramp's three colours and how much of it shows (#2758).
    at(GRADIENT_MAP, "shadow", P::shown_as(Widget::ColourRgb)),
    at(GRADIENT_MAP, "mid", P::shown_as(Widget::ColourRgb)),
    at(GRADIENT_MAP, "highlight", P::shown_as(Widget::ColourRgb)),
    at(GRADIENT_MAP, "mix", P::scrub(0.005, 2)),
    at(OPACITY, "value", P::scrub(0.005, 2)),
    // Mode first, as today, then the colour and the duration.
    at(COLOR_FADE, "mode", P::shown_as(Widget::Combo).labels(&["Fade From", "Fade To"]).order(0)),
    at(COLOR_FADE, "color", P::shown_as(Widget::ColourRgba).order(1)),
    at(COLOR_FADE, "duration_secs", P::scrub(0.05, 2).order(2)),
    at(COLOR_FADE, "progress", P::hidden()),
    at("klipp.fx.invert", "mix", P::scrub(0.005, 2)),
    at("klipp.fx.posterize", "levels", P::scrub(1.0, 0)),
    at(MIXER, "red_red", P::scrub(0.01, 2).label("Red").group("Red Output")),
    at(MIXER, "red_green", P::scrub(0.01, 2).label("Green").group("Red Output")),
    at(MIXER, "red_blue", P::scrub(0.01, 2).label("Blue").group("Red Output")),
    at(MIXER, "green_red", P::scrub(0.01, 2).label("Red").group("Green Output")),
    at(MIXER, "green_green", P::scrub(0.01, 2).label("Green").group("Green Output")),
    at(MIXER, "green_blue", P::scrub(0.01, 2).label("Blue").group("Green Output")),
    at(MIXER, "blue_red", P::scrub(0.01, 2).label("Red").group("Blue Output")),
    at(MIXER, "blue_green", P::scrub(0.01, 2).label("Green").group("Blue Output")),
    at(MIXER, "blue_blue", P::scrub(0.01, 2).label("Blue").group("Blue Output")),
    at("klipp.fx.gamma", "gamma", P::scrub(0.05, 2)),
    at(HSV, "hue_shift", P::scrub(1.0, 1).label("Hue Shift")),
    at(HSV, "saturation", P::scrub(0.01, 2)),
    at(HSV, "value", P::scrub(0.01, 2)),
    // Resets to the catalog's 0, not today's 0.5 (IC2).
    at("klipp.fx.vibrance", "amount", P::scrub(0.01, 2)),
    at(LEVELS, "in_black", P::scrub(0.005, 3).label("In Black")),
    at(LEVELS, "in_white", P::scrub(0.005, 3).label("In White")),
    at(LEVELS, "gamma", P::scrub(0.05, 2)),
    at(LEVELS, "out_black", P::scrub(0.005, 3).label("Out Black")),
    at(LEVELS, "out_white", P::scrub(0.005, 3).label("Out White")),
    at("klipp.fx.equalize", "strength", P::scrub(0.005, 2)),
    at(LUT, "path", P::shown_as(Widget::Custom("effect_lut_file"))),
    at(LUT, "opacity", P::scrub(0.005, 2)),
    at(LUT, "tetrahedral", P::shown_as(Widget::Toggle)),
    at(LUT, "prepared", P::hidden()),
    at(CST, "input_gamut", P::shown_as(Widget::Combo).group("Input")),
    at(CST, "input_curve", P::shown_as(Widget::Combo).group("Input")),
    at(CST, "output_gamut", P::shown_as(Widget::Combo).group("Output")),
    at(CST, "output_curve", P::shown_as(Widget::Combo).group("Output")),
    at(CST, "prepared", P::hidden()),
];

pub(super) static TYPES: &[TypeEntry] = &[TypeEntry { type_id: COLOR_FADE, pres: TypePresentation { notice: None, extra_ui: Some("color_fade") } }];
