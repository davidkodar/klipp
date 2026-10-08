//! The blur family's rows (M3 step 0b), as the effects tab draws them
//! today (`klipp-ui` `inspector/effects_tab.rs`; recorded in
//! `klipp-ui/tests/fixtures/old_rows.json`). Focus Blur has no rows: a
//! title's roll sets it from its focus band, in the Title tab.

use super::{ParamEntry, Presentation as P, Suffix, TypeEntry, TypePresentation, Widget};

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

const FOCUS: &str = "klipp.fx.focus_blur";
const DIRECTIONAL: &str = "klipp.fx.directional_blur";
const RADIAL: &str = "klipp.fx.radial_blur";
const TILT: &str = "klipp.fx.tilt_shift";
const DENOISE: &str = "klipp.fx.denoise";
const EMBOSS: &str = "klipp.fx.emboss";
const EDGES: &str = "klipp.fx.edge_detect";

pub(super) static PARAMS: &[ParamEntry] = &[
    at("klipp.fx.gaussian_blur", "radius", P::scrub(0.5, 1)),
    at(FOCUS, "radius", P::hidden()),
    at(FOCUS, "vertical", P::hidden()),
    at(FOCUS, "band", P::hidden()),
    // Resets to the catalog's 0, not today's 0.5 (IC2).
    at("klipp.fx.sharpen", "amount", P::scrub(0.01, 2)),
    at(DIRECTIONAL, "angle", P::scrub(1.0, 1)),
    at(DIRECTIONAL, "distance", P::scrub(0.5, 1)),
    at(DIRECTIONAL, "samples", P::scrub(1.0, 0)),
    at(RADIAL, "center_x", P::scrub(0.005, 3)),
    at(RADIAL, "center_y", P::scrub(0.005, 3)),
    at(RADIAL, "amount", P::scrub(0.5, 1)),
    at(RADIAL, "samples", P::scrub(1.0, 0).widget(Widget::Slider).soft_whole((4, 32))),
    at(RADIAL, "mode", P::shown_as(Widget::Combo).labels(&["Zoom", "Spin"])),
    at(TILT, "center", P::scrub(0.005, 2)),
    at(TILT, "width", P::scrub(0.005, 2)),
    at(TILT, "softness", P::scrub(0.005, 2)),
    at(TILT, "radius", P::scrub(0.2, 1)),
    at(DENOISE, "spatial_sigma", P::scrub(0.5, 1).suffix(Suffix::None)),
    at(DENOISE, "color_sigma", P::scrub(0.01, 2)),
    at(DENOISE, "iterations", P::scrub(1.0, 0)),
    at("klipp.fx.median_filter", "radius", P::scrub(1.0, 0)),
    at("klipp.fx.dilate", "radius", P::scrub(1.0, 0)),
    at("klipp.fx.erode", "radius", P::scrub(1.0, 0)),
    at(EMBOSS, "angle", P::scrub(1.0, 1)),
    at(EMBOSS, "strength", P::scrub(0.05, 2)),
    at(EMBOSS, "mix", P::scrub(0.005, 2)),
    at(EDGES, "strength", P::scrub(0.05, 2)),
    at(EDGES, "mix", P::scrub(0.005, 2)),
    at("klipp.fx.mosaic", "block_size", P::scrub(0.5, 1)),
];

pub(super) static TYPES: &[TypeEntry] = &[TypeEntry {
    type_id: FOCUS,
    pres: TypePresentation { notice: Some("Set by the roll's focus band in the Title tab"), extra_ui: Some("focus_blur") },
}];
