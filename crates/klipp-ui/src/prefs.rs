use klipp_core::{ExportPreset, ProxyResolution};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_true() -> bool { true }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preferences {
    pub general: GeneralPrefs,
    pub playback: PlaybackPrefs,
    pub export: ExportPrefs,
    pub proxy: ProxyPrefs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralPrefs {
    /// Auto-save interval in seconds. 0 = disabled. Default 300 (5 min).
    pub auto_save_interval_secs: u32,
    pub default_project_dir: Option<PathBuf>,
    /// Most-recent first, max 10 entries.
    pub recent_projects: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackPrefs {
    pub scrub_audio: bool,
    /// Whether audio plays during hover skimming (Shift+S toggle).
    #[serde(default = "default_true")]
    pub skim_audio: bool,
    /// Speed above which audio mutes during JKL playback. Default 4.0.
    pub jkl_audio_mute_threshold: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportPrefs {
    pub default_output_dir: Option<PathBuf>,
    pub last_preset_id: Option<String>,
    pub custom_presets: Vec<ExportPreset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPrefs {
    pub auto_generate_on_import: bool,
    pub proxy_resolution: ProxyResolution,
    /// None = ~/.cache/klipp/proxies
    pub proxy_cache_dir: Option<PathBuf>,
}

impl Default for GeneralPrefs {
    fn default() -> Self {
        Self {
            auto_save_interval_secs: 300,
            default_project_dir: None,
            recent_projects: Vec::new(),
        }
    }
}

impl Default for PlaybackPrefs {
    fn default() -> Self {
        Self {
            scrub_audio: true,
            skim_audio: true,
            jkl_audio_mute_threshold: 4.0,
        }
    }
}

impl Default for ProxyPrefs {
    fn default() -> Self {
        Self {
            auto_generate_on_import: true,
            proxy_resolution: ProxyResolution::default(),
            proxy_cache_dir: None,
        }
    }
}

impl Preferences {
    /// Returns the config file path: ~/.config/klipp/preferences.json
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("klipp").join("preferences.json"))
    }

    /// Load preferences from disk. Returns defaults if file doesn't exist or is malformed.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            tracing::warn!("Could not determine config directory, using default preferences");
            return Self::default();
        };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(prefs) => {
                    tracing::info!("Preferences loaded from {}", path.display());
                    prefs
                }
                Err(e) => {
                    tracing::warn!("Failed to parse preferences ({}), using defaults", e);
                    Self::default()
                }
            },
            Err(_) => {
                tracing::info!("No preferences file found, using defaults");
                Self::default()
            }
        }
    }

    /// Save preferences to disk.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::config_path() else {
            return Err("Could not determine config directory".into());
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {e}"))?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize preferences: {e}"))?;
        std::fs::write(&path, json)
            .map_err(|e| format!("Failed to write preferences: {e}"))?;

        tracing::info!("Preferences saved to {}", path.display());
        Ok(())
    }

    /// Add a project path to the recent projects list (most recent first, max 10).
    pub fn add_recent_project(&mut self, path: PathBuf) {
        self.general.recent_projects.retain(|p| p != &path);
        self.general.recent_projects.insert(0, path);
        self.general.recent_projects.truncate(10);
    }
}
