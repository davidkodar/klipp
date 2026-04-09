use serde::{Deserialize, Serialize};

use crate::types::TransitionUserParams;

// ---------------------------------------------------------------------------
// TransitionKind enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionKind {
    CrossDissolve,
    WipeLeft,
    WipeRight,
    WipeUp,
    WipeDown,
    DipToBlack,
    DipToWhite,
    PushLeft,
    FilmDissolve,
    BlurDissolve,
    Flash,
    Spin,
    LightLeak,
    LuminanceDissolve,
    CircleWipeIn,
    CircleWipeOut,
    ClockWipe,
    BarnDoorOpen,
    BarnDoorClose,
    Pixelate,
    Ripple,
    PageTurn,
    PageTurnReverse,
    ChromaticAberration,
    FilmBurn,
    StarWipe,
    SplitWipeHorizontal,
    SplitWipeVertical,
    VenetianBlindHorizontal,
    VenetianBlindVertical,
    CheckerboardWipe,
    DiagonalWipeTLBR,
    DiagonalWipeTRBL,
    BoxWipeIn,
    BoxWipeOut,
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
    CoverLeft,
    CoverRight,
    UncoverLeft,
    UncoverRight,
    SqueezeH,
    SqueezeV,
    AdditiveDissolve,
    FadeToColor,
    GlitchDissolve,
    Desaturate,
    NegativeFlash,
    Sepia,
    ColorizeFlash,
    HueRotate,
    Swirl,
    WaveDistortion,
    StaticTV,
    VHSTracking,
    GlitchRGBSplit,
    ProjectorFlicker,
    Halation,
    AnamorphicFlare,
    DreamSequence,
    BokehDissolve,
    ZoomInDissolve,
    ZoomOutDissolve,
    CrossZoom,
}

// ---------------------------------------------------------------------------
// TransitionSpec
// ---------------------------------------------------------------------------

pub struct TransitionSpec {
    pub kind: TransitionKind,
    pub kind_id: u8,
    pub string_key: &'static str,
    pub display_name: &'static str,
    pub default_params: TransitionUserParams,
}

// ---------------------------------------------------------------------------
// Registry — const array of all 66 specs
// ---------------------------------------------------------------------------

pub const TRANSITION_SPECS: [TransitionSpec; 66] = [
    TransitionSpec {
        kind: TransitionKind::CrossDissolve,
        kind_id: 0,
        string_key: "CrossDissolve",
        display_name: "Dissolve",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::WipeLeft,
        kind_id: 1,
        string_key: "WipeLeft",
        display_name: "Wipe L",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::WipeRight,
        kind_id: 2,
        string_key: "WipeRight",
        display_name: "Wipe R",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::WipeUp,
        kind_id: 3,
        string_key: "WipeUp",
        display_name: "Wipe U",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::WipeDown,
        kind_id: 4,
        string_key: "WipeDown",
        display_name: "Wipe D",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::DipToBlack,
        kind_id: 5,
        string_key: "DipToBlack",
        display_name: "Dip Blk",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [0.0, 0.0, 0.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::DipToWhite,
        kind_id: 6,
        string_key: "DipToWhite",
        display_name: "Dip Wht",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::PushLeft,
        kind_id: 7,
        string_key: "PushLeft",
        display_name: "Push L",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::FilmDissolve,
        kind_id: 8,
        string_key: "FilmDissolve",
        display_name: "Film",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::BlurDissolve,
        kind_id: 9,
        string_key: "BlurDissolve",
        display_name: "Blur",
        default_params: TransitionUserParams { param0: 32.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::Flash,
        kind_id: 10,
        string_key: "Flash",
        display_name: "Flash",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.5, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::Spin,
        kind_id: 11,
        string_key: "Spin",
        display_name: "Spin",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.3, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::LightLeak,
        kind_id: 12,
        string_key: "LightLeak",
        display_name: "Leak",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 0.706, 0.235, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::LuminanceDissolve,
        kind_id: 13,
        string_key: "LuminanceDissolve",
        display_name: "Luma",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::CircleWipeIn,
        kind_id: 14,
        string_key: "CircleWipeIn",
        display_name: "Circle In",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.5, param2: 0.5, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::CircleWipeOut,
        kind_id: 15,
        string_key: "CircleWipeOut",
        display_name: "Circle Out",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.5, param2: 0.5, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::ClockWipe,
        kind_id: 16,
        string_key: "ClockWipe",
        display_name: "Clock",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::BarnDoorOpen,
        kind_id: 17,
        string_key: "BarnDoorOpen",
        display_name: "Barn Opn",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::BarnDoorClose,
        kind_id: 18,
        string_key: "BarnDoorClose",
        display_name: "Barn Cls",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::Pixelate,
        kind_id: 19,
        string_key: "Pixelate",
        display_name: "Pixel",
        default_params: TransitionUserParams { param0: 32.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::Ripple,
        kind_id: 20,
        string_key: "Ripple",
        display_name: "Ripple",
        default_params: TransitionUserParams { param0: 20.0, param1: 8.0, param2: 1.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::PageTurn,
        kind_id: 21,
        string_key: "PageTurn",
        display_name: "Page",
        default_params: TransitionUserParams { param0: 0.6, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::PageTurnReverse,
        kind_id: 22,
        string_key: "PageTurnReverse",
        display_name: "Page R",
        default_params: TransitionUserParams { param0: 0.6, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::ChromaticAberration,
        kind_id: 23,
        string_key: "ChromaticAberration",
        display_name: "Chroma",
        default_params: TransitionUserParams { param0: 20.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::FilmBurn,
        kind_id: 24,
        string_key: "FilmBurn",
        display_name: "Burn",
        default_params: TransitionUserParams { param0: 0.8, param1: 1.0, param2: 0.4, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::StarWipe,
        kind_id: 25,
        string_key: "StarWipe",
        display_name: "Star",
        default_params: TransitionUserParams { param0: 1.0, param1: 5.0, param2: 0.4, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::SplitWipeHorizontal,
        kind_id: 26,
        string_key: "SplitWipeHorizontal",
        display_name: "Split H",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::SplitWipeVertical,
        kind_id: 27,
        string_key: "SplitWipeVertical",
        display_name: "Split V",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::VenetianBlindHorizontal,
        kind_id: 28,
        string_key: "VenetianBlindHorizontal",
        display_name: "Blind H",
        default_params: TransitionUserParams { param0: 1.0, param1: 8.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::VenetianBlindVertical,
        kind_id: 29,
        string_key: "VenetianBlindVertical",
        display_name: "Blind V",
        default_params: TransitionUserParams { param0: 1.0, param1: 8.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::CheckerboardWipe,
        kind_id: 30,
        string_key: "CheckerboardWipe",
        display_name: "Check",
        default_params: TransitionUserParams { param0: 1.0, param1: 8.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::DiagonalWipeTLBR,
        kind_id: 31,
        string_key: "DiagonalWipeTLBR",
        display_name: "Diag \\",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::DiagonalWipeTRBL,
        kind_id: 32,
        string_key: "DiagonalWipeTRBL",
        display_name: "Diag /",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::BoxWipeIn,
        kind_id: 33,
        string_key: "BoxWipeIn",
        display_name: "Box In",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::BoxWipeOut,
        kind_id: 34,
        string_key: "BoxWipeOut",
        display_name: "Box Out",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::SlideLeft,
        kind_id: 35,
        string_key: "SlideLeft",
        display_name: "Slide L",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::SlideRight,
        kind_id: 36,
        string_key: "SlideRight",
        display_name: "Slide R",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::SlideUp,
        kind_id: 37,
        string_key: "SlideUp",
        display_name: "Slide U",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::SlideDown,
        kind_id: 38,
        string_key: "SlideDown",
        display_name: "Slide D",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::CoverLeft,
        kind_id: 39,
        string_key: "CoverLeft",
        display_name: "Cover L",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::CoverRight,
        kind_id: 40,
        string_key: "CoverRight",
        display_name: "Cover R",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::UncoverLeft,
        kind_id: 41,
        string_key: "UncoverLeft",
        display_name: "Uncvr L",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::UncoverRight,
        kind_id: 42,
        string_key: "UncoverRight",
        display_name: "Uncvr R",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::SqueezeH,
        kind_id: 43,
        string_key: "SqueezeH",
        display_name: "Sqz H",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::SqueezeV,
        kind_id: 44,
        string_key: "SqueezeV",
        display_name: "Sqz V",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::AdditiveDissolve,
        kind_id: 45,
        string_key: "AdditiveDissolve",
        display_name: "Add Dis",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::FadeToColor,
        kind_id: 46,
        string_key: "FadeToColor",
        display_name: "Fade Clr",
        default_params: TransitionUserParams { param0: 0.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [0.0, 0.0, 0.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::GlitchDissolve,
        kind_id: 47,
        string_key: "GlitchDissolve",
        display_name: "Glitch",
        default_params: TransitionUserParams { param0: 1.0, param1: 16.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::Desaturate,
        kind_id: 48,
        string_key: "Desaturate",
        display_name: "Desat",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::NegativeFlash,
        kind_id: 49,
        string_key: "NegativeFlash",
        display_name: "Neg",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.5, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::Sepia,
        kind_id: 50,
        string_key: "Sepia",
        display_name: "Sepia",
        default_params: TransitionUserParams { param0: 1.0, param1: 1.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::ColorizeFlash,
        kind_id: 51,
        string_key: "ColorizeFlash",
        display_name: "Colorize",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.5, param2: 0.0, param3: 0.0, color: [0.0, 0.8, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::HueRotate,
        kind_id: 52,
        string_key: "HueRotate",
        display_name: "Hue Rot",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::Swirl,
        kind_id: 53,
        string_key: "Swirl",
        display_name: "Swirl",
        default_params: TransitionUserParams { param0: 90.0, param1: 1.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::WaveDistortion,
        kind_id: 54,
        string_key: "WaveDistortion",
        display_name: "Wave",
        default_params: TransitionUserParams { param0: 20.0, param1: 4.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::StaticTV,
        kind_id: 55,
        string_key: "StaticTV",
        display_name: "Static",
        default_params: TransitionUserParams { param0: 1.0, param1: 2.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::VHSTracking,
        kind_id: 56,
        string_key: "VHSTracking",
        display_name: "VHS",
        default_params: TransitionUserParams { param0: 1.0, param1: 6.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::GlitchRGBSplit,
        kind_id: 57,
        string_key: "GlitchRGBSplit",
        display_name: "RGB Spl",
        default_params: TransitionUserParams { param0: 1.0, param1: 8.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::ProjectorFlicker,
        kind_id: 58,
        string_key: "ProjectorFlicker",
        display_name: "Flicker",
        default_params: TransitionUserParams { param0: 1.0, param1: 8.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::Halation,
        kind_id: 59,
        string_key: "Halation",
        display_name: "Halat",
        default_params: TransitionUserParams { param0: 0.3, param1: 1.0, param2: 0.0, param3: 0.0, color: [1.0, 0.4, 0.1, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::AnamorphicFlare,
        kind_id: 60,
        string_key: "AnamorphicFlare",
        display_name: "Flare",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.15, param2: 0.0, param3: 0.0, color: [0.4, 0.7, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::DreamSequence,
        kind_id: 61,
        string_key: "DreamSequence",
        display_name: "Dream",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.8, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::BokehDissolve,
        kind_id: 62,
        string_key: "BokehDissolve",
        display_name: "Bokeh",
        default_params: TransitionUserParams { param0: 1.0, param1: 0.6, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::ZoomInDissolve,
        kind_id: 63,
        string_key: "ZoomInDissolve",
        display_name: "Zoom In",
        default_params: TransitionUserParams { param0: 1.3, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::ZoomOutDissolve,
        kind_id: 64,
        string_key: "ZoomOutDissolve",
        display_name: "Zoom Out",
        default_params: TransitionUserParams { param0: 1.3, param1: 0.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
    TransitionSpec {
        kind: TransitionKind::CrossZoom,
        kind_id: 65,
        string_key: "CrossZoom",
        display_name: "X Zoom",
        default_params: TransitionUserParams { param0: 1.4, param1: 1.0, param2: 0.0, param3: 0.0, color: [1.0, 1.0, 1.0, 1.0] },
    },
];

// Compile-time assertion: exactly 66 entries.
const _: () = assert!(TRANSITION_SPECS.len() == 66);

// ---------------------------------------------------------------------------
// Lookup helpers
// ---------------------------------------------------------------------------

/// Returns the spec for a given `TransitionKind`.
/// Panics if the kind is not registered (invariant: all kinds are in the table).
pub fn spec_for_kind(kind: TransitionKind) -> &'static TransitionSpec {
    let mut i = 0;
    while i < TRANSITION_SPECS.len() {
        // We compare via kind_id since PartialEq isn't available in const,
        // but this is a runtime function so we can use the derived PartialEq.
        if std::mem::discriminant(&TRANSITION_SPECS[i].kind) == std::mem::discriminant(&kind) {
            return &TRANSITION_SPECS[i];
        }
        i += 1;
    }
    panic!("TransitionKind not found in TRANSITION_SPECS — registry invariant violated");
}

/// Returns the spec for a given `kind_id`, or `None` if not found.
pub fn spec_for_kind_id(kind_id: u8) -> Option<&'static TransitionSpec> {
    let mut i = 0;
    while i < TRANSITION_SPECS.len() {
        if TRANSITION_SPECS[i].kind_id == kind_id {
            return Some(&TRANSITION_SPECS[i]);
        }
        i += 1;
    }
    None
}

/// Returns the spec for a given `string_key`, or `None` if not found.
pub fn spec_for_string_key(string_key: &str) -> Option<&'static TransitionSpec> {
    let mut i = 0;
    while i < TRANSITION_SPECS.len() {
        if TRANSITION_SPECS[i].string_key.eq(string_key) {
            return Some(&TRANSITION_SPECS[i]);
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spec_count() {
        assert_eq!(TRANSITION_SPECS.len(), 66);
    }

    #[test]
    fn test_kind_ids_are_contiguous() {
        for (i, spec) in TRANSITION_SPECS.iter().enumerate() {
            assert_eq!(
                spec.kind_id as usize, i,
                "kind_id {} at index {} — expected contiguous IDs",
                spec.kind_id, i,
            );
        }
    }

    #[test]
    fn test_kind_ids_unique() {
        let mut seen = [false; 256];
        for spec in &TRANSITION_SPECS {
            assert!(
                !seen[spec.kind_id as usize],
                "duplicate kind_id: {}",
                spec.kind_id,
            );
            seen[spec.kind_id as usize] = true;
        }
    }

    #[test]
    fn test_string_keys_unique() {
        for (i, a) in TRANSITION_SPECS.iter().enumerate() {
            for (j, b) in TRANSITION_SPECS.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        a.string_key, b.string_key,
                        "duplicate string_key: {}",
                        a.string_key,
                    );
                }
            }
        }
    }

    #[test]
    fn test_spec_for_kind_all_variants() {
        for spec in &TRANSITION_SPECS {
            let found = spec_for_kind(spec.kind);
            assert_eq!(found.kind, spec.kind);
            assert_eq!(found.kind_id, spec.kind_id);
        }
    }

    #[test]
    fn test_spec_for_kind_id_found() {
        for spec in &TRANSITION_SPECS {
            let found = spec_for_kind_id(spec.kind_id).expect("should find spec by kind_id");
            assert_eq!(found.kind, spec.kind);
        }
    }

    #[test]
    fn test_spec_for_kind_id_not_found() {
        assert!(spec_for_kind_id(200).is_none());
    }

    #[test]
    fn test_spec_for_string_key_found() {
        for spec in &TRANSITION_SPECS {
            let found =
                spec_for_string_key(spec.string_key).expect("should find spec by string_key");
            assert_eq!(found.kind, spec.kind);
        }
    }

    #[test]
    fn test_spec_for_string_key_not_found() {
        assert!(spec_for_string_key("Nonexistent").is_none());
    }

    #[test]
    fn test_cross_dissolve_defaults() {
        let spec = spec_for_kind(TransitionKind::CrossDissolve);
        assert_eq!(spec.kind_id, 0);
        assert_eq!(spec.string_key, "CrossDissolve");
        assert_eq!(spec.display_name, "Dissolve");
        assert_eq!(spec.default_params.param0, 0.0);
    }

    #[test]
    fn test_dip_to_black_color() {
        let spec = spec_for_kind(TransitionKind::DipToBlack);
        assert_eq!(spec.default_params.color, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn test_light_leak_color() {
        let spec = spec_for_kind(TransitionKind::LightLeak);
        assert_eq!(spec.default_params.color, [1.0, 0.706, 0.235, 1.0]);
    }

    #[test]
    fn test_blur_dissolve_param0() {
        let spec = spec_for_kind(TransitionKind::BlurDissolve);
        assert_eq!(spec.default_params.param0, 32.0);
    }

    #[test]
    fn test_cross_zoom_params() {
        let spec = spec_for_kind(TransitionKind::CrossZoom);
        assert_eq!(spec.kind_id, 65);
        assert_eq!(spec.default_params.param0, 1.4);
        assert_eq!(spec.default_params.param1, 1.0);
    }
}
