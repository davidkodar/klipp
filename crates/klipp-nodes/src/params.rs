use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use klipp_core::{
    BlendMode, ColorWarperParams, CurveSet, DifferenceKeySettings, FilmGrainSettings, HdrZonesSettings, HueSatCurves,
    LayerMixerInput, LogFormat, LumaKeySettings, MaskStack, QualifierSettings, SoftClipSettings,
};

/// All possible parameter values a node can have.
/// Each node kind uses only a subset of these fields.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeParams {
    /// `Effect`: its one built-in effect, resolved for the frame;
    /// `EffectSegment`: the effects it runs together, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<klipp_effects::BuiltinEffect>,
    /// `Ofx`: the plug-in and its parameter values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ofx: Option<crate::plugin::OfxNodeParams>,

    /// `Effect`/`EffectSegment`: how the effects' result is mixed back over
    /// their input (phase 2b M2, step 9c): `None` takes the result whole.
    /// The run's own setting, never saved.
    #[serde(skip)]
    pub effect_blend: Option<EffectBlend>,

    /// A gateable correction: how its result is mixed back over its input,
    /// by `mix` and by its gate's matte read from `channel` (M3 step 17a);
    /// on the gate the executor inserts after it, the same. `None` takes
    /// the result whole where the matte is, its red channel. The run's
    /// own setting, never saved: a clip's picture holds it.
    #[serde(skip)]
    pub correction_blend: Option<EffectBlend>,
    /// A gateable correction: its gate takes the matte on its port 1
    /// inverted, `1 - coverage` (#898), so it grades where the key isn't:
    /// an outside node, reading the same key as the inside one. `None` is
    /// as the key is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_invert: Option<bool>,

    /// `SceneToDisplay`: the display step it runs (phase 2b M2, step 11c).
    /// The run's own setting, never saved.
    #[serde(skip)]
    pub scene_to_display: Option<klipp_color::hdr::SceneToDisplay>,

    /// `SidePicture`: the picture it outputs, when not the one its effect
    /// draws (#1746). The run's own setting, never saved.
    #[serde(skip)]
    pub side_picture: Option<crate::side_picture::SideKey>,

    // InputTransform
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_format: Option<LogFormat>,

    // Exposure
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,

    // Contrast
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f32>,
    /// Where contrast pivots, as a linear working-space value.
    ///
    /// `None` is mid grey, 0.18, which is where it pivoted before this
    /// was adjustable — so every grade written before it reads back
    /// identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast_pivot: Option<f32>,
    // Clarity (midtone detail)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarity: Option<f32>,

    // Saturation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f32>,
    // Vibrance
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vibrance: Option<f32>,

    // ToneControls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlights: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whites: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blacks: Option<f32>,

    // ColorWheels
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_master: Option<f32>,

    // Log Wheels (HDR mode: Low/Mid/High)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_low_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_low_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_low_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_mid_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_mid_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_mid_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_high_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_high_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_high_master: Option<f32>,
    /// Whether wheels operate in log mode (true) or linear mode (false/default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wheels_log_mode: Option<bool>,
    /// Where the log wheels' low band centres on the encoded luma
    /// (#900): `klipp_grade_model::LOG_LOW_RANGE` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_low_range: Option<f32>,
    /// Where the high band centres: `LOG_HIGH_RANGE` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_high_range: Option<f32>,
    /// How much of the wheels' change to luma is kept, 0 to 1 (#900):
    /// 1 when unset, as the wheels always were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luma_mix: Option<f32>,

    // WheelSatBri
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_sat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_bri: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_sat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_bri: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_sat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_bri: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_sat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_bri: Option<f32>,

    // ColorBalance
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_mix: Option<f32>,

    // RGBCurves
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves: Option<CurveSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves_preserve_luma: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves_mix: Option<f32>,

    // HueSatCurves
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huesat: Option<HueSatCurves>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huesat_mix: Option<f32>,

    // Qualifier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<QualifierSettings>,

    // Standalone luma key — luminance threshold matte producer.
    // Simpler than Qualifier (no HSL); see `LumaKeySettings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luma_key: Option<LumaKeySettings>,

    // Standalone difference key — frame-subtraction matte from a
    // clean-plate reference loaded from disk. The plate path lives
    // INSIDE this struct (not on the shared `matte_image_path` slot
    // ExternalMatte uses) so a future scenario where both nodes sit
    // on the same clip can't have the paths collide. See
    // `DifferenceKeySettings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difference_key: Option<DifferenceKeySettings>,

    // Standalone soft-clip / hard-clip — rolls highlights off below
    // 1.0 with a Reinhard-style asymptotic curve. Value transform,
    // not a matte. See `SoftClipSettings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_clip: Option<SoftClipSettings>,

    // HDR zones (#240): exposure, colour and saturation by tonal zone.
    // See `HdrZonesSettings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_zones: Option<HdrZonesSettings>,

    // Mask
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masks: Option<MaskStack>,

    // Lut3D. Stored portably: a LUT from the bundled library by its
    // place in the library, so a project opens anywhere (#331).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "klipp_core::assets::lut_path_serde")]
    pub lut_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut_opacity: Option<f32>,
    /// Per-node override for the loaded LUT's `input_space` tag. The
    /// `.cube` format doesn't carry colorspace metadata, so loaded
    /// LUTs default to `Rec709Linear`; users override here when
    /// they know the LUT was authored in a different basis. `None`
    /// means "use the LUT's own default tag" (currently always
    /// `Rec709Linear` for parser-loaded LUTs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut_input_space: Option<klipp_core::WorkingColorspace>,
    /// Per-node override for the LUT's `output_space` tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut_output_space: Option<klipp_core::WorkingColorspace>,

    // PrimariesMatrix — source / destination working-space tags.
    // The executor derives the 3×3 matrix from the pair via
    // `klipp_color::primaries::working_space_conversion_matrix`.
    // Both `None` = passthrough (used for freshly-added unset nodes
    // and for migrated projects that no longer need the wrappers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primaries_from: Option<klipp_core::WorkingColorspace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primaries_to: Option<klipp_core::WorkingColorspace>,

    // FilmGrain
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<FilmGrainSettings>,

    // ColorWarper
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warper_params: Option<ColorWarperParams>,

    // ColorMatch — pre-computed adjustments derived from running
    // klipp_color::match_colors against a user-picked reference frame.
    // None or default = pass-through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_match: Option<klipp_core::ColorMatchSettings>,

    // Denoise
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_luma: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_chroma: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_radius: Option<u32>,

    // Blend / Layer / ApplyMatte
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<f32>,

    /// Blend as a merge (#2758): the picture on port 1 laid over the one
    /// on port 0 as a layer mixer's top layer over its base, by its blend
    /// mode and its opacity times its own alpha, the base's alpha kept.
    /// `None`: a plain mix by `mix`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<klipp_core::LayerMixerInput>,

    // LayerMixer — per-port settings (None = default 2-input config,
    // composited Normal at full opacity). When the editor grows the
    // port count this Vec is grown in lockstep so `inputs[port]` is
    // always defined for any port the executor sees on an edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_mixer: Option<LayerMixerParams>,

    // ExternalMatte
    /// Absolute path to the image file read as a matte source.
    /// `None`, or an image that will not load: the matte selects
    /// everything, as every matte producer does with nothing chosen
    /// (#1039), so a correction it gates grades as if there were no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_image_path: Option<PathBuf>,
    /// A tracked subject's matte (#1881): bound by the clip's media and
    /// this model, its folder found at render from the media as linked
    /// now (so a relink follows the file), not by a path. `None` reads
    /// `matte_image_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_subject: Option<SubjectMatte>,
    /// Which channel of the image becomes the matte signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_channel: Option<MatteChannel>,
    /// Output is inverted (matte = 1 - source) before feeding
    /// downstream. Mirrors the Qualifier node's Invert toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_invert: Option<bool>,

    // CombineMattes
    /// How a `CombineMattes` node combines its two inputs. `None` is
    /// [`MatteCombineOp::Intersect`], which is what the node did before
    /// it could do anything else — so a project saved without this field
    /// keeps the picture it had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_combine_op: Option<MatteCombineOp>,

    // KeyMixer
    /// Each input's gain and invert, and the output's gain and offset.
    /// `None` is every input at gain 1, not inverted, and the output as
    /// summed. Grown with the ports, as `layer_mixer` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_mixer: Option<KeyMixerParams>,

    // Compound
    /// Sub-graph executed by a `NodeKind::Compound` node. The
    /// compound's input feeds the sub-graph's Source, the sub-
    /// graph's Output feeds the compound's output. `None` = the
    /// compound acts as a passthrough (executor copies primary
    /// input to output). The recursive nesting is safe: NodeGraph
    /// derives Serialize/Deserialize and contains no cycles by
    /// construction. Old projects don't gain a noisy null field
    /// thanks to `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compound_subgraph: Option<crate::graph::NodeGraph>,
}

/// How a `CombineMattes` node combines its two matte inputs.
///
/// Mattes are *coverage*, so the operators are the ones that make sense
/// on a value in [0, 1] rather than the ones that make sense on a
/// colour, and they are evaluated in linear light — see the executor for
/// why that distinction is not academic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MatteCombineOp {
    /// Both: `a * b`. The only thing the node used to do, so it stays
    /// the default and old projects keep their picture.
    #[default]
    Intersect,
    /// Either: `a + b - a*b`. The complement of intersecting the
    /// inverses, which is what keeps it inside [0, 1] — plain `a + b`
    /// would clip and flatten every overlap to solid.
    Union,
    /// A without B: `a * (1 - b)`. The one a colourist reaches for most
    /// after Intersect — qualify a face, then subtract the eyes.
    Subtract,
}

impl MatteCombineOp {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Intersect => "Intersect (A and B)",
            Self::Union => "Union (A or B)",
            Self::Subtract => "Subtract (A minus B)",
        }
    }

    pub const ALL: [Self; 3] = [Self::Intersect, Self::Union, Self::Subtract];

    /// The B value that makes this operator a no-op, for an unwired or
    /// wrong-sized second input. White is the identity for Intersect
    /// only: as a Union it would select the whole frame and as a
    /// Subtract it would select none of it, so an unwired input has to
    /// ask the operator rather than assume.
    #[inline]
    pub fn identity_b(self) -> f32 {
        match self {
            Self::Intersect => 1.0,
            Self::Union | Self::Subtract => 0.0,
        }
    }

    /// Combine two coverage values. Inputs are assumed already clamped
    /// to [0, 1]; every operator here maps that range onto itself, so
    /// the result needs no clamp of its own.
    #[inline]
    pub fn apply(self, a: f32, b: f32) -> f32 {
        match self {
            Self::Intersect => a * b,
            Self::Union => a + b - a * b,
            Self::Subtract => a * (1.0 - b),
        }
    }
}

/// Which source channel of an ExternalMatte image becomes the matte.
/// The model a tracked subject's matte is made by (#1881).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectMatte {
    /// One detected subject, followed.
    #[default]
    Subject,
    /// Everything in front.
    Foreground,
}

impl SubjectMatte {
    /// Its place in the folders a run is given
    /// (`GraphExecutor::set_current_clip_subject_mattes`): 0 the subject,
    /// 1 the foreground, as the frame sources number them.
    pub fn index(self) -> usize {
        match self {
            SubjectMatte::Subject => 0,
            SubjectMatte::Foreground => 1,
        }
    }

    /// Its name, as `TrackSubject` takes it.
    pub fn name(self) -> &'static str {
        match self {
            SubjectMatte::Subject => "subject",
            SubjectMatte::Foreground => "foreground",
        }
    }
}

/// Alpha is the natural choice for images painted with transparency;
/// Luminance is the practical fallback for opaque grayscale masks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MatteChannel {
    Alpha,
    #[default]
    Luminance,
    Red,
    Green,
    Blue,
}

impl MatteChannel {
    pub fn display_name(&self) -> &'static str {
        match self {
            MatteChannel::Alpha => "Alpha",
            MatteChannel::Luminance => "Luminance",
            MatteChannel::Red => "Red",
            MatteChannel::Green => "Green",
            MatteChannel::Blue => "Blue",
        }
    }

    pub const ALL: [MatteChannel; 5] = [
        MatteChannel::Alpha, MatteChannel::Luminance,
        MatteChannel::Red, MatteChannel::Green, MatteChannel::Blue,
    ];
}

// ── KeyMixer ──────────────────────────────────────────────────────

/// One input of a `NodeKind::KeyMixer`.
#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KeyMixerInput {
    /// What its coverage is multiplied by before the sum; negative
    /// subtracts it.
    #[serde(default = "one")]
    pub gain: f32,
    /// Its coverage taken as `1 - c`.
    #[serde(default)]
    pub invert: bool,
}

impl Default for KeyMixerInput {
    fn default() -> Self {
        Self { gain: 1.0, invert: false }
    }
}

/// How a `NodeKind::KeyMixer` mixes its mattes (#898): a plain sum,
/// clamped once at the end.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct KeyMixerParams {
    /// One entry per input port, `inputs[n]` port n; a port past the end
    /// is at the default.
    #[serde(default)]
    pub inputs: Vec<KeyMixerInput>,
    #[serde(default = "one")]
    pub output_gain: f32,
    #[serde(default)]
    pub output_offset: f32,
}

impl Default for KeyMixerParams {
    fn default() -> Self {
        Self { inputs: Vec::new(), output_gain: 1.0, output_offset: 0.0 }
    }
}

fn one() -> f32 {
    1.0
}

impl KeyMixerParams {
    /// Input `port`'s settings.
    pub fn input(&self, port: u8) -> KeyMixerInput {
        self.inputs.get(usize::from(port)).copied().unwrap_or_default()
    }

    /// The mixed coverage of one pixel, from each wired input's
    /// `(port, coverage)`: `clamp(offset + output_gain * Σ gain * c, 0, 1)`,
    /// `c` taken as `1 - c` when the input is inverted.
    pub fn mix(&self, coverages: impl IntoIterator<Item = (u8, f32)>) -> f32 {
        self.mix_inputs(coverages.into_iter().map(|(port, c)| (self.input(port), c)))
    }

    /// [`Self::mix`] with each input's settings already looked up: the
    /// executor resolves them once a frame, not once a pixel.
    pub fn mix_inputs(&self, coverages: impl IntoIterator<Item = (KeyMixerInput, f32)>) -> f32 {
        let sum: f32 = coverages
            .into_iter()
            .map(|(input, c)| {
                let c = c.clamp(0.0, 1.0);
                input.gain * if input.invert { 1.0 - c } else { c }
            })
            .sum();
        (self.output_offset + self.output_gain * sum).clamp(0.0, 1.0)
    }
}

// ── LayerMixer ────────────────────────────────────────────────────

/// Per-input compositing settings for `NodeKind::LayerMixer`.
/// `LayerMixerInput` + `BlendMode` themselves live in `klipp_core` so
/// that `AppCommand` (also in klipp-core) can reference them without
/// inverting the crate dependency.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct LayerMixerParams {
    /// One entry per input port. `inputs[0]` is port 0 (top of stack).
    pub inputs: Vec<LayerMixerInput>,
}

impl LayerMixerParams {
    /// True when every input is at default (Normal blend, opacity 1).
    /// Used by `NodeParams::is_default` so an unmodified mixer doesn't
    /// keep the clip out of identity-graph fast-paths.
    pub fn is_identity(&self) -> bool {
        self.inputs.iter().all(|i|
            i.blend_mode == BlendMode::Normal && (i.opacity - 1.0).abs() < 1e-4
        )
    }
}

impl NodeParams {
    /// Check if all params are at their default (identity) values — no grading applied.
    pub fn is_default(&self) -> bool {
        // Treat both None and Some(~0.0) as default for numeric fields
        let z = |f: &Option<f32>| f.is_none_or(|v| v.abs() < 0.0001);
        z(&self.exposure)
            && z(&self.contrast)
            && z(&self.clarity)
            && self.saturation.is_none_or(|v| (v - 1.0).abs() < 0.0001) // default is 1.0
            && z(&self.vibrance)
            && z(&self.temperature)
            && z(&self.tint)
            && z(&self.hue)
            && z(&self.highlights)
            && z(&self.shadows)
            && z(&self.whites)
            && z(&self.blacks)
            && z(&self.lift_x)
            && z(&self.lift_y)
            && z(&self.lift_master)
            && z(&self.gamma_x)
            && z(&self.gamma_y)
            && z(&self.gamma_master)
            && z(&self.gain_x)
            && z(&self.gain_y)
            && z(&self.gain_master)
            && z(&self.offset_x)
            && z(&self.offset_y)
            && z(&self.offset_master)
            && z(&self.log_low_x)
            && z(&self.log_low_y)
            && z(&self.log_low_master)
            && z(&self.log_mid_x)
            && z(&self.log_mid_y)
            && z(&self.log_mid_master)
            && z(&self.log_high_x)
            && z(&self.log_high_y)
            && z(&self.log_high_master)
            && self.wheels_log_mode.is_none()
            && self.log_low_range.is_none()
            && self.log_high_range.is_none()
            && self.luma_mix.is_none_or(|v| (v - 1.0).abs() < 0.0001)
            && z(&self.lift_sat)
            && z(&self.lift_bri)
            && z(&self.gamma_sat)
            && z(&self.gamma_bri)
            && z(&self.gain_sat)
            && z(&self.gain_bri)
            && z(&self.offset_sat)
            && z(&self.offset_bri)
            && z(&self.global_mix)
            && self.curves.is_none()
            && self.curves_preserve_luma.is_none()
            && z(&self.curves_mix)
            && self.huesat.is_none()
            && z(&self.huesat_mix)
            && self.log_format.is_none()
            && self.qualifier.is_none()
            && self.luma_key.is_none()
            && self.difference_key.is_none()
            && self.color_match.as_ref().is_none_or(|m| m.is_identity())
            && self.soft_clip.is_none()
            && self.hdr_zones.as_ref().is_none_or(HdrZonesSettings::is_neutral)
            && self.masks.is_none()
            && self.lut_path.is_none()
            && z(&self.lut_opacity)
            && self.lut_input_space.is_none()
            && self.lut_output_space.is_none()
            && self.primaries_from.is_none()
            && self.primaries_to.is_none()
            && self.grain.is_none()
            && self.warper_params.is_none()
            && z(&self.denoise_luma)
            && z(&self.denoise_chroma)
            && self.denoise_radius.is_none()
            && z(&self.mix)
            && self.layer_mixer.as_ref().is_none_or(|m| m.is_identity())
            && self.matte_image_path.is_none()
            && self.matte_subject.is_none()
            && self.compound_subgraph.is_none()
    }

    /// Apply a keyframe override by param name. Returns whether the
    /// name is one this struct carries.
    ///
    /// Values near 0.0 are stored as None (neutral) to keep is_default()
    /// correct.
    ///
    /// The return value is what lets a caller that did not come from the
    /// inspector — the control socket, a script — be told that its
    /// parameter name went nowhere. Ignoring an unknown name silently is
    /// indistinguishable from applying it and reads as "the grade did
    /// not work".
    ///
    /// A value that isn't a number (NaN, an infinity) is never written
    /// (#2995): the grade keeps what it had. The LUT spaces read infinity
    /// as "the LUT's own", so take it.
    pub fn apply_override(&mut self, param_name: &str, value: f32) -> bool {
        if !value.is_finite() && !matches!(param_name, "lut_input_space" | "lut_output_space") {
            return self.clone().apply_override(param_name, 0.0);
        }
        // For most params, near-zero means neutral → store None
        let n = if value.abs() < 0.0001 { None } else { Some(value) };
        match param_name {
            "exposure" => self.exposure = n,
            "contrast" => self.contrast = n,
            "contrast_pivot" => self.contrast_pivot = Some(value), // unset is the default pivot: 0 is a value
            "clarity" => self.clarity = n,
            "saturation" => self.saturation = Some(value), // default is 1.0, not 0.0
            "vibrance" => self.vibrance = n,
            "temperature" => self.temperature = n,
            "tint" => self.tint = n,
            "highlights" => self.highlights = n,
            "shadows" => self.shadows = n,
            "whites" => self.whites = n,
            "blacks" => self.blacks = n,
            "lift_x" => self.lift_x = n,
            "lift_y" => self.lift_y = n,
            "lift_master" => self.lift_master = n,
            "gamma_x" => self.gamma_x = n,
            "gamma_y" => self.gamma_y = n,
            "gamma_master" => self.gamma_master = n,
            "gain_x" => self.gain_x = n,
            "gain_y" => self.gain_y = n,
            "gain_master" => self.gain_master = n,
            "offset_x" => self.offset_x = n,
            "offset_y" => self.offset_y = n,
            "offset_master" => self.offset_master = n,
            "log_low_x" => self.log_low_x = n,
            "log_low_y" => self.log_low_y = n,
            "log_low_master" => self.log_low_master = n,
            "log_mid_x" => self.log_mid_x = n,
            "log_mid_y" => self.log_mid_y = n,
            "log_mid_master" => self.log_mid_master = n,
            "log_high_x" => self.log_high_x = n,
            "log_high_y" => self.log_high_y = n,
            "log_high_master" => self.log_high_master = n,
            "wheels_log_mode" => self.wheels_log_mode = Some(value > 0.5),
            "log_low_range" => self.log_low_range = Some(value),
            "log_high_range" => self.log_high_range = Some(value),
            "luma_mix" => self.luma_mix = Some(value), // default is 1.0, not 0.0
            "lift_sat" => self.lift_sat = n,
            "lift_bri" => self.lift_bri = n,
            "gamma_sat" => self.gamma_sat = n,
            "gamma_bri" => self.gamma_bri = n,
            "gain_sat" => self.gain_sat = n,
            "gain_bri" => self.gain_bri = n,
            "offset_sat" => self.offset_sat = n,
            "offset_bri" => self.offset_bri = n,
            "hue" => self.hue = n,
            "global_mix" => self.global_mix = Some(value), // default is 1.0: 0 is a value, not "unset"
            "curves_mix" => self.curves_mix = Some(value), // default is 1.0: 0 is a value, not "unset"
            "curves_preserve_luma" => self.curves_preserve_luma = Some(value > 0.5),
            "huesat_mix" => self.huesat_mix = Some(value), // default is 1.0: 0 is a value, not "unset"
            "lut_opacity" => self.lut_opacity = Some(value), // default is 1.0: 0 is a value, not "unset"
            "lut_input_space" => {
                // Encoded as the WorkingColorspace::as_u8 tag in
                // f32. Any other value (the catalog's "Use LUT default"
                // item, 3; the old menu's sentinel, infinity; NaN or a
                // negative from a corrupted dispatch) is `None`: use the
                // LUT's own tag.
                self.lut_input_space = lut_space(value);
            }
            "lut_output_space" => self.lut_output_space = lut_space(value),
            // A choice's index, as the catalog names its items. An index
            // past the end falls back to the default rather than refusing,
            // as the old panel's dispatch does: a bad discriminant should
            // not wedge the node.
            "matte_channel" => {
                use crate::MatteChannel::*;
                self.matte_channel = Some([Alpha, Luminance, Red, Green, Blue].get(value as usize).copied().unwrap_or_default());
            }
            // 0 none (the image), 1 the subject, 2 the foreground (#2758).
            "matte_subject" => {
                self.matte_subject = [None, Some(crate::SubjectMatte::Subject), Some(crate::SubjectMatte::Foreground)].get(value as usize).copied().flatten();
            }
            "matte_combine_op" => {
                self.matte_combine_op = Some(crate::MatteCombineOp::ALL.get(value as usize).copied().unwrap_or_default());
            }
            "difference_key.match_space" => {
                use klipp_core::color_grade::DifferenceKeyMatchSpace::*;
                self.difference_key.get_or_insert_with(Default::default).match_space = [Rgb, Luminance].get(value as usize).copied().unwrap_or_default();
            }
            "primaries_from" => {
                // Encoded as the WorkingColorspace::as_u8 tag in
                // f32. Out-of-range / non-finite → None (= the node
                // becomes a passthrough). Mirrors lut_input_space
                // dispatch.
                self.primaries_from = if value.is_finite() && (0.0..256.0).contains(&value) {
                    Some(klipp_core::WorkingColorspace::from_u8(value as u8))
                } else {
                    None
                };
            }
            "primaries_to" => {
                self.primaries_to = if value.is_finite() && (0.0..256.0).contains(&value) {
                    Some(klipp_core::WorkingColorspace::from_u8(value as u8))
                } else {
                    None
                };
            }
            "mix" => self.mix = Some(value), // default isn't 0: 0 is a value, not "unset"
            // The index is part of the name, so a bad one is a bad name
            // rather than a no-op on a good one.
            p if p.starts_with("warper_hue_") => {
                match p["warper_hue_".len()..].parse::<usize>() {
                    Ok(idx) if idx < 12 => {
                        let w = self.warper_params.get_or_insert_with(Default::default);
                        w.hue_shifts[idx] = value;
                    }
                    _ => return false,
                }
            }
            p if p.starts_with("warper_sat_") => {
                match p["warper_sat_".len()..].parse::<usize>() {
                    Ok(idx) if idx < 12 => {
                        let w = self.warper_params.get_or_insert_with(Default::default);
                        w.sat_shifts[idx] = value;
                    }
                    _ => return false,
                }
            }
            // HDR zones (#240): `hdr_<zone>_<control>`, the zone by its
            // name (`hdr_highlight_exposure`); a name no zone or control
            // reads is a bad name.
            p if p.starts_with("hdr_") => {
                if hdr_zone_slot(&mut HdrZonesSettings::default(), p).is_none() {
                    return false;
                }
                if let Some(slot) = hdr_zone_slot(self.hdr_zones.get_or_insert_with(Default::default), p) {
                    *slot = value;
                }
            }
            "denoise_luma" => self.denoise_luma = Some(value),
            "denoise_chroma" => self.denoise_chroma = Some(value),
            "denoise_radius" => self.denoise_radius = Some(value as u32),

            // ── HSL qualifier (secondary correction) ──────────────────
            // Touching any of these turns the key on: a caller setting a
            // hue window means to key on it, and a qualifier that is
            // configured but disabled passes the whole frame, which
            // looks like the key failing.
            p if p.starts_with("qual_") => {
                let q = self.qualifier.get_or_insert_with(Default::default);
                match &p["qual_".len()..] {
                    "enabled" => q.enabled = value != 0.0,
                    "hue_center" => { q.hue_center = value; q.enabled = true }
                    "hue_width" => { q.hue_width = value; q.enabled = true }
                    "hue_soft" => { q.hue_soft = value; q.enabled = true }
                    "sat_low" => { q.sat_low = value; q.enabled = true }
                    "sat_high" => { q.sat_high = value; q.enabled = true }
                    "sat_soft" => { q.sat_soft = value; q.enabled = true }
                    "lum_low" => { q.lum_low = value; q.enabled = true }
                    "lum_high" => { q.lum_high = value; q.enabled = true }
                    "lum_soft" => { q.lum_soft = value; q.enabled = true }
                    "invert" => q.invert = value != 0.0,
                    // No exposure, saturation, hue_shift or temperature:
                    // a Qualifier node outputs its matte, and the change
                    // a secondary makes is the gated correction's, so those
                    // names keyed nothing and went nowhere (#1040).
                    "clean_black" => q.clean_black = value,
                    "clean_white" => q.clean_white = value,
                    "denoise" => q.denoise = value,
                    "shrink_grow" => q.shrink_grow = value,
                    "blur" => q.blur = value,
                    // The key's mode and the RGB mode's ranges (#895).
                    "mode" => { q.mode = klipp_core::QualifierMode::from_index(value); q.enabled = true }
                    "red_low" => { q.red_low = value; q.enabled = true }
                    "red_high" => { q.red_high = value; q.enabled = true }
                    "green_low" => { q.green_low = value; q.enabled = true }
                    "green_high" => { q.green_high = value; q.enabled = true }
                    "blue_low" => { q.blue_low = value; q.enabled = true }
                    "blue_high" => { q.blue_high = value; q.enabled = true }
                    "rgb_soft" => { q.rgb_soft = value; q.enabled = true }
                    "key_radius" => { q.key_radius = value; q.enabled = true }
                    "key_soft" => { q.key_soft = value; q.enabled = true }
                    _ => return false,
                }
            }

            // ── Luminance key ────────────────────────────────────────
            "key_invert" => self.key_invert = (value != 0.0).then_some(true),
            "key_mixer_output_gain" => self.key_mixer.get_or_insert_with(Default::default).output_gain = value,
            "key_mixer_output_offset" => self.key_mixer.get_or_insert_with(Default::default).output_offset = value,
            p if p.starts_with("key_mixer_gain_") || p.starts_with("key_mixer_invert_") => {
                let (invert, n) = match p.strip_prefix("key_mixer_gain_") {
                    Some(n) => (false, n),
                    None => (true, &p["key_mixer_invert_".len()..]),
                };
                // A port the node could have: as many as an edge's port
                // can name.
                let Ok(port) = n.parse::<u8>() else { return false };
                let mixer = self.key_mixer.get_or_insert_with(Default::default);
                if mixer.inputs.len() <= usize::from(port) {
                    mixer.inputs.resize_with(usize::from(port) + 1, KeyMixerInput::default);
                }
                let input = &mut mixer.inputs[usize::from(port)];
                if invert {
                    input.invert = value != 0.0;
                } else {
                    input.gain = value;
                }
            }
            p if p.starts_with("luma_key_") => {
                let k = self.luma_key.get_or_insert_with(Default::default);
                match &p["luma_key_".len()..] {
                    "enabled" => k.enabled = value != 0.0,
                    "low" => { k.low = value; k.enabled = true }
                    "high" => { k.high = value; k.enabled = true }
                    "soft" => { k.soft = value; k.enabled = true }
                    "invert" => k.invert = value != 0.0,
                    _ => return false,
                }
            }
            _ => return false,
        }
        true
    }

    /// Whether [`Self::apply_override`] carries this parameter name.
    ///
    /// Asks `apply_override` itself, on a throwaway struct, so the
    /// answer cannot drift from the match arms the way a hand-kept list
    /// of names would.
    pub fn accepts(param_name: &str) -> bool {
        Self::default().apply_override(param_name, 0.0)
    }
}

/// The setting `key` names in the HDR zones (#240): `hdr_<zone>_<control>`,
/// the zone one of Global, Black, Dark, Shadow, Light, Highlight,
/// Specular in lower case, the control `exposure`, `colour_x`, `colour_y`
/// (the colour wheel's puck) or `saturation`, or for every zone but Global `boundary` or
/// `falloff`.
pub fn hdr_zone_slot<'a>(z: &'a mut HdrZonesSettings, key: &str) -> Option<&'a mut f32> {
    let rest = key.strip_prefix("hdr_")?;
    let (zone, control) = HDR_ZONE_KEYS.iter().enumerate().find_map(|(i, name)| rest.strip_prefix(name)?.strip_prefix('_').map(|c| (i, c)))?;
    Some(match control {
        "exposure" => &mut z.exposure[zone],
        "colour_x" => &mut z.colour_x[zone],
        "colour_y" => &mut z.colour_y[zone],
        "saturation" => &mut z.saturation[zone],
        "boundary" if zone > 0 => &mut z.boundary[zone],
        "falloff" if zone > 0 => &mut z.falloff[zone],
        _ => return None,
    })
}

/// The HDR zones' names as their keys spell them (#240), in order.
pub const HDR_ZONE_KEYS: [&str; 7] = ["global", "black", "dark", "shadow", "light", "highlight", "specular"];

/// A LUT's working-space tag from its `apply_override` value: a tag for
/// 0, 1 or 2, else `None` (the LUT's own).
fn lut_space(value: f32) -> Option<klipp_core::WorkingColorspace> {
    (value.is_finite() && (0.0..3.0).contains(&value)).then(|| klipp_core::WorkingColorspace::from_u8(value as u8))
}

// ── Keyframe tracks for nodes ──────────────────────────────────────────────

/// Keyframe tracks for a single node.
/// Each track identifies a parameter by name string.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeKeyframeTracks {
    #[serde(default)]
    pub tracks: Vec<NodeKeyframeTrack>,
}

/// A single keyframe track animating one parameter of a node.
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct NodeKeyframeTrack {
    /// e.g. "exposure", "lift_x"
    pub param_name: String,
    /// Sorted by time. Reuses existing Keyframe struct.
    pub keyframes: Vec<klipp_core::Keyframe>,
}

impl NodeKeyframeTracks {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() || self.tracks.iter().all(|t| t.keyframes.is_empty())
    }

    /// Evaluate all animated params at clip_time_secs.
    /// Returns owned (param_name, value) pairs.
    pub fn evaluate_all(&self, time_secs: f64) -> Vec<(String, f32)> {
        self.tracks
            .iter()
            .filter_map(|t| t.evaluate(time_secs).map(|v| (t.param_name.clone(), v)))
            .collect()
    }

    /// Find or create a track for the given parameter.
    pub fn track_mut(&mut self, param_name: &str) -> &mut NodeKeyframeTrack {
        if let Some(idx) = self.tracks.iter().position(|t| t.param_name == param_name) {
            &mut self.tracks[idx]
        } else {
            self.tracks.push(NodeKeyframeTrack {
                param_name: param_name.to_string(),
                keyframes: Vec::new(),
            });
            self.tracks.last_mut().unwrap()
        }
    }

    /// Check if a specific parameter has keyframes.
    pub fn has_keyframes(&self, param_name: &str) -> bool {
        self.tracks
            .iter()
            .any(|t| t.param_name == param_name && !t.keyframes.is_empty())
    }
}

impl NodeKeyframeTrack {
    /// Evaluate the track at a given time (seconds relative to clip source_in).
    /// Returns None if the track has no keyframes.
    ///
    /// By the one keyframe evaluator ([`klipp_core::keyframe::evaluate`]),
    /// which the curve editor and the inspector draw a key's curve by, so an
    /// eased key's handles (their time as well as their value) bend the
    /// picture as they bend the curve drawn (#891).
    pub fn evaluate(&self, time_secs: f64) -> Option<f32> {
        (!self.keyframes.is_empty()).then(|| klipp_core::keyframe::evaluate(&self.keyframes, time_secs, 0.0))
    }

    /// Insert at stored-time precision, preserving existing interpolation.
    pub fn set_keyframe(&mut self, time_secs: f64, value: f32) {
        klipp_core::keyframe::upsert_keyframe(&mut self.keyframes, time_secs, value);
    }

    /// Remove only the identified stored timestamp.
    pub fn remove_keyframe(&mut self, time_secs: f64) -> bool {
        let before = self.keyframes.len();
        klipp_core::keyframe::remove_keyframe_at(&mut self.keyframes,time_secs);
        self.keyframes.len() != before
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grade value that isn't a number is never written (#2995), and
    /// the name is still known.
    #[test]
    fn a_grade_never_takes_a_value_that_isnt_a_number() {
        let mut p = NodeParams { exposure: Some(0.5), ..Default::default() };
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(p.apply_override("exposure", bad), "exposure is a known name");
            assert_eq!(p.exposure, Some(0.5), "{bad} kept out");
            assert!(p.apply_override("saturation", bad));
            assert_eq!(p.saturation, None, "{bad} kept out");
        }
        assert!(!p.apply_override("no_such_param", f32::NAN));
        // The LUT spaces read infinity as the LUT's own.
        p.lut_input_space = Some(klipp_core::WorkingColorspace::default());
        assert!(p.apply_override("lut_input_space", f32::INFINITY));
        assert_eq!(p.lut_input_space, None);
    }

    /// A grade key's track renders as its curve is drawn: an eased key's
    /// handles bend the value by their time as well as their value, the
    /// one evaluator's answer at every time.
    #[test]
    fn a_track_evaluates_as_its_curve_is_drawn() {
        let mut keys = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut keys, 0.0, 0.0);
        klipp_core::keyframe::upsert_keyframe(&mut keys, 2.0, 1.0);
        klipp_core::keyframe::ease_key(&mut keys, 1, klipp_core::KeyEase::In);
        let track = NodeKeyframeTrack { param_name: "exposure".into(), keyframes: keys.clone() };
        for t in [-1.0, 0.0, 0.3, 0.9, 1.5, 1.9, 2.0, 3.0] {
            assert_eq!(track.evaluate(t), Some(klipp_core::keyframe::evaluate(&keys, t, 0.0)), "at {t}");
        }
        assert_eq!(NodeKeyframeTrack { param_name: "exposure".into(), keyframes: Vec::new() }.evaluate(1.0), None);
    }

    /// A Color Match node holding a match is not "default": default nodes
    /// are bypassed, and one that ignored its match never ran.
    #[test]
    fn a_colour_match_is_not_default() {
        let mut p = NodeParams {
            color_match: Some(klipp_core::ColorMatchSettings { exposure: 0.0, temperature: 0.0, tint: 0.0, saturation: 1.0 }),
            ..Default::default()
        };
        assert!(p.is_default(), "an identity match changes nothing");
        p.color_match = Some(klipp_core::ColorMatchSettings { exposure: 0.3, temperature: 20.0, tint: 0.0, saturation: 1.0 });
        assert!(!p.is_default());
    }

    /// `set_param("lut_input_space", n)` decodes `n` as a u8 and
    /// resolves it via `WorkingColorspace::from_u8`. In-range values
    /// land on the right enum variant; out-of-range values (notably
    /// `f32::INFINITY`, the inspector's "Use LUT default" sentinel)
    /// map to `None`.
        /// The qualifier's window is reachable by name, and touching it arms
    /// the key — a configured-but-disabled qualifier passes the whole
    /// frame, which reads as the key not working.
    #[test]
    fn qualifier_window_is_settable_by_name() {
        let mut p = NodeParams::default();
        assert!(p.qualifier.is_none());
        assert!(p.apply_override("qual_hue_center", 40.0));
        assert!(p.apply_override("qual_hue_width", 25.0));
        assert!(p.apply_override("qual_lum_low", 0.1));
        assert!(p.apply_override("qual_lum_high", 0.4));
        let q = p.qualifier.as_ref().expect("qualifier materialised on first touch");
        assert!(q.enabled, "setting a window arms the key");
        assert_eq!(q.hue_center, 40.0);
        assert_eq!(q.hue_width, 25.0);
        assert_eq!((q.lum_low, q.lum_high), (0.1, 0.4));
        // `enabled` itself stays a plain switch, so a caller can arm and
        // disarm without the write arming it again.
        assert!(p.apply_override("qual_enabled", 0.0));
        assert!(!p.qualifier.as_ref().unwrap().enabled);

        let mut k = NodeParams::default();
        assert!(k.apply_override("luma_key_low", 0.2));
        assert!(k.apply_override("luma_key_soft", 0.05));
        let lk = k.luma_key.as_ref().expect("luma key materialised");
        assert!(lk.enabled && lk.low == 0.2 && lk.soft == 0.05);
    }

    /// The HDR zones (#240) are set by name, the zone then the control,
    /// and a node with every zone neutral is a default one (bypassed).
    /// Controls: a zone or control no zone has is refused and makes
    /// nothing, and Global has no boundary.
    #[test]
    fn hdr_zones_are_settable_by_name() {
        let mut p = NodeParams::default();
        assert!(p.apply_override("hdr_highlight_exposure", 1.5));
        assert!(p.apply_override("hdr_black_colour_y", 0.2));
        assert!(p.apply_override("hdr_shadow_boundary", -1.5));
        let z = p.hdr_zones.as_ref().expect("made on first touch");
        assert_eq!((z.exposure[5], z.colour_y[1], z.boundary[3]), (1.5, 0.2, -1.5));
        assert!(!p.is_default());
        for bad in ["hdr_global_boundary", "hdr_midtone_exposure", "hdr_light_gain", "hdr_lightexposure"] {
            let mut q = NodeParams::default();
            assert!(!q.apply_override(bad, 1.0), "{bad}");
            assert!(q.hdr_zones.is_none(), "{bad} made nothing");
        }
        let mut n = NodeParams::default();
        assert!(n.apply_override("hdr_dark_boundary", -3.0));
        assert!(n.is_default(), "a boundary moved alone changes no pixel");
    }

    /// A name the params do not carry is reported, not swallowed. The
    /// fall-through used to be silent, which made a typo on the control
    /// socket indistinguishable from a grade that refused to apply.
    #[test]
    fn an_unknown_param_name_is_reported() {
        let mut p = NodeParams::default();
        for name in ["", "nonsense", "Temperature", "qual_", "qual_nonsense",
                     "luma_key_nonsense", "warper_hue_nonsense", "warper_hue_12",
                     "warper_sat_99"] {
            assert!(!p.apply_override(name, 1.0), "{name:?} should not be accepted");
            assert!(!NodeParams::accepts(name), "accepts({name:?}) should be false");
        }
        // And the real ones still are, by both routes.
        for name in ["exposure", "contrast", "temperature", "tint", "saturation",
                     "vibrance", "clarity", "highlights", "shadows", "whites",
                     "blacks", "lift_x", "gain_master", "mix", "global_mix",
                     "warper_hue_0", "qual_sat_low", "luma_key_high",
                     "log_low_range", "log_high_range", "luma_mix"] {
            assert!(p.apply_override(name, 0.5), "{name:?} should be accepted");
            assert!(NodeParams::accepts(name), "accepts({name:?}) should be true");
        }
    }

    #[test]
    fn lut_input_space_param_dispatch() {
        let mut p = NodeParams::default();
        p.apply_override("lut_input_space", 1.0);
        assert_eq!(p.lut_input_space, Some(klipp_core::WorkingColorspace::AcesCG));
        p.apply_override("lut_input_space", 0.0);
        assert_eq!(p.lut_input_space, Some(klipp_core::WorkingColorspace::Rec709Linear));
        p.apply_override("lut_input_space", 2.0);
        assert_eq!(p.lut_input_space, Some(klipp_core::WorkingColorspace::Bt2020Linear));
        // "Use LUT default" sentinel: f32::INFINITY → None.
        p.apply_override("lut_input_space", f32::INFINITY);
        assert_eq!(p.lut_input_space, None);
        // Defensive: NaN → None.
        p.apply_override("lut_input_space", f32::NAN);
        assert_eq!(p.lut_input_space, None);
        // Out-of-range positive → None.
        p.apply_override("lut_input_space", 256.0);
        assert_eq!(p.lut_input_space, None);
        // The catalog's "Use LUT default" item, past the three tags → None.
        p.apply_override("lut_input_space", 1.0);
        p.apply_override("lut_input_space", 3.0);
        assert_eq!(p.lut_input_space, None);
    }

    #[test]
    fn lut_output_space_param_dispatch() {
        let mut p = NodeParams::default();
        p.apply_override("lut_output_space", 1.0);
        assert_eq!(p.lut_output_space, Some(klipp_core::WorkingColorspace::AcesCG));
        p.apply_override("lut_output_space", f32::INFINITY);
        assert_eq!(p.lut_output_space, None);
    }

    /// Setting a LUT colorspace override flips the params out of the
    /// "all-default / can-skip-history" state. Otherwise the
    /// inspector dropdown wouldn't trigger a save.
    #[test]
    fn lut_colorspace_override_marks_params_non_empty() {
        let mut p = NodeParams::default();
        assert!(p.is_default());
        p.lut_input_space = Some(klipp_core::WorkingColorspace::AcesCG);
        assert!(!p.is_default(),
            "an explicit LUT colorspace tag should mark the params as non-default");
    }
}

#[cfg(test)]
mod matte_combine_op_tests {
    use super::*;

    /// Coverage in, coverage out: every operator maps [0, 1] onto
    /// itself, so nothing downstream has to clamp after one.
    #[test]
    fn every_operator_stays_inside_the_unit_range() {
        let vals = [0.0, 0.01, 0.25, 0.5, 0.75, 0.99, 1.0];
        for op in MatteCombineOp::ALL {
            for a in vals {
                for b in vals {
                    let k = op.apply(a, b);
                    assert!(
                        (0.0..=1.0).contains(&k),
                        "{op:?}({a}, {b}) = {k}, outside [0, 1]",
                    );
                }
            }
        }
    }

    /// The operators mean what their names say, on the cases a colourist
    /// would predict without doing arithmetic.
    #[test]
    fn the_operators_do_what_they_are_called() {
        use MatteCombineOp::*;
        // Fully inside both, either, or A-not-B.
        assert_eq!(Intersect.apply(1.0, 1.0), 1.0);
        assert_eq!(Intersect.apply(1.0, 0.0), 0.0);
        assert_eq!(Union.apply(1.0, 0.0), 1.0);
        assert_eq!(Union.apply(0.0, 0.0), 0.0);
        assert_eq!(Subtract.apply(1.0, 1.0), 0.0);
        assert_eq!(Subtract.apply(1.0, 0.0), 1.0);
        // Half of each: the intersect is a quarter, and the union is
        // three quarters — not 1.0, which is what a plain `a + b` would
        // give and why Union is not a sum.
        assert_eq!(Intersect.apply(0.5, 0.5), 0.25);
        assert_eq!(Union.apply(0.5, 0.5), 0.75);
        assert_eq!(Subtract.apply(0.5, 0.5), 0.25);
    }

    /// Intersect and Union are symmetric; Subtract deliberately is not.
    /// Swapping the inputs of a Subtract is a different selection, which
    /// is the whole point of having it.
    #[test]
    fn only_subtract_cares_which_input_is_which() {
        for (a, b) in [(0.3f32, 0.7f32), (0.1, 0.9), (0.25, 0.5)] {
            assert!((MatteCombineOp::Intersect.apply(a, b) - MatteCombineOp::Intersect.apply(b, a)).abs() < 1e-6);
            assert!((MatteCombineOp::Union.apply(a, b) - MatteCombineOp::Union.apply(b, a)).abs() < 1e-6);
            assert_ne!(
                MatteCombineOp::Subtract.apply(a, b),
                MatteCombineOp::Subtract.apply(b, a),
                "Subtract({a}, {b}) should differ from Subtract({b}, {a})",
            );
        }
    }

    /// An unwired B leaves the matte alone, whichever operator is
    /// selected. White is the identity for Intersect only — used blindly
    /// it would make a Union select the whole frame.
    #[test]
    fn an_unwired_second_input_is_a_no_op() {
        for op in MatteCombineOp::ALL {
            for a in [0.0, 0.2, 0.5, 0.8, 1.0] {
                let k = op.apply(a, op.identity_b());
                assert!(
                    (k - a).abs() < 1e-6,
                    "{op:?} with an unwired B moved {a} to {k}",
                );
            }
        }
    }

    /// A project saved before the operator existed keeps its picture:
    /// the field is absent, and absent means Intersect.
    #[test]
    fn a_project_without_the_field_intersects() {
        let json = r#"{"mix":1.0}"#;
        let p: NodeParams = serde_json::from_str(json).expect("old params still parse");
        assert_eq!(p.matte_combine_op, None, "no field should stay None");
        assert_eq!(p.matte_combine_op.unwrap_or_default(), MatteCombineOp::Intersect);
    }

    /// And the field round-trips once set, without disturbing the
    /// absent-is-Intersect shape for everyone else.
    #[test]
    fn the_operator_round_trips() {
        for op in MatteCombineOp::ALL {
            let p = NodeParams { matte_combine_op: Some(op), ..Default::default() };
            let s = serde_json::to_string(&p).expect("serialise");
            let back: NodeParams = serde_json::from_str(&s).expect("deserialise");
            assert_eq!(back.matte_combine_op, Some(op));
        }
        // Default params must not write the key at all.
        let s = serde_json::to_string(&NodeParams::default()).expect("serialise");
        assert!(!s.contains("matte_combine_op"), "default params wrote the key: {s}");
    }
}

#[cfg(test)]
mod precision_review_tests {
    use super::*;
    #[test]
    fn adjacent_high_rate_keys_survive_exact_remove_and_value_edit() {
        let mut t = NodeKeyframeTrack { param_name: "exposure".into(), keyframes: vec![] };
        t.set_keyframe(5.0/120.0, 1.0); t.set_keyframe(6.0/120.0, 2.0);
        t.keyframes[0].interpolation = klipp_core::Interpolation::Bezier;
        t.set_keyframe(5.0/120.0,3.0);
        assert_eq!(t.keyframes[0].interpolation,klipp_core::Interpolation::Bezier);
        assert!(t.remove_keyframe(6.0/120.0));
        assert_eq!(t.keyframes.len(),1); assert_eq!(t.keyframes[0].time_secs,5.0/120.0);
        t.set_keyframe(0.7,1.0); t.set_keyframe(0.7005,2.0);
        assert_eq!(t.keyframes.len(),3);
    }
}

#[cfg(test)]
mod portable_lut_path_tests {
    use super::NodeParams;

    /// #331: a look from the bundled library is saved by its place in
    /// the library, not by where the library was on this machine, and
    /// loads back to the same file.
    #[test]
    fn a_bundled_look_saves_portably_and_loads_back() {
        let Some(lib) = klipp_core::assets::bundled_luts_dir() else { return };
        let path = lib.join("fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube");
        let params = NodeParams { lut_path: Some(path.clone()), ..Default::default() };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"lut_path\":\"bundled:fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube\""), "{json}");
        assert!(!json.contains(&*lib.to_string_lossy()), "the machine's library path leaked into the project: {json}");
        let back: NodeParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.lut_path, Some(path));
    }

    #[test]
    fn no_look_is_still_no_look() {
        let json = serde_json::to_string(&NodeParams::default()).unwrap();
        assert!(!json.contains("lut_path"));
        let back: NodeParams = serde_json::from_str("{}").unwrap();
        assert_eq!(back.lut_path, None);
    }
}

#[cfg(test)]
mod override_names_tests {
    use super::NodeParams;

    /// #1040: the qualifier names that keyed nothing are not taken, so a
    /// caller setting one is told it went nowhere.
    #[test]
    fn the_qualifiers_correction_names_are_not_taken() {
        for name in ["qual_exposure", "qual_saturation", "qual_hue_shift", "qual_temperature"] {
            assert!(!NodeParams::accepts(name), "{name} keys nothing and must not be taken");
        }
        assert!(NodeParams::accepts("qual_hue_center"), "the key's own settings still are");
    }
}

/// How an effect node's result is mixed back over its input: by `mix`,
/// and by the coverage its mask input gives (port 1; none is white),
/// read from `channel`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectBlend {
    pub mix: f32,
    pub channel: klipp_effects::mask_mix::MaskChannel,
}

#[cfg(test)]
mod pivot_zero_tests {
    use super::NodeParams;

    /// A contrast pivot set to 0 stays 0: stored as unset it read back as
    /// the default pivot, as the mixes did before #1444 (#1038).
    #[test]
    fn a_contrast_pivot_at_zero_stays_zero() {
        let mut p = NodeParams::default();
        assert!(p.apply_override("contrast_pivot", 0.0));
        assert_eq!(p.contrast_pivot, Some(0.0));
    }
}
