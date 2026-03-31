use klipp_core::{ExportPreset, ProxyResolution};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_true() -> bool { true }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preferences {
    pub general: GeneralPrefs,
    pub playback: PlaybackPrefs,
    #[serde(default)]
    pub editing: EditingPrefs,
    #[serde(default)]
    pub audio: AudioPrefs,
    pub export: ExportPrefs,
    pub proxy: ProxyPrefs,
}

/// Rich recent project entry with cached metadata for the launch screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentProject {
    pub path: PathBuf,
    pub name: String,
    #[serde(default)]
    pub last_saved: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub fps: Option<String>,
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub thumbnail_path: Option<PathBuf>,
    /// Total media size in bytes (cached for display).
    #[serde(default)]
    pub total_size_bytes: Option<u64>,
}

impl RecentProject {
    /// Create a minimal entry from just a path (used for migration from old format).
    pub fn from_path(path: PathBuf) -> Self {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".into());
        Self {
            path,
            name,
            last_saved: None,
            duration_secs: None,
            fps: None,
            resolution: None,
            thumbnail_path: None,
            total_size_bytes: None,
        }
    }
}

/// Custom deserializer that handles both the new Vec<RecentProject> format
/// and the old Vec<PathBuf> format for backward compatibility.
fn deserialize_recent_projects<'de, D>(deserializer: D) -> Result<Vec<RecentProject>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: serde_json::Value = serde::Deserialize::deserialize(deserializer)?;
    // Try new format first
    if let Ok(projects) = serde_json::from_value::<Vec<RecentProject>>(value.clone()) {
        return Ok(projects);
    }
    // Fall back to old Vec<PathBuf> format
    if let Ok(paths) = serde_json::from_value::<Vec<PathBuf>>(value) {
        return Ok(paths.into_iter().map(RecentProject::from_path).collect());
    }
    // If neither works, return empty
    Ok(Vec::new())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralPrefs {
    /// Auto-save interval in seconds. 0 = disabled. Default 300 (5 min).
    pub auto_save_interval_secs: u32,
    pub default_project_dir: Option<PathBuf>,
    /// Most-recent first, max 20 entries.
    #[serde(deserialize_with = "deserialize_recent_projects")]
    pub recent_projects: Vec<RecentProject>,
    /// Remember which inspector sections are collapsed across restarts.
    #[serde(default = "default_true")]
    pub remember_inspector_layout: bool,
    /// Names of inspector sections that are collapsed (persisted when remember_inspector_layout is true).
    #[serde(default)]
    pub collapsed_sections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackPrefs {
    pub scrub_audio: bool,
    /// Whether audio plays during hover skimming (Shift+S toggle).
    #[serde(default = "default_true")]
    pub skim_audio: bool,
    /// Speed above which audio mutes during JKL playback. Default 4.0.
    pub jkl_audio_mute_threshold: f32,
    /// Whether loop playback is enabled (cycle between in/out points or whole sequence).
    #[serde(default)]
    pub loop_playback: bool,
    /// Resolution (max height in pixels) for skim frame decodes. Higher = sharper but slower.
    #[serde(default = "default_skim_resolution")]
    pub skim_resolution: u32,
}

fn default_skim_resolution() -> u32 {
    480
}

/// Where to show audio fade overlays on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FadeOverlayMode {
    /// Show fade overlay on audio track clips only.
    #[default]
    AudioOnly,
    /// Show fade overlay on both video and audio track clips.
    Both,
    /// Don't show fade overlays on the timeline.
    Hidden,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPrefs {
    /// Where to display fade in/out overlays on the timeline.
    #[serde(default)]
    pub fade_overlay_mode: FadeOverlayMode,
    /// Show the rubber band (volume/pan/gain automation line) on audio clips.
    #[serde(default = "default_true")]
    pub show_rubber_band: bool,
    /// Show waveform on audio track clips.
    #[serde(default = "default_true")]
    pub show_waveform: bool,
    /// Apply a short anti-click crossfade (2ms) at clip boundaries to prevent pops.
    #[serde(default = "default_true")]
    pub anti_click_fade: bool,
    /// Show crossfade indicator at adjacent clip boundaries.
    #[serde(default = "default_true")]
    pub show_crossfade_indicators: bool,
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            fade_overlay_mode: FadeOverlayMode::default(),
            show_rubber_band: true,
            show_waveform: true,
            anti_click_fade: true,
            show_crossfade_indicators: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditingPrefs {
    /// Automatically create a new track when a clip is dropped
    /// above the top video track or below the bottom audio track.
    #[serde(default = "default_true")]
    pub auto_create_tracks: bool,
    /// Automatically generate filmstrip thumbnails when media is imported.
    #[serde(default = "default_true")]
    pub auto_generate_filmstrips: bool,
    /// Automatically generate skim transcodes for hover skimming on import.
    #[serde(default = "default_true")]
    pub auto_generate_skim: bool,
    /// Default duration in seconds for imported still images (PNG, JPG, etc.).
    #[serde(default = "default_still_duration")]
    pub default_still_duration_secs: f64,
    /// Media browser view mode (persisted across sessions).
    #[serde(default)]
    pub media_browser_view_mode: crate::browser::media_tab::BrowserViewMode,
    /// Maximum number of undo steps (0 = unlimited).
    #[serde(default = "default_undo_limit")]
    pub undo_limit: usize,
    /// Default transition duration in frames (e.g. 24 frames = 1 second at 24fps).
    #[serde(default = "default_transition_frames")]
    pub default_transition_frames: u32,
}

fn default_undo_limit() -> usize {
    100
}

fn default_transition_frames() -> u32 {
    24
}

fn default_still_duration() -> f64 {
    5.0
}

impl Default for EditingPrefs {
    fn default() -> Self {
        Self {
            auto_create_tracks: true,
            auto_generate_filmstrips: true,
            auto_generate_skim: true,
            default_still_duration_secs: default_still_duration(),
            undo_limit: default_undo_limit(),
            media_browser_view_mode: Default::default(),
            default_transition_frames: default_transition_frames(),
        }
    }
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
            remember_inspector_layout: true,
            collapsed_sections: Vec::new(),
        }
    }
}

impl Default for PlaybackPrefs {
    fn default() -> Self {
        Self {
            scrub_audio: true,
            skim_audio: true,
            jkl_audio_mute_threshold: 4.0,
            loop_playback: false,
            skim_resolution: default_skim_resolution(),
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

    /// Add a project to the recent projects list (most recent first, max 20).
    pub fn add_recent_project(&mut self, project: RecentProject) {
        self.general.recent_projects.retain(|p| p.path != project.path);
        self.general.recent_projects.insert(0, project);
        self.general.recent_projects.truncate(20);
    }

    /// Enrich recent project entries that are missing metadata by reading
    /// from the project files on disk. Returns true if any entries were updated.
    pub fn enrich_recent_projects(&mut self) -> bool {
        let mut changed = false;
        for rp in &mut self.general.recent_projects {
            if rp.total_size_bytes.is_some() && rp.duration_secs.is_some() && rp.fps.is_some() {
                continue; // already has data
            }
            if !rp.path.exists() {
                continue;
            }
            // Try to load project metadata from disk
            match klipp_project::load_project(&rp.path) {
                Ok((sequences, media_pool, _color, _active_id, _created_at)) => {
                    if rp.total_size_bytes.is_none() {
                        rp.total_size_bytes = Some(media_pool.all().map(|m| m.file_size_bytes).sum());
                        changed = true;
                    }
                    if let Some(seq) = sequences.first() {
                        if rp.duration_secs.is_none() {
                            rp.duration_secs = Some(seq.total_duration().to_secs_f64());
                            changed = true;
                        }
                        if rp.fps.is_none() {
                            rp.fps = Some(if seq.fps.den == 1 {
                                format!("{}fps", seq.fps.num)
                            } else {
                                format!("{:.3}fps", seq.fps.num as f64 / seq.fps.den as f64)
                            });
                            changed = true;
                        }
                        if rp.resolution.is_none() {
                            rp.resolution = Some(format!(
                                "{}x{}",
                                seq.resolution.width, seq.resolution.height
                            ));
                            changed = true;
                        }
                        if rp.name.is_empty() || rp.name == rp.path.file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default()
                        {
                            // Use sequence name if project name is just the directory stem
                            rp.name = seq.name.clone();
                            changed = true;
                        }
                    }
                    tracing::info!("Enriched recent project metadata for {:?}", rp.path);
                }
                Err(e) => {
                    tracing::debug!("Could not enrich recent project {:?}: {e}", rp.path);
                }
            }
        }
        changed
    }
}
