use klipp_core::{ExportPreset, ProxyResolution};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_true() -> bool { true }
fn default_fade_ms() -> u32 { 500 }

/// The preferences file's revision. A file below it has the one-time
/// migrations of `Preferences::migrate_to_revision` run on load; a file
/// at it is read as written, so a value chosen after the migration stays.
pub const PREFS_REVISION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    /// See [`PREFS_REVISION`]. Absent in a file written before it: 0.
    #[serde(default)]
    pub revision: u32,
    pub general: GeneralPrefs,
    pub playback: PlaybackPrefs,
    #[serde(default)]
    pub editing: EditingPrefs,
    #[serde(default)]
    pub file_browser: FileBrowserPrefs,
    #[serde(default)]
    pub effects_browser: EffectsBrowserPrefs,
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
    #[serde(default)]
    pub external_tools: ExternalToolsPrefs,
    #[serde(default)]
    pub ai: AiPrefs,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            revision: PREFS_REVISION,
            general: Default::default(),
            playback: Default::default(),
            editing: Default::default(),
            file_browser: Default::default(),
            effects_browser: Default::default(),
            audio: Default::default(),
            export: Default::default(),
            proxy: Default::default(),
            system: Default::default(),
            audio_hardware: Default::default(),
            appearance: Default::default(),
            media_cache: Default::default(),
            color: Default::default(),
            title: Default::default(),
            timeline: Default::default(),
            viewer: Default::default(),
            advanced: Default::default(),
            external_tools: Default::default(),
            ai: Default::default(),
        }
    }
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
    /// How many sequences the project holds.
    #[serde(default)]
    pub sequence_count: Option<u32>,
    /// Kept at the top of the recent list, ahead of the newer ones.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

impl RecentProject {
    pub(crate) fn control_summary(&self) -> klipp_core::RecentProjectSummary {
        klipp_core::RecentProjectSummary { path: self.path.clone(), name: self.name.clone(), last_saved: self.last_saved.clone(), duration_secs: self.duration_secs, fps: self.fps.clone(), resolution: self.resolution.clone(), missing_media: self.missing_media }
    }

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
            sequence_count: None,
            pinned: false,
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
        return Ok(without_autosaves(projects));
    }
    // Fall back to old Vec<PathBuf> format
    if let Ok(paths) = serde_json::from_value::<Vec<PathBuf>>(value) {
        return Ok(without_autosaves(paths.into_iter().map(RecentProject::from_path).collect()));
    }
    // If neither works, return empty
    Ok(Vec::new())
}

/// The recent list with each auto-save replaced by the project it belongs
/// to, or dropped when that project is already listed.
///
/// Restoring an auto-save used to put its folder in the list, and every
/// reader of the list (the project browser, Open Recent, reopening the
/// last project at startup) then opened the auto-save as if it were the
/// project. Opening the project instead offers the auto-save to restore.
fn without_autosaves(projects: Vec<RecentProject>) -> Vec<RecentProject> {
    let mut out: Vec<RecentProject> = Vec::with_capacity(projects.len());
    for mut rp in projects {
        if crate::app::is_autosave_path(&rp.path) {
            let Some(main) = crate::app::project_for_autosave(&rp.path) else { continue };
            rp.name = main.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(rp.name);
            rp.path = main;
        }
        if !out.iter().any(|p| p.path == rp.path) {
            out.push(rp);
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralPrefs {
    /// Whether the Welcome window shows the next time a project is open in
    /// the editor (#2933). A fresh profile has it on, and it goes off once
    /// shown, unless the window's own checkbox turns it back on. A settings
    /// file from before it has none, and an existing user is not welcomed
    /// again: it reads as off.
    #[serde(default)]
    pub show_welcome: bool,
    /// Whether the window opens maximised (#2944): a fresh profile's first
    /// start does, and from then on as it was left. A settings file from
    /// before it reads as no, so nobody's window changes.
    #[serde(default)]
    pub open_maximized: bool,
    /// The project format New Project last made a project from, by name
    /// (#3307): it opens on that one.
    #[serde(default)]
    pub last_project_format: Option<String>,
    /// Explicit opt-in to background release checks. The original setting
    /// defaulted on, so its serialized value cannot establish user consent.
    #[serde(default, rename = "automatic_update_checks_opt_in")]
    pub check_for_updates: bool,
    /// Whether the user has answered whether to check automatically: by
    /// the question Klipp asks, a switch, or leaving the question
    /// unanswered too often. `check_for_updates` false is ambiguous in
    /// older prefs files (it was always written), so the answer is kept
    /// apart; true there was always the user's own.
    #[serde(default)]
    pub update_checks_answered: bool,
    /// Starts the question was shown at and left unanswered.
    #[serde(default)]
    pub update_checks_asked: u32,
    /// Whether the update check offers preview releases, when the user
    /// has said; unset, a preview build offers them and a stable one
    /// doesn't (`updates::include_previews`). A key of its own: the
    /// earlier `preview_updates` was always written, `false` when never
    /// touched, so it cannot tell a choice from the default.
    #[serde(default, rename = "include_preview_releases")]
    pub preview_updates: Option<bool>,
    #[serde(default)]
    pub last_update_check: u64,
    #[serde(default)]
    pub dismissed_update: Option<String>,
    /// Auto-save interval in seconds. 0 = disabled. Default 300 (5 min).
    pub auto_save_interval_secs: u32,
    /// Also auto-save every N editing operations. 0 = disabled. Default 25.
    #[serde(default = "default_auto_save_ops")]
    pub auto_save_op_threshold: u32,
    pub default_project_dir: Option<PathBuf>,
    /// The frame rate last chosen for a welcome-screen format, which the
    /// formats offering it start at (#1864).
    #[serde(default)]
    pub last_format_rate: Option<(u32, u32)>,
    /// Most-recent first, max 20 entries.
    #[serde(deserialize_with = "deserialize_recent_projects")]
    pub recent_projects: Vec<RecentProject>,
    /// Remember which inspector sections are collapsed across restarts.
    #[serde(default = "default_true")]
    pub remember_inspector_layout: bool,
    /// Whether the text editors underline misspelt words (#3236). On
    /// unless turned off.
    #[serde(default = "default_true")]
    pub check_spelling: bool,
    /// Names of inspector sections that are collapsed (persisted when remember_inspector_layout is true).
    #[serde(default)]
    pub collapsed_sections: Vec<String>,
    /// What to show when Klipp starts.
    #[serde(default)]
    pub startup_behavior: StartupBehavior,
    /// Phase 5 — string keys of one-shot tips that have already been
    /// shown and dismissed (e.g. `"track_matte_intro"`). Persisted so
    /// the same tip never re-fires across sessions. Use a string set
    /// rather than per-tip bool fields so future tips don't churn the
    /// schema.
    #[serde(default)]
    pub dismissed_tips: std::collections::HashSet<String>,
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
    /// Whether playback is heard at the loudness the export will deliver,
    /// when the export or the sequence's brief sets one (#1565).
    #[serde(default = "default_true")]
    pub hear_delivery_loudness: bool,
    /// Resolution (max height in pixels) for skim frame decodes. Higher = sharper but slower.
    #[serde(default = "default_skim_resolution")]
    pub skim_resolution: u32,
    /// Playback quality preset.
    #[serde(default)]
    pub playback_quality: klipp_core::PlaybackQuality,
    /// Whether a note suggests a fix when playback keeps dropping frames
    /// (#3234).
    #[serde(default = "default_true")]
    pub suggest_playback_fixes: bool,
    /// The note last shown for each project, by its folder: once per
    /// project, again only for another fix or clearly worse (#3234).
    #[serde(default)]
    pub playback_notes_shown: std::collections::BTreeMap<String, PlaybackNoteShown>,
    /// Draw the heavy parts of the timeline ahead while idle, on mains
    /// power (#3212).
    #[serde(default = "default_true")]
    pub background_render: bool,
    /// Seconds with nothing done before drawing ahead starts.
    #[serde(default = "default_background_render_delay")]
    pub background_render_delay_secs: f32,
    /// How aggressively background work (skim transcode, filmstrip
    /// generation, audio conform) yields when the user is playing /
    /// scrubbing / interacting. The default is the "editor stays
    /// responsive" mode.
    #[serde(default)]
    pub background_performance: BackgroundPerformance,
    /// Whether the playback decoder substitutes the lower-res skim
    /// file for source media that would otherwise stutter at full
    /// resolution (4K60 etc.). Mirrors DaVinci's "Optimized Media"
    /// and FCP's "Proxy" toggle. Auto = swap when source > 1080p OR
    /// > 30 fps AND skim is on disk; AlwaysOriginal = never swap;
    /// > AlwaysOptimized = swap whenever a skim exists.
    #[serde(default)]
    pub optimised_playback: OptimisedPlayback,
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

fn default_background_render_delay() -> f32 {
    klipp_engine::background_render::DEFAULT_DELAY.as_secs_f32()
}

fn default_skim_resolution() -> u32 {
    // What skimming has always actually delivered. The cap used to be
    // `pref.max(source.min(1080))` — a floor, not a cap — so the preference
    // never lowered anything and every skim frame came back at the source
    // height capped to 1080. #483 made the cap real, which would have
    // quietly halved the preview at the old default of 720, because the
    // reducer halves rather than scales: 1080 asked down to 720 gives 540.
    // The default therefore matches the picture that has been in use, and
    // the knob is there for anyone who wants a cheaper decode.
    1080
}

/// How aggressively background jobs (skim transcode, filmstrip,
/// audio conform) yield to foreground activity. Maps 1:1 onto
/// [`klipp_jobs::SchedulerPolicy`]; kept as a separate type so the
/// pref serialisation doesn't depend on the jobs crate's exact
/// variant names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BackgroundPerformance {
    /// Pause background work whenever the user plays / scrubs /
    /// interacts. Default — keeps the editor instantly responsive.
    #[default]
    Defer,
    /// Same as Defer for now (placeholder for a future "throttle but
    /// don't fully pause" mode that uses partial chunks).
    Balanced,
    /// Never pause background work. Use when you want the fastest
    /// possible cache build and don't mind a less-responsive editor.
    MaxThroughput,
}

impl BackgroundPerformance {
    pub fn to_scheduler_policy(self) -> klipp_jobs::SchedulerPolicy {
        match self {
            BackgroundPerformance::Defer => klipp_jobs::SchedulerPolicy::Defer,
            BackgroundPerformance::Balanced => klipp_jobs::SchedulerPolicy::Balanced,
            BackgroundPerformance::MaxThroughput => klipp_jobs::SchedulerPolicy::MaxThroughput,
        }
    }
}

/// User-facing optimised-playback mode. Maps to the engine's
/// `set_optimised_playback_mode(u8)` — kept as its own enum so the
/// pref serialisation doesn't depend on engine internal numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OptimisedPlayback {
    /// Use skim file when source > 1080p OR > 30 fps AND a skim is
    /// available. The most common "just works" choice.
    #[default]
    Auto,
    /// Always play the original source file, even when it stutters.
    /// Pick this when you specifically need to inspect 4K detail.
    AlwaysOriginal,
    /// Always use the skim file when one exists, even for clips
    /// that would have played fine from source. Lowest-spec / battery-
    /// saver choice.
    AlwaysOptimized,
}

/// Playback Quality as the engine's divisor of the viewer size.
pub fn playback_quality_divisor(q: klipp_core::PlaybackQuality) -> u8 {
    match q {
        klipp_core::PlaybackQuality::Full => 1,
        klipp_core::PlaybackQuality::Half => 2,
        klipp_core::PlaybackQuality::Quarter => 4,
    }
}

impl OptimisedPlayback {
    /// Map onto the engine-side u8 atomic. Kept as a method so the
    /// numbering can shift without touching every call site.
    pub fn to_engine_mode(self) -> u8 {
        match self {
            OptimisedPlayback::Auto => 0,
            OptimisedPlayback::AlwaysOriginal => 1,
            OptimisedPlayback::AlwaysOptimized => 2,
        }
    }
}

/// Default multicam sync method for aligning angles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MulticamSyncMethod {
    /// Timecode where a clip has it, the sound for the rest. (Named
    /// `Manual` in files written before it acted.)
    #[default]
    Manual,
    Timecode,
    AudioWaveform,
}

impl MulticamSyncMethod {
    /// How a multicam made by Create Synced Multicam syncs its angles.
    pub fn sync(self) -> klipp_core::MulticamSync {
        match self {
            Self::Manual => klipp_core::MulticamSync::TimecodeThenAudio,
            Self::Timecode => klipp_core::MulticamSync::Timecode,
            Self::AudioWaveform => klipp_core::MulticamSync::Audio,
        }
    }
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
    /// Mark where one audio clip fades down into the next fading up.
    ///
    /// The field name says crossfade and is kept so saved preferences
    /// keep loading, but a crossfade needs the two to overlap and the
    /// timeline has no audio transition to make that with. What is
    /// marked is the fade handoff — the dip to silence and back — which
    /// is real and otherwise invisible. It used to mark every butted
    /// pair whether or not either clip faded at all.
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
    /// How long a fade is when one is added (a clip's Add Fade In or
    /// Add Fade Out), in milliseconds.
    #[serde(default = "default_fade_ms")]
    pub default_fade_duration_ms: u32,
    /// Loudness standard for metering/normalization.
    #[serde(default)]
    pub loudness_standard: klipp_core::LoudnessStandard,
    /// Peak hold time in seconds for audio meters.
    #[serde(default = "default_peak_hold")]
    pub peak_hold_time_secs: f32,
    /// Auto-duck default: duck target level (0.0 = silence, 1.0 = unchanged).
    #[serde(default = "default_duck_level")]
    pub duck_level: f32,
    /// Auto-duck default: attack time in seconds.
    #[serde(default = "default_duck_attack")]
    pub duck_attack_secs: f32,
    /// Auto-duck default: release time in seconds.
    #[serde(default = "default_duck_release")]
    pub duck_release_secs: f32,
    /// Auto-duck default: sidechain threshold (0..1).
    #[serde(default = "default_duck_threshold")]
    pub duck_threshold: f32,
    /// Use 4× polyphase FIR upsampling for true-peak metering
    /// (BS.1770-4 Annex 2). Disable for a small CPU saving at the
    /// cost of under-reading inter-sample peaks by up to ~3 dB.
    #[serde(default = "default_true")]
    pub tp_oversample: bool,
    /// The Normalize dialog's last mode, target, ceiling and grouping
    /// (#1843), offered the next time it opens.
    #[serde(default)]
    pub normalize: klipp_core::normalize::NormalizeSettings,
}

fn default_peak_hold() -> f32 { 3.0 }
fn default_duck_level() -> f32 { 0.2 }
fn default_duck_attack() -> f32 { 0.1 }
fn default_duck_release() -> f32 { 0.3 }
fn default_duck_threshold() -> f32 { 0.1 }

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
            default_fade_duration_ms: default_fade_ms(),
            loudness_standard: Default::default(),
            peak_hold_time_secs: default_peak_hold(),
            duck_level: default_duck_level(),
            duck_attack_secs: default_duck_attack(),
            duck_release_secs: default_duck_release(),
            duck_threshold: default_duck_threshold(),
            tp_oversample: true,
            normalize: Default::default(),
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
    /// The media list's columns, in order.
    #[serde(default = "default_media_list_columns")]
    pub media_list_columns: Vec<klipp_media::Field>,
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
    /// Show the "Recents" virtual folder in the file browser sidebar.
    /// When off, the 🕐 Recents button is hidden and recent files are
    /// still tracked (so turning it back on recovers the list) but the
    /// UI element is gone.
    #[serde(default = "default_true")]
    pub show_recent_files: bool,
    /// Automatically refresh the file list when files are added, removed,
    /// or renamed externally (e.g. by the OS file manager, a render job,
    /// or a camera card sync). Uses inotify on Linux / FSEvents on macOS.
    /// Users on slow or network-mounted drives may want to disable this
    /// and rely on the manual "Refresh Folder" right-click action instead.
    #[serde(default = "default_true")]
    pub auto_refresh: bool,
    /// Group files by modification date (Today / Yesterday / This Week /
    /// This Month / Older). When on, sort is forced to DateModified
    /// descending. When off, the previous sort key is restored.
    /// Show multi-frame filmstrip tiles on video hover with a large
    /// preview popup. When off, the existing single-frame skim is used
    /// instead (lighter, no popup).
    #[serde(default = "default_true")]
    pub filmstrip_hover: bool,
    /// Decode quality for filmstrip hover frames. Higher = sharper
    /// preview popup but slower initial generation on first hover.
    #[serde(default)]
    pub filmstrip_quality: FilmstripQuality,
    /// Keep filmstrip tiles visible after the cursor leaves (instead of
    /// reverting to the poster thumbnail). The large preview popup
    /// always disappears on leave regardless of this setting.
    #[serde(default)]
    pub filmstrip_persist: bool,
    /// Which columns are visible in the file browser list view.
    /// Name is always shown and not included here.
    #[serde(default = "default_visible_columns")]
    pub visible_columns: Vec<crate::browser::ColumnId>,
    #[serde(default)]
    pub group_by_date: bool,
    /// Most recently imported file paths, most-recent-first. Updated
    /// whenever `AppCommand::LoadMediaFile` completes successfully.
    /// Cross-project: survives project close/switch because it lives
    /// in the user's global prefs, not in a project file. Stale paths
    /// (files that have been moved/deleted) are pruned silently when
    /// the Recents view is rendered. Max 50 entries.
    #[serde(default)]
    pub recent_files: Vec<std::path::PathBuf>,
}

const MAX_RECENT_FILES: usize = 50;

/// Decode quality for file-browser filmstrip hover frames. Controls the
/// target height passed to `generate_filmstrip_incremental`. Higher
/// values produce sharper preview popups but take longer to generate
/// on first hover (~0.5s Low, ~1s Medium, ~2s High for a typical clip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FilmstripQuality {
    /// 120px height — fast, adequate for small popups.
    Low,
    /// 240px height — good balance of quality and speed.
    #[default]
    Medium,
    /// 480px height — crisp at full popup size, slower decode.
    High,
}

impl FilmstripQuality {
    pub fn target_height(self) -> u32 {
        match self {
            Self::Low => 120,
            Self::Medium => 240,
            Self::High => 480,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

impl Default for FileBrowserPrefs {
    fn default() -> Self {
        Self {
            file_thumb_size: Default::default(),
            file_sort_by: Default::default(),
            file_sort_ascending: true,
            file_type_filter: Default::default(),
            pinned_folders: Vec::new(),
            media_list_columns: default_media_list_columns(),
            file_browser_video_skim: true,
            file_browser_audio_skim: true,
            show_recent_files: true,
            visible_columns: default_visible_columns(),
            filmstrip_hover: true,
            filmstrip_quality: FilmstripQuality::default(),
            filmstrip_persist: false,
            auto_refresh: true,
            group_by_date: false,
            recent_files: Vec::new(),
        }
    }
}

impl FileBrowserPrefs {
    /// Record a file path as recently used. Moves it to the front if
    /// already present, then truncates to `MAX_RECENT_FILES`.
    pub fn add_recent_file(&mut self, path: std::path::PathBuf) {
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(MAX_RECENT_FILES);
    }
}

/// Preferences for the effects side of the Library browser tab. Kept
/// separate from `FileBrowserPrefs` because effects aren't files and
/// the two sets of knobs have no overlap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EffectsBrowserPrefs {
    /// Effect names the user has starred. Keyed by `EffectEntry.name`
    /// (the display name — stable across runs for built-ins; OFX names
    /// remain stable as long as the plugin identifier doesn't change).
    #[serde(default)]
    pub favorite_effects: Vec<String>,
    /// Transitions the user has starred, by the registry's `string_key`.
    #[serde(default)]
    pub favorite_transitions: Vec<String>,
    /// Title templates the user has starred, by template name.
    #[serde(default)]
    pub favorite_titles: Vec<String>,
    /// Generators the user has starred, by name.
    #[serde(default)]
    pub favorite_generators: Vec<String>,
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
    /// How many frames Shift+Left and Shift+Right move the playhead; 0
    /// moves one second, whatever the frame rate (#921).
    #[serde(default)]
    pub shift_step_frames: u32,
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
    /// Grid-snap interval. `Off` (default) preserves byte-identical
    /// pre-feature behaviour; flipping to any other variant makes
    /// dragged clip edges snap to the chosen grid spacing in addition
    /// to the existing clip-edge / playhead / marker / range targets.
    #[serde(default)]
    pub grid_snap_interval: klipp_core::GridInterval,
    /// Zoom step multiplier (e.g. 1.5 = 50% zoom per step).
    #[serde(default = "default_zoom_step")]
    pub zoom_step: f32,
    /// Backup version count (project.json.bak.1 through .bak.N).
    #[serde(default = "default_backup_count")]
    pub backup_count: u32,
    /// Whether playhead scrolls to follow playback position.
    #[serde(default = "default_true")]
    pub playhead_follows_playback: bool,
    /// Keep the timeline fitted to the available viewport as the window or
    /// layout changes. Off by default so manual zoom and track sizing remain stable.
    #[serde(default)]
    pub adaptive_timeline_fit: bool,
    /// Enable magnetic timeline (FCP-style collision-based editing).
    #[serde(default = "default_true")]
    pub magnetic_timeline: bool,
    /// Default multicam sync method.
    #[serde(default)]
    pub multicam_sync_method: MulticamSyncMethod,
    /// Show angle numbers overlay on multicam clips in timeline.
    #[serde(default = "default_true")]
    pub multicam_show_angles: bool,
    /// Mark spans of clips that show the same frames as another clip.
    #[serde(default = "default_true")]
    pub show_reused_footage: bool,
    /// Default tracker search region radius in pixels.
    #[serde(default = "default_track_search_radius")]
    pub track_search_radius: u32,
    /// Track point patch size (half-width) for matching.
    #[serde(default = "default_track_patch_size")]
    pub track_patch_size: u32,
    /// SAD threshold for tracking loss detection.
    #[serde(default = "default_track_threshold")]
    pub track_loss_threshold: u32,

    /// Scene-detection sensitivity, 0.1..=1.0. Threshold = value * 0.4.
    #[serde(default = "default_scene_sensitivity")]
    pub scene_detect_sensitivity: f32,
    /// Minimum seconds between detected scene cuts.
    #[serde(default = "default_scene_min_gap")]
    pub scene_detect_min_gap_secs: f32,
    /// Colour applied to markers created by scene detection.
    #[serde(default = "default_scene_marker_color")]
    pub scene_detect_marker_color: klipp_core::MarkerColor,
    /// Default resolution preset index for new sequences. Index into
    /// `panels::new_project_dialog::RESOLUTION_PRESETS`. Index 0 is
    /// "Automatic (from first clip)" — keeps the existing behaviour of
    /// inheriting from the active sequence / first imported clip. Other
    /// indexes pin a specific preset (HD, vertical, square, etc.) so
    /// every new sequence starts at a known frame size.
    #[serde(default)]
    pub default_sequence_resolution_idx: usize,
}

fn default_undo_limit() -> usize {
    1000
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
fn default_scene_sensitivity() -> f32 { 0.4 }
fn default_scene_min_gap() -> f32 { 0.5 }
fn default_scene_marker_color() -> klipp_core::MarkerColor { klipp_core::MarkerColor::Yellow }

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
            shift_step_frames: 0,
            default_video_track_height: default_video_track_h(),
            default_audio_track_height: default_audio_track_h(),
            snap_threshold_px: default_snap_px(),
            grid_snap_interval: klipp_core::GridInterval::default(),
            zoom_step: default_zoom_step(),
            backup_count: default_backup_count(),
            playhead_follows_playback: true,
            adaptive_timeline_fit: false,
            magnetic_timeline: true,
            multicam_sync_method: MulticamSyncMethod::default(),
            multicam_show_angles: true,
            show_reused_footage: true,
            track_search_radius: default_track_search_radius(),
            track_patch_size: default_track_patch_size(),
            track_loss_threshold: default_track_threshold(),
            scene_detect_sensitivity: default_scene_sensitivity(),
            scene_detect_min_gap_secs: default_scene_min_gap(),
            scene_detect_marker_color: default_scene_marker_color(),
            default_sequence_resolution_idx: 0, // Automatic
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPrefs {
    pub default_output_dir: Option<PathBuf>,
    pub last_preset_id: Option<String>,
    pub custom_presets: Vec<ExportPreset>,
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
    /// How many render jobs can run simultaneously. Higher = faster multi-segment
    /// exports, more CPU usage. Read at job-start time (takes effect on next export).
    #[serde(default = "default_parallel_jobs")]
    pub parallel_render_jobs: usize,
    /// Shell command to run after an export completes. Placeholders:
    /// `{file}` = output file path, `{dir}` = output directory, passed to
    /// the command as `$KLIPP_EXPORT_FILE` and `$KLIPP_EXPORT_DIR` rather
    /// than written into it (see `crate::export_hook`). Runs via `bash -c`
    /// in the background. Leave empty to skip.
    #[serde(default)]
    pub post_export_command: String,
}
fn default_export_sr() -> u32 { 48000 }
fn default_parallel_jobs() -> usize { 2 }

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
            audio_sample_rate: default_export_sr(),
            overwrite_mode: Default::default(),
            parallel_render: true,
            open_folder_after_export: false,
            parallel_render_jobs: default_parallel_jobs(),
            post_export_command: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
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

fn default_auto_save_ops() -> u32 { 25 }

impl Default for GeneralPrefs {
    fn default() -> Self {
        Self {
            show_welcome: true,
            open_maximized: true,
            last_project_format: None,
            auto_save_interval_secs: 300,
            check_for_updates: false,
            update_checks_answered: false,
            update_checks_asked: 0,
            preview_updates: None,
            last_update_check: 0,
            dismissed_update: None,
            auto_save_op_threshold: default_auto_save_ops(),
            default_project_dir: None,
            last_format_rate: None,
            recent_projects: Vec::new(),
            remember_inspector_layout: true,
            check_spelling: true,
            collapsed_sections: Vec::new(),
            startup_behavior: StartupBehavior::default(),
            dismissed_tips: std::collections::HashSet::new(),
        }
    }
}

/// A playback note shown for a project (#3234): its fix's kind, and the
/// share of frames dropped then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackNoteShown {
    pub fix: String,
    pub dropping: f32,
}

/// The most projects whose last playback note is kept.
pub const PLAYBACK_NOTES_KEPT: usize = 64;

impl Default for PlaybackPrefs {
    fn default() -> Self {
        Self {
            scrub_audio: true,
            skim_audio: true,
            jkl_audio_mute_threshold: 4.0,
            loop_playback: false,
            hear_delivery_loudness: true,
            skim_resolution: default_skim_resolution(),
            playback_quality: Default::default(),
            suggest_playback_fixes: true,
            playback_notes_shown: Default::default(),
            background_render: true,
            background_render_delay_secs: default_background_render_delay(),
            background_performance: BackgroundPerformance::default(),
            optimised_playback: OptimisedPlayback::default(),
            pre_roll_duration: default_pre_roll(),
            jkl_speed_step: default_jkl_step(),
            dim_ui_on_play: true,
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
                let prefs = Self::from_json_str(&contents);
                tracing::info!("Preferences loaded from {}", path.display());
                prefs
            }
            Err(_) => {
                tracing::info!("No preferences file found, using defaults");
                let mut prefs = Self::default();
                // A fresh install on Omarchy matches the desktop from
                // the first launch. Existing installs keep their theme.
                if crate::theme::omarchy::detect().is_some() {
                    tracing::info!("Omarchy detected: following the system theme by default");
                    prefs.appearance.follow_system_theme = true;
                } else {
                    // Elsewhere the desktop portal decides, once it has
                    // answered (`sync_system_theme`).
                    prefs.appearance.follow_system_theme_pending = true;
                }
                prefs
            }
        }
    }

    /// Parse a preferences JSON string with the full migration pipeline,
    /// falling back to `Preferences::default()` on any error path. Extracted
    /// from `load()` so the malformed-input behaviour can be tested without
    /// going through the on-disk config path.
    pub fn from_json_str(contents: &str) -> Self {
        // Two-pass parse: first into a generic JSON Value so we can run the
        // legacy field migrations, then into the typed Preferences struct.
        // The migrations are idempotent — if the new fields already exist,
        // the legacy ones are just dropped.
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(contents);
        let mut value = match parsed {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to read preferences JSON ({}), using defaults", e);
                return Self::default();
            }
        };
        Self::migrate_legacy_editing_to_file_browser(&mut value);
        Self::migrate_skim_resolution_default(&mut value);
        Self::migrate_filmstrip_height_default(&mut value);
        Self::migrate_skim_quality_default(&mut value);
        Self::migrate_to_revision(&mut value);
        match serde_json::from_value::<Self>(value) {
            Ok(mut prefs) => {
                // Migrate legacy theme_mode → theme_name (added when the
                // registry-based theme system landed). Idempotent.
                prefs.appearance.migrate_theme_mode();
                prefs
            }
            Err(e) => {
                tracing::warn!("Failed to parse preferences ({}), using defaults", e);
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
    /// Carry the old literal defaults, 480 and then 720, up to 1080.
    /// `serde(default)` only fills in *missing* fields, so a prefs file
    /// written before the default changed keeps its saved number. That
    /// mattered little while the cap was a floor rather than a cap and
    /// every skim frame came back at the source capped to 1080 whatever
    /// the number said; now that the cap is real, leaving 720 saved would
    /// halve the preview, because the reducer halves rather than scales.
    /// Only the two literal old defaults move, so a number the user chose
    /// is left alone.
    fn migrate_skim_resolution_default(value: &mut serde_json::Value) {
        let Some(playback) = value.get_mut("playback").and_then(|v| v.as_object_mut()) else {
            return;
        };
        if let Some(res) = playback.get("skim_resolution").and_then(|v| v.as_u64()) {
            if res == 480 || res == 720 {
                playback.insert(
                    "skim_resolution".to_string(),
                    serde_json::Value::from(1080u64),
                );
                tracing::info!(
                    "Migrated skim_resolution: {res} -> 1080 (the height skimming always delivered)"
                );
            }
        }
    }

    /// Bump `media_cache.filmstrip_height` from the old defaults (48 then 96)
    /// to the new default of 128. At enlarged track heights (>= 200px) lower
    /// values produce visibly jagged/blurry tiles. Only rewrites the literal
    /// old defaults — any value the user explicitly set is left alone.
    /// Bump `media_cache.skim_transcode_quality` from the old default of
    /// 23 (visually-transparent CRF/QP) to 30 (industry-standard proxy
    /// quality). 23 combined with the encoder's `-g 1` flag produced
    /// ~50 GB skim files for 35-min 4K clips — 5× what FCP or DaVinci
    /// write. Only rewrites the literal old default; anything the user
    /// explicitly set is left alone.
    /// Settings that were saved but read by nothing until #1816 carry
    /// the default they had then, which may not be the one they have
    /// now that they act. A file below [`PREFS_REVISION`] has those old
    /// defaults moved to the new ones, once: the revision is then
    /// written, so a value set afterwards is kept even if it equals an
    /// old default.
    fn migrate_to_revision(value: &mut serde_json::Value) {
        let Some(root) = value.as_object_mut() else { return };
        let revision = root.get("revision").and_then(|v| v.as_u64()).unwrap_or(0);
        if revision < 1 {
            // The title size's old default, 48, was never a title's size:
            // titles were 72 while it did nothing.
            if let Some(title) = root.get_mut("title").and_then(|v| v.as_object_mut()) {
                if title.get("default_size_pt").and_then(|v| v.as_f64()) == Some(48.0) {
                    title.insert("default_size_pt".into(), serde_json::Value::from(default_title_size()));
                }
            }
            // The thumbnail size was a height of 128 to 320 that sized
            // nothing; thumbnails were always 480 wide, now its default.
            if let Some(mc) = root.get_mut("media_cache").and_then(|v| v.as_object_mut()) {
                if mc.contains_key("thumbnail_height") {
                    mc.insert("thumbnail_height".into(), serde_json::Value::from(default_thumb_h()));
                }
            }
            // A fade of 0 ms is no fade: Add Fade In would do nothing.
            if let Some(audio) = root.get_mut("audio").and_then(|v| v.as_object_mut()) {
                if audio.get("default_fade_duration_ms").and_then(|v| v.as_u64()) == Some(0) {
                    audio.insert("default_fade_duration_ms".into(), serde_json::Value::from(default_fade_ms()));
                }
            }
        }
        if revision < 2 {
            // The max track height's old default, 200, capped nothing:
            // tracks were resized up to 300 while it did nothing.
            if let Some(tl) = root.get_mut("timeline").and_then(|v| v.as_object_mut()) {
                if tl.get("max_track_height").and_then(|v| v.as_f64()) == Some(200.0) {
                    tl.insert("max_track_height".into(), serde_json::Value::from(default_max_track_h()));
                }
            }
        }
        root.insert("revision".into(), serde_json::Value::from(PREFS_REVISION));
    }

    fn migrate_skim_quality_default(value: &mut serde_json::Value) {
        let Some(mc) = value.get_mut("media_cache").and_then(|v| v.as_object_mut()) else {
            return;
        };
        if let Some(q) = mc.get("skim_transcode_quality").and_then(|v| v.as_u64()) {
            if q == 23 {
                mc.insert(
                    "skim_transcode_quality".to_string(),
                    serde_json::Value::from(30u64),
                );
                tracing::info!(
                    "Migrated skim_transcode_quality: 23 -> 30 (proxy-size defaults)"
                );
            }
        }
    }

    fn migrate_filmstrip_height_default(value: &mut serde_json::Value) {
        let Some(mc) = value.get_mut("media_cache").and_then(|v| v.as_object_mut()) else {
            return;
        };
        if let Some(h) = mc.get("filmstrip_height").and_then(|v| v.as_u64()) {
            if h == 48 || h == 96 {
                mc.insert(
                    "filmstrip_height".to_string(),
                    serde_json::Value::from(128u64),
                );
                tracing::info!(
                    "Migrated filmstrip_height: {} -> 128 (sharper tiles at enlarged tracks)", h
                );
            }
        }
    }

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
        // Restoring an auto-save opens its folder; the project it belongs
        // to is what goes in the list.
        if crate::app::is_autosave_path(&project.path) {
            return;
        }
        let mut project = project;
        // Saving or opening a pinned project keeps it pinned.
        project.pinned |= self.general.recent_projects.iter().any(|p| p.path == project.path && p.pinned);
        self.general.recent_projects.retain(|p| p.path != project.path);
        self.general.recent_projects.insert(0, project);
        self.general.recent_projects.truncate(20);
    }

    /// Enrich recent project entries that are missing metadata by reading
    /// from the project files on disk. Returns true if any entries were updated.
    pub fn enrich_recent_projects(&mut self) -> bool {
        let mut changed = false;
        for rp in &mut self.general.recent_projects {
            // A project goes by its file's name, as Files and Open show
            // it. Its sequence's name was put here once and read as the
            // project's ("Main Sequence" for a project called Two Shores).
            if let Some(stem) = rp.path.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()) {
                if rp.name != stem {
                    rp.name = stem;
                    changed = true;
                }
            }
            if rp.total_size_bytes.is_some()
                && rp.duration_secs.is_some()
                && rp.fps.is_some()
                && rp.clip_count.is_some()
                && rp.missing_media.is_some()
                && rp.sequence_count.is_some()
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
                Ok((sequences, media_pool, _color, _active_id, _created_at, _notes, _zoom, _scroll, _track_scale, _assets)) => {
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
                    if rp.sequence_count.is_none() {
                        rp.sequence_count = Some(sequences.len() as u32);
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
    /// How often (in seconds) to re-show the "muted by system" toast
    /// while the OS-level app mute is active. 0 = show only once.
    /// Detects PipeWire/WirePlumber per-app mute state.
    #[serde(default = "default_mute_reminder_interval")]
    pub mute_reminder_interval_secs: u32,
    /// Input device name used for audio recording (#60). Empty string = system
    /// default. Kept as `String` (not `Option<String>`) to match the "Default"
    /// sentinel idiom used by the recording input picker dropdown.
    #[serde(default)]
    pub input_device: String,
}

fn default_sample_rate() -> u32 { 48000 }
fn default_buffer_size() -> u32 { 256 }
fn default_mute_reminder_interval() -> u32 { 15 }
fn default_visible_columns() -> Vec<crate::browser::ColumnId> {
    use crate::browser::ColumnId;
    vec![ColumnId::Size, ColumnId::Date]
}
impl Default for AudioHardwarePrefs {
    fn default() -> Self {
        Self {
            output_device: None,
            sample_rate: default_sample_rate(),
            buffer_size: default_buffer_size(),
            mute_reminder_interval_secs: default_mute_reminder_interval(),
            input_device: String::new(),
        }
    }
}

/// Legacy theme-mode enum, retained for back-compat with
/// preferences.json files written before the registry-based theme
/// system. New code should use `AppearancePrefs.theme_name: String`.
/// On load, a present `theme_mode` is translated to a `theme_name`
/// via [`AppearancePrefs::migrate_theme_mode`] then dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
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
    /// Whether the note that Auto made the interface smaller for a small
    /// window has been shown (#2944): once, ever.
    #[serde(default)]
    pub small_screen_note_shown: bool,
    /// Theme name as registered in [`crate::theme::ThemeRegistry`].
    /// Defaults to "Dark" (the built-in name from `theme/builtin/dark.toml`).
    /// User themes drop into `~/.config/klipp/themes/*.toml` and
    /// appear here by their TOML `name` field.
    #[serde(default = "default_theme_name")]
    pub theme_name: String,
    /// Follow the desktop's theme instead of `theme_name`. Only
    /// Omarchy is understood today (`theme/omarchy.rs`); on a desktop
    /// without it the flag is ignored and `theme_name` applies, so
    /// preferences travel between machines. `theme_name` is left
    /// untouched while following, so switching it off restores the
    /// user's own choice. Defaults to on for a fresh install under
    /// Omarchy (see `Preferences::load`), off otherwise.
    #[serde(default)]
    pub follow_system_theme: bool,
    /// A fresh install with no full system palette, waiting for the
    /// desktop portal's first answer: following turns on then if the
    /// desktop prefers dark or light (#2625). Not saved.
    #[serde(skip)]
    pub follow_system_theme_pending: bool,
    /// Legacy: pre-registry theme mode enum. If present, the
    /// post-load migration in `Preferences::load` translates it to
    /// `theme_name` and clears this field. Kept Serialize so existing
    /// installs that still emit it round-trip cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_mode: Option<ThemeMode>,
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
fn default_accent_color() -> [u8; 3] { [0x4a, 0x8f, 0xe7] }
fn default_playhead_color() -> [u8; 3] { [0xff, 0x5a, 0x3c] }
fn default_theme_name() -> String { "Dark".to_string() }
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
            small_screen_note_shown: false,
            theme_name: default_theme_name(),
            follow_system_theme: false,
            follow_system_theme_pending: false,
            theme_mode: None,
            accent_color: default_accent_color(),
            playhead_color: default_playhead_color(),
            role_video: default_role_video(), role_dialogue: default_role_dialogue(),
            role_music: default_role_music(), role_sfx: default_role_sfx(),
            role_title: default_role_title(), role_broll: default_role_broll(),
            role_ambience: default_role_ambience(), role_foley: default_role_foley(),
            role_voiceover: default_role_voiceover(), role_graphics: default_role_graphics(),
        }
    }
}

impl AppearancePrefs {
    /// True while every role colour is still the shipped default,
    /// i.e. the user has never picked their own. A theme that carries
    /// role colours (the Omarchy system theme) may then supply them.
    pub fn roles_are_default(&self) -> bool {
        let d = AppearancePrefs::default();
        self.role_video == d.role_video
            && self.role_dialogue == d.role_dialogue
            && self.role_music == d.role_music
            && self.role_sfx == d.role_sfx
            && self.role_title == d.role_title
            && self.role_broll == d.role_broll
            && self.role_ambience == d.role_ambience
            && self.role_foley == d.role_foley
            && self.role_voiceover == d.role_voiceover
            && self.role_graphics == d.role_graphics
    }

    /// Translate the legacy `theme_mode` enum to the registry-based
    /// `theme_name`. Run once during `Preferences::load` so the
    /// pre-registry preferences.json migrates forward without
    /// dropping the user's choice.
    ///
    /// After translation, `theme_mode` is cleared so the next save
    /// stops emitting it (the field is `skip_serializing_if = "Option::is_none"`).
    pub fn migrate_theme_mode(&mut self) {
        if let Some(mode) = self.theme_mode.take() {
            // Only overwrite the name if it's still the default —
            // a user who already set `theme_name` explicitly wins.
            if self.theme_name == default_theme_name() {
                self.theme_name = match mode {
                    ThemeMode::Dark => "Dark".to_string(),
                    ThemeMode::Light => "Light".to_string(),
                };
            }
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
    /// How wide a media item's thumbnail is decoded, in pixels: wider
    /// is sharper on large tiles and costs more to make and keep.
    /// (Named for the height it was once meant to be.)
    #[serde(default = "default_thumb_h")]
    pub thumbnail_height: u32,
    /// Filmstrip tile height in pixels. Each tile in the timeline clip's
    /// mini-strip is decoded and cached at this height. Too-low values
    /// look blurry when track height is increased; too-high values eat
    /// disk and memory. Previously hardcoded to 48 — visibly poor on
    /// enlarged tracks (200px+ look 4× upscaled).
    #[serde(default = "default_filmstrip_h")]
    pub filmstrip_height: u32,
    /// Legacy field — replaced by max_cache_size_gb.
    #[serde(default)]
    pub max_cache_size_mb: u32,
}
fn default_cache_size_gb() -> u32 { 20 }
// 30 lands at ~industry-proxy bitrate (FCP ProRes Proxy / DaVinci
// DNxHR LB equivalent, ~10 GB for a 35-min 4K clip). The previous
// default of 23 was "visually transparent" quality — when paired
// with the encoder's `-g 1` setting that forces every frame to be
// a keyframe, it ballooned 1080p skim output to ~50 GB on the same
// clip. Users who want finer detail can still select High (24) in
// the Cache prefs.
fn default_skim_quality() -> u32 { 30 }
/// The width media thumbnails were always decoded at.
fn default_thumb_h() -> u32 { 480 }
fn default_filmstrip_h() -> u32 { 128 }
impl Default for MediaCachePrefs {
    fn default() -> Self {
        Self {
            filmstrip_cache_dir: None, audio_cache_dir: None, render_cache_dir: None, enable_render_cache: true,
            max_cache_size_gb: default_cache_size_gb(),
            skim_transcode_quality: default_skim_quality(),
            thumbnail_height: default_thumb_h(),
            filmstrip_height: default_filmstrip_h(),
            max_cache_size_mb: 0,
        }
    }
}

/// Color grading preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorPrefs {
    /// Viewer LUT file path (applied for display only).
    #[serde(default)]
    pub viewer_lut_path: Option<PathBuf>,
    /// LUT interpolation method.
    #[serde(default)]
    pub lut_interpolation: LutInterpolation,
    /// The monitor the SDR viewer encodes for: sRGB (a desktop monitor)
    /// or BT.1886 (a grading monitor, gamma 2.4).
    #[serde(default)]
    pub sdr_display_transfer: klipp_core::SdrTransfer,
    /// Scope rendering quality.
    #[serde(default)]
    pub scope_quality: klipp_core::ScopeQuality,
    /// Scope refresh interval in milliseconds.
    #[serde(default = "default_scope_ms")]
    pub scope_update_interval_ms: u32,
    /// Vectorscope skin-tone line angle in degrees.
    #[serde(default = "default_skin_angle")]
    pub vectorscope_skin_angle: f32,
    /// Node-graph: highlight + snap the in-flight drag wire to the
    /// nearest input port within `port_snap_radius_px`. Toggleable
    /// for users who prefer click-precise drops with no magnetism.
    #[serde(default = "default_port_snap_enabled")]
    pub port_snap_enabled: bool,
    /// Node-graph: snap radius in screen pixels. Bigger = easier
    /// to land a connection but harder to release in empty space
    /// (the wire snaps to the nearest port instead of dropping).
    #[serde(default = "default_port_snap_radius")]
    pub port_snap_radius_px: f32,
    /// One printer point on the offset, as a shift of the signal (0 to
    /// 1): the step the printer-light keys move a channel by (#241).
    #[serde(default = "default_printer_point")]
    pub printer_point: f32,
    /// The node editor's recent add-menu picks, newest first (#1745),
    /// kept across sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_add_recents: Vec<String>,
    /// Offer, on the Colour page, to give a graded piece's grade to the
    /// other ungraded pieces of its split shot. Off unless turned on.
    #[serde(default)]
    pub offer_grade_to_shot_pieces: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LutInterpolation {
    Trilinear,
    #[default]
    Tetrahedral,
}

fn default_scope_ms() -> u32 { 32 }
fn default_skin_angle() -> f32 { 123.0 }
fn default_port_snap_enabled() -> bool { true }
fn default_port_snap_radius() -> f32 { 18.0 }
fn default_printer_point() -> f32 { 0.0125 }
impl Default for ColorPrefs {
    fn default() -> Self {
        Self {
            viewer_lut_path: None,
            lut_interpolation: Default::default(),
            sdr_display_transfer: Default::default(),
            scope_quality: Default::default(),
            scope_update_interval_ms: default_scope_ms(),
            vectorscope_skin_angle: default_skin_angle(),
            port_snap_enabled: default_port_snap_enabled(),
            port_snap_radius_px: default_port_snap_radius(),
            printer_point: default_printer_point(),
            node_add_recents: Vec::new(),
            offer_grade_to_shot_pieces: false,
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
    /// Font families starred in the font picker, shown first (#1719).
    #[serde(default)]
    pub favourite_fonts: Vec<String>,
    /// Font families picked lately, most recent first (#1719).
    #[serde(default)]
    pub recent_fonts: Vec<String>,
    /// Default title color as RGB.
    #[serde(default = "default_title_color")]
    pub default_color: [u8; 3],
    /// Default text alignment.
    #[serde(default)]
    pub default_alignment: TextAlignment,
    /// Default title clip duration in seconds.
    #[serde(default = "default_title_duration")]
    pub default_duration_secs: f32,
    /// Title-inspector groups currently expanded by the user. Empty
    /// (the default) means every group is collapsed on a fresh
    /// install. Names match the group headers exactly: "Effects",
    /// "Typography", "Background & Layer Opacity", "Animation",
    /// "3D & Distortion", "Text Path", "Character Animation".
    #[serde(default)]
    pub inspector_expanded_groups: std::collections::HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextAlignment {
    Left,
    #[default]
    Center,
    Right,
}

fn default_title_font() -> String { String::new() }
/// A new title's size: the default title's own, so a fresh install's
/// titles look as they did before this setting acted (#1816).
fn default_title_size() -> f32 { 72.0 }
fn default_title_color() -> [u8; 3] { [255, 255, 255] }
fn default_title_duration() -> f32 { 5.0 }
/// The font picker keeps this many recent families.
pub const MAX_RECENT_FONTS: usize = 8;

impl TitlePrefs {
    /// `family` starred, or unstarred if it was.
    pub fn toggle_favourite_font(&mut self, family: &str) {
        if let Some(i) = self.favourite_fonts.iter().position(|f| f == family) {
            self.favourite_fonts.remove(i);
        } else {
            self.favourite_fonts.push(family.to_string());
        }
    }

    /// `family` used now: first of the recent ones, the list kept short.
    pub fn note_recent_font(&mut self, family: &str) {
        self.recent_fonts.retain(|f| f != family);
        self.recent_fonts.insert(0, family.to_string());
        self.recent_fonts.truncate(MAX_RECENT_FONTS);
    }
}

impl Default for TitlePrefs {
    fn default() -> Self {
        Self {
            default_font: default_title_font(),
            default_size_pt: default_title_size(),
            font_search_paths: Vec::new(),
            favourite_fonts: Vec::new(),
            recent_fonts: Vec::new(),
            default_color: default_title_color(),
            default_alignment: Default::default(),
            default_duration_secs: default_title_duration(),
            inspector_expanded_groups: std::collections::HashSet::new(),
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
    /// Border width for selected clips, in pixels. Default 3.0 — thicker
    /// than the hover/idle border so selection reads at a glance.
    #[serde(default = "default_selection_border_width")]
    pub clip_selection_border_width: f32,
    /// Fit the timeline to the whole sequence after an assistant's edit
    /// changes its length or puts a clip out of view. On by default.
    #[serde(default = "default_true")]
    pub fit_after_assistant_edits: bool,
}
fn default_clip_colors() -> Vec<[u8; 3]> { vec![[50,90,160],[50,130,80],[160,90,50],[130,50,130],[50,130,130],[160,50,80]] }
fn default_clip_gap() -> f32 { 3.0 }
fn default_trim_handle() -> f32 { 14.0 }
fn default_overview_h() -> f32 { 24.0 }
fn default_min_track_h() -> f32 { 24.0 }
/// The tallest a track is resized to: the 300 px it always was.
fn default_max_track_h() -> f32 { 300.0 }
fn default_text_track_h() -> f32 { 32.0 }
fn default_selection_border_width() -> f32 { 3.0 }
impl Default for TimelinePrefs {
    fn default() -> Self {
        Self {
            clip_colors: default_clip_colors(), clip_gap_px: default_clip_gap(),
            trim_handle_width: default_trim_handle(),
            overview_bar_height: default_overview_h(),
            min_track_height: default_min_track_h(), max_track_height: default_max_track_h(),
            text_track_height: default_text_track_h(),
            clip_selection_border_width: default_selection_border_width(),
            fit_after_assistant_edits: true,
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
    /// Zebra high threshold (overexposure warning). Per-channel, 0–255.
    #[serde(default = "default_zebra_high")]
    pub zebra_high_threshold: u8,
    /// Zebra low threshold (underexposure warning). Per-channel, 0–255.
    #[serde(default = "default_zebra_low")]
    pub zebra_low_threshold: u8,
    /// Focus peaking sensitivity (0.0 = show all edges, 1.0 = only strongest).
    #[serde(default = "default_peaking_sensitivity")]
    pub focus_peaking_sensitivity: f32,
    /// Focus peaking overlay color [R, G, B].
    #[serde(default = "default_peaking_color")]
    pub focus_peaking_color: [u8; 3],
    /// False color palette (customizable stops).
    #[serde(default = "default_false_color_palette")]
    pub false_color_palette: FalseColorPalette,
}
fn default_action_safe() -> f32 { 0.9 }
fn default_title_safe() -> f32 { 0.8 }
fn default_grid_color() -> [u8; 4] { [200, 200, 200, 80] }
fn default_checker_tile() -> f32 { 12.0 }
fn default_handle_size() -> f32 { 8.0 }
fn default_fullscreen_timeout() -> f32 { 3.0 }
fn default_zebra_high() -> u8 { 235 }
fn default_zebra_low() -> u8 { 16 }
fn default_peaking_sensitivity() -> f32 { 0.5 }
fn default_peaking_color() -> [u8; 3] { [255, 0, 255] } // magenta
impl Default for ViewerPrefs {
    fn default() -> Self {
        Self {
            action_safe_pct: default_action_safe(), title_safe_pct: default_title_safe(),
            grid_color: default_grid_color(), checkerboard_tile_size: default_checker_tile(),
            transform_handle_size: default_handle_size(), fullscreen_overlay_timeout_secs: default_fullscreen_timeout(),
            zebra_high_threshold: default_zebra_high(), zebra_low_threshold: default_zebra_low(),
            focus_peaking_sensitivity: default_peaking_sensitivity(), focus_peaking_color: default_peaking_color(),
            false_color_palette: default_false_color_palette(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// False Color palette
// ═══════════════════════════════════════════════════════════════════

/// A single color stop in a false color palette.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FalseColorStop {
    pub luma_min: u8,
    pub luma_max: u8,
    pub color: [u8; 3],
    pub label: String,
}

/// A named false color palette with an ordered set of luma→color stops.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FalseColorPalette {
    pub name: String,
    pub stops: Vec<FalseColorStop>,
}

impl FalseColorPalette {
    /// The built-in palette `name`, or, if Klipp's file doesn't have
    /// it, one with no stops (every luma passes through).
    fn built_in(name: &str) -> Self {
        Self::built_ins().into_iter().find(|p| p.name == name).unwrap_or_else(|| Self { name: name.into(), stops: Vec::new() })
    }

    /// Standard broadcast false color palette (13 stops, full 0–255 coverage).
    pub fn broadcast() -> Self {
        Self::built_in("Broadcast Standard")
    }

    /// ARRI LogC-style palette: blue shadows, green mids, pink highlights.
    pub fn arri() -> Self {
        Self::built_in("ARRI")
    }

    /// RED-style palette: teal shadows, green mids, orange-red highlights.
    pub fn red() -> Self {
        Self::built_in("RED")
    }

    /// Pre-build a 256-entry LUT: luma index → [R, G, B].
    /// Uncovered luma values map to [0, 0, 0] (passthrough sentinel).
    pub fn build_lut(&self) -> [u8; 768] {
        let mut lut = [0u8; 768];
        for stop in &self.stops {
            for luma in stop.luma_min..=stop.luma_max {
                let idx = luma as usize * 3;
                lut[idx]     = stop.color[0];
                lut[idx + 1] = stop.color[1];
                lut[idx + 2] = stop.color[2];
            }
        }
        lut
    }

    /// The built-in palettes, Klipp's file
    /// `assets/presets/false_colour/builtin.toml` (#2887), read once; a
    /// file that doesn't read leaves none, said, never a crash.
    pub fn built_ins() -> Vec<FalseColorPalette> {
        static PALETTES: std::sync::OnceLock<Vec<FalseColorPalette>> = std::sync::OnceLock::new();
        PALETTES
            .get_or_init(|| {
                read_palettes(include_str!("../../../assets/presets/false_colour/builtin.toml")).unwrap_or_else(|why| {
                    tracing::error!("the built-in false colour palettes don't read: {why}");
                    Vec::new()
                })
            })
            .clone()
    }

    /// Every palette offered: Klipp's, then a person's own from their
    /// `false_colour` presets folder (#3095), read once a run, each read
    /// as someone else's file; what of theirs is skipped is said in the
    /// log.
    pub fn palettes() -> Vec<FalseColorPalette> {
        static PALETTES: klipp_core::preset_file::Reloading<Vec<FalseColorPalette>> = klipp_core::preset_file::Reloading::new();
        PALETTES
            .get(|| {
                let (palettes, skipped) = palettes_in(&klipp_core::preset_places::dirs_for(PALETTES_FOLDER));
                for why in skipped {
                    tracing::warn!("a false colour palette of yours was skipped: {why}");
                }
                palettes
            })
            .clone()
    }

    /// Count of luma values (0–255) not covered by any stop.
    pub fn unmapped_count(&self) -> u32 {
        let mut covered = [false; 256];
        for stop in &self.stops {
            for luma in stop.luma_min..=stop.luma_max {
                covered[luma as usize] = true;
            }
        }
        covered.iter().filter(|&&c| !c).count() as u32
    }

    /// Check for overlapping stop ranges. Returns pairs of indices that overlap.
    pub fn overlapping_stops(&self) -> Vec<(usize, usize)> {
        let mut overlaps = Vec::new();
        for i in 0..self.stops.len() {
            for j in (i + 1)..self.stops.len() {
                if self.stops[i].luma_min <= self.stops[j].luma_max
                    && self.stops[j].luma_min <= self.stops[i].luma_max
                {
                    overlaps.push((i, j));
                }
            }
        }
        overlaps
    }

    /// Sort stops by luma_min.
    pub fn sort_stops(&mut self) {
        self.stops.sort_by_key(|s| s.luma_min);
    }
}

use klipp_core::false_colour::{PaletteEntry, PALETTES_FOLDER};

impl From<PaletteEntry> for FalseColorPalette {
    fn from(e: PaletteEntry) -> Self {
        let stops = e.stop.into_iter().map(|s| FalseColorStop { luma_min: s.luma.0, luma_max: s.luma.1, color: s.color, label: s.label }).collect();
        FalseColorPalette { name: e.name, stops }
    }
}

/// The palettes of a false colour palettes file, as
/// [`klipp_core::false_colour::read_palette_entries`] reads them.
fn read_palettes(text: &str) -> Result<Vec<FalseColorPalette>, String> {
    klipp_core::false_colour::read_palette_entries(text).map(|entries| entries.into_iter().map(FalseColorPalette::from).collect())
}

/// Klipp's palettes, then those in `dirs` (a person's folder, then the
/// packs'), each read as someone else's (#3095), and what was skipped.
fn palettes_in(dirs: &[std::path::PathBuf]) -> (Vec<FalseColorPalette>, Vec<String>) {
    let merged = klipp_core::false_colour::palette_listing_in(dirs);
    (merged.entries.into_iter().map(FalseColorPalette::from).collect(), merged.skipped)
}

fn default_false_color_palette() -> FalseColorPalette {
    FalseColorPalette::broadcast()
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

/// External-application handoff for round-trip editing (GIMP / Krita / Audacity / etc).
/// Empty strings fall back to `xdg-open`, which delegates to the user's OS default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolsPrefs {
    #[serde(default)]
    pub image_editor: String,
    #[serde(default)]
    pub audio_editor: String,
    #[serde(default)]
    pub video_editor: String,
    /// When true, external-edit saves are picked up by the existing media
    /// watcher and the clip's metadata / thumbnails refresh automatically.
    #[serde(default = "default_true")]
    pub round_trip_reload: bool,
}

impl Default for ExternalToolsPrefs {
    fn default() -> Self {
        Self {
            image_editor: String::new(),
            audio_editor: String::new(),
            video_editor: String::new(),
            round_trip_reload: true,
        }
    }
}

/// On-device AI: the three features, each its own opt-out, and how speech
/// transcription runs. Nothing here ever leaves the machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiPrefs {
    /// Speech transcription (for captions and subtitles).
    #[serde(default = "default_true")]
    pub transcription: bool,
    /// Subject detection — the framing / masking vision model.
    #[serde(default = "default_true")]
    pub subject_detection: bool,
    /// Reading on-screen text (OCR) in screen recordings, for search,
    /// the beat sheet and the sensitive-text check. Its two small
    /// models are downloaded only on request.
    #[serde(default = "default_true")]
    pub screen_text: bool,
    /// The AI assistant: whether the control socket and MCP server run,
    /// so an agent can drive the app. Takes effect on restart.
    #[serde(default = "default_true")]
    pub assistant: bool,
    /// Run speech transcription on the CPU instead of the GPU: slower,
    /// but works without a working Vulkan GPU.
    #[serde(default)]
    pub transcribe_on_cpu: bool,
}

impl Default for AiPrefs {
    fn default() -> Self {
        Self { transcription: true, subject_detection: true, screen_text: true, assistant: true, transcribe_on_cpu: false }
    }
}

fn default_media_list_columns() -> Vec<klipp_media::Field> {
    klipp_media::Field::DEFAULT_COLUMNS.to_vec()
}

#[cfg(test)]
mod tests {
    /// #3234: the playback suggestions' switch and each project's last
    /// note are kept across sessions; a preferences file from before them
    /// suggests, with none shown yet.
    #[test]
    fn playback_suggestions_are_kept_across_sessions() {
        let mut p = PlaybackPrefs::default();
        assert!(p.suggest_playback_fixes && p.playback_notes_shown.is_empty());
        p.suggest_playback_fixes = false;
        p.playback_notes_shown.insert("/films/a.klipp".into(), PlaybackNoteShown { fix: "freeze_track".into(), dropping: 0.12 });
        let back: PlaybackPrefs = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert!(!back.suggest_playback_fixes);
        assert_eq!(back.playback_notes_shown["/films/a.klipp"], PlaybackNoteShown { fix: "freeze_track".into(), dropping: 0.12 });
        let mut old = serde_json::to_value(PlaybackPrefs::default()).unwrap();
        old.as_object_mut().unwrap().remove("suggest_playback_fixes");
        old.as_object_mut().unwrap().remove("playback_notes_shown");
        let old: PlaybackPrefs = serde_json::from_value(old).unwrap();
        assert!(old.suggest_playback_fixes && old.playback_notes_shown.is_empty());
    }

    /// #3212: drawing ahead is on by default, after two idle seconds, and
    /// a preferences file from before the delay keeps the default.
    #[test]
    fn drawing_ahead_is_on_after_two_seconds_by_default() {
        let p = PlaybackPrefs::default();
        assert!(p.background_render);
        assert_eq!(p.background_render_delay_secs, 2.0);
        let mut old = serde_json::to_value(PlaybackPrefs { background_render_delay_secs: 9.0, ..PlaybackPrefs::default() }).unwrap();
        assert_eq!(serde_json::from_value::<PlaybackPrefs>(old.clone()).unwrap().background_render_delay_secs, 9.0, "kept");
        old.as_object_mut().unwrap().remove("background_render_delay_secs");
        assert_eq!(serde_json::from_value::<PlaybackPrefs>(old).unwrap().background_render_delay_secs, 2.0);
    }

    use super::*;

    /// #1854: a recent project goes by its file's name, not a name
    /// stored in it, even when its folder is gone.
    #[test]
    fn a_recent_project_is_named_by_its_file() {
        let mut prefs = Preferences::default();
        let mut rp = RecentProject::from_path(PathBuf::from("/nowhere/Two Shores.klipp"));
        rp.name = "Main Sequence".into();
        prefs.general.recent_projects = vec![rp];
        assert!(prefs.enrich_recent_projects());
        assert_eq!(prefs.general.recent_projects[0].name, "Two Shores");
    }

    /// #1847: saving a pinned project puts it first in time order and
    /// keeps its pin.
    #[test]
    fn saving_a_pinned_project_keeps_it_pinned() {
        let mut prefs = Preferences::default();
        let mut a = RecentProject::from_path(PathBuf::from("/p/A.klipp"));
        a.pinned = true;
        prefs.add_recent_project(a);
        prefs.add_recent_project(RecentProject::from_path(PathBuf::from("/p/B.klipp")));
        prefs.add_recent_project(RecentProject::from_path(PathBuf::from("/p/A.klipp")));
        let r = &prefs.general.recent_projects;
        assert_eq!(r[0].path, PathBuf::from("/p/A.klipp"));
        assert!(r[0].pinned && !r[1].pinned);
    }

    #[test]
    fn a_saved_recent_list_is_read_without_autosaves() {
        let json = serde_json::json!({ "general": { "recent_projects": [
            { "path": "/p/X_autosave.klipp", "name": "X_autosave" },
            { "path": "/p/Y_autosave.klipp", "name": "Y_autosave" },
            { "path": "/p/Y.klipp", "name": "Y" },
            { "path": "/p/Z.klipp", "name": "Z" },
        ] } });
        let mut prefs = serde_json::to_value(Preferences::default()).unwrap();
        prefs["general"]["recent_projects"] = json["general"]["recent_projects"].clone();
        let prefs: Preferences = serde_json::from_value(prefs).unwrap();
        let listed: Vec<_> = prefs.general.recent_projects.iter()
            .map(|r| (r.path.to_string_lossy().into_owned(), r.name.clone()))
            .collect();
        assert_eq!(listed, vec![
            ("/p/X.klipp".to_string(), "X".to_string()),
            ("/p/Y.klipp".to_string(), "Y".to_string()),
            ("/p/Z.klipp".to_string(), "Z".to_string()),
        ]);
    }

    #[test]
    fn an_autosave_never_enters_the_recent_list() {
        let mut prefs = Preferences::default();
        prefs.add_recent_project(RecentProject::from_path(PathBuf::from("/p/X_autosave.klipp")));
        prefs.add_recent_project(RecentProject::from_path(PathBuf::from("/p/X.klipp")));
        let paths: Vec<_> = prefs.general.recent_projects.iter().map(|r| r.path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("/p/X.klipp")]);
    }

    #[test]
    fn a_newer_autosave_is_found_and_an_older_one_is_not() {
        let dir = std::env::temp_dir().join(format!("klipp-newer-autosave-{}", std::process::id()));
        let project = dir.join("X.klipp");
        let autosave = crate::app::autosave_path_for(&project);
        let save = |folder: &std::path::Path, at: &str| {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("project.json"), format!(r#"{{"saved_at":"{at}","generation":"{at}","sequences":[]}}"#)).unwrap();
        };
        save(&project, "2026-10-26T10:00:00Z");
        assert!(!crate::app::has_newer_autosave(&project), "no auto-save at all");
        save(&autosave, "2026-10-26T10:05:00Z");
        assert!(crate::app::has_newer_autosave(&project));
        // Said at startup, by the project's name.
        let notice = crate::app::crash_notice(&[project.clone(), dir.join("Other.klipp")]).expect("a notice");
        assert!(notice.contains("\u{201c}X\u{201d}") && notice.contains("recovered"), "{notice}");
        // A lock written into the project afterwards doesn't hide it.
        std::fs::write(project.join("project.lock"), "pid 1").unwrap();
        assert!(crate::app::has_newer_autosave(&project), "the lock moves the folder's time, not the saves'");
        // Keep Saved Version marks it seen, and leaves it on disk.
        crate::app::retire_autosave(&project);
        assert!(!crate::app::has_newer_autosave(&project));
        assert_eq!(crate::app::crash_notice(std::slice::from_ref(&project)), None);
        assert!(autosave.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        assert_eq!(rt.playback.skim_resolution, 1080);

        // EditingPrefs
        assert!(rt.editing.auto_create_tracks);
        assert!(rt.editing.auto_generate_filmstrips);
        assert!(rt.editing.auto_generate_skim);
        assert_eq!(rt.editing.default_still_duration_secs, 5.0);
        assert_eq!(rt.editing.undo_limit, 1000);
        assert_eq!(rt.editing.default_transition_frames, 24);
        assert_eq!(rt.editing.default_video_track_height, 50.0);
        assert_eq!(rt.editing.default_audio_track_height, 48.0);
        assert_eq!(rt.editing.snap_threshold_px, 10.0);
        // Pinned: default grid_snap_interval MUST be Off — preserves
        // byte-identical pre-feature snap behaviour for users who
        // never opt in to grid snap.
        assert_eq!(rt.editing.grid_snap_interval, klipp_core::GridInterval::Off);
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
        assert_eq!(rt.audio.default_fade_duration_ms, 500);
        assert_eq!(rt.audio.loudness_standard, klipp_core::LoudnessStandard::EbuR128);

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
        assert_eq!(rt.appearance.accent_color, [0x4a, 0x8f, 0xe7]);
        assert_eq!(rt.appearance.playhead_color, [0xff, 0x5a, 0x3c]);

        // MediaCachePrefs
        assert!(rt.media_cache.filmstrip_cache_dir.is_none());
        assert!(rt.media_cache.audio_cache_dir.is_none());
        assert_eq!(rt.media_cache.max_cache_size_gb, 20);
        assert_eq!(rt.media_cache.skim_transcode_quality, 30);
        assert_eq!(rt.media_cache.thumbnail_height, 480);

        // ColorPrefs
        assert!(rt.color.viewer_lut_path.is_none());
        assert_eq!(rt.color.scope_quality, klipp_core::ScopeQuality::Medium);
        assert_eq!(rt.color.scope_update_interval_ms, 32);

        // TitlePrefs
        assert!(rt.title.default_font.is_empty());
        assert_eq!(rt.title.default_size_pt, 72.0);
        assert!(rt.title.font_search_paths.is_empty());

        // TimelinePrefs
        assert_eq!(rt.timeline.clip_colors.len(), 6);
        assert_eq!(rt.timeline.clip_gap_px, 3.0);
        assert_eq!(rt.timeline.trim_handle_width, 14.0);
        assert_eq!(rt.timeline.overview_bar_height, 24.0);
        assert_eq!(rt.timeline.min_track_height, 24.0);
        assert_eq!(rt.timeline.max_track_height, 300.0);
        assert_eq!(rt.timeline.text_track_height, 32.0);
        assert!(rt.timeline.fit_after_assistant_edits, "on by default");

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
        assert_eq!(prefs.playback.skim_resolution, 1080); // default_skim_resolution
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
        assert_eq!(prefs.appearance.accent_color, [0x4a, 0x8f, 0xe7]);
        assert_eq!(prefs.media_cache.thumbnail_height, 480);
        assert_eq!(prefs.color.scope_update_interval_ms, 32);
        assert_eq!(prefs.title.default_size_pt, 72.0);
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
        prefs.playback.skim_resolution = 1080;

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
        prefs.editing.grid_snap_interval = klipp_core::GridInterval::Custom(0.25);
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
        assert_eq!(rt.playback.skim_resolution, 1080);

        assert!(!rt.editing.auto_create_tracks);
        assert!(!rt.editing.auto_generate_filmstrips);
        assert!(!rt.editing.auto_generate_skim);
        assert_eq!(rt.editing.default_still_duration_secs, 10.0);
        assert_eq!(rt.editing.undo_limit, 50);
        assert_eq!(rt.editing.default_transition_frames, 12);
        assert_eq!(rt.editing.default_video_track_height, 80.0);
        assert_eq!(rt.editing.default_audio_track_height, 60.0);
        assert_eq!(rt.editing.snap_threshold_px, 20.0);
        assert_eq!(rt.editing.grid_snap_interval, klipp_core::GridInterval::Custom(0.25));
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
        assert_eq!(prefs.appearance.accent_color, [0x4a, 0x8f, 0xe7]);
        assert_eq!(prefs.media_cache.max_cache_size_gb, 20);
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
    /// Both old literal defaults move up to Full. While the cap was a floor
    /// rather than a cap, a saved 480 or 720 still decoded at the source
    /// capped to 1080, so nobody was on the number they had saved; now that
    /// the cap is real, leaving it would halve their preview, because the
    /// reducer halves rather than scales. A number the user chose is theirs.
    fn migrate_carries_the_old_skim_defaults_up_to_full() {
        for old in [480u64, 720] {
            let mut value: serde_json::Value =
                serde_json::from_str(&format!(r#"{{ "playback": {{ "skim_resolution": {old} }} }}"#)).unwrap();
            Preferences::migrate_skim_resolution_default(&mut value);
            assert_eq!(
                value["playback"]["skim_resolution"].as_u64(),
                Some(1080),
                "{old} was a default nobody actually got, so it moves"
            );
        }
        // Half and Quarter are real choices on this reducer, and stay.
        for chosen in [540u64, 270, 1080] {
            let mut value: serde_json::Value =
                serde_json::from_str(&format!(r#"{{ "playback": {{ "skim_resolution": {chosen} }} }}"#)).unwrap();
            Preferences::migrate_skim_resolution_default(&mut value);
            assert_eq!(value["playback"]["skim_resolution"].as_u64(), Some(chosen), "a chosen height is left alone");
        }
    }

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
            "Colour".into(),
        ];
        let rt = round_trip(&prefs);
        assert_eq!(rt.general.collapsed_sections, vec!["Transform", "Effects", "Colour"]);
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
                sequence_count: Some(2),
                pinned: true,
            },
            RecentProject::from_path(PathBuf::from("/projects/b.klipp")),
        ];
        let rt = round_trip(&prefs);
        assert_eq!(rt.general.recent_projects.len(), 2);
        assert_eq!(rt.general.recent_projects[0].name, "Project A");
        assert_eq!(rt.general.recent_projects[0].duration_secs, Some(120.5));
        assert_eq!(rt.general.recent_projects[0].total_size_bytes, Some(1_000_000));
        assert_eq!(rt.general.recent_projects[0].sequence_count, Some(2));
        assert!(rt.general.recent_projects[0].pinned && !rt.general.recent_projects[1].pinned);
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

    // ── False color palette tests ──

    /// The built-in palettes read from Klipp's file as the code had them
    /// (#2887): their names, their stop counts, and each one's first and
    /// last stop whole.
    #[test]
    fn the_built_in_palettes_read_as_they_were() {
        let built = FalseColorPalette::built_ins();
        let names: Vec<&str> = built.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Broadcast Standard", "ARRI", "RED"]);
        assert_eq!(built.iter().map(|p| p.stops.len()).collect::<Vec<_>>(), [13, 13, 12]);
        let stop = |p: &FalseColorPalette, i: usize| { let s = &p.stops[i]; (s.luma_min, s.luma_max, s.color, s.label.clone()) };
        assert_eq!(stop(&built[0], 0), (0, 10, [0x40, 0x00, 0x80], "Black crush".to_string()));
        assert_eq!(stop(&built[0], 12), (241, 255, [0xff, 0x40, 0x80], "Clipped".to_string()));
        assert_eq!(stop(&built[1], 0), (0, 5, [0x10, 0x00, 0x20], "Black".to_string()));
        assert_eq!(stop(&built[1], 12), (251, 255, [0xff, 0x90, 0xd0], "Clipped".to_string()));
        assert_eq!(stop(&built[2], 0), (0, 8, [0x10, 0x10, 0x30], "Black".to_string()));
        assert_eq!(stop(&built[2], 11), (241, 255, [0xff, 0x60, 0x60], "Clipped".to_string()));
        assert_eq!(FalseColorPalette::arri(), built[1]);
        assert!(built.iter().all(|p| p.unmapped_count() == 0), "each covers 0-255");
    }

    /// A file dropped in a person's false colour folder lists its palette
    /// after Klipp's (#3095); a hostile one is refused, said, and Klipp's
    /// stay whole: a link, one past the size, an unknown field, the wrong
    /// kind, a built-in's name, no stops, a label past its length.
    #[test]
    fn a_person_s_palette_lists_after_klipp_s_and_a_hostile_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("klipp-user-palettes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |n: &str, body: &str| std::fs::write(dir.join(n), format!("kind = \"false_colour_palette\"\nformat_version = 1\n{body}")).unwrap();
        let palette = |name: &str, label: &str| format!("[[palette]]\nname = \"{name}\"\n[[palette.stop]]\nluma = [0, 255]\ncolor = [10, 20, 30]\nlabel = \"{label}\"\n");
        file("a.toml", &palette("Studio", "All"));
        file("b.toml", &(palette("Odd", "All") + "colour = 2\n"));
        std::fs::write(dir.join("c.toml"), "kind = \"generator_entry\"\nformat_version = 1\n").unwrap();
        file("d.toml", &format!("# {}\n", "x".repeat(klipp_core::preset_file::MAX_FILE_BYTES)));
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("a.toml"), dir.join("e.toml")).unwrap();
        file("f.toml", &(palette("arri", "All") + "[[palette]]\nname = \"Empty\"\nstop = []\n" + &palette("Wordy", &"x".repeat(65))));
        let (palettes, skipped) = palettes_in(std::slice::from_ref(&dir));
        let names: Vec<&str> = palettes.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Broadcast Standard", "ARRI", "RED", "Studio"]);
        assert_eq!(palettes[..3], FalseColorPalette::built_ins()[..], "Klipp's, whole and first");
        assert_eq!(palettes[3].stops, [FalseColorStop { luma_min: 0, luma_max: 255, color: [10, 20, 30], label: "All".into() }]);
        let said = |what: &str| assert!(skipped.iter().any(|s| s.contains(what)), "{what}: {skipped:?}");
        said("b.toml: false_colour_palette entry 1: unknown field");
        said("c.toml: a generator_entry file, not a false_colour_palette file");
        said("d.toml: ");
        #[cfg(unix)]
        said("e.toml: not a plain file");
        said("\"arri\" is the name of one of Klipp's own");
        said("\"Empty\": 1 to 256 stops");
        said("\"Wordy\": the stop");
        // A stop's luma and colour are 8-bit integers: a value that isn't
        // one is refused with its file, whole.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bad = ["luma = [nan, 255]", "luma = [0, inf]", "luma = [0, 256]", "luma = [-1, 255]", "luma = [0.5, 255]", "luma = [0, 255]\ncolor = [300, 0, 0]"];
        for (i, luma) in bad.iter().enumerate() {
            let colour = if luma.contains("color") { "" } else { "\ncolor = [1, 2, 3]" };
            file(&format!("bad{i}.toml"), &format!("[[palette]]\nname = \"Bad {i}\"\n[[palette.stop]]\n{luma}{colour}\nlabel = \"L\"\n"));
        }
        let (palettes, skipped) = palettes_in(std::slice::from_ref(&dir));
        assert_eq!(palettes, FalseColorPalette::built_ins(), "none of them read");
        assert_eq!(skipped.iter().filter(|s| s.starts_with("bad") && s.contains("false_colour_palette entry 1")).count(), bad.len(), "{skipped:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A palettes file with one that can't be offered is refused, saying
    /// why: a name twice or none, no stops, a band that runs backwards, a
    /// stop with no label, a luma past 255, an unknown field.
    #[test]
    fn a_bad_palettes_file_is_refused() {
        let file = |body: &str| format!("kind = \"false_colour_palette\"\nformat_version = 1\n{body}");
        let stop = "[[palette.stop]]\nluma = [0, 255]\ncolor = [1, 2, 3]\nlabel = \"All\"\n";
        assert!(read_palettes(&file(&format!("[[palette]]\nname = \"Flat\"\n{stop}"))).is_ok());
        for (body, says) in [
            (format!("[[palette]]\nname = \"A\"\n{stop}[[palette]]\nname = \"A\"\n{stop}"), "a name, said once"),
            (format!("[[palette]]\nname = \" \"\n{stop}"), "a name, said once"),
            ("[[palette]]\nname = \"Empty\"\nstop = []\n".to_string(), "1 to 256 stops"),
            ("[[palette]]\nname = \"Back\"\n[[palette.stop]]\nluma = [200, 100]\ncolor = [1, 2, 3]\nlabel = \"Back\"\n".to_string(), "runs from low luma to high"),
            ("[[palette]]\nname = \"Mute\"\n[[palette.stop]]\nluma = [0, 10]\ncolor = [1, 2, 3]\nlabel = \"\"\n".to_string(), "says what it is"),
            ("[[palette]]\nname = \"Over\"\n[[palette.stop]]\nluma = [0, 300]\ncolor = [1, 2, 3]\nlabel = \"Over\"\n".to_string(), ""),
            (format!("[[palette]]\nname = \"Extra\"\nglow = 1\n{stop}"), "unknown field"),
        ] {
            let why = read_palettes(&file(&body)).err().unwrap_or_default();
            assert!(!why.is_empty() && why.contains(says), "{body}: {why}");
        }
    }

    #[test]
    fn broadcast_lut_maps_known_values() {
        let lut = FalseColorPalette::broadcast().build_lut();
        // luma 0 → deep purple #400080
        assert_eq!(&lut[0..3], &[0x40, 0x00, 0x80]);
        // luma 128 → yellow-green #80c000 (116–135 range)
        assert_eq!(&lut[128 * 3..128 * 3 + 3], &[0x80, 0xc0, 0x00]);
        // luma 255 → pink-white #ff4080
        assert_eq!(&lut[255 * 3..255 * 3 + 3], &[0xff, 0x40, 0x80]);
    }

    #[test]
    fn partial_palette_leaves_gaps_as_passthrough() {
        let palette = FalseColorPalette {
            name: "Partial".into(),
            stops: vec![
                FalseColorStop { luma_min: 0, luma_max: 127, color: [0xff, 0x00, 0x00], label: "Low".into() },
            ],
        };
        let lut = palette.build_lut();
        // Covered: luma 64 → red
        assert_eq!(&lut[64 * 3..64 * 3 + 3], &[0xff, 0x00, 0x00]);
        // Uncovered: luma 200 → passthrough sentinel [0, 0, 0]
        assert_eq!(&lut[200 * 3..200 * 3 + 3], &[0, 0, 0]);
    }

    #[test]
    fn palette_round_trip_json() {
        let palette = FalseColorPalette::broadcast();
        let json = serde_json::to_string(&palette).expect("serialize");
        let restored: FalseColorPalette = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(palette, restored);
    }

    #[test]
    fn default_palette_has_13_stops() {
        let palette = default_false_color_palette();
        assert_eq!(palette.stops.len(), 13);
    }

    #[test]
    fn overlapping_stops_detected() {
        let palette = FalseColorPalette {
            name: "Test".into(),
            stops: vec![
                FalseColorStop { luma_min: 0, luma_max: 50, color: [0xff, 0, 0], label: "A".into() },
                FalseColorStop { luma_min: 40, luma_max: 100, color: [0, 0xff, 0], label: "B".into() },
                FalseColorStop { luma_min: 101, luma_max: 200, color: [0, 0, 0xff], label: "C".into() },
            ],
        };
        let overlaps = palette.overlapping_stops();
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0], (0, 1));
    }

    #[test]
    fn unmapped_count_with_gap() {
        let palette = FalseColorPalette {
            name: "Test".into(),
            stops: vec![
                FalseColorStop { luma_min: 0, luma_max: 99, color: [0xff, 0, 0], label: "A".into() },
                FalseColorStop { luma_min: 111, luma_max: 255, color: [0, 0xff, 0], label: "B".into() },
            ],
        };
        // Gap: 100–110 = 11 unmapped values
        assert_eq!(palette.unmapped_count(), 11);
    }

    #[test]
    fn sort_stops_by_luma() {
        let mut palette = FalseColorPalette {
            name: "Test".into(),
            stops: vec![
                FalseColorStop { luma_min: 100, luma_max: 200, color: [0, 0, 0xff], label: "C".into() },
                FalseColorStop { luma_min: 5, luma_max: 50, color: [0xff, 0, 0], label: "A".into() },
                FalseColorStop { luma_min: 51, luma_max: 99, color: [0, 0xff, 0], label: "B".into() },
            ],
        };
        palette.sort_stops();
        assert_eq!(palette.stops[0].label, "A");
        assert_eq!(palette.stops[1].label, "B");
        assert_eq!(palette.stops[2].label, "C");
    }

    #[test]
    fn viewer_prefs_with_palette_round_trip() {
        let mut prefs = Preferences::default();
        prefs.viewer.false_color_palette = FalseColorPalette::arri();
        let rt = round_trip(&prefs);
        assert_eq!(rt.viewer.false_color_palette.name, "ARRI");
        assert_eq!(rt.viewer.false_color_palette.stops.len(), 13);
    }

    // ── #62 / #63 / #65 — export pipeline features ─────────────────────────

    #[test]
    fn parallel_render_default_is_two() {
        // #62: safe default — 2 parallel jobs on every system.
        let p = ExportPrefs::default();
        assert_eq!(p.parallel_render_jobs, 2);
    }

    #[test]
    fn post_export_command_default_is_empty() {
        // #63 Tier-1: pref defaults to empty (no command run).
        let p = ExportPrefs::default();
        assert!(p.post_export_command.is_empty());
    }

    #[test]
    fn hdr_mode_default_is_sdr() {
        // #65: SDR is the default so existing presets are unchanged.
        assert_eq!(klipp_core::HdrMode::default(), klipp_core::HdrMode::Sdr);
    }

    #[test]
    fn export_settings_bit_depth_default_zero() {
        // #65: bit_depth == 0 preserves 8-bit / existing behavior across every
        // codec path.
        let s = klipp_core::ExportSettings::default();
        assert_eq!(s.bit_depth, 0);
        assert_eq!(s.hdr_mode, klipp_core::HdrMode::Sdr);
    }

    #[test]
    fn parallel_jobs_pref_round_trips_through_json() {
        let mut prefs = Preferences::default();
        prefs.export.parallel_render_jobs = 4;
        prefs.export.post_export_command = "echo {file}".into();
        let rt = round_trip(&prefs);
        assert_eq!(rt.export.parallel_render_jobs, 4);
        assert_eq!(rt.export.post_export_command, "echo {file}");
    }

    #[test]
    fn parallel_jobs_missing_uses_default() {
        // A config from before #62 has no `parallel_render_jobs` key —
        // serde must fall back to default_parallel_jobs().
        let json = r#"{
            "general": { "auto_save_interval_secs": 300, "recent_projects": [] },
            "playback": { "scrub_audio": true, "jkl_audio_mute_threshold": 4.0 },
            "export": { "custom_presets": [] },
            "proxy": { "auto_generate_on_import": true, "proxy_resolution": "Half" }
        }"#;
        let prefs: Preferences = serde_json::from_str(json).expect("deserialize");
        assert_eq!(prefs.export.parallel_render_jobs, 2);
        assert!(prefs.export.post_export_command.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════
    // PANIC-PREFS regression.
    //
    // The original tracker entry pointed at three migration sites
    // that used `value.get(...).unwrap()`, which would crash startup
    // if a hand-edited or stale prefs.json failed the implicit shape
    // check. The migrations have since been rewritten with `let
    // Some(...) else { return };` and `if let Some(...)` guards, and
    // load() falls back to defaults on every error path (file-read
    // error, JSON-parse error, schema-deserialize error). These
    // tests pin that contract via the new from_json_str entry point
    // so a future "tighten up the migrations" can't silently re-
    // introduce the panic.
    //
    // The hostile inputs deliberately stress every guard in the
    // migrations: malformed JSON, JSON that's the wrong root type,
    // objects with the wrong-shape `playback`/`media_cache`/`editing`
    // sections, schema mismatches at the typed-deserialize step.
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn from_json_str_recovers_from_garbage_input() {
        // Not JSON at all → defaults, no panic.
        let prefs = Preferences::from_json_str("this is not json {[(");
        assert_eq!(prefs.general.recent_projects.len(), 0,
            "garbage input must yield default prefs");
    }

    #[test]
    fn from_json_str_recovers_from_wrong_root_type() {
        // Valid JSON but the root is an array, not an object → the
        // typed deserialize fails → defaults.
        let prefs = Preferences::from_json_str("[1, 2, 3]");
        assert_eq!(prefs.general.recent_projects.len(), 0);
        let prefs2 = Preferences::from_json_str("\"a string\"");
        assert_eq!(prefs2.general.recent_projects.len(), 0);
        let prefs3 = Preferences::from_json_str("null");
        assert_eq!(prefs3.general.recent_projects.len(), 0);
    }

    #[test]
    fn from_json_str_recovers_when_migration_sections_have_wrong_type() {
        // The migrations' `let Some(...).as_object_mut() else { return };`
        // bails out cleanly when the expected sub-object is the wrong
        // type. The typed deserialize then either succeeds-with-
        // defaults or fails (both yield default prefs from
        // from_json_str) — both must NOT panic.
        let inputs = [
            r#"{ "playback": "not an object" }"#,
            r#"{ "playback": [1, 2, 3] }"#,
            r#"{ "media_cache": 42 }"#,
            r#"{ "editing": null }"#,
            r#"{ "playback": null, "media_cache": null, "editing": null, "file_browser": null }"#,
            // Wrong INNER types inside the migration sections.
            r#"{ "playback": { "skim_resolution": "string" } }"#,
            r#"{ "media_cache": { "filmstrip_height": [], "skim_transcode_quality": {} } }"#,
        ];
        for input in &inputs {
            let prefs = Preferences::from_json_str(input);
            // Don't care about the specific output, only that we
            // returned without panicking and got a usable struct.
            let _ = prefs.general.recent_projects.len();
        }
    }

    #[test]
    fn from_json_str_handles_empty_object_with_serde_defaults() {
        // `{}` should deserialize successfully because every top-
        // level field on Preferences has #[serde(default)]. Pinning
        // this so a future cleanup that drops a `default` annotation
        // (which would make us crash on missing fields) surfaces
        // here.
        let prefs = Preferences::from_json_str("{}");
        let _ = prefs.general.recent_projects.len();
    }

    #[test]
    fn from_json_str_loads_valid_preferences() {
        // Sanity: the happy path still works after the extraction.
        let prefs = Preferences::default();
        let json = serde_json::to_string(&prefs).expect("serialize defaults");
        let round_tripped = Preferences::from_json_str(&json);
        assert_eq!(
            round_tripped.general.auto_save_interval_secs,
            prefs.general.auto_save_interval_secs,
            "round-trip via from_json_str must preserve fields",
        );
    }
}

#[cfg(test)]
mod font_pick_tests {
    use super::*;

    /// A star toggles; a pick goes first among the recent ones, once,
    /// the list kept to its length; preferences from before read with
    /// none (#1719).
    #[test]
    fn stars_toggle_and_recent_fonts_stay_short_and_fresh() {
        let mut t = TitlePrefs::default();
        t.toggle_favourite_font("Inter");
        t.toggle_favourite_font("Bebas Neue");
        t.toggle_favourite_font("Inter");
        assert_eq!(t.favourite_fonts, ["Bebas Neue"]);
        for f in ["A", "B", "C", "A"] {
            t.note_recent_font(f);
        }
        assert_eq!(t.recent_fonts, ["A", "C", "B"]);
        for k in 0..20 {
            t.note_recent_font(&format!("F{k}"));
        }
        assert_eq!(t.recent_fonts.len(), MAX_RECENT_FONTS);
        assert_eq!(t.recent_fonts[0], "F19");
        let old: TitlePrefs = serde_json::from_str("{}").unwrap();
        assert!(old.favourite_fonts.is_empty() && old.recent_fonts.is_empty());
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;

    /// A file in its default state, as an older build wrote it.
    fn old_file(edit: impl FnOnce(&mut serde_json::Value)) -> Preferences {
        let mut v = serde_json::to_value(Preferences::default()).unwrap();
        v.as_object_mut().unwrap().remove("revision");
        edit(&mut v);
        Preferences::from_json_str(&v.to_string())
    }

    /// #1816: a file from before the fades acted, with the then-default
    /// 0 ms, gets the half-second default once; a file already at the
    /// revision keeps whatever it holds, so a value set later stands.
    #[test]
    fn an_old_zero_fade_becomes_the_default_once() {
        let p = old_file(|v| v["audio"]["default_fade_duration_ms"] = 0.into());
        assert_eq!(p.audio.default_fade_duration_ms, 500);
        assert_eq!(p.revision, PREFS_REVISION);
        let p = old_file(|v| v["audio"]["default_fade_duration_ms"] = 300.into());
        assert_eq!(p.audio.default_fade_duration_ms, 300, "a value chosen is kept");
        let mut current = serde_json::to_value(Preferences::default()).unwrap();
        current["audio"]["default_fade_duration_ms"] = 0.into();
        assert_eq!(Preferences::from_json_str(&current.to_string()).audio.default_fade_duration_ms, 0, "at the revision, nothing moves");
        assert_eq!(Preferences::default().revision, PREFS_REVISION, "a fresh install is at the revision");
    }

    /// The title size's old default, 48, never sized a title (they were
    /// 72): an old file's 48 becomes 72 once; another size is kept.
    #[test]
    fn an_old_default_title_size_becomes_the_titles_own() {
        assert_eq!(old_file(|v| v["title"]["default_size_pt"] = 48.0.into()).title.default_size_pt, 72.0);
        assert_eq!(old_file(|v| v["title"]["default_size_pt"] = 60.0.into()).title.default_size_pt, 60.0);
    }

    /// The max track height's old default, 200, capped nothing (tracks
    /// went to 300); a file from before revision 2 has it moved to 300
    /// once, so tracks resize as far as they did. A file at revision 1
    /// still gets this move; another height is kept.
    #[test]
    fn an_old_default_max_track_height_becomes_300() {
        assert_eq!(old_file(|v| v["timeline"]["max_track_height"] = 200.0.into()).timeline.max_track_height, 300.0);
        let at_1 = |h: f64| {
            let mut v = serde_json::to_value(Preferences::default()).unwrap();
            v["revision"] = 1.into();
            v["timeline"]["max_track_height"] = h.into();
            Preferences::from_json_str(&v.to_string()).timeline.max_track_height
        };
        assert_eq!(at_1(200.0), 300.0);
        assert_eq!(at_1(250.0), 250.0);
    }

    /// The thumbnail size sized nothing before it acted; an old file's
    /// value becomes the width thumbnails always had, once.
    #[test]
    fn an_old_thumbnail_size_becomes_the_width_they_had() {
        assert_eq!(old_file(|v| v["media_cache"]["thumbnail_height"] = 192.into()).media_cache.thumbnail_height, 480);
        let mut current = serde_json::to_value(Preferences::default()).unwrap();
        current["media_cache"]["thumbnail_height"] = 640.into();
        assert_eq!(Preferences::from_json_str(&current.to_string()).media_cache.thumbnail_height, 640);
    }
}
