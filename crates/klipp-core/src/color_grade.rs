use crate::keyframe::{self, Keyframe};
use serde::{Deserialize, Serialize};

/// Per-channel RGB curves. Each channel is a list of control points [x, y] in 0..1.
/// Default has endpoints `[[0,0],[1,1]]` (identity).
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct CurveSet {
    #[serde(default = "default_curve_points")]
    pub master: Vec<[f32; 2]>,
    #[serde(default = "default_curve_points")]
    pub red: Vec<[f32; 2]>,
    #[serde(default = "default_curve_points")]
    pub green: Vec<[f32; 2]>,
    #[serde(default = "default_curve_points")]
    pub blue: Vec<[f32; 2]>,
}

fn default_curve_points() -> Vec<[f32; 2]> {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

impl Default for CurveSet {
    fn default() -> Self {
        Self {
            master: default_curve_points(),
            red: default_curve_points(),
            green: default_curve_points(),
            blue: default_curve_points(),
        }
    }
}

/// Six Hue/Sat curves. Each curve: Vec of [x, y] control points.
/// x = 0..1 (input range), y = 0..1 (0.5 = identity for relative curves).
/// Empty Vec = identity (no change).
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct HueSatCurves {
    #[serde(default)]
    pub hue_vs_hue: Vec<[f32; 2]>,
    #[serde(default)]
    pub hue_vs_sat: Vec<[f32; 2]>,
    #[serde(default)]
    pub hue_vs_lum: Vec<[f32; 2]>,
    #[serde(default)]
    pub lum_vs_sat: Vec<[f32; 2]>,
    #[serde(default)]
    pub sat_vs_sat: Vec<[f32; 2]>,
    #[serde(default)]
    pub orange_vs_sat: Vec<[f32; 2]>,
    /// Luminance as a function of saturation.
    ///
    /// The one direction the other six do not cover: `hue_vs_lum`
    /// moves brightness by hue and `lum_vs_sat` moves saturation by
    /// brightness, but nothing moved brightness by saturation. It is
    /// how a neon sign or a saturated sky is stopped from reading too
    /// bright, and how saturated speculars are held down in HDR.
    ///
    /// Resolve carries it among its six; this set spent that slot on
    /// `orange_vs_sat`, which is a convenience rather than a
    /// fundamental. Added rather than swapped, so nothing that used
    /// the orange curve loses it.
    #[serde(default)]
    pub sat_vs_lum: Vec<[f32; 2]>,
}

/// HSL qualifier for secondary color correction.
/// Selects a range of hue/saturation/luminance to isolate specific colors.
/// The matte (0-1 per pixel) determines how much the secondary grade is applied.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct QualifierSettings {
    pub enabled: bool,
    /// Hue center in degrees (0-360)
    pub hue_center: f32,
    /// Hue width in degrees (half-width of the selection)
    pub hue_width: f32,
    /// Hue feather (soft edge, degrees)
    pub hue_soft: f32,
    /// Saturation range (0-1)
    pub sat_low: f32,
    pub sat_high: f32,
    pub sat_soft: f32,
    /// Luminance range (0-1)
    pub lum_low: f32,
    pub lum_high: f32,
    pub lum_soft: f32,
    /// Invert the matte
    pub invert: bool,
    /// Secondary grade adjustment (simpler: just exposure, sat, hue shift)
    pub exposure: f32,
    pub saturation: f32,
    pub hue_shift: f32,
    pub temperature: f32,
    /// Matte finesse, applied after the key in this order. Clean black:
    /// matte at or below this is 0 and the rest rescaled (0..1).
    #[serde(default)]
    pub clean_black: f32,
    /// Clean white: matte at or above `1 − clean_white` is 1, the rest
    /// rescaled (0..1).
    #[serde(default)]
    pub clean_white: f32,
    /// Pixels: the radius of the median that takes the matte's specks
    /// and pinholes out, its edges kept (#901). 0 is off.
    #[serde(default)]
    pub denoise: f32,
    /// Pixels: negative shrinks the matte, positive grows it.
    #[serde(default)]
    pub shrink_grow: f32,
    /// Blur radius in pixels, after shrink/grow.
    #[serde(default)]
    pub blur: f32,
    /// What the key selects by (#895): hue, saturation and luminance, or
    /// each of red, green and blue on its own.
    #[serde(default)]
    pub mode: QualifierMode,
    /// The RGB mode's ranges, each 0-1 on the display-encoded picture:
    /// red, green and blue low and high, and the soft edge outside them.
    #[serde(default = "zero")]
    pub red_low: f32,
    #[serde(default = "one")]
    pub red_high: f32,
    #[serde(default = "zero")]
    pub green_low: f32,
    #[serde(default = "one")]
    pub green_high: f32,
    #[serde(default = "zero")]
    pub blue_low: f32,
    #[serde(default = "one")]
    pub blue_high: f32,
    #[serde(default = "tenth")]
    pub rgb_soft: f32,
    /// The 3D mode's strokes (#895): colours drawn across what to keep
    /// and what to take out, display-encoded RGB (0-1). The key is the
    /// colour volume each set spans, in a luma-chroma space.
    #[serde(default)]
    pub keep_samples: Vec<[f32; 3]>,
    #[serde(default)]
    pub remove_samples: Vec<[f32; 3]>,
    /// How far out from a volume's centre the 3D key reaches, in its own
    /// spread (standard deviations), and the soft edge beyond that.
    #[serde(default = "key_radius")]
    pub key_radius: f32,
    #[serde(default = "key_soft")]
    pub key_soft: f32,
}

fn key_radius() -> f32 {
    2.5
}
fn key_soft() -> f32 {
    1.5
}

fn zero() -> f32 {
    0.0
}
fn one() -> f32 {
    1.0
}
fn tenth() -> f32 {
    0.1
}

/// What a qualifier selects by (#895).
#[derive(PartialEq, Eq, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum QualifierMode {
    /// Hue, saturation and luminance ranges: the usual key.
    #[default]
    Hsl,
    /// Red, green and blue ranges, each on its own: for a colour HSL
    /// separates poorly.
    Rgb,
    /// A volume of colour drawn across with strokes to keep and to take
    /// out: cleanest on skin and skies.
    // Saved as its menu names it, which the generated row matches.
    #[serde(rename = "3D")]
    ThreeD,
}

impl QualifierMode {
    /// Its index as the node's `qual_mode` choice stores it.
    pub fn index(self) -> u8 {
        match self {
            QualifierMode::Hsl => 0,
            QualifierMode::Rgb => 1,
            QualifierMode::ThreeD => 2,
        }
    }

    /// The mode a `qual_mode` index names; HSL for any other.
    pub fn from_index(i: f32) -> Self {
        match i.round() as i32 {
            1 => QualifierMode::Rgb,
            2 => QualifierMode::ThreeD,
            _ => QualifierMode::Hsl,
        }
    }
}

impl QualifierSettings {
    /// Whether the matte is refined after the key: cleaned, denoised,
    /// shrunk or grown, or blurred.
    pub fn refines(&self) -> bool {
        self.clean_black != 0.0 || self.clean_white != 0.0 || self.denoise != 0.0 || self.shrink_grow != 0.0 || self.blur != 0.0
    }
}

impl Default for QualifierSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            hue_center: 0.0,
            hue_width: 30.0,
            hue_soft: 10.0,
            sat_low: 0.0,
            sat_high: 1.0,
            sat_soft: 0.1,
            lum_low: 0.0,
            lum_high: 1.0,
            lum_soft: 0.1,
            invert: false,
            exposure: 0.0,
            saturation: 1.0,
            hue_shift: 0.0,
            temperature: 0.0,
            clean_black: 0.0,
            clean_white: 0.0,
            denoise: 0.0,
            shrink_grow: 0.0,
            blur: 0.0,
            mode: QualifierMode::Hsl,
            red_low: 0.0,
            red_high: 1.0,
            green_low: 0.0,
            green_high: 1.0,
            blue_low: 0.0,
            blue_high: 1.0,
            rgb_soft: 0.1,
            keep_samples: Vec::new(),
            remove_samples: Vec::new(),
            key_radius: 2.5,
            key_soft: 1.5,
        }
    }
}

/// Standalone luminance-threshold key. Produces a matte (0–1 per
/// pixel) selecting pixels whose Rec.709 luma falls within
/// `[low, high]`, with a `soft` falloff outside the range. Simpler
/// than [`QualifierSettings`], which evaluates HSL jointly — this
/// exists for the common "key on brightness alone" workflow
/// (windowing skies, crushing blacks, isolating a bright element).
///
/// **Colorspace note:** the Rec.709 weights baked into the matte
/// generator are technically wrong for AP1 (ACEScg working
/// colorspace) — they should be approximately `0.272/0.674/0.054`
/// there. Matches [`QualifierSettings`]'s behaviour today; both will
/// be re-tuned per working_colorspace in a post-Phase-E follow-up.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct LumaKeySettings {
    pub enabled: bool,
    /// Lower luminance bound (0–1). Pixels brighter than `low` enter
    /// the matte at full strength.
    pub low: f32,
    /// Upper luminance bound (0–1). Pixels darker than `high` enter
    /// the matte at full strength.
    pub high: f32,
    /// Soft falloff distance in luminance (0–0.5 typical). Outside
    /// the `[low, high]` range, matte ramps from 1 to 0 across this
    /// width.
    pub soft: f32,
    /// Invert the matte. When true, pixels OUTSIDE the luma range
    /// become the matte instead.
    pub invert: bool,
}

impl Default for LumaKeySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            low: 0.0,
            high: 1.0,
            soft: 0.05,
            invert: false,
        }
    }
}

/// Which colour metric the difference-key uses to compare a live
/// pixel against the clean plate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum DifferenceKeyMatchSpace {
    /// Per-pixel `d = max(|R_live − R_plate|, |G_live − G_plate|,
    /// |B_live − B_plate|)` (Chebyshev / L∞ norm). Catches subjects
    /// that share luminance with the plate but differ in colour
    /// (red shirt against grey wall). More discriminating; default.
    #[default]
    Rgb,
    /// Per-pixel `d = |Y_live − Y_plate|` with Rec.709 weights.
    /// Faster, more forgiving of plate-noise chroma drift; suits
    /// fixed-camera shots where the subject is meaningfully
    /// brighter or darker than the background.
    Luminance,
}


/// Standalone difference-key matte producer. Compares each live-frame
/// pixel against a clean-plate reference image loaded from disk;
/// pixels that differ from the plate by more than `tolerance` enter
/// the matte. Useful for fixed-camera composites where you have a
/// "background only" take available.
///
/// **Colorspace note:** the Rec.709 weights baked into the luminance
/// match-space are technically wrong for AP1 (ACEScg working
/// colorspace) — they should be approximately `0.272/0.674/0.054`
/// there. Matches [`LumaKeySettings`]'s and [`QualifierSettings`]'s
/// behaviour today; both will be re-tuned per working_colorspace in a
/// post-Phase-E follow-up.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct DifferenceKeySettings {
    pub enabled: bool,
    /// Path to the clean-plate image on disk. `None`, or a plate that will
    /// not load: the matte selects everything, as every matte producer
    /// does with nothing chosen (#1039), so a freshly added node changes
    /// nothing before the user picks a plate. As `ExternalMatte` with no
    /// image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate_path: Option<std::path::PathBuf>,
    /// The clean plate as a frame of the clip's own media (#2758): its
    /// time in source seconds, the media's own, so a trim leaves it where
    /// it is. Set, it is the plate, before `plate_path`. A time that
    /// isn't finite or lies outside the media gives no plate, said; it is
    /// never moved to the nearest frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate_frame: Option<f64>,
    /// Distance threshold in normalised colour space (0–1). Pixels
    /// whose match-space distance from the plate is >= `tolerance`
    /// enter the matte at full strength; pixels with distance 0
    /// (perfect match) are excluded.
    pub tolerance: f32,
    /// Soft falloff width (0–0.5 typical). Distances in
    /// `[tolerance, tolerance + soft]` ramp from 0 → 1 for a clean
    /// edge instead of a hard binary cut.
    pub soft: f32,
    /// Invert the matte. When true, pixels matching the plate
    /// (distance below `tolerance`) become the matte instead.
    pub invert: bool,
    /// Which distance metric to compare against the plate.
    #[serde(default)]
    pub match_space: DifferenceKeyMatchSpace,
}

impl DifferenceKeySettings {
    /// Whether a plate is chosen: a frame of the clip's, or a file.
    pub fn has_plate(&self) -> bool {
        self.plate_frame.is_some() || self.plate_path.is_some()
    }
}

impl Default for DifferenceKeySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            plate_path: None,
            plate_frame: None,
            tolerance: 0.05,
            soft: 0.02,
            invert: false,
            match_space: DifferenceKeyMatchSpace::Rgb,
        }
    }
}

/// Standalone soft-clip / hard-clip transform. Rolls off highlights
/// above `threshold` so they asymptotically approach 1.0 instead of
/// clipping abruptly. Common in delivery prep — broadcast targets
/// often want highlights compressed below 100 % so they don't trip
/// downstream limiters or cause banding at the cap.
///
/// **Curve**: Reinhard-style exponential rolloff per RGB channel:
///
/// ```text
/// y = x                                              if x <= threshold
/// y = threshold                                      if knee == 0   (hard clip)
/// y = threshold + (1 - threshold) * (1 - e^(-excess / knee))   otherwise
/// ```
///
/// where `excess = x - threshold`. The curve is monotonic,
/// asymptotic to 1.0, and tangent to passthrough at the threshold
/// point — no kink at the rolloff start.
///
/// **Per-channel application** is intentional: slight desaturation
/// at extreme highlights is the filmic look. A hue-preserving
/// variant (scale the entire RGB vector) is a possible follow-up
/// but not v1.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct SoftClipSettings {
    pub enabled: bool,
    /// Point at which rolloff starts (0..1). Values below this pass
    /// through untouched. Typical broadcast: 0.90–0.95.
    pub threshold: f32,
    /// Width of the rolloff curve (0..0.5). `0` = hard clip at
    /// threshold; larger values = smoother / more compressed
    /// highlights. Typical broadcast: 0.05.
    pub knee: f32,
}

impl Default for SoftClipSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 0.95,
            knee: 0.05,
        }
    }
}

/// HDR zones (#240): exposure, colour and saturation by tonal zone, each
/// zone's reach set in stops about mid grey. Seven zones, in order:
/// Global, Black, Dark, Shadow, Light, Highlight, Specular
/// (`klipp_grade_model::hdr_zones`, which holds the maths). Global's
/// boundary and falloff are unused: it reaches everything.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HdrZonesSettings {
    /// Each zone's exposure, in stops.
    pub exposure: [f32; 7],
    /// Each zone's colour, as its wheel's puck (x, y): its angle the hue,
    /// its distance how far apart the channels move, as a Lift or Gain
    /// wheel's (`klipp_color::wheels::wheel_offset_to_rgb`), in stops. The
    /// balance never changes brightness.
    pub colour_x: [f32; 7],
    pub colour_y: [f32; 7],
    /// Each zone's saturation, about the pixel's luminance (1: none).
    pub saturation: [f32; 7],
    /// Each zone's boundary, in stops from mid grey.
    pub boundary: [f32; 7],
    /// Each zone's falloff width, in stops (0: a hard edge).
    pub falloff: [f32; 7],
}

impl HdrZonesSettings {
    /// The zones' boundaries as a new node has them, in stops, for a
    /// scene-referred source (HDR or camera log), whose highlights run on
    /// past display white.
    pub const DEFAULT_BOUNDARY: [f32; 7] = [0.0, -4.5, -2.5, -1.0, 1.0, 2.5, 4.5];
    /// The same for an SDR source (#2767), whose white is about +2.5
    /// stops: Light, Highlight and Specular inside its range, from about
    /// 25%, 51% and 83% of white in linear light. The lower zones are as
    /// above, well inside both.
    pub const SDR_BOUNDARY: [f32; 7] = [0.0, -4.5, -2.5, -1.0, 0.5, 1.5, 2.2];
    /// A new zone's falloff, in stops.
    pub const DEFAULT_FALLOFF: f32 = 1.0;

    /// Whether the zones change no pixel: every exposure and colour 0,
    /// every saturation 1. The boundaries place nothing then.
    pub fn is_neutral(&self) -> bool {
        [&self.exposure, &self.colour_x, &self.colour_y].iter().all(|a| a.iter().all(|v| *v == 0.0)) && self.saturation.iter().all(|v| *v == 1.0)
    }
}

impl HdrZonesSettings {
    /// The zones a new or reset zones node gets for its source (#2767):
    /// neutral, with boundaries where that source's tones are, so on an
    /// SDR picture "highlight" is its bright part rather than above its
    /// white. The one place this is chosen: the colour page's zones, their
    /// presets and the `zones` op all ask it. Stored zones keep theirs.
    pub fn for_source(scene_referred: bool) -> Self {
        Self { boundary: Self::boundary_for(scene_referred), ..Self::default() }
    }

    /// The boundaries [`Self::for_source`] places.
    pub fn boundary_for(scene_referred: bool) -> [f32; 7] {
        if scene_referred { Self::DEFAULT_BOUNDARY } else { Self::SDR_BOUNDARY }
    }

    /// A zone colour wheel's puck for a tint of hue `hue` (degrees on the
    /// wheel: 0 red, 30 orange, 210 a cool blue) and `amount` (0 none, 1
    /// the wheel's rim).
    pub fn colour_puck(hue: f32, amount: f32) -> (f32, f32) {
        let dist = 2.0 * amount;
        let (s, c) = hue.to_radians().sin_cos();
        (dist * c, dist * s)
    }

    /// The zones' starting points (#240), as a colourist names them: each
    /// its name, what it does, and its settings, its boundaries those of
    /// the source ([`Self::for_source`]).
    pub fn presets(scene_referred: bool) -> Vec<(&'static str, &'static str, HdrZonesSettings)> {
        let with = |f: &dyn Fn(&mut HdrZonesSettings)| {
            let mut z = HdrZonesSettings::for_source(scene_referred);
            f(&mut z);
            z
        };
        let tint = |z: &mut HdrZonesSettings, zone: usize, hue: f32, amount: f32| {
            (z.colour_x[zone], z.colour_y[zone]) = Self::colour_puck(hue, amount);
        };
        vec![
            ("HDR highlight roll-off", "Brings the highlights down half a stop and the speculars a stop, and calms their colour.", with(&|z| {
                z.exposure[5] = -0.5;
                z.exposure[6] = -1.0;
                z.saturation[6] = 0.8;
            })),
            ("Lift shadows", "Opens the shadows and the darks without touching black.", with(&|z| {
                z.exposure[3] = 0.4;
                z.exposure[2] = 0.3;
            })),
            ("Deepen blacks", "Sets the blacks down and the darks with them, for a denser floor.", with(&|z| {
                z.exposure[1] = -0.6;
                z.exposure[2] = -0.2;
            })),
            ("Midtone contrast", "Pushes the shadows down and the lights up about mid grey, black and white left where they are.", with(&|z| {
                z.exposure[3] = -0.3;
                z.exposure[4] = 0.3;
            })),
            ("Warm highlights, cool shadows", "A split tone: orange in the highlights, a cool blue in the shadows.", with(&|z| {
                tint(z, 5, 30.0, 0.25);
                tint(z, 3, 210.0, 0.25);
            })),
        ]
    }

    /// The preset named `name` (any case), when there is one.
    pub fn preset(name: &str, scene_referred: bool) -> Option<HdrZonesSettings> {
        Self::presets(scene_referred).into_iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name.trim())).map(|(_, _, z)| z)
    }
}

impl Default for HdrZonesSettings {
    fn default() -> Self {
        Self {
            exposure: [0.0; 7],
            colour_x: [0.0; 7],
            colour_y: [0.0; 7],
            saturation: [1.0; 7],
            boundary: Self::DEFAULT_BOUNDARY,
            falloff: [Self::DEFAULT_FALLOFF; 7],
        }
    }
}

/// Shape-based mask for regional color grading (power windows).
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub enum MaskShape {
    Circle {
        center_x: f32, // normalized 0-1
        center_y: f32,
        radius: f32,   // normalized to frame width
    },
    Rectangle {
        center_x: f32,
        center_y: f32,
        width: f32,
        height: f32,
        rotation: f32, // degrees
    },
    LinearGradient {
        start_x: f32,
        start_y: f32,
        end_x: f32,
        end_y: f32,
    },
    /// A round gradient (#2704): whole inside `inner` of the centre, gone
    /// past `outer`, the two radii in frame widths as a circle's is.
    RadialGradient {
        center_x: f32,
        center_y: f32,
        inner: f32,
        outer: f32,
    },
    /// Freeform path mask (#74): points in frame fractions, each with
    /// curve handles, the path closed (the last point joins the first).
    /// A point without handles is a corner; a path of corners is a
    /// polygon. Filled as its outline, flattened at the render's size
    /// ([`crate::mask_path::outline`]).
    Bezier {
        points: Vec<crate::mask_path::BezierPoint>,
    },
}

impl MaskShape {
    /// The freeform shape with corners at `points`: a polygon.
    pub fn polygon(points: impl IntoIterator<Item = (f32, f32)>) -> MaskShape {
        MaskShape::Bezier { points: points.into_iter().map(crate::mask_path::BezierPoint::from).collect() }
    }
}

/// How a mask joins the ones before it in its stack (#74). The stack's
/// matte starts empty, or, when its first mask isn't added, as the whole
/// frame, so a stack that starts by subtracting cuts a hole in
/// everything.
#[derive(PartialEq, Eq, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum MaskCombine {
    /// The union: the more covered of the two.
    #[default]
    Add,
    /// Takes this mask out of what is there.
    Subtract,
    /// Keeps only what both cover.
    Intersect,
}

impl MaskCombine {
    pub fn is_add(&self) -> bool {
        *self == MaskCombine::Add
    }

    /// The matte where `before` was and this mask covers `m`.
    pub fn apply(self, before: f32, m: f32) -> f32 {
        match self {
            MaskCombine::Add => before.max(m),
            MaskCombine::Subtract => before.min(1.0 - m),
            MaskCombine::Intersect => before.min(m),
        }
    }

    /// Its number in a GPU mask record's flags (bits 2-3).
    pub fn code(self) -> u32 {
        match self {
            MaskCombine::Add => 0,
            MaskCombine::Subtract => 1,
            MaskCombine::Intersect => 2,
        }
    }
}

/// How covered a pixel `dist` from a mask's edge is (negative inside), its
/// soft edge running from `inner` inside the line to `outer` outside it
/// (#74), on a smooth step. With no inner feather, as masks always were:
/// all inside the line, falling off outside it.
pub fn soft_edge(dist: f32, inner: f32, outer: f32) -> f32 {
    let (inner, outer) = (inner.max(0.0), outer.max(0.0));
    let span = inner + outer;
    if dist <= -inner {
        1.0
    } else if span > 0.0001 && dist < outer {
        let t = (dist + inner) / span;
        1.0 - t * t * (3.0 - 2.0 * t)
    } else {
        0.0
    }
}

/// A window's tracked motion at one moment (#238): its pivot moved by
/// `dx`, `dy` (frame fractions), and the window scaled by `scale` and
/// turned `rotation` degrees clockwise about it, on the picture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowMotion {
    pub dx: f32,
    pub dy: f32,
    pub scale: f32,
    pub rotation: f32,
}

impl WindowMotion {
    /// Where the point `p` (frame fractions) on the moved window was on
    /// the window before the motion, about `pivot`, on a frame `aspect`
    /// tall: the inverse of what [`MaskShape::moved`] does to a point.
    pub fn undo(self, p: (f32, f32), pivot: (f32, f32), aspect: f32) -> (f32, f32) {
        let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 9.0 / 16.0 };
        let scale = if self.scale.abs() > 1e-6 { self.scale } else { 1.0 };
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        let (ux, uy) = (p.0 - pivot.0 - self.dx, (p.1 - pivot.1 - self.dy) * aspect);
        let (rx, ry) = ((ux * cos + uy * sin) / scale, (-ux * sin + uy * cos) / scale);
        (pivot.0 + rx, pivot.1 + ry / aspect)
    }
}

impl MaskShape {
    /// The point the window tracker moves the shape by and turns and
    /// scales it about: a circle's or rectangle's centre, a gradient's
    /// midpoint, a free shape's centroid.
    pub fn pivot(&self) -> (f32, f32) {
        match self {
            MaskShape::Circle { center_x, center_y, .. }
            | MaskShape::Rectangle { center_x, center_y, .. }
            | MaskShape::RadialGradient { center_x, center_y, .. } => (*center_x, *center_y),
            MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => ((start_x + end_x) * 0.5, (start_y + end_y) * 0.5),
            MaskShape::Bezier { points } if !points.is_empty() => {
                let n = points.len() as f32;
                (points.iter().map(|p| p.x).sum::<f32>() / n, points.iter().map(|p| p.y).sum::<f32>() / n)
            }
            MaskShape::Bezier { .. } => (0.5, 0.5),
        }
    }

    /// This shape moved by `m` on a frame `aspect` (height over width)
    /// tall. Points — a gradient's ends, a free shape's corners — move as
    /// the picture does, the turn taken in pixels, so a turn does not
    /// shear them on a wide frame. A circle's radius and a rectangle's
    /// size scale; a rectangle's own angle takes the turn.
    pub fn moved(&self, m: WindowMotion, aspect: f32) -> MaskShape {
        let (px, py) = self.pivot();
        let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 9.0 / 16.0 };
        let (sin, cos) = m.rotation.to_radians().sin_cos();
        // In frame-width units both ways, where a turn is a turn.
        let point = |x: f32, y: f32| -> (f32, f32) {
            let (ux, uy) = (x - px, (y - py) * aspect);
            let (rx, ry) = (m.scale * (ux * cos - uy * sin), m.scale * (ux * sin + uy * cos));
            (px + m.dx + rx, py + m.dy + ry / aspect)
        };
        match self {
            MaskShape::Circle { center_x, center_y, radius } => {
                MaskShape::Circle { center_x: center_x + m.dx, center_y: center_y + m.dy, radius: radius * m.scale }
            }
            MaskShape::Rectangle { center_x, center_y, width, height, rotation } => MaskShape::Rectangle {
                center_x: center_x + m.dx,
                center_y: center_y + m.dy,
                width: width * m.scale,
                height: height * m.scale,
                rotation: rotation + m.rotation,
            },
            MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
                let (sx, sy) = point(*start_x, *start_y);
                let (ex, ey) = point(*end_x, *end_y);
                MaskShape::LinearGradient { start_x: sx, start_y: sy, end_x: ex, end_y: ey }
            }
            MaskShape::RadialGradient { center_x, center_y, inner, outer } => MaskShape::RadialGradient {
                center_x: center_x + m.dx,
                center_y: center_y + m.dy,
                inner: inner * m.scale,
                outer: outer * m.scale,
            },
            // Each point moves; its handles turn and scale with it, as
            // offsets.
            MaskShape::Bezier { points } => MaskShape::Bezier {
                points: points
                    .iter()
                    .map(|p| {
                        let (x, y) = point(p.x, p.y);
                        let offset = |dx: f32, dy: f32| {
                            let (ux, uy) = (dx, dy * aspect);
                            (m.scale * (ux * cos - uy * sin), m.scale * (ux * sin + uy * cos) / aspect)
                        };
                        let (in_dx, in_dy) = offset(p.in_dx, p.in_dy);
                        let (out_dx, out_dy) = offset(p.out_dx, p.out_dy);
                        crate::mask_path::BezierPoint { x, y, in_dx, in_dy, out_dx, out_dy, smooth: p.smooth }
                    })
                    .collect(),
            },
        }
    }
}

/// A plane projective map in frame fractions (#238), row by row, 3×3:
/// how a window tracked in perspective is carried from the frame it was
/// drawn on to another. Oriented so a point in front of the eye has a
/// positive third coordinate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneMap(pub [f32; 9]);

impl PlaneMap {
    /// Where `p` goes, or `None` when it lies on or past the map's
    /// horizon, where the picture has nothing.
    pub fn apply(&self, p: (f32, f32)) -> Option<(f32, f32)> {
        let h = &self.0;
        let w = h[6] * p.0 + h[7] * p.1 + h[8];
        if w.is_nan() || w <= 1e-9 {
            return None;
        }
        Some(((h[0] * p.0 + h[1] * p.1 + h[2]) / w, (h[3] * p.0 + h[4] * p.1 + h[5]) / w))
    }

    /// The map back, keeping what is in front in front.
    pub fn inverse(&self) -> Option<PlaneMap> {
        let m: [f64; 9] = self.0.map(|v| v as f64);
        invert3(&m).map(|i| PlaneMap(i.map(|v| v as f32)))
    }

    /// The map taking the quadrilateral `from` (four corners in order) to
    /// `to`. `None` when either is no quadrilateral — three corners in a
    /// line, two the same.
    pub fn from_quads(from: &[[f32; 2]; 4], to: &[[f32; 2]; 4]) -> Option<PlaneMap> {
        let s = square_to_quad(from)?;
        let t = square_to_quad(to)?;
        let mut h = mul3(&t, &invert3(&s)?);
        // Up to its sign: the one with `from` in front.
        let (cx, cy) = from.iter().fold((0.0, 0.0), |a, p| (a.0 + p[0] as f64 / 4.0, a.1 + p[1] as f64 / 4.0));
        if h[6] * cx + h[7] * cy + h[8] < 0.0 {
            h = h.map(|v| -v);
        }
        let norm = h.iter().map(|v| v * v).sum::<f64>().sqrt();
        (norm.is_finite() && norm > 0.0).then(|| PlaneMap(h.map(|v| (v / norm) as f32)))
    }
}

/// The map from the unit square — (0,0) (1,0) (1,1) (0,1) — to four
/// corners, in closed form (Heckbert).
fn square_to_quad(q: &[[f32; 2]; 4]) -> Option<[f64; 9]> {
    let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = q.map(|p| (p[0] as f64, p[1] as f64));
    let (sx, sy) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
    let (dx1, dy1, dx2, dy2) = (x1 - x2, y1 - y2, x3 - x2, y3 - y2);
    let det = dx1 * dy2 - dx2 * dy1;
    if det.abs() < 1e-12 {
        return None;
    }
    let g = (sx * dy2 - dx2 * sy) / det;
    let h = (dx1 * sy - sx * dy1) / det;
    Some([x1 - x0 + g * x1, x3 - x0 + h * x3, x0, y1 - y0 + g * y1, y3 - y0 + h * y3, y0, g, h, 1.0])
}

fn mul3(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    std::array::from_fn(|k| {
        let (i, j) = (k / 3, k % 3);
        a[i * 3] * b[j] + a[i * 3 + 1] * b[3 + j] + a[i * 3 + 2] * b[6 + j]
    })
}

/// The true inverse, not the adjugate: dividing by a negative
/// determinant keeps a point's side of the horizon.
fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let c = [
        m[4] * m[8] - m[5] * m[7], m[2] * m[7] - m[1] * m[8], m[1] * m[5] - m[2] * m[4],
        m[5] * m[6] - m[3] * m[8], m[0] * m[8] - m[2] * m[6], m[2] * m[3] - m[0] * m[5],
        m[3] * m[7] - m[4] * m[6], m[1] * m[6] - m[0] * m[7], m[0] * m[4] - m[1] * m[3],
    ];
    let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
    (det.is_finite() && det.abs() > 1e-18).then(|| c.map(|v| v / det))
}

/// Where a window tracked in perspective (#238) has its corners at one
/// moment, in frame fractions, in the order of [`PerspectiveTrack::from`].
#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CornerKey {
    pub time_secs: f64,
    pub corners: [[f32; 2]; 4],
}

/// A window tracked in perspective (#238): the four corners of the
/// region followed, on the frame the window was drawn on, and where the
/// tracker found them since. The window is drawn as it was on that
/// frame and carried by the plane map from those corners to these, so
/// a window on a screen or a sign turns away with it.
#[derive(PartialEq, Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerspectiveTrack {
    /// Top left, top right, bottom right, bottom left.
    pub from: [[f32; 2]; 4],
    /// Earliest first.
    pub keys: Vec<CornerKey>,
}

impl PerspectiveTrack {
    /// The corners at `time_secs`: between two keys, each corner on the
    /// line between them; before the first or after the last, held.
    pub fn corners_at(&self, time_secs: f64) -> Option<[[f32; 2]; 4]> {
        let (first, last) = (self.keys.first()?, self.keys.last()?);
        if time_secs <= first.time_secs {
            return Some(first.corners);
        }
        if time_secs >= last.time_secs {
            return Some(last.corners);
        }
        let i = self.keys.partition_point(|k| k.time_secs <= time_secs);
        let (a, b) = (&self.keys[i - 1], &self.keys[i]);
        let span = b.time_secs - a.time_secs;
        let t = if span > 0.0 { ((time_secs - a.time_secs) / span) as f32 } else { 0.0 };
        Some(std::array::from_fn(|c| [a.corners[c][0] + (b.corners[c][0] - a.corners[c][0]) * t, a.corners[c][1] + (b.corners[c][1] - a.corners[c][1]) * t]))
    }

    /// The map from the frame the window was drawn on to `time_secs`.
    pub fn map_at(&self, time_secs: f64) -> Option<PlaneMap> {
        PlaneMap::from_quads(&self.from, &self.corners_at(time_secs)?)
    }
}

/// Keyframe tracks for animating mask parameters over time.
/// Each `Vec<Keyframe>` is empty by default (static mask). When non-empty,
/// the keyframes override the corresponding static parameter at render time.
#[derive(PartialEq, Debug, Clone, Default, Serialize, Deserialize)]
pub struct MaskKeyframes {
    #[serde(default)]
    pub center_x: Vec<Keyframe>,
    #[serde(default)]
    pub center_y: Vec<Keyframe>,
    #[serde(default)]
    pub radius: Vec<Keyframe>,
    #[serde(default)]
    pub width: Vec<Keyframe>,
    #[serde(default)]
    pub height: Vec<Keyframe>,
    #[serde(default)]
    pub rotation: Vec<Keyframe>,
    #[serde(default)]
    pub feather: Vec<Keyframe>,
    #[serde(default)]
    pub opacity: Vec<Keyframe>,
    /// LinearGradient start_x
    #[serde(default)]
    pub start_x: Vec<Keyframe>,
    /// LinearGradient start_y
    #[serde(default)]
    pub start_y: Vec<Keyframe>,
    /// LinearGradient end_x
    #[serde(default)]
    pub end_x: Vec<Keyframe>,
    /// LinearGradient end_y
    #[serde(default)]
    pub end_y: Vec<Keyframe>,
    /// RadialGradient inner (#2704)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inner: Vec<Keyframe>,
    /// RadialGradient outer (#2704)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outer: Vec<Keyframe>,
    /// The window's tracked motion (#238), written by the window tracker:
    /// how far its pivot (see [`MaskShape::pivot`]) has moved, in frame
    /// fractions, and how much it has grown and turned, about that pivot,
    /// on the picture itself. Applied to any shape after its own
    /// keyframes, so a window can be tracked and still be reshaped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_x: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_y: Vec<Keyframe>,
    /// A factor: 1 is the size the window was drawn at.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_scale: Vec<Keyframe>,
    /// Degrees, clockwise on the picture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_rotation: Vec<Keyframe>,
    /// The window tracked in perspective instead (#238).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_perspective: Option<PerspectiveTrack>,
    /// A freeform mask's shape keyed over time (#74), earliest first:
    /// each key holds every point, as many as the shape has. Applied
    /// before the tracked motion, as the other keys are.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<crate::mask_path::PathKey>,
    /// On an evaluated window only (see [`Mask::evaluated_in`]), never
    /// saved: the map from a point on the frame now to where it was on
    /// the frame the window was drawn on, where the window's shape is
    /// measured. `None` for a window not tracked in perspective.
    #[serde(skip)]
    pub warp: Option<PlaneMap>,
}

impl MaskKeyframes {
    /// Returns true if no keyframe tracks have any keyframes.
    pub fn is_empty(&self) -> bool {
        self.center_x.is_empty()
            && self.center_y.is_empty()
            && self.radius.is_empty()
            && self.width.is_empty()
            && self.height.is_empty()
            && self.rotation.is_empty()
            && self.feather.is_empty()
            && self.opacity.is_empty()
            && self.start_x.is_empty()
            && self.start_y.is_empty()
            && self.end_x.is_empty()
            && self.end_y.is_empty()
            && self.inner.is_empty()
            && self.outer.is_empty()
            && self.path.is_empty()
            && !self.is_tracked()
    }

    /// Whether the window tracker drives this window (#238).
    pub fn is_tracked(&self) -> bool {
        !(self.track_x.is_empty() && self.track_y.is_empty() && self.track_scale.is_empty() && self.track_rotation.is_empty())
            || self.track_perspective.is_some()
    }

    /// Take the tracked motion off.
    pub fn clear_track(&mut self) {
        self.track_x.clear();
        self.track_y.clear();
        self.track_scale.clear();
        self.track_rotation.clear();
        self.track_perspective = None;
    }

    /// Get a mutable reference to the keyframe track for a given parameter name.
    pub fn track_mut(&mut self, param: &str) -> Option<&mut Vec<Keyframe>> {
        match param {
            "center_x" => Some(&mut self.center_x),
            "center_y" => Some(&mut self.center_y),
            "radius" => Some(&mut self.radius),
            "width" => Some(&mut self.width),
            "height" => Some(&mut self.height),
            "rotation" => Some(&mut self.rotation),
            "feather" => Some(&mut self.feather),
            "opacity" => Some(&mut self.opacity),
            "start_x" => Some(&mut self.start_x),
            "start_y" => Some(&mut self.start_y),
            "end_x" => Some(&mut self.end_x),
            "end_y" => Some(&mut self.end_y),
            "inner" => Some(&mut self.inner),
            "outer" => Some(&mut self.outer),
            "track_x" => Some(&mut self.track_x),
            "track_y" => Some(&mut self.track_y),
            "track_scale" => Some(&mut self.track_scale),
            "track_rotation" => Some(&mut self.track_rotation),
            _ => None,
        }
    }

    /// Get an immutable reference to the keyframe track for a given parameter name.
    pub fn track(&self, param: &str) -> Option<&Vec<Keyframe>> {
        match param {
            "center_x" => Some(&self.center_x),
            "center_y" => Some(&self.center_y),
            "radius" => Some(&self.radius),
            "width" => Some(&self.width),
            "height" => Some(&self.height),
            "rotation" => Some(&self.rotation),
            "feather" => Some(&self.feather),
            "opacity" => Some(&self.opacity),
            "start_x" => Some(&self.start_x),
            "start_y" => Some(&self.start_y),
            "end_x" => Some(&self.end_x),
            "end_y" => Some(&self.end_y),
            "inner" => Some(&self.inner),
            "outer" => Some(&self.outer),
            "track_x" => Some(&self.track_x),
            "track_y" => Some(&self.track_y),
            "track_scale" => Some(&self.track_scale),
            "track_rotation" => Some(&self.track_rotation),
            _ => None,
        }
    }

    /// List of animatable parameter names for a given mask shape variant.
    pub fn params_for_shape(shape: &MaskShape) -> &'static [&'static str] {
        match shape {
            MaskShape::Circle { .. } => &["center_x", "center_y", "radius", "feather", "opacity"],
            MaskShape::Rectangle { .. } => &["center_x", "center_y", "width", "height", "rotation", "feather", "opacity"],
            MaskShape::LinearGradient { .. } => &["start_x", "start_y", "end_x", "end_y", "feather", "opacity"],
            MaskShape::RadialGradient { .. } => &["center_x", "center_y", "inner", "outer", "feather", "opacity"],
            MaskShape::Bezier { .. } => &["feather", "opacity"],
        }
    }
}

/// A single mask with shape, feathering, and blend controls.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct Mask {
    pub shape: MaskShape,
    pub feather: f32,   // 0-1, soft edge width outside the line
    pub invert: bool,
    pub opacity: f32,   // 0-1
    pub enabled: bool,
    #[serde(default)]
    pub keyframes: MaskKeyframes,
    /// The soft edge's width inside the line (#74), in frame widths as
    /// `feather` is: none, as masks always were, keeps the inside whole.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub feather_inner: f32,
    /// How it joins the masks before it (#74).
    #[serde(default, skip_serializing_if = "MaskCombine::is_add")]
    pub combine: MaskCombine,
    /// Its name in the mask list; empty shows "Mask n".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Kept from edits in the viewer (#74).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 },
            feather: 0.1,
            invert: false,
            opacity: 1.0,
            enabled: true,
            keyframes: MaskKeyframes::default(),
            feather_inner: 0.0,
            combine: MaskCombine::Add,
            name: String::new(),
            locked: false,
        }
    }
}

impl Mask {
    /// A mask of `shape` as it is added: a radial gradient fading over
    /// its whole ring, inner to outer (#2704); any other shape as
    /// [`Mask::default`] has it.
    pub fn new(shape: MaskShape) -> Self {
        let ring = matches!(shape, MaskShape::RadialGradient { .. });
        Mask { shape, feather: if ring { 0.5 } else { Mask::default().feather }, feather_inner: if ring { 0.5 } else { 0.0 }, ..Mask::default() }
    }

    /// Evaluate keyframes at the given time and return a copy of this mask
    /// with animated parameters overridden.
    pub fn evaluated_at(&self, time_secs: f64) -> Mask {
        if self.keyframes.is_empty() {
            return self.clone();
        }

        let mut result = self.clone();

        // Evaluate feather and opacity (common to all shapes)
        if !self.keyframes.feather.is_empty() {
            result.feather = keyframe::evaluate(&self.keyframes.feather, time_secs, self.feather);
        }
        if !self.keyframes.opacity.is_empty() {
            result.opacity = keyframe::evaluate(&self.keyframes.opacity, time_secs, self.opacity);
        }

        // Evaluate shape-specific parameters
        match &self.shape {
            MaskShape::Circle { center_x, center_y, radius } => {
                let cx = if !self.keyframes.center_x.is_empty() {
                    keyframe::evaluate(&self.keyframes.center_x, time_secs, *center_x)
                } else { *center_x };
                let cy = if !self.keyframes.center_y.is_empty() {
                    keyframe::evaluate(&self.keyframes.center_y, time_secs, *center_y)
                } else { *center_y };
                let r = if !self.keyframes.radius.is_empty() {
                    keyframe::evaluate(&self.keyframes.radius, time_secs, *radius)
                } else { *radius };
                result.shape = MaskShape::Circle { center_x: cx, center_y: cy, radius: r };
            }
            MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
                let cx = if !self.keyframes.center_x.is_empty() {
                    keyframe::evaluate(&self.keyframes.center_x, time_secs, *center_x)
                } else { *center_x };
                let cy = if !self.keyframes.center_y.is_empty() {
                    keyframe::evaluate(&self.keyframes.center_y, time_secs, *center_y)
                } else { *center_y };
                let w = if !self.keyframes.width.is_empty() {
                    keyframe::evaluate(&self.keyframes.width, time_secs, *width)
                } else { *width };
                let h = if !self.keyframes.height.is_empty() {
                    keyframe::evaluate(&self.keyframes.height, time_secs, *height)
                } else { *height };
                let rot = if !self.keyframes.rotation.is_empty() {
                    keyframe::evaluate(&self.keyframes.rotation, time_secs, *rotation)
                } else { *rotation };
                result.shape = MaskShape::Rectangle { center_x: cx, center_y: cy, width: w, height: h, rotation: rot };
            }
            MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
                let sx = if !self.keyframes.start_x.is_empty() {
                    keyframe::evaluate(&self.keyframes.start_x, time_secs, *start_x)
                } else { *start_x };
                let sy = if !self.keyframes.start_y.is_empty() {
                    keyframe::evaluate(&self.keyframes.start_y, time_secs, *start_y)
                } else { *start_y };
                let ex = if !self.keyframes.end_x.is_empty() {
                    keyframe::evaluate(&self.keyframes.end_x, time_secs, *end_x)
                } else { *end_x };
                let ey = if !self.keyframes.end_y.is_empty() {
                    keyframe::evaluate(&self.keyframes.end_y, time_secs, *end_y)
                } else { *end_y };
                result.shape = MaskShape::LinearGradient { start_x: sx, start_y: sy, end_x: ex, end_y: ey };
            }
            MaskShape::RadialGradient { center_x, center_y, inner, outer } => {
                let at = |track: &Vec<Keyframe>, v: f32| if track.is_empty() { v } else { keyframe::evaluate(track, time_secs, v) };
                result.shape = MaskShape::RadialGradient {
                    center_x: at(&self.keyframes.center_x, *center_x),
                    center_y: at(&self.keyframes.center_y, *center_y),
                    inner: at(&self.keyframes.inner, *inner),
                    outer: at(&self.keyframes.outer, *outer),
                };
            }
            // Its shape keys (#74).
            MaskShape::Bezier { .. } => {
                if let Some(points) = crate::mask_path::shape_at(&self.keyframes.path, time_secs) {
                    result.shape = MaskShape::Bezier { points };
                }
            }
        }

        result
    }

    /// A freeform mask's points as its shape keys put them at
    /// `time_secs` (#74), else as drawn; `None` for another shape.
    pub fn path_at(&self, time_secs: f64) -> Option<Vec<crate::mask_path::BezierPoint>> {
        let MaskShape::Bezier { points } = &self.shape else { return None };
        Some(crate::mask_path::shape_at(&self.keyframes.path, time_secs).unwrap_or_else(|| points.clone()))
    }

    /// This mask with its freeform shape at `time_secs` set to `points`:
    /// the shape key there (within `tolerance`) or a new one when the
    /// shape is keyed, else the shape itself.
    pub fn with_path_at(&self, time_secs: f64, points: Vec<crate::mask_path::BezierPoint>, tolerance: f64) -> Mask {
        let mut m = self.clone();
        if m.keyframes.path.is_empty() {
            m.shape = MaskShape::Bezier { points };
        } else {
            m.keyframes.path = crate::mask_path::with_key(&m.keyframes.path, time_secs, points, tolerance);
        }
        m
    }

    /// This mask with a point added on segment `seg` at `along` in its
    /// shape and in every shape key alike (#74), so each key keeps as
    /// many points as the others, and its shape.
    pub fn with_point_split(&self, seg: usize, along: f32) -> Mask {
        let mut m = self.clone();
        if let MaskShape::Bezier { points } = &m.shape {
            m.shape = MaskShape::Bezier { points: crate::mask_path::split_segment(points, seg, along) };
        }
        for k in &mut m.keyframes.path {
            k.points = crate::mask_path::split_segment(&k.points, seg, along);
        }
        m
    }

    /// This mask less the points in `remove`, from its shape and every
    /// shape key alike; `None` when fewer than three would be left.
    pub fn without_path_points(&self, remove: &std::collections::BTreeSet<usize>) -> Option<Mask> {
        let mut m = self.clone();
        if let MaskShape::Bezier { points } = &m.shape {
            m.shape = MaskShape::Bezier { points: crate::mask_path::without_points(points, remove)? };
        }
        for k in &mut m.keyframes.path {
            k.points = crate::mask_path::without_points(&k.points, remove)?;
        }
        Some(m)
    }

    /// This mask's freeform shape keyed from `time_secs` on (a first key
    /// there, of the shape as drawn), or no longer keyed (the shape as it
    /// is at `time_secs` kept, the keys dropped).
    pub fn with_path_keyed(&self, keyed: bool, time_secs: f64) -> Mask {
        let mut m = self.clone();
        let Some(now) = self.path_at(time_secs) else { return m };
        if keyed {
            if m.keyframes.path.is_empty() {
                m.keyframes.path = vec![crate::mask_path::PathKey { time_secs, points: now, ease: crate::mask_path::PathEase::Linear }];
            }
        } else {
            m.shape = MaskShape::Bezier { points: now };
            m.keyframes.path.clear();
        }
        m
    }

    /// The name a freeform mask's shape keys go by among its tracks
    /// (#1947): beside "feather", "radius" and the rest.
    pub const SHAPE_TRACK: &'static str = "shape";

    /// This mask's tracks that hold keys, in the order its list shows
    /// them, the shape's last (#1947).
    pub fn keyed_tracks(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = MaskKeyframes::params_for_shape(&self.shape).iter().copied().filter(|p| self.keyframes.track(p).is_some_and(|t| !t.is_empty())).collect();
        if !self.keyframes.path.is_empty() {
            out.push(Self::SHAPE_TRACK);
        }
        out
    }

    /// Its tracks that can be keyed and hold no keys yet.
    pub fn unkeyed_tracks(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = MaskKeyframes::params_for_shape(&self.shape).iter().copied().filter(|p| self.keyframes.track(p).is_none_or(|t| t.is_empty())).collect();
        if matches!(self.shape, MaskShape::Bezier { .. }) && self.keyframes.path.is_empty() {
            out.push(Self::SHAPE_TRACK);
        }
        out
    }

    /// The track `param`'s keys as times and values: a shape key's value
    /// is none, its interpolation said by its ease (Hold as Step, Ease as
    /// Bezier).
    pub fn track_keys(&self, param: &str) -> Vec<Keyframe> {
        if param == Self::SHAPE_TRACK {
            return self
                .keyframes
                .path
                .iter()
                .map(|k| Keyframe {
                    time_secs: k.time_secs,
                    value: 0.5,
                    interpolation: match k.ease {
                        crate::mask_path::PathEase::Linear => keyframe::Interpolation::Linear,
                        crate::mask_path::PathEase::Hold => keyframe::Interpolation::Step,
                        crate::mask_path::PathEase::Ease => keyframe::Interpolation::Bezier,
                    },
                    tangent_out: (0.0, 0.0),
                    tangent_in: (0.0, 0.0),
                    handles: Default::default(),
                })
                .collect();
        }
        self.keyframes.track(param).cloned().unwrap_or_default()
    }

    /// A key on track `param` at `time_secs`: `value` for a value's track
    /// (a key there takes it), the shape as it is then for the shape's.
    pub fn add_key(&mut self, param: &str, time_secs: f64, value: f32) {
        if param == Self::SHAPE_TRACK {
            if let Some(now) = self.path_at(time_secs) {
                *self = self.with_path_keyed(true, time_secs).with_path_at(time_secs, now, keyframe::KEYFRAME_TIME_EPSILON);
            }
            return;
        }
        let Some(track) = self.keyframes.track_mut(param) else { return };
        let epsilon = keyframe::KEYFRAME_TIME_EPSILON;
        if let Some(kf) = track.iter_mut().find(|k| (k.time_secs - time_secs).abs() < epsilon) {
            kf.value = value;
        } else {
            track.push(Keyframe { time_secs, value, interpolation: keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() });
            track.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
        }
    }

    /// The key on track `param` at `time_secs` taken off. The shape's last
    /// key off keeps the shape as it was there.
    pub fn remove_key(&mut self, param: &str, time_secs: f64) {
        if param == Self::SHAPE_TRACK {
            let epsilon = keyframe::KEYFRAME_TIME_EPSILON;
            if let Some(i) = self.keyframes.path.iter().position(|k| (k.time_secs - time_secs).abs() < epsilon) {
                if self.keyframes.path.len() == 1 {
                    *self = self.with_path_keyed(false, time_secs);
                } else {
                    self.keyframes.path.remove(i);
                }
            }
            return;
        }
        if let Some(track) = self.keyframes.track_mut(param) {
            keyframe::remove_keyframe_at(track, time_secs);
        }
    }

    /// Every key of track `param` taken off.
    pub fn clear_track(&mut self, param: &str) {
        if param == Self::SHAPE_TRACK {
            let at = self.keyframes.path.first().map_or(0.0, |k| k.time_secs);
            *self = self.with_path_keyed(false, at);
        } else if let Some(track) = self.keyframes.track_mut(param) {
            track.clear();
        }
    }

    /// The key on track `param` at `time_secs` moved to `new_time_secs`,
    /// with `value` when given (a shape key moves in time only). Refused,
    /// and nothing changed, when no key is there or another key is at the
    /// new time: whether it moved.
    pub fn move_key(&mut self, param: &str, time_secs: f64, new_time_secs: f64, value: Option<f32>) -> bool {
        let epsilon = keyframe::KEYFRAME_TIME_EPSILON;
        let new_time_secs = new_time_secs.max(0.0);
        if param == Self::SHAPE_TRACK {
            let keys = &mut self.keyframes.path;
            let Some(i) = keys.iter().position(|k| (k.time_secs - time_secs).abs() < epsilon) else { return false };
            if keys.iter().enumerate().any(|(j, k)| j != i && (k.time_secs - new_time_secs).abs() < epsilon) {
                return false;
            }
            keys[i].time_secs = new_time_secs;
            keys.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
            return true;
        }
        let Some(track) = self.keyframes.track_mut(param) else { return false };
        let Some(i) = track.iter().position(|k| (k.time_secs - time_secs).abs() < epsilon) else { return false };
        if track.iter().enumerate().any(|(j, k)| j != i && (k.time_secs - new_time_secs).abs() < epsilon) {
            return false;
        }
        track[i].time_secs = new_time_secs;
        if let Some(v) = value {
            track[i].value = v;
        }
        track.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
        true
    }

    /// How track `param` leaves its key at `time_secs`: a value's
    /// interpolation, or a shape key's ease (Step holds, Bezier eases).
    pub fn set_key_interpolation(&mut self, param: &str, time_secs: f64, interpolation: keyframe::Interpolation) {
        let epsilon = keyframe::KEYFRAME_TIME_EPSILON;
        if param == Self::SHAPE_TRACK {
            if let Some(k) = self.keyframes.path.iter_mut().find(|k| (k.time_secs - time_secs).abs() < epsilon) {
                k.ease = match interpolation {
                    keyframe::Interpolation::Linear => crate::mask_path::PathEase::Linear,
                    keyframe::Interpolation::Step => crate::mask_path::PathEase::Hold,
                    keyframe::Interpolation::Bezier | keyframe::Interpolation::Eased(_) => crate::mask_path::PathEase::Ease,
                };
            }
        } else if let Some(k) = self.keyframes.track_mut(param).and_then(|t| t.iter_mut().find(|k| (k.time_secs - time_secs).abs() < epsilon)) {
            k.interpolation = interpolation;
        }
    }

    /// [`Self::evaluated_at`], then the window's tracked motion (#238)
    /// applied, on a frame `aspect` (height over width) tall: what is
    /// drawn at `time_secs`. A window tracked in perspective keeps its
    /// shape as drawn and carries the map there in `keyframes.warp`.
    pub fn evaluated_in(&self, time_secs: f64, aspect: f32) -> Mask {
        let mut m = self.evaluated_at(time_secs);
        if let Some(motion) = self.track_at(time_secs) {
            m.shape = m.shape.moved(motion, aspect);
        }
        m.keyframes.warp = self.keyframes.track_perspective.as_ref().and_then(|p| p.map_at(time_secs)).and_then(|f| f.inverse());
        m
    }

    /// The tracked motion at `time_secs`, if the window is tracked.
    pub fn track_at(&self, time_secs: f64) -> Option<WindowMotion> {
        let k = &self.keyframes;
        if !k.is_tracked() {
            return None;
        }
        let at = |track: &Vec<Keyframe>, rest: f32| if track.is_empty() { rest } else { keyframe::evaluate(track, time_secs, rest) };
        Some(WindowMotion { dx: at(&k.track_x, 0.0), dy: at(&k.track_y, 0.0), scale: at(&k.track_scale, 1.0), rotation: at(&k.track_rotation, 0.0) })
    }

    /// Get the current static value of a named parameter.
    pub fn param_value(&self, param: &str) -> Option<f32> {
        match param {
            "feather" => Some(self.feather),
            "opacity" => Some(self.opacity),
            _ => match &self.shape {
                MaskShape::Circle { center_x, center_y, radius } => match param {
                    "center_x" => Some(*center_x),
                    "center_y" => Some(*center_y),
                    "radius" => Some(*radius),
                    _ => None,
                },
                MaskShape::Rectangle { center_x, center_y, width, height, rotation } => match param {
                    "center_x" => Some(*center_x),
                    "center_y" => Some(*center_y),
                    "width" => Some(*width),
                    "height" => Some(*height),
                    "rotation" => Some(*rotation),
                    _ => None,
                },
                MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => match param {
                    "start_x" => Some(*start_x),
                    "start_y" => Some(*start_y),
                    "end_x" => Some(*end_x),
                    "end_y" => Some(*end_y),
                    _ => None,
                },
                MaskShape::RadialGradient { center_x, center_y, inner, outer } => match param {
                    "center_x" => Some(*center_x),
                    "center_y" => Some(*center_y),
                    "inner" => Some(*inner),
                    "outer" => Some(*outer),
                    _ => None,
                },
                MaskShape::Bezier { .. } => None,
            }
        }
    }
}

/// Stack of masks applied to a clip's color grade.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaskStack {
    pub masks: Vec<Mask>,
}

impl MaskStack {
    pub fn is_empty(&self) -> bool {
        self.masks.is_empty() || self.masks.iter().all(|m| !m.enabled)
    }

    /// Returns true if any mask in the stack has keyframes.
    pub fn has_keyframes(&self) -> bool {
        self.masks.iter().any(|m| !m.keyframes.is_empty())
    }

    /// [`Mask::evaluated_in`] for every mask: what is drawn at
    /// `time_secs` on a frame `aspect` (height over width) tall.
    pub fn evaluated_in(&self, time_secs: f64, aspect: f32) -> MaskStack {
        if !self.has_keyframes() {
            return self.clone();
        }
        MaskStack { masks: self.masks.iter().map(|m| m.evaluated_in(time_secs, aspect)).collect() }
    }

    /// Evaluate all mask keyframes at the given time and return a new stack
    /// with animated parameters resolved.
    pub fn evaluated_at(&self, time_secs: f64) -> MaskStack {
        if !self.has_keyframes() {
            return self.clone();
        }
        MaskStack {
            masks: self.masks.iter().map(|m| m.evaluated_at(time_secs)).collect(),
        }
    }
}

/// Film grain overlay settings.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct FilmGrainSettings {
    pub enabled: bool,
    /// Grain intensity (0.0 = off, 1.0 = maximum)
    pub amount: f32,
    /// Grain particle size (0.5 = fine, 3.0 = coarse)
    pub size: f32,
    /// How much grain in shadows (0.0 = none, 1.0 = full)
    pub shadows: f32,
    /// How much grain in highlights (0.0 = none, 1.0 = full)
    pub highlights: f32,
    /// Monochrome grain (true) vs colour grain (false)
    pub monochrome: bool,
}

impl Default for FilmGrainSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 0.0,
            size: 1.0,
            shadows: 1.0,
            highlights: 0.5,
            monochrome: true,
        }
    }
}

/// Color Warper parameters: 12 hue sectors at 30-degree intervals.
/// Each sector can shift hue (degrees) and saturation (relative offset).
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct ColorWarperParams {
    /// Per-sector hue shifts in degrees. Index 0 = red (0deg), 1 = orange (30deg), etc.
    #[serde(default)]
    pub hue_shifts: [f32; 12],
    /// Per-sector saturation offsets. 0.0 = no change, -1.0 = desaturate, 1.0 = double.
    #[serde(default)]
    pub sat_shifts: [f32; 12],
}

impl Default for ColorWarperParams {
    fn default() -> Self {
        Self {
            hue_shifts: [0.0; 12],
            sat_shifts: [0.0; 12],
        }
    }
}

impl ColorWarperParams {
    pub fn is_identity(&self) -> bool {
        self.hue_shifts.iter().all(|v| v.abs() < 0.001)
            && self.sat_shifts.iter().all(|v| v.abs() < 0.001)
    }
}

impl HueSatCurves {
    pub fn is_all_identity(&self) -> bool {
        self.hue_vs_hue.is_empty()
            && self.hue_vs_sat.is_empty()
            && self.hue_vs_lum.is_empty()
            && self.lum_vs_sat.is_empty()
            && self.sat_vs_sat.is_empty()
            && self.orange_vs_sat.is_empty()
            && self.sat_vs_lum.is_empty()
    }
}


/// Stored result of a colour-match analysis between a source frame
/// (the input to the ColorMatch node) and a user-picked reference
/// frame. The four scalar adjustments map directly to slots on the
/// unified grade shader (`apply_color_grade_chain`), so the executor
/// dispatches the match as a single GPU pass with these values.
///
/// Default = identity (no change). The struct is serialisable so a
/// project file can carry the computed match without holding the
/// reference frame bytes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorMatchSettings {
    /// Exposure adjustment in stops (-3..+3).
    pub exposure: f32,
    /// Temperature adjustment (-100..+100, +ve = warmer).
    pub temperature: f32,
    /// Tint adjustment (-100..+100, +ve = magenta).
    pub tint: f32,
    /// Saturation multiplier (0.1..3.0, 1.0 = identity).
    pub saturation: f32,
}

impl Default for ColorMatchSettings {
    fn default() -> Self {
        Self { exposure: 0.0, temperature: 0.0, tint: 0.0, saturation: 1.0 }
    }
}

impl ColorMatchSettings {
    pub fn is_identity(&self) -> bool {
        self.exposure.abs() < f32::EPSILON
            && self.temperature.abs() < f32::EPSILON
            && self.tint.abs() < f32::EPSILON
            && (self.saturation - 1.0).abs() < f32::EPSILON
    }
}

// ── LayerMixer compositing ─────────────────────────────────────────

/// Per-input compositing settings for `NodeKind::LayerMixer`.
/// Lives here (not in klipp-nodes) so AppCommand can reference it
/// without inverting the crate dependency. Blend mode reuses the
/// existing `crate::types::BlendMode` (13 modes) used by the timeline
/// compositor, so adding a mode in one place lights it up in both.
#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LayerMixerInput {
    /// 0.0..=1.0 — how strongly this layer contributes when mixed
    /// over the layer beneath it.
    pub opacity: f32,
    pub blend_mode: crate::types::BlendMode,
}

impl Default for LayerMixerInput {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            blend_mode: crate::types::BlendMode::Normal,
        }
    }
}

#[cfg(test)]
mod window_motion_tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
    }

    /// #238: a tracked window moves, grows and turns about its pivot,
    /// and a turn is a turn on the picture, not in frame fractions.
    #[test]
    fn a_tracked_window_moves_scales_and_turns_on_the_picture() {
        let aspect = 9.0 / 16.0;
        let m = WindowMotion { dx: 0.1, dy: -0.05, scale: 2.0, rotation: 90.0 };
        let MaskShape::Circle { center_x, center_y, radius } = (MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.1 }).moved(m, aspect) else { panic!() };
        assert!(close((center_x, center_y), (0.6, 0.45)) && (radius - 0.2).abs() < 1e-6);
        let MaskShape::Rectangle { width, height, rotation, .. } =
            (MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.2, height: 0.1, rotation: 10.0 }).moved(m, aspect)
        else {
            panic!()
        };
        assert!((width - 0.4).abs() < 1e-6 && (height - 0.2).abs() < 1e-6 && (rotation - 100.0).abs() < 1e-4);
        // A gradient across the frame, turned a quarter clockwise about its
        // middle and doubled: on a 16:9 picture its end 0.1 of the width
        // right of the middle lands 0.2 of the width (0.356 of the height)
        // below it.
        let g = MaskShape::LinearGradient { start_x: 0.4, start_y: 0.5, end_x: 0.6, end_y: 0.5 };
        let MaskShape::LinearGradient { start_x, start_y, end_x, end_y } = g.moved(WindowMotion { dx: 0.0, dy: 0.0, scale: 2.0, rotation: 90.0 }, aspect) else { panic!() };
        assert!(close((end_x, end_y), (0.5, 0.5 + 0.2 / aspect)), "{end_x} {end_y}");
        assert!(close((start_x, start_y), (0.5, 0.5 - 0.2 / aspect)));
        // A free shape moves with its centroid.
        let b = MaskShape::polygon([(0.4, 0.4), (0.6, 0.4), (0.5, 0.6)]);
        let MaskShape::Bezier { points } = b.moved(WindowMotion { dx: 0.1, dy: 0.2, scale: 1.0, rotation: 0.0 }, aspect) else { panic!() };
        assert!(close(points[0].pos(), (0.5, 0.6)) && close(points[2].pos(), (0.6, 0.8)));
        // And a point on the moved window goes back where it was.
        let m = WindowMotion { dx: 0.05, dy: -0.02, scale: 1.4, rotation: 33.0 };
        let MaskShape::Bezier { points: moved } = (MaskShape::polygon([(0.3, 0.2), (0.7, 0.3), (0.5, 0.8)])).moved(m, aspect) else { panic!() };
        let pivot = (0.5, 0.4333333);
        assert!(close(m.undo(moved[1].pos(), pivot, aspect), (0.7, 0.3)), "{:?}", m.undo(moved[1].pos(), pivot, aspect));
        // A point's handles turn and scale with it: its handle's end lands
        // where that end, moved as a point, does.
        let curved = MaskShape::Bezier { points: vec![
            crate::mask_path::BezierPoint { x: 0.3, y: 0.2, out_dx: 0.1, out_dy: 0.05, in_dx: -0.05, in_dy: 0.02, smooth: false },
            (0.7, 0.3).into(),
            (0.5, 0.8).into(),
        ] };
        let MaskShape::Bezier { points: turned } = curved.moved(m, aspect) else { panic!() };
        let end = m.undo(turned[0].out_handle(), pivot, aspect);
        assert!(close(end, (0.4, 0.25)), "{end:?}");
        let end = m.undo(turned[0].in_handle(), pivot, aspect);
        assert!(close(end, (0.25, 0.22)), "{end:?}");
    }

    /// The motion is keyframed, evaluated with the rest, and the window
    /// untracked draws as it did.
    #[test]
    fn a_window_draws_where_its_track_says_at_each_moment() {
        let mut m = Mask { shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.1 }, ..Mask::default() };
        assert_eq!(m.evaluated_in(1.0, 0.5625), m);
        let key = |t: f64, v: f32| crate::keyframe::Keyframe { time_secs: t, value: v, interpolation: crate::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        m.keyframes.track_x = vec![key(0.0, 0.0), key(1.0, 0.2)];
        m.keyframes.track_scale = vec![key(0.0, 1.0), key(1.0, 1.5)];
        assert!(m.keyframes.is_tracked() && !m.keyframes.is_empty());
        let MaskShape::Circle { center_x, radius, .. } = m.evaluated_in(1.0, 0.5625).shape else { panic!() };
        assert!((center_x - 0.7).abs() < 1e-5 && (radius - 0.15).abs() < 1e-5);
        m.keyframes.clear_track();
        assert!(!m.keyframes.is_tracked());
    }

    /// #238: a window tracked in perspective keeps its shape as drawn and
    /// carries the map back to the frame it was drawn on: the tracked
    /// corners there go back to where they started, a key's corners are
    /// held past the last key and met halfway between two, and the
    /// whole round trips through a project file.
    #[test]
    fn a_window_in_perspective_is_carried_by_its_corners() {
        let from = [[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]];
        let later = [[0.42, 0.38], [0.58, 0.43], [0.57, 0.57], [0.43, 0.63]];
        let mut m = Mask { shape: MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.2, height: 0.2 * 16.0 / 9.0, rotation: 0.0 }, ..Mask::default() };
        m.keyframes.track_perspective = Some(PerspectiveTrack { from, keys: vec![CornerKey { time_secs: 0.0, corners: from }, CornerKey { time_secs: 1.0, corners: later }] });
        assert!(m.keyframes.is_tracked() && !m.keyframes.is_empty());
        let e = m.evaluated_in(2.0, 9.0 / 16.0);
        assert_eq!(e.shape, m.shape, "the shape stays as drawn");
        let back = e.keyframes.warp.expect("a warp");
        for (now, then) in later.iter().zip(&from) {
            let got = back.apply((now[0], now[1])).unwrap();
            assert!(close(got, (then[0], then[1])), "{now:?} went back to {got:?}, not {then:?}");
        }
        let half = m.keyframes.track_perspective.as_ref().unwrap().corners_at(0.5).unwrap();
        assert!(close((half[3][0], half[3][1]), (0.415, 0.615)));
        // Untracked at the start: no move at all.
        let start = m.evaluated_in(0.0, 9.0 / 16.0).keyframes.warp.unwrap();
        assert!(close(start.apply((0.13, 0.77)).unwrap(), (0.13, 0.77)));
        let json = serde_json::to_string(&m).unwrap();
        let again: Mask = serde_json::from_str(&json).unwrap();
        assert_eq!(again, m);
        assert!(!json.contains("warp"));
        m.keyframes.clear_track();
        assert!(!m.keyframes.is_tracked() && m.evaluated_in(1.0, 0.5625).keyframes.warp.is_none());
    }

    /// A surface turned so far that the frame's corner lies past its
    /// horizon still maps the window in front of the eye, and a point
    /// past the horizon maps nowhere.
    #[test]
    fn a_plane_map_keeps_the_window_in_front_past_the_horizon() {
        let from = [[0.45, 0.45], [0.55, 0.45], [0.55, 0.55], [0.45, 0.55]];
        // The right side a sliver of the left: the far edge near the
        // vanishing point.
        let to = [[0.40, 0.40], [0.52, 0.49], [0.52, 0.51], [0.40, 0.60]];
        let f = PlaneMap::from_quads(&from, &to).unwrap();
        for (a, b) in from.iter().zip(&to) {
            assert!(close(f.apply((a[0], a[1])).unwrap(), (b[0], b[1])));
        }
        let inv = f.inverse().unwrap();
        let p = inv.apply((0.46, 0.5)).unwrap();
        assert!(close(f.apply(p).unwrap(), (0.46, 0.5)));
        // The right side shrinks to a tenth of the left, so a little to
        // the left of the window the surface is edge-on, and beyond that
        // behind the eye.
        assert!(f.apply((0.3, 0.5)).is_none());
        assert!(PlaneMap::from_quads(&from, &[[0.1, 0.1], [0.2, 0.2], [0.3, 0.3], [0.4, 0.4]]).is_none_or(|m| m.inverse().is_none()));
    }
}

#[cfg(test)]
mod mask_track_tests {
    use super::*;
    use crate::mask_path::{BezierPoint, PathEase, PathKey};

    fn kf(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    fn sq(off: f32) -> Vec<BezierPoint> {
        [(0.2, 0.2), (0.4, 0.2), (0.4, 0.4)].into_iter().map(|(x, y)| BezierPoint::corner(x + off, y)).collect()
    }

    /// #1947: a mask's tracks, keyed and not; a key moved in time and
    /// value, never onto another; a shape key moved in time, its ease set
    /// from an interpolation; the shape's last key off keeps the shape.
    #[test]
    fn a_masks_keys_move_ease_and_go() {
        let mut m = Mask { shape: MaskShape::Bezier { points: sq(0.0) }, ..Mask::default() };
        m.keyframes.feather = vec![kf(0.5, 0.1), kf(1.5, 0.2)];
        m.keyframes.path = vec![PathKey { time_secs: 1.0, points: sq(0.0), ease: PathEase::Linear }, PathKey { time_secs: 2.0, points: sq(0.2), ease: PathEase::Linear }];
        assert_eq!(m.keyed_tracks(), ["feather", "shape"]);
        assert_eq!(m.unkeyed_tracks(), ["opacity"]);

        assert!(m.move_key("feather", 0.5, 1.0, Some(0.3)));
        assert_eq!((m.keyframes.feather[0].time_secs, m.keyframes.feather[0].value), (1.0, 0.3));
        assert!(!m.move_key("feather", 1.0, 1.5, None), "onto the other key: refused");
        assert!(m.move_key("shape", 2.0, 0.5, Some(9.0)));
        assert_eq!(m.keyframes.path.iter().map(|k| k.time_secs).collect::<Vec<_>>(), [0.5, 1.0], "in time order");
        assert_eq!(m.keyframes.path[0].points, sq(0.2), "a shape key's points go with it");

        m.set_key_interpolation("shape", 0.5, keyframe::Interpolation::Step);
        assert_eq!(m.keyframes.path[0].ease, PathEase::Hold);
        assert_eq!(m.track_keys("shape")[0].interpolation, keyframe::Interpolation::Step);

        m.remove_key("shape", 0.5);
        m.remove_key("shape", 1.0);
        assert!(m.keyframes.path.is_empty());
        assert_eq!(m.path_at(0.0).unwrap(), sq(0.0), "the shape as the last key had it");
        m.add_key("shape", 3.0, 0.0);
        assert_eq!(m.keyframes.path.len(), 1);
        m.clear_track("feather");
        assert!(m.keyframes.feather.is_empty());
    }
}
