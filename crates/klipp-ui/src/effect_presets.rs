//! Effect preset system — built-in and user-created presets for all effects.
//!
//! Built-in presets are curated parameter sets shipped with the app.
//! User presets are saved as JSON in ~/.config/klipp/effect_presets/ and persist
//! across sessions. Both work for built-in effects and OFX plugins.

use std::collections::HashMap;
use std::path::PathBuf;

/// A saved effect preset.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EffectPreset {
    pub name: String,
    pub description: String,
    /// The effect type key (e.g. "bloom", "vignette", "net.sf.openfx.Invert")
    pub effect_key: String,
    /// For built-in effects: the full BuiltinEffect with preset values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtin: Option<klipp_effects::BuiltinEffect>,
    /// For OFX effects: parameter values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ofx_params: Option<HashMap<String, klipp_core::OfxValue>>,
    /// Whether this is a built-in (non-deletable) preset
    #[serde(default)]
    pub is_builtin: bool,
}

/// Manages all presets — built-in + user-created.
pub struct PresetManager {
    presets: Vec<EffectPreset>,
    user_presets_dir: PathBuf,
}

impl Default for PresetManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetManager {
    pub fn new() -> Self {
        let user_presets_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("klipp")
            .join("effect_presets");

        let mut mgr = Self {
            presets: Vec::new(),
            user_presets_dir,
        };
        mgr.load_builtin_presets();
        mgr.load_user_presets();
        mgr
    }

    /// Get all presets for a given effect key.
    pub fn presets_for(&self, effect_key: &str) -> Vec<&EffectPreset> {
        self.presets
            .iter()
            .filter(|p| p.effect_key == effect_key)
            .collect()
    }

    /// Save a user preset. Persists to disk.
    pub fn save_user_preset(&mut self, preset: EffectPreset) {
        let effect_dir = self.user_presets_dir.join(&preset.effect_key);
        if let Err(e) = std::fs::create_dir_all(&effect_dir) {
            tracing::warn!("Failed to create preset dir: {e}");
            return;
        }

        let filename = sanitize_filename(&preset.name);
        let path = effect_dir.join(format!("{filename}.json"));

        match serde_json::to_string_pretty(&preset) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("Failed to save preset: {e}");
                } else {
                    tracing::info!("Saved preset '{}' to {}", preset.name, path.display());
                    self.presets.retain(|p| !(p.effect_key == preset.effect_key && p.name == preset.name && !p.is_builtin));
                    self.presets.push(preset);
                }
            }
            Err(e) => tracing::warn!("Failed to serialize preset: {e}"),
        }
    }

    /// Delete a user preset (built-in presets cannot be deleted).
    pub fn delete_user_preset(&mut self, effect_key: &str, name: &str) {
        let filename = sanitize_filename(name);
        let path = self.user_presets_dir.join(effect_key).join(format!("{filename}.json"));
        let _ = std::fs::remove_file(&path);
        self.presets.retain(|p| !(p.effect_key == effect_key && p.name == name && !p.is_builtin));
    }

    fn load_user_presets(&mut self) {
        let Ok(entries) = std::fs::read_dir(&self.user_presets_dir) else { return };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let Ok(files) = std::fs::read_dir(entry.path()) else { continue };
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("json") {
                        if let Ok(json) = std::fs::read_to_string(&path) {
                            if let Ok(preset) = serde_json::from_str::<EffectPreset>(&json) {
                                self.presets.push(preset);
                            }
                        }
                    }
                }
            }
        }
    }

    fn load_builtin_presets(&mut self) {
        // ── Bloom ──────────────────────────────────────────────
        self.bi("bloom", "Subtle", "Gentle highlight enhancement",
            klipp_effects::BuiltinEffect::Bloom { threshold: 0.8, radius: 15.0, intensity: 0.25 });
        self.bi("bloom", "Cinematic", "Classic film bloom",
            klipp_effects::BuiltinEffect::Bloom { threshold: 0.65, radius: 25.0, intensity: 0.5 });
        self.bi("bloom", "Dreamy", "Heavy dreamlike glow",
            klipp_effects::BuiltinEffect::Bloom { threshold: 0.5, radius: 40.0, intensity: 0.8 });
        self.bi("bloom", "Music Video", "Intense overexposed look",
            klipp_effects::BuiltinEffect::Bloom { threshold: 0.4, radius: 35.0, intensity: 1.2 });

        // ── Glow ───────────────────────────────────────────────
        self.bi("glow", "Soft Touch", "Barely visible softening",
            klipp_effects::BuiltinEffect::Glow { radius: 20.0, intensity: 0.2 });
        self.bi("glow", "Romantic", "Soft warm glow",
            klipp_effects::BuiltinEffect::Glow { radius: 35.0, intensity: 0.45 });
        self.bi("glow", "Ethereal", "Strong diffuse glow",
            klipp_effects::BuiltinEffect::Glow { radius: 50.0, intensity: 0.7 });

        // ── Light Rays ─────────────────────────────────────────
        self.bi("light_rays", "Morning Sun", "Soft rays from upper right",
            klipp_effects::BuiltinEffect::LightRays { center_x: 0.8, center_y: 0.2, threshold: 0.65, samples: 64, decay: 0.3, intensity: 0.4 });
        self.bi("light_rays", "Divine Light", "Strong rays from above",
            klipp_effects::BuiltinEffect::LightRays { center_x: 0.5, center_y: 0.1, threshold: 0.55, samples: 96, decay: 0.4, intensity: 0.7 });
        self.bi("light_rays", "Window Light", "Gentle rays from center left",
            klipp_effects::BuiltinEffect::LightRays { center_x: 0.15, center_y: 0.4, threshold: 0.6, samples: 48, decay: 0.25, intensity: 0.35 });

        // ── Vignette ───────────────────────────────────────────
        self.bi("vignette", "Subtle Dark", "Gentle corner darkening",
            klipp_effects::BuiltinEffect::Vignette { amount: -0.25, size: 0.9, softness: 0.6, roundness: 0.5 });
        self.bi("vignette", "Classic Film", "Standard film vignette",
            klipp_effects::BuiltinEffect::Vignette { amount: -0.45, size: 0.75, softness: 0.5, roundness: 0.5 });
        self.bi("vignette", "Heavy Drama", "Strong dramatic darkening",
            klipp_effects::BuiltinEffect::Vignette { amount: -0.7, size: 0.6, softness: 0.4, roundness: 0.4 });
        self.bi("vignette", "Spotlight", "Tight circular focus",
            klipp_effects::BuiltinEffect::Vignette { amount: -0.8, size: 0.5, softness: 0.3, roundness: 0.9 });
        self.bi("vignette", "Light Edges", "Bright edge glow",
            klipp_effects::BuiltinEffect::Vignette { amount: 0.3, size: 0.8, softness: 0.5, roundness: 0.5 });

        // ── Diffusion ──────────────────────────────────────────
        self.bi("diffusion", "Light Mist", "Subtle pro-mist filter",
            klipp_effects::BuiltinEffect::Diffusion { amount: 0.15, radius: 20.0, threshold: 0.6, desaturation: 0.1 });
        self.bi("diffusion", "Classic Pro-Mist", "Standard diffusion filter",
            klipp_effects::BuiltinEffect::Diffusion { amount: 0.3, radius: 30.0, threshold: 0.5, desaturation: 0.2 });
        self.bi("diffusion", "Heavy Fog", "Strong dreamy diffusion",
            klipp_effects::BuiltinEffect::Diffusion { amount: 0.5, radius: 45.0, threshold: 0.35, desaturation: 0.35 });
        self.bi("diffusion", "Golden Hour", "Warm highlight softening",
            klipp_effects::BuiltinEffect::Diffusion { amount: 0.35, radius: 25.0, threshold: 0.45, desaturation: 0.05 });

        // ── Lens Flare ─────────────────────────────────────────
        self.bi("lens_flare", "Subtle Flare", "Minimal lens flare",
            klipp_effects::BuiltinEffect::LensFlare { position_x: 0.7, position_y: 0.3, brightness: 0.3, flare_size: 0.1, ghost_count: 2, ghost_intensity: 0.15, chromatic_aberration: 0.2 });
        self.bi("lens_flare", "Cinematic", "Classic film lens flare",
            klipp_effects::BuiltinEffect::LensFlare { position_x: 0.75, position_y: 0.25, brightness: 0.6, flare_size: 0.15, ghost_count: 4, ghost_intensity: 0.3, chromatic_aberration: 0.4 });
        self.bi("lens_flare", "Sci-Fi", "Strong dramatic flare",
            klipp_effects::BuiltinEffect::LensFlare { position_x: 0.6, position_y: 0.4, brightness: 1.0, flare_size: 0.2, ghost_count: 6, ghost_intensity: 0.5, chromatic_aberration: 0.7 });

        // ── Light Wrap ─────────────────────────────────────────
        self.bi("light_wrap", "Gentle", "Subtle edge integration",
            klipp_effects::BuiltinEffect::LightWrap { width: 10.0, intensity: 0.3, softness: 1.0 });
        self.bi("light_wrap", "Standard", "Natural compositing wrap",
            klipp_effects::BuiltinEffect::LightWrap { width: 20.0, intensity: 0.5, softness: 1.0 });
        self.bi("light_wrap", "Strong", "Heavy background bleed",
            klipp_effects::BuiltinEffect::LightWrap { width: 35.0, intensity: 0.7, softness: 1.5 });

        // ── Anamorphic Streak ──────────────────────────────────
        self.bi("anamorphic_streak", "Subtle Warm", "Gentle amber streaks",
            klipp_effects::BuiltinEffect::AnamorphicStreak { threshold: 0.75, length: 60.0, intensity: 0.3, tint: 0.2 });
        self.bi("anamorphic_streak", "Classic Anamorphic", "Standard lens streak",
            klipp_effects::BuiltinEffect::AnamorphicStreak { threshold: 0.65, length: 100.0, intensity: 0.5, tint: 0.3 });
        self.bi("anamorphic_streak", "Blade Runner", "Strong blue-tinted streaks",
            klipp_effects::BuiltinEffect::AnamorphicStreak { threshold: 0.6, length: 150.0, intensity: 0.7, tint: 0.8 });
        self.bi("anamorphic_streak", "Neon Nights", "Intense wide streaks",
            klipp_effects::BuiltinEffect::AnamorphicStreak { threshold: 0.5, length: 180.0, intensity: 0.9, tint: 0.6 });

        // ── Film Halation ──────────────────────────────────────
        self.bi("film_halation", "Subtle Red", "Gentle film halation",
            klipp_effects::BuiltinEffect::FilmHalation { threshold: 0.8, radius: 12.0, intensity: 0.2, hue: 0.1 });
        self.bi("film_halation", "Kodak 5219", "Classic motion picture film",
            klipp_effects::BuiltinEffect::FilmHalation { threshold: 0.7, radius: 18.0, intensity: 0.35, hue: 0.25 });
        self.bi("film_halation", "Vintage Warm", "Strong warm halation",
            klipp_effects::BuiltinEffect::FilmHalation { threshold: 0.6, radius: 25.0, intensity: 0.5, hue: 0.5 });

        // ── Gaussian Blur ──────────────────────────────────────
        self.bi("gaussian_blur", "Light Soften", "Subtle background softening",
            klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 });
        self.bi("gaussian_blur", "Medium Blur", "Standard blur",
            klipp_effects::BuiltinEffect::GaussianBlur { radius: 10.0 });
        self.bi("gaussian_blur", "Heavy Blur", "Strong defocus",
            klipp_effects::BuiltinEffect::GaussianBlur { radius: 30.0 });
        self.bi("gaussian_blur", "Privacy Blur", "Face/plate obscuring",
            klipp_effects::BuiltinEffect::GaussianBlur { radius: 50.0 });

        // ── The layer treatment ────────────────────────────────
        // These show only where the picture does not fill the frame:
        // on a clip placed smaller than the frame, or a title.
        self.bi("rounded_corners", "Card", "Soft corners, as a UI card",
            klipp_effects::BuiltinEffect::RoundedCorners { radius: 0.02, feather: 0.002 });
        self.bi("rounded_corners", "Pill", "Fully rounded ends",
            klipp_effects::BuiltinEffect::RoundedCorners { radius: 0.5, feather: 0.002 });
        self.bi("drop_shadow", "Lift", "A close shadow, as if just off the surface",
            klipp_effects::BuiltinEffect::DropShadow { offset_x: 0.0, offset_y: 0.006, blur: 0.012, color: [0, 0, 0], opacity: 0.45 });
        self.bi("drop_shadow", "Float", "A wide soft shadow, well above the background",
            klipp_effects::BuiltinEffect::DropShadow { offset_x: 0.0, offset_y: 0.02, blur: 0.05, color: [0, 0, 0], opacity: 0.6 });
        self.bi("outer_glow", "Edge Light", "A tight rim of light",
            klipp_effects::BuiltinEffect::OuterGlow { radius: 0.012, intensity: 0.7, color: [255, 255, 255] });
        self.bi("outer_glow", "Halo", "A wide glow onto the background",
            klipp_effects::BuiltinEffect::OuterGlow { radius: 0.06, intensity: 0.8, color: [120, 150, 255] });

        // ── Sharpen ────────────────────────────────────────────
        self.bi("sharpen", "Subtle", "Light sharpening for web",
            klipp_effects::BuiltinEffect::Sharpen { amount: 0.3 });
        self.bi("sharpen", "Standard", "General-purpose sharpening",
            klipp_effects::BuiltinEffect::Sharpen { amount: 0.6 });
        self.bi("sharpen", "Crispy", "Strong detail enhancement",
            klipp_effects::BuiltinEffect::Sharpen { amount: 1.2 });

        // ── Opacity ────────────────────────────────────────────
        self.bi("opacity", "25%", "Quarter opacity",
            klipp_effects::BuiltinEffect::Opacity { value: 0.25 });
        self.bi("opacity", "50%", "Half opacity",
            klipp_effects::BuiltinEffect::Opacity { value: 0.5 });
        self.bi("opacity", "75%", "Three-quarter opacity",
            klipp_effects::BuiltinEffect::Opacity { value: 0.75 });

        // ── Rolling Shutter Correction ──────────────────────────
        self.bi("rolling_shutter_correction", "iPhone / Modern Phone", "Fast readout ~15ms",
            klipp_effects::BuiltinEffect::RollingShutterCorrection { strength: 0.5, readout_ratio: 0.35 });
        self.bi("rolling_shutter_correction", "Action Cam (GoPro)", "Medium readout ~16ms",
            klipp_effects::BuiltinEffect::RollingShutterCorrection { strength: 0.5, readout_ratio: 0.40 });
        self.bi("rolling_shutter_correction", "Mirrorless Camera", "Sony/Canon ~20ms readout",
            klipp_effects::BuiltinEffect::RollingShutterCorrection { strength: 0.5, readout_ratio: 0.50 });
        self.bi("rolling_shutter_correction", "Drone (DJI)", "Slow readout ~30ms",
            klipp_effects::BuiltinEffect::RollingShutterCorrection { strength: 0.8, readout_ratio: 0.75 });

        // ── Video Stabilization ────────────────────────────────
        self.bi("video_stabilization", "Subtle", "Gentle smoothing for handheld footage",
            klipp_effects::BuiltinEffect::VideoStabilization { smoothing: 0.3, crop_ratio: 0.03, zoom: true });
        self.bi("video_stabilization", "Smooth Handheld", "Moderate smoothing for walking/handheld",
            klipp_effects::BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true });
        self.bi("video_stabilization", "Cinematic Glide", "Strong smoothing for a floaty look",
            klipp_effects::BuiltinEffect::VideoStabilization { smoothing: 0.8, crop_ratio: 0.08, zoom: true });
        self.bi("video_stabilization", "Locked Tripod", "Maximum smoothing for minor vibration",
            klipp_effects::BuiltinEffect::VideoStabilization { smoothing: 1.0, crop_ratio: 0.03, zoom: false });

        // ── Speed Ramp ─────────────────────────────────────────
        self.bi("speed_ramp", "Slow-Mo 50%", "Half-speed playback",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: 1, ramp_curve: 0, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.0, freeze: false });
        self.bi("speed_ramp", "Slow-Mo 25%", "Quarter-speed playback",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 0.25, blend_mode: 1, ramp_curve: 0, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.0, freeze: false });
        self.bi("speed_ramp", "Speed Up 2x", "Double-speed playback",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 2.0, blend_mode: 0, ramp_curve: 0, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.0, freeze: false });
        self.bi("speed_ramp", "Speed Up 4x", "Quadruple-speed playback",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 4.0, blend_mode: 0, ramp_curve: 0, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.0, freeze: false });
        self.bi("speed_ramp", "Smooth Slow-Mo", "Slow-mo with ease-in-out ramp",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: 1, ramp_curve: 3, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.0, freeze: false });
        self.bi("speed_ramp", "Speed Ramp In", "Accelerate with ease-in",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 2.0, blend_mode: 1, ramp_curve: 1, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.0, freeze: false });
        self.bi("speed_ramp", "Speed Ramp Out", "Decelerate with ease-out",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 0.5, blend_mode: 1, ramp_curve: 2, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.0, freeze: false });
        self.bi("speed_ramp", "Reverse", "Reverse playback",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 0, ramp_curve: 0, ramp_start: 0.0, ramp_end: 1.0, reverse: true, freeze_at: 0.0, freeze: false });
        self.bi("speed_ramp", "Freeze Frame", "Freeze at midpoint",
            klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 0, ramp_curve: 0, ramp_start: 0.0, ramp_end: 1.0, reverse: false, freeze_at: 0.5, freeze: true });

        // ── Chroma Key ─────────────────────────────────────────
        // New presets ship with ChannelClamp + 0.6 strength so users
        // see real spill suppression on day one. Existing projects
        // load with `None` / 0.0 via serde defaults, preserving look.
        self.bi("chroma_key", "Green Screen", "Standard green screen key",
            klipp_effects::BuiltinEffect::ChromaKey {
                key_color: [0, 177, 64], tolerance: 0.25, soft_edge: 0.15,
                spill_method: klipp_effects::SpillMethod::ChannelClamp, spill_strength: 0.6,
            });
        self.bi("chroma_key", "Blue Screen", "Standard blue screen key",
            klipp_effects::BuiltinEffect::ChromaKey {
                key_color: [0, 71, 187], tolerance: 0.25, soft_edge: 0.15,
                spill_method: klipp_effects::SpillMethod::ChannelClamp, spill_strength: 0.6,
            });
        self.bi("chroma_key", "Green Screen (Tight)", "Clean green screen with tight key",
            klipp_effects::BuiltinEffect::ChromaKey {
                key_color: [0, 177, 64], tolerance: 0.15, soft_edge: 0.25,
                spill_method: klipp_effects::SpillMethod::ChannelClamp, spill_strength: 0.6,
            });

        // ── Posterize ─────────────────────────────────────────
        self.bi("posterize", "Pop Art", "Bold 4-level poster look",
            klipp_effects::BuiltinEffect::Posterize { levels: 4.0 });
        self.bi("posterize", "Retro Game", "8-bit game palette",
            klipp_effects::BuiltinEffect::Posterize { levels: 8.0 });
        self.bi("posterize", "Subtle", "Gentle banding (16 levels)",
            klipp_effects::BuiltinEffect::Posterize { levels: 16.0 });

        // ── Mosaic ────────────────────────────────────────────
        self.bi("mosaic", "Privacy", "Face/plate obscuring",
            klipp_effects::BuiltinEffect::Mosaic { block_size: 20.0 });
        self.bi("mosaic", "Pixel Art", "Retro pixel look",
            klipp_effects::BuiltinEffect::Mosaic { block_size: 8.0 });
        self.bi("mosaic", "Extreme", "Very coarse pixelation",
            klipp_effects::BuiltinEffect::Mosaic { block_size: 50.0 });

        // ── Edge Detect ───────────────────────────────────────
        self.bi("edge_detect", "Sketch", "Clean edge outline",
            klipp_effects::BuiltinEffect::EdgeDetect { strength: 2.0, mix: 1.0 });
        self.bi("edge_detect", "Overlay Edges", "Subtle edge enhancement",
            klipp_effects::BuiltinEffect::EdgeDetect { strength: 1.5, mix: 0.3 });

        // ── Emboss ────────────────────────────────────────────
        self.bi("emboss", "Stamp", "Classic stamp/relief look",
            klipp_effects::BuiltinEffect::Emboss { angle: 135.0, strength: 1.5, mix: 1.0 });
        self.bi("emboss", "Subtle Relief", "Gentle 3D texture",
            klipp_effects::BuiltinEffect::Emboss { angle: 135.0, strength: 0.8, mix: 0.4 });
        self.bi("emboss", "Side Light", "Lit from the right",
            klipp_effects::BuiltinEffect::Emboss { angle: 0.0, strength: 1.5, mix: 1.0 });

        // ── Directional Blur ──────────────────────────────────
        self.bi("directional_blur", "Horizontal Sweep", "Horizontal motion blur",
            klipp_effects::BuiltinEffect::DirectionalBlur { angle: 0.0, distance: 15.0, samples: 12 });
        self.bi("directional_blur", "Vertical Rain", "Vertical streak blur",
            klipp_effects::BuiltinEffect::DirectionalBlur { angle: 90.0, distance: 20.0, samples: 16 });
        self.bi("directional_blur", "Diagonal Speed", "Dynamic diagonal motion",
            klipp_effects::BuiltinEffect::DirectionalBlur { angle: 45.0, distance: 25.0, samples: 12 });

        // ── Lens Distortion ───────────────────────────────────
        self.bi("lens_distortion", "GoPro Fix", "Correct action cam barrel distortion",
            klipp_effects::BuiltinEffect::LensDistortion { distortion: -0.3, scale: 1.1 });
        self.bi("lens_distortion", "Fisheye", "Extreme barrel distortion",
            klipp_effects::BuiltinEffect::LensDistortion { distortion: -0.8, scale: 0.7 });
        self.bi("lens_distortion", "Pincushion", "Inward pinch distortion",
            klipp_effects::BuiltinEffect::LensDistortion { distortion: 0.5, scale: 1.0 });

        // ── Denoise ───────────────────────────────────────────
        self.bi("denoise", "Light", "Subtle noise reduction for well-lit scenes",
            klipp_effects::BuiltinEffect::Denoise { spatial_sigma: 3.0, color_sigma: 0.08, iterations: 1 });
        self.bi("denoise", "Standard", "General-purpose denoising",
            klipp_effects::BuiltinEffect::Denoise { spatial_sigma: 5.0, color_sigma: 0.12, iterations: 1 });
        self.bi("denoise", "Heavy", "Strong noise reduction for phone/low-light footage",
            klipp_effects::BuiltinEffect::Denoise { spatial_sigma: 8.0, color_sigma: 0.2, iterations: 2 });

        // ── Gamma ─────────────────────────────────────────────
        self.bi("gamma", "Brighten", "Lift midtones",
            klipp_effects::BuiltinEffect::Gamma { gamma: 1.5 });
        self.bi("gamma", "Darken", "Crush midtones",
            klipp_effects::BuiltinEffect::Gamma { gamma: 0.7 });
        self.bi("gamma", "High Contrast", "Strong S-curve via gamma",
            klipp_effects::BuiltinEffect::Gamma { gamma: 0.5 });

        // ── Invert ────────────────────────────────────────────
        self.bi("invert", "Full Invert", "Complete colour inversion",
            klipp_effects::BuiltinEffect::Invert { mix: 1.0 });
        self.bi("invert", "Partial Invert", "50% inverted blend",
            klipp_effects::BuiltinEffect::Invert { mix: 0.5 });

        // ── Mirror ────────────────────────────────────────────
        self.bi("mirror", "Horizontal Flip", "Flip left to right",
            klipp_effects::BuiltinEffect::Mirror { horizontal: 1, vertical: 0 });
        self.bi("mirror", "Vertical Flip", "Flip top to bottom",
            klipp_effects::BuiltinEffect::Mirror { horizontal: 0, vertical: 1 });
        self.bi("mirror", "Both", "Rotate 180 degrees",
            klipp_effects::BuiltinEffect::Mirror { horizontal: 1, vertical: 1 });

        // ── HSV Adjust ────────────────────────────────────────
        self.bi("hsv_adjust", "Hue Shift 30\u{00b0}", "Subtle warm shift",
            klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 30.0, saturation: 1.0, value: 1.0 });
        self.bi("hsv_adjust", "Desaturate", "Remove all colour",
            klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.0, value: 1.0 });
        self.bi("hsv_adjust", "Boost Saturation", "Vivid colours",
            klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 1.8, value: 1.0 });
        self.bi("hsv_adjust", "Warm Shift", "Orange/warm tone shift",
            klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: -15.0, saturation: 1.2, value: 1.05 });

        // ── Noise Generator ───────────────────────────────────
        self.bi("noise_generator", "Subtle Grain", "Light film grain texture",
            klipp_effects::BuiltinEffect::NoiseGenerator { amount: 0.05, monochrome: 1, seed: 0 });
        self.bi("noise_generator", "Heavy Noise", "Strong noise overlay",
            klipp_effects::BuiltinEffect::NoiseGenerator { amount: 0.3, monochrome: 0, seed: 0 });
        self.bi("noise_generator", "B&W Grain", "Classic monochrome film grain",
            klipp_effects::BuiltinEffect::NoiseGenerator { amount: 0.15, monochrome: 1, seed: 0 });

        // ── Film Grain ────────────────────────────────────────
        self.bi("film_grain", "Fine 35mm", "Tight grain that lives in the shadows",
            klipp_effects::BuiltinEffect::FilmGrain { amount: 0.25, size: 1.2, color: 0, shadows: 0.7, seed: 0 });
        self.bi("film_grain", "16mm", "Coarser, more visible grain",
            klipp_effects::BuiltinEffect::FilmGrain { amount: 0.45, size: 2.2, color: 0, shadows: 0.5, seed: 0 });
        self.bi("film_grain", "Colour Negative", "Per-channel grain of a colour stock",
            klipp_effects::BuiltinEffect::FilmGrain { amount: 0.3, size: 1.6, color: 1, shadows: 0.6, seed: 0 });

        // ── Old Film ──────────────────────────────────────────
        self.bi("old_film", "Silent Era", "Sepia, heavy weave and scratches",
            klipp_effects::BuiltinEffect::OldFilm { flicker: 0.6, scratches: 0.8, dust: 0.8, weave: 0.8, sepia: 0.9, seed: 0 });
        self.bi("old_film", "Home Movie", "Light wear, colour kept",
            klipp_effects::BuiltinEffect::OldFilm { flicker: 0.3, scratches: 0.3, dust: 0.4, weave: 0.4, sepia: 0.15, seed: 0 });

        // ── VHS ───────────────────────────────────────────────
        self.bi("vhs", "Worn Tape", "Heavy tracking and bleed",
            klipp_effects::BuiltinEffect::Vhs { tracking: 0.8, bleed: 0.8, noise: 0.6, scanlines: 0.6, seed: 0 });
        self.bi("vhs", "Clean Dub", "A subtle tape look",
            klipp_effects::BuiltinEffect::Vhs { tracking: 0.2, bleed: 0.4, noise: 0.2, scanlines: 0.4, seed: 0 });

        // ── Tilt-Shift ────────────────────────────────────────
        self.bi("tilt_shift", "Miniature", "A narrow sharp band, strong blur",
            klipp_effects::BuiltinEffect::TiltShift { center: 0.55, width: 0.2, softness: 0.12, radius: 24.0 });
        self.bi("tilt_shift", "Soft Frame", "Wide focus, gentle fall-off",
            klipp_effects::BuiltinEffect::TiltShift { center: 0.5, width: 0.5, softness: 0.3, radius: 8.0 });

        // ── Median Filter ─────────────────────────────────────
        self.bi("median_filter", "Light Smooth", "Gentle noise removal",
            klipp_effects::BuiltinEffect::MedianFilter { radius: 1 });
        self.bi("median_filter", "Strong Smooth", "Aggressive noise removal",
            klipp_effects::BuiltinEffect::MedianFilter { radius: 2 });

        // ── Radial Blur ───────────────────────────────────────
        self.bi("radial_blur", "Zoom Blur", "Zoom-in motion blur from center",
            klipp_effects::BuiltinEffect::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 15.0, samples: 16, mode: 0 });
        self.bi("radial_blur", "Spin Blur", "Rotational motion blur",
            klipp_effects::BuiltinEffect::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 16, mode: 1 });
        self.bi("radial_blur", "Subtle Zoom", "Gentle zoom blur",
            klipp_effects::BuiltinEffect::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 5.0, samples: 12, mode: 0 });

        // ── Levels ────────────────────────────────────────────
        self.bi("levels", "Crush Blacks", "Lift input black point for contrast",
            klipp_effects::BuiltinEffect::Levels { in_black: 0.15, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0 });
        self.bi("levels", "Expand Range", "Stretch histogram to full range",
            klipp_effects::BuiltinEffect::Levels { in_black: 0.05, in_white: 0.95, gamma: 1.0, out_black: 0.0, out_white: 1.0 });
        self.bi("levels", "Faded Film", "Lift output blacks for a faded look",
            klipp_effects::BuiltinEffect::Levels { in_black: 0.0, in_white: 1.0, gamma: 1.1, out_black: 0.1, out_white: 0.9 });

        // ── Equalize ──────────────────────────────────────────
        self.bi("equalize", "Auto Levels", "Full histogram equalization",
            klipp_effects::BuiltinEffect::Equalize { strength: 1.0 });
        self.bi("equalize", "Subtle", "Gentle contrast enhancement",
            klipp_effects::BuiltinEffect::Equalize { strength: 0.3 });
        self.bi("equalize", "Strong", "Strong contrast enhancement",
            klipp_effects::BuiltinEffect::Equalize { strength: 0.7 });

        // ── Letterbox ─────────────────────────────────────────
        self.bi("letterbox", "CinemaScope 2.39:1", "Anamorphic widescreen matte",
            klipp_effects::BuiltinEffect::Letterbox { aspect_ratio: 2.39, bar_color: 0, opacity: 1.0 });
        self.bi("letterbox", "Theatrical 1.85:1", "Standard theatrical widescreen",
            klipp_effects::BuiltinEffect::Letterbox { aspect_ratio: 1.85, bar_color: 0, opacity: 1.0 });
        self.bi("letterbox", "Classic TV 4:3", "Pillarbox for 4:3 content",
            klipp_effects::BuiltinEffect::Letterbox { aspect_ratio: 1.333, bar_color: 0, opacity: 1.0 });
        self.bi("letterbox", "Widescreen 16:9", "Standard HD widescreen",
            klipp_effects::BuiltinEffect::Letterbox { aspect_ratio: 1.778, bar_color: 0, opacity: 1.0 });
        self.bi("letterbox", "IMAX 1.43:1", "IMAX original aspect ratio",
            klipp_effects::BuiltinEffect::Letterbox { aspect_ratio: 1.43, bar_color: 0, opacity: 1.0 });
        self.bi("letterbox", "Ultra Panavision 2.76:1", "Ultra-wide anamorphic",
            klipp_effects::BuiltinEffect::Letterbox { aspect_ratio: 2.76, bar_color: 0, opacity: 1.0 });

        // ── Chromatic Aberration ──────────────────────────────
        self.bi("chromatic_aberration", "Subtle Fringe", "Light edge fringing",
            klipp_effects::BuiltinEffect::ChromaticAberration { red_shift: 2.0, blue_shift: -2.0, center_x: 0.5, center_y: 0.5 });
        self.bi("chromatic_aberration", "Vintage Lens", "Classic imperfect lens",
            klipp_effects::BuiltinEffect::ChromaticAberration { red_shift: 4.0, blue_shift: -4.0, center_x: 0.5, center_y: 0.5 });
        self.bi("chromatic_aberration", "Heavy Aberration", "Strong RGB separation",
            klipp_effects::BuiltinEffect::ChromaticAberration { red_shift: 8.0, blue_shift: -6.0, center_x: 0.5, center_y: 0.5 });
        self.bi("chromatic_aberration", "Anamorphic Edge", "Asymmetric lens character",
            klipp_effects::BuiltinEffect::ChromaticAberration { red_shift: 3.0, blue_shift: -5.0, center_x: 0.5, center_y: 0.5 });
    }

    fn bi(&mut self, key: &str, name: &str, desc: &str, effect: klipp_effects::BuiltinEffect) {
        self.presets.push(EffectPreset {
            name: name.to_string(),
            description: desc.to_string(),
            effect_key: key.to_string(),
            builtin: Some(effect),
            ofx_params: None,
            is_builtin: true,
        });
    }
}

/// Map a BuiltinEffect to its preset key.
pub fn builtin_effect_key(effect: &klipp_effects::BuiltinEffect) -> &'static str {
    match effect {
        klipp_effects::BuiltinEffect::ColorFade { .. } => "color_fade",
        klipp_effects::BuiltinEffect::Opacity { .. } => "opacity",
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => "gaussian_blur",
        klipp_effects::BuiltinEffect::RoundedCorners { .. } => "rounded_corners",
        klipp_effects::BuiltinEffect::DropShadow { .. } => "drop_shadow",
        klipp_effects::BuiltinEffect::OuterGlow { .. } => "outer_glow",
        klipp_effects::BuiltinEffect::Sharpen { .. } => "sharpen",
        klipp_effects::BuiltinEffect::Crop { .. } => "crop",
        klipp_effects::BuiltinEffect::Transform { .. } => "transform",
        klipp_effects::BuiltinEffect::ChromaKey { .. } => "chroma_key",
        klipp_effects::BuiltinEffect::Bloom { .. } => "bloom",
        klipp_effects::BuiltinEffect::Glow { .. } => "glow",
        klipp_effects::BuiltinEffect::LightRays { .. } => "light_rays",
        klipp_effects::BuiltinEffect::Vignette { .. } => "vignette",
        klipp_effects::BuiltinEffect::Diffusion { .. } => "diffusion",
        klipp_effects::BuiltinEffect::LensFlare { .. } => "lens_flare",
        klipp_effects::BuiltinEffect::LightWrap { .. } => "light_wrap",
        klipp_effects::BuiltinEffect::AnamorphicStreak { .. } => "anamorphic_streak",
        klipp_effects::BuiltinEffect::FilmHalation { .. } => "film_halation",
        klipp_effects::BuiltinEffect::Invert { .. } => "invert",
        klipp_effects::BuiltinEffect::Posterize { .. } => "posterize",
        klipp_effects::BuiltinEffect::Mosaic { .. } => "mosaic",
        klipp_effects::BuiltinEffect::EdgeDetect { .. } => "edge_detect",
        klipp_effects::BuiltinEffect::ChannelMixer { .. } => "channel_mixer",
        klipp_effects::BuiltinEffect::DirectionalBlur { .. } => "directional_blur",
        klipp_effects::BuiltinEffect::Emboss { .. } => "emboss",
        klipp_effects::BuiltinEffect::Mirror { .. } => "mirror",
        klipp_effects::BuiltinEffect::LensDistortion { .. } => "lens_distortion",
        klipp_effects::BuiltinEffect::Dilate { .. } => "dilate",
        klipp_effects::BuiltinEffect::Erode { .. } => "erode",
        klipp_effects::BuiltinEffect::Denoise { .. } => "denoise",
        klipp_effects::BuiltinEffect::Gamma { .. } => "gamma",
        klipp_effects::BuiltinEffect::HSVAdjust { .. } => "hsv_adjust",
        klipp_effects::BuiltinEffect::NoiseGenerator { .. } => "noise_generator",
        klipp_effects::BuiltinEffect::FilmGrain { .. } => "film_grain",
        klipp_effects::BuiltinEffect::TiltShift { .. } => "tilt_shift",
        klipp_effects::BuiltinEffect::OldFilm { .. } => "old_film",
        klipp_effects::BuiltinEffect::Vhs { .. } => "vhs",
        klipp_effects::BuiltinEffect::MedianFilter { .. } => "median_filter",
        klipp_effects::BuiltinEffect::RadialBlur { .. } => "radial_blur",
        klipp_effects::BuiltinEffect::Levels { .. } => "levels",
        klipp_effects::BuiltinEffect::Equalize { .. } => "equalize",
        klipp_effects::BuiltinEffect::Letterbox { .. } => "letterbox",
        klipp_effects::BuiltinEffect::ChromaticAberration { .. } => "chromatic_aberration",
        klipp_effects::BuiltinEffect::RollingShutterCorrection { .. } => "rolling_shutter_correction",
        klipp_effects::BuiltinEffect::TemporalDenoise { .. } => "temporal_denoise",
        klipp_effects::BuiltinEffect::Deflicker { .. } => "deflicker",
        klipp_effects::BuiltinEffect::VideoStabilization { .. } => "video_stabilization",
        klipp_effects::BuiltinEffect::SpeedRamp { .. } => "speed_ramp",
        klipp_effects::BuiltinEffect::Vibrance { .. } => "vibrance",
        // Third-party plugin — preset key folded under a single bucket
        // (presets across distinct frei0r plugins don't make sense to
        // share). The actual plugin identity lives in `type_id`.
        klipp_effects::BuiltinEffect::Frei0r { .. } => "frei0r",
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
