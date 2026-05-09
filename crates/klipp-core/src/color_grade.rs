use crate::keyframe::{self, Keyframe};
use serde::{Deserialize, Serialize};

/// Per-channel RGB curves. Each channel is a list of control points [x, y] in 0..1.
/// Default has endpoints [[0,0],[1,1]] (identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
}

/// HSL qualifier for secondary color correction.
/// Selects a range of hue/saturation/luminance to isolate specific colors.
/// The matte (0-1 per pixel) determines how much the secondary grade is applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub enum DifferenceKeyMatchSpace {
    /// Per-pixel `d = max(|R_live − R_plate|, |G_live − G_plate|,
    /// |B_live − B_plate|)` (Chebyshev / L∞ norm). Catches subjects
    /// that share luminance with the plate but differ in colour
    /// (red shirt against grey wall). More discriminating; default.
    Rgb,
    /// Per-pixel `d = |Y_live − Y_plate|` with Rec.709 weights.
    /// Faster, more forgiving of plate-noise chroma drift; suits
    /// fixed-camera shots where the subject is meaningfully
    /// brighter or darker than the background.
    Luminance,
}

impl Default for DifferenceKeyMatchSpace {
    fn default() -> Self {
        Self::Rgb
    }
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DifferenceKeySettings {
    pub enabled: bool,
    /// Path to the clean-plate image on disk. `None` → matte is empty
    /// (passthrough), so a freshly-added node doesn't black out the
    /// viewer before the user picks a plate. Same fallback shape as
    /// `ExternalMatte` with no path set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate_path: Option<std::path::PathBuf>,
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

impl Default for DifferenceKeySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            plate_path: None,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Shape-based mask for regional color grading (power windows).
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Freeform bezier path mask. Points are in normalized coords (0-1).
    /// The path is closed (last point connects to first).
    Bezier {
        /// Control points as (x, y) pairs in normalized coordinates.
        points: Vec<(f32, f32)>,
    },
}

/// Keyframe tracks for animating mask parameters over time.
/// Each Vec<Keyframe> is empty by default (static mask). When non-empty,
/// the keyframes override the corresponding static parameter at render time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
            _ => None,
        }
    }

    /// List of animatable parameter names for a given mask shape variant.
    pub fn params_for_shape(shape: &MaskShape) -> &'static [&'static str] {
        match shape {
            MaskShape::Circle { .. } => &["center_x", "center_y", "radius", "feather", "opacity"],
            MaskShape::Rectangle { .. } => &["center_x", "center_y", "width", "height", "rotation", "feather", "opacity"],
            MaskShape::LinearGradient { .. } => &["start_x", "start_y", "end_x", "end_y", "feather", "opacity"],
            MaskShape::Bezier { .. } => &["feather", "opacity"],
        }
    }
}

/// A single mask with shape, feathering, and blend controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mask {
    pub shape: MaskShape,
    pub feather: f32,   // 0-1, soft edge width
    pub invert: bool,
    pub opacity: f32,   // 0-1
    pub enabled: bool,
    #[serde(default)]
    pub keyframes: MaskKeyframes,
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
        }
    }
}

impl Mask {
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
            MaskShape::Bezier { .. } => {
                // Bezier point animation deferred — too complex for initial implementation
            }
        }

        result
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
                MaskShape::Bezier { .. } => None,
            }
        }
    }
}

/// Stack of masks applied to a clip's color grade.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
