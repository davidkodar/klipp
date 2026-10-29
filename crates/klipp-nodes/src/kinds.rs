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
    /// Camera log → Rec.709 input transform. The node holds the choice
    /// (`log_format`); the decoder applies it as it decodes, and the
    /// executor passes the picture through (#1040).
    InputTransform,
    /// Bundled "do everything" corrector — exposes the full set of
    /// primary-grade params (exposure, contrast, saturation, vibrance,
    /// tone, wheels, wheel sat/bri, color balance, RGB curves, hue/sat
    /// curves, color warper) on a single node. Default for new clips so
    /// the inspector's tab-based grading UI always has somewhere to
    /// write. Power users can still add the dedicated single-purpose
    /// nodes alongside it for granular pipelines or parallel mixers.
    /// Log-mode wheels work here too (the Wheels tab offers them); they
    /// take the Corrector off the fused GPU chain, as they do a
    /// `ColorWheels` node.
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
    /// HDR zones (#240): exposure, colour and saturation by tonal zone,
    /// each zone's reach set in stops about mid grey (`HdrZonesSettings`).
    HdrZones,

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
    /// It draws its shapes and reads no picture: its counted input is not
    /// read (#1040).
    Mask,
    /// Combine two matte inputs by `params.matte_combine_op` —
    /// intersect (the default, and what this node did when multiplying
    /// was all it could do), union, or A-minus-B. Coverage is combined
    /// in linear light, not as encoded bytes.
    CombineMattes,
    /// Mix any number of mattes into one (#898): each input's coverage,
    /// inverted when asked, times its gain, summed, then the output gain
    /// and offset, clamped to [0, 1] once (see `KeyMixerParams`). A
    /// negative gain subtracts. Its ports grow as wires are dropped onto
    /// it, as `LayerMixer`'s do; it starts with two. With nothing wired it
    /// can use (nothing wired, or every input of the wrong size) it selects
    /// everything, as a matte producer with nothing chosen does, whatever
    /// its output gain and offset: they act on a sum, and there is none.
    KeyMixer,
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
    /// Colour match: an exposure, white balance and saturation worked out
    /// once, by matching the Lab mean and spread of a frame the user picks
    /// ("Apply match"), and applied at render. Port 1 (reference) is
    /// counted for the editor but not read by the executor: the reference
    /// is read when the match is made, not at render (#1040).
    ColorMatch,

    // ── Color Warper ────────────────────────────────────────────
    /// 2D hue/saturation vector grading with 12 sectors.
    ColorWarper,

    // ── Noise Reduction ────────────────────────────────────────
    /// Spatial noise reduction (bilateral filter).
    Denoise,

    /// Holds a nested `NodeGraph` in `params.compound_subgraph`, which
    /// the executor renders recursively in place of the node.
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

    // ── Clip effects (phase 2b M2, step 10) ──────────────────────
    /// One built-in effect from the clip's resolved list (its payload
    /// in `NodeParams::effects`), run as a node of the clip's plan.
    /// Made by the renderer for a run, never by the user or saved.
    Effect,
    /// Effects that run together as one: a spill group, each cast from
    /// the same picture.
    EffectSegment,
    /// A frei0r plug-in from the clip's list (its entry in
    /// `NodeParams::effects`), run on the CPU between the GPU nodes around
    /// it, with instances kept across frames (M4 step 5). Made by the
    /// renderer for a run, never by the user or saved.
    Frei0r,
    /// One OFX plug-in instance (M4 step 3), its plug-in and values in
    /// `NodeParams::ofx`, run on the CPU through the frame context's host
    /// (`crate::plugin`). Made by the renderer for a run.
    Ofx,
    /// A scene-referred clip's display step (step 11c): the SDR tone map
    /// or display light, its settings in `NodeParams::scene_to_display`,
    /// spliced by the renderer after the grade and before the effects.
    /// Made by the renderer for a run, never by the user or saved.
    SceneToDisplay,
    /// A picture handed to the run beside its source (#1746): a media
    /// source's frame, by `NodeParams::side_picture` or the effect it
    /// holds ([`crate::side_picture`]). It reads nothing. Made by the
    /// renderer for a run, never by the user or saved.
    SidePicture,
    /// Where a grade's picture takes the matte wired into its Output's
    /// alpha (#898): port 0 the picture, port 1 the matte; the picture's
    /// alpha times the matte's coverage, its colour as it is. Put at the
    /// grade's end by `NodeGraph::with_gates_applied` (and so before any
    /// effect the renderer splices in, which then moves picture and alpha
    /// together). Made for a run, never by the user or saved.
    OutputAlpha,

    // ── Future ──────────────────────────────────────────────────
    #[serde(other)]
    Unknown,
}

/// Where a node belongs in the order a colourist works in.
///
/// The order is not a style; it is what makes each step mean anything.
/// A balance made *after* a look is a balance fighting the look's own
/// cast, and it will never settle: every correction the loop makes
/// shows up partly undone at the next measurement. A secondary pulled
/// *before* the balance keys on the uncorrected picture, so the key
/// drifts the moment the balance moves under it. A look applied before
/// the balance grades a picture that is still wrong.
///
/// So:
///
/// 1. **Input** — the camera's own encoding undone. Log or raw becomes
///    scene-linear in the working space. Nothing below means anything
///    until this has happened; see `measure::looks_like_log`.
/// 2. **Balance** — the picture made *correct*: neutrals neutral,
///    exposure where the face wants it, contrast placed. This is the
///    step that is measured, and the only one with a right answer.
/// 3. **Secondary** — one thing changed and the rest left alone: skin
///    held on its band, a sky pulled down, a window on a face. Keyed
///    off a balanced picture, so the key is stable.
/// 4. **Look** — the creative pass, which has no right answer and is
///    therefore last among the choices: a LUT, a film emulation, a
///    deliberate cast, grain.
/// 5. **Output** — the delivery encoding. Everything above is in the
///    working space; this is what leaves.
///
/// This is the order of the classic DaVinci node graph and of the ASC's
/// own CDL-then-look convention, and it is the order the grading loop
/// works in: balance to a measured target first, secondaries second,
/// look last, because only the first two have numbers to hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GradeStage {
    Input,
    Balance,
    Secondary,
    Look,
    Output,
    /// Plumbing with no place of its own: reroutes, splitters, mattes
    /// feeding something else. Ordering says nothing about these.
    Plumbing,
}

impl GradeStage {
    pub fn label(&self) -> &'static str {
        match self {
            GradeStage::Input => "input transform",
            GradeStage::Balance => "balance",
            GradeStage::Secondary => "secondary",
            GradeStage::Look => "look",
            GradeStage::Output => "output transform",
            GradeStage::Plumbing => "plumbing",
        }
    }

    /// What this stage is for, in one line.
    pub fn purpose(&self) -> &'static str {
        match self {
            GradeStage::Input => "the camera's encoding undone, so the numbers below mean something",
            GradeStage::Balance => "the picture made correct: neutrals neutral, the face exposed, contrast placed",
            GradeStage::Secondary => "one thing changed and the rest left alone, keyed off a balanced picture",
            GradeStage::Look => "the creative pass, which has no right answer and so comes last",
            GradeStage::Output => "the delivery encoding",
            GradeStage::Plumbing => "wiring",
        }
    }
}

impl NodeKind {
    /// Where this kind belongs in [`GradeStage`]'s order.
    ///
    /// `Corrector` is the bundled do-everything node the inspector
    /// writes to, so it counts as balance: that is what it is used for,
    /// and what the grading loop drives. A `Corrector` gated by a matte
    /// is a secondary instead, which the chain reader works out from the
    /// wiring rather than from the kind.
    pub fn grade_stage(&self) -> GradeStage {
        match self {
            NodeKind::Source => GradeStage::Plumbing,
            NodeKind::Output => GradeStage::Output,
            NodeKind::InputTransform => GradeStage::Input,
            // Balance: everything with a right answer.
            NodeKind::Corrector
            | NodeKind::Exposure
            | NodeKind::Contrast
            | NodeKind::Saturation
            | NodeKind::ToneControls
            | NodeKind::ColorWheels
            | NodeKind::WheelSatBri
            | NodeKind::RGBCurves
            | NodeKind::ColorBalance
            | NodeKind::HdrZones
            | NodeKind::PrimariesMatrix
            | NodeKind::ColorMatch => GradeStage::Balance,
            // Secondary: a key, a window, or a correction gated by one.
            NodeKind::Qualifier
            | NodeKind::LumaKey
            | NodeKind::Mask
            | NodeKind::CombineMattes
            | NodeKind::KeyMixer
            | NodeKind::ExternalMatte
            | NodeKind::DifferenceKey
            | NodeKind::ApplyMatte
            | NodeKind::MaskCutout
            | NodeKind::HueSatCurves => GradeStage::Secondary,
            // Look: no right answer, so last among the choices.
            NodeKind::Lut3D | NodeKind::FilmGrain | NodeKind::ColorWarper | NodeKind::SoftClip => GradeStage::Look,
            // Denoise belongs before anything that would amplify what it
            // leaves behind, which in practice means with the balance.
            NodeKind::Denoise => GradeStage::Balance,
            _ => GradeStage::Plumbing,
        }
    }
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
            | NodeKind::KeyMixer
            | NodeKind::ColorMatch
            | NodeKind::LayerMixer => 2,
            NodeKind::RGBCombiner => 3,
            NodeKind::MaskCutout | NodeKind::OutputAlpha => 2,
            // The picture, and the matte it leaves with as its alpha
            // (#898), when one is wired.
            NodeKind::Output => 2,
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
            // CombineMattes: two mattes combined by its operator.
            (NodeKind::CombineMattes, _) => PortRole::Matte,
            // KeyMixer: every input a matte of equal standing.
            (NodeKind::KeyMixer, _) => PortRole::Matte,
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
            (NodeKind::Output, 1) | (NodeKind::OutputAlpha, 1) => PortRole::Matte,
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
            | (NodeKind::KeyMixer, _)
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
            (NodeKind::KeyMixer, _) => "Key",
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
            (NodeKind::Output, 1) | (NodeKind::OutputAlpha, 1) => "Alpha",
            _ => "Input",
        }
    }

    pub fn output_port_label(&self, port: u8) -> &'static str {
        match (self, port) {
            (NodeKind::Qualifier, _)
            | (NodeKind::LumaKey, _)
            | (NodeKind::Mask, _)
            | (NodeKind::CombineMattes, _)
            | (NodeKind::KeyMixer, _)
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
            NodeKind::HdrZones => "HDR Zones",
            NodeKind::Qualifier => "Qualifier",
            NodeKind::LumaKey => "Luma Key",
            NodeKind::SoftClip => "Soft Clip",
            NodeKind::Mask => "Mask",
            NodeKind::CombineMattes => "Combine Mattes",
            NodeKind::KeyMixer => "Key Mixer",
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
            NodeKind::Effect => "Effect",
            NodeKind::EffectSegment => "Effect Segment",
            NodeKind::Frei0r => "frei0r Plug-in",
            NodeKind::Ofx => "OFX Plug-in",
            NodeKind::SceneToDisplay => "Scene to Display",
            NodeKind::OutputAlpha => "Output Alpha",
            NodeKind::SidePicture => "Side Picture",
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

/// The node kinds a grade can be given by name — the node graph's add
/// menus, the control socket's `add_node` — each with the name it is
/// asked for by, its variant's name. Made from each type's
/// [`Offered`](crate::registry::Offered), so a name the socket accepts is
/// always one the app makes. Source and Output are not here: a graph has
/// exactly one of each.
pub fn addable_node_kinds() -> &'static [(&'static str, NodeKind)] {
    static NAMED: std::sync::OnceLock<Vec<(String, NodeKind)>> = std::sync::OnceLock::new();
    static KINDS: std::sync::OnceLock<Vec<(&'static str, NodeKind)>> = std::sync::OnceLock::new();
    KINDS.get_or_init(|| {
        NAMED
            .get_or_init(|| {
                crate::registry::node_types()
                    .filter(|t| !matches!(t.offered, crate::registry::Offered::Fixed(_)))
                    .map(|t| (format!("{:?}", t.kind), t.kind))
                    .collect()
            })
            .iter()
            .map(|(name, kind)| (name.as_str(), *kind))
            .collect()
    })
}

/// The kind [`addable_node_kinds`] names `name`, or `None` when no node
/// of that name can be added.
pub fn addable_node_kind(name: &str) -> Option<NodeKind> {
    addable_node_kinds().iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
}
