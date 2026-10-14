// Frozen from crates/klipp-nodes/src/kinds.rs at 7d309e9f. Do not follow app type changes.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Source,
    Output,

    InputTransform,
    Corrector,
    Exposure,
    Contrast,
    Saturation,
    ToneControls,
    ColorWheels,
    WheelSatBri,
    RGBCurves,
    HueSatCurves,
    ColorBalance,
    // HDR zones (#240), added after the freeze: a node of it reads.
    HdrZones,

    Qualifier,
    LumaKey,
    SoftClip,
    Mask,
    CombineMattes,
    ExternalMatte,
    DifferenceKey,

    Layer,
    Blend,
    ApplyMatte,
    LayerMixer,

    Lut3D,
    FilmGrain,
    ColorMatch,

    ColorWarper,

    Denoise,

    Compound,

    RGBSplitter,

    RGBCombiner,

    PrimariesMatrix,

    MaskCutout,

    Reroute,

    #[serde(other)]
    Unknown,
}
