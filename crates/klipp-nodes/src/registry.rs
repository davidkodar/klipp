//! The grade's node types, described with the same types as the built-in
//! effects ([`klipp_effects::registry`]): ports, parameters with their
//! defaults, ranges, units and whether they can be keyframed (#1003,
//! #1009).
//!
//! Nothing reads it yet. `tests/registry_coverage.rs` checks each
//! description against the code: every kind is described, every
//! parameter is a field of [`NodeParams`](crate::NodeParams), the
//! keyframable ones are exactly the names
//! [`NodeParams::apply_override`](crate::NodeParams::apply_override)
//! takes, and the ports agree with the kind's port counts and roles.
//!
//! How to read a description:
//!
//! * A parameter's `field` is its `NodeParams` field, and for a field that
//!   is a struct (`qualifier`, `luma_key`, `grain`, ...) the struct's own
//!   field after a dot: `qualifier.hue_width`. Its `key` is the name a
//!   keyframe track and `apply_override` use (`qual_hue_width`).
//! * The default is what the executor uses when the field is unset, which
//!   is what a new node has. For a struct field it is the struct's own
//!   default.
//! * Soft ranges are the node inspector's sliders. No parameter has a
//!   hard range: the executor takes any number.
//! * Sizes in pixels are the sequence's pixels, which the executor scales
//!   to the rendered frame ([`Scaling::Executor`], #1012).
//! * A correction a matte can limit (port 1, `NodeGraph::with_gates_applied`)
//!   has an optional `mask` input beyond its port count, with the mask's
//!   channel setting, and is mixable: faded by `mix` against its input
//!   (M3 step 17a). Its inputs are [`GATE_IN`]: a gate reads red until a
//!   channel is picked.
//!
//! Each kind is described as the code behaves, including where two places
//! disagree; those are filed, not resolved here:
//!
//! * #1038: a mix or opacity whose unset value is not zero comes back at
//!   that value when set to zero (`global_mix`, `curves_mix`,
//!   `huesat_mix`, `lut_opacity`, `mix`).
//! * #1039: Blend's mix defaults to 0.5 here and 1.0 in the inspector;
//!   the Contrast node's pivot is always BT.1886-encoded; the domains
//!   below are each kind's main stage (the Corrector and Contrast mix
//!   grade-encoded and linear stages); and more.
//! * #1040: Color Match's reference input and Mask's input are counted
//!   and never read; `qual_exposure`, `qual_saturation`, `qual_hue_shift`
//!   and `qual_temperature` are taken and never applied (so they are not
//!   described); Input Transform does nothing in the executor (the
//!   decoder reads `log_format`).
//!
//! Masks are keyframed through their own tracks (`MaskKeyframes`), not
//! through `apply_override`, so a Mask's shapes are not keyframable here.

use crate::NodeKind;
use klipp_effects::registry::{
    Alpha, Category, ColourDomain, NodeTypeDesc, ParamDesc, ParamKind, PortDesc, PortType, Scaling, Spatial, Temporal, Unit, GATE_IN,
};

/// The log wheels' ranges keep clear of mid grey by 0.08, as the wheels
/// themselves bound them (`wheel_shape_sliders`): their rows' ends.
const LOG_LOW_RANGE_MAX: f32 = klipp_color::grade_model::MID_GREY_ENCODED - 0.08;
const LOG_HIGH_RANGE_MIN: f32 = klipp_color::grade_model::MID_GREY_ENCODED + 0.08;

#[path = "registry_presentation.rs"]
pub mod presentation;

/// A grade node's description, tied to its kind.
#[derive(Debug, Clone, Copy)]
pub struct NodeType {
    pub desc: NodeTypeDesc,
    pub kind: NodeKind,
}

/// Every grade node type.
pub fn node_types() -> impl Iterator<Item = &'static NodeType> {
    TYPES.iter()
}

/// The description of `kind`. `None` only for `NodeKind::Unknown`.
pub fn node_type(kind: NodeKind) -> Option<&'static NodeType> {
    node_types().find(|t| t.kind == kind)
}

static P_SOURCE: &[ParamDesc] = &[];
static P_OUTPUT: &[ParamDesc] = &[];
static P_INPUT_TRANSFORM: &[ParamDesc] = &[
    ParamDesc { field: "log_format", index: None, key: "log_format", label: "Camera encoding", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
];
/// A gateable correction's gate taking its key inverted, so it grades where
/// the key isn't: an outside node (#898). Stepped when keyed, as a switch.
const KEY_INVERT: ParamDesc = ParamDesc { field: "key_invert", index: None, key: "key_invert", label: "Invert key", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false };
static P_CORRECTOR: &[ParamDesc] = &[
    ParamDesc { field: "exposure", index: None, key: "exposure", label: "Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-3.0, 3.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "contrast", index: None, key: "contrast", label: "Contrast", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "contrast_pivot", index: None, key: "contrast_pivot", label: "Pivot", kind: ParamKind::Float, default: 0.18, range: None, soft: Some((0.01, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "clarity", index: None, key: "clarity", label: "Clarity", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "saturation", index: None, key: "saturation", label: "Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "vibrance", index: None, key: "vibrance", label: "Vibrance", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "highlights", index: None, key: "highlights", label: "Highlights", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "shadows", index: None, key: "shadows", label: "Shadows", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "whites", index: None, key: "whites", label: "Whites", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "blacks", index: None, key: "blacks", label: "Blacks", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lift_x", index: None, key: "lift_x", label: "Lift x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lift_y", index: None, key: "lift_y", label: "Lift y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lift_master", index: None, key: "lift_master", label: "Lift master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_x", index: None, key: "gamma_x", label: "Gamma x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_y", index: None, key: "gamma_y", label: "Gamma y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_master", index: None, key: "gamma_master", label: "Gamma master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_x", index: None, key: "gain_x", label: "Gain x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_y", index: None, key: "gain_y", label: "Gain y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_master", index: None, key: "gain_master", label: "Gain master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_x", index: None, key: "offset_x", label: "Offset x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_y", index: None, key: "offset_y", label: "Offset y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_master", index: None, key: "offset_master", label: "Offset master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "wheels_log_mode", index: None, key: "wheels_log_mode", label: "Log wheels", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_low_x", index: None, key: "log_low_x", label: "Low x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_low_y", index: None, key: "log_low_y", label: "Low y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_low_master", index: None, key: "log_low_master", label: "Low master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_mid_x", index: None, key: "log_mid_x", label: "Mid x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_mid_y", index: None, key: "log_mid_y", label: "Mid y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_mid_master", index: None, key: "log_mid_master", label: "Mid master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_high_x", index: None, key: "log_high_x", label: "High x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_high_y", index: None, key: "log_high_y", label: "High y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_high_master", index: None, key: "log_high_master", label: "High master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_low_range", index: None, key: "log_low_range", label: "Low range", kind: ParamKind::Float, default: 0.2, range: None, soft: Some((0.0, LOG_LOW_RANGE_MAX)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_high_range", index: None, key: "log_high_range", label: "High range", kind: ParamKind::Float, default: 0.78, range: None, soft: Some((LOG_HIGH_RANGE_MIN, 1.2)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "luma_mix", index: None, key: "luma_mix", label: "Luma mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lift_sat", index: None, key: "lift_sat", label: "Lift saturation", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lift_bri", index: None, key: "lift_bri", label: "Lift brightness", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_sat", index: None, key: "gamma_sat", label: "Gamma saturation", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_bri", index: None, key: "gamma_bri", label: "Gamma brightness", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_sat", index: None, key: "gain_sat", label: "Gain saturation", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_bri", index: None, key: "gain_bri", label: "Gain brightness", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_sat", index: None, key: "offset_sat", label: "Offset saturation", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_bri", index: None, key: "offset_bri", label: "Offset brightness", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "curves", index: None, key: "curves", label: "Curves", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "curves_preserve_luma", index: None, key: "curves_preserve_luma", label: "Preserve luma", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "curves_mix", index: None, key: "curves_mix", label: "Mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "huesat", index: None, key: "huesat", label: "Hue/Sat curves", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "huesat_mix", index: None, key: "huesat_mix", label: "Mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "temperature", index: None, key: "temperature", label: "Temperature", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-50.0, 50.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "tint", index: None, key: "tint", label: "Tint", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-50.0, 50.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hue", index: None, key: "hue", label: "Hue", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 360.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "global_mix", index: None, key: "global_mix", label: "Mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(0), key: "warper_hue_0", label: "Hue 1", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(1), key: "warper_hue_1", label: "Hue 2", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(2), key: "warper_hue_2", label: "Hue 3", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(3), key: "warper_hue_3", label: "Hue 4", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(4), key: "warper_hue_4", label: "Hue 5", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(5), key: "warper_hue_5", label: "Hue 6", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(6), key: "warper_hue_6", label: "Hue 7", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(7), key: "warper_hue_7", label: "Hue 8", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(8), key: "warper_hue_8", label: "Hue 9", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(9), key: "warper_hue_9", label: "Hue 10", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(10), key: "warper_hue_10", label: "Hue 11", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(11), key: "warper_hue_11", label: "Hue 12", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(0), key: "warper_sat_0", label: "Saturation 1", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(1), key: "warper_sat_1", label: "Saturation 2", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(2), key: "warper_sat_2", label: "Saturation 3", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(3), key: "warper_sat_3", label: "Saturation 4", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(4), key: "warper_sat_4", label: "Saturation 5", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(5), key: "warper_sat_5", label: "Saturation 6", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(6), key: "warper_sat_6", label: "Saturation 7", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(7), key: "warper_sat_7", label: "Saturation 8", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(8), key: "warper_sat_8", label: "Saturation 9", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(9), key: "warper_sat_9", label: "Saturation 10", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(10), key: "warper_sat_10", label: "Saturation 11", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(11), key: "warper_sat_11", label: "Saturation 12", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    KEY_INVERT,
];
static P_EXPOSURE: &[ParamDesc] = &[
    ParamDesc { field: "exposure", index: None, key: "exposure", label: "Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-3.0, 3.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    KEY_INVERT,
];
static P_CONTRAST: &[ParamDesc] = &[
    ParamDesc { field: "contrast", index: None, key: "contrast", label: "Contrast", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "contrast_pivot", index: None, key: "contrast_pivot", label: "Pivot", kind: ParamKind::Float, default: 0.18, range: None, soft: Some((0.01, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "clarity", index: None, key: "clarity", label: "Clarity", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    KEY_INVERT,
];
static P_SATURATION: &[ParamDesc] = &[
    ParamDesc { field: "saturation", index: None, key: "saturation", label: "Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "vibrance", index: None, key: "vibrance", label: "Vibrance", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    KEY_INVERT,
];
static P_TONE_CONTROLS: &[ParamDesc] = &[
    ParamDesc { field: "highlights", index: None, key: "highlights", label: "Highlights", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "shadows", index: None, key: "shadows", label: "Shadows", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "whites", index: None, key: "whites", label: "Whites", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "blacks", index: None, key: "blacks", label: "Blacks", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    KEY_INVERT,
];
static P_COLOR_WHEELS: &[ParamDesc] = &[
    ParamDesc { field: "lift_x", index: None, key: "lift_x", label: "Lift x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lift_y", index: None, key: "lift_y", label: "Lift y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lift_master", index: None, key: "lift_master", label: "Lift master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_x", index: None, key: "gamma_x", label: "Gamma x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_y", index: None, key: "gamma_y", label: "Gamma y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_master", index: None, key: "gamma_master", label: "Gamma master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_x", index: None, key: "gain_x", label: "Gain x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_y", index: None, key: "gain_y", label: "Gain y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_master", index: None, key: "gain_master", label: "Gain master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_x", index: None, key: "offset_x", label: "Offset x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_y", index: None, key: "offset_y", label: "Offset y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_master", index: None, key: "offset_master", label: "Offset master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "wheels_log_mode", index: None, key: "wheels_log_mode", label: "Log wheels", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_low_x", index: None, key: "log_low_x", label: "Low x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_low_y", index: None, key: "log_low_y", label: "Low y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_low_master", index: None, key: "log_low_master", label: "Low master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_mid_x", index: None, key: "log_mid_x", label: "Mid x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_mid_y", index: None, key: "log_mid_y", label: "Mid y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_mid_master", index: None, key: "log_mid_master", label: "Mid master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_high_x", index: None, key: "log_high_x", label: "High x", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_high_y", index: None, key: "log_high_y", label: "High y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-2.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_high_master", index: None, key: "log_high_master", label: "High master", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_low_range", index: None, key: "log_low_range", label: "Low range", kind: ParamKind::Float, default: 0.2, range: None, soft: Some((0.0, LOG_LOW_RANGE_MAX)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "log_high_range", index: None, key: "log_high_range", label: "High range", kind: ParamKind::Float, default: 0.78, range: None, soft: Some((LOG_HIGH_RANGE_MIN, 1.2)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "luma_mix", index: None, key: "luma_mix", label: "Luma mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    KEY_INVERT,
];
static P_WHEEL_SAT_BRI: &[ParamDesc] = &[
    ParamDesc { field: "lift_sat", index: None, key: "lift_sat", label: "Lift saturation", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lift_bri", index: None, key: "lift_bri", label: "Lift brightness", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_sat", index: None, key: "gamma_sat", label: "Gamma saturation", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gamma_bri", index: None, key: "gamma_bri", label: "Gamma brightness", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_sat", index: None, key: "gain_sat", label: "Gain saturation", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "gain_bri", index: None, key: "gain_bri", label: "Gain brightness", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_sat", index: None, key: "offset_sat", label: "Offset saturation", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "offset_bri", index: None, key: "offset_bri", label: "Offset brightness", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_RGB_CURVES: &[ParamDesc] = &[
    ParamDesc { field: "curves", index: None, key: "curves", label: "Curves", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "curves_preserve_luma", index: None, key: "curves_preserve_luma", label: "Preserve luma", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "curves_mix", index: None, key: "curves_mix", label: "Mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    KEY_INVERT,
];
static P_HUE_SAT_CURVES: &[ParamDesc] = &[
    ParamDesc { field: "huesat", index: None, key: "huesat", label: "Hue/Sat curves", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "huesat_mix", index: None, key: "huesat_mix", label: "Mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_COLOR_BALANCE: &[ParamDesc] = &[
    ParamDesc { field: "temperature", index: None, key: "temperature", label: "Temperature", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-50.0, 50.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "tint", index: None, key: "tint", label: "Tint", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-50.0, 50.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hue", index: None, key: "hue", label: "Hue", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 360.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "global_mix", index: None, key: "global_mix", label: "Mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    KEY_INVERT,
];
/// HDR zones (#240): per zone (Global, Black, Dark, Shadow, Light,
/// Highlight, Specular) its exposure in stops, colour (its wheel's
/// puck) and saturation; and for every zone but Global its boundary
/// and falloff, in stops from mid grey.
static P_HDR_ZONES: &[ParamDesc] = &[
    ParamDesc { field: "hdr_zones.exposure", index: Some(0), key: "hdr_global_exposure", label: "Global Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-4.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_x", index: Some(0), key: "hdr_global_colour_x", label: "Global Colour X", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_y", index: Some(0), key: "hdr_global_colour_y", label: "Global Colour Y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.saturation", index: Some(0), key: "hdr_global_saturation", label: "Global Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.exposure", index: Some(1), key: "hdr_black_exposure", label: "Black Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-4.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_x", index: Some(1), key: "hdr_black_colour_x", label: "Black Colour X", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_y", index: Some(1), key: "hdr_black_colour_y", label: "Black Colour Y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.saturation", index: Some(1), key: "hdr_black_saturation", label: "Black Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.boundary", index: Some(1), key: "hdr_black_boundary", label: "Black Boundary", kind: ParamKind::Float, default: -4.5, range: None, soft: Some((-8.0, 8.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.falloff", index: Some(1), key: "hdr_black_falloff", label: "Black Falloff", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.exposure", index: Some(2), key: "hdr_dark_exposure", label: "Dark Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-4.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_x", index: Some(2), key: "hdr_dark_colour_x", label: "Dark Colour X", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_y", index: Some(2), key: "hdr_dark_colour_y", label: "Dark Colour Y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.saturation", index: Some(2), key: "hdr_dark_saturation", label: "Dark Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.boundary", index: Some(2), key: "hdr_dark_boundary", label: "Dark Boundary", kind: ParamKind::Float, default: -2.5, range: None, soft: Some((-8.0, 8.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.falloff", index: Some(2), key: "hdr_dark_falloff", label: "Dark Falloff", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.exposure", index: Some(3), key: "hdr_shadow_exposure", label: "Shadow Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-4.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_x", index: Some(3), key: "hdr_shadow_colour_x", label: "Shadow Colour X", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_y", index: Some(3), key: "hdr_shadow_colour_y", label: "Shadow Colour Y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.saturation", index: Some(3), key: "hdr_shadow_saturation", label: "Shadow Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.boundary", index: Some(3), key: "hdr_shadow_boundary", label: "Shadow Boundary", kind: ParamKind::Float, default: -1.0, range: None, soft: Some((-8.0, 8.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.falloff", index: Some(3), key: "hdr_shadow_falloff", label: "Shadow Falloff", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.exposure", index: Some(4), key: "hdr_light_exposure", label: "Light Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-4.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_x", index: Some(4), key: "hdr_light_colour_x", label: "Light Colour X", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_y", index: Some(4), key: "hdr_light_colour_y", label: "Light Colour Y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.saturation", index: Some(4), key: "hdr_light_saturation", label: "Light Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.boundary", index: Some(4), key: "hdr_light_boundary", label: "Light Boundary", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((-8.0, 8.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.falloff", index: Some(4), key: "hdr_light_falloff", label: "Light Falloff", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.exposure", index: Some(5), key: "hdr_highlight_exposure", label: "Highlight Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-4.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_x", index: Some(5), key: "hdr_highlight_colour_x", label: "Highlight Colour X", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_y", index: Some(5), key: "hdr_highlight_colour_y", label: "Highlight Colour Y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.saturation", index: Some(5), key: "hdr_highlight_saturation", label: "Highlight Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.boundary", index: Some(5), key: "hdr_highlight_boundary", label: "Highlight Boundary", kind: ParamKind::Float, default: 2.5, range: None, soft: Some((-8.0, 8.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.falloff", index: Some(5), key: "hdr_highlight_falloff", label: "Highlight Falloff", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.exposure", index: Some(6), key: "hdr_specular_exposure", label: "Specular Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-4.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_x", index: Some(6), key: "hdr_specular_colour_x", label: "Specular Colour X", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.colour_y", index: Some(6), key: "hdr_specular_colour_y", label: "Specular Colour Y", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.saturation", index: Some(6), key: "hdr_specular_saturation", label: "Specular Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.boundary", index: Some(6), key: "hdr_specular_boundary", label: "Specular Boundary", kind: ParamKind::Float, default: 4.5, range: None, soft: Some((-8.0, 8.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "hdr_zones.falloff", index: Some(6), key: "hdr_specular_falloff", label: "Specular Falloff", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_QUALIFIER: &[ParamDesc] = &[
    ParamDesc { field: "qualifier.enabled", index: None, key: "qual_enabled", label: "Enabled", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.mode", index: None, key: "qual_mode", label: "Mode", kind: ParamKind::Choice(&["HSL", "RGB", "3D"]), default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.red_low", index: None, key: "qual_red_low", label: "Red low", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.red_high", index: None, key: "qual_red_high", label: "Red high", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.green_low", index: None, key: "qual_green_low", label: "Green low", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.green_high", index: None, key: "qual_green_high", label: "Green high", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.blue_low", index: None, key: "qual_blue_low", label: "Blue low", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.blue_high", index: None, key: "qual_blue_high", label: "Blue high", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.rgb_soft", index: None, key: "qual_rgb_soft", label: "RGB softness", kind: ParamKind::Float, default: 0.1, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.key_radius", index: None, key: "qual_key_radius", label: "Key reach", kind: ParamKind::Float, default: 2.5, range: None, soft: Some((0.5, 6.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.key_soft", index: None, key: "qual_key_soft", label: "Key softness", kind: ParamKind::Float, default: 1.5, range: None, soft: Some((0.0, 4.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.hue_center", index: None, key: "qual_hue_center", label: "Hue centre", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 360.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.hue_width", index: None, key: "qual_hue_width", label: "Hue width", kind: ParamKind::Float, default: 30.0, range: None, soft: Some((0.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.hue_soft", index: None, key: "qual_hue_soft", label: "Hue softness", kind: ParamKind::Float, default: 10.0, range: None, soft: Some((0.0, 90.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.sat_low", index: None, key: "qual_sat_low", label: "Saturation low", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.sat_high", index: None, key: "qual_sat_high", label: "Saturation high", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.sat_soft", index: None, key: "qual_sat_soft", label: "Saturation softness", kind: ParamKind::Float, default: 0.1, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.lum_low", index: None, key: "qual_lum_low", label: "Luminance low", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.lum_high", index: None, key: "qual_lum_high", label: "Luminance high", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.lum_soft", index: None, key: "qual_lum_soft", label: "Luminance softness", kind: ParamKind::Float, default: 0.1, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.invert", index: None, key: "qual_invert", label: "Invert", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.clean_black", index: None, key: "qual_clean_black", label: "Clean black", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.clean_white", index: None, key: "qual_clean_white", label: "Clean white", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.denoise", index: None, key: "qual_denoise", label: "Denoise", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 10.0)), unit: Unit::CanvasPx, scaling: Scaling::Executor, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.shrink_grow", index: None, key: "qual_shrink_grow", label: "Shrink/grow", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-20.0, 20.0)), unit: Unit::CanvasPx, scaling: Scaling::Executor, keyframable: true, derived: false },
    ParamDesc { field: "qualifier.blur", index: None, key: "qual_blur", label: "Blur", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 50.0)), unit: Unit::CanvasPx, scaling: Scaling::Executor, keyframable: true, derived: false },
];
static P_LUMA_KEY: &[ParamDesc] = &[
    ParamDesc { field: "luma_key.enabled", index: None, key: "luma_key_enabled", label: "Enabled", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "luma_key.low", index: None, key: "luma_key_low", label: "Low", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "luma_key.high", index: None, key: "luma_key_high", label: "High", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "luma_key.soft", index: None, key: "luma_key_soft", label: "Softness", kind: ParamKind::Float, default: 0.05, range: None, soft: Some((0.0, 0.5)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "luma_key.invert", index: None, key: "luma_key_invert", label: "Invert", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_SOFT_CLIP: &[ParamDesc] = &[
    ParamDesc { field: "soft_clip.enabled", index: None, key: "soft_clip.enabled", label: "Enabled", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "soft_clip.threshold", index: None, key: "soft_clip.threshold", label: "Threshold", kind: ParamKind::Float, default: 0.95, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "soft_clip.knee", index: None, key: "soft_clip.knee", label: "Knee", kind: ParamKind::Float, default: 0.05, range: None, soft: Some((0.0, 0.5)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_MASK: &[ParamDesc] = &[
    ParamDesc { field: "masks", index: None, key: "masks", label: "Shapes", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_COMBINE_MATTES: &[ParamDesc] = &[
    ParamDesc { field: "matte_combine_op", index: None, key: "matte_combine_op", label: "Operation", kind: ParamKind::Choice(&["Intersect", "Union", "Subtract"]), default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_KEY_MIXER: &[ParamDesc] = &[
    ParamDesc { field: "key_mixer.inputs", index: None, key: "key_mixer", label: "Inputs", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "key_mixer.output_gain", index: None, key: "key_mixer_output_gain", label: "Output gain", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 2.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "key_mixer.output_offset", index: None, key: "key_mixer_output_offset", label: "Output offset", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_EXTERNAL_MATTE: &[ParamDesc] = &[
    ParamDesc { field: "matte_image_path", index: None, key: "matte_image_path", label: "Image", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "matte_channel", index: None, key: "matte_channel", label: "Channel", kind: ParamKind::Choice(&["Alpha", "Luminance", "Red", "Green", "Blue"]), default: 1.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "matte_invert", index: None, key: "matte_invert", label: "Invert", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    // A tracked subject's matte of the clip's media in place of an image
    // (#1881); settable by graph ops, so a recipe reads the matte the
    // media already has (#2758).
    ParamDesc { field: "matte_subject", index: None, key: "matte_subject", label: "Tracked subject", kind: ParamKind::Choice(&["None", "Subject", "Foreground"]), default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_DIFFERENCE_KEY: &[ParamDesc] = &[
    ParamDesc { field: "difference_key.enabled", index: None, key: "difference_key.enabled", label: "Enabled", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "difference_key.plate_path", index: None, key: "difference_key.plate_path", label: "Clean plate", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    // A frame of the clip's own media as the plate (#2758), by its source
    // seconds; unset, the file.
    ParamDesc { field: "difference_key.plate_frame", index: None, key: "difference_key.plate_frame", label: "Clean frame", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::Seconds, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "difference_key.tolerance", index: None, key: "difference_key.tolerance", label: "Tolerance", kind: ParamKind::Float, default: 0.05, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "difference_key.soft", index: None, key: "difference_key.soft", label: "Softness", kind: ParamKind::Float, default: 0.02, range: None, soft: Some((0.0, 0.5)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "difference_key.invert", index: None, key: "difference_key.invert", label: "Invert", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "difference_key.match_space", index: None, key: "difference_key.match_space", label: "Compare by", kind: ParamKind::Choice(&["RGB", "Luminance"]), default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_LAYER: &[ParamDesc] = &[];
static P_BLEND: &[ParamDesc] = &[
    ParamDesc { field: "mix", index: None, key: "mix", label: "Mix", kind: ParamKind::Float, default: 0.5, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_APPLY_MATTE: &[ParamDesc] = &[
    ParamDesc { field: "mix", index: None, key: "mix", label: "Mix", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_LAYER_MIXER: &[ParamDesc] = &[
    ParamDesc { field: "layer_mixer", index: None, key: "layer_mixer", label: "Inputs", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_LUT_3D: &[ParamDesc] = &[
    ParamDesc { field: "lut_path", index: None, key: "lut_path", label: "LUT file", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "lut_opacity", index: None, key: "lut_opacity", label: "Opacity", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lut_input_space", index: None, key: "lut_input_space", label: "Input space", kind: ParamKind::Choice(&["Rec.709 linear", "ACEScg", "BT.2020 linear", "Use LUT default"]), default: 3.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "lut_output_space", index: None, key: "lut_output_space", label: "Output space", kind: ParamKind::Choice(&["Rec.709 linear", "ACEScg", "BT.2020 linear", "Use LUT default"]), default: 3.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_FILM_GRAIN: &[ParamDesc] = &[
    ParamDesc { field: "grain.enabled", index: None, key: "grain.enabled", label: "Enabled", kind: ParamKind::Bool, default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "grain.amount", index: None, key: "grain.amount", label: "Amount", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "grain.size", index: None, key: "grain.size", label: "Size", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.5, 3.0)), unit: Unit::CanvasPx, scaling: Scaling::Executor, keyframable: false, derived: false },
    ParamDesc { field: "grain.shadows", index: None, key: "grain.shadows", label: "Shadows", kind: ParamKind::Float, default: 1.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "grain.highlights", index: None, key: "grain.highlights", label: "Highlights", kind: ParamKind::Float, default: 0.5, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "grain.monochrome", index: None, key: "grain.monochrome", label: "Monochrome", kind: ParamKind::Bool, default: 1.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_COLOR_MATCH: &[ParamDesc] = &[
    ParamDesc { field: "color_match.exposure", index: None, key: "color_match.exposure", label: "Exposure", kind: ParamKind::Float, default: 0.0, range: None, soft: None, unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "color_match.temperature", index: None, key: "color_match.temperature", label: "Temperature", kind: ParamKind::Float, default: 0.0, range: None, soft: None, unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "color_match.tint", index: None, key: "color_match.tint", label: "Tint", kind: ParamKind::Float, default: 0.0, range: None, soft: None, unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
    ParamDesc { field: "color_match.saturation", index: None, key: "color_match.saturation", label: "Saturation", kind: ParamKind::Float, default: 1.0, range: None, soft: None, unit: Unit::Ratio, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_COLOR_WARPER: &[ParamDesc] = &[
    ParamDesc { field: "warper_params.hue_shifts", index: Some(0), key: "warper_hue_0", label: "Hue 1", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(1), key: "warper_hue_1", label: "Hue 2", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(2), key: "warper_hue_2", label: "Hue 3", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(3), key: "warper_hue_3", label: "Hue 4", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(4), key: "warper_hue_4", label: "Hue 5", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(5), key: "warper_hue_5", label: "Hue 6", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(6), key: "warper_hue_6", label: "Hue 7", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(7), key: "warper_hue_7", label: "Hue 8", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(8), key: "warper_hue_8", label: "Hue 9", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(9), key: "warper_hue_9", label: "Hue 10", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(10), key: "warper_hue_10", label: "Hue 11", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.hue_shifts", index: Some(11), key: "warper_hue_11", label: "Hue 12", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-180.0, 180.0)), unit: Unit::Degrees, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(0), key: "warper_sat_0", label: "Saturation 1", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(1), key: "warper_sat_1", label: "Saturation 2", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(2), key: "warper_sat_2", label: "Saturation 3", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(3), key: "warper_sat_3", label: "Saturation 4", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(4), key: "warper_sat_4", label: "Saturation 5", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(5), key: "warper_sat_5", label: "Saturation 6", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(6), key: "warper_sat_6", label: "Saturation 7", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(7), key: "warper_sat_7", label: "Saturation 8", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(8), key: "warper_sat_8", label: "Saturation 9", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(9), key: "warper_sat_9", label: "Saturation 10", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(10), key: "warper_sat_10", label: "Saturation 11", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "warper_params.sat_shifts", index: Some(11), key: "warper_sat_11", label: "Saturation 12", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((-1.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_DENOISE: &[ParamDesc] = &[
    ParamDesc { field: "denoise_luma", index: None, key: "denoise_luma", label: "Luma", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "denoise_chroma", index: None, key: "denoise_chroma", label: "Chroma", kind: ParamKind::Float, default: 0.0, range: None, soft: Some((0.0, 1.0)), unit: Unit::Ratio, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "denoise_radius", index: None, key: "denoise_radius", label: "Radius", kind: ParamKind::Whole, default: 3.0, range: None, soft: Some((1.0, 7.0)), unit: Unit::CanvasPx, scaling: Scaling::Executor, keyframable: true, derived: false },
];
static P_COMPOUND: &[ParamDesc] = &[
    ParamDesc { field: "compound_subgraph", index: None, key: "compound_subgraph", label: "Graph", kind: ParamKind::Other, default: f32::NAN, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: false, derived: false },
];
static P_RGB_SPLITTER: &[ParamDesc] = &[];
static P_RGB_COMBINER: &[ParamDesc] = &[];
static P_PRIMARIES_MATRIX: &[ParamDesc] = &[
    ParamDesc { field: "primaries_from", index: None, key: "primaries_from", label: "From", kind: ParamKind::Choice(&["Rec.709 linear", "ACEScg", "BT.2020 linear"]), default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
    ParamDesc { field: "primaries_to", index: None, key: "primaries_to", label: "To", kind: ParamKind::Choice(&["Rec.709 linear", "ACEScg", "BT.2020 linear"]), default: 0.0, range: None, soft: None, unit: Unit::None, scaling: Scaling::None, keyframable: true, derived: false },
];
static P_MASK_CUTOUT: &[ParamDesc] = &[];
static P_REROUTE: &[ParamDesc] = &[];

static TYPES: &[NodeType] = &[
    NodeType {
        kind: NodeKind::Source,
        desc: NodeTypeDesc {
            id: "klipp.grade.source",
            name: "Source",
            category: Category::Grade,
            inputs: &[],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_SOURCE,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Output,
        desc: NodeTypeDesc {
            id: "klipp.grade.output",
            name: "Output",
            category: Category::Grade,
            // The grade's picture, and the matte it leaves with as its
            // alpha (#898): the clip then composites by it.
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "alpha", ty: PortType::Matte, optional: true, params: &[] }],
            outputs: &[],
            params: P_OUTPUT,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::InputTransform,
        desc: NodeTypeDesc {
            id: "klipp.grade.input_transform",
            name: "Input Transform",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_INPUT_TRANSFORM,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: false,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Corrector,
        desc: NodeTypeDesc {
            id: "klipp.grade.corrector",
            name: "Corrector",
            category: Category::Grade,
            inputs: GATE_IN,
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_CORRECTOR,
            domain: ColourDomain::GradeEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::Neighbourhood,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: true,
        },
    },
    NodeType {
        kind: NodeKind::Exposure,
        desc: NodeTypeDesc {
            id: "klipp.grade.exposure",
            name: "Exposure",
            category: Category::Grade,
            inputs: GATE_IN,
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_EXPOSURE,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: true,
        },
    },
    NodeType {
        kind: NodeKind::Contrast,
        desc: NodeTypeDesc {
            id: "klipp.grade.contrast",
            name: "Contrast",
            category: Category::Grade,
            inputs: GATE_IN,
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_CONTRAST,
            domain: ColourDomain::GradeEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::Neighbourhood,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: true,
        },
    },
    NodeType {
        kind: NodeKind::Saturation,
        desc: NodeTypeDesc {
            id: "klipp.grade.saturation",
            name: "Saturation",
            category: Category::Grade,
            inputs: GATE_IN,
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_SATURATION,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: true,
        },
    },
    NodeType {
        kind: NodeKind::ToneControls,
        desc: NodeTypeDesc {
            id: "klipp.grade.tone_controls",
            name: "Tone Controls",
            category: Category::Grade,
            inputs: GATE_IN,
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_TONE_CONTROLS,
            domain: ColourDomain::GradeEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: true,
        },
    },
    NodeType {
        kind: NodeKind::ColorWheels,
        desc: NodeTypeDesc {
            id: "klipp.grade.color_wheels",
            name: "Color Wheels",
            category: Category::Grade,
            inputs: GATE_IN,
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_COLOR_WHEELS,
            domain: ColourDomain::GradeEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: true,
        },
    },
    NodeType {
        kind: NodeKind::WheelSatBri,
        desc: NodeTypeDesc {
            id: "klipp.grade.wheel_sat_bri",
            name: "Wheel Sat/Bri",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_WHEEL_SAT_BRI,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::RGBCurves,
        desc: NodeTypeDesc {
            id: "klipp.grade.rgb_curves",
            name: "RGB Curves",
            category: Category::Grade,
            inputs: GATE_IN,
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_RGB_CURVES,
            domain: ColourDomain::DisplayEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: true,
        },
    },
    NodeType {
        kind: NodeKind::HueSatCurves,
        desc: NodeTypeDesc {
            id: "klipp.grade.hue_sat_curves",
            name: "Hue/Sat Curves",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_HUE_SAT_CURVES,
            domain: ColourDomain::DisplayEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::ColorBalance,
        desc: NodeTypeDesc {
            id: "klipp.grade.color_balance",
            name: "Color Balance",
            category: Category::Grade,
            inputs: GATE_IN,
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_COLOR_BALANCE,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: true,
        },
    },
    NodeType {
        kind: NodeKind::HdrZones,
        desc: NodeTypeDesc {
            id: "klipp.grade.hdr_zones",
            name: "HDR Zones",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_HDR_ZONES,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Qualifier,
        desc: NodeTypeDesc {
            id: "klipp.grade.qualifier",
            name: "Qualifier",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "matte", ty: PortType::Matte, optional: false, params: &[] }],
            params: P_QUALIFIER,
            domain: ColourDomain::DisplayEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::Neighbourhood,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::LumaKey,
        desc: NodeTypeDesc {
            id: "klipp.grade.luma_key",
            name: "Luma Key",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "matte", ty: PortType::Matte, optional: false, params: &[] }],
            params: P_LUMA_KEY,
            domain: ColourDomain::DisplayEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::SoftClip,
        desc: NodeTypeDesc {
            id: "klipp.grade.soft_clip",
            name: "Soft Clip",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_SOFT_CLIP,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Mask,
        desc: NodeTypeDesc {
            id: "klipp.grade.mask",
            name: "Mask",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "matte", ty: PortType::Matte, optional: false, params: &[] }],
            params: P_MASK,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::Geometric,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::CombineMattes,
        desc: NodeTypeDesc {
            id: "klipp.grade.combine_mattes",
            name: "Combine Mattes",
            category: Category::Grade,
            inputs: &[PortDesc { key: "a", ty: PortType::Matte, optional: false, params: &[] }, PortDesc { key: "b", ty: PortType::Matte, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "matte", ty: PortType::Matte, optional: false, params: &[] }],
            params: P_COMBINE_MATTES,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::KeyMixer,
        desc: NodeTypeDesc {
            id: "klipp.grade.key_mixer",
            name: "Key Mixer",
            category: Category::Grade,
            // Two to start; more as wires are dropped on it (#898).
            inputs: &[PortDesc { key: "in0", ty: PortType::Matte, optional: false, params: &[] }, PortDesc { key: "in1", ty: PortType::Matte, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "matte", ty: PortType::Matte, optional: false, params: &[] }],
            params: P_KEY_MIXER,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::ExternalMatte,
        desc: NodeTypeDesc {
            id: "klipp.grade.external_matte",
            name: "External Matte",
            category: Category::Grade,
            inputs: &[],
            outputs: &[PortDesc { key: "matte", ty: PortType::Matte, optional: false, params: &[] }],
            params: P_EXTERNAL_MATTE,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::Geometric,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::DifferenceKey,
        desc: NodeTypeDesc {
            id: "klipp.grade.difference_key",
            name: "Difference Key",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "matte", ty: PortType::Matte, optional: false, params: &[] }],
            params: P_DIFFERENCE_KEY,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Layer,
        desc: NodeTypeDesc {
            id: "klipp.grade.layer",
            name: "Layer",
            category: Category::Grade,
            inputs: &[PortDesc { key: "fg", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "bg", ty: PortType::Image, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_LAYER,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Blend,
        desc: NodeTypeDesc {
            id: "klipp.grade.blend",
            name: "Blend",
            category: Category::Grade,
            inputs: &[PortDesc { key: "a", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "b", ty: PortType::Image, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_BLEND,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::ApplyMatte,
        desc: NodeTypeDesc {
            id: "klipp.grade.apply_matte",
            name: "Apply Matte",
            category: Category::Grade,
            inputs: &[PortDesc { key: "graded", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "matte", ty: PortType::Matte, optional: true, params: &[] }, PortDesc { key: "ungraded", ty: PortType::Image, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_APPLY_MATTE,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::LayerMixer,
        desc: NodeTypeDesc {
            id: "klipp.grade.layer_mixer",
            name: "Layer Mixer",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in_1", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "in_2", ty: PortType::Image, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_LAYER_MIXER,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Lut3D,
        desc: NodeTypeDesc {
            id: "klipp.grade.lut_3d",
            name: "3D LUT",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_LUT_3D,
            domain: ColourDomain::DisplayEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::FilmGrain,
        desc: NodeTypeDesc {
            id: "klipp.grade.film_grain",
            name: "Film Grain",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_FILM_GRAIN,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::ColorMatch,
        desc: NodeTypeDesc {
            id: "klipp.grade.color_match",
            name: "Color Match",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "reference", ty: PortType::Image, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_COLOR_MATCH,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::ColorWarper,
        desc: NodeTypeDesc {
            id: "klipp.grade.color_warper",
            name: "Color Warper",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_COLOR_WARPER,
            domain: ColourDomain::DisplayEncoded,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Denoise,
        desc: NodeTypeDesc {
            id: "klipp.grade.denoise",
            name: "Noise Reduction",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_DENOISE,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::Neighbourhood,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Compound,
        desc: NodeTypeDesc {
            id: "klipp.grade.compound",
            name: "Compound",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_COMPOUND,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::RGBSplitter,
        desc: NodeTypeDesc {
            id: "klipp.grade.rgb_splitter",
            name: "RGB Splitter",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "r", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "g", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "b", ty: PortType::Image, optional: false, params: &[] }],
            params: P_RGB_SPLITTER,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::RGBCombiner,
        desc: NodeTypeDesc {
            id: "klipp.grade.rgb_combiner",
            name: "RGB Combiner",
            category: Category::Grade,
            inputs: &[PortDesc { key: "r", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "g", ty: PortType::Image, optional: true, params: &[] }, PortDesc { key: "b", ty: PortType::Image, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_RGB_COMBINER,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::PrimariesMatrix,
        desc: NodeTypeDesc {
            id: "klipp.grade.primaries_matrix",
            name: "Primaries Matrix",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_PRIMARIES_MATRIX,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::MaskCutout,
        desc: NodeTypeDesc {
            id: "klipp.grade.mask_cutout",
            name: "Mask Cutout",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }, PortDesc { key: "matte", ty: PortType::Matte, optional: true, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_MASK_CUTOUT,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: true,
            cpu: true,
            mixable: false,
        },
    },
    NodeType {
        kind: NodeKind::Reroute,
        desc: NodeTypeDesc {
            id: "klipp.grade.reroute",
            name: "Reroute",
            category: Category::Grade,
            inputs: &[PortDesc { key: "in", ty: PortType::Image, optional: false, params: &[] }],
            outputs: &[PortDesc { key: "out", ty: PortType::Image, optional: false, params: &[] }],
            params: P_REROUTE,
            domain: ColourDomain::Linear,
            alpha: Alpha::Straight,
            spatial: Spatial::PerPixel,
            temporal: Temporal::None,
            draws_outside: false,
            gpu: false,
            cpu: true,
            mixable: false,
        },
    },
];
