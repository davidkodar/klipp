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
    // ── New preference categories ──
    #[serde(default)]
    pub system: SystemPrefs,
    #[serde(default)]
    pub audio_hardware: AudioHardwarePrefs,
    #[serde(default)]
    pub appearance: AppearancePrefs,
    #[serde(default)]
    pub media_cache: MediaCachePrefs,
    #[serde(default)]
    pub color: ColorPrefs,
    #[serde(default)]
    pub title: TitlePrefs,
    #[serde(default)]
    pub timeline: TimelinePrefs,
    #[serde(default)]
    pub viewer: ViewerPrefs,
    #[serde(default)]
    pub advanced: AdvancedPrefs,
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
    // ── Expanded fields ──
    /// Pan law for stereo panning.
    #[serde(default)]
    pub pan_law: klipp_core::PanLaw,
    /// Metering ballistics mode.
    #[serde(default)]
    pub metering_ballistics: klipp_core::MeteringBallistics,
    /// Default fade curve for new clips.
    #[serde(default)]
    pub default_fade_curve: klipp_core::FadeCurve,
    /// Default fade duration in milliseconds (0 = no fade).
    #[serde(default)]
    pub default_fade_duration_ms: u32,
    /// Loudness standard for metering/normalization.
    #[serde(default)]
    pub loudness_standard: klipp_core::LoudnessStandard,
    /// Normalize target in LUFS.
    #[serde(default = "default_normalize_lufs")]
    pub normalize_target_lufs: f32,
}

fn default_normalize_lufs() -> f32 { -14.0 }

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            fade_overlay_mode: FadeOverlayMode::default(),
            show_rubber_band: true,
            show_waveform: true,
            anti_click_fade: true,
            show_crossfade_indicators: true,
            pan_law: Default::default(),
            metering_ballistics: Default::default(),
            default_fade_curve: Default::default(),
            default_fade_duration_ms: 0,
            loudness_standard: Default::default(),
            normalize_target_lufs: default_normalize_lufs(),
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
    // ── Expanded fields ──
    /// Default video track height in pixels.
    #[serde(default = "default_video_track_h")]
    pub default_video_track_height: f32,
    /// Default audio track height in pixels.
    #[serde(default = "default_audio_track_h")]
    pub default_audio_track_height: f32,
    /// Snap threshold in pixels (how close clips must be to snap).
    #[serde(default = "default_snap_px")]
    pub snap_threshold_px: f32,
    /// Zoom step multiplier (e.g. 1.5 = 50% zoom per step).
    #[serde(default = "default_zoom_step")]
    pub zoom_step: f32,
    /// Backup version count (project.json.bak.1 through .bak.N).
    #[serde(default = "default_backup_count")]
    pub backup_count: u32,
}

fn default_undo_limit() -> usize {
    100
}

fn default_transition_frames() -> u32 {
    24
}

fn default_still_duration() -> f64 { 5.0 }
fn default_video_track_h() -> f32 { 50.0 }
fn default_audio_track_h() -> f32 { 48.0 }
fn default_snap_px() -> f32 { 10.0 }
fn default_zoom_step() -> f32 { 1.5 }
fn default_backup_count() -> u32 { 5 }

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
            default_video_track_height: default_video_track_h(),
            default_audio_track_height: default_audio_track_h(),
            snap_threshold_px: default_snap_px(),
            zoom_step: default_zoom_step(),
            backup_count: default_backup_count(),
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

// ══════════════════════════════════════════════════════════
// NEW PREFERENCE SUB-STRUCTS (Phase 1: struct + persistence)
// ══════════════════════════════════════════════════════════

/// System/Performance preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemPrefs {
    /// GPU device preference.
    #[serde(default)]
    pub gpu_preference: klipp_core::GpuPreference,
    /// Decode thread count (0 = auto/all cores).
    #[serde(default)]
    pub decode_threads: u32,
    /// Maximum playback preview height in pixels (e.g. 1080, 2160).
    #[serde(default = "default_max_playback_h")]
    pub max_playback_height: u32,
    /// Decoded frame queue capacity.
    #[serde(default = "default_frame_queue")]
    pub frame_queue_size: u32,
}
fn default_max_playback_h() -> u32 { 1080 }
fn default_frame_queue() -> u32 { 16 }
impl Default for SystemPrefs {
    fn default() -> Self {
        Self { gpu_preference: Default::default(), decode_threads: 0, max_playback_height: default_max_playback_h(), frame_queue_size: default_frame_queue() }
    }
}

/// Audio hardware / device preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioHardwarePrefs {
    /// Output device name (None = system default).
    #[serde(default)]
    pub output_device: Option<String>,
    /// Output sample rate in Hz.
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    /// Audio buffer size in samples.
    #[serde(default = "default_buffer_size")]
    pub buffer_size: u32,
}
fn default_sample_rate() -> u32 { 48000 }
fn default_buffer_size() -> u32 { 512 }
impl Default for AudioHardwarePrefs {
    fn default() -> Self {
        Self { output_device: None, sample_rate: default_sample_rate(), buffer_size: default_buffer_size() }
    }
}

/// Appearance / theme preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearancePrefs {
    /// Base UI text size in points.
    #[serde(default = "default_ui_text_size")]
    pub ui_text_size: f32,
    /// Accent color [R, G, B].
    #[serde(default = "default_accent_color")]
    pub accent_color: [u8; 3],
    /// Playhead color [R, G, B].
    #[serde(default = "default_accent_color")]
    pub playhead_color: [u8; 3],
    // ── Role colors ──
    #[serde(default = "default_role_video")]    pub role_video: [u8; 3],
    #[serde(default = "default_role_dialogue")] pub role_dialogue: [u8; 3],
    #[serde(default = "default_role_music")]    pub role_music: [u8; 3],
    #[serde(default = "default_role_sfx")]      pub role_sfx: [u8; 3],
    #[serde(default = "default_role_title")]    pub role_title: [u8; 3],
    #[serde(default = "default_role_broll")]    pub role_broll: [u8; 3],
    #[serde(default = "default_role_ambience")] pub role_ambience: [u8; 3],
    #[serde(default = "default_role_foley")]    pub role_foley: [u8; 3],
    #[serde(default = "default_role_voiceover")] pub role_voiceover: [u8; 3],
    #[serde(default = "default_role_graphics")] pub role_graphics: [u8; 3],
}
fn default_ui_text_size() -> f32 { 13.0 }
fn default_accent_color() -> [u8; 3] { [0xe8, 0xa0, 0x20] }
fn default_role_video() -> [u8; 3] { [80, 130, 200] }
fn default_role_dialogue() -> [u8; 3] { [100, 180, 100] }
fn default_role_music() -> [u8; 3] { [180, 120, 200] }
fn default_role_sfx() -> [u8; 3] { [200, 160, 60] }
fn default_role_title() -> [u8; 3] { [200, 200, 100] }
fn default_role_broll() -> [u8; 3] { [100, 160, 160] }
fn default_role_ambience() -> [u8; 3] { [140, 140, 100] }
fn default_role_foley() -> [u8; 3] { [160, 100, 100] }
fn default_role_voiceover() -> [u8; 3] { [100, 140, 200] }
fn default_role_graphics() -> [u8; 3] { [200, 140, 160] }
impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            ui_text_size: default_ui_text_size(),
            accent_color: default_accent_color(),
            playhead_color: default_accent_color(),
            role_video: default_role_video(), role_dialogue: default_role_dialogue(),
            role_music: default_role_music(), role_sfx: default_role_sfx(),
            role_title: default_role_title(), role_broll: default_role_broll(),
            role_ambience: default_role_ambience(), role_foley: default_role_foley(),
            role_voiceover: default_role_voiceover(), role_graphics: default_role_graphics(),
        }
    }
}

/// Media cache preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaCachePrefs {
    /// Filmstrip cache directory override (None = ~/.cache/klipp/filmstrips).
    #[serde(default)]
    pub filmstrip_cache_dir: Option<PathBuf>,
    /// Audio conform cache directory override (None = ~/.cache/klipp/audio-cache).
    #[serde(default)]
    pub audio_cache_dir: Option<PathBuf>,
    /// Maximum total cache size in MB (0 = unlimited).
    #[serde(default = "default_cache_size")]
    pub max_cache_size_mb: u32,
    /// Skim transcode quality (CRF value, lower = better quality, larger files).
    #[serde(default = "default_skim_quality")]
    pub skim_transcode_quality: u32,
    /// Poster thumbnail height in pixels.
    #[serde(default = "default_thumb_h")]
    pub thumbnail_height: u32,
}
fn default_cache_size() -> u32 { 4096 }
fn default_skim_quality() -> u32 { 23 }
fn default_thumb_h() -> u32 { 120 }
impl Default for MediaCachePrefs {
    fn default() -> Self {
        Self { filmstrip_cache_dir: None, audio_cache_dir: None, max_cache_size_mb: default_cache_size(), skim_transcode_quality: default_skim_quality(), thumbnail_height: default_thumb_h() }
    }
}

/// Color grading preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorPrefs {
    /// Working color space.
    #[serde(default)]
    pub working_color_space: klipp_core::ColorSpace,
    /// Viewer LUT file path (applied for display only).
    #[serde(default)]
    pub viewer_lut_path: Option<PathBuf>,
    /// Scope rendering quality.
    #[serde(default)]
    pub scope_quality: klipp_core::ScopeQuality,
    /// Scope refresh interval in milliseconds.
    #[serde(default = "default_scope_ms")]
    pub scope_update_interval_ms: u32,
}
fn default_scope_ms() -> u32 { 32 }
impl Default for ColorPrefs {
    fn default() -> Self {
        Self { working_color_space: klipp_core::ColorSpace::Rec709, viewer_lut_path: None, scope_quality: Default::default(), scope_update_interval_ms: default_scope_ms() }
    }
}

/// Title/text defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitlePrefs {
    /// Default font family name.
    #[serde(default = "default_title_font")]
    pub default_font: String,
    /// Default font size in points.
    #[serde(default = "default_title_size")]
    pub default_size_pt: f32,
    /// Additional font search directories.
    #[serde(default)]
    pub font_search_paths: Vec<PathBuf>,
}
fn default_title_font() -> String { String::new() } // empty = system default
fn default_title_size() -> f32 { 48.0 }
impl Default for TitlePrefs {
    fn default() -> Self {
        Self { default_font: default_title_font(), default_size_pt: default_title_size(), font_search_paths: Vec::new() }
    }
}

/// Timeline visual preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelinePrefs {
    /// Clip color palette [R, G, B] per-index.
    #[serde(default = "default_clip_colors")]
    pub clip_colors: Vec<[u8; 3]>,
    /// Gap between clips in pixels.
    #[serde(default = "default_clip_gap")]
    pub clip_gap_px: f32,
    /// Trim handle width in pixels.
    #[serde(default = "default_trim_handle")]
    pub trim_handle_width: f32,
    /// Ruler height in pixels.
    #[serde(default = "default_ruler_h")]
    pub ruler_height: f32,
    /// Clip corner radius.
    #[serde(default = "default_clip_radius")]
    pub clip_corner_radius: f32,
    /// Overview bar height in pixels.
    #[serde(default = "default_overview_h")]
    pub overview_bar_height: f32,
    /// Minimum track height in pixels.
    #[serde(default = "default_min_track_h")]
    pub min_track_height: f32,
    /// Maximum track height in pixels.
    #[serde(default = "default_max_track_h")]
    pub max_track_height: f32,
    /// Text track default height in pixels.
    #[serde(default = "default_text_track_h")]
    pub text_track_height: f32,
}
fn default_clip_colors() -> Vec<[u8; 3]> { vec![[50,90,160],[50,130,80],[160,90,50],[130,50,130],[50,130,130],[160,50,80]] }
fn default_clip_gap() -> f32 { 3.0 }
fn default_trim_handle() -> f32 { 14.0 }
fn default_ruler_h() -> f32 { 24.0 }
fn default_clip_radius() -> f32 { 4.0 }
fn default_overview_h() -> f32 { 24.0 }
fn default_min_track_h() -> f32 { 24.0 }
fn default_max_track_h() -> f32 { 200.0 }
fn default_text_track_h() -> f32 { 32.0 }
impl Default for TimelinePrefs {
    fn default() -> Self {
        Self {
            clip_colors: default_clip_colors(), clip_gap_px: default_clip_gap(),
            trim_handle_width: default_trim_handle(), ruler_height: default_ruler_h(),
            clip_corner_radius: default_clip_radius(), overview_bar_height: default_overview_h(),
            min_track_height: default_min_track_h(), max_track_height: default_max_track_h(),
            text_track_height: default_text_track_h(),
        }
    }
}

/// Viewer panel preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewerPrefs {
    /// Action safe area percentage (0.0 - 1.0).
    #[serde(default = "default_action_safe")]
    pub action_safe_pct: f32,
    /// Title safe area percentage (0.0 - 1.0).
    #[serde(default = "default_title_safe")]
    pub title_safe_pct: f32,
    /// Grid line color [R, G, B, A].
    #[serde(default = "default_grid_color")]
    pub grid_color: [u8; 4],
    /// Checkerboard tile size in pixels.
    #[serde(default = "default_checker_tile")]
    pub checkerboard_tile_size: f32,
    /// Transform handle size in pixels.
    #[serde(default = "default_handle_size")]
    pub transform_handle_size: f32,
    /// Fullscreen overlay auto-hide timeout in seconds.
    #[serde(default = "default_fullscreen_timeout")]
    pub fullscreen_overlay_timeout_secs: f32,
}
fn default_action_safe() -> f32 { 0.9 }
fn default_title_safe() -> f32 { 0.8 }
fn default_grid_color() -> [u8; 4] { [200, 200, 200, 80] }
fn default_checker_tile() -> f32 { 12.0 }
fn default_handle_size() -> f32 { 8.0 }
fn default_fullscreen_timeout() -> f32 { 3.0 }
impl Default for ViewerPrefs {
    fn default() -> Self {
        Self {
            action_safe_pct: default_action_safe(), title_safe_pct: default_title_safe(),
            grid_color: default_grid_color(), checkerboard_tile_size: default_checker_tile(),
            transform_handle_size: default_handle_size(), fullscreen_overlay_timeout_secs: default_fullscreen_timeout(),
        }
    }
}

/// Advanced / power-user preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvancedPrefs {
    /// Toast notification display duration in seconds.
    #[serde(default = "default_toast_secs")]
    pub toast_duration_secs: f64,
    /// Modal dialog backdrop opacity (0-255).
    #[serde(default = "default_modal_alpha")]
    pub modal_backdrop_alpha: u8,
    /// Performance overlay update interval in milliseconds.
    #[serde(default = "default_perf_ms")]
    pub perf_overlay_interval_ms: u32,
    /// Anti-click crossfade duration in milliseconds.
    #[serde(default = "default_anti_click_ms")]
    pub anti_click_fade_ms: f64,
    /// Pre-roll lookahead duration in seconds.
    #[serde(default = "default_preroll_secs")]
    pub pre_roll_secs: f64,
}
fn default_toast_secs() -> f64 { 2.2 }
fn default_modal_alpha() -> u8 { 120 }
fn default_perf_ms() -> u32 { 500 }
fn default_anti_click_ms() -> f64 { 2.0 }
fn default_preroll_secs() -> f64 { 1.5 }
impl Default for AdvancedPrefs {
    fn default() -> Self {
        Self {
            toast_duration_secs: default_toast_secs(), modal_backdrop_alpha: default_modal_alpha(),
            perf_overlay_interval_ms: default_perf_ms(), anti_click_fade_ms: default_anti_click_ms(),
            pre_roll_secs: default_preroll_secs(),
        }
    }
}
