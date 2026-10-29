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
        Self::with_user_dir(dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("klipp").join("effect_presets"))
    }

    /// The built-in presets and the user's, kept in `user_presets_dir`.
    pub(crate) fn with_user_dir(user_presets_dir: PathBuf) -> Self {
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

    /// The presets of the effect type `type_id` (`"klipp.fx.bloom"`): the
    /// built-in ones and the user's.
    pub fn presets_for_type(&self, type_id: &str) -> Vec<&EffectPreset> {
        self.presets_for(type_id)
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
                                // A built-in effect's preset is keyed by its
                                // catalog type id; a short key (`"bloom"`) is
                                // not read.
                                if preset.builtin.is_some() && preset_type(&preset.effect_key).is_none() {
                                    tracing::warn!("Ignoring preset '{}' at {}: '{}' is not an effect type id", preset.name, path.display(), preset.effect_key);
                                    continue;
                                }
                                self.presets.push(preset);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Klipp's own presets, from its presets file (#2887), each under
    /// its effect's type id.
    fn load_builtin_presets(&mut self) {
        for p in klipp_effects::presets::builtin() {
            if let Some(key) = preset_type_id(&p.effect) {
                self.bi(key, &p.name, &p.description, p.effect.clone());
            }
        }
    }

    fn bi(&mut self, key: &str, name: &str, desc: &str, effect: klipp_effects::BuiltinEffect) {
        // A built-in preset is keyed by its effect's catalog type id.
        debug_assert_eq!(preset_type_id(&effect), Some(key), "preset {name} is listed under {key}");
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

/// The catalog type a preset's key names: its type id
/// (`"klipp.fx.bloom"`). `None` for anything else, an OFX plug-in's
/// identifier or a short key (`"bloom"`), which no preset is keyed by.
pub fn preset_type(key: &str) -> Option<&'static klipp_effects::registry::EffectType> {
    klipp_effects::registry::effect_types().find(|t| t.desc.id == key)
}

/// The catalog type id a built-in effect's presets are keyed by: its
/// type's, found by the effect's stored tag.
pub fn preset_type_id(effect: &klipp_effects::BuiltinEffect) -> Option<&'static str> {
    let tag = serde_json::to_value(effect).ok()?.get("type")?.as_str()?.to_owned();
    klipp_effects::registry::effect_types().find(|t| t.tag == tag).map(|t| t.desc.id)
}

/// A built-in effect's short name, as the row recorder scopes its rows.
pub fn builtin_effect_key(effect: &klipp_effects::BuiltinEffect) -> &'static str {
    match effect {
        klipp_effects::BuiltinEffect::ColorFade { .. } => "color_fade",
        klipp_effects::BuiltinEffect::Opacity { .. } => "opacity",
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => "gaussian_blur",
        klipp_effects::BuiltinEffect::RoundedCorners { .. } => "rounded_corners",
        klipp_effects::BuiltinEffect::DropShadow { .. } => "drop_shadow",
        klipp_effects::BuiltinEffect::OuterGlow { .. } => "outer_glow",
        klipp_effects::BuiltinEffect::FillBars { .. } => "fill_bars",
        klipp_effects::BuiltinEffect::Sharpen { .. } => "sharpen",
        klipp_effects::BuiltinEffect::Crop { .. } => "crop",
        klipp_effects::BuiltinEffect::Transform { .. } => "transform",
        klipp_effects::BuiltinEffect::ChromaKey { .. } => "chroma_key",
        klipp_effects::BuiltinEffect::LumaKey { .. } => "luma_key",
        klipp_effects::BuiltinEffect::CornerPin { .. } => "corner_pin",
        klipp_effects::BuiltinEffect::SurfacePin { .. } => "surface_pin",
        klipp_effects::BuiltinEffect::Warp { .. } => "warp",
        klipp_effects::BuiltinEffect::GradientMap { .. } => "gradient_map",
        klipp_effects::BuiltinEffect::Lut3D { .. } => "lut_3d",
        klipp_effects::BuiltinEffect::ColourSpaceTransform { .. } => "colour_space_transform",
        klipp_effects::BuiltinEffect::PictureTransform { .. } => "picture_transform",
        klipp_effects::BuiltinEffect::Merge { .. } => "merge",
        klipp_effects::BuiltinEffect::Generator { .. } => "generator",
        klipp_effects::BuiltinEffect::MediaSource { .. } => "media_source",
        klipp_effects::BuiltinEffect::TitleSource { .. } => "title_source",
        klipp_effects::BuiltinEffect::ClipBelow {} => "clip_below",
        klipp_effects::BuiltinEffect::GradeCompound {} => "grade_compound",
        klipp_effects::BuiltinEffect::Bloom { .. } => "bloom",
        klipp_effects::BuiltinEffect::Glow { .. } => "glow",
        klipp_effects::BuiltinEffect::LightRays { .. } => "light_rays",
        klipp_effects::BuiltinEffect::Vignette { .. } => "vignette",
        klipp_effects::BuiltinEffect::BinocularVignette { .. } => "binocular_vignette",
        klipp_effects::BuiltinEffect::Flicker { .. } => "flicker",
        klipp_effects::BuiltinEffect::Particles { .. } => "particles",
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
        klipp_effects::BuiltinEffect::Displace { .. } => "displace",
        klipp_effects::BuiltinEffect::Reflection { .. } => "reflection",
        klipp_effects::BuiltinEffect::Dilate { .. } => "dilate",
        klipp_effects::BuiltinEffect::Erode { .. } => "erode",
        klipp_effects::BuiltinEffect::Denoise { .. } => "denoise",
        klipp_effects::BuiltinEffect::Gamma { .. } => "gamma",
        klipp_effects::BuiltinEffect::HSVAdjust { .. } => "hsv_adjust",
        klipp_effects::BuiltinEffect::NoiseGenerator { .. } => "noise_generator",
        klipp_effects::BuiltinEffect::FilmGrain { .. } => "film_grain",
        klipp_effects::BuiltinEffect::TiltShift { .. } => "tilt_shift",
        klipp_effects::BuiltinEffect::FocusBlur { .. } => "focus_blur",
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

/// The particle presets' names and descriptions, by kind (#1141).
/// Particles of `kind` with that kind's own numbers (#1141): what its
/// preset and a reset give.
pub fn particles_default(kind: u32) -> klipp_effects::BuiltinEffect {
    klipp_effects::particles::preset(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(dir: &std::path::Path) -> PresetManager {
        PresetManager::with_user_dir(dir.to_path_buf())
    }

    /// Every built-in preset is keyed by its effect's catalog type id, and
    /// every one is found by it.
    #[test]
    fn every_built_in_preset_is_keyed_by_its_type_id() {
        let m = manager(std::path::Path::new("/nonexistent/klipp-presets"));
        let builtins: Vec<_> = m.presets.iter().filter(|p| p.is_builtin).collect();
        // The four film templates in place of the two old film presets
        // (#2831).
        assert_eq!(builtins.len(), 151);
        for p in &builtins {
            let id = p.builtin.as_ref().and_then(preset_type_id);
            assert_eq!(Some(p.effect_key.as_str()), id, "{}", p.name);
            assert!(m.presets_for_type(&p.effect_key).iter().any(|q| q.name == p.name));
        }
        assert_eq!(m.presets_for_type("klipp.fx.hsv_adjust").len(), 4, "HSV Adjust's, by its type id");
    }

    /// Every effect type's preset, saved as the card saves it (under the
    /// type's id, `generated::presets`) and read back by a fresh manager
    /// from the same folder, is found under that id, and its key and its
    /// effect both resolve to that type.
    #[test]
    fn every_saved_preset_key_still_resolves() {
        let dir = std::env::temp_dir().join(format!("klipp-effect-presets-{}", uuid::Uuid::new_v4()));
        let types: Vec<_> = klipp_effects::registry::effect_types().collect();
        let mut saving = manager(&dir);
        for ty in &types {
            let preset = EffectPreset { name: format!("Mine {}", ty.tag), description: String::new(), effect_key: ty.desc.id.to_owned(), builtin: Some((ty.sample)()), ofx_params: None, is_builtin: false };
            saving.save_user_preset(preset);
        }
        let read = manager(&dir);
        for ty in &types {
            let name = format!("Mine {}", ty.tag);
            let found = read.presets_for_type(ty.desc.id).into_iter().find(|p| p.name == name).unwrap_or_else(|| panic!("{}: the saved preset isn't read back under its id", ty.desc.id));
            assert_eq!(preset_type(&found.effect_key).map(|t| t.desc.id), Some(ty.desc.id), "{}: its key resolves to its type", ty.desc.id);
            assert_eq!(found.builtin.as_ref().and_then(preset_type_id), Some(ty.desc.id), "{}: its effect maps back to its type", ty.desc.id);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A user's preset of a built-in effect is read only under its type
    /// id: one saved under a short key (`"bloom"`) is not read, nor
    /// mapped to the type.
    #[test]
    fn a_preset_under_a_short_key_is_not_read() {
        let dir = std::env::temp_dir().join(format!("klipp-effect-presets-{}", uuid::Uuid::new_v4()));
        let effect = klipp_effects::BuiltinEffect::Bloom { threshold: 0.7, radius: 20.0, intensity: 0.3 };
        for key in ["bloom", "klipp.fx.bloom"] {
            let preset = EffectPreset { name: format!("Mine {key}"), description: String::new(), effect_key: key.into(), builtin: Some(effect.clone()), ofx_params: None, is_builtin: false };
            std::fs::create_dir_all(dir.join(key)).unwrap();
            std::fs::write(dir.join(key).join("mine.json"), serde_json::to_string(&preset).unwrap()).unwrap();
        }
        let m = manager(&dir);
        let user: Vec<_> = m.presets.iter().filter(|p| !p.is_builtin).map(|p| p.effect_key.clone()).collect();
        assert_eq!(user, vec!["klipp.fx.bloom".to_string()], "only the type id's is read");
        assert!(m.presets_for_type("klipp.fx.bloom").iter().any(|p| p.name == "Mine klipp.fx.bloom"));
        assert!(!m.presets_for_type("klipp.fx.bloom").iter().any(|p| p.name == "Mine bloom"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

