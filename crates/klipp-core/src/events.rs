use crate::id::{BinId, ClipId, GroupId, MarkerId, MediaId, TrackId};
use crate::modifier::{Modifier, ParamId};
use crate::time::{Fps, RationalTime, Timecode};
use crate::types::Resolution;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which audio parameter a keyframe operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioParam {
    Volume,
    Pan,
    Gain,
}

/// Which video parameter a keyframe operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoParam {
    Opacity,
    PositionX,
    PositionY,
    Scale,
    Rotation,
}

/// HDR transfer characteristic for export. Writes matching color-space tags
/// into the output container (BT.2020 primaries + the selected transfer).
/// #65 baseline — full mastering-display metadata deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HdrMode {
    #[default]
    Sdr,
    /// Hybrid Log-Gamma (ARIB STD-B67).
    Hlg,
    /// Perceptual Quantizer / HDR10 (SMPTE ST 2084).
    Pq,
}

#[derive(Debug, Clone)]
pub struct ExportSettings {
    pub output_path: PathBuf,
    pub format: ExportFormat,
    pub video_codec: ExportVideoCodec,
    pub resolution: Option<Resolution>,
    pub fps: Option<Fps>,
    pub crf: u32,
    pub preset: EncoderPreset,
    pub audio_codec: ExportAudioCodec,
    pub audio_bitrate_kbps: Option<u32>,
    pub video_bitrate_kbps: Option<u32>,
    pub rate_control: RateControl,
    /// Burn subtitle/text track content into the video. Default true to preserve
    /// existing behavior (subtitle clips have always been composited as overlays).
    pub burn_in_subtitles: bool,
    /// Embed subtitles as a soft track in the output container.
    /// Uses format-appropriate codec: mov_text (MP4/MOV), subrip (MKV), webvtt (WebM).
    pub embed_subtitles: bool,
    /// Target integrated loudness in LUFS for EBU R128 normalization.
    /// None = no normalization (pass audio through unchanged).
    pub target_lufs: Option<f32>,
    /// Embed sequence markers as chapter metadata in the output container.
    /// Only honoured for MP4/MKV/MOV — WebM has no chapter concept.
    pub embed_chapters: bool,
    /// Requested encoder bit depth. `0` = codec default (8-bit); `10` enables
    /// 10-bit encode for H.264/H.265. Other codecs ignore this (ProRes is
    /// always 10-bit, DNxHD is 8-bit). #65 baseline.
    pub bit_depth: u8,
    /// HDR transfer characteristic. Defaults to SDR (BT.709). When 10-bit
    /// encode is active, HLG/PQ write BT.2020 primaries + matching transfer.
    pub hdr_mode: HdrMode,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            output_path: PathBuf::new(),
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            resolution: None,
            fps: None,
            crf: 23,
            preset: EncoderPreset::default(),
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(192),
            video_bitrate_kbps: None,
            rate_control: RateControl::default(),
            burn_in_subtitles: true,
            embed_subtitles: false,
            target_lufs: None,
            embed_chapters: false,
            bit_depth: 0,
            hdr_mode: HdrMode::default(),
        }
    }
}

/// A single chapter entry frozen at export time. Timebase is milliseconds
/// (start_ms/end_ms). `title` is the marker label; empty is allowed and the
/// player displays a default label.
#[derive(Debug, Clone)]
pub struct ChapterSnapshot {
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum EncoderPreset {
    Fast,
    #[default]
    Medium,
    Slow,
    VerySlow,
}

impl EncoderPreset {
    pub fn ffmpeg_value(&self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Medium => "medium",
            Self::Slow => "slow",
            Self::VerySlow => "veryslow",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Medium => "Medium",
            Self::Slow => "Slow",
            Self::VerySlow => "Very Slow",
        }
    }

    pub fn all() -> &'static [EncoderPreset] {
        &[Self::Fast, Self::Medium, Self::Slow, Self::VerySlow]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportFormat {
    Mp4,
    Mkv,
    Mov,
    WebM,
}

impl ExportFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Mov => "mov",
            Self::WebM => "webm",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mkv => "MKV",
            Self::Mov => "MOV",
            Self::WebM => "WebM",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportVideoCodec {
    H264,
    H265,
    ProRes422,
    ProRes422HQ,
    ProRes4444,
    DNxHD115,
    DNxHD145,
    Av1,
    Vp9,
}

impl ExportVideoCodec {
    pub fn ffmpeg_codec(&self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 => "prores_ks",
            Self::DNxHD115 | Self::DNxHD145 => "dnxhd",
            Self::Av1 => "libsvtav1",
            Self::Vp9 => "libvpx-vp9",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
            Self::ProRes422 => "ProRes 422",
            Self::ProRes422HQ => "ProRes 422 HQ",
            Self::ProRes4444 => "ProRes 4444",
            Self::DNxHD115 => "DNxHD 115 Mbps",
            Self::DNxHD145 => "DNxHD 145 Mbps",
            Self::Av1 => "AV1",
            Self::Vp9 => "VP9",
        }
    }

    pub fn hint(&self) -> &'static str {
        match self {
            Self::H264 => "GPU accelerated \u{26A1} — fast, universal",
            Self::H265 => "GPU accelerated \u{26A1} — best quality/size",
            Self::ProRes422 => "CPU only — NLE interchange, large files",
            Self::ProRes422HQ => "CPU only — high quality NLE interchange",
            Self::ProRes4444 => "CPU only — alpha channel, mastering",
            Self::DNxHD115 => "CPU only — Avid interchange",
            Self::DNxHD145 => "CPU only — Avid interchange, high bitrate",
            Self::Av1 => "CPU — next-gen, best compression, slow encode",
            Self::Vp9 => "CPU — web video (YouTube, WebM)",
        }
    }

    pub fn uses_crf(&self) -> bool {
        matches!(self, Self::H264 | Self::H265 | Self::Av1 | Self::Vp9)
    }

    pub fn supports_preset(&self) -> bool {
        matches!(self, Self::H264 | Self::H265 | Self::Av1)
    }

    pub fn requires_mov(&self) -> bool {
        matches!(
            self,
            Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 | Self::DNxHD115 | Self::DNxHD145
        )
    }

    /// Whether this codec requires a WebM container.
    pub fn requires_webm(&self) -> bool {
        matches!(self, Self::Vp9)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportAudioCodec {
    Aac,
    Mp3,
    Flac,
    Opus,
    Pcm16,
    Pcm24,
    Copy,
}

impl ExportAudioCodec {
    pub fn ffmpeg_args(&self, bitrate_kbps: Option<u32>) -> Vec<String> {
        match self {
            Self::Aac => {
                let mut args = vec!["-c:a".into(), "aac".into()];
                if let Some(br) = bitrate_kbps {
                    args.push("-b:a".into());
                    args.push(format!("{br}k"));
                }
                args
            }
            Self::Mp3 => {
                let mut args = vec!["-c:a".into(), "libmp3lame".into()];
                if let Some(br) = bitrate_kbps {
                    args.push("-b:a".into());
                    args.push(format!("{br}k"));
                }
                args
            }
            Self::Opus => {
                let mut args = vec!["-c:a".into(), "libopus".into()];
                if let Some(br) = bitrate_kbps {
                    args.push("-b:a".into());
                    args.push(format!("{br}k"));
                }
                args
            }
            Self::Flac => vec!["-c:a".into(), "flac".into()],
            Self::Pcm16 => vec!["-c:a".into(), "pcm_s16le".into()],
            Self::Pcm24 => vec!["-c:a".into(), "pcm_s24le".into()],
            Self::Copy => vec!["-c:a".into(), "copy".into()],
        }
    }

    pub fn ffmpeg_codec(&self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Mp3 => "libmp3lame",
            Self::Flac => "flac",
            Self::Opus => "libopus",
            Self::Pcm16 => "pcm_s16le",
            Self::Pcm24 => "pcm_s24le",
            Self::Copy => "pcm_s16le",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Aac => "AAC",
            Self::Mp3 => "MP3",
            Self::Flac => "FLAC (lossless)",
            Self::Opus => "Opus",
            Self::Pcm16 => "PCM 16-bit (WAV)",
            Self::Pcm24 => "PCM 24-bit (WAV)",
            Self::Copy => "Copy Source",
        }
    }

    /// Whether this codec supports variable bitrate.
    pub fn uses_bitrate(&self) -> bool {
        matches!(self, Self::Aac | Self::Mp3 | Self::Opus)
    }
}

/// Video rate control mode for export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RateControl {
    /// Constant Rate Factor — quality-based (default).
    #[default]
    Crf,
    /// Constant Bitrate.
    Cbr,
    /// Variable Bitrate (target average).
    Vbr,
}

impl RateControl {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Crf => "CRF (Quality)",
            Self::Cbr => "CBR (Constant Bitrate)",
            Self::Vbr => "VBR (Variable Bitrate)",
        }
    }
}

#[derive(Debug, Clone)]
pub enum AppCommand {
    // Transport
    Play,
    Pause,
    Stop,
    SeekTo(Timecode),
    SeekRelative(i64),
    SetPlayheadFromTimecode { input: String },
    RequestWhiteBalanceSample { nx: f32, ny: f32 },
    RequestQualifierSample { nx: f32, ny: f32 },
    BeginTitleEdit { clip_id: ClipId },
    EndTitleEdit,

    // Media
    LoadMediaFile { path: PathBuf, media_id: MediaId },
    ImportMedia { path: PathBuf },
    /// Recursively import all media files from a directory.
    ImportMediaFolder { directory: PathBuf },

    // Loop
    ToggleLoopPlayback,
    SetLoopPlayback(bool),

    /// Sync the user's "play audio during playhead scrub" preference into the
    /// engine so mute/chirp decisions can honour it.
    SetScrubAudioEnabled(bool),

    // Timeline
    SetPlayhead(Timecode),
    SetSpeed(f64),
    AppendClip { media_id: MediaId },
    AppendClipToTrack { media_id: MediaId, track_index: usize },
    RemoveClip { clip_id: ClipId },
    MoveClip { clip_id: ClipId, new_timeline_in: Timecode },
    MoveClipToTrack { clip_id: ClipId, new_timeline_in: Timecode, from_track: usize, to_track: usize },
    TrimClip {
        clip_id: ClipId,
        old_source_in: Timecode,
        old_source_out: Timecode,
        old_timeline_in: Timecode,
        old_timeline_out: Timecode,
        new_source_in: Timecode,
        new_source_out: Timecode,
        new_timeline_in: Timecode,
        new_timeline_out: Timecode,
        ripple: bool,
    },
    /// Roll trim: adjust the boundary between two adjacent clips.
    /// Clip A's out moves and Clip B's in moves by the same amount, keeping total duration constant.
    RollTrimClip {
        clip_a_id: ClipId,
        clip_b_id: ClipId,
        new_a_source_out: Timecode,
        new_a_timeline_out: Timecode,
        new_b_source_in: Timecode,
        new_b_timeline_in: Timecode,
    },
    SelectClip(Option<ClipId>),

    // Project
    NewProject,
    CloseProject,
    /// Wipe all engine-side state (decoders, caches, frame buffer) so the next
    /// project starts from a clean slate. Sent by the UI on new/close project.
    ResetEngine,
    SaveProject,
    SaveProjectAs { path: PathBuf },
    OpenProject { path: PathBuf },
    CreateProject {
        name: String,
        location: PathBuf,
        fps_num: u32,
        fps_den: u32,
        width: u32,
        height: u32,
        sequence_name: String,
    },
    RemoveRecentProject { path: PathBuf },
    DeleteProject { path: PathBuf },
    RenameProjectOnDisk { old_path: PathBuf, new_name: String },
    /// Set project thumbnail from the current viewer frame.
    SetProjectThumbnailFromViewer,
    /// Set project thumbnail from an external image file.
    SetProjectThumbnailFromFile { path: PathBuf },

    // Sequences
    AddSequence { name: String, fps_num: u32, fps_den: u32, width: u32, height: u32 },
    DuplicateSequence { sequence_idx: usize, new_name: String },
    SetActiveSequence { sequence_idx: usize },
    RenameSequence { sequence_idx: usize, new_name: String },
    DeleteSequence { sequence_idx: usize },
    ReorderSequences { from_idx: usize, to_idx: usize },

    // Workspace
    CreateWorkspace { path: PathBuf, name: String },

    // Edit
    Undo,
    Redo,
    SequenceUpdated,
    SetAntiClickFade(bool),

    // Export
    ShowExportDialog,
    ExportSequence { settings: ExportSettings },
    CancelExport,
    /// Delete a user-defined export preset by id.
    DeleteExportPreset { preset_id: String },
    /// Rename a user-defined export preset.
    RenameExportPreset { preset_id: String, new_name: String },
    /// Clone an existing preset (user or built-in) into a new
    /// user-preset entry with "(copy)" appended to the name.
    DuplicateExportPreset { preset_id: String },

    // Clip properties dialog (#47)
    /// Open the Clip Properties modal focused on the given media item.
    OpenClipProperties { media_id: MediaId },

    // Software jog wheel (#38, scoped — HID support is deferred)
    /// Step the playhead by N frames (positive = forward, negative =
    /// backward). Emitted by the transport-bar jog wheel widget.
    JogFrames { frames: i32 },

    // Workspace layout presets (#39)
    /// Apply one of the 5 built-in workspace layouts by index into
    /// `builtin_workspace_layouts()`. Writes visibility flags + sizes.
    ApplyBuiltinWorkspaceLayout { index: usize },
    /// Apply a user-saved workspace layout by its `name`.
    ApplyCustomWorkspaceLayout { name: String },
    /// Remove a user-saved workspace layout by name (built-ins are never
    /// removable — checked in the handler).
    DeleteWorkspaceLayout { name: String },
    /// Rename a user-saved workspace layout. No-op if `new_name` is empty
    /// or if a layout with that name already exists.
    RenameWorkspaceLayout { old_name: String, new_name: String },

    // Compositing
    SetTrackOpacity { track_index: usize, opacity: f32 },
    SetTrackVisibility { track_index: usize, visible: bool },

    // Audio mixing
    SetTrackVolume { track_index: usize, volume: f32 },
    SetTrackMute { track_index: usize, muted: bool },
    SetTrackSolo { track_index: usize, solo: bool },
    SetTrackPan { track_index: usize, pan: f32 },
    SetMasterVolume(f32),
    SetMasterMute(bool),
    /// Reset the BS.1770-4 LUFS meter in the mix thread.
    ResetLufsMeter,
    SetClipVolume(f32),
    SetClipPan(f32),
    SetClipGain(f32),
    SetClipChannelConfig(u32),
    /// Switch the timeline rubber band to show a specific audio param.
    SetRubberBandParam(AudioParam),
    /// Switch the timeline rubber band to show a specific video param.
    SetVideoRubberBandParam(VideoParam),

    // Editing
    BladeClip { track_index: usize, clip_id: ClipId, cut_at: RationalTime },
    CutClip { clip_id: ClipId },
    CopyClip { clip_id: ClipId },
    PasteClip,
    DuplicateClip { clip_id: ClipId },
    /// Copy attributes from a clip for later paste-attributes.
    CopyAttributes { clip_id: ClipId },
    /// Paste previously copied attributes onto the selected clip(s).
    PasteAttributes {
        effects: bool,
        color_grade: bool,
        audio: bool,
        transform: bool,
        speed: bool,
    },
    /// Toggle a clip's enabled state (disabled clips are dimmed and skipped in render).
    SetClipEnabled { clip_id: ClipId, enabled: bool },
    /// Solo a clip — mute all other clips on the same track.
    SetClipSolo { clip_id: ClipId, solo: bool },
    SelectAllClips,
    /// Select all clips that start after the playhead.
    SelectClipsForward,
    /// Select all clips that end before or at the playhead.
    SelectClipsBackward,
    /// Save current viewer frame as PNG via a file dialog.
    FrameGrab,
    /// Save the current viewer frame as a PNG under `<project>/media/stills/`
    /// (or a temp directory when the project is unsaved) and import it into
    /// the media bin as a still MediaItem. No file dialog.
    ExtractFrameToBin,
    /// Import SRT/VTT subtitle file onto a subtitle track.
    ImportSubtitles { path: PathBuf },
    /// Import a Kdenlive MLT XML project as a new sequence.
    /// Unsupported features (filters, non-cross-dissolve transitions, keyframes)
    /// are logged to a `<name>.import_warnings.txt` file alongside the source.
    ImportKdenlive { path: PathBuf },
    /// Parse an OpenTimelineIO `.otio` JSON file and import its timeline as
    /// a new Sequence. Unsupported features (transitions, effects, nested
    /// stacks) are logged to a `<name>.import_warnings.txt` file alongside
    /// the source.
    ImportOtio { path: PathBuf },
    /// Copy the current project and all referenced media into a new
    /// self-contained directory at `dest_dir`. Source project is untouched.
    /// Runs on a background thread — see `ConsolidateDone` for completion.
    ConsolidateProject {
        dest_dir: PathBuf,
        include_proxies: bool,
        include_skims: bool,
        /// After copying, zip the consolidated folder into `<dest_dir>.zip`.
        create_zip: bool,
        /// After a successful zip, remove the consolidated folder. Ignored
        /// when `create_zip` is false.
        delete_folder_after_zip: bool,
    },
    /// Create a new empty subtitle clip at the given timeline position on
    /// the first Subtitle/Text track (creates a Subtitle track if none exists).
    /// Used by the subtitle editor panel's "Add at playhead" button.
    AddSubtitleAtPlayhead { timeline_in: Timecode, duration_frames: u32 },
    /// Export subtitles from subtitle tracks as SRT.
    ExportSubtitles { path: PathBuf },
    /// Export sequence as FCPXML 1.9 for Final Cut Pro interchange.
    ExportFcpxml { path: PathBuf },
    /// Export sequence as MLT XML (.kdenlive) for Kdenlive interchange (cuts-only v1).
    ExportKdenlive { path: PathBuf },
    /// Export sequence as CMX 3600 EDL for broad NLE interchange.
    ExportEdl { path: PathBuf },
    /// Export sequence markers as YouTube-compatible chapter text
    /// ("H:MM:SS Label" per line, with a synthetic "Intro" at 0:00 when
    /// the first marker is after the start).
    ExportYouTubeChapters { path: PathBuf },
    /// Export sequence markers as a DVDStyler / dvdauthor chapter list
    /// (one `HH:MM:SS.mmm` per line, labels dropped).
    ExportDvdChapters { path: PathBuf },
    /// Generate a headless bash render script that calls ffmpeg to render
    /// the current sequence. Cuts only — effects/grades/transitions are NOT
    /// serialized. The script includes a prominent WARNING comment advising
    /// users to Ctrl+E for full-quality export.
    GenerateRenderScript { path: PathBuf },
    /// Create a compound clip (nested sequence) from selected clips.
    CreateCompoundClip { name: String },
    /// Create a multicam clip from clips on different tracks at the playhead.
    CreateMulticamClip,
    /// Switch multicam angle (1-indexed).
    SwitchMulticamAngle { angle: usize },
    /// Set the audio mixer's pan law.
    SetPanLaw(crate::config::PanLaw),
    /// Zoom timeline to fit all clips.
    FitTimelineToWindow,
    /// Set timeline zoom (pixels-per-second), optionally with a new scroll_x in
    /// the same step for cursor-anchored zoom. Clamped to (0.1, 2000.0).
    SetTimelineZoom {
        zoom: f32,
        scroll_x: Option<f32>,
    },
    /// Fit the timeline to the visible content width. Preferred over
    /// `FitTimelineToWindow` for zoom-button / fit-button call sites.
    ZoomTimelineToFit,
    /// Apply a motion preset (Ken Burns / PiP) to selected clip.
    ApplyMotionPreset { clip_id: ClipId, preset: String },
    /// Create an adjustment layer at the playhead on the top video track.
    CreateAdjustmentLayer,
    /// Create a generator clip on a video track at the playhead.
    CreateGeneratorClip { track_index: Option<usize>, data: crate::GeneratorData },
    /// Update a generator clip's data (color, gradient, etc.).
    UpdateGeneratorData { clip_id: ClipId, data: crate::GeneratorData },
    /// Capture the current viewer frame and insert a freeze-frame generator
    /// clip at the playhead. Blades any clip under the playhead so the freeze
    /// lands cleanly between the two halves.
    InsertFreezeFrame { duration_secs: f64 },
    /// Open (drill into) a compound clip for editing.
    OpenCompoundClip { clip_id: ClipId },
    /// Navigate back from a nested sequence to the parent.
    CloseCompoundClip,
    /// Decompose a compound clip — replace it on the parent timeline with the
    /// inner sequence's clips at their original positions, then delete the
    /// now-orphaned inner sequence. Inverse of `CreateCompoundClip`.
    DecomposeCompoundClip { clip_id: ClipId },
    /// Navigate to next edit point (clip boundary) on the timeline.
    NextEditPoint,
    /// Navigate to previous edit point (clip boundary) on the timeline.
    PrevEditPoint,
    /// Close the gap (empty space) at the playhead on a given track, or the nearest gap.
    CloseGap { track_index: usize, gap_start: RationalTime, gap_end: RationalTime },

    // Source monitor
    LoadSourceMedia { media_id: MediaId },
    SetSourceInPoint(RationalTime),
    SetSourceOutPoint(RationalTime),
    InsertSourceAtPlayhead,
    /// Create a subclip from the source media's current IN/OUT zone. The new
    /// MediaItem inherits codec/fps/resolution from the parent, reuses the
    /// parent's path, and carries its own default_source_in/out matching the
    /// zone. `name: None` → auto-generated as "{parent_stem}_sub{N}".
    CreateSubclipFromZone { source_media_id: MediaId, name: Option<String> },

    // Sequence markers
    SetSequenceInPoint(RationalTime),
    SetSequenceOutPoint(RationalTime),
    ClearSequenceInPoint,
    ClearSequenceOutPoint,

    // Proxy
    GenerateProxy { media_id: MediaId },
    CancelProxy { media_id: MediaId },
    SetUseProxies(bool),
    /// Begin timeline scrubbing — engine may switch to proxy decode for speed.
    /// Enable or disable the smart render cache.
    SetRenderCacheEnabled(bool),
    /// Scan cache directories and remove files not referenced by any media in the pool.
    CleanOrphanedCaches,
    /// Cache Manager (feature #78): clear a single cache category by wiping
    /// its directory then recreating the empty folder so later writes don't
    /// fail. Followed by an implicit refresh of the cache-manager snapshot.
    ClearCacheCategory { path: PathBuf },
    /// Re-scan all cache categories and update `AppState::cache_categories`.
    RefreshCacheManager,
    /// Open the Cache Manager modal. Dispatched from Preferences → Proxy.
    /// Handled by flipping `AppState::show_cache_manager` to true after
    /// calling `refresh_cache_categories`.
    OpenCacheManager,
    BeginScrub,
    /// End timeline scrubbing — engine restores full-res decode.
    EndScrub,
    /// Relink a specific offline media item to a new path.
    RelinkMedia { media_id: MediaId, new_path: PathBuf },
    /// Relink all offline media by searching a directory recursively.
    RelinkAllFromDirectory { directory: PathBuf },
    /// Show the media relink dialog.
    ShowRelinkDialog,
    AddMediaTag { media_id: MediaId, tag: String },
    RemoveMediaTag { media_id: MediaId, tag_index: usize },

    // Node graph (color grading)
    AddNode { clip_id: ClipId, kind: String, position: (f32, f32) },
    RemoveNode { clip_id: ClipId, node_id: uuid::Uuid },
    ConnectNodes { clip_id: ClipId, from: uuid::Uuid, from_port: u8, to: uuid::Uuid, to_port: u8 },
    DisconnectEdge { clip_id: ClipId, to: uuid::Uuid, to_port: u8 },
    MoveNode { clip_id: ClipId, node_id: uuid::Uuid, delta_x: f32, delta_y: f32 },
    ToggleNodeEnabled { clip_id: ClipId, node_id: uuid::Uuid },
    RenameNode { clip_id: ClipId, node_id: uuid::Uuid, label: String },
    SetScopeTapNode { clip_id: ClipId, node_id: Option<uuid::Uuid> },
    /// Set which scope mode the engine should render (0=waveform, 1=parade, 3=histogram).
    SetScopeRenderMode { mode: u8 },
    SetNodeParam { clip_id: ClipId, node_id: uuid::Uuid, param: String, value: f32 },
    /// Called when the user starts dragging a color-grading parameter
    /// (color wheel, sat/bri slider, scrubby slider, etc.). Snapshots the
    /// current node graph so the gesture can be committed as one undo entry.
    BeginNodeParamEdit { clip_id: ClipId },
    /// Called when the user releases a color-grading parameter drag. Commits
    /// the snapshot taken at the matching BeginNodeParamEdit as a single
    /// `SetNodeGraph` history entry covering the whole gesture.
    EndNodeParamEdit { clip_id: ClipId },
    SetNodeCurves { clip_id: ClipId, node_id: uuid::Uuid, curves: crate::color_grade::CurveSet },
    SetNodeHueSatCurves { clip_id: ClipId, node_id: uuid::Uuid, huesat: crate::color_grade::HueSatCurves },
    SetNodeQualifier { clip_id: ClipId, node_id: uuid::Uuid, qualifier: crate::color_grade::QualifierSettings },
    SetNodeMasks { clip_id: ClipId, node_id: uuid::Uuid, masks: crate::color_grade::MaskStack },
    /// Add or update a keyframe on a mask parameter.
    AddMaskKeyframe { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, param: String, time_secs: f64, value: f32 },
    /// Remove a mask keyframe at a specific time.
    RemoveMaskKeyframe { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, param: String, time_secs: f64 },
    /// Clear all keyframes for a mask parameter.
    ClearMaskKeyframes { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, param: String },
    AddKeyframe { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, value: f32 },
    RemoveKeyframe { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64 },
    ClearKeyframes { clip_id: ClipId, node_id: uuid::Uuid, param: String },
    SetKeyframeInterpolation { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, interpolation: crate::keyframe::Interpolation },
    SetKeyframeTangents { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, tangent_in: (f64, f32), tangent_out: (f64, f32) },
    /// Trigger a grade refresh without changing any params (used after bulk preset apply).
    RefreshGrade,
    ToggleColorPanel,
    ShowEffectsBrowser,
    ToggleBeforeAfter,
    /// Toggle stabilization compare split view (raw vs stabilized).
    /// Mutually exclusive with ToggleBeforeAfter and grade still compare.
    ToggleStabilizationCompare,
    /// Activate / deactivate per-effect split compare. `clip_id = None`
    /// deactivates and clears the compare state on both sides. `is_ofx`
    /// tells the engine which effect Vec on the clip to target.
    SetSplitCompareEffect {
        clip_id: Option<ClipId>,
        effect_index: usize,
        is_ofx: bool,
    },
    SetMattePreview(bool),
    SetClipLogFormat { clip_id: ClipId, log_format: crate::types::LogFormat },
    /// Auto white balance: analyze current frame and set temperature/tint on the clip's ColorBalance node.
    AutoWhiteBalance { clip_id: ClipId },

    // Global LUT
    LoadGlobalLut { path: PathBuf },
    ClearGlobalLut,
    SetGlobalLutOpacity(f32),
    SetGlobalLutEnabled(bool),
    /// Switch LUT interpolation: true = tetrahedral, false = trilinear.
    SetLutTetrahedral(bool),

    // Effects (OFX)
    AddEffect { clip_id: ClipId, plugin_name: String },
    RemoveEffect { clip_id: ClipId, effect_index: usize },
    SetEffectEnabled { clip_id: ClipId, effect_index: usize, enabled: bool },
    ReorderEffect { clip_id: ClipId, from_index: usize, to_index: usize },
    SetEffectParam { clip_id: ClipId, effect_index: usize, param_name: String, value: crate::OfxValue },

    // Built-in effects
    AddBuiltinEffect { clip_id: ClipId, effect: klipp_effects::BuiltinEffect },
    RemoveBuiltinEffect { clip_id: ClipId, index: usize },
    ToggleBuiltinEffectEnabled { clip_id: ClipId, index: usize },
    UpdateBuiltinEffect { clip_id: ClipId, index: usize, effect: klipp_effects::BuiltinEffect },
    ReorderBuiltinEffect { clip_id: ClipId, from_index: usize, to_index: usize },

    // Effect presets
    SaveBuiltinEffectPreset { name: String, description: String, effect_key: String, effect: klipp_effects::BuiltinEffect },
    DeleteBuiltinEffectPreset { effect_key: String, name: String },

    // Render Queue
    ShowRenderQueue,
    ClearCompletedJobs,

    // Slip & Slide trim
    SlipClip {
        clip_id: ClipId,
        new_source_in: Timecode,
        new_source_out: Timecode,
    },
    SlideClip {
        clip_id: ClipId,
        new_timeline_in: Timecode,
        new_timeline_out: Timecode,
        left_clip_id: Option<ClipId>,
        new_left_out: Option<Timecode>,
        right_clip_id: Option<ClipId>,
        new_right_in: Option<Timecode>,
    },

    // Snap & Ripple
    SetSnapEnabled(bool),
    SetRippleEnabled(bool),

    // Filmstrip
    /// Request more filmstrip detail for a time range (progressive refinement on zoom)
    RefineFilmstrip {
        media_id: MediaId,
        start_secs: f64,
        end_secs: f64,
        interval_secs: f64,
    },

    // Three-point edits
    InsertEdit,
    OverwriteEdit,
    LiftEdit,
    ExtractEdit,

    // Transitions
    AddTransition { track_index: usize, clip_a_id: ClipId, clip_b_id: ClipId, kind_id: u8, duration_frames: u32 },
    RemoveTransition { track_index: usize, transition_id: crate::id::TransitionId },
    SetTransitionDuration { track_index: usize, transition_id: crate::id::TransitionId, duration_frames: u32 },
    SetTransitionKind { track_index: usize, transition_id: crate::id::TransitionId, kind_id: u8 },
    SetTransitionParam { track_index: usize, transition_id: crate::id::TransitionId, user_params: crate::types::TransitionUserParams },
    SetTransitionAlignment { track_index: usize, transition_id: crate::id::TransitionId, alignment: crate::types::TransitionAlignment },

    // Title clips
    CreateTitleClip { track_index: usize, timeline_in: Timecode, duration_frames: u32 },
    UpdateTitleData { clip_id: ClipId, text: String, font_size: f32, font_name: String, font_style: u8, color: [u8; 4] },
    SetTitleAlignment { clip_id: ClipId, alignment: u8 },  // 0=Left, 1=Center, 2=Right
    SetTitlePosition { clip_id: ClipId, x: f32, y: f32 },
    SetTitleBackground { clip_id: ClipId, bg_kind: u8, color: [u8; 4] },  // 0=None, 1=Color, 2=FullFrame
    SetTitleShadow { clip_id: ClipId, enabled: bool, color: [u8; 4], offset_x: f32, offset_y: f32, blur: f32 },
    /// Set text outline. `None` = disable.
    SetTitleOutline { clip_id: ClipId, enabled: bool, color: [u8; 4], width: f32 },
    /// Set text glow. `enabled=false` = disable.
    SetTitleGlow { clip_id: ClipId, enabled: bool, color: [u8; 4], radius: f32, intensity: f32 },
    /// Set gradient fill. `enabled=false` = disable, uses solid color instead.
    SetTitleGradient { clip_id: ClipId, enabled: bool, top_color: [u8; 4], bottom_color: [u8; 4] },
    /// Set multiple outline layers (replaces single outline).
    /// Each tuple is (color_rgba, width_px).
    SetTitleOutlines { clip_id: ClipId, outlines: Vec<([u8; 4], f32)> },
    /// Set inner shadow. `enabled=false` = disable.
    SetTitleInnerShadow { clip_id: ClipId, enabled: bool, color: [u8; 4], offset_x: f32, offset_y: f32, blur: f32 },
    /// Set pattern fill. pattern_kind: 0=None, 1=HStripes, 2=VStripes, 3=Checker, 4=Diagonal
    SetTitlePattern { clip_id: ClipId, pattern_kind: u8, color_a: [u8; 4], color_b: [u8; 4], size: f32 },
    /// Set per-character tracking (extra spacing in px).
    SetTitleTracking { clip_id: ClipId, tracking: f32 },
    /// Set write-on reveal progress (0.0-1.0).
    SetTitleWriteOn { clip_id: ClipId, write_on: f32 },
    /// Set line spacing multiplier.
    SetTitleLineSpacing { clip_id: ClipId, line_spacing: f32 },
    /// Set 3D text extrusion. `enabled=false` = disable.
    SetTitleExtrusion { clip_id: ClipId, enabled: bool, depth: f32, angle: f32, face_color: [u8; 4], side_color: [u8; 4], lighting: f32 },
    /// Set text path. path_kind: 0=None, 1=Arc, 2=Circle, 3=Wave.
    /// For Arc: p1=radius, p2=start_angle, p3=sweep
    /// For Circle: p1=radius
    /// For Wave: p1=amplitude, p2=frequency, p3=phase
    SetTitleTextPath { clip_id: ClipId, path_kind: u8, p1: f32, p2: f32, p3: f32 },
    /// Set per-character animation. entrance: 0=None, 1=FadeIn, 2=SlideUp, 3=SlideDown, 4=ScaleUp.
    /// Set enabled=false to disable entirely.
    SetTitleCharAnimation { clip_id: ClipId, enabled: bool, stagger: f32, entrance: u8, wave_amplitude: f32, wave_speed: f32 },
    /// Set image texture fill path. `path: None` = disable texture fill.
    SetTitleTextureFill { clip_id: ClipId, path: Option<String> },
    /// Set motion blur. `enabled=false` = disable.
    SetTitleMotionBlur { clip_id: ClipId, enabled: bool, shutter_angle: f32, samples: u8 },
    /// Apply a style property to a character range (rich text style runs).
    /// `property`: "font_size", "color", "font_style", "tracking", "baseline_offset"
    /// `value`: JSON-encoded value for the property.
    ApplyTitleStyle {
        clip_id: ClipId,
        range_start: usize,
        range_end: usize,
        property: String,
        value: String,
    },
    /// Clear all style runs — reset to uniform base style.
    ClearTitleStyleRuns { clip_id: ClipId },
    /// Update the title animation fade duration stored in AppState.
    SetTitleFadeDuration { duration: f32 },
    ClearTitleOverlay,

    // Linked clips
    ImportMediaLinked { media_id: MediaId },
    UnlinkClip { clip_id: ClipId },
    LinkClips { clip_a: ClipId, clip_b: ClipId },

    // Markers
    AddMarker { position: Timecode, label: String, color: MarkerColor },
    RemoveMarker { marker_id: MarkerId },
    UpdateMarkerLabel { marker_id: MarkerId, label: String },
    UpdateMarkerColor { marker_id: MarkerId, color: MarkerColor },
    /// Set a sequence marker's range duration. `None` converts the marker
    /// back to a point marker; `Some(dur)` promotes it to a range spanning
    /// `position..position + dur`.
    SetMarkerDuration { marker_id: MarkerId, duration: Option<Timecode> },
    JumpToMarker { marker_id: MarkerId },

    // Per-clip markers
    AddClipMarker { clip_id: ClipId, time_secs: f64, name: String, color: MarkerColor },
    RemoveClipMarker { clip_id: ClipId, index: usize },

    // Dynamic tracks
    AddVideoTrack,
    AddAudioTrack,
    /// Sync auto_create_tracks preference to AppState.
    SyncAutoCreateTracks(bool),
    /// Insert a new track of the same kind above the given track index.
    AddTrackAbove { track_index: usize },
    /// Insert a new track of the same kind below the given track index.
    AddTrackBelow { track_index: usize },
    RemoveTrack { track_id: TrackId },
    RenameTrack { track_id: TrackId, name: String },
    /// Reset the per-track waveform amplitude zoom back to 1.0 (#50).
    ResetWaveformZoom { track_id: TrackId },

    // Audio sync (#61)
    /// Mark a clip as the reference for future `AlignToAudioReference` calls.
    SetAudioReference { clip_id: ClipId },
    /// Cross-correlate the given clip's audio with the current reference clip
    /// and shift its timeline position to align them.
    AlignToAudioReference { clip_id: ClipId },

    // Media pool management
    RemoveFromPool { media_id: MediaId },
    ConfirmRemoveFromPool { media_id: MediaId },
    SetMediaRating { media_id: MediaId, rating: u8 },

    SetMediaTags { media_id: MediaId, tags: Vec<String> },
    RevealInFileManager { path: PathBuf },
    /// Open a media clip in an external application (GIMP / Krita / Audacity / etc).
    /// The app is chosen from `ExternalToolsPrefs` based on media type and falls
    /// back to `xdg-open` when the configured field is empty.
    OpenInExternalApp { media_id: MediaId },
    RevealMediaInBrowser { media_id: MediaId },
    SetMediaColorLabel { media_id: MediaId, label: u8 },
    DeleteUnusedMedia,
    ConfirmDeleteUnusedMedia,
    /// Export despite offline media (user confirmed from warning dialog).
    ForceExportSequence { settings: Box<ExportSettings> },

    // Bins
    CreateBin { name: String },
    /// Create a child bin inside an existing bin.
    CreateSubBin { parent_id: BinId, name: String },
    /// Import a detected image sequence as a single video clip.
    ImportImageSequence {
        pattern: String,   // ffmpeg pattern e.g. "/path/frame_%04d.png"
        start_frame: u32,
        frame_count: u32,
        fps: f64,
    },
    DeleteBin { name: String },
    MoveItemToBin { media_id: MediaId, bin: Option<String> },
    RenameBin { old_name: String, new_name: String },
    /// Rename a single MediaItem — updates `MediaItem.filename` only. The
    /// source file on disk (`MediaItem.path`) is never touched.
    RenameMediaItem { media_id: MediaId, new_filename: String },
    /// Apply a set of filename changes atomically as one undo step. The UI
    /// computes `(id, new_filename)` pairs from the preview; the handler
    /// snapshots the old names from the current pool to build the undo
    /// record.
    BatchRenameMediaItems { changes: Vec<(MediaId, String)> },
    /// Open the batch-rename modal seeded with the given media ids.
    /// Dispatched from the bin context menus where `AppState` is not
    /// directly accessible.
    OpenBatchRenameDialog { media_ids: Vec<MediaId> },

    // Browser skimming
    StartSkimTranscode { media_id: MediaId },
    /// Force-regenerate skim transcode (deletes existing cache first).
    RegenerateSkimTranscode { media_id: MediaId },
    /// Force-regenerate filmstrip thumbnails (deletes existing cache first).
    RegenerateFilmstrip { media_id: MediaId },
    BrowserSkimFrame { media_id: MediaId, time_secs: f64 },

    // Browser audio skimming
    /// Begin an audio skim session for a media item (mouse enters skimmable clip).
    BrowserSkimAudioStart {
        media_id: MediaId,
        source_path: PathBuf,
        time_secs: f64,
    },
    /// Begin an audio skim session for a file that has NOT been imported into
    /// the media pool (no `MediaId`, no PCM cache). Used by the file browser
    /// hover preview where the user is just exploring a directory. The audio
    /// is decoded on the fly via ffmpeg into the same 48 kHz stereo f32 PCM
    /// the existing skim feeder thread expects.
    BrowserSkimAudioStartFromPath {
        source_path: PathBuf,
        time_secs: f64,
    },
    /// Update audio skim position (mouse moves while skimming).
    BrowserSkimAudioUpdate {
        media_id: MediaId,
        time_secs: f64,
    },
    /// End the audio skim session (mouse leaves clip or skimming disabled).
    BrowserSkimAudioEnd,

    // UI
    SetViewerMode(ViewerMode),
    SetActiveTool(Tool),
    ToggleFullscreen,
    ShowPreferences,

    // Track locking
    SetTrackLocked { track_id: TrackId, locked: bool },

    /// Set (or clear) a track's custom pixel height. `None` restores the
    /// kind's default height. Height is clamped to (32.0, 300.0).
    SetTrackCustomHeight { track_id: TrackId, height: Option<f32> },

    // Clip grouping
    GroupClips { clip_ids: Vec<ClipId> },
    UngroupClips { group_id: GroupId },
    UngroupClip { clip_id: ClipId },

    // Parameter modifiers (behaviors)
    AddModifier { clip_id: ClipId, param: ParamId, modifier: Modifier },
    RemoveModifier { clip_id: ClipId, param: ParamId, modifier_index: usize },
    UpdateModifier { clip_id: ClipId, param: ParamId, modifier_index: usize, modifier: Modifier },
    ToggleModifier { clip_id: ClipId, param: ParamId, modifier_index: usize },

    // Application
    Quit,

    // Audio keyframes
    AddAudioKeyframe { clip_id: ClipId, param: AudioParam, time_secs: f64, value: f32 },
    RemoveAudioKeyframe { clip_id: ClipId, param: AudioParam, time_secs: f64 },
    MoveAudioKeyframe { clip_id: ClipId, param: AudioParam, index: usize, new_time_secs: f64, new_value: f32 },
    SetAudioKeyframeInterpolation { clip_id: ClipId, param: AudioParam, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },
    SetAudioKeyframeTangent { clip_id: ClipId, param: AudioParam, keyframe_index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)> },

    // Video keyframes
    AddVideoKeyframe { clip_id: ClipId, param: VideoParam, time_secs: f64, value: f32 },
    RemoveVideoKeyframe { clip_id: ClipId, param: VideoParam, time_secs: f64 },
    MoveVideoKeyframe { clip_id: ClipId, param: VideoParam, index: usize, new_time_secs: f64, new_value: f32 },
    SetVideoKeyframeInterpolation { clip_id: ClipId, param: VideoParam, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },
    SetVideoKeyframeTangent { clip_id: ClipId, param: VideoParam, keyframe_index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)> },

    // Keyframe clipboard operations
    CopyAudioKeyframes { clip_id: ClipId, param: AudioParam },
    CopyVideoKeyframes { clip_id: ClipId, param: VideoParam },
    PasteAudioKeyframes { clip_id: ClipId, param: AudioParam },
    PasteVideoKeyframes { clip_id: ClipId, param: VideoParam },

    // Reverse keyframes
    ReverseAudioKeyframes { clip_id: ClipId, param: AudioParam },
    ReverseVideoKeyframes { clip_id: ClipId, param: VideoParam },

    // Effect keyframes
    AddEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, time_secs: f64, value: f32 },
    RemoveEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, time_secs: f64 },
    MoveEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, keyframe_index: usize, new_time_secs: f64, new_value: f32 },
    SetEffectKeyframeInterpolation { clip_id: ClipId, effect_index: usize, param_name: String, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },

    // Clip role
    SetClipRole { clip_id: ClipId, role: crate::ClipRole },

    // Clip speed
    SetClipSpeed { clip_id: ClipId, speed: f64 },

    // Speed keyframes (variable speed ramp)
    /// Add a speed keyframe to a clip at the given time.
    AddSpeedKeyframe { clip_id: ClipId, time_secs: f64, speed: f64 },
    /// Remove a speed keyframe by index.
    RemoveSpeedKeyframe { clip_id: ClipId, index: usize },
    /// Update an existing speed keyframe's time and value.
    UpdateSpeedKeyframe { clip_id: ClipId, index: usize, time_secs: f64, speed: f64 },
    /// Replace all speed keyframes (used by presets).
    SetSpeedKeyframes { clip_id: ClipId, keyframes: Vec<crate::Keyframe> },
    /// Begin drag for speed keyframe undo coalescing.
    BeginSpeedKeyframeDrag { key: String, clip_id: ClipId },
    /// End drag for speed keyframe undo coalescing.
    EndSpeedKeyframeDrag { key: String, clip_id: ClipId },

    // Source rotation override (0, 90, 180, 270 degrees)
    SetSourceRotation { clip_id: ClipId, degrees: i32 },

    // Clip audio
    UpdateClipAudio {
        clip_id: ClipId, gain: f32, volume: f32, pan: f32, fade_in_secs: f32, fade_out_secs: f32,
        fade_in_curve: crate::types::FadeCurve, fade_out_curve: crate::types::FadeCurve,
        channel_config: crate::types::ChannelConfig,
    },
    BeginAudioDrag { key: String, clip_id: ClipId, old_gain: f32, old_volume: f32, old_pan: f32, old_fade_in: f32, old_fade_out: f32 },
    EndAudioDrag { key: String, clip_id: ClipId, new_gain: f32, new_volume: f32, new_pan: f32, new_fade_in: f32, new_fade_out: f32 },

    // Scale keyframe timing
    ScaleAudioKeyframeTiming { clip_id: ClipId, param: AudioParam, factor: f64 },
    ScaleVideoKeyframeTiming { clip_id: ClipId, param: VideoParam, factor: f64 },

    // Insert keyframe bump pattern
    InsertAudioKeyframeBump { clip_id: ClipId, param: AudioParam, center_time: f64, bump_value: f32, base_value: f32, duration_secs: f64, ramp_secs: f64 },
    InsertVideoKeyframeBump { clip_id: ClipId, param: VideoParam, center_time: f64, bump_value: f32, base_value: f32, duration_secs: f64, ramp_secs: f64 },

    // Undo coalescing — drag gesture signals for keyframe moves/tangents
    BeginKeyframeDrag { key: String, clip_id: ClipId },
    EndKeyframeDrag { key: String, clip_id: ClipId },

    // Ken Burns presets
    ApplyKenBurns {
        clip_id: ClipId,
        start_scale: f32,
        end_scale: f32,
        start_pos_x: f32,
        start_pos_y: f32,
        end_pos_x: f32,
        end_pos_y: f32,
    },
    ClearTransformKeyframes { clip_id: ClipId },
    ClearVideoParamKeyframes { clip_id: ClipId, param: VideoParam },

    // Audio ducking
    AutoDuckAudio {
        target_clip_id: ClipId,
        /// Track index of the dialogue/sidechain source
        sidechain_track_idx: usize,
        /// How much to reduce volume (0.0 = silence, 0.3 = reduce to 30%)
        duck_level: f32,
        /// How quickly to ramp down in seconds
        attack_secs: f32,
        /// How quickly to ramp back up in seconds
        release_secs: f32,
        /// Threshold: audio level above which ducking activates (0.0-1.0)
        threshold: f32,
    },

    // Magnetic timeline ops
    RippleTrimHead { clip_id: ClipId },
    RippleTrimTail { clip_id: ClipId },
    InsertGapAtPlayhead,
    /// Q — trim the OUT edge of the clip whose OUT is at or before the playhead
    /// to the playhead position. Picks target by proximity; no selection required.
    TrimOutToPlayhead,
    /// W — trim the IN edge of the clip whose IN is at or after the playhead
    /// to the playhead position. Picks target by proximity; no selection required.
    TrimInToPlayhead,

    // Audio DSP effects
    AddAudioEffect { clip_id: ClipId, effect_type: String },
    RemoveAudioEffect { clip_id: ClipId, effect_index: usize },
    UpdateAudioEffectParam { clip_id: ClipId, effect_index: usize, param_name: String, value: f32 },
    ToggleAudioEffect { clip_id: ClipId, effect_index: usize },
    SetAudioEffectWetDry { clip_id: ClipId, effect_index: usize, wet_dry: f32 },
    ReorderAudioEffect { clip_id: ClipId, from_index: usize, to_index: usize },

    // Undo coalescing — drag gesture signals for builtin effects
    BeginEffectDrag {
        key: String,
        clip_id: ClipId,
        effect_index: usize,
        old_effect: klipp_effects::BuiltinEffect,
    },
    EndEffectDrag {
        key: String,
        clip_id: ClipId,
        effect_index: usize,
        new_effect: klipp_effects::BuiltinEffect,
    },

    // Video stabilization
    AnalyzeStabilization { clip_id: ClipId },
    CancelStabilization { clip_id: ClipId },

    // Scene (shot-boundary) detection
    DetectScenes { clip_id: ClipId },
    CancelSceneDetect { clip_id: ClipId },
    /// Razor-split a clip at every scene marker it carries, atomically.
    /// Undo restores the original single clip in one step.
    SplitAtSceneMarkers { clip_id: ClipId },
    /// Re-run smoothing on existing stabilization_data with the current smoothing
    /// slider value. Cheap (microseconds) — no decode, just gaussian over the
    /// already-baked trajectory. Used for live slider updates.
    ResmoothStabilization { clip_id: ClipId },

    // Point tracking
    /// Add a new track point to a clip at the given normalized position.
    AddTrackPoint { clip_id: ClipId, x: f64, y: f64 },
    /// Remove a track point by index.
    RemoveTrackPoint { clip_id: ClipId, index: usize },
    /// Analyze a track point (forward and backward from current frame).
    AnalyzeTrackPoint { clip_id: ClipId, point_index: usize },
    /// Cancel track analysis.
    CancelTrackAnalysis { clip_id: ClipId },
    /// Toggle tracking placement mode.
    ToggleTrackingMode,
    /// Generate keyframes from track point data to a target parameter.
    GenerateKeyframesFromTrack { clip_id: ClipId, point_index: usize, target: TrackTarget },

    // Grade versioning
    /// Save the current node_graph as a new named grade version.
    SaveGradeVersion { clip_id: ClipId, name: String },
    /// Switch to a different grade version by index.
    SwitchGradeVersion { clip_id: ClipId, version_index: usize },
    /// Delete a grade version by index.
    DeleteGradeVersion { clip_id: ClipId, version_index: usize },
    /// Rename a grade version.
    RenameGradeVersion { clip_id: ClipId, version_index: usize, name: String },

    // Grade stills (reference frame comparison)
    /// Capture the current viewer frame as a grade reference still.
    CaptureGradeStill { name: String },
    /// Delete a saved grade still by index.
    DeleteGradeStill { index: usize },
    /// Set the active grade still for comparison.
    SetActiveGradeStill { index: Option<usize> },
    /// Toggle split-view comparison with the active grade still.
    ToggleGradeStillCompare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerMode {
    Program,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Blade,
    Range,
    /// Trim tool — modifier-free access to Slip (clip body) and Roll
    /// (junction). Shift+drag on clip body triggers Slide. Alt/Alt+Shift
    /// modifiers continue to work in Tool::Select for power-user access.
    Trim,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum MarkerColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl MarkerColor {
    pub fn to_rgb(self) -> (u8, u8, u8) {
        match self {
            MarkerColor::Red => (220, 60, 60),
            MarkerColor::Orange => (220, 140, 40),
            MarkerColor::Yellow => (220, 200, 40),
            MarkerColor::Green => (60, 180, 80),
            MarkerColor::Blue => (60, 120, 220),
            MarkerColor::Purple => (160, 80, 200),
        }
    }

    pub fn all() -> &'static [MarkerColor] {
        &[
            MarkerColor::Red,
            MarkerColor::Orange,
            MarkerColor::Yellow,
            MarkerColor::Green,
            MarkerColor::Blue,
            MarkerColor::Purple,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            MarkerColor::Red => "Red",
            MarkerColor::Orange => "Orange",
            MarkerColor::Yellow => "Yellow",
            MarkerColor::Green => "Green",
            MarkerColor::Blue => "Blue",
            MarkerColor::Purple => "Purple",
        }
    }
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    // Playback state
    PlaybackStarted,
    PlaybackPaused,
    PlaybackStopped,
    PlayheadMoved(Timecode),
    LoopedBack,

    // Media
    MediaLoaded {
        media_id: MediaId,
        path: PathBuf,
        duration: Timecode,
        fps: Fps,
        resolution: Resolution,
        video_codec: String,
        audio_codec: String,
        has_audio: bool,
        file_size_bytes: u64,
        sample_rate: Option<u32>,
        audio_channels: Option<u16>,
        is_still: bool,
        file_modified_at: Option<u64>,
        media_created_at: Option<u64>,
        colour_space: u8,
        source_rotation: i32,
        // Extended metadata
        camera_make: Option<String>,
        camera_model: Option<String>,
        encoder: Option<String>,
        bit_depth: Option<u8>,
        video_bitrate_kbps: Option<u32>,
        audio_bitrate_kbps: Option<u32>,
        color_primaries: Option<String>,
        color_transfer: Option<String>,
        pixel_format: Option<String>,
    },
    MediaLoadFailed {
        media_id: MediaId,
        error: String,
    },
    /// File watcher detected a source file was modified or deleted.
    MediaFileChanged {
        path: PathBuf,
        deleted: bool,
    },

    // Frame
    FrameReady,

    // Multi-clip
    ClipAppended { clip_id: ClipId },
    ClipRemoved { clip_id: ClipId },
    ClipMoved { clip_id: ClipId, new_timeline_in: Timecode },
    ClipTrimmed { clip_id: ClipId },
    ClipSelected(Option<ClipId>),

    // Project
    ProjectSaved { path: PathBuf },
    ProjectLoaded { path: PathBuf },
    ProjectError { message: String },

    // Export
    ExportProgress { percent: f32, time_secs: f64 },
    ExportComplete { path: PathBuf },
    ExportError { message: String },

    // Source monitor
    SourceFrameReady,
    SourceMediaLoaded { media_id: MediaId, duration: Timecode },

    // Proxy
    ProxyGenerationStarted { media_id: MediaId },
    ProxyGenerationComplete { media_id: MediaId, path: PathBuf },
    ProxyGenerationFailed { media_id: MediaId, error: String },

    // Thumbnails
    ThumbnailReady { media_id: MediaId, rgba: Vec<u8>, width: u32, height: u32 },

    // Audio conforming
    AudioConformStarted { media_id: MediaId },
    AudioConformComplete { media_id: MediaId },
    AudioConformFailed { media_id: MediaId, error: String },

    // Audio metering
    TrackPeakLevels { track_index: usize, left: f32, right: f32 },
    MasterPeakLevels { left: f32, right: f32 },
    /// BS.1770-4 loudness meter readings from the mix thread.
    LufsReadings { momentary: f32, short_term: f32, integrated: f32, loudness_range: f32, true_peak_dbfs: f32 },

    // Performance stats
    EngineStats { dropped_frames: u64, hw_decode: bool },

    // Skim transcode
    SkimTranscodeProgress { media_id: MediaId, progress: f32 },
    SkimTranscodeComplete { media_id: MediaId, path: PathBuf },
    SkimTranscodeFailed { media_id: MediaId, error: String },

    // Browser skim frame
    BrowserSkimFrameReady { media_id: MediaId, rgba: Vec<u8>, width: u32, height: u32 },

    // Video stabilization
    StabilizationProgress { clip_id: ClipId, progress: f32 },
    StabilizationComplete { clip_id: ClipId },
    StabilizationFailed { clip_id: ClipId, error: String },

    // Scene (shot-boundary) detection
    SceneDetectProgress { clip_id: ClipId, progress: f32 },
    SceneDetectComplete { clip_id: ClipId, cut_times: Vec<f64> },
    SceneDetectFailed { clip_id: ClipId, error: String },

    // Point tracking
    TrackingProgress { clip_id: ClipId, progress: f32 },
    TrackingComplete { clip_id: ClipId, point_index: usize, positions: Vec<(f64, f64)>, start_frame: usize },
    TrackingFailed { clip_id: ClipId, error: String },

    // Audio sync (#61)
    /// Background cross-correlation finished. `offset_samples` at 48 kHz —
    /// positive means the target clip was found to start _later_ than the
    /// reference; negative means _earlier_. `ref_timeline_in_secs` is the
    /// reference clip's timeline position at the moment the job was spawned
    /// (read on the UI thread, shipped to the worker, and echoed back so the
    /// completion handler has it without another lookup).
    AudioSyncComplete {
        clip_id: ClipId,
        offset_samples: i64,
        ref_timeline_in_secs: f64,
    },
    AudioSyncFailed { clip_id: ClipId, error: String },
}

/// Target for generating keyframes from tracked point data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TrackTarget {
    /// Generate position keyframes for a mask's center.
    MaskCenter { node_id: uuid::Uuid, mask_index: usize },
    /// Generate transform position keyframes.
    TransformPosition,
}
