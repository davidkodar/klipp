//! Picture residency inventory (#2065). A GPU compositor is not proof that
//! its sources or styles ran on the GPU. These are path classifications, not
//! promises about every graph or the absence of a device-loss fallback.
use klipp_core::GeneratorData;
use klipp_effects::BuiltinEffect as E;

/// Where picture work happens on the supported GPU composition route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residency {
    /// No intermediate picture download in this operation.
    Gpu,
    /// CPU raster/setup cached for unchanged inputs; later GPU placement.
    CpuCached,
    /// Time-varying CPU picture work followed by upload.
    CpuPerFrame,
    /// GPU picture downloaded for CPU work, then uploaded again.
    RoundTrip,
    /// A separate execution family owns the route; inspect its conditions.
    Conditional,
}

/// A declared path, its limits and the issue tracking its exception.
#[derive(Clone, Copy, Debug)]
pub struct Capability {
    pub residency: Residency,
    pub reason: &'static str,
    pub followup: Option<u32>,
}
const fn cap(residency: Residency, reason: &'static str, followup: Option<u32>) -> Capability {
    Capability {
        residency,
        reason,
        followup,
    }
}

/// Every built-in variant must choose a path: intentionally no wildcard.
/// Device loss and unsupported graph composition can still invoke CPU twins.
pub fn effect(effect: &E) -> Capability {
    use Residency::*;
    match effect {
        E::Crop { .. }
        | E::Transform { .. }
        | E::Opacity { .. }
        | E::ColorFade { .. }
        | E::GaussianBlur { .. }
        | E::FocusBlur { .. }
        | E::Sharpen { .. }
        | E::ChromaKey { .. }
        | E::LumaKey { .. }
        | E::Bloom { .. }
        | E::Glow { .. }
        | E::LightRays { .. }
        | E::Vignette { .. }
        | E::Diffusion { .. }
        | E::LensFlare { .. }
        | E::RoundedCorners { .. }
        | E::DropShadow { .. }
        | E::FillBars { .. }
        | E::OuterGlow { .. }
        | E::LightWrap { .. }
        | E::AnamorphicStreak { .. }
        | E::FilmHalation { .. }
        | E::Invert { .. }
        | E::Posterize { .. }
        | E::Mosaic { .. }
        | E::EdgeDetect { .. }
        | E::ChannelMixer { .. }
        | E::DirectionalBlur { .. }
        | E::Emboss { .. }
        | E::Mirror { .. }
        | E::LensDistortion { .. }
        | E::Displace { .. }
        | E::Dilate { .. }
        | E::Erode { .. }
        | E::Denoise { .. }
        | E::Gamma { .. }
        | E::HSVAdjust { .. }
        | E::Vibrance { .. }
        | E::NoiseGenerator { .. }
        | E::MedianFilter { .. }
        | E::RadialBlur { .. }
        | E::Levels { .. }
        | E::Letterbox { .. }
        | E::ChromaticAberration { .. }
        | E::FilmGrain { .. }
        | E::TiltShift { .. }
        | E::OldFilm { .. }
        | E::Vhs { .. }
        | E::CornerPin { .. }
        | E::Lut3D { .. }
        | E::ColourSpaceTransform { .. }
        | E::PictureTransform { .. }
        | E::Flicker { .. }
        | E::BinocularVignette { .. }
        | E::Particles { .. }
        | E::Generator { .. } => cap(Gpu, "Effect recorder; input and output stay in GPU buffers", None),
        E::Equalize { .. } => cap(Gpu, "Effect recorder; histogram, CDF and map passes stay in GPU buffers", None),
        E::Frei0r { .. } => cap(RoundTrip, "Plugin ABI consumes CPU pixels; explicit local CPU segment", Some(1465)),
        E::TemporalDenoise { .. } => cap(Conditional, "Neighbours on the GPU at the render size (playback, pause, export) use the GPU kernel; software-decoded or fitted neighbours, and the composite stack, still run the CPU kernel", Some(2337)),
        E::MediaSource { .. } | E::ClipBelow { .. } | E::Merge { .. } | E::GradeCompound { .. } | E::SpeedRamp { .. } =>
            cap(Conditional, "Graph/source operation, not an effect-recorder pass; residency depends on connected sources and conversion", Some(1465)),
        E::VideoStabilization { .. } | E::RollingShutterCorrection { .. } | E::Deflicker { .. } =>
            cap(Gpu, "Picture warp/gain is GPU; preparation/analysis is separate offline CPU work", None),
    }
}

/// Native timeline generators are different from the Generator effect node.
/// Static generators cache CPU pixels but upload them to the stack; they are
/// not described as fully GPU generated.
pub fn generator(data: &GeneratorData) -> Capability {
    use GeneratorData::*;
    use Residency::*;
    match data {
        LivingGradient { .. } | Grain { .. } | Countdown | Timer { .. } => cap(
            Gpu,
            "Native stack fast path; CPU fallback when GPU composition is unavailable",
            None,
        ),
        SolidColor { .. }
        | StillFrame { .. }
        | Placeholder { .. }
        | Gradient { .. }
        | ColorBars
        | ColorBars100
        | Slate { .. }
        | Polygon { .. }
        | Star { .. } => cap(
            CpuCached,
            "Still-generator cache; invalidated by authored data, frame size or (a still frame's) file change",
            None,
        ),
    }
}

/// Common composition limitations apply regardless of a kernel's residency.
pub const COMPOSITION_LIMITS: &[Capability] = &[
    cap(Residency::Conditional, "Unavailable/lost GPU or rejected GPU stack falls back to CPU; kernel coverage alone does not establish graph residency", Some(1465)),
    cap(Residency::Conditional, "Eligible animated text/3D styles keep masks and composition on GPU; unsupported title combinations retain CPU composition and large-mask callback downloads", Some(2335)),
    cap(Residency::Conditional, "Eligible animated plain 3D text caches meshes and rasterizes camera/light/glyph poses on GPU; rich authoring combinations retain CPU fallback", Some(2335)),
    cap(Residency::Conditional, "Keyed typography uses cached glyph outlines and GPU placement; unsupported layout combinations retain CPU raster", Some(2335)),
    cap(Residency::Conditional, "Title glitch and chromatic treatment run on GPU for supported sources; unsupported source combinations retain CPU post-processing", Some(2335)),
];

/// Every transition kernel has a GPU stack branch; source seeding and fallback
/// are separate obligations. Exhaustiveness forces review of new kinds.
pub fn transition(kind: klipp_core::transition_registry::TransitionKind) -> Capability {
    use klipp_core::transition_registry::TransitionKind::*;
    match kind {
        CrossDissolve
        | WipeLeft
        | WipeRight
        | WipeUp
        | WipeDown
        | DipToBlack
        | DipToWhite
        | PushLeft
        | FilmDissolve
        | BlurDissolve
        | Flash
        | Spin
        | LightLeak
        | LuminanceDissolve
        | CircleWipeIn
        | CircleWipeOut
        | ClockWipe
        | BarnDoorOpen
        | BarnDoorClose
        | Pixelate
        | Ripple
        | PageTurn
        | PageTurnReverse
        | ChromaticAberration
        | FilmBurn
        | StarWipe
        | SplitWipeHorizontal
        | SplitWipeVertical
        | VenetianBlindHorizontal
        | VenetianBlindVertical
        | CheckerboardWipe
        | DiagonalWipeTLBR
        | DiagonalWipeTRBL
        | BoxWipeIn
        | BoxWipeOut
        | SlideLeft
        | SlideRight
        | SlideUp
        | SlideDown
        | CoverLeft
        | CoverRight
        | UncoverLeft
        | UncoverRight
        | SqueezeH
        | SqueezeV
        | AdditiveDissolve
        | FadeToColor
        | GlitchDissolve
        | Desaturate
        | NegativeFlash
        | Sepia
        | ColorizeFlash
        | HueRotate
        | Swirl
        | WaveDistortion
        | StaticTV
        | VHSTracking
        | GlitchRGBSplit
        | ProjectorFlicker
        | Halation
        | AnamorphicFlare
        | DreamSequence
        | BokehDissolve
        | ZoomInDissolve
        | ZoomOutDissolve
        | CrossZoom
        | WaterDrop
        | WhipPan
        | SlideStrips
        | SlidePanels
        | PartHalves
        | Shatter => cap(Residency::Gpu, "GPU stack transition when both inputs are resident; composition fallback remains possible", None),
        SmoothCut => cap(Residency::RoundTrip, "Optical-flow morph runs on the GPU from the composite route's two held frames, read back after", None),
    }
}

/// A title feature family and the authored fields that select it. Static
/// coverage and time-varying raster work are distinguished in the explanation.
pub struct TitleFamily {
    pub owner: &'static str,
    pub fields: &'static [&'static str],
    pub capability: Capability,
}

/// Field coverage is checked against the public title authoring structures.
/// A new field cannot silently inherit a claim that its pixels stay on GPU.
pub const TITLE_FAMILIES: &[TitleFamily] = &[
    TitleFamily { owner: "TextBox", fields: &["tracking_keyframes"], capability: cap(Residency::Conditional, "Plain opaque nonoverlapping tracking glyphs use cached coverage and GPU placement; unsupported layouts or styling rasterize per frame", Some(2335)) },
    TitleFamily { owner: "Shape", fields: &["kind", "fill", "gradient", "stroke", "fit"], capability: cap(Residency::Conditional, "Simple shapes have GPU placement; vector paths with keyed geometry or placement draw fill (solid or gradient), stroke and glow on GPU from CPU-flattened contours (the shape alone: an unchanged text layer beside it is kept, #2095)", Some(2335)) },
    TitleFamily { owner: "ImageLayer", fields: &["path", "fit"], capability: cap(Residency::Conditional, "Decoded picture uploaded once and fitted to its box on GPU each frame with the CPU's sampling, keyed or not; posed, faded, masked and blended on GPU; layer styles or an unposed motion raster with the title", Some(2335)) },

    TitleFamily { owner: "TextBox", fields: &["text", "font_size", "font_name", "font_style", "color", "alignment", "tracking", "line_spacing", "underline", "strikethrough", "case_transform", "baseline_shift", "drop_cap_scale", "columns", "vertical_layout", "vertical_columns_ltr", "text_direction", "last_line", "vertical_align", "fit", "paragraph", "features", "font_weight", "font_axes", "style_runs"], capability: cap(Residency::CpuCached, "CPU shaping remains; supported layout changes reuse cached outlines with GPU coverage, rich fills and decorations; static finished rasters cache", Some(2335)) },
    TitleFamily { owner: "TextBox", fields: &["font_size_keyframes", "line_spacing_keyframes", "color_keyframes"], capability: cap(Residency::Conditional, "Keyed size, spacing and colour use cached outlines and GPU fill; unsupported bitmap, linked-path and shutter combinations retain CPU raster", Some(2335)) },
    TitleFamily { owner: "TextBox", fields: &["gradient", "pattern_fill", "pattern_fill_enabled", "texture_fill", "write_on", "outline_write_on", "extrusion"], capability: cap(Residency::Conditional, "Supported glyph sources use GPU gradient, pattern and cached texture fills with reveal; extrusion and unsupported source combinations retain raster fallback", Some(2335)) },
    TitleFamily { owner: "TextBox", fields: &["material"], capability: cap(Residency::Conditional, "A metal face (#2706) is shaded on the GPU from the face's coverage, its reflection of the shot drawn as the layers are laid; without a GPU the text draws its colour, and a 3D text is not made metal", Some(2706)) },
    TitleFamily { owner: "TextBox", fields: &["three_d"], capability: cap(Residency::Conditional, "Static solids cache rasters; eligible animated plain solids cache geometry and rasterize on GPU; unsupported combinations retain CPU fallback", Some(2335)) },
    TitleFamily { owner: "TextBox", fields: &["text_path", "char_animation"], capability: cap(Residency::Conditional, "Eligible path travel poses cached glyphs; arc, circle and wave typography uses GPU outlines; linked-path and glyph-motion combinations retain fallback", Some(2335)) },
    TitleFamily { owner: "TextBox", fields: &["chromatic_aberration_px"], capability: cap(Residency::Conditional, "GPU title chromatic processing follows supported sources; unsupported combinations retain cached or animated CPU processing", Some(2335)) },
    TitleFamily { owner: "TextBox", fields: &["glitch_intensity"], capability: cap(Residency::Conditional, "Deterministic GPU title glitch follows supported sources; unsupported combinations retain CPU processing", Some(2335)) },
    TitleFamily { owner: "TextBox", fields: &["legacy_fx", "ass_style_name"], capability: cap(Residency::Conditional, "Migration/round-trip metadata; not a picture-processing kernel", Some(1465)) },
    TitleFamily { owner: "LayerFx", fields: &["motion", "loops", "animators"], capability: cap(Residency::Conditional, "Eligible glyph sources apply movement, scale, opacity, blur, rotation and colour on GPU; focus bands, shutter and unsupported source combinations retain raster fallback", Some(2335)) },
    TitleFamily { owner: "LayerFx", fields: &["glows", "glow_stack_enabled", "shadows", "shadow_stack_enabled", "outlines", "outline_stack_enabled", "inner_shadows", "inner_shadow_stack_enabled", "motion_blur"], capability: cap(Residency::Conditional, "Eligible animated single-text/3D glow, shadow, outline and inner shadow compose on GPU; static titles cache; unsupported styling/motion combinations use CPU or callback downloads", Some(2335)) },
    TitleFamily { owner: "LayerFx", fields: &["edge_fade"], capability: cap(Residency::Conditional, "GPU placement supports eligible edge fades; glyph/motion combinations may rasterize", Some(2335)) },
    TitleFamily { owner: "TitleObject", fields: &["id", "name", "visible", "locked"], capability: cap(Residency::Conditional, "Authoring identity and visibility, no picture kernel of their own", Some(1465)) },
    TitleFamily { owner: "TitleObject", fields: &["transform", "opacity", "opacity_keyframes", "content", "fx", "blend", "mask"], capability: cap(Residency::Conditional, "Supported per-object sources compose blend modes, masks, backings and keyed opacity on GPU; linked-path and fitted-shape context can require CPU composition", Some(2335)) },
    TitleFamily { owner: "ObjectTransform", fields: &["center", "size", "rotation", "anchor_norm"], capability: cap(Residency::Conditional, "Static text layout/rotation participates in the raster cache; eligible simple shapes use GPU placement", Some(2335)) },
    TitleFamily { owner: "ObjectTransform", fields: &["center_x_keyframes", "center_y_keyframes", "size_x_keyframes", "size_y_keyframes", "rotation_keyframes", "anchor_x_keyframes", "anchor_y_keyframes"], capability: cap(Residency::Conditional, "Supported text sources reuse outlines for keyed layout and rotate on GPU; unsupported source combinations retain CPU raster", Some(2335)) },
];

/// Motion kinds must explicitly account for stagger-dependent CPU fallbacks.
pub fn motion(kind: klipp_core::title_motion::MotionKind) -> Capability {
    use klipp_core::title_motion::MotionKind::*;
    match kind {
        Blur | FocusIn | SoftRise => cap(Residency::Conditional, "Whole-layer blur uses GPU placement and supported changing glyph sources blur locally on GPU; unsupported source combinations retain CPU raster", Some(2335)),
        Fade | Rise | Drop | SlideLeft | SlideRight | ScaleUp | ScaleDown | MaskRise | MaskDrop | Typewriter | Pop | Slam | FlyLeft | FlyRight | FlyTop | FlyBottom | Bounce | Flicker | Larger | Smaller =>
            cap(Residency::Conditional, "Cached sprite/glyph pose when eligible; 3D, styling, mixed layers or other keyed properties can require CPU raster", Some(2335)),
    }
}

/// Loops that change coverage cannot be treated as GPU placement alone.
pub fn title_loop(value: &klipp_core::title_motion::TitleLoop) -> Capability {
    use klipp_core::title_motion::TitleLoop::*;
    match value {
        Wave { .. } | ColorCycle { .. } | ColorBlend { .. } | Counter { .. } =>
            cap(Residency::Conditional, "Supported content loops reuse cached glyphs and GPU coverage/fill; unsupported source combinations retain CPU raster", Some(2335)),
        Blink { .. } | Pulse { .. } | Shake { .. } | Scroll { .. } | Crawl { .. } | Float { .. } | Sway { .. } | Drift { .. } | Flicker { .. } =>
            cap(Residency::Conditional, "Single eligible layer poses cached sprite; combined/raster-dependent loops can fall back", Some(2335)),
    }
}
