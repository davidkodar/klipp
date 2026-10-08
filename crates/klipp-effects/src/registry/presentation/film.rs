//! The film family's rows (M3 step 0b), as the effects tab draws them
//! today (`klipp-ui` `inspector/effects_tab.rs`; recorded in
//! `klipp-ui/tests/fixtures/old_rows.json`). Today's labels are kept where
//! they differ from the catalog's. A grain's seed and the deflicker's
//! gain are system values, hidden by name.

use super::{Cond, ParamEntry, Presentation as P, Suffix, TypeEntry, TypePresentation, Widget};

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

const GRAIN: &str = "klipp.fx.film_grain";
const OLD_FILM: &str = "klipp.fx.old_film";
const VHS: &str = "klipp.fx.vhs";
const NOISE: &str = "klipp.fx.noise_generator";
const CHROMA_KEY: &str = "klipp.fx.chroma_key";
const SHUTTER: &str = "klipp.fx.rolling_shutter_correction";
const TEMPORAL: &str = "klipp.fx.temporal_denoise";
const DEFLICKER: &str = "klipp.fx.deflicker";
const STABILIZE: &str = "klipp.fx.video_stabilization";
const RAMP: &str = "klipp.fx.speed_ramp";
const FREI0R: &str = "klipp.fx.frei0r";

const fn toggle() -> P {
    P::shown_as(Widget::Toggle)
}

const fn combo() -> P {
    P::shown_as(Widget::Combo)
}

pub(super) static PARAMS: &[ParamEntry] = &[
    // Amount, size, shadows, then the colour switch, as today.
    at(GRAIN, "amount", P::scrub(0.005, 3).order(0)),
    at(GRAIN, "size", P::scrub(0.02, 2).order(1)),
    at(GRAIN, "color", toggle().label("Colour grain").order(3)),
    at(GRAIN, "shadows", P::scrub(0.005, 2).order(2)),
    at(GRAIN, "seed", P::hidden()),
    at(OLD_FILM, "flicker", P::scrub(0.005, 2)),
    at(OLD_FILM, "scratches", P::scrub(0.005, 2)),
    at(OLD_FILM, "dust", P::scrub(0.005, 2)),
    at(OLD_FILM, "weave", P::scrub(0.005, 2).label("Gate weave")),
    at(OLD_FILM, "sepia", P::scrub(0.005, 2)),
    at(OLD_FILM, "seed", P::hidden()),
    at(VHS, "tracking", P::scrub(0.005, 2)),
    at(VHS, "bleed", P::scrub(0.005, 2).label("Chroma bleed")),
    at(VHS, "noise", P::scrub(0.005, 2)),
    at(VHS, "scanlines", P::scrub(0.005, 2)),
    at(VHS, "seed", P::hidden()),
    at(NOISE, "amount", P::scrub(0.005, 3)),
    // A switch, not a 0-to-1 slider (IC5).
    at(NOISE, "monochrome", toggle()),
    at(NOISE, "seed", P::scrub(1.0, 0).widget(Widget::Slider).soft_whole((0, 10_000))),
    at(CHROMA_KEY, "key_color", P::shown_as(Widget::ColourRgb)),
    at(CHROMA_KEY, "tolerance", P::scrub(0.005, 3)),
    at(CHROMA_KEY, "spill", P::scrub(0.005, 3)),
    at(CHROMA_KEY, "spill_method", combo().label("Spill Method").labels(&["None", "Channel Clamp", "Hawk"])),
    at(CHROMA_KEY, "spill_strength", P::scrub(0.005, 3)),
    at(SHUTTER, "strength", P::scrub(0.005, 2)),
    at(SHUTTER, "readout_ratio", P::scrub(0.005, 2).label("Readout Ratio")),
    at(TEMPORAL, "strength", P::scrub(0.01, 2)),
    at(TEMPORAL, "threshold", P::scrub(0.001, 3).help(
        "How far a pixel may change between frames and still count as the same pixel. Too high and moving subjects ghost; too low and nothing is cleaned.",
    )),
    at(DEFLICKER, "strength", P::scrub(0.01, 2)),
    at(DEFLICKER, "window_secs", P::scrub(0.05, 2)),
    at(DEFLICKER, "gain", P::hidden()),
    at(STABILIZE, "smoothing", P::scrub(0.005, 2)),
    at(STABILIZE, "crop_ratio", P::scrub(0.002, 3)),
    at(STABILIZE, "zoom", toggle().label("Auto-zoom to fill")),
    at(RAMP, "speed", P::scrub(0.05, 2).suffix(Suffix::Text("×")).order(0)),
    at(RAMP, "blend_mode", combo().label("Blend Mode").labels(&["Nearest", "Linear"]).order(1)),
    at(RAMP, "ramp_curve", combo().label("Ramp Curve").labels(&["Constant", "Ease In", "Ease Out", "Ease In-Out", "S-Curve"]).order(2)),
    at(RAMP, "ramp_start", P::scrub(0.005, 3).order(3)),
    at(RAMP, "ramp_end", P::scrub(0.005, 3).order(4)),
    at(RAMP, "reverse", toggle().order(5)),
    at(RAMP, "freeze", toggle().label("Freeze Frame").order(6)),
    // Shown while it freezes, and reset to the middle (the one reset hint,
    // P2).
    at(RAMP, "freeze_at", P::scrub(0.005, 3).order(7).reset(0.5).visible_if(Cond::ParamIs { key: "freeze", value: 1.0 })),
    // The plug-in and its values, drawn by its editor (M3 step 0d).
    at(FREI0R, "type_id", P::shown_as(Widget::Custom("frei0r_plugin")).group("Plug-in")),
    at(FREI0R, "params", P::shown_as(Widget::Custom("frei0r_plugin")).group("Plug-in")),
];

pub(super) static TYPES: &[TypeEntry] = &[
    TypeEntry {
        type_id: TEMPORAL,
        pres: TypePresentation {
            notice: Some("Runs on the source, before the grade — where it sits in this list makes no difference."),
            extra_ui: Some("temporal_denoise"),
        },
    },
    TypeEntry { type_id: DEFLICKER, pres: TypePresentation { notice: None, extra_ui: Some("deflicker") } },
    TypeEntry { type_id: STABILIZE, pres: TypePresentation { notice: None, extra_ui: Some("video_stabilization") } },
    TypeEntry { type_id: RAMP, pres: TypePresentation { notice: None, extra_ui: Some("speed_ramp") } },
];
