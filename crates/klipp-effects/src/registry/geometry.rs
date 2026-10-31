//! Geometry and frame effects (#1007), describing the existing renderers.
//!
//! Defaults follow the inspector's effect reset and soft ranges its controls.
//! Crop feather is measured in the sequence's pixels, its slider 0..200 (#1030).
//! Rounded-corner radius and feather are fractions of the opaque picture's
//! shorter side, not the entire canvas. Both rounded corners and bar filling
//! need the full frame to find that picture's bounds.
//! Mirror switches and Letterbox's colour selector are stored as integers:
//! zero is off/black and any non-zero value is on/white. They are not animated.
//! Transform uses the clip's dedicated animation, not effect keyframes;
//! the compositor scales its pixel offsets. The effect kernel supports
//! independent scales, but clip placement currently ignores `scale_y`.
//! Perspective resets to zero; the video inspector shows an implicit one
//! and offers 0.1..3, and the compositor substitutes one when tilted.

use super::{
    hidden, listed, starting, BrowserCategory,
    Alpha, Category, ColourDomain, EffectType, NodeTypeDesc, ParamDesc, ParamKind, Scaling,
    Spatial, Temporal, Unit, IMAGE_IN, IMAGE_OUT, SEED_RANGE, TIME_RANGE,
};
use super::mixable;
use crate::BuiltinEffect;

const fn geometry(
    id: &'static str,
    name: &'static str,
    params: &'static [ParamDesc],
    spatial: Spatial,
    draws_outside: bool,
) -> NodeTypeDesc {
    NodeTypeDesc {
        id,
        name,
        category: Category::Geometry,
        inputs: IMAGE_IN,
        outputs: IMAGE_OUT,
        params,
        domain: ColourDomain::Linear,
        alpha: Alpha::Straight,
        spatial,
        temporal: Temporal::None,
        draws_outside,
        gpu: true,
        cpu: true,
        mixable: false,
    }
}

const fn choice(
    field: &'static str,
    label: &'static str,
    default: f32,
    names: &'static [&'static str],
) -> ParamDesc {
    ParamDesc {
        kind: ParamKind::Choice(names),
        soft: None,
        ..ParamDesc::float(field, label, default, (0.0, 1.0), Unit::None).fixed()
    }
}

const fn corner(
    field: &'static str,
    index: usize,
    key: &'static str,
    label: &'static str,
    default: f32,
) -> ParamDesc {
    ParamDesc::float(field, label, default, (-1.0, 2.0), Unit::Normalised).element(index, key)
}

pub(super) static TYPES: &[EffectType] = &[
    EffectType {
        desc: geometry(
            "klipp.fx.crop",
            "Crop",
            &[
                ParamDesc::float("left", "Left", 0.0, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("right", "Right", 0.0, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("top", "Top", 0.0, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("bottom", "Bottom", 0.0, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("feather", "Feather", 0.0, (0.0, 200.0), Unit::CanvasPx),
                // A shape to cover, width over height (0 none): the
                // picture cropped to it and scaled to fill it (#2757).
                ParamDesc::float("aspect", "Cover shape", 0.0, (0.0, 4.0), Unit::Ratio),
                ParamDesc::float("focus_x", "Focus X", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("focus_y", "Focus Y", 0.5, (0.0, 1.0), Unit::Normalised),
            ],
            Spatial::PerPixel,
            false,
        ),
        tag: "crop",
        browser: listed(BrowserCategory::Distort, "Distort", "Trims the picture's edges"),
        sample: || BuiltinEffect::Crop {
            left: 0.0,
            right: 0.0,
            top: 0.0,
            bottom: 0.0,
            feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5
        },
    },
    EffectType {
        desc: geometry(
            "klipp.fx.transform",
            "Transform",
            &[
                ParamDesc::float(
                    "position_x",
                    "Pos X",
                    0.0,
                    (-2000.0, 2000.0),
                    Unit::CanvasPx,
                )
                .scaled_by(Scaling::Placement)
                .fixed(),
                ParamDesc::float(
                    "position_y",
                    "Pos Y",
                    0.0,
                    (-2000.0, 2000.0),
                    Unit::CanvasPx,
                )
                .scaled_by(Scaling::Placement)
                .fixed(),
                ParamDesc::float("scale_x", "Scale X", 1.0, (0.0, 4.0), Unit::Ratio).fixed(),
                ParamDesc::float("scale_y", "Scale Y", 1.0, (0.0, 4.0), Unit::Ratio).fixed(),
                ParamDesc::float("rotation", "Rotation", 0.0, (-360.0, 360.0), Unit::Degrees)
                    .fixed(),
                ParamDesc::float("anchor_x", "Anchor X", 0.5, (0.0, 1.0), Unit::Normalised).fixed(),
                ParamDesc::float("anchor_y", "Anchor Y", 0.5, (0.0, 1.0), Unit::Normalised).fixed(),
                ParamDesc::float("rotation_x", "Tilt X", 0.0, (-80.0, 80.0), Unit::Degrees).fixed(),
                ParamDesc::float("rotation_y", "Tilt Y", 0.0, (-80.0, 80.0), Unit::Degrees).fixed(),
                ParamDesc::float("perspective", "Perspective", 0.0, (0.1, 3.0), Unit::Ratio)
                    .fixed()
                    // 0 is flat, no perspective.
                    .within((0.0, 12.0)),
                ParamDesc {
                    kind: ParamKind::Other,
                    soft: None,
                    ..ParamDesc::float(
                        "focal_ref",
                        "Focal Anchor",
                        f32::NAN,
                        (0.0, 0.0),
                        Unit::None,
                    )
                    .fixed()
                },
            ],
            Spatial::Geometric,
            false,
        ),
        tag: "transform",
        browser: listed(BrowserCategory::Distort, "Distort", "Moves, scales and turns the picture in the frame"),
        sample: || BuiltinEffect::Transform {
            position_x: 0.0,
            position_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5,
            anchor_y: 0.5,
            rotation_x: 0.0,
            rotation_y: 0.0,
            perspective: 0.0,
            focal_ref: None,
        },
    },
    EffectType {
        desc: geometry(
            "klipp.fx.rounded_corners",
            "Rounded Corners",
            &[
                ParamDesc::float("radius", "Radius", 0.0, (0.0, 0.5), Unit::Normalised),
                ParamDesc::float("feather", "Feather", 0.0, (0.0, 0.1), Unit::Normalised),
            ],
            Spatial::Global,
            false,
        ),
        tag: "rounded_corners",
        browser: listed(BrowserCategory::Distort, "Distort", "Rounds the corners of the picture, so a placed clip reads as a card"),
        sample: || BuiltinEffect::RoundedCorners {
            radius: 0.0,
            feather: 0.0,
        },
    },
    EffectType {
        desc: geometry(
            "klipp.fx.fill_bars",
            "Fill Bars",
            &[
                ParamDesc::float("blur", "Blur", 0.05, (0.0, 0.2), Unit::Normalised),
                ParamDesc::float("zoom", "Zoom", 1.0, (1.0, 3.0), Unit::Ratio),
                ParamDesc::float("darken", "Darken", 0.25, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::Global,
            true,
        ),
        tag: "fill_bars",
        browser: listed(BrowserCategory::Blur, "Blur", "Fills letterbox and pillarbox bars, as round an upright phone shot, with the picture itself, enlarged and blurred. Also known as blanking fill, blur fill, blurred background."),
        sample: || BuiltinEffect::FillBars {
            blur: 0.05,
            zoom: 1.0,
            darken: 0.25,
        },
    },
    EffectType {
        desc: geometry(
            "klipp.fx.mirror",
            "Mirror",
            &[
                choice("horizontal", "Horizontal", 1.0, &["Off", "On"]),
                choice("vertical", "Vertical", 0.0, &["Off", "On"]),
            ],
            Spatial::Geometric,
            false,
        ),
        tag: "mirror",
        browser: listed(BrowserCategory::Distort, "Transform", "Flips the picture across or up and down"),
        sample: || BuiltinEffect::Mirror {
            horizontal: 1,
            vertical: 0,
        },
    },
    EffectType {
        desc: geometry(
            "klipp.fx.lens_distortion",
            "Lens Distortion",
            &[
                ParamDesc::float("distortion", "Distortion", 0.0, (-1.0, 1.0), Unit::Ratio),
                ParamDesc::float("scale", "Scale", 1.0, (0.5, 2.0), Unit::Ratio),
            ],
            Spatial::Geometric,
            false,
        ),
        tag: "lens_distortion",
        // A wide lens's barrel to start; at its reset, 0, the picture was
        // unchanged (#3243).
        browser: starting(BrowserCategory::Distort, "Distort", "Bends the picture as a wide or long lens does, barrel or pincushion", || BuiltinEffect::LensDistortion { distortion: -0.2, scale: 1.0 }),
        sample: || BuiltinEffect::LensDistortion {
            distortion: 0.0,
            scale: 1.0,
        },
    },
    // Each pixel moved by a noise field (#2702): heat haze, water, a
    // wobble. Its time is the compositor's (the clip's time each frame),
    // as Flicker's is; its seed the user's.
    EffectType {
        desc: mixable(geometry(
            "klipp.fx.displace",
            "Displace",
            &[
                ParamDesc::float("amount", "Amount", 12.0, (0.0, 200.0), Unit::CanvasPx),
                ParamDesc::float("size", "Size", 80.0, (4.0, 800.0), Unit::CanvasPx),
                ParamDesc::float("speed", "Speed", 1.0, (0.0, 10.0), Unit::Ratio),
                choice("direction", "Direction", 0.0, &["both", "across", "up_down"]),
                ParamDesc { kind: ParamKind::Whole, ..ParamDesc::float("seed", "Seed", 0.0, (0.0, 10_000.0), Unit::Count).fixed().within(SEED_RANGE) },
                ParamDesc { soft: None, ..ParamDesc::float("time", "Time", 0.0, (0.0, 1.0), Unit::Seconds).fixed().within(TIME_RANGE) },
            ],
            Spatial::Geometric,
            false,
        )),
        tag: "displace",
        browser: listed(BrowserCategory::Distort, "Distort", "Moves the pixels by a drifting noise field: ripples and heat haze"),
        sample: || BuiltinEffect::Displace {
            amount: 12.0,
            size: 80.0,
            speed: 1.0,
            direction: 0,
            seed: 0,
            time: 0.0,
        },
    },
    // The picture bent by one of the shared warps (#2758): a wave, a
    // twirl, a bulge or pinch, ripples, torn rows. Its time is the
    // compositor's, as Displace's is.
    EffectType {
        desc: mixable(geometry(
            "klipp.fx.warp",
            "Warp",
            &[
                choice("mode", "Shape", 0.0, &["wave", "twirl", "bulge", "ripples", "torn_rows"]),
                ParamDesc::float("amplitude", "Amplitude", 20.0, (0.0, 400.0), Unit::Per1080),
                ParamDesc::float("wavelength", "Wavelength", 0.2, (0.01, 2.0), Unit::Normalised),
                ParamDesc::float("speed", "Speed", 1.0, (0.0, 20.0), Unit::Ratio),
                choice("direction", "Direction", 0.0, &["across", "up_down", "both"]),
                ParamDesc::float("angle", "Angle", 180.0, (-1440.0, 1440.0), Unit::Degrees),
                ParamDesc::float("strength", "Strength", 0.5, (-0.9, 3.0), Unit::Ratio),
                ParamDesc::float("radius", "Radius", 0.5, (0.0, 2.0), Unit::Normalised),
                ParamDesc::float("center_x", "Centre X", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("center_y", "Centre Y", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("band", "Band", 0.04, (0.005, 0.5), Unit::Normalised),
                ParamDesc { soft: None, ..ParamDesc::float("time", "Time", 0.0, (0.0, 1.0), Unit::Seconds).fixed().within(TIME_RANGE) },
            ],
            Spatial::Geometric,
            false,
        )),
        tag: "warp",
        browser: listed(BrowserCategory::Distort, "Distort", "Bends the picture: a wave, a twirl, a bulge or pinch, ripples or torn rows"),
        sample: || BuiltinEffect::Warp {
            mode: 0,
            amplitude: 20.0,
            wavelength: 0.2,
            speed: 1.0,
            direction: 0,
            angle: 180.0,
            strength: 0.5,
            radius: 0.5,
            center_x: 0.5,
            center_y: 0.5,
            band: 0.04,
            time: 0.0,
        },
    },
    // The layer mirrored about a waterline (#2707). It draws below the
    // layer, so it runs on the placed layer; its time is the
    // compositor's, as Displace's is.
    EffectType {
        desc: geometry(
            "klipp.fx.reflection",
            "Reflection",
            &[
                ParamDesc::float("waterline", "Waterline", 0.6, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("gap", "Gap", 0.0, (0.0, 200.0), Unit::CanvasPx),
                ParamDesc::float("ripple_amount", "Ripple", 10.0, (0.0, 60.0), Unit::CanvasPx),
                ParamDesc::float("ripple_size", "Ripple Size", 60.0, (4.0, 400.0), Unit::CanvasPx),
                ParamDesc::float("ripple_speed", "Ripple Speed", 0.6, (0.0, 5.0), Unit::Ratio),
                ParamDesc::float("breakup", "Break-up", 0.6, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("smear", "Smear", 0.5, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("fade", "Fade", 0.45, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("opacity", "Opacity", 0.85, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("darken", "Darken", 0.45, (0.0, 1.0), Unit::Ratio),
                ParamDesc::float("contact", "Contact Shadow", 0.6, (0.0, 1.0), Unit::Ratio),
                choice("blend", "Blend", 0.0, &["normal", "screen", "soft_light", "overlay"]),
                ParamDesc {
                    field: "tint",
                    index: None,
                    key: "tint",
                    label: "Tint",
                    kind: ParamKind::Colour,
                    default: f32::NAN,
                    range: None,
                    soft: None,
                    unit: Unit::None,
                    scaling: Scaling::None,
                    keyframable: false,
                    derived: false,
                },
                ParamDesc::float("tint_amount", "Tint Amount", 0.2, (0.0, 1.0), Unit::Ratio),
                ParamDesc { kind: ParamKind::Bool, range: None, soft: None, unit: Unit::None, keyframable: false, ..ParamDesc::float("tint_auto", "Tint From the Water", 1.0, (0.0, 1.0), Unit::None) },
                ParamDesc { kind: ParamKind::Bool, range: None, soft: None, unit: Unit::None, keyframable: false, ..ParamDesc::float("darken_auto", "Darken From the Water", 1.0, (0.0, 1.0), Unit::None) },
                ParamDesc { field: "water", key: "water", label: "Water", kind: ParamKind::Colour, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false, index: None },
                ParamDesc { kind: ParamKind::Whole, ..ParamDesc::float("water_key", "Water Measured For", 0.0, (0.0, 16_777_215.0), Unit::Count).fixed() },
                ParamDesc::float("submerge", "Submerge", crate::reflection::SUBMERGE, (0.0, 1.0), Unit::Ratio),
                // The picture below as the ripple's source comes later.
                choice("ripple_from", "Ripple From", 0.0, &["noise"]),
                ParamDesc { kind: ParamKind::Whole, ..ParamDesc::float("seed", "Seed", 0.0, (0.0, 10_000.0), Unit::Count).fixed().within(SEED_RANGE) },
                ParamDesc { soft: None, ..ParamDesc::float("time", "Time", 0.0, (0.0, 1.0), Unit::Seconds).fixed().within(TIME_RANGE) },
            ],
            Spatial::Geometric,
            true,
        ),
        tag: "reflection",
        browser: listed(BrowserCategory::Distort, "Distort", "The picture mirrored below a waterline, on water, a floor, a wet street or glass"),
        sample: || BuiltinEffect::Reflection {
            waterline: 0.6,
            gap: 0.0,
            ripple_amount: 10.0,
            ripple_size: 60.0,
            ripple_speed: 0.6,
            breakup: 0.6,
            smear: 0.5,
            fade: 0.45,
            opacity: 0.85,
            darken: 0.45,
            contact: 0.6,
            blend: 0,
            tint: [25, 85, 100],
            tint_amount: 0.2,
            tint_auto: true,
            darken_auto: true,
            water: [0, 0, 0],
            water_key: 0,
            submerge: crate::reflection::SUBMERGE,
            ripple_from: 0,
            seed: 0,
            time: 0.0,
        },
    },
    EffectType {
        desc: geometry(
            "klipp.fx.letterbox",
            "Letterbox",
            &[
                ParamDesc::float(
                    "aspect_ratio",
                    "Aspect Ratio",
                    2.39,
                    (0.5, 4.0),
                    Unit::Ratio,
                ),
                choice("bar_color", "Bar Colour", 0.0, &["Black", "White"]),
                ParamDesc::float("opacity", "Opacity", 1.0, (0.0, 1.0), Unit::Ratio),
            ],
            Spatial::PerPixel,
            false,
        ),
        tag: "letterbox",
        browser: listed(BrowserCategory::Stylize, "Stylize", "Bars to a wider frame shape, black or white"),
        sample: || BuiltinEffect::Letterbox {
            aspect_ratio: 2.39,
            bar_color: 0,
            opacity: 1.0,
        },
    },
    EffectType {
        desc: geometry(
            "klipp.fx.corner_pin",
            "Corner Pin",
            &[
                corner("tl", 0, "tl_x", "Top Left X", 0.0),
                corner("tl", 1, "tl_y", "Top Left Y", 0.0),
                corner("tr", 0, "tr_x", "Top Right X", 1.0),
                corner("tr", 1, "tr_y", "Top Right Y", 0.0),
                corner("br", 0, "br_x", "Bottom Right X", 1.0),
                corner("br", 1, "br_y", "Bottom Right Y", 1.0),
                corner("bl", 0, "bl_x", "Bottom Left X", 0.0),
                corner("bl", 1, "bl_y", "Bottom Left Y", 1.0),
            ],
            Spatial::Geometric,
            false,
        ),
        tag: "corner_pin",
        browser: listed(BrowserCategory::Distort, "Distort", "Pin the picture to four corners, in perspective"),
        sample: crate::corner_pin::still,
    },
    EffectType {
        desc: geometry(
            "klipp.fx.surface_pin",
            "Surface Pin",
            &[
                // The surface the clip carries, by name or id: set where
                // the pin is made, not a row.
                ParamDesc { kind: ParamKind::Other, soft: None, ..ParamDesc::float("surface", "Surface", f32::NAN, (0.0, 0.0), Unit::None).fixed() },
                ParamDesc::float("inset", "Inset", 0.0, (0.0, 0.45), Unit::Ratio),
            ],
            Spatial::Geometric,
            false,
        ),
        tag: "surface_pin",
        browser: hidden("Pins the picture to a tracked surface; the planar tracker's pin places it and sets the surface it follows"),
        sample: || BuiltinEffect::SurfacePin { surface: String::new(), inset: 0.0, on_source: [[0.0; 2]; 4], source: [0; 2] },
    },
    EffectType {
        desc: geometry(
            "klipp.fx.picture_transform",
            "Picture Transform",
            &[
                // A move inside the frame, not a placement across it: the
                // slider keeps to a frame's width either way.
                ParamDesc::float("position_x", "Pos X", 0.0, (-1000.0, 1000.0), Unit::CanvasPx),
                ParamDesc::float("position_y", "Pos Y", 0.0, (-1000.0, 1000.0), Unit::CanvasPx),
                ParamDesc::float("scale_x", "Scale X", 1.0, (0.0, 4.0), Unit::Ratio),
                ParamDesc::float("scale_y", "Scale Y", 1.0, (0.0, 4.0), Unit::Ratio),
                ParamDesc::float("rotation", "Rotation", 0.0, (-360.0, 360.0), Unit::Degrees),
                ParamDesc::float("anchor_x", "Anchor X", 0.5, (0.0, 1.0), Unit::Normalised),
                ParamDesc::float("anchor_y", "Anchor Y", 0.5, (0.0, 1.0), Unit::Normalised),
            ],
            Spatial::Geometric,
            false,
        ),
        tag: "picture_transform",
        browser: listed(BrowserCategory::Distort, "Distort", "Moves, scales and turns the picture inside the clip's frame, where it sits"),
        sample: || BuiltinEffect::PictureTransform { position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5 },
    },
];
