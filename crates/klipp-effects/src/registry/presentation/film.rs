//! The film family's rows (M3 step 0b), as the effects tab draws them
//! today (`klipp-ui` `inspector/effects_tab.rs`; recorded in
//! `klipp-ui/tests/fixtures/old_rows.json`). Today's labels are kept where
//! they differ from the catalog's. A grain's seed and the deflicker's
//! gain are system values, hidden by name.

use super::{ParamEntry, Presentation as P, Suffix, TypeEntry, TypePresentation, Widget};

const fn at(type_id: &'static str, key: &'static str, pres: P) -> ParamEntry {
    ParamEntry { type_id, key, pres }
}

const GRAIN: &str = "klipp.fx.film_grain";
const OLD_FILM: &str = "klipp.fx.old_film";
const VHS: &str = "klipp.fx.vhs";
const NOISE: &str = "klipp.fx.noise_generator";
const CHROMA_KEY: &str = "klipp.fx.chroma_key";
const LUMA_KEY: &str = "klipp.fx.luma_key";
const SHUTTER: &str = "klipp.fx.rolling_shutter_correction";
const TEMPORAL: &str = "klipp.fx.temporal_denoise";
const DEFLICKER: &str = "klipp.fx.deflicker";
const STABILIZE: &str = "klipp.fx.video_stabilization";
const RAMP: &str = "klipp.fx.speed_ramp";
const FREI0R: &str = "klipp.fx.frei0r";
const FLICKER: &str = "klipp.fx.flicker";
const PARTICLES: &str = "klipp.fx.particles";

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
    // Age and Damage first (#2744), then each part.
    at(OLD_FILM, "age", P::scrub(0.005, 2).order(0)),
    at(OLD_FILM, "amount", P::scrub(0.005, 2).label("Damage").order(1)),
    at(OLD_FILM, "flicker", P::scrub(0.005, 2).order(2)),
    at(OLD_FILM, "scratches", P::scrub(0.005, 2).order(2)),
    at(OLD_FILM, "dust", P::scrub(0.005, 2).label("Debris").order(2)),
    at(OLD_FILM, "stains", P::scrub(0.005, 2).order(2)),
    at(OLD_FILM, "weave", P::scrub(0.005, 2).label("Gate weave").order(3)),
    at(OLD_FILM, "frame_lines", P::scrub(0.005, 2).label("Frame line").order(3)),
    at(OLD_FILM, "leaks", P::scrub(0.005, 2).label("Light leaks").order(3)),
    at(OLD_FILM, "burn", P::scrub(0.005, 2).label("Burned edges").order(3)),
    at(OLD_FILM, "sepia", P::scrub(0.005, 2).order(4)),
    at(OLD_FILM, "mono", P::scrub(0.005, 2).label("Black and white").order(4)),
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
    // Flicker (#989): what flickers first; its time is the compositor's.
    at(FLICKER, "target", combo().labels(&["Opacity", "Exposure", "Gamma"]).order(0)),
    at(FLICKER, "amount", P::scrub(0.005, 2).order(1)),
    at(FLICKER, "speed", P::scrub(0.1, 1).suffix(Suffix::Text("/s")).order(2)),
    at(FLICKER, "smoothness", P::scrub(0.005, 2).order(3)),
    at(FLICKER, "hold", P::scrub(0.005, 2).order(4)),
    at(FLICKER, "seed", P::scrub(1.0, 0).soft_whole((0, 10_000)).order(5)),
    at(FLICKER, "time", P::hidden()),
    // Particles (#1141): the kind first; their time is the compositor's.
    at(PARTICLES, "style", combo().labels(&["Dust", "Snow", "Rain", "Sparks", "Embers"]).order(0)),
    at(PARTICLES, "amount", P::scrub(1.0, 0).order(1)),
    at(PARTICLES, "size", P::scrub(0.05, 1).order(2)),
    at(PARTICLES, "speed", P::scrub(1.0, 0).suffix(Suffix::Text("px/s")).order(3)),
    at(PARTICLES, "direction", P::scrub(1.0, 0).order(4)),
    at(PARTICLES, "spread", P::scrub(0.005, 2).order(5)),
    at(PARTICLES, "lifetime", P::scrub(0.05, 1).order(6)),
    at(PARTICLES, "opacity", P::scrub(0.005, 2).order(7)),
    at(PARTICLES, "color", P::shown_as(Widget::ColourRgb).order(8)),
    at(PARTICLES, "seed", P::scrub(1.0, 0).soft_whole((0, 10_000)).order(9)),
    at(PARTICLES, "time", P::hidden()),
    at(CHROMA_KEY, "key_color", P::shown_as(Widget::ColourRgb)),
    at(CHROMA_KEY, "tolerance", P::scrub(0.005, 3)),
    at(CHROMA_KEY, "spill", P::scrub(0.005, 3)),
    at(CHROMA_KEY, "spill_method", combo().label("Spill Method").labels(&["None", "Channel Clamp", "Hawk"])),
    at(CHROMA_KEY, "spill_strength", P::scrub(0.005, 3)),
    // Luma Key (#1638): the range, its softness, then the invert.
    at(LUMA_KEY, "low", P::scrub(0.005, 3).order(0).help("The darkest brightness keyed out, 0 black to 1 white.")),
    at(LUMA_KEY, "high", P::scrub(0.005, 3).order(1).help("The brightest brightness keyed out: Low to High is see-through.")),
    at(LUMA_KEY, "soft", P::scrub(0.005, 3).order(2).help("How far past Low and High the key fades back in, for a soft edge.")),
    at(LUMA_KEY, "invert", toggle().order(3).help("Keep Low to High and key out the rest.")),
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
    at(RAMP, "blend_mode", combo().label("Blend Mode").labels(&["Nearest", "Linear", "Optical Flow"]).order(1)),
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
