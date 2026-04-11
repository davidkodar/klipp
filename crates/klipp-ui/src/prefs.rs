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
    pub file_browser: FileBrowserPrefs,
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
    /// Number of clips across all tracks.
    #[serde(default)]
    pub clip_count: Option<u32>,
    /// Number of tracks.
    #[serde(default)]
    pub track_count: Option<u32>,
    /// True if any media item is offline (file missing).
    #[serde(default)]
    pub missing_media: Option<bool>,
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
            clip_count: None,
            track_count: None,
            missing_media: None,
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
    /// What to show when Klipp starts.
    #[serde(default)]
    pub startup_behavior: StartupBehavior,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StartupBehavior {
    /// Show the project manager / welcome screen.
    #[default]
    ProjectManager,
    /// Open a blank empty project immediately.
    EmptyProject,
    /// Reopen the most recently saved project.
    LastProject,
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
    /// Playback quality preset.
    #[serde(default)]
    pub playback_quality: klipp_core::PlaybackQuality,
    /// Pre-render complex sections in background.
    #[serde(default = "default_true")]
    pub background_render: bool,
    /// Pre-roll duration in seconds before playback starts.
    #[serde(default = "default_pre_roll")]
    pub pre_roll_duration: f32,
    /// JKL speed step multiplier per press (e.g. 2.0 = 2x, 4x, 8x...).
    #[serde(default = "default_jkl_step")]
    pub jkl_speed_step: f32,
    /// Dim everything outside the viewer during playback for cinematic focus.
    #[serde(default = "default_true")]
    pub dim_ui_on_play: bool,
}
fn default_pre_roll() -> f32 { 2.0 }
fn default_jkl_step() -> f32 { 2.0 }

fn default_skim_resolution() -> u32 {
    480
}

/// Default multicam sync method for aligning angles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MulticamSyncMethod {
    #[default]
    Manual,
    Timecode,
    AudioWaveform,
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
    /// Peak hold time in seconds for audio meters.
    #[serde(default = "default_peak_hold")]
    pub peak_hold_time_secs: f32,
}

fn default_normalize_lufs() -> f32 { -23.0 }
fn default_peak_hold() -> f32 { 3.0 }

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
            peak_hold_time_secs: default_peak_hold(),
        }
    }
}

/// File browser preferences. Lives in its own struct (and own prefs tab)
/// because the file browser has accumulated enough settings — thumbnail
/// size, sort, filter, pinned favorites, hover-skim toggles — that
/// burying them under "Editing" was hurting discoverability.
///
/// Backward compatibility: prior versions stored these fields directly
/// inside `EditingPrefs`. `Preferences::load` runs a small migration
/// pass that moves any legacy `editing.file_*` keys into the new
/// `file_browser` section before serde sees them, so existing users'
/// settings are preserved on first launch after the upgrade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileBrowserPrefs {
    /// Files-tab thumbnail size (Small/Medium/Large).
    #[serde(default)]
    pub file_thumb_size: crate::browser::FileThumbSize,
    /// Files-tab sort key (Name / Date / Size / Type).
    #[serde(default)]
    pub file_sort_by: crate::browser::FileSortBy,
    /// Files-tab sort direction (true = ascending).
    #[serde(default = "default_true")]
    pub file_sort_ascending: bool,
    /// Files-tab quick type filter (All / Video / Audio / Images).
    #[serde(default)]
    pub file_type_filter: crate::browser::FileTypeFilter,
    /// User-pinned favorite folders shown in the Files-tab bookmarks bar.
    #[serde(default)]
    pub pinned_folders: Vec<std::path::PathBuf>,
    /// Enable hover-skim previews for VIDEO files in the file browser.
    /// When off, hovering a video row leaves the cached poster thumbnail
    /// in place — no per-frame decode happens. Off-by-default users:
    /// people on slow disks, low-power machines, or anyone who finds the
    /// constant decoding distracting.
    #[serde(default = "default_true")]
    pub file_browser_video_skim: bool,
    /// Enable hover-skim playback for AUDIO files in the file browser.
    /// When off, hovering an audio row only shows its waveform thumbnail
    /// — no decoding, no audio output. Useful for users who don't want
    /// audio interrupting their work or who are reviewing files in a
    /// shared/quiet environment.
    #[serde(default = "default_true")]
    pub file_browser_audio_skim: bool,
}

impl Default for FileBrowserPrefs {
    fn default() -> Self {
        Self {
            file_thumb_size: Default::default(),
            file_sort_by: Default::default(),
            file_sort_ascending: true,
            file_type_filter: Default::default(),
            pinned_folders: Vec::new(),
            file_browser_video_skim: true,
            file_browser_audio_skim: true,
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
    /// Whether playhead scrolls to follow playback position.
    #[serde(default = "default_true")]
    pub playhead_follows_playback: bool,
    /// Enable magnetic timeline (FCP-style collision-based editing).
    #[serde(default = "default_true")]
    pub magnetic_timeline: bool,
    /// Default multicam sync method.
    #[serde(default)]
    pub multicam_sync_method: MulticamSyncMethod,
    /// Auto-switch to multicam viewer when multicam clip is selected.
    #[serde(default = "default_true")]
    pub multicam_auto_viewer: bool,
    /// Show angle numbers overlay on multicam clips in timeline.
    #[serde(default = "default_true")]
    pub multicam_show_angles: bool,
    /// Default tracker search region radius in pixels.
    #[serde(default = "default_track_search_radius")]
    pub track_search_radius: u32,
    /// Track point patch size (half-width) for matching.
    #[serde(default = "default_track_patch_size")]
    pub track_patch_size: u32,
    /// SAD threshold for tracking loss detection.
    #[serde(default = "default_track_threshold")]
    pub track_loss_threshold: u32,
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
fn default_track_search_radius() -> u32 { 32 }
fn default_track_patch_size() -> u32 { 6 }
fn default_track_threshold() -> u32 { 25 }

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
            playhead_follows_playback: true,
            magnetic_timeline: true,
            multicam_sync_method: MulticamSyncMethod::default(),
            multicam_auto_viewer: true,
            multicam_show_angles: true,
            track_search_radius: default_track_search_radius(),
            track_patch_size: default_track_patch_size(),
            track_loss_threshold: default_track_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPrefs {
    pub default_output_dir: Option<PathBuf>,
    pub last_preset_id: Option<String>,
    pub custom_presets: Vec<ExportPreset>,
    /// Default encoder preset label (e.g. "H.264 High Quality").
    #[serde(default = "default_encoder_preset")]
    pub default_encoder_preset: String,
    /// Color space tag written to exported file metadata.
    #[serde(default)]
    pub color_space_tag: klipp_core::ColorSpace,
    /// Audio sample rate for export.
    #[serde(default = "default_export_sr")]
    pub audio_sample_rate: u32,
    /// What to do when the output file already exists.
    #[serde(default)]
    pub overwrite_mode: OverwriteMode,
    /// Run render jobs in parallel when possible.
    #[serde(default = "default_true")]
    pub parallel_render: bool,
    /// Open the output folder in file manager after export finishes.
    #[serde(default)]
    pub open_folder_after_export: bool,
}
fn default_encoder_preset() -> String { "H.264 High Quality".into() }
fn default_export_sr() -> u32 { 48000 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OverwriteMode {
    #[default]
    Ask,
    Always,
    Never,
}

impl Default for ExportPrefs {
    fn default() -> Self {
        Self {
            default_output_dir: None,
            last_preset_id: None,
            custom_presets: Vec::new(),
            default_encoder_preset: default_encoder_preset(),
            color_space_tag: klipp_core::ColorSpace::Rec709,
            audio_sample_rate: default_export_sr(),
            overwrite_mode: Default::default(),
            parallel_render: true,
            open_folder_after_export: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPrefs {
    pub auto_generate_on_import: bool,
    /// Automatically switch to proxy files during timeline scrubbing for speed.
    /// Disabled by default — opt-in only.
    #[serde(default)]
    pub auto_switch_during_scrub: bool,
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
            startup_behavior: StartupBehavior::default(),
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
            playback_quality: Default::default(),
            background_render: true,
            pre_roll_duration: default_pre_roll(),
            jkl_speed_step: default_jkl_step(),
            dim_ui_on_play: true,
        }
    }
}

impl Default for ProxyPrefs {
    fn default() -> Self {
        Self {
            auto_generate_on_import: false,
            auto_switch_during_scrub: false,
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
    /// Runs `migrate_legacy_editing_to_file_browser` before deserializing so that
    /// file browser fields stored under the old `editing.*` location are
    /// transparently moved to the new `file_browser.*` section on first load.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            tracing::warn!("Could not determine config directory, using default preferences");
            return Self::default();
        };

        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                // Two-pass parse: first into a generic JSON Value so we can
                // run the legacy field migration, then into the typed
                // Preferences struct. The migration is idempotent — if the
                // new fields already exist, the legacy ones are just dropped.
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(&contents);
                match parsed {
                    Ok(mut value) => {
                        Self::migrate_legacy_editing_to_file_browser(&mut value);
                        match serde_json::from_value::<Self>(value) {
                            Ok(prefs) => {
                                tracing::info!("Preferences loaded from {}", path.display());
                                prefs
                            }
                            Err(e) => {
                                tracing::warn!("Failed to parse preferences ({}), using defaults", e);
                                Self::default()
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read preferences JSON ({}), using defaults", e);
                        Self::default()
                    }
                }
            }
            Err(_) => {
                tracing::info!("No preferences file found, using defaults");
                Self::default()
            }
        }
    }

    /// Migrate file-browser-related fields from the old `editing.*` location
    /// to the new `file_browser.*` location. Operates on the raw JSON Value
    /// so the typed deserialize step sees the fields under their current
    /// names. Idempotent: any field already present in `file_browser` wins
    /// over the legacy `editing` value, and legacy fields are removed from
    /// `editing` regardless so they don't accumulate stale state.
    ///
    /// Was added when the file browser settings were extracted into their
    /// own preferences tab. The legacy fields lived in `EditingPrefs` from
    /// the original "everything related to editing-mode UI lives here"
    /// design until users (rightly) couldn't find them.
    fn migrate_legacy_editing_to_file_browser(value: &mut serde_json::Value) {
        // Field names that moved from `editing.*` to `file_browser.*`.
        const MIGRATED_FIELDS: &[&str] = &[
            "file_thumb_size",
            "file_sort_by",
            "file_sort_ascending",
            "file_type_filter",
            "pinned_folders",
            "file_browser_video_skim",
            "file_browser_audio_skim",
        ];

        let Some(obj) = value.as_object_mut() else { return };

        // First pass: collect any legacy values from `editing.*`. We do this
        // in a separate phase because we need a mutable borrow of `editing`
        // to remove fields, then a mutable borrow of `file_browser` to
        // insert them — the borrow checker won't allow both at once.
        let mut moved: Vec<(&'static str, serde_json::Value)> = Vec::new();
        if let Some(editing) = obj.get_mut("editing").and_then(|v| v.as_object_mut()) {
            for &field in MIGRATED_FIELDS {
                if let Some(v) = editing.remove(field) {
                    moved.push((field, v));
                }
            }
        }

        if moved.is_empty() {
            return; // nothing to migrate
        }

        // Second pass: insert into `file_browser.*`, but only for fields
        // that aren't already present in the new location (so an explicit
        // new-format value wins over a stale legacy value).
        let file_browser = obj
            .entry("file_browser")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let Some(fb_obj) = file_browser.as_object_mut() else { return };
        for (field, value) in moved {
            if !fb_obj.contains_key(field) {
                fb_obj.insert(field.to_string(), value);
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
            if rp.total_size_bytes.is_some()
                && rp.duration_secs.is_some()
                && rp.fps.is_some()
                && rp.clip_count.is_some()
                && rp.missing_media.is_some()
            {
                continue; // already has data
            }
            if !rp.path.exists() {
                continue;
            }

            // Populate last_saved from project.json mtime if not set
            if rp.last_saved.is_none() {
                let project_json = rp.path.join("project.json");
                if let Ok(meta) = std::fs::metadata(&project_json) {
                    if let Ok(mtime) = meta.modified() {
                        let dt: chrono::DateTime<chrono::Local> = mtime.into();
                        rp.last_saved = Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
                        changed = true;
                    }
                }
            }

            // Try to load project metadata from disk
            match klipp_project::load_project(&rp.path) {
                Ok((sequences, media_pool, _color, _active_id, _created_at)) => {
                    if rp.total_size_bytes.is_none() {
                        rp.total_size_bytes = Some(media_pool.all().map(|m| m.file_size_bytes).sum());
                        changed = true;
                    }
                    // Missing media detection
                    if rp.missing_media.is_none() {
                        let has_offline = media_pool.all().any(|m| m.offline || !m.path.exists());
                        rp.missing_media = Some(has_offline);
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
                        // Clip and track counts
                        if rp.clip_count.is_none() {
                            let total: u32 = seq.tracks.iter()
                                .map(|t| t.clips.len() as u32)
                                .sum();
                            rp.clip_count = Some(total);
                            changed = true;
                        }
                        if rp.track_count.is_none() {
                            rp.track_count = Some(seq.tracks.len() as u32);
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
    /// Enable hardware video decode (NVDEC / VAAPI).
    #[serde(default = "default_true")]
    pub hardware_decode: bool,
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
        Self { gpu_preference: Default::default(), hardware_decode: true, decode_threads: 0, max_playback_height: default_max_playback_h(), frame_queue_size: default_frame_queue() }
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
    /// Audio monitoring mode (Software vs Hardware direct).
    #[serde(default)]
    pub monitoring_mode: MonitoringMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MonitoringMode {
    #[default]
    Software,
    Hardware,
}

fn default_sample_rate() -> u32 { 48000 }
fn default_buffer_size() -> u32 { 256 }
impl Default for AudioHardwarePrefs {
    fn default() -> Self {
        Self { output_device: None, sample_rate: default_sample_rate(), buffer_size: default_buffer_size(), monitoring_mode: Default::default() }
    }
}

/// Appearance / theme preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppearancePrefs {
    /// Base UI text size in points (legacy, used as fallback).
    #[serde(default = "default_ui_text_size")]
    pub ui_text_size: f32,
    /// UI scale percentage. 0 = Auto (match system DPI).
    #[serde(default)]
    pub ui_scale_percent: u32,
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
// Role colors match theme.rs Default exactly
fn default_role_video() -> [u8; 3] { [0x2d, 0x6f, 0xa8] }
fn default_role_dialogue() -> [u8; 3] { [0x3a, 0x8c, 0x6e] }
fn default_role_music() -> [u8; 3] { [0x42, 0x85, 0xf4] }
fn default_role_sfx() -> [u8; 3] { [0xa8, 0x5c, 0x2d] }
fn default_role_title() -> [u8; 3] { [0xc0, 0x39, 0x2b] }
fn default_role_broll() -> [u8; 3] { [0x4a, 0x6e, 0xa8] }
fn default_role_ambience() -> [u8; 3] { [0x9c, 0x27, 0xb0] }
fn default_role_foley() -> [u8; 3] { [0xe9, 0x1e, 0x63] }
fn default_role_voiceover() -> [u8; 3] { [0x00, 0xbc, 0xd4] }
fn default_role_graphics() -> [u8; 3] { [0xff, 0xeb, 0x3b] }
impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            ui_text_size: default_ui_text_size(),
            ui_scale_percent: 0, // Auto
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
    /// Filmstrip/thumbnail cache directory override (None = ~/.cache/klipp/thumbnails).
    #[serde(default)]
    pub filmstrip_cache_dir: Option<PathBuf>,
    /// Audio/waveform cache directory override (None = ~/.cache/klipp/waveforms).
    #[serde(default)]
    pub audio_cache_dir: Option<PathBuf>,
    /// Render cache directory override (None = ~/.cache/klipp/render).
    #[serde(default)]
    pub render_cache_dir: Option<PathBuf>,
    /// Enable smart render cache (pre-renders graded frames to disk).
    #[serde(default = "default_true")]
    pub enable_render_cache: bool,
    /// Maximum total cache size in GB.
    #[serde(default = "default_cache_size_gb")]
    pub max_cache_size_gb: u32,
    /// Skim transcode quality (CRF value, lower = better quality, larger files).
    #[serde(default = "default_skim_quality")]
    pub skim_transcode_quality: u32,
    /// Poster thumbnail height in pixels.
    #[serde(default = "default_thumb_h")]
    pub thumbnail_height: u32,
    /// Legacy field — replaced by max_cache_size_gb.
    #[serde(default)]
    pub max_cache_size_mb: u32,
}
fn default_cache_size_gb() -> u32 { 20 }
fn default_skim_quality() -> u32 { 23 }
fn default_thumb_h() -> u32 { 256 }
impl Default for MediaCachePrefs {
    fn default() -> Self {
        Self {
            filmstrip_cache_dir: None, audio_cache_dir: None, render_cache_dir: None, enable_render_cache: true,
            max_cache_size_gb: default_cache_size_gb(),
            skim_transcode_quality: default_skim_quality(),
            thumbnail_height: default_thumb_h(),
            max_cache_size_mb: 0,
        }
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
    /// LUT interpolation method.
    #[serde(default)]
    pub lut_interpolation: LutInterpolation,
    /// Scope rendering quality.
    #[serde(default)]
    pub scope_quality: klipp_core::ScopeQuality,
    /// Scope refresh interval in milliseconds.
    #[serde(default = "default_scope_ms")]
    pub scope_update_interval_ms: u32,
    /// Vectorscope skin-tone line angle in degrees.
    #[serde(default = "default_skin_angle")]
    pub vectorscope_skin_angle: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LutInterpolation {
    Trilinear,
    #[default]
    Tetrahedral,
}

fn default_scope_ms() -> u32 { 32 }
fn default_skin_angle() -> f32 { 123.0 }
impl Default for ColorPrefs {
    fn default() -> Self {
        Self {
            working_color_space: klipp_core::ColorSpace::Rec709,
            viewer_lut_path: None,
            lut_interpolation: Default::default(),
            scope_quality: Default::default(),
            scope_update_interval_ms: default_scope_ms(),
            vectorscope_skin_angle: default_skin_angle(),
        }
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
    /// Default title color as RGB.
    #[serde(default = "default_title_color")]
    pub default_color: [u8; 3],
    /// Default text alignment.
    #[serde(default)]
    pub default_alignment: TextAlignment,
    /// Default title clip duration in seconds.
    #[serde(default = "default_title_duration")]
    pub default_duration_secs: f32,
    /// Show title-safe area overlay in viewer.
    #[serde(default = "default_true")]
    pub safe_area_overlay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextAlignment {
    Left,
    #[default]
    Center,
    Right,
}

fn default_title_font() -> String { String::new() }
fn default_title_size() -> f32 { 48.0 }
fn default_title_color() -> [u8; 3] { [255, 255, 255] }
fn default_title_duration() -> f32 { 5.0 }
impl Default for TitlePrefs {
    fn default() -> Self {
        Self {
            default_font: default_title_font(),
            default_size_pt: default_title_size(),
            font_search_paths: Vec::new(),
            default_color: default_title_color(),
            default_alignment: Default::default(),
            default_duration_secs: default_title_duration(),
            safe_area_overlay: true,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: serialize to JSON string and deserialize back.
    fn round_trip(prefs: &Preferences) -> Preferences {
        let json = serde_json::to_string(prefs).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    // ── 1. Default round-trip ──

    #[test]
    fn default_round_trip() {
        let orig = Preferences::default();
        let rt = round_trip(&orig);

        // GeneralPrefs
        assert_eq!(rt.general.auto_save_interval_secs, 300);
        assert!(rt.general.default_project_dir.is_none());
        assert!(rt.general.recent_projects.is_empty());
        assert!(rt.general.remember_inspector_layout);
        assert!(rt.general.collapsed_sections.is_empty());

        // PlaybackPrefs
        assert!(rt.playback.scrub_audio);
        assert!(rt.playback.skim_audio);
        assert_eq!(rt.playback.jkl_audio_mute_threshold, 4.0);
        assert!(!rt.playback.loop_playback);
        assert_eq!(rt.playback.skim_resolution, 480);

        // EditingPrefs
        assert!(rt.editing.auto_create_tracks);
        assert!(rt.editing.auto_generate_filmstrips);
        assert!(rt.editing.auto_generate_skim);
        assert_eq!(rt.editing.default_still_duration_secs, 5.0);
        assert_eq!(rt.editing.undo_limit, 100);
        assert_eq!(rt.editing.default_transition_frames, 24);
        assert_eq!(rt.editing.default_video_track_height, 50.0);
        assert_eq!(rt.editing.default_audio_track_height, 48.0);
        assert_eq!(rt.editing.snap_threshold_px, 10.0);
        assert_eq!(rt.editing.zoom_step, 1.5);
        assert_eq!(rt.editing.backup_count, 5);

        // FileBrowserPrefs (extracted from EditingPrefs in r58)
        assert!(rt.file_browser.file_browser_video_skim);
        assert!(rt.file_browser.file_browser_audio_skim);
        assert!(rt.file_browser.file_sort_ascending);
        assert!(rt.file_browser.pinned_folders.is_empty());

        // AudioPrefs
        assert_eq!(rt.audio.fade_overlay_mode, FadeOverlayMode::AudioOnly);
        assert!(rt.audio.show_rubber_band);
        assert!(rt.audio.show_waveform);
        assert!(rt.audio.anti_click_fade);
        assert!(rt.audio.show_crossfade_indicators);
        assert_eq!(rt.audio.pan_law, klipp_core::PanLaw::Linear);
        assert_eq!(rt.audio.metering_ballistics, klipp_core::MeteringBallistics::Peak);
        assert_eq!(rt.audio.default_fade_curve, klipp_core::FadeCurve::Linear);
        assert_eq!(rt.audio.default_fade_duration_ms, 0);
        assert_eq!(rt.audio.loudness_standard, klipp_core::LoudnessStandard::EbuR128);
        assert_eq!(rt.audio.normalize_target_lufs, -23.0);

        // ExportPrefs
        assert!(rt.export.default_output_dir.is_none());
        assert!(rt.export.last_preset_id.is_none());
        assert!(rt.export.custom_presets.is_empty());

        // ProxyPrefs
        assert!(!rt.proxy.auto_generate_on_import);
        assert_eq!(rt.proxy.proxy_resolution, ProxyResolution::default());
        assert!(rt.proxy.proxy_cache_dir.is_none());

        // SystemPrefs
        assert_eq!(rt.system.gpu_preference, klipp_core::GpuPreference::Auto);
        assert_eq!(rt.system.decode_threads, 0);
        assert_eq!(rt.system.max_playback_height, 1080);
        assert_eq!(rt.system.frame_queue_size, 16);

        // AudioHardwarePrefs
        assert!(rt.audio_hardware.output_device.is_none());
        assert_eq!(rt.audio_hardware.sample_rate, 48000);
        assert_eq!(rt.audio_hardware.buffer_size, 256);

        // AppearancePrefs
        assert_eq!(rt.appearance.ui_text_size, 13.0);
        assert_eq!(rt.appearance.accent_color, [0xe8, 0xa0, 0x20]);
        assert_eq!(rt.appearance.playhead_color, [0xe8, 0xa0, 0x20]);

        // MediaCachePrefs
        assert!(rt.media_cache.filmstrip_cache_dir.is_none());
        assert!(rt.media_cache.audio_cache_dir.is_none());
        assert_eq!(rt.media_cache.max_cache_size_gb, 20);
        assert_eq!(rt.media_cache.skim_transcode_quality, 23);
        assert_eq!(rt.media_cache.thumbnail_height, 256);

        // ColorPrefs
        assert_eq!(rt.color.working_color_space, klipp_core::ColorSpace::Rec709);
        assert!(rt.color.viewer_lut_path.is_none());
        assert_eq!(rt.color.scope_quality, klipp_core::ScopeQuality::Medium);
        assert_eq!(rt.color.scope_update_interval_ms, 32);

        // TitlePrefs
        assert!(rt.title.default_font.is_empty());
        assert_eq!(rt.title.default_size_pt, 48.0);
        assert!(rt.title.font_search_paths.is_empty());

        // TimelinePrefs
        assert_eq!(rt.timeline.clip_colors.len(), 6);
        assert_eq!(rt.timeline.clip_gap_px, 3.0);
        assert_eq!(rt.timeline.trim_handle_width, 14.0);
        assert_eq!(rt.timeline.ruler_height, 24.0);
        assert_eq!(rt.timeline.clip_corner_radius, 4.0);
        assert_eq!(rt.timeline.overview_bar_height, 24.0);
        assert_eq!(rt.timeline.min_track_height, 24.0);
        assert_eq!(rt.timeline.max_track_height, 200.0);
        assert_eq!(rt.timeline.text_track_height, 32.0);

        // ViewerPrefs
        assert_eq!(rt.viewer.action_safe_pct, 0.9);
        assert_eq!(rt.viewer.title_safe_pct, 0.8);
        assert_eq!(rt.viewer.grid_color, [200, 200, 200, 80]);
        assert_eq!(rt.viewer.checkerboard_tile_size, 12.0);
        assert_eq!(rt.viewer.transform_handle_size, 8.0);
        assert_eq!(rt.viewer.fullscreen_overlay_timeout_secs, 3.0);

        // AdvancedPrefs
        assert_eq!(rt.advanced.toast_duration_secs, 2.2);
        assert_eq!(rt.advanced.modal_backdrop_alpha, 120);
        assert_eq!(rt.advanced.perf_overlay_interval_ms, 500);
        assert_eq!(rt.advanced.anti_click_fade_ms, 2.0);
        assert_eq!(rt.advanced.pre_roll_secs, 1.5);
    }

    // ── 2. Backward compatibility: minimal JSON with required sections ──

    /// Minimal JSON that satisfies non-defaulted top-level fields.
    /// Note: `general`, `playback`, `export`, `proxy` lack `#[serde(default)]`
    /// on the Preferences struct, so they must be present. All `#[serde(default)]`
    /// sections (editing, audio, system, appearance, etc.) can be omitted.
    const MINIMAL_JSON: &str = r#"{
        "general": {
            "auto_save_interval_secs": 300,
            "recent_projects": []
        },
        "playback": {
            "scrub_audio": true,
            "jkl_audio_mute_threshold": 4.0
        },
        "export": {
            "custom_presets": []
        },
        "proxy": {
            "auto_generate_on_import": true,
            "proxy_resolution": "Half"
        }
    }"#;

    #[test]
    fn minimal_json_gives_defaults_for_optional_sections() {
        let prefs: Preferences = serde_json::from_str(MINIMAL_JSON)
            .expect("deserialize minimal");
        // Required sections present with their values
        assert_eq!(prefs.general.auto_save_interval_secs, 300);
        assert!(prefs.playback.scrub_audio);
        assert!(prefs.export.custom_presets.is_empty());
        assert!(prefs.proxy.auto_generate_on_import);

        // All #[serde(default)] sections get defaults
        assert!(prefs.editing.auto_create_tracks);
        assert_eq!(prefs.audio.fade_overlay_mode, FadeOverlayMode::AudioOnly);
        assert_eq!(prefs.system.max_playback_height, 1080);
        assert_eq!(prefs.audio_hardware.sample_rate, 48000);
        assert_eq!(prefs.appearance.ui_text_size, 13.0);
        assert_eq!(prefs.media_cache.max_cache_size_gb, 20);
        assert_eq!(prefs.color.working_color_space, klipp_core::ColorSpace::Rec709);
        assert!(prefs.title.default_font.is_empty());
        assert_eq!(prefs.timeline.clip_colors.len(), 6);
        assert_eq!(prefs.viewer.grid_color, [200, 200, 200, 80]);
        assert_eq!(prefs.advanced.toast_duration_secs, 2.2);
    }

    #[test]
    fn missing_fields_within_required_sections_get_serde_defaults() {
        // export section with empty object - custom_presets has no serde(default)
        // but default_output_dir and last_preset_id are Option so they default to null
        let json = r#"{
            "general": {
                "auto_save_interval_secs": 300,
                "recent_projects": []
            },
            "playback": {
                "scrub_audio": true,
                "jkl_audio_mute_threshold": 4.0
            },
            "export": {
                "custom_presets": []
            },
            "proxy": {
                "auto_generate_on_import": true,
                "proxy_resolution": "Half"
            }
        }"#;
        let prefs: Preferences = serde_json::from_str(json).expect("deserialize");
        // Fields with serde(default) within playback
        assert!(prefs.playback.skim_audio); // default_true
        assert!(!prefs.playback.loop_playback); // default false
        assert_eq!(prefs.playback.skim_resolution, 480); // default_skim_resolution
        // Fields with serde(default) within general
        assert!(prefs.general.remember_inspector_layout);
        assert!(prefs.general.collapsed_sections.is_empty());
    }

    // ── 3. Partial JSON: custom values in required sections, defaults elsewhere ──

    #[test]
    fn partial_json_custom_required_defaults_optional() {
        let json = r#"{
            "general": { "auto_save_interval_secs": 60, "recent_projects": [] },
            "playback": { "scrub_audio": false, "jkl_audio_mute_threshold": 2.0 },
            "export": { "custom_presets": [] },
            "proxy": { "auto_generate_on_import": false, "proxy_resolution": "Half" }
        }"#;
        let prefs: Preferences = serde_json::from_str(json).expect("deserialize partial");

        // Explicitly set values
        assert_eq!(prefs.general.auto_save_interval_secs, 60);
        assert!(!prefs.playback.scrub_audio);
        assert_eq!(prefs.playback.jkl_audio_mute_threshold, 2.0);
        assert!(!prefs.proxy.auto_generate_on_import);

        // Optional sections not in JSON get defaults
        assert!(prefs.editing.auto_create_tracks);
        assert_eq!(prefs.audio.fade_overlay_mode, FadeOverlayMode::AudioOnly);
        assert_eq!(prefs.system.gpu_preference, klipp_core::GpuPreference::Auto);
        assert_eq!(prefs.audio_hardware.buffer_size, 256);
        assert_eq!(prefs.appearance.accent_color, [0xe8, 0xa0, 0x20]);
        assert_eq!(prefs.media_cache.thumbnail_height, 256);
        assert_eq!(prefs.color.scope_update_interval_ms, 32);
        assert_eq!(prefs.title.default_size_pt, 48.0);
        assert_eq!(prefs.timeline.trim_handle_width, 14.0);
        assert_eq!(prefs.viewer.action_safe_pct, 0.9);
        assert_eq!(prefs.advanced.modal_backdrop_alpha, 120);
    }

    // ── 4. Custom values round-trip ──

    #[test]
    fn custom_values_round_trip() {
        let mut prefs = Preferences::default();

        // GeneralPrefs
        prefs.general.auto_save_interval_secs = 120;
        prefs.general.default_project_dir = Some(PathBuf::from("/home/user/projects"));
        prefs.general.remember_inspector_layout = false;
        prefs.general.collapsed_sections = vec!["Effects".into(), "Audio".into()];

        // PlaybackPrefs
        prefs.playback.scrub_audio = false;
        prefs.playback.skim_audio = false;
        prefs.playback.jkl_audio_mute_threshold = 8.0;
        prefs.playback.loop_playback = true;
        prefs.playback.skim_resolution = 720;

        // EditingPrefs
        prefs.editing.auto_create_tracks = false;
        prefs.editing.auto_generate_filmstrips = false;
        prefs.editing.auto_generate_skim = false;
        prefs.editing.default_still_duration_secs = 10.0;
        prefs.editing.undo_limit = 50;
        prefs.editing.default_transition_frames = 12;
        prefs.editing.default_video_track_height = 80.0;
        prefs.editing.default_audio_track_height = 60.0;
        prefs.editing.snap_threshold_px = 20.0;
        prefs.editing.zoom_step = 2.0;
        prefs.editing.backup_count = 10;

        // FileBrowserPrefs
        prefs.file_browser.file_browser_video_skim = false;
        prefs.file_browser.file_browser_audio_skim = false;
        prefs.file_browser.file_sort_ascending = false;
        prefs.file_browser.pinned_folders =
            vec![PathBuf::from("/home/u/Movies"), PathBuf::from("/data/footage")];

        // AudioPrefs
        prefs.audio.fade_overlay_mode = FadeOverlayMode::Both;
        prefs.audio.show_rubber_band = false;
        prefs.audio.show_waveform = false;
        prefs.audio.anti_click_fade = false;
        prefs.audio.show_crossfade_indicators = false;
        prefs.audio.pan_law = klipp_core::PanLaw::ConstantPower;
        prefs.audio.metering_ballistics = klipp_core::MeteringBallistics::Vu;
        prefs.audio.default_fade_curve = klipp_core::FadeCurve::Exponential;
        prefs.audio.default_fade_duration_ms = 50;
        prefs.audio.normalize_target_lufs = -23.0;

        // ExportPrefs
        prefs.export.default_output_dir = Some(PathBuf::from("/tmp/exports"));
        prefs.export.last_preset_id = Some("h264-fast".into());

        // ProxyPrefs
        prefs.proxy.auto_generate_on_import = false;
        prefs.proxy.proxy_resolution = ProxyResolution::Quarter;
        prefs.proxy.proxy_cache_dir = Some(PathBuf::from("/data/proxies"));

        // SystemPrefs
        prefs.system.gpu_preference = klipp_core::GpuPreference::LowPower;
        prefs.system.decode_threads = 4;
        prefs.system.max_playback_height = 2160;
        prefs.system.frame_queue_size = 32;

        // AudioHardwarePrefs
        prefs.audio_hardware.output_device = Some("HDMI Audio".into());
        prefs.audio_hardware.sample_rate = 44100;
        prefs.audio_hardware.buffer_size = 1024;

        // AppearancePrefs
        prefs.appearance.ui_text_size = 16.0;
        prefs.appearance.accent_color = [255, 0, 128];
        prefs.appearance.playhead_color = [0, 255, 0];
        prefs.appearance.role_video = [10, 20, 30];
        prefs.appearance.role_dialogue = [40, 50, 60];
        prefs.appearance.role_music = [70, 80, 90];
        prefs.appearance.role_sfx = [100, 110, 120];
        prefs.appearance.role_title = [130, 140, 150];
        prefs.appearance.role_broll = [160, 170, 180];
        prefs.appearance.role_ambience = [190, 200, 210];
        prefs.appearance.role_foley = [220, 230, 240];
        prefs.appearance.role_voiceover = [11, 22, 33];
        prefs.appearance.role_graphics = [44, 55, 66];

        // MediaCachePrefs
        prefs.media_cache.filmstrip_cache_dir = Some(PathBuf::from("/data/filmstrips"));
        prefs.media_cache.audio_cache_dir = Some(PathBuf::from("/data/audio-cache"));
        prefs.media_cache.max_cache_size_gb = 40;
        prefs.media_cache.skim_transcode_quality = 28;
        prefs.media_cache.thumbnail_height = 200;

        // ColorPrefs
        prefs.color.working_color_space = klipp_core::ColorSpace::Rec2020;
        prefs.color.viewer_lut_path = Some(PathBuf::from("/luts/my.cube"));
        prefs.color.scope_quality = klipp_core::ScopeQuality::High;
        prefs.color.scope_update_interval_ms = 64;

        // TitlePrefs
        prefs.title.default_font = "Inter".into();
        prefs.title.default_size_pt = 72.0;
        prefs.title.font_search_paths = vec![
            PathBuf::from("/usr/share/fonts"),
            PathBuf::from("/home/user/.fonts"),
        ];

        // TimelinePrefs
        prefs.timeline.clip_colors = vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]];
        prefs.timeline.clip_gap_px = 5.0;
        prefs.timeline.trim_handle_width = 20.0;
        prefs.timeline.ruler_height = 32.0;
        prefs.timeline.clip_corner_radius = 8.0;
        prefs.timeline.overview_bar_height = 30.0;
        prefs.timeline.min_track_height = 16.0;
        prefs.timeline.max_track_height = 300.0;
        prefs.timeline.text_track_height = 48.0;

        // ViewerPrefs
        prefs.viewer.action_safe_pct = 0.85;
        prefs.viewer.title_safe_pct = 0.75;
        prefs.viewer.grid_color = [255, 128, 64, 200];
        prefs.viewer.checkerboard_tile_size = 24.0;
        prefs.viewer.transform_handle_size = 12.0;
        prefs.viewer.fullscreen_overlay_timeout_secs = 5.0;

        // AdvancedPrefs
        prefs.advanced.toast_duration_secs = 4.0;
        prefs.advanced.modal_backdrop_alpha = 200;
        prefs.advanced.perf_overlay_interval_ms = 250;
        prefs.advanced.anti_click_fade_ms = 5.0;
        prefs.advanced.pre_roll_secs = 3.0;

        let rt = round_trip(&prefs);

        // Verify every custom value survived
        assert_eq!(rt.general.auto_save_interval_secs, 120);
        assert_eq!(rt.general.default_project_dir.as_ref().unwrap().to_str().unwrap(), "/home/user/projects");
        assert!(!rt.general.remember_inspector_layout);
        assert_eq!(rt.general.collapsed_sections, vec!["Effects", "Audio"]);

        assert!(!rt.playback.scrub_audio);
        assert!(!rt.playback.skim_audio);
        assert_eq!(rt.playback.jkl_audio_mute_threshold, 8.0);
        assert!(rt.playback.loop_playback);
        assert_eq!(rt.playback.skim_resolution, 720);

        assert!(!rt.editing.auto_create_tracks);
        assert!(!rt.editing.auto_generate_filmstrips);
        assert!(!rt.editing.auto_generate_skim);
        assert_eq!(rt.editing.default_still_duration_secs, 10.0);
        assert_eq!(rt.editing.undo_limit, 50);
        assert_eq!(rt.editing.default_transition_frames, 12);
        assert_eq!(rt.editing.default_video_track_height, 80.0);
        assert_eq!(rt.editing.default_audio_track_height, 60.0);
        assert_eq!(rt.editing.snap_threshold_px, 20.0);
        assert_eq!(rt.editing.zoom_step, 2.0);
        assert_eq!(rt.editing.backup_count, 10);

        // FileBrowserPrefs round-trip
        assert!(!rt.file_browser.file_browser_video_skim);
        assert!(!rt.file_browser.file_browser_audio_skim);
        assert!(!rt.file_browser.file_sort_ascending);
        assert_eq!(rt.file_browser.pinned_folders.len(), 2);
        assert_eq!(
            rt.file_browser.pinned_folders[0].to_str().unwrap(),
            "/home/u/Movies"
        );

        assert_eq!(rt.audio.fade_overlay_mode, FadeOverlayMode::Both);
        assert!(!rt.audio.show_rubber_band);
        assert!(!rt.audio.show_waveform);
        assert!(!rt.audio.anti_click_fade);
        assert!(!rt.audio.show_crossfade_indicators);
        assert_eq!(rt.audio.pan_law, klipp_core::PanLaw::ConstantPower);
        assert_eq!(rt.audio.metering_ballistics, klipp_core::MeteringBallistics::Vu);
        assert_eq!(rt.audio.default_fade_curve, klipp_core::FadeCurve::Exponential);
        assert_eq!(rt.audio.default_fade_duration_ms, 50);
        assert_eq!(rt.audio.normalize_target_lufs, -23.0);

        assert_eq!(rt.export.default_output_dir.as_ref().unwrap().to_str().unwrap(), "/tmp/exports");
        assert_eq!(rt.export.last_preset_id.as_deref(), Some("h264-fast"));

        assert!(!rt.proxy.auto_generate_on_import);
        assert_eq!(rt.proxy.proxy_resolution, ProxyResolution::Quarter);
        assert_eq!(rt.proxy.proxy_cache_dir.as_ref().unwrap().to_str().unwrap(), "/data/proxies");

        assert_eq!(rt.system.gpu_preference, klipp_core::GpuPreference::LowPower);
        assert_eq!(rt.system.decode_threads, 4);
        assert_eq!(rt.system.max_playback_height, 2160);
        assert_eq!(rt.system.frame_queue_size, 32);

        assert_eq!(rt.audio_hardware.output_device.as_deref(), Some("HDMI Audio"));
        assert_eq!(rt.audio_hardware.sample_rate, 44100);
        assert_eq!(rt.audio_hardware.buffer_size, 1024);

        assert_eq!(rt.appearance.ui_text_size, 16.0);
        assert_eq!(rt.appearance.accent_color, [255, 0, 128]);
        assert_eq!(rt.appearance.playhead_color, [0, 255, 0]);
        assert_eq!(rt.appearance.role_video, [10, 20, 30]);
        assert_eq!(rt.appearance.role_dialogue, [40, 50, 60]);
        assert_eq!(rt.appearance.role_music, [70, 80, 90]);
        assert_eq!(rt.appearance.role_sfx, [100, 110, 120]);
        assert_eq!(rt.appearance.role_title, [130, 140, 150]);
        assert_eq!(rt.appearance.role_broll, [160, 170, 180]);
        assert_eq!(rt.appearance.role_ambience, [190, 200, 210]);
        assert_eq!(rt.appearance.role_foley, [220, 230, 240]);
        assert_eq!(rt.appearance.role_voiceover, [11, 22, 33]);
        assert_eq!(rt.appearance.role_graphics, [44, 55, 66]);

        assert_eq!(rt.media_cache.filmstrip_cache_dir.as_ref().unwrap().to_str().unwrap(), "/data/filmstrips");
        assert_eq!(rt.media_cache.audio_cache_dir.as_ref().unwrap().to_str().unwrap(), "/data/audio-cache");
        assert_eq!(rt.media_cache.max_cache_size_gb, 40);
        assert_eq!(rt.media_cache.skim_transcode_quality, 28);
        assert_eq!(rt.media_cache.thumbnail_height, 200);

        assert_eq!(rt.color.working_color_space, klipp_core::ColorSpace::Rec2020);
        assert_eq!(rt.color.viewer_lut_path.as_ref().unwrap().to_str().unwrap(), "/luts/my.cube");
        assert_eq!(rt.color.scope_quality, klipp_core::ScopeQuality::High);
        assert_eq!(rt.color.scope_update_interval_ms, 64);

        assert_eq!(rt.title.default_font, "Inter");
        assert_eq!(rt.title.default_size_pt, 72.0);
        assert_eq!(rt.title.font_search_paths.len(), 2);
        assert_eq!(rt.title.font_search_paths[0].to_str().unwrap(), "/usr/share/fonts");
        assert_eq!(rt.title.font_search_paths[1].to_str().unwrap(), "/home/user/.fonts");

        assert_eq!(rt.timeline.clip_colors, vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]]);
        assert_eq!(rt.timeline.clip_gap_px, 5.0);
        assert_eq!(rt.timeline.trim_handle_width, 20.0);
        assert_eq!(rt.timeline.ruler_height, 32.0);
        assert_eq!(rt.timeline.clip_corner_radius, 8.0);
        assert_eq!(rt.timeline.overview_bar_height, 30.0);
        assert_eq!(rt.timeline.min_track_height, 16.0);
        assert_eq!(rt.timeline.max_track_height, 300.0);
        assert_eq!(rt.timeline.text_track_height, 48.0);

        assert_eq!(rt.viewer.action_safe_pct, 0.85);
        assert_eq!(rt.viewer.title_safe_pct, 0.75);
        assert_eq!(rt.viewer.grid_color, [255, 128, 64, 200]);
        assert_eq!(rt.viewer.checkerboard_tile_size, 24.0);
        assert_eq!(rt.viewer.transform_handle_size, 12.0);
        assert_eq!(rt.viewer.fullscreen_overlay_timeout_secs, 5.0);

        assert_eq!(rt.advanced.toast_duration_secs, 4.0);
        assert_eq!(rt.advanced.modal_backdrop_alpha, 200);
        assert_eq!(rt.advanced.perf_overlay_interval_ms, 250);
        assert_eq!(rt.advanced.anti_click_fade_ms, 5.0);
        assert_eq!(rt.advanced.pre_roll_secs, 3.0);
    }

    // ── 5. New fields compatibility (old-format JSON missing new sections) ──

    #[test]
    fn old_format_json_gets_new_field_defaults() {
        // Simulate a preferences file from before the new sections were added.
        // Only contains general, playback, export, proxy (the original required fields).
        let json = r#"{
            "general": {
                "auto_save_interval_secs": 180,
                "default_project_dir": null,
                "recent_projects": [],
                "remember_inspector_layout": true,
                "collapsed_sections": []
            },
            "playback": {
                "scrub_audio": false,
                "jkl_audio_mute_threshold": 3.0
            },
            "export": {
                "custom_presets": []
            },
            "proxy": {
                "auto_generate_on_import": true,
                "proxy_resolution": "W1280"
            }
        }"#;
        let prefs: Preferences = serde_json::from_str(json).expect("deserialize old format");

        // Old fields preserved
        assert_eq!(prefs.general.auto_save_interval_secs, 180);
        assert!(!prefs.playback.scrub_audio);
        assert_eq!(prefs.playback.jkl_audio_mute_threshold, 3.0);
        assert_eq!(prefs.proxy.proxy_resolution, ProxyResolution::W1280);

        // New sections get defaults
        assert_eq!(prefs.system.gpu_preference, klipp_core::GpuPreference::Auto);
        assert_eq!(prefs.system.frame_queue_size, 16);
        assert_eq!(prefs.audio_hardware.sample_rate, 48000);
        assert_eq!(prefs.appearance.ui_text_size, 13.0);
        assert_eq!(prefs.appearance.accent_color, [0xe8, 0xa0, 0x20]);
        assert_eq!(prefs.media_cache.max_cache_size_gb, 20);
        assert_eq!(prefs.color.working_color_space, klipp_core::ColorSpace::Rec709);
        assert!(prefs.title.default_font.is_empty());
        assert_eq!(prefs.timeline.clip_colors.len(), 6);
        assert_eq!(prefs.viewer.grid_color, [200, 200, 200, 80]);
        assert_eq!(prefs.advanced.toast_duration_secs, 2.2);

        // Editing and audio sections (also serde(default)) get defaults
        assert!(prefs.editing.auto_create_tracks);
        assert_eq!(prefs.audio.fade_overlay_mode, FadeOverlayMode::AudioOnly);
    }

    // ── 6. Color values: [u8;3] and [u8;4] round-trip ──

    #[test]
    fn color_arrays_round_trip() {
        let mut prefs = Preferences::default();

        // Edge cases: all zeros, all max, specific values
        prefs.appearance.accent_color = [0, 0, 0];
        prefs.appearance.playhead_color = [255, 255, 255];
        prefs.appearance.role_video = [1, 128, 254];
        prefs.viewer.grid_color = [0, 0, 0, 0]; // fully transparent

        let rt = round_trip(&prefs);
        assert_eq!(rt.appearance.accent_color, [0, 0, 0]);
        assert_eq!(rt.appearance.playhead_color, [255, 255, 255]);
        assert_eq!(rt.appearance.role_video, [1, 128, 254]);
        assert_eq!(rt.viewer.grid_color, [0, 0, 0, 0]);

        // Also verify the JSON representation is a plain array
        let json = serde_json::to_value(&prefs).expect("to value");
        let accent = &json["appearance"]["accent_color"];
        assert!(accent.is_array());
        assert_eq!(accent[0], 0);
        assert_eq!(accent[1], 0);
        assert_eq!(accent[2], 0);

        let grid = &json["viewer"]["grid_color"];
        assert!(grid.is_array());
        assert_eq!(grid.as_array().unwrap().len(), 4);
    }

    #[test]
    fn color_array_max_values() {
        let mut prefs = Preferences::default();
        prefs.appearance.accent_color = [255, 255, 255];
        prefs.viewer.grid_color = [255, 255, 255, 255];

        let rt = round_trip(&prefs);
        assert_eq!(rt.appearance.accent_color, [255, 255, 255]);
        assert_eq!(rt.viewer.grid_color, [255, 255, 255, 255]);
    }

    // ── 6.5. Migration: legacy editing.file_* → file_browser.* ──

    /// When the file-browser settings were extracted from `EditingPrefs`
    /// into their own struct, prior versions had stored these fields
    /// inside `editing`. `Preferences::load` runs a migration shim
    /// (`migrate_legacy_editing_to_file_browser`) that moves the values
    /// before serde deserialization. These tests cover the shim directly
    /// against synthetic JSON so the migration path doesn't bit-rot.
    #[test]
    fn migrate_moves_legacy_editing_file_fields_to_file_browser() {
        let raw = r#"{
            "general": {"auto_save_interval_secs": 300, "recent_projects": []},
            "playback": {"scrub_audio": true, "jkl_audio_mute_threshold": 4.0},
            "export": {"custom_presets": []},
            "proxy": {"auto_generate_on_import": false, "proxy_resolution": "Half"},
            "editing": {
                "file_thumb_size": "Large",
                "file_sort_ascending": false,
                "pinned_folders": ["/home/u/Movies"],
                "file_browser_video_skim": false,
                "file_browser_audio_skim": false
            }
        }"#;
        let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
        Preferences::migrate_legacy_editing_to_file_browser(&mut value);

        // Legacy fields are gone from editing.
        let editing = value.get("editing").and_then(|v| v.as_object()).unwrap();
        assert!(!editing.contains_key("file_thumb_size"));
        assert!(!editing.contains_key("file_sort_ascending"));
        assert!(!editing.contains_key("pinned_folders"));
        assert!(!editing.contains_key("file_browser_video_skim"));
        assert!(!editing.contains_key("file_browser_audio_skim"));

        // file_browser section was created and populated.
        let fb = value.get("file_browser").and_then(|v| v.as_object()).unwrap();
        assert_eq!(fb.get("file_thumb_size").and_then(|v| v.as_str()), Some("Large"));
        assert_eq!(fb.get("file_sort_ascending").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            fb.get("file_browser_video_skim").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            fb.get("file_browser_audio_skim").and_then(|v| v.as_bool()),
            Some(false)
        );

        // Roundtrip the migrated Value through Preferences to confirm it
        // deserializes cleanly with the migrated values.
        let prefs: Preferences = serde_json::from_value(value).unwrap();
        assert!(!prefs.file_browser.file_browser_video_skim);
        assert!(!prefs.file_browser.file_browser_audio_skim);
        assert!(!prefs.file_browser.file_sort_ascending);
        assert_eq!(prefs.file_browser.pinned_folders.len(), 1);
    }

    #[test]
    fn migrate_prefers_new_location_when_both_present() {
        // If a user has both old and new entries (e.g. they manually edited
        // the JSON), the new file_browser.* value must win — never have a
        // ghost legacy value silently override an explicit modern setting.
        let raw = r#"{
            "general": {"auto_save_interval_secs": 300, "recent_projects": []},
            "playback": {"scrub_audio": true, "jkl_audio_mute_threshold": 4.0},
            "export": {"custom_presets": []},
            "proxy": {"auto_generate_on_import": false, "proxy_resolution": "Half"},
            "editing": {
                "file_browser_video_skim": false
            },
            "file_browser": {
                "file_browser_video_skim": true
            }
        }"#;
        let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
        Preferences::migrate_legacy_editing_to_file_browser(&mut value);

        let fb = value.get("file_browser").and_then(|v| v.as_object()).unwrap();
        assert_eq!(
            fb.get("file_browser_video_skim").and_then(|v| v.as_bool()),
            Some(true),
            "explicit new value must win over legacy editing.* value"
        );
    }

    #[test]
    fn migrate_is_noop_when_no_legacy_fields_present() {
        // Modern JSON with only file_browser.* fields should pass through
        // unchanged — nothing copied, nothing removed.
        let raw = r#"{
            "general": {"auto_save_interval_secs": 300, "recent_projects": []},
            "playback": {"scrub_audio": true, "jkl_audio_mute_threshold": 4.0},
            "export": {"custom_presets": []},
            "proxy": {"auto_generate_on_import": false, "proxy_resolution": "Half"},
            "file_browser": {
                "file_browser_video_skim": false
            }
        }"#;
        let mut value: serde_json::Value = serde_json::from_str(raw).unwrap();
        let before = value.clone();
        Preferences::migrate_legacy_editing_to_file_browser(&mut value);
        assert_eq!(value, before, "migration must be a no-op for modern JSON");
    }

    // ── 7. Vec fields: clip_colors and font_search_paths round-trip ──

    #[test]
    fn vec_clip_colors_round_trip() {
        let mut prefs = Preferences::default();

        // Empty vec
        prefs.timeline.clip_colors = vec![];
        let rt = round_trip(&prefs);
        assert!(rt.timeline.clip_colors.is_empty());

        // Single element
        prefs.timeline.clip_colors = vec![[42, 43, 44]];
        let rt = round_trip(&prefs);
        assert_eq!(rt.timeline.clip_colors, vec![[42, 43, 44]]);

        // Many elements
        prefs.timeline.clip_colors = (0..20).map(|i| [i, i * 2, i * 3]).collect();
        let rt = round_trip(&prefs);
        assert_eq!(rt.timeline.clip_colors.len(), 20);
        assert_eq!(rt.timeline.clip_colors[0], [0, 0, 0]);
        assert_eq!(rt.timeline.clip_colors[19], [19, 38, 57]);
    }

    #[test]
    fn vec_font_search_paths_round_trip() {
        let mut prefs = Preferences::default();

        // Empty
        prefs.title.font_search_paths = vec![];
        let rt = round_trip(&prefs);
        assert!(rt.title.font_search_paths.is_empty());

        // Multiple paths including ones with special characters
        prefs.title.font_search_paths = vec![
            PathBuf::from("/usr/share/fonts"),
            PathBuf::from("/home/user/.local/share/fonts"),
            PathBuf::from("/mnt/data/My Fonts"),
        ];
        let rt = round_trip(&prefs);
        assert_eq!(rt.title.font_search_paths.len(), 3);
        assert_eq!(rt.title.font_search_paths[0], PathBuf::from("/usr/share/fonts"));
        assert_eq!(rt.title.font_search_paths[2], PathBuf::from("/mnt/data/My Fonts"));
    }

    #[test]
    fn vec_collapsed_sections_round_trip() {
        let mut prefs = Preferences::default();
        prefs.general.collapsed_sections = vec![
            "Transform".into(),
            "Effects".into(),
            "Color".into(),
        ];
        let rt = round_trip(&prefs);
        assert_eq!(rt.general.collapsed_sections, vec!["Transform", "Effects", "Color"]);
    }

    #[test]
    fn vec_recent_projects_round_trip() {
        let mut prefs = Preferences::default();
        prefs.general.recent_projects = vec![
            RecentProject {
                path: PathBuf::from("/projects/a.klipp"),
                name: "Project A".into(),
                last_saved: Some("2026-03-31".into()),
                duration_secs: Some(120.5),
                fps: Some("24fps".into()),
                resolution: Some("1920x1080".into()),
                thumbnail_path: Some(PathBuf::from("/tmp/thumb.png")),
                total_size_bytes: Some(1_000_000),
                clip_count: Some(5),
                track_count: Some(3),
                missing_media: None,
            },
            RecentProject::from_path(PathBuf::from("/projects/b.klipp")),
        ];
        let rt = round_trip(&prefs);
        assert_eq!(rt.general.recent_projects.len(), 2);
        assert_eq!(rt.general.recent_projects[0].name, "Project A");
        assert_eq!(rt.general.recent_projects[0].duration_secs, Some(120.5));
        assert_eq!(rt.general.recent_projects[0].total_size_bytes, Some(1_000_000));
        assert_eq!(rt.general.recent_projects[1].name, "b");
        assert!(rt.general.recent_projects[1].duration_secs.is_none());
    }

    // ── Bonus: recent_projects backward compat with old Vec<PathBuf> format ──

    #[test]
    fn recent_projects_old_pathbuf_format() {
        let json = r#"{
            "general": {
                "auto_save_interval_secs": 300,
                "recent_projects": ["/projects/old1.klipp", "/projects/old2.klipp"]
            },
            "playback": { "scrub_audio": true, "jkl_audio_mute_threshold": 4.0 },
            "export": { "custom_presets": [] },
            "proxy": { "auto_generate_on_import": true, "proxy_resolution": "Half" }
        }"#;
        let prefs: Preferences = serde_json::from_str(json).expect("deserialize old recent format");
        assert_eq!(prefs.general.recent_projects.len(), 2);
        assert_eq!(prefs.general.recent_projects[0].path, PathBuf::from("/projects/old1.klipp"));
        assert_eq!(prefs.general.recent_projects[0].name, "old1");
        assert!(prefs.general.recent_projects[0].duration_secs.is_none());
    }
}
