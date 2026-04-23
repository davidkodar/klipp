use serde::{Deserialize, Serialize};

/// Every type of node in the color grading graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    // ── Infrastructure ──────────────────────────────────────────
    /// The source frame — always the root. One per graph.
    Source,
    /// The final output — always the sink. One per graph.
    Output,

    // ── Primary Grade (each maps to one step of the old 12-step pipeline) ──
    /// Camera log → Rec.709 input transform.
    InputTransform,
    /// Exposure in stops — single parameter.
    Exposure,
    /// Contrast — single parameter.
    Contrast,
    /// Saturation — single parameter.
    Saturation,
    /// Highlights, Shadows, Whites, Blacks.
    ToneControls,
    /// Lift / Gamma / Gain / Offset color wheels.
    ColorWheels,
    /// Per-wheel Saturation + Brightness.
    WheelSatBri,
    /// RGB Curves (Master + R/G/B with preserve luma + mix).
    RGBCurves,
    /// Hue/Sat Curves (6 parametric curves).
    HueSatCurves,
    /// Temperature + Tint + Hue rotation + Global Mix.
    ColorBalance,

    // ── Secondary Correction ────────────────────────────────────
    /// HSL qualifier — generates a matte from the input frame (matte only).
    Qualifier,
    /// Spatial masks (circle, rectangle, gradient) — generates a matte.
    Mask,
    /// Multiply two matte inputs (port 0 × port 1) for matte intersection.
    CombineMattes,

    // ── Compositing ─────────────────────────────────────────────
    /// Alpha composite two inputs (A over B). Ports: 0=foreground, 1=background.
    Layer,
    /// Blend two inputs with a scalar mix. Ports: 0=input_a, 1=input_b.
    Blend,
    /// Apply a grade only where a matte selects.
    /// Ports: 0=graded frame, 1=matte, 2=ungraded frame (pre-grade signal).
    ApplyMatte,

    // ── Utilities ───────────────────────────────────────────────
    /// 3D LUT (Adobe .cube / .3dl / .csp).
    Lut3D,
    /// Film grain overlay.
    FilmGrain,
    /// Colour match (histogram matching). Ports: 0=input, 1=reference frame.
    ColorMatch,

    // ── Color Warper ────────────────────────────────────────────
    /// 2D hue/saturation vector grading with 12 sectors.
    ColorWarper,

    // ── Noise Reduction ────────────────────────────────────────
    /// Spatial noise reduction (bilateral filter).
    Denoise,

    // ── Future ──────────────────────────────────────────────────
    #[serde(other)]
    Unknown,
}

impl NodeKind {
    /// How many input ports this node has.
    pub fn input_port_count(&self) -> u8 {
        match self {
            NodeKind::Source => 0,
            NodeKind::ApplyMatte => 3,
            NodeKind::Blend | NodeKind::Layer | NodeKind::CombineMattes | NodeKind::ColorMatch => 2,
            _ => 1,
        }
    }

    /// How many output ports this node has.
    pub fn output_port_count(&self) -> u8 {
        match self {
            NodeKind::Output => 0,
            _ => 1,
        }
    }

    /// User-facing display name for the add-node menu.
    pub fn display_name(&self) -> &'static str {
        match self {
            NodeKind::Source => "Source",
            NodeKind::Output => "Output",
            NodeKind::InputTransform => "Input Transform",
            NodeKind::Exposure => "Exposure",
            NodeKind::Contrast => "Contrast",
            NodeKind::Saturation => "Saturation",
            NodeKind::ToneControls => "Tone Controls",
            NodeKind::ColorWheels => "Color Wheels",
            NodeKind::WheelSatBri => "Wheel Sat/Bri",
            NodeKind::RGBCurves => "RGB Curves",
            NodeKind::HueSatCurves => "Hue/Sat Curves",
            NodeKind::ColorBalance => "Color Balance",
            NodeKind::Qualifier => "Qualifier",
            NodeKind::Mask => "Mask",
            NodeKind::CombineMattes => "Combine Mattes",
            NodeKind::Layer => "Layer",
            NodeKind::Blend => "Blend",
            NodeKind::ApplyMatte => "Apply Matte",
            NodeKind::Lut3D => "3D LUT",
            NodeKind::FilmGrain => "Film Grain",
            NodeKind::ColorMatch => "Color Match",
            NodeKind::ColorWarper => "Color Warper",
            NodeKind::Denoise => "Noise Reduction",
            NodeKind::Unknown => "Unknown",
        }
    }
}
