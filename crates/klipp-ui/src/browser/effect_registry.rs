//! Unified effect registry — merges built-in effects and OFX plugins into one browseable list.

use crate::browser::effects_tab::EffectCategory;

/// Whether an effect is built-in or from an OFX plugin.
#[derive(Debug, Clone)]
pub enum EffectSource {
    Builtin(klipp_effects::BuiltinEffect),
    /// A one-click look: several built-in effects applied together.
    Look(Vec<klipp_effects::BuiltinEffect>),
    Ofx(String),   // plugin identifier
    Audio(String), // audio effect type_id (e.g. "eq", "compressor")
    /// External LADSPA plugin. Holds the full discovered metadata so the
    /// inspector can render parameter sliders without a second lookup.
    Ladspa(klipp_audio::ladspa::LadspaPluginInfo),
    /// Third-party frei0r 1.x video plugin. Holds the full discovered
    /// metadata so the inspector can render typed parameter widgets
    /// without a second registry lookup.
    Frei0r(klipp_frei0r::Frei0rPluginInfo),
    /// Third-party LV2 audio plugin. Holds the full discovered
    /// metadata (URI, port list with stable lv2:symbols) so the
    /// inspector can render symbol-keyed sliders without a second
    /// registry lookup. v1 supports control-input + ≤ 2-channel
    /// audio I/O; surround / native-UI / state-extension deferred
    /// to LV2-NEXT-* queue items.
    Lv2(klipp_audio::lv2::Lv2PluginInfo),
    /// Third-party CLAP audio plugin. Holds the full discovered
    /// metadata (plugin ID, vendor, version, params keyed by stable
    /// u32 ParamID) so the inspector can render hex-id-keyed sliders
    /// without a second registry lookup. v1 stub runtime — chain
    /// loads + params round-trip cleanly via ParamID, but the
    /// per-block process() pass-throughs until CLAP-NEXT-RUNTIME-
    /// WIRING ships.
    Clap(klipp_audio::clap::ClapPluginInfo),
}

/// A single entry in the effects browser.
#[derive(Debug, Clone)]
pub struct EffectEntry {
    pub name: String,
    pub category: EffectCategory,
    pub source: EffectSource,
    pub source_label: &'static str,
    pub grouping: String,
    /// Human-readable description (from OFX Describe, empty for built-ins).
    pub description: String,
}

impl EffectEntry {
    /// A built-in entry's catalog type id (`"klipp.fx.bloom"`), its
    /// identity: its presets and what an add puts in a clip are found by
    /// it. `None` for a look, a plug-in or an audio effect.
    pub fn type_id(&self) -> Option<&'static str> {
        match &self.source {
            EffectSource::Builtin(effect) => crate::effect_presets::preset_type_id(effect),
            _ => None,
        }
    }

    /// Whether this is an OFX plug-in that declared it reads frames at
    /// other times: the render path's own test (`OfxPluginInfo::temporal`),
    /// never a guess from its name.
    pub fn is_temporal(&self, ofx_plugins: &[klipp_plugin::OfxPluginInfo]) -> bool {
        match &self.source {
            EffectSource::Ofx(id) => ofx_plugins.iter().any(|p| &p.name == id && p.temporal),
            _ => false,
        }
    }

    /// Whether this effect has a GPU implementation that runs on the
    /// video preview path. Drives the on-thumbnail GPU/CPU badge and the
    /// "GPU only" filter pill.
    ///
    /// Derivation rules:
    /// - **Built-in video effects** route through Klipp's wgpu pipeline
    ///   (`klipp_effects::GpuEffectsPipeline`), so they're always `true`.
    /// - **OFX plugins** are looked up against `available_plugins` for
    ///   their `supports_opengl` flag — passed in by the caller because
    ///   the registry doesn't own that table.
    /// - **Audio / LADSPA** are CPU-only.
    pub fn supports_gpu(&self, ofx_plugins: &[klipp_plugin::OfxPluginInfo]) -> bool {
        match &self.source {
            EffectSource::Builtin(_) | EffectSource::Look(_) => true,
            EffectSource::Ofx(id) => ofx_plugins
                .iter()
                .find(|p| &p.name == id)
                .map(|p| p.supports_opengl)
                .unwrap_or(false),
            EffectSource::Audio(_) | EffectSource::Ladspa(_) => false,
            // frei0r 1.x has no GPU surface: a frei0r node runs on the
            // CPU between the GPU nodes around it (M4 step 5).
            EffectSource::Frei0r(_) => false,
            // LV2 hosting runs on the CPU mix thread (audio effect).
            EffectSource::Lv2(_) => false,
            // CLAP — same story, audio-thread CPU.
            EffectSource::Clap(_) => false,
        }
    }
}

/// A downscaled reference frame used as source for effect thumbnails.
#[derive(Clone)]
pub struct ReferenceFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct EffectRegistry {
    pub effects: Vec<EffectEntry>,
    /// The current reference frame for thumbnail generation.
    pub reference_frame: Option<ReferenceFrame>,
}

impl Default for EffectRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectRegistry {
    pub fn new() -> Self {
        let effects = vec![
            EffectEntry {
                name: "Opacity".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Opacity { value: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "Gaussian Blur".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Fill Bars".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::FillBars { blur: 0.05, zoom: 1.0, darken: 0.25 }),
                source_label: "Built-in",
                description: "Fills letterbox and pillarbox bars, as round an upright phone shot, with the picture itself, enlarged and blurred. Also known as blanking fill, blur fill, blurred background.".into(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Sharpen".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Sharpen { amount: 0.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Crop".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Crop {
                    left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Transform".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Transform {
                    position_x: 0.0, position_y: 0.0,
                    scale_x: 1.0, scale_y: 1.0,
                    rotation: 0.0,
                    anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
                    focal_ref: None }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Corner Pin".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::corner_pin::still()),
                source_label: "Built-in",
                description: "Pin the picture to four corners, in perspective".into(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Chroma Key".into(),
                category: EffectCategory::Keying,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::ChromaKey {
                    key_color: [0, 255, 0],
                    tolerance: 0.2,
                    soft_edge: 0.1,
                    spill_method: klipp_effects::SpillMethod::ChannelClamp,
                    spill_strength: 0.6,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Keying".into(),
            },
            EffectEntry {
                name: "Luma Key".into(),
                category: EffectCategory::Keying,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LumaKey { low: 0.0, high: 0.1, soft: 0.05, invert: false }),
                source_label: "Built-in",
                description: "Key out a range of brightness, black by default: Low to High is see-through.".into(),
                grouping: "Keying".into(),
            },
            // Light effects
            EffectEntry {
                name: "Bloom".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Bloom {
                    threshold: 0.7, radius: 20.0, intensity: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Glow".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Glow {
                    radius: 30.0, intensity: 0.4,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Light Rays".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LightRays {
                    center_x: 0.5, center_y: 0.3,
                    threshold: 0.6, samples: 64,
                    decay: 0.3, intensity: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Vignette".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Vignette {
                    amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Binocular Vignette".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::BinocularVignette {
                    amount: -1.0, spacing: 0.4, width: 0.85, height: 0.85, softness: 0.15, fringe: 4.0,
                }),
                source_label: "Built-in",
                description: "The frame seen through two eyepieces, a colour fringe at their rims.".into(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Diffusion".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Diffusion {
                    amount: 0.3, radius: 25.0, threshold: 0.5, desaturation: 0.2,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Lens Flare".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LensFlare {
                    position_x: 0.7, position_y: 0.3, brightness: 0.6,
                    flare_size: 0.15, ghost_count: 4, ghost_intensity: 0.3,
                    chromatic_aberration: 0.4,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Light Wrap".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LightWrap {
                    width: 15.0, intensity: 0.5, softness: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Anamorphic Streak".into(),
                category: EffectCategory::Light,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::AnamorphicStreak {
                    threshold: 0.7, length: 80.0, intensity: 0.5, tint: 0.3,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Film Halation".into(),
                category: EffectCategory::Film,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::FilmHalation {
                    threshold: 0.75, radius: 15.0, intensity: 0.3, hue: 0.2,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Light".into(),
            },
            EffectEntry {
                name: "Invert".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Invert { mix: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "Posterize".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Posterize { levels: 8.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Mosaic".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Mosaic { block_size: 10.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Edge Detect".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::EdgeDetect { strength: 1.0, mix: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Emboss".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Emboss { angle: 135.0, strength: 1.0, mix: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Channel Mixer".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::ChannelMixer {
                    red_red: 1.0, red_green: 0.0, red_blue: 0.0,
                    green_red: 0.0, green_green: 1.0, green_blue: 0.0,
                    blue_red: 0.0, blue_green: 0.0, blue_blue: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "Directional Blur".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::DirectionalBlur {
                    angle: 0.0, distance: 20.0, samples: 8,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Mirror".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Mirror {
                    horizontal: 1, vertical: 0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Transform".into(),
            },
            EffectEntry {
                name: "Lens Distortion".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::LensDistortion {
                    distortion: 0.0, scale: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Displace".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Displace {
                    amount: 12.0, size: 80.0, speed: 1.0, direction: 0, seed: 0, time: 0.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Rolling Shutter Correction".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::RollingShutterCorrection {
                    strength: 0.5, readout_ratio: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Temporal Noise Reduction".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::TemporalDenoise { strength: 0.5, threshold: 0.02 }),
                source_label: "Built-in",
                description: "Cleans grain by averaging each pixel with the frames either side, leaving moving subjects alone. Runs on the source, before the grade.".into(),
                grouping: "Repair".into(),
            },
            EffectEntry {
                name: "Deflicker".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Deflicker {
                    strength: 0.8, window_secs: 1.0, gain: 1.0,
                }),
                source_label: "Built-in",
                description: "Steadies whole-frame brightness pumping from mains-frequency lighting. Measures the clip first.".into(),
                grouping: "Repair".into(),
            },
            EffectEntry {
                name: "Video Stabilization".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::VideoStabilization {
                    smoothing: 0.5, crop_ratio: 0.05, zoom: true,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "Dilate".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Dilate { radius: 1 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Erode".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Erode { radius: 1 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Denoise".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Denoise {
                    spatial_sigma: 5.0, color_sigma: 0.1, iterations: 1,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Gamma".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Gamma { gamma: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "HSV Adjust".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::HSVAdjust {
                    hue_shift: 0.0, saturation: 1.0, value: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "Noise".into(),
                category: EffectCategory::Film,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::NoiseGenerator {
                    amount: 0.1, monochrome: 0, seed: 0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Film".into(),
            },
            EffectEntry {
                name: "Film Grain".into(),
                category: EffectCategory::Film,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::FilmGrain {
                    amount: 0.3, size: 1.5, color: 0, shadows: 0.6, seed: 0,
                }),
                source_label: "Built-in",
                description: "Animated emulsion grain, heavier in the shadows.".into(),
                grouping: "Film".into(),
            },
            EffectEntry {
                name: "Old Film".into(),
                category: EffectCategory::Film,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::OldFilm {
                    flicker: 0.4, scratches: 0.5, dust: 0.5, weave: 0.5, sepia: 0.6, seed: 0,
                }),
                source_label: "Built-in",
                description: "Flicker, scratches, dust, gate weave and a sepia wash.".into(),
                grouping: "Film".into(),
            },
            EffectEntry {
                name: "Particles".into(),
                category: EffectCategory::Film,
                source: EffectSource::Builtin(crate::effect_presets::particles_default(0)),
                source_label: "Built-in",
                description: "Dust, snow, rain, sparks or embers over the picture, seeded, the same every play.".into(),
                grouping: "Film".into(),
            },
            EffectEntry {
                name: "Flicker".into(),
                category: EffectCategory::Film,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Flicker {
                    amount: 0.5, speed: 8.0, smoothness: 0.0, hold: 0.3, seed: 67, target: 0, time: 0.0,
                }),
                source_label: "Built-in",
                description: "Random jumps of the opacity, exposure or gamma, at a speed, with still spells.".into(),
                grouping: "Film".into(),
            },
            EffectEntry {
                name: "VHS".into(),
                category: EffectCategory::Film,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Vhs {
                    tracking: 0.5, bleed: 0.5, noise: 0.4, scanlines: 0.5, seed: 0,
                }),
                source_label: "Built-in",
                description: "Tape tracking, chroma bleed, noise and scanlines.".into(),
                grouping: "Film".into(),
            },
            EffectEntry {
                name: "Tilt-Shift".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::TiltShift {
                    center: 0.5, width: 0.3, softness: 0.2, radius: 12.0,
                }),
                source_label: "Built-in",
                description: "A sharp band across the frame, blurred above and below.".into(),
                grouping: "Blur".into(),
            },
            // ── Looks: several built-ins in one click, each still its own
            // row in the inspector afterwards. ──
            EffectEntry {
                name: "Cinematic".into(),
                category: EffectCategory::Looks,
                source: EffectSource::Look(vec![
                    klipp_effects::BuiltinEffect::FilmHalation { threshold: 0.65, radius: 30.0, intensity: 0.35, hue: 0.05 },
                    klipp_effects::BuiltinEffect::Vignette { amount: -0.35, size: 0.75, softness: 0.6, roundness: 0.6 },
                    klipp_effects::BuiltinEffect::NoiseGenerator { amount: 0.05, monochrome: 1, seed: 0 },
                ]),
                source_label: "Look",
                description: "Halation on the highlights, a soft vignette and fine grain".into(),
                grouping: "Looks".into(),
            },
            EffectEntry {
                name: "Dreamy".into(),
                category: EffectCategory::Looks,
                source: EffectSource::Look(vec![
                    klipp_effects::BuiltinEffect::Diffusion { amount: 0.35, radius: 40.0, threshold: 0.55, desaturation: 0.2 },
                    klipp_effects::BuiltinEffect::Glow { radius: 30.0, intensity: 0.25 },
                    klipp_effects::BuiltinEffect::Vignette { amount: -0.25, size: 0.8, softness: 0.7, roundness: 0.7 },
                ]),
                source_label: "Look",
                description: "Pro-mist diffusion with a gentle glow".into(),
                grouping: "Looks".into(),
            },
            EffectEntry {
                name: "Punchy".into(),
                category: EffectCategory::Looks,
                source: EffectSource::Look(vec![
                    klipp_effects::BuiltinEffect::Levels { in_black: 0.03, in_white: 0.97, gamma: 1.0, out_black: 0.0, out_white: 1.0 },
                    klipp_effects::BuiltinEffect::Vibrance { amount: 0.35 },
                    klipp_effects::BuiltinEffect::Sharpen { amount: 0.5 },
                    klipp_effects::BuiltinEffect::Vignette { amount: -0.2, size: 0.85, softness: 0.5, roundness: 0.5 },
                ]),
                source_label: "Look",
                description: "Contrast, vibrance and bite for social delivery".into(),
                grouping: "Looks".into(),
            },
            EffectEntry {
                name: "Bleach Bypass".into(),
                category: EffectCategory::Looks,
                source: EffectSource::Look(vec![
                    klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.45, value: 1.0 },
                    klipp_effects::BuiltinEffect::Levels { in_black: 0.08, in_white: 0.92, gamma: 1.15, out_black: 0.0, out_white: 1.0 },
                    klipp_effects::BuiltinEffect::NoiseGenerator { amount: 0.05, monochrome: 1, seed: 0 },
                ]),
                source_label: "Look",
                description: "Desaturated, high contrast, grainy".into(),
                grouping: "Looks".into(),
            },
            EffectEntry {
                name: "Vintage".into(),
                category: EffectCategory::Looks,
                source: EffectSource::Look(vec![
                    klipp_effects::BuiltinEffect::ChannelMixer {
                        red_red: 1.0, red_green: 0.1, red_blue: 0.0,
                        green_red: 0.0, green_green: 0.95, green_blue: 0.05,
                        blue_red: 0.0, blue_green: 0.1, blue_blue: 0.8,
                    },
                    klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.75, value: 0.98 },
                    klipp_effects::BuiltinEffect::Vignette { amount: -0.5, size: 0.7, softness: 0.6, roundness: 0.5 },
                    klipp_effects::BuiltinEffect::NoiseGenerator { amount: 0.08, monochrome: 1, seed: 0 },
                ]),
                source_label: "Look",
                description: "Warm, faded, vignetted, grainy".into(),
                grouping: "Looks".into(),
            },
            EffectEntry {
                name: "Anamorphic".into(),
                category: EffectCategory::Looks,
                source: EffectSource::Look(vec![
                    klipp_effects::BuiltinEffect::AnamorphicStreak { threshold: 0.75, length: 120.0, intensity: 0.5, tint: 0.6 },
                    klipp_effects::BuiltinEffect::FilmHalation { threshold: 0.7, radius: 24.0, intensity: 0.25, hue: 0.05 },
                    klipp_effects::BuiltinEffect::Letterbox { aspect_ratio: 2.39, bar_color: 0, opacity: 1.0 },
                ]),
                source_label: "Look",
                description: "Blue streaks on the highlights, halation and a 2.39 frame".into(),
                grouping: "Looks".into(),
            },
            EffectEntry {
                name: "Median Filter".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::MedianFilter { radius: 1 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Radial Blur".into(),
                category: EffectCategory::Blur,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::RadialBlur {
                    center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 12, mode: 0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Blur".into(),
            },
            EffectEntry {
                name: "Levels".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Levels {
                    in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "Equalize".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Equalize { strength: 1.0 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "Picture Transform".into(),
                category: EffectCategory::Distort,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::PictureTransform {
                    position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
                }),
                source_label: "Built-in",
                description: "Moves, scales and turns the picture inside the clip's frame, where it sits".into(),
                grouping: "Distort".into(),
            },
            EffectEntry {
                name: "3D LUT".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Lut3D {
                    path: String::new(), opacity: 1.0, tetrahedral: true, prepared: None,
                }),
                source_label: "Built-in",
                description: "A .cube, .3dl or .csp look over the picture".into(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "Colour Space Transform".into(),
                category: EffectCategory::Color,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::ColourSpaceTransform {
                    input_gamut: 0, input_curve: 0, output_gamut: 0, output_curve: 0, prepared: None,
                }),
                source_label: "Built-in",
                description: "The picture from one gamut and curve to another".into(),
                grouping: "Colour".into(),
            },
            EffectEntry {
                name: "Letterbox".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::Letterbox {
                    aspect_ratio: 2.39, bar_color: 0, opacity: 1.0,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Chromatic Aberration".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::ChromaticAberration {
                    red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5,
                }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            EffectEntry {
                name: "Speed Ramp".into(),
                category: EffectCategory::Stylize,
                source: EffectSource::Builtin(klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 1 }),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Stylize".into(),
            },
            // ── Audio effects ──
            EffectEntry {
                name: "Parametric EQ".into(),
                category: EffectCategory::AudioEQ,
                source: EffectSource::Audio("eq".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "EQ".into(),
            },
            EffectEntry {
                name: "Pitch Shift".into(),
                category: EffectCategory::AudioPitch,
                source: EffectSource::Audio("pitch_shift".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Pitch".into(),
            },
            EffectEntry {
                name: "Compressor".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("compressor".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Limiter".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("limiter".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "Noise Gate".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("gate".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "High-pass Filter".into(),
                category: EffectCategory::AudioRestoration,
                source: EffectSource::Audio("high_pass".into()),
                source_label: "Built-in",
                description: "Remove low-frequency rumble and wind".into(),
                grouping: "Restoration".into(),
            },
            EffectEntry {
                name: "Broadband Denoiser".into(),
                category: EffectCategory::AudioRestoration,
                source: EffectSource::Audio("denoiser".into()),
                source_label: "Built-in",
                description: "Learn and reduce steady background noise".into(),
                grouping: "Restoration".into(),
            },
            EffectEntry {
                name: "Voice Isolation".into(),
                category: EffectCategory::AudioRestoration,
                source: EffectSource::Audio("voice_isolation".into()),
                source_label: "Built-in",
                description: "Separate speech from wind, traffic and room noise (AI)".into(),
                grouping: "Restoration".into(),
            },
            EffectEntry {
                name: "De-Esser".into(),
                category: EffectCategory::AudioRestoration,
                source: EffectSource::Audio("deesser".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Restoration".into(),
            },
            EffectEntry {
                name: "Normalize".into(),
                category: EffectCategory::AudioUtility,
                source: EffectSource::Audio("normalize".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Utility".into(),
            },
            EffectEntry {
                // this effect is a multi-band gate, not
                // spectral-subtraction NR. The internal type_id stays
                // "noise_reduction" so saved .klipp projects keep
                // loading; the user-facing name advertises the honest
                // behaviour.
                name: "Multi-band Gate".into(),
                category: EffectCategory::AudioDynamics,
                source: EffectSource::Audio("noise_reduction".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Dynamics".into(),
            },
            EffectEntry {
                name: "De-click".into(),
                category: EffectCategory::AudioRestoration,
                source: EffectSource::Audio("declick".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Restoration".into(),
            },
            EffectEntry {
                name: "Reverb".into(),
                category: EffectCategory::AudioReverb,
                source: EffectSource::Audio("reverb".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Reverb".into(),
            },
            EffectEntry {
                name: "Delay".into(),
                category: EffectCategory::AudioDelay,
                source: EffectSource::Audio("delay".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Delay".into(),
            },
            EffectEntry {
                name: "Chorus".into(),
                category: EffectCategory::AudioModulation,
                source: EffectSource::Audio("chorus".into()),
                source_label: "Built-in",
                description: String::new(),
                grouping: "Modulation".into(),
            },
        ];

        Self { effects, reference_frame: None }
    }

    /// Merge discovered LADSPA plugins into the registry. One `EffectEntry`
    /// per plugin; grouping is the plugin maker so the browser buckets them
    /// sensibly. Discovery already de-dupes by unique_id.
    pub fn merge_ladspa_plugins(
        &mut self,
        plugins: &[klipp_audio::ladspa::LadspaPluginInfo],
    ) {
        for plugin in plugins {
            let name = if plugin.name.is_empty() {
                plugin.label.clone()
            } else {
                plugin.name.clone()
            };
            let grouping = if plugin.maker.is_empty() {
                "LADSPA".to_string()
            } else {
                plugin.maker.clone()
            };
            let description = if plugin.maker.is_empty() {
                format!("LADSPA plugin: {}", name)
            } else {
                format!("LADSPA plugin: {} by {}", name, plugin.maker)
            };
            self.effects.push(EffectEntry {
                category: ladspa_category_from_name(&name, &plugin.label),
                name,
                source: EffectSource::Ladspa(plugin.clone()),
                source_label: "LADSPA",
                description,
                grouping,
            });
        }
    }

    /// Merge discovered frei0r plugins into the registry. Filter-only —
    /// SOURCE / MIXER2 / MIXER3 plugins are kept in the runtime cache
    /// (so v2 doesn't need to rescan) but skipped here.
    ///
    /// `grouping` is set to "frei0r" rather than the plugin author so
    /// the browser buckets the entire ecosystem together — matches the
    /// LADSPA precedent, which uses the maker only when present (most
    /// frei0r authors leave it blank).
    pub fn merge_frei0r_plugins(&mut self, plugins: &[klipp_frei0r::Frei0rPluginInfo]) {
        for plugin in plugins {
            if !matches!(plugin.plugin_type, klipp_frei0r::PluginType::Filter) {
                // SOURCE / MIXER2 / MIXER3 — a generator and the
                // two- and three-input blends. The browser lists
                // filters only, so these are skipped silently.
                continue;
            }
            let grouping = if plugin.author.is_empty() {
                "frei0r".to_string()
            } else {
                plugin.author.clone()
            };
            let description = if plugin.explanation.is_empty() {
                format!("frei0r plugin: {}", plugin.name)
            } else {
                format!("{}: {}", plugin.name, plugin.explanation)
            };
            self.effects.push(EffectEntry {
                category: frei0r_category_from_name(&plugin.name),
                name: plugin.name.clone(),
                source: EffectSource::Frei0r(plugin.clone()),
                source_label: "frei0r",
                description,
                grouping,
            });
        }
    }

    /// Merge discovered LV2 audio plugins into the registry. One
    /// entry per plugin; grouping defaults to "LV2" since most
    /// plugins don't expose a maker field at the LV2 introspection
    /// level. The category heuristic looks at the plugin name +
    /// classes for common substrings (Reverb / Compressor / etc.).
    pub fn merge_lv2_plugins(&mut self, plugins: &[klipp_audio::lv2::Lv2PluginInfo]) {
        for plugin in plugins {
            self.effects.push(EffectEntry {
                category: lv2_category_from_name(&plugin.name),
                name: plugin.name.clone(),
                source: EffectSource::Lv2(plugin.clone()),
                source_label: "LV2",
                description: format!("LV2 plugin: {}", plugin.uri),
                grouping: "LV2".to_string(),
            });
        }
    }

    /// Merge discovered CLAP plugins into the registry. One entry
    /// per plugin; grouping is the vendor when present, "CLAP"
    /// otherwise. Category heuristic substring-matches on the name.
    pub fn merge_clap_plugins(&mut self, plugins: &[klipp_audio::clap::ClapPluginInfo]) {
        for plugin in plugins {
            let grouping = if plugin.vendor.is_empty() {
                "CLAP".to_string()
            } else {
                plugin.vendor.clone()
            };
            let description = if plugin.vendor.is_empty() {
                format!("CLAP plugin: {}", plugin.id)
            } else {
                format!("by {}", plugin.vendor)
            };
            self.effects.push(EffectEntry {
                // Reuses the LV2 category heuristic — substring match
                // on the name covers most cases (Reverb / Compressor /
                // EQ / Delay / Modulation / Pitch).
                category: lv2_category_from_name(&plugin.name),
                name: plugin.name.clone(),
                source: EffectSource::Clap(plugin.clone()),
                source_label: "CLAP",
                description,
                grouping,
            });
        }
    }

    /// Merge discovered OFX plugins into the registry, skipping duplicates.
    /// The host already lists each identifier once, as the version that
    /// renders it (the highest); the check here only guards a list built
    /// some other way.
    pub fn merge_ofx_plugins(&mut self, plugins: &[klipp_plugin::OfxPluginInfo]) {
        let mut seen = std::collections::HashSet::new();
        for plugin in plugins {
            if !seen.insert(plugin.name.clone()) {
                continue;
            }
            let display_name = format_ofx_name(&plugin.name);
            let category = category_from_grouping(&plugin.grouping);

            self.effects.push(EffectEntry {
                name: display_name,
                category,
                source: EffectSource::Ofx(plugin.name.clone()),
                source_label: "OFX",
                description: plugin.description.clone(),
                grouping: plugin.grouping.clone(),
            });
        }
    }

    /// Set the reference frame from a full-resolution RGBA buffer.
    /// Downscales to thumbnail size (192x108) using nearest-neighbor.
    pub fn set_reference_frame(&mut self, rgba: &[u8], src_w: u32, src_h: u32) {
        const THUMB_W: u32 = 192;
        const THUMB_H: u32 = 108;

        if rgba.len() != (src_w * src_h * 4) as usize || src_w == 0 || src_h == 0 {
            return;
        }

        let mut thumb = vec![0u8; (THUMB_W * THUMB_H * 4) as usize];
        for ty in 0..THUMB_H {
            for tx in 0..THUMB_W {
                let sx = (tx as f32 / THUMB_W as f32 * src_w as f32) as u32;
                let sy = (ty as f32 / THUMB_H as f32 * src_h as f32) as u32;
                let si = ((sy * src_w + sx) * 4) as usize;
                let di = ((ty * THUMB_W + tx) * 4) as usize;
                thumb[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
            }
        }

        self.reference_frame = Some(ReferenceFrame {
            rgba: thumb,
            width: THUMB_W,
            height: THUMB_H,
        });
    }

    /// Filter effects by category and search query, sorted alphabetically.
    /// `favorite_names` backs the `EffectCategory::Favourites` pseudo-category
    /// — when the caller doesn't need it (sidebar count queries, most tests)
    /// an empty slice is fine.
    pub fn filtered(&self, category: EffectCategory, search: &str) -> Vec<&EffectEntry> {
        self.filtered_with_favorites(category, search, &[])
    }

    pub fn filtered_with_favorites(
        &self,
        category: EffectCategory,
        search: &str,
        favorite_names: &[String],
    ) -> Vec<&EffectEntry> {
        self.filtered_full(category, search, favorite_names, &[])
    }

    /// Like [`Self::filtered_with_favorites`] but also accepts a `recent_names`
    /// slice that backs the [`EffectCategory::Recent`] pseudo-category.
    /// MRU order is preserved when `category == Recent`; every other
    /// category keeps the alphabetical sort the rest of the UI expects.
    pub fn filtered_full(
        &self,
        category: EffectCategory,
        search: &str,
        favorite_names: &[String],
        recent_names: &[String],
    ) -> Vec<&EffectEntry> {
        let search_lower = search.to_lowercase();
        let mut result: Vec<&EffectEntry> = self
            .effects
            .iter()
            .filter(|e| {
                let e_is_audio = matches!(
                    e.category,
                    EffectCategory::AudioEQ
                        | EffectCategory::AudioDynamics
                        | EffectCategory::AudioReverb
                        | EffectCategory::AudioDelay
                        | EffectCategory::AudioModulation
                        | EffectCategory::AudioPitch
                        | EffectCategory::AudioRestoration
                        | EffectCategory::AudioUtility
                ) || matches!(e.source, EffectSource::Ladspa(_));
                let cat_match = match category {
                    // Favourites: show every effect the user has starred,
                    // video or audio alike. Empty favorites list → empty
                    // grid (handled naturally by the contains() check).
                    EffectCategory::Favourites => favorite_names.contains(&e.name),
                    EffectCategory::Recent => recent_names.contains(&e.name),
                    // Installed = every registered effect, video + audio.
                    EffectCategory::Installed => true,
                    EffectCategory::All => !e_is_audio, // All = all video effects
                    EffectCategory::AudioAll => e_is_audio, // AudioAll = all audio effects
                    _ => e.category == category,
                };
                cat_match
                    && (search_lower.is_empty()
                        || e.name.to_lowercase().contains(&search_lower)
                        || e.grouping.to_lowercase().contains(&search_lower)
                        || e.description.to_lowercase().contains(&search_lower))
            })
            .collect();
        if category == EffectCategory::Recent {
            // MRU order — most-recently-used first. Preserve the caller's
            // slice ordering instead of sorting alphabetically so users
            // see their last pick at the top.
            result.sort_by_key(|e| {
                recent_names
                    .iter()
                    .position(|n| n == &e.name)
                    .unwrap_or(usize::MAX)
            });
        } else {
            result.sort_by_key(|a| a.name.to_lowercase());
        }
        result
    }
}

/// Convert an OFX plugin identifier to a human-readable name.
/// "net.sf.openfx.GaussianBlur" → "Gaussian Blur"
/// "net.sf.cimg.CImgBloom" → "CImg Bloom"
///
/// Drops a trailing "Plugin" suffix — every OFX entry IS a plugin, so
/// the word adds zero information and clutters the card title. The
/// per-id override map in `effects_tab::display_name_override` handles
/// the small set of names where this mechanical algorithm still goofs.
fn format_ofx_name(identifier: &str) -> String {
    let last = identifier.rsplit_once('.').map(|(_, n)| n).unwrap_or(identifier);
    // Insert spaces before uppercase letters (CamelCase → Camel Case)
    let mut result = String::with_capacity(last.len() + 4);
    for (i, c) in last.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            // Don't insert space if previous char was also uppercase (e.g. "CImg")
            let prev = last.as_bytes()[i - 1] as char;
            if prev.is_lowercase() {
                result.push(' ');
            }
        }
        result.push(c);
    }
    // Strip "Plugin" suffix (with or without leading space). Be defensive
    // — only strip when the suffix is genuinely a separate token, not a
    // word like "Toplugin".
    let trimmed = result
        .trim_end_matches("Plugin")
        .trim_end_matches(" Plugin")
        .trim_end()
        .to_string();
    if trimmed.is_empty() { result } else { trimmed }
}

/// Map an OFX grouping string to an EffectCategory.
fn category_from_grouping(grouping: &str) -> EffectCategory {
    let g = grouping.to_lowercase();
    if g.contains("blur") || g.contains("sharpen") || g.contains("soften") || g.contains("denoise") {
        EffectCategory::Blur
    } else if g.contains("color") || g.contains("colour") || g.contains("grade") || g.contains("correct")
        || g.contains("saturation") || g.contains("hue")
    {
        EffectCategory::Color
    } else if g.contains("key") {
        EffectCategory::Keying
    } else if g.contains("distort") || g.contains("warp") || g.contains("transform") || g.contains("lens") {
        EffectCategory::Distort
    } else if g.contains("light") || g.contains("glow") || g.contains("flare") || g.contains("bloom") {
        EffectCategory::Light
    } else {
        EffectCategory::Stylize
    }
}

/// Heuristic mapping from a LADSPA plugin's display name + label to an
/// audio category for the effects-tab category sidebar. LADSPA itself
/// carries no category metadata in the C ABI, so this is a substring
/// match against common plugin name conventions.
///
/// Order matters — checks are top-to-bottom, first match wins. The order
/// reflects how distinctive each substring is: "reverb" rarely appears in
/// non-reverb plugin names, but "filter" can appear in EQ-style or
/// dynamics-style plugins, so we check the more distinctive groups first.
///
/// Falls back to `AudioEQ` for unknown plugins. AudioEQ is a forgiving
/// default: it isn't claiming the plugin IS an EQ, it's saying "no clear
/// category match — show in the EQ sub-tab" (the AudioAll sub-tab still
/// shows everything).
/// Heuristic category mapping for frei0r plugins, based on the
/// plugin name (no formal taxonomy in the spec). Conservative —
/// unknown names land under [`EffectCategory::Color`] since most
/// frei0r filters are color/level adjustments and that's the
/// least-surprising default.
fn frei0r_category_from_name(name: &str) -> EffectCategory {
    let n = name.to_lowercase();
    if n.contains("blur")
        || n.contains("sharp")
        || n.contains("edge")
        || n.contains("emboss")
    {
        return EffectCategory::Blur;
    }
    if n.contains("key")
        || n.contains("alpha")
        || n.contains("matte")
        || n.contains("chroma")
        || n.contains("luma")
    {
        return EffectCategory::Keying;
    }
    if n.contains("distort")
        || n.contains("warp")
        || n.contains("defish")
        || n.contains("lens")
        || n.contains("perspective")
        || n.contains("3dflippo")
    {
        return EffectCategory::Distort;
    }
    if n.contains("light")
        || n.contains("glow")
        || n.contains("flare")
        || n.contains("vignette")
        || n.contains("bloom")
    {
        return EffectCategory::Light;
    }
    if n.contains("cartoon")
        || n.contains("sketch")
        || n.contains("posterize")
        || n.contains("baltan")
        || n.contains("nervous")
        || n.contains("invert")
        || n.contains("solar")
    {
        return EffectCategory::Stylize;
    }
    EffectCategory::Color
}

/// Heuristic category mapping for LV2 plugins. Same shape as the
/// LADSPA helper — most LV2 plugins don't declare a formal
/// taxonomy beyond the lv2:Plugin class hierarchy, so we substring-
/// match on the plugin name. Conservative — unknown names fall
/// through to AudioUtility.
fn lv2_category_from_name(name: &str) -> EffectCategory {
    let n = name.to_lowercase();
    if n.contains("denois")
        || n.contains("declick")
        || n.contains("dehum")
        || n.contains("dehiss")
        || n.contains("hum remov")
        || n.contains("repair")
    {
        return EffectCategory::AudioRestoration;
    }
    if n.contains("pitch")
        || n.contains("autotune")
        || n.contains("auto-tune")
        || n.contains("harmoni")
        || n.contains("octaver")
    {
        return EffectCategory::AudioPitch;
    }
    if n.contains("reverb") || n.contains("hall") || n.contains("room") || n.contains("plate")
    {
        return EffectCategory::AudioReverb;
    }
    if n.contains("delay")
        || n.contains("echo")
        || n.contains("tape")
        || n.contains("baltan")
    {
        return EffectCategory::AudioDelay;
    }
    if n.contains("eq")
        || n.contains("equaliz")
        || n.contains("filter")
        || n.contains("lowpass")
        || n.contains("highpass")
        || n.contains("bandpass")
        || n.contains("notch")
    {
        return EffectCategory::AudioEQ;
    }
    if n.contains("compress")
        || n.contains("limit")
        || n.contains("gate")
        || n.contains("expand")
        || n.contains("dynam")
    {
        return EffectCategory::AudioDynamics;
    }
    if n.contains("chorus")
        || n.contains("flang")
        || n.contains("phaser")
        || n.contains("trem")
        || n.contains("vibrato")
    {
        return EffectCategory::AudioModulation;
    }
    EffectCategory::AudioUtility
}

fn ladspa_category_from_name(name: &str, label: &str) -> EffectCategory {
    let s = format!("{} {}", name.to_lowercase(), label.to_lowercase());

    // Restoration — denoise / declick / dehum / repair (checked early
    // since "noise" can otherwise match overly broad rules). Match
    // both hyphenated ("de-click") and concatenated ("declick") forms
    // since plugin authors are inconsistent.
    if s.contains("denois")
        || s.contains("noise reduc")
        || s.contains("noise_reduc")
        || s.contains("declick")
        || s.contains("de-click")
        || s.contains("decrackle")
        || s.contains("de-crackle")
        || s.contains("dehum")
        || s.contains("de-hum")
        || s.contains("dehiss")
        || s.contains("de-hiss")
        || s.contains("hum remov")
        || s.contains("hiss")
        || s.contains("repair")
    {
        return EffectCategory::AudioRestoration;
    }

    // Pitch — pitch shift, harmoniser, autotune, tuning utilities.
    if s.contains("pitch")
        || s.contains("autotune")
        || s.contains("auto-tune")
        || s.contains("harmoni")
        || s.contains("octaver")
        || s.contains("transpose")
    {
        return EffectCategory::AudioPitch;
    }

    // Reverb / spatial — very distinctive substrings, checked first
    // among spatial/temporal categories.
    if s.contains("reverb")
        || s.contains("hall")
        || s.contains("plate")
        || s.contains("room")
        || s.contains("freeverb")
        || s.contains("gverb")
        || s.contains("plateverb")
        || s.contains("spring")
    {
        return EffectCategory::AudioReverb;
    }

    // Modulation — chorus / flanger / phaser / tremolo / vibrato.
    // Checked before Delay so "chorus" doesn't slip into the comb/echo
    // bucket.
    if s.contains("chorus")
        || s.contains("flanger")
        || s.contains("phaser")
        || s.contains("tremolo")
        || s.contains("vibrato")
        || s.contains("modulator")
        || s.contains("ring mod")
        || s.contains("ring_mod")
    {
        return EffectCategory::AudioModulation;
    }

    // Delay / echo / comb.
    if s.contains("delay")
        || s.contains("echo")
        || s.contains("tap_")
        || s.contains("comb")
    {
        return EffectCategory::AudioDelay;
    }

    // Dynamics — compressor/limiter/gate/expander/maximiser families.
    if s.contains("compressor")
        || s.contains("compress")
        || s.contains("limiter")
        || s.contains("gate")
        || s.contains("expander")
        || s.contains("maximizer")
        || s.contains("maximiser")
        || s.contains("ducker")
        || s.contains("sidechain")
        || s.contains("dynamics")
        || s.contains("leveler")
        || s.contains("leveller")
    {
        return EffectCategory::AudioDynamics;
    }

    // Utility — gain, channel, stereo, mid/side, mono, normalise.
    if s.contains("gain")
        || s.contains("amplif")
        || s.contains("normali")
        || s.contains("channel")
        || s.contains("stereo")
        || s.contains("mono")
        || s.contains("mid_side")
        || s.contains("midside")
        || s.contains("ms_decoder")
        || s.contains("balance")
        || s.contains("invert")
        || s.contains("dc_remov")
    {
        return EffectCategory::AudioUtility;
    }

    // EQ / filter family — equalisers, filters, shelves, parametric/graphic.
    if s.contains("eq ")
        || s.ends_with("eq")
        || s.starts_with("eq ")
        || s.contains("equali")
        || s.contains("equaliser")
        || s.contains("equalizer")
        || s.contains("filter")
        || s.contains("lowpass")
        || s.contains("highpass")
        || s.contains("bandpass")
        || s.contains("notch")
        || s.contains("shelf")
        || s.contains("low_shelf")
        || s.contains("high_shelf")
        || s.contains("allpass")
        || s.contains("biquad")
        || s.contains("formant")
        || s.contains("vocoder")
        || s.contains("crossover")
    {
        return EffectCategory::AudioEQ;
    }

    // Default: route unmatched plugins to AudioUtility. They still
    // appear in AudioAll; the only consequence is which sub-tab in the
    // category sidebar surfaces them. Utility is a more honest "misc"
    // bucket than EQ for plugins that don't match any of the rules
    // above.
    EffectCategory::AudioUtility
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal registry with three distinct entries so tests don't
    /// depend on the real (large, evolving) registry content.
    fn test_registry() -> EffectRegistry {
        EffectRegistry {
            effects: vec![
                EffectEntry {
                    name: "Blur".into(),
                    category: EffectCategory::Blur,
                    source: EffectSource::Builtin(
                        klipp_effects::BuiltinEffect::GaussianBlur { radius: 0.0 },
                    ),
                    source_label: "Built-in",
                    description: String::new(),
                    grouping: "Blur".into(),
                },
                EffectEntry {
                    name: "Sharpen".into(),
                    category: EffectCategory::Blur,
                    source: EffectSource::Builtin(
                        klipp_effects::BuiltinEffect::Sharpen { amount: 0.0 },
                    ),
                    source_label: "Built-in",
                    description: String::new(),
                    grouping: "Blur".into(),
                },
                EffectEntry {
                    name: "Opacity".into(),
                    category: EffectCategory::Color,
                    source: EffectSource::Builtin(
                        klipp_effects::BuiltinEffect::Opacity { value: 1.0 },
                    ),
                    source_label: "Built-in",
                    description: String::new(),
                    grouping: "Colour".into(),
                },
            ],
            reference_frame: None,
        }
    }

    #[test]
    fn favourites_filter_returns_only_starred() {
        let reg = test_registry();
        let favs = vec!["Blur".to_string()];
        let out = reg.filtered_with_favorites(EffectCategory::Favourites, "", &favs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Blur");
    }

    #[test]
    fn favourites_filter_with_search_narrows_further() {
        let reg = test_registry();
        let favs = vec!["Blur".to_string(), "Opacity".to_string()];
        // Both starred — search "opac" uniquely hits Opacity by name, so the
        // starred Blur entry is filtered out even though it's a favourite.
        let out = reg.filtered_with_favorites(EffectCategory::Favourites, "opac", &favs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Opacity");
    }

    #[test]
    fn favourites_filter_empty_when_no_stars() {
        let reg = test_registry();
        let out = reg.filtered_with_favorites(EffectCategory::Favourites, "", &[]);
        assert!(out.is_empty(), "no stars → empty grid");
    }

    #[test]
    fn star_toggle_adds_and_removes() {
        // Mirrors the `pending_fav_toggle` apply logic in effects_tab.rs.
        let mut favs: Vec<String> = Vec::new();
        let name = "Blur".to_string();

        // First toggle → add.
        if let Some(i) = favs.iter().position(|n| n == &name) {
            favs.remove(i);
        } else {
            favs.push(name.clone());
        }
        assert_eq!(favs, vec!["Blur".to_string()]);

        // Second toggle → remove.
        if let Some(i) = favs.iter().position(|n| n == &name) {
            favs.remove(i);
        } else {
            favs.push(name.clone());
        }
        assert!(favs.is_empty());
    }

    #[test]
    fn filtered_backwards_compat_no_favorites() {
        // The old `filtered()` helper (no favorites slice) must still return
        // non-favorite categories identically to the new variant.
        let reg = test_registry();
        let old = reg.filtered(EffectCategory::Blur, "");
        let new = reg.filtered_with_favorites(EffectCategory::Blur, "", &[]);
        let old_names: Vec<_> = old.iter().map(|e| e.name.as_str()).collect();
        let new_names: Vec<_> = new.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(old_names, new_names);
    }

    // ─── ladspa_category_from_name heuristic ─────────────────────────
    //
    // Locks down the substring matches that route LADSPA plugins into the
    // audio category sidebar. The plugin names below are real LADSPA
    // plugins from common bundles (CALF, SWH, TAP, LSP, ...). Any future
    // change to the heuristic that would re-route one of these wrongly
    // will fail loudly here.

    #[test]
    fn ladspa_category_routes_reverbs() {
        for name in [
            "Calf Reverb",
            "Plate Reverb",
            "Plate 2x2",
            "Plateverb",
            "Freeverb",
            "GVerb",
            "Hall Reverb",
            "Room Reverb",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioReverb,
                "expected {name:?} to be AudioReverb",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_delays() {
        // Delay / echo / comb effects route to AudioDelay so they get
        // their own sidebar bucket, separate from Reverb and Modulation.
        for name in [
            "Stereo Delay",
            "Tape Echo",
            "Tap Delay",
            "Comb Filter Delay",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioDelay,
                "expected {name:?} to be AudioDelay",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_modulation() {
        // Chorus / flanger / phaser / tremolo / vibrato → Modulation.
        for name in [
            "Calf Flanger",
            "Calf Chorus",
            "Phaser",
            "Tremolo",
            "Vibrato",
            "Ring Mod",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioModulation,
                "expected {name:?} to be AudioModulation",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_pitch() {
        for name in ["Pitch Shifter", "Auto-Tune", "Harmonizer", "Octaver"] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioPitch,
                "expected {name:?} to be AudioPitch",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_restoration() {
        for name in [
            "Denoiser",
            "Noise Reduction",
            "De-click",
            "De-crackle",
            "De-hum",
            "De-hiss",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioRestoration,
                "expected {name:?} to be AudioRestoration",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_utility() {
        for name in [
            "Gain",
            "Amplifier",
            "Stereo Balance",
            "Mid/Side Decoder",
            "DC Removal",
            "Mono Mixer",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioUtility,
                "expected {name:?} to be AudioUtility",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_dynamics() {
        for name in [
            "Calf Compressor",
            "Calf Sidechain Compressor",
            "Calf Limiter",
            "Calf Multiband Limiter",
            "Calf Sidechain Gate",
            "Expander",
            "Maximizer",
            "Loudness Maximiser",
            "Auto Leveler",
            "Stereo Ducker",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioDynamics,
                "expected {name:?} to be AudioDynamics",
            );
        }
    }

    #[test]
    fn ladspa_category_routes_eqs_and_filters() {
        for name in [
            "Calf Equalizer 8 Band",
            "Calf Equaliser 12 Band",
            "Parametric EQ",
            "DJ EQ",
            "TAP Equalizer",
            "4-pole Lowpass Filter",
            "Highpass Filter",
            "Bandpass Filter",
            "Allpass Filter",
            "Notch Filter",
            "Low Shelf",
            "High Shelf",
            "Biquad",
            "Crossover",
            "Vocoder",
            "Formant Filter",
        ] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioEQ,
                "expected {name:?} to be AudioEQ",
            );
        }
    }

    #[test]
    fn ladspa_category_falls_back_for_unknown_names() {
        // No matching substring → AudioUtility default ("misc" bucket).
        // AudioAll always shows these too; the default just controls
        // which sub-tab surfaces them.
        for name in ["Mystery Plugin", "Foo Bar", "Saturator", "Bitcrusher"] {
            assert_eq!(
                ladspa_category_from_name(name, ""),
                EffectCategory::AudioUtility,
                "expected {name:?} to fall back to AudioUtility",
            );
        }
    }

    #[test]
    fn ladspa_category_uses_label_when_name_is_empty() {
        // Some plugins don't fill the human-readable name field; the
        // heuristic should still match against the technical label.
        assert_eq!(
            ladspa_category_from_name("", "calf_reverb"),
            EffectCategory::AudioReverb,
        );
        assert_eq!(
            ladspa_category_from_name("", "plate_reverb_lr"),
            EffectCategory::AudioReverb,
        );
        assert_eq!(
            ladspa_category_from_name("", "compressor_st"),
            EffectCategory::AudioDynamics,
        );
    }
}
