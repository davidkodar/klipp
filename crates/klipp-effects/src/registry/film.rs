//! Film, generator, key, temporal and plug-in effects (#1008).
//!
//! Defaults are what resetting the effect gives (the inspector's
//! `builtin_effect_default`); soft ranges are the inspector sliders', or,
//! where the inspector draws no slider (stabilisation), the ranges the
//! fields' own documentation gives.
//!
//! Some fields are written by the compositor every frame but are still
//! saved: the grain, old-film, VHS and noise `seed` (the frame number),
//! deflicker's `gain` (from the clip's analysis) and flicker's and
//! particles' `time` (the clip's time, #989, #1141). They are described
//! as saved, fixed parameters; nothing a user sets.
//!
//! Where two places disagree, the description follows the reset and says
//! so:
//!
//! * grain `size` is measured per 1080-line frame (the effect scales it
//!   internally), so `at_render_scale` rightly leaves it alone.
//!
//! The temporal ones read other times in one of two ways: temporal noise
//! reduction and a speed ramp's frame blending read neighbouring source
//! frames; deflicker, stabilisation and rolling-shutter correction read
//! per-frame results of an analysis of the clip.

use super::{
    hidden, listed, BrowserCategory,
    mixable,
    Alpha, Category, ColourDomain, EffectType, NodeTypeDesc, ParamDesc, ParamKind, Scaling, Spatial, Temporal, Unit,
    IMAGE_IN, IMAGE_OUT, SEED_RANGE, TIME_RANGE,
};
use crate::{BuiltinEffect, SpillMethod};

#[allow(clippy::too_many_arguments)]
const fn effect(
    id: &'static str,
    name: &'static str,
    category: Category,
    params: &'static [ParamDesc],
    domain: ColourDomain,
    spatial: Spatial,
    temporal: Temporal,
    gpu: bool,
) -> NodeTypeDesc {
    NodeTypeDesc {
        id,
        name,
        category,
        inputs: IMAGE_IN,
        outputs: IMAGE_OUT,
        params,
        domain,
        alpha: Alpha::Straight,
        spatial,
        temporal,
        draws_outside: false,
        gpu,
        cpu: true,
        mixable: false,
    }
}

/// A value that is not a single number: a colour, a path, a list.
const fn other(field: &'static str, label: &'static str, kind: ParamKind) -> ParamDesc {
    ParamDesc {
        field,
        index: None,
        key: field,
        label,
        kind,
        default: f32::NAN,
        range: None,
        soft: None,
        unit: Unit::None,
        scaling: Scaling::None,
        keyframable: false,
        derived: false,
    }
}

/// One of a fixed set, saved as its index (a `u32`) or its name.
const fn choice(field: &'static str, label: &'static str, default: f32, names: &'static [&'static str]) -> ParamDesc {
    ParamDesc { kind: ParamKind::Choice(names), soft: None, ..ParamDesc::float(field, label, default, (0.0, 1.0), Unit::None).fixed() }
}

/// An on or off switch.
const fn switch(field: &'static str, label: &'static str, on: bool) -> ParamDesc {
    ParamDesc {
        kind: ParamKind::Bool,
        soft: None,
        ..ParamDesc::float(field, label, if on { 1.0 } else { 0.0 }, (0.0, 1.0), Unit::None).fixed()
    }
}

/// A pattern seed the compositor sets to the frame number each frame.
const fn seed() -> ParamDesc {
    ParamDesc { kind: ParamKind::Whole, soft: None, ..ParamDesc::float("seed", "Seed", 0.0, (0.0, 1.0), Unit::Count).fixed().within(SEED_RANGE) }
}

const UNIT: (f32, f32) = (0.0, 1.0);

pub(super) static TYPES: &[EffectType] = &[
    EffectType {
        desc: mixable(effect(
            "klipp.fx.film_grain",
            "Film Grain",
            Category::FilmTemporal,
            &[
                ParamDesc::float("amount", "Amount", 0.3, UNIT, Unit::Ratio),
                ParamDesc::float("size", "Size", 1.5, (0.5, 6.0), Unit::Per1080),
                choice("color", "Colour", 0.0, &["mono", "colour"]),
                ParamDesc::float("shadows", "Shadows", 0.6, UNIT, Unit::Ratio),
                seed(),
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
            Temporal::None,
            true,
        )),
        tag: "film_grain",
        browser: listed(BrowserCategory::Film, "Film", "Animated emulsion grain, heavier in the shadows."),
        sample: || BuiltinEffect::FilmGrain { amount: 0.3, size: 1.5, color: 0, shadows: 0.6, seed: 0 },
    },
    EffectType {
        desc: mixable(effect(
            "klipp.fx.old_film",
            "Old Film",
            Category::FilmTemporal,
            &[
                ParamDesc::float("flicker", "Flicker", 0.4, UNIT, Unit::Ratio),
                ParamDesc::float("scratches", "Scratches", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("dust", "Dust", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("weave", "Weave", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("sepia", "Sepia", 0.6, UNIT, Unit::Ratio),
                seed(),
                ParamDesc::float("amount", "Damage", 1.0, (0.0, 2.0), Unit::Ratio),
                ParamDesc::float("age", "Age", 0.0, UNIT, Unit::Ratio),
                ParamDesc::float("mono", "Black and White", 0.0, UNIT, Unit::Ratio),
                ParamDesc::float("stains", "Stains", 0.0, UNIT, Unit::Ratio),
                ParamDesc::float("frame_lines", "Frame Line", 0.0, UNIT, Unit::Ratio),
                ParamDesc::float("leaks", "Light Leaks", 0.0, UNIT, Unit::Ratio),
                ParamDesc::float("burn", "Burned Edges", 0.0, UNIT, Unit::Ratio),
            ],
            ColourDomain::Linear,
            Spatial::Geometric,
            Temporal::None,
            true,
        )),
        tag: "old_film",
        browser: listed(BrowserCategory::Film, "Film", "Flicker, scratches, dust, gate weave and a sepia wash."),
        sample: || BuiltinEffect::OldFilm { flicker: 0.4, scratches: 0.5, dust: 0.5, weave: 0.5, sepia: 0.6, seed: 0, amount: 1.0, age: 0.0, mono: 0.0, stains: 0.0, frame_lines: 0.0, leaks: 0.0, burn: 0.0 },
    },
    EffectType {
        desc: mixable(effect(
            "klipp.fx.vhs",
            "VHS",
            Category::FilmTemporal,
            &[
                ParamDesc::float("tracking", "Tracking", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("bleed", "Bleed", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("noise", "Noise", 0.4, UNIT, Unit::Ratio),
                ParamDesc::float("scanlines", "Scanlines", 0.5, UNIT, Unit::Ratio),
                seed(),
            ],
            ColourDomain::Linear,
            Spatial::Neighbourhood,
            Temporal::None,
            true,
        )),
        tag: "vhs",
        browser: listed(BrowserCategory::Film, "Film", "Tape tracking, chroma bleed, noise and scanlines."),
        sample: || BuiltinEffect::Vhs { tracking: 0.5, bleed: 0.5, noise: 0.4, scanlines: 0.5, seed: 0 },
    },
    EffectType {
        desc: mixable(effect(
            "klipp.fx.noise_generator",
            "Noise",
            Category::FilmTemporal,
            &[ParamDesc::float("amount", "Amount", 0.1, UNIT, Unit::Ratio), choice("monochrome", "Monochrome", 0.0, &["colour", "grey"]), seed()],
            ColourDomain::Linear,
            Spatial::PerPixel,
            Temporal::None,
            true,
        )),
        tag: "noise_generator",
        browser: listed(BrowserCategory::Film, "Film", "Random noise over the picture, in colour or grey"),
        sample: || BuiltinEffect::NoiseGenerator { amount: 0.1, monochrome: 0, seed: 0 },
    },
    // A random flicker (#989): its seed the user's, its time the
    // compositor's (the clip's time each frame), as grain's seed is.
    EffectType {
        desc: mixable(effect(
            "klipp.fx.flicker",
            "Flicker",
            Category::FilmTemporal,
            &[
                ParamDesc::float("amount", "Amount", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("speed", "Speed", 8.0, (0.5, 30.0), Unit::Ratio),
                ParamDesc::float("smoothness", "Smoothness", 0.0, UNIT, Unit::Ratio),
                ParamDesc::float("hold", "Hold", 0.3, UNIT, Unit::Ratio),
                ParamDesc { kind: ParamKind::Whole, ..ParamDesc::float("seed", "Seed", 67.0, (0.0, 10_000.0), Unit::Count).fixed().within(SEED_RANGE) },
                choice("target", "Flickers", 0.0, &["opacity", "exposure", "gamma"]),
                ParamDesc { soft: None, ..ParamDesc::float("time", "Time", 0.0, (0.0, 1.0), Unit::Seconds).fixed().within(TIME_RANGE) },
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
            Temporal::None,
            true,
        )),
        tag: "flicker",
        browser: listed(BrowserCategory::Film, "Film", "Random jumps of the opacity, exposure or gamma, at a speed, with still spells."),
        sample: || BuiltinEffect::Flicker { amount: 0.5, speed: 8.0, smoothness: 0.0, hold: 0.3, seed: 67, target: 0, time: 0.0 },
    },
    // Particles drawn over the picture (#1141): their seed the user's,
    // their time the compositor's (the clip's time each frame).
    EffectType {
        desc: effect(
            "klipp.fx.particles",
            "Particles",
            Category::FilmTemporal,
            &[
                choice("style", "Kind", 0.0, &["dust", "snow", "rain", "sparks", "embers"]),
                ParamDesc::float("amount", "Amount", 150.0, (10.0, 2000.0), Unit::Count),
                ParamDesc::float("size", "Size", 2.5, (0.5, 20.0), Unit::Per1080),
                ParamDesc::float("speed", "Speed", 30.0, (0.0, 1500.0), Unit::Per1080),
                ParamDesc::float("direction", "Direction", 300.0, (0.0, 360.0), Unit::Degrees),
                ParamDesc::float("spread", "Spread", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("lifetime", "Lifetime", 4.0, (0.2, 20.0), Unit::Seconds),
                ParamDesc::float("opacity", "Opacity", 0.6, UNIT, Unit::Ratio),
                other("color", "Colour", ParamKind::Colour),
                ParamDesc { kind: ParamKind::Whole, ..ParamDesc::float("seed", "Seed", 1.0, (0.0, 10_000.0), Unit::Count).fixed().within(SEED_RANGE) },
                ParamDesc { soft: None, ..ParamDesc::float("time", "Time", 0.0, (0.0, 1.0), Unit::Seconds).fixed().within(TIME_RANGE) },
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
            Temporal::None,
            true,
        ),
        tag: "particles",
        browser: listed(BrowserCategory::Film, "Film", "Dust, snow, rain, sparks or embers over the picture, seeded, the same every play."),
        sample: || BuiltinEffect::Particles { style: 0, amount: 150.0, size: 2.5, speed: 30.0, direction: 300.0, spread: 0.5, lifetime: 4.0, opacity: 0.6, color: [255, 240, 220], seed: 1, time: 0.0 },
    },
    EffectType {
        desc: effect(
            "klipp.fx.chroma_key",
            "Chroma Key",
            Category::Key,
            &[
                other("key_color", "Key Colour", ParamKind::Colour),
                ParamDesc::float("tolerance", "Tolerance", 0.2, UNIT, Unit::Ratio),
                // Keyed "spill" since before it was renamed from
                // spill_suppression: the key never followed.
                ParamDesc { key: "spill", ..ParamDesc::float("soft_edge", "Soft Edge", 0.1, UNIT, Unit::Ratio) },
                choice("spill_method", "Spill", 1.0, &["none", "channel_clamp", "hawk"]),
                ParamDesc::float("spill_strength", "Spill Strength", 0.6, UNIT, Unit::Ratio),
            ],
            ColourDomain::DisplayEncoded,
            Spatial::PerPixel,
            Temporal::None,
            true,
        ),
        tag: "chroma_key",
        browser: listed(BrowserCategory::Keying, "Keying", "Key out a colour, green by default, with its spill pulled back"),
        sample: || BuiltinEffect::ChromaKey {
            key_color: [0, 255, 0],
            tolerance: 0.2,
            soft_edge: 0.1,
            spill_method: SpillMethod::ChannelClamp,
            spill_strength: 0.6,
        },
    },
    // Keyed by brightness (#1638): the range keyed out, black by
    // default, soft past its ends; invert keeps the range instead.
    EffectType {
        desc: effect(
            "klipp.fx.luma_key",
            "Luma Key",
            Category::Key,
            &[
                ParamDesc::float("low", "Low", 0.0, UNIT, Unit::Ratio),
                ParamDesc::float("high", "High", 0.1, UNIT, Unit::Ratio),
                ParamDesc::float("soft", "Softness", 0.05, (0.0, 0.5), Unit::Ratio),
                switch("invert", "Invert", false),
            ],
            ColourDomain::DisplayEncoded,
            Spatial::PerPixel,
            Temporal::None,
            true,
        ),
        tag: "luma_key",
        browser: listed(BrowserCategory::Keying, "Keying", "Key out a range of brightness, black by default: Low to High is see-through."),
        sample: || BuiltinEffect::LumaKey { low: 0.0, high: 0.1, soft: 0.05, invert: false },
    },
    EffectType {
        desc: effect(
            "klipp.fx.rolling_shutter_correction",
            "Rolling Shutter Correction",
            Category::FilmTemporal,
            &[
                ParamDesc::float("strength", "Strength", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("readout_ratio", "Readout", 0.5, UNIT, Unit::Ratio),
            ],
            ColourDomain::Linear,
            Spatial::Geometric,
            Temporal::AnalysisDriven,
            true,
        ),
        tag: "rolling_shutter_correction",
        browser: listed(BrowserCategory::Distort, "Distort", "Straightens the lean a rolling shutter gives fast pans"),
        sample: || BuiltinEffect::RollingShutterCorrection { strength: 0.5, readout_ratio: 0.5 },
    },
    EffectType {
        desc: effect(
            "klipp.fx.temporal_denoise",
            "Temporal Noise Reduction",
            Category::FilmTemporal,
            &[
                ParamDesc::float("strength", "Strength", 0.5, UNIT, Unit::Ratio),
                ParamDesc::float("threshold", "Threshold", 0.02, (0.001, 0.2), Unit::Ratio),
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
            Temporal::SourceNeighbours,
            // The GPU paths hand a clip with it to the CPU (#996).
            false,
        ),
        tag: "temporal_denoise",
        browser: listed(BrowserCategory::Distort, "Repair", "Cleans grain by averaging each pixel with the frames either side, leaving moving subjects alone. Runs on the source, before the grade."),
        sample: || BuiltinEffect::TemporalDenoise { strength: 0.5, threshold: 0.02 },
    },
    EffectType {
        desc: effect(
            "klipp.fx.deflicker",
            "Deflicker",
            Category::FilmTemporal,
            &[
                ParamDesc::float("strength", "Strength", 0.8, UNIT, Unit::Ratio),
                ParamDesc::float("window_secs", "Window", 1.0, (0.2, 4.0), Unit::Seconds).fixed(),
                // Set each frame from the clip's analysis; 1.0 until it is analysed.
                ParamDesc { soft: None, ..ParamDesc::float("gain", "Gain", 1.0, (0.0, 1.0), Unit::Ratio).fixed().within((0.0, 8.0)) },
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
            Temporal::AnalysisDriven,
            true,
        ),
        tag: "deflicker",
        browser: listed(BrowserCategory::Distort, "Repair", "Steadies whole-frame brightness pumping from mains-frequency lighting. Measures the clip first."),
        sample: || BuiltinEffect::Deflicker { strength: 0.8, window_secs: 1.0, gain: 1.0 },
    },
    EffectType {
        desc: effect(
            "klipp.fx.video_stabilization",
            "Stabilisation",
            Category::FilmTemporal,
            &[
                ParamDesc::float("smoothing", "Smoothing", 0.5, UNIT, Unit::Ratio).fixed(),
                ParamDesc::float("crop_ratio", "Crop", 0.05, (0.0, 0.2), Unit::Ratio),
                switch("zoom", "Zoom to Fill", true),
            ],
            ColourDomain::Linear,
            Spatial::Geometric,
            Temporal::AnalysisDriven,
            true,
        ),
        tag: "video_stabilization",
        browser: listed(BrowserCategory::Distort, "Distort", "Takes out the shake, keeping a pan or tracking move"),
        sample: || BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true },
    },
    EffectType {
        desc: effect(
            "klipp.fx.speed_ramp",
            "Speed Ramp",
            Category::FilmTemporal,
            &[
                ParamDesc::float("speed", "Speed", 1.0, (0.1, 4.0), Unit::Ratio).fixed(),
                choice("blend_mode", "Blend", 1.0, &["nearest", "linear", "optical flow"]),
            ],
            ColourDomain::Linear,
            Spatial::PerPixel,
            Temporal::SourceNeighbours,
            false,
        ),
        tag: "speed_ramp",
        browser: listed(BrowserCategory::Stylize, "Stylize", "Plays the clip faster or slower, blending or following the motion between frames"),
        sample: || BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 1 },
    },
    EffectType {
        desc: effect(
            "klipp.fx.frei0r",
            "frei0r",
            Category::PlugIn,
            // The plug-in's own parameters are described per plug-in later;
            // here the effect records which plug-in and its values in order.
            &[other("type_id", "Plug-in", ParamKind::Other), other("params", "Values", ParamKind::Other)],
            ColourDomain::DisplayEncoded,
            Spatial::Global,
            Temporal::None,
            false,
        ),
        tag: "frei0r",
        browser: hidden("Runs one installed frei0r plug-in; each plug-in the scan finds is listed as its own row"),
        sample: || BuiltinEffect::Frei0r { type_id: String::new(), params: Vec::new() },
    },
];
