// Frozen from crates/klipp-effects/src/lib.rs at 7d309e9f. Do not follow app type changes.
use super::Frei0rValue;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    ColorDodge,
    ColorBurn,
    Darken,
    Lighten,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FocalRef {
    Glyph {
        title_clip_id: uuid::Uuid,
        char_index: u32,
        fallback_xy: (f32, f32),
    },
    Tracker {
        clip_id: uuid::Uuid,
        point_index: u32,
        fallback_xy: (f32, f32),
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ColorFadeMode {
    #[default]
    In,
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpillMethod {
    #[default]
    None,
    ChannelClamp,
    Hawk,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuiltinEffect {
    Crop {
        left: f32,
        right: f32,
        top: f32,
        bottom: f32,
        feather: f32,
    },
    Transform {
        position_x: f32,
        position_y: f32,
        scale_x: f32,
        scale_y: f32,
        rotation: f32,
        anchor_x: f32,
        anchor_y: f32,
        #[serde(default)]
        rotation_x: f32,
        #[serde(default)]
        rotation_y: f32,
        #[serde(default)]
        perspective: f32,
        #[serde(default)]
        focal_ref: Option<FocalRef>,
    },
    Opacity {
        value: f32,
    },
    ColorFade {
        color: [u8; 4],
        duration_secs: f32,
        mode: ColorFadeMode,
        #[serde(skip)]
        progress: f32,
    },
    GaussianBlur {
        radius: f32,
    },
    FocusBlur {
        radius: f32,
        vertical: bool,
        band: f32,
    },
    Sharpen {
        amount: f32,
    },
    ChromaKey {
        key_color: [u8; 3],
        tolerance: f32,
        #[serde(alias = "spill_suppression")]
        soft_edge: f32,
        #[serde(default)]
        spill_method: SpillMethod,
        #[serde(default)]
        spill_strength: f32,
    },
    Bloom {
        threshold: f32,
        radius: f32,
        intensity: f32,
    },
    Glow {
        radius: f32,
        intensity: f32,
    },
    LightRays {
        center_x: f32,
        center_y: f32,
        threshold: f32,
        samples: u32,
        decay: f32,
        intensity: f32,
    },
    Vignette {
        amount: f32,
        size: f32,
        softness: f32,
        roundness: f32,
    },
    Diffusion {
        amount: f32,
        radius: f32,
        threshold: f32,
        desaturation: f32,
    },
    LensFlare {
        position_x: f32,
        position_y: f32,
        brightness: f32,
        flare_size: f32,
        ghost_count: u32,
        ghost_intensity: f32,
        chromatic_aberration: f32,
    },
    RoundedCorners {
        radius: f32,
        feather: f32,
    },
    DropShadow {
        offset_x: f32,
        offset_y: f32,
        blur: f32,
        color: [u8; 3],
        opacity: f32,
    },
    FillBars {
        blur: f32,
        zoom: f32,
        darken: f32,
    },
    OuterGlow {
        radius: f32,
        intensity: f32,
        color: [u8; 3],
    },
    LightWrap {
        width: f32,
        intensity: f32,
        softness: f32,
    },
    AnamorphicStreak {
        threshold: f32,
        length: f32,
        intensity: f32,
        tint: f32,
    },
    FilmHalation {
        threshold: f32,
        radius: f32,
        intensity: f32,
        hue: f32,
    },
    Invert {
        mix: f32,
    },
    Posterize {
        levels: f32,
    },
    Mosaic {
        block_size: f32,
    },
    EdgeDetect {
        strength: f32,
        mix: f32,
    },
    ChannelMixer {
        red_red: f32,
        red_green: f32,
        red_blue: f32,
        green_red: f32,
        green_green: f32,
        green_blue: f32,
        blue_red: f32,
        blue_green: f32,
        blue_blue: f32,
    },
    DirectionalBlur {
        angle: f32,
        distance: f32,
        samples: u32,
    },
    Emboss {
        angle: f32,
        strength: f32,
        mix: f32,
    },
    Mirror {
        horizontal: u32,
        vertical: u32,
    },
    LensDistortion {
        distortion: f32,
        scale: f32,
    },
    Dilate {
        radius: u32,
    },
    Erode {
        radius: u32,
    },
    Denoise {
        spatial_sigma: f32,
        color_sigma: f32,
        iterations: u32,
    },
    Gamma {
        gamma: f32,
    },
    HSVAdjust {
        hue_shift: f32,
        saturation: f32,
        value: f32,
    },
    Vibrance {
        amount: f32,
    },
    NoiseGenerator {
        amount: f32,
        monochrome: u32,
        seed: u32,
    },
    MedianFilter {
        radius: u32,
    },
    RadialBlur {
        center_x: f32,
        center_y: f32,
        amount: f32,
        samples: u32,
        mode: u32,
    },
    Levels {
        in_black: f32,
        in_white: f32,
        gamma: f32,
        out_black: f32,
        out_white: f32,
    },
    Equalize {
        strength: f32,
    },
    Letterbox {
        aspect_ratio: f32,
        bar_color: u32,
        opacity: f32,
    },
    ChromaticAberration {
        red_shift: f32,
        blue_shift: f32,
        center_x: f32,
        center_y: f32,
    },
    FilmGrain {
        amount: f32,
        size: f32,
        color: u32,
        shadows: f32,
        seed: u32,
    },
    TiltShift {
        center: f32,
        width: f32,
        softness: f32,
        radius: f32,
    },
    OldFilm {
        flicker: f32,
        scratches: f32,
        dust: f32,
        weave: f32,
        sepia: f32,
        seed: u32,
    },
    Vhs {
        tracking: f32,
        bleed: f32,
        noise: f32,
        scanlines: f32,
        seed: u32,
    },
    RollingShutterCorrection {
        strength: f32,
        readout_ratio: f32,
    },
    TemporalDenoise {
        strength: f32,
        threshold: f32,
    },
    Deflicker {
        strength: f32,
        window_secs: f32,
        gain: f32,
    },
    VideoStabilization {
        smoothing: f32,
        crop_ratio: f32,
        zoom: bool,
    },
    SpeedRamp {
        speed: f32,
        blend_mode: u32,
        ramp_curve: u32,
        ramp_start: f32,
        ramp_end: f32,
        reverse: bool,
        freeze_at: f32,
        freeze: bool,
    },
    Frei0r {
        type_id: String,
        params: Vec<Frei0rValue>,
    },

    CornerPin {
        tl: [f32; 2],
        tr: [f32; 2],
        br: [f32; 2],
        bl: [f32; 2],
    },
}
