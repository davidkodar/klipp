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

    /// Semantic role of an input port — what kind of signal the
    /// node expects on that port. Drives wire colour-coding and
    /// tooltips in the node-graph editor; the executor doesn't read
    /// this (it routes by port index alone). `port` index out of
    /// range returns `PortRole::Primary`.
    pub fn input_port_role(&self, port: u8) -> PortRole {
        match (self, port) {
            // ApplyMatte: graded frame, matte, ungraded frame.
            (NodeKind::ApplyMatte, 0) => PortRole::Primary,   // graded
            (NodeKind::ApplyMatte, 1) => PortRole::Matte,
            (NodeKind::ApplyMatte, 2) => PortRole::PreGrade,
            // Layer: foreground (over) + background (under).
            (NodeKind::Layer, 0) => PortRole::Primary,        // foreground
            (NodeKind::Layer, 1) => PortRole::Background,
            // Blend: two equal-weight inputs.
            (NodeKind::Blend, 0) => PortRole::Primary,
            (NodeKind::Blend, 1) => PortRole::Secondary,
            // CombineMattes: two mattes intersected.
            (NodeKind::CombineMattes, _) => PortRole::Matte,
            // ColorMatch: source + reference frame.
            (NodeKind::ColorMatch, 0) => PortRole::Primary,
            (NodeKind::ColorMatch, 1) => PortRole::Reference,
            // Default — every other (kind, port) is the colour signal.
            _ => PortRole::Primary,
        }
    }

    /// Semantic role of an output port. Today everything outputs a
    /// primary colour signal except for matte producers (Qualifier,
    /// Mask) whose output is itself a matte — useful when wire
    /// colour-coding follows the producer's role rather than the
    /// consumer's.
    pub fn output_port_role(&self, _port: u8) -> PortRole {
        match self {
            NodeKind::Qualifier | NodeKind::Mask | NodeKind::CombineMattes => PortRole::Matte,
            _ => PortRole::Primary,
        }
    }

    /// Short user-facing label for a port — shown as a tooltip in
    /// the editor and (eventually) on the port row in the
    /// inspector.
    pub fn input_port_label(&self, port: u8) -> &'static str {
        match (self, port) {
            (NodeKind::ApplyMatte, 0) => "Graded",
            (NodeKind::ApplyMatte, 1) => "Matte",
            (NodeKind::ApplyMatte, 2) => "Pre-grade",
            (NodeKind::Layer, 0) => "Foreground",
            (NodeKind::Layer, 1) => "Background",
            (NodeKind::Blend, 0) => "Input A",
            (NodeKind::Blend, 1) => "Input B",
            (NodeKind::CombineMattes, 0) => "Matte A",
            (NodeKind::CombineMattes, 1) => "Matte B",
            (NodeKind::ColorMatch, 0) => "Source",
            (NodeKind::ColorMatch, 1) => "Reference",
            _ => "Input",
        }
    }

    pub fn output_port_label(&self, _port: u8) -> &'static str {
        match self {
            NodeKind::Qualifier | NodeKind::Mask | NodeKind::CombineMattes => "Matte",
            _ => "Output",
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

/// Semantic role of a node port — drives editor wire colour-coding,
/// tooltip text, and (eventually) connection compatibility checks.
/// Pure UX; the executor doesn't inspect this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortRole {
    /// The colour signal — the default flow through a grade chain.
    Primary,
    /// A second colour input weighted equally (Blend's input B).
    Secondary,
    /// A backing layer composited under a primary (Layer's bg).
    Background,
    /// A matte / mask signal — single-channel selection used to
    /// gate where a grade applies.
    Matte,
    /// A reference image used for analysis (ColorMatch).
    Reference,
    /// The pre-grade (un-graded) version of the primary, used by
    /// ApplyMatte's compare-pass slot.
    PreGrade,
}

impl PortRole {
    /// Editor-side colour for the port outline + outgoing wire.
    /// Only used by the V2 node-graph editor; semantic.
    pub fn editor_color(&self) -> (u8, u8, u8) {
        match self {
            PortRole::Primary    => (0xc8, 0xa8, 0x4a),  // amber-gold (matches accent)
            PortRole::Secondary  => (0x8a, 0xa8, 0xc8),  // muted blue
            PortRole::Background => (0x6a, 0x7a, 0x8a),  // slate
            PortRole::Matte      => (0xc8, 0x4a, 0xa8),  // magenta
            PortRole::Reference  => (0xe8, 0x88, 0x3a),  // bright orange
            PortRole::PreGrade   => (0x90, 0x90, 0x90),  // grey
        }
    }
}
