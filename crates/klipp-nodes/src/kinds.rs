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
    /// Bundled "do everything" corrector — exposes the full set of
    /// primary-grade params (exposure, contrast, saturation, vibrance,
    /// tone, wheels, wheel sat/bri, color balance, RGB curves, hue/sat
    /// curves, color warper) on a single node. Default for new clips so
    /// the inspector's tab-based grading UI always has somewhere to
    /// write. Power users can still add the dedicated single-purpose
    /// nodes alongside it for granular pipelines or parallel mixers.
    /// Note: log-mode wheels are NOT supported here — they break the
    /// GPU chain and force a CPU detour. Use a dedicated `ColorWheels`
    /// node when log mode is needed.
    Corrector,
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
    /// Standalone luminance threshold key — generates a matte selecting
    /// pixels whose Rec.709 luma falls inside `[low, high]` with a
    /// soft falloff. Simpler than `Qualifier`; matte-only output.
    LumaKey,
    /// Standalone soft-clip / hard-clip transform — rolls highlights
    /// off below 1.0 with a Reinhard-style asymptotic curve per
    /// channel. Knee=0 = hard clip at threshold; knee&gt;0 = smooth
    /// rolloff. Value transform (not a matte producer).
    SoftClip,
    /// Spatial masks (circle, rectangle, gradient) — generates a matte.
    Mask,
    /// Multiply two matte inputs (port 0 × port 1) for matte intersection.
    CombineMattes,
    /// External image loaded from disk as a matte source (alpha or
    /// luminance / per-channel). No inputs; its only output is a matte
    /// signal that feeds ApplyMatte / CombineMattes. Useful for
    /// scene-specific masks painted in an external tool.
    ExternalMatte,
    /// Difference key — generates a matte by comparing the live frame
    /// to a clean-plate reference loaded from disk. Pixels that
    /// differ from the plate by more than `tolerance` enter the matte.
    /// One input (the live frame); the plate is loaded by the
    /// executor and resized to live-frame dimensions on the fly.
    /// Output is matte-only (R=G=B=matte, A=1).
    DifferenceKey,

    // ── Compositing ─────────────────────────────────────────────
    /// Alpha composite two inputs (A over B). Ports: 0=foreground, 1=background.
    Layer,
    /// Blend two inputs with a scalar mix. Ports: 0=input_a, 1=input_b.
    Blend,
    /// Apply a grade only where a matte selects.
    /// Ports: 0=graded frame, 1=matte, 2=ungraded frame (pre-grade signal).
    ApplyMatte,
    /// N-input compositor — port 0 sits on top, the highest wired port
    /// is the base layer. Each input has its own opacity + blend mode
    /// (see `LayerMixerParams`). Port count grows as wires are dropped
    /// onto the node in the editor; minimum static count is 2.
    LayerMixer,

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

    /// Holds a nested `NodeGraph` in `params.compound_subgraph`.
    /// Acts as a passthrough until stage 5 wires the executor to
    /// recursively render the sub-graph; before that the user can
    /// already create + group + ungroup compounds, which is the
    /// editing affordance the V2 canvas needs first.
    Compound,

    /// RGB Splitter — 1 input, 3 outputs (R / G / B). Each output
    /// port emits the same full-RGBA frame (labelled fan-out); the
    /// grade chain wired to the R branch is the one that grades the
    /// red channel. Channel extraction happens at the matching
    /// `RGBCombiner` on the other side. Matches the DaVinci
    /// convention and avoids a multi-output refactor of the
    /// executor.
    RGBSplitter,

    /// RGB Combiner — 3 inputs (R / G / B), 1 output. Output R comes
    /// from input 0's R channel, G from input 1's G, B from input
    /// 2's B; alpha from input 0. Paired with `RGBSplitter` to
    /// enable per-channel grading.
    RGBCombiner,

    /// Primaries-conversion matrix. 1 input, 1 output. Applies a
    /// fixed 3×3 matrix per pixel in linear-light RGB to move between
    /// working-space bases (e.g. BT.709 ↔ ACEScg AP1). The matrix is
    /// derived from `params.primaries_from` / `primaries_to` via
    /// [`klipp_color::primaries::working_space_conversion_matrix`]; if
    /// either tag is `None` or the pair has no matrix (same source +
    /// dest, or unsupported pairs like BT.2020 stubs), the node acts
    /// as a passthrough.
    ///
    /// Used by the project-wide working-colorspace migration: when
    /// converting an existing Rec.709 project to ACEScg, two
    /// `PrimariesMatrix` wrappers get inserted around each clip's
    /// grade chain (head: AcesCG → Rec709Linear, tail: Rec709Linear →
    /// AcesCG) so the sandwiched grade keeps producing the same
    /// pixels it did before the switch.
    PrimariesMatrix,

    /// Mask cutout — 2 inputs (source image, matte). Output is
    /// `source.rgb * matte.r` so the image shows only where the
    /// matte is non-zero and goes to black outside. Alpha
    /// is kept at 255 so downstream consumers see a normal opaque
    /// frame; use Layer / Blend if you need alpha-aware compositing.
    /// Feathered edges just fall out of the multiply.
    MaskCutout,

    /// Reroute — 1 input, 1 output, no params, no work. The output
    /// is byte-for-byte the input; the executor's bypass path
    /// already does this so the CPU/GPU dispatchers just call
    /// `primary_input.to_vec()`. Sole purpose is wire-routing
    /// hygiene in dense graphs: insert one to bend a long wire
    /// around a cluster of nodes, or to merge a labelled split.
    /// Renders as a tiny dot/diamond rather than a full card so
    /// it doesn't compete with real nodes for visual weight.
    Reroute,

    // ── Future ──────────────────────────────────────────────────
    #[serde(other)]
    Unknown,
}

impl NodeKind {
    /// How many input ports this node has — STATIC minimum count.
    /// `LayerMixer` actually grows past this in practice; UI + executor
    /// must read the dynamic count off the node's params (see
    /// `Node::dynamic_input_port_count`). 2 is the floor so a freshly
    /// added LayerMixer renders with two empty ports ready to wire.
    pub fn input_port_count(&self) -> u8 {
        match self {
            NodeKind::Source => 0,
            // ExternalMatte reads from disk — no inputs, only a matte output.
            NodeKind::ExternalMatte => 0,
            NodeKind::ApplyMatte => 3,
            NodeKind::Blend
            | NodeKind::Layer
            | NodeKind::CombineMattes
            | NodeKind::ColorMatch
            | NodeKind::LayerMixer => 2,
            NodeKind::RGBCombiner => 3,
            NodeKind::MaskCutout => 2,
            _ => 1,
        }
    }

    /// How many output ports this node has.
    pub fn output_port_count(&self) -> u8 {
        match self {
            NodeKind::Output => 0,
            NodeKind::RGBSplitter => 3,
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
            // LayerMixer: every input is a colour signal of equal standing
            // (no foreground/background asymmetry — order alone carries
            // depth, with port 0 on top).
            (NodeKind::LayerMixer, _) => PortRole::Primary,
            // RGB Combiner: port 0 takes the red-channel-graded signal,
            // 1 green, 2 blue. Colour-coded wires make the channel flow
            // legible at a glance.
            (NodeKind::RGBCombiner, 0) => PortRole::ChannelRed,
            (NodeKind::RGBCombiner, 1) => PortRole::ChannelGreen,
            (NodeKind::RGBCombiner, 2) => PortRole::ChannelBlue,
            (NodeKind::MaskCutout, 0) => PortRole::Primary,
            (NodeKind::MaskCutout, 1) => PortRole::Matte,
            // Default — every other (kind, port) is the colour signal.
            _ => PortRole::Primary,
        }
    }

    /// Semantic role of an output port. Today everything outputs a
    /// primary colour signal except for matte producers (Qualifier,
    /// Mask) whose output is itself a matte — useful when wire
    /// colour-coding follows the producer's role rather than the
    /// consumer's.
    pub fn output_port_role(&self, port: u8) -> PortRole {
        match (self, port) {
            (NodeKind::Qualifier, _)
            | (NodeKind::LumaKey, _)
            | (NodeKind::Mask, _)
            | (NodeKind::CombineMattes, _)
            | (NodeKind::ExternalMatte, _)
            | (NodeKind::DifferenceKey, _) => PortRole::Matte,
            (NodeKind::RGBSplitter, 0) => PortRole::ChannelRed,
            (NodeKind::RGBSplitter, 1) => PortRole::ChannelGreen,
            (NodeKind::RGBSplitter, 2) => PortRole::ChannelBlue,
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
            // LayerMixer ports are 1-indexed for users — port 0 reads
            // as "Input 1 (top)" so the stack ordering is unambiguous.
            (NodeKind::LayerMixer, 0) => "Input 1 (top)",
            (NodeKind::LayerMixer, _) => "Input N",
            (NodeKind::RGBCombiner, 0) => "R",
            (NodeKind::RGBCombiner, 1) => "G",
            (NodeKind::RGBCombiner, 2) => "B",
            (NodeKind::MaskCutout, 0) => "Source",
            (NodeKind::MaskCutout, 1) => "Matte",
            _ => "Input",
        }
    }

    pub fn output_port_label(&self, port: u8) -> &'static str {
        match (self, port) {
            (NodeKind::Qualifier, _)
            | (NodeKind::LumaKey, _)
            | (NodeKind::Mask, _)
            | (NodeKind::CombineMattes, _)
            | (NodeKind::ExternalMatte, _)
            | (NodeKind::DifferenceKey, _) => "Matte",
            (NodeKind::RGBSplitter, 0) => "R",
            (NodeKind::RGBSplitter, 1) => "G",
            (NodeKind::RGBSplitter, 2) => "B",
            _ => "Output",
        }
    }

    /// User-facing display name for the add-node menu.
    pub fn display_name(&self) -> &'static str {
        match self {
            NodeKind::Source => "Source",
            NodeKind::Output => "Output",
            NodeKind::InputTransform => "Input Transform",
            NodeKind::Corrector => "Corrector",
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
            NodeKind::LumaKey => "Luma Key",
            NodeKind::SoftClip => "Soft Clip",
            NodeKind::Mask => "Mask",
            NodeKind::CombineMattes => "Combine Mattes",
            NodeKind::ExternalMatte => "External Matte",
            NodeKind::DifferenceKey => "Difference Key",
            NodeKind::Layer => "Layer",
            NodeKind::LayerMixer => "Layer Mixer",
            NodeKind::Blend => "Blend",
            NodeKind::ApplyMatte => "Apply Matte",
            NodeKind::Lut3D => "3D LUT",
            NodeKind::FilmGrain => "Film Grain",
            NodeKind::ColorMatch => "Color Match",
            NodeKind::ColorWarper => "Color Warper",
            NodeKind::Denoise => "Noise Reduction",
            NodeKind::Compound => "Compound",
            NodeKind::RGBSplitter => "RGB Splitter",
            NodeKind::RGBCombiner => "RGB Combiner",
            NodeKind::MaskCutout => "Mask Cutout",
            NodeKind::PrimariesMatrix => "Primaries Matrix",
            NodeKind::Reroute => "Reroute",
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
    /// Red channel signal in an RGB Splitter / Combiner branch.
    ChannelRed,
    /// Green channel signal.
    ChannelGreen,
    /// Blue channel signal.
    ChannelBlue,
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
            PortRole::ChannelRed   => (0xe8, 0x5a, 0x5a),  // bright red
            PortRole::ChannelGreen => (0x5a, 0xd0, 0x6a),  // bright green
            PortRole::ChannelBlue  => (0x5a, 0x8a, 0xe8),  // bright blue
        }
    }
}
