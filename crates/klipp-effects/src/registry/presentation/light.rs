//! The light family's rows (M3 step 0b), as the effects tab draws them
//! today (`klipp-ui` `inspector/effects_tab.rs`; recorded in
//! `klipp-ui/tests/fixtures/old_rows.json`): Outer Glow and Drop Shadow
//! show their colour first, as today.

use super::{ParamEntry, Presentation as P, TypeEntry, Widget};

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

pub(super) static PARAMS: &[ParamEntry] = &[
    at("klipp.fx.bloom", "threshold", P::scrub(0.005, 2)),
    at("klipp.fx.bloom", "radius", P::scrub(0.5, 1)),
    at("klipp.fx.bloom", "intensity", P::scrub(0.01, 2)),
    at("klipp.fx.glow", "radius", P::scrub(0.5, 1)),
    at("klipp.fx.glow", "intensity", P::scrub(0.01, 2)),
    at("klipp.fx.glow", "threshold", P::scrub(0.005, 2)),
    at("klipp.fx.light_rays", "center_x", P::scrub(0.005, 3)),
    at("klipp.fx.light_rays", "center_y", P::scrub(0.005, 3)),
    at("klipp.fx.light_rays", "threshold", P::scrub(0.005, 2)),
    at("klipp.fx.light_rays", "samples", P::scrub(1.0, 0)),
    at("klipp.fx.light_rays", "decay", P::scrub(0.005, 3)),
    at("klipp.fx.light_rays", "intensity", P::scrub(0.01, 2)),
    at("klipp.fx.vignette", "amount", P::scrub(0.005, 2)),
    at("klipp.fx.vignette", "size", P::scrub(0.01, 2)),
    at("klipp.fx.vignette", "softness", P::scrub(0.01, 2)),
    at("klipp.fx.vignette", "roundness", P::scrub(0.005, 2)),
    at("klipp.fx.binocular_vignette", "amount", P::scrub(0.005, 2)),
    at("klipp.fx.binocular_vignette", "spacing", P::scrub(0.005, 2)),
    at("klipp.fx.binocular_vignette", "width", P::scrub(0.005, 2)),
    at("klipp.fx.binocular_vignette", "height", P::scrub(0.005, 2)),
    at("klipp.fx.binocular_vignette", "softness", P::scrub(0.005, 2)),
    at("klipp.fx.binocular_vignette", "fringe", P::scrub(0.1, 1)),
    at("klipp.fx.diffusion", "amount", P::scrub(0.005, 2)),
    at("klipp.fx.diffusion", "radius", P::scrub(0.5, 1)),
    at("klipp.fx.diffusion", "threshold", P::scrub(0.005, 2)),
    at("klipp.fx.diffusion", "desaturation", P::scrub(0.005, 2)),
    at("klipp.fx.lens_flare", "position_x", P::scrub(0.005, 3)),
    at("klipp.fx.lens_flare", "position_y", P::scrub(0.005, 3)),
    at("klipp.fx.lens_flare", "brightness", P::scrub(0.01, 2)),
    at("klipp.fx.lens_flare", "flare_size", P::scrub(0.005, 3)),
    at("klipp.fx.lens_flare", "ghost_count", P::scrub(1.0, 0)),
    at("klipp.fx.lens_flare", "ghost_intensity", P::scrub(0.005, 2)),
    at("klipp.fx.lens_flare", "chromatic_aberration", P::scrub(0.005, 2)),
    at("klipp.fx.outer_glow", "radius", P::scrub(0.001, 3).order(1)),
    at("klipp.fx.outer_glow", "intensity", P::scrub(0.005, 3).order(2)),
    at("klipp.fx.outer_glow", "color", P::shown_as(Widget::ColourRgb).order(0)),
    at("klipp.fx.light_wrap", "width", P::scrub(0.5, 1)),
    at("klipp.fx.light_wrap", "intensity", P::scrub(0.005, 2)),
    at("klipp.fx.light_wrap", "softness", P::scrub(0.01, 2)),
    at("klipp.fx.anamorphic_streak", "threshold", P::scrub(0.005, 2)),
    at("klipp.fx.anamorphic_streak", "length", P::scrub(1.0, 0)),
    at("klipp.fx.anamorphic_streak", "intensity", P::scrub(0.01, 2)),
    at("klipp.fx.anamorphic_streak", "tint", P::scrub(0.005, 2)),
    at("klipp.fx.film_halation", "threshold", P::scrub(0.005, 2)),
    at("klipp.fx.film_halation", "radius", P::scrub(0.5, 1)),
    at("klipp.fx.film_halation", "intensity", P::scrub(0.005, 2)),
    at("klipp.fx.film_halation", "hue", P::scrub(0.005, 2)),
    at("klipp.fx.chromatic_aberration", "red_shift", P::scrub(0.1, 1)),
    at("klipp.fx.chromatic_aberration", "blue_shift", P::scrub(0.1, 1)),
    at("klipp.fx.chromatic_aberration", "center_x", P::scrub(0.005, 3)),
    at("klipp.fx.chromatic_aberration", "center_y", P::scrub(0.005, 3)),
    at("klipp.fx.drop_shadow", "offset_x", P::scrub(0.001, 3).order(1)),
    at("klipp.fx.drop_shadow", "offset_y", P::scrub(0.001, 3).order(2)),
    at("klipp.fx.drop_shadow", "blur", P::scrub(0.001, 3).order(3)),
    at("klipp.fx.drop_shadow", "color", P::shown_as(Widget::ColourRgb).order(0)),
    at("klipp.fx.drop_shadow", "opacity", P::scrub(0.005, 3).order(4)),
];

pub(super) static TYPES: &[TypeEntry] = &[];
