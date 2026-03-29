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
        self.bi("chroma_key", "Green Screen", "Standard green screen key",
            klipp_effects::BuiltinEffect::ChromaKey { key_color: [0, 177, 64], tolerance: 0.25, spill_suppression: 0.15 });
        self.bi("chroma_key", "Blue Screen", "Standard blue screen key",
            klipp_effects::BuiltinEffect::ChromaKey { key_color: [0, 71, 187], tolerance: 0.25, spill_suppression: 0.15 });
        self.bi("chroma_key", "Green Screen (Tight)", "Clean green screen with tight key",
            klipp_effects::BuiltinEffect::ChromaKey { key_color: [0, 177, 64], tolerance: 0.15, spill_suppression: 0.25 });
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
        klipp_effects::BuiltinEffect::Opacity { .. } => "opacity",
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => "gaussian_blur",
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
        klipp_effects::BuiltinEffect::SpeedRamp { .. } => "speed_ramp",
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
