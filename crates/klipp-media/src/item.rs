use klipp_core::{BinId, Fps, MediaId, Resolution, Timecode};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A bin (folder) for organising media items. Supports nesting via parent_id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bin {
    pub id: BinId,
    pub name: String,
    /// None = root-level bin. Some(id) = child of another bin.
    #[serde(default)]
    pub parent_id: Option<BinId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorLabel {
    #[default]
    None,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl ColorLabel {
    /// Returns (r, g, b) — no egui dependency. Conversion to Color32 in klipp-ui.
    pub fn rgb(&self) -> Option<(u8, u8, u8)> {
        match self {
            Self::None   => Option::None,
            Self::Red    => Some((200, 60,  60)),
            Self::Orange => Some((210, 120, 40)),
            Self::Yellow => Some((200, 180, 40)),
            Self::Green  => Some((60,  160, 70)),
            Self::Blue   => Some((60,  100, 200)),
            Self::Purple => Some((130, 60,  190)),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::None   => "None",
            Self::Red    => "Red",
            Self::Orange => "Orange",
            Self::Yellow => "Yellow",
            Self::Green  => "Green",
            Self::Blue   => "Blue",
            Self::Purple => "Purple",
        }
    }

    pub const ALL: [ColorLabel; 7] = [
        Self::None, Self::Red, Self::Orange, Self::Yellow,
        Self::Green, Self::Blue, Self::Purple,
    ];

    pub fn to_u8(self) -> u8 {
        match self {
            Self::None => 0, Self::Red => 1, Self::Orange => 2,
            Self::Yellow => 3, Self::Green => 4, Self::Blue => 5, Self::Purple => 6,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Red, 2 => Self::Orange, 3 => Self::Yellow,
            4 => Self::Green, 5 => Self::Blue, 6 => Self::Purple,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColourSpace {
    #[default]
    Unknown,
    Sdr,    // BT.709 / sRGB
    Hdr10,  // BT.2020 + PQ (SMPTE ST 2084)
    Hlg,    // BT.2020 + HLG (ARIB STD-B67)
}

impl ColourSpace {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Sdr     => "SDR (BT.709)",
            Self::Hdr10   => "HDR10 (BT.2020 / PQ)",
            Self::Hlg     => "HLG (BT.2020 / HLG)",
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Self::Unknown => 0, Self::Sdr => 1, Self::Hdr10 => 2, Self::Hlg => 3,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Sdr, 2 => Self::Hdr10, 3 => Self::Hlg, _ => Self::Unknown,
        }
    }
}

/// Discriminates "real" file-backed media from synthetic project
/// assets. Synthetic items reuse the entire `MediaItem` plumbing
/// (drag-and-drop, bins, ratings, sort, search) but their underlying
/// data lives in project-scoped stores keyed by the same `MediaId`.
/// Adding a new variant means adding a matching project-side store
/// (e.g. `title_assets: HashMap<MediaId, TitleAsset>` on AppState
/// for `SyntheticTitle`).
///
/// `#[serde(default)]` on the enclosing field means existing `.klipp`
/// projects without this field load as `File`, preserving every
/// pre-existing media item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum MediaItemKind {
    /// Standard file-backed item — `path` points to an actual file
    /// on disk. This is what every MediaItem was before synthetic
    /// assets shipped.
    #[default]
    File,
    /// Title clip saved as a reusable project asset. The
    /// `TitleData` lives in the project's `title_assets` store
    /// keyed by `MediaItem.id`. `path` stays as a synthetic stub
    /// (e.g. an empty `PathBuf` or a sentinel "<title-asset:UUID>")
    /// so file-only code paths can't accidentally treat it as a
    /// real file.
    SyntheticTitle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: MediaId,
    pub path: PathBuf,
    pub filename: String,
    pub duration: Timecode,
    pub fps: Fps,
    pub resolution: Resolution,
    pub video_codec: String,
    pub audio_codec: String,
    pub has_audio: bool,
    pub file_size_bytes: u64,
    /// File-backed vs synthetic-asset discriminator. Default is
    /// `File` for serde backward-compat with existing projects.
    #[serde(default)]
    pub kind: MediaItemKind,
    /// Audio sample rate in Hz (e.g. 48000, 44100, 96000). None if no audio stream.
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// Audio channel count (1=mono, 2=stereo, 6=5.1). None if no audio stream.
    #[serde(default)]
    pub audio_channels: Option<u16>,
    #[serde(default)]
    pub offline: bool,
    /// True if this is a still image (PNG/JPG/etc.) — single frame, default duration applied.
    #[serde(default)]
    pub is_still: bool,
    #[serde(default)]
    pub proxy_path: Option<PathBuf>,
    #[serde(default)]
    pub proxy_ready: bool,
    #[serde(default)]
    pub default_source_in: Option<Timecode>,
    #[serde(default)]
    pub default_source_out: Option<Timecode>,
    /// User rating (0–5 stars)
    #[serde(default)]
    pub rating: u8,
    /// User tags
    #[serde(default)]
    pub tags: Vec<String>,
    /// User notes
    #[serde(default)]
    pub user_notes: String,
    /// Legacy bin name (used for backward compat with old projects). Prefer bin_id.
    #[serde(default)]
    pub bin: Option<String>,
    /// Bin ID reference (None = root/unassigned). Used for nested bin hierarchy.
    #[serde(default)]
    pub bin_id: Option<BinId>,
    /// Subclip parent — `Some(id)` marks this item as a subclip of another
    /// MediaItem. The subclip reuses the parent's `path` and inherits codec /
    /// fps / resolution; its `default_source_in/out` describe the zone
    /// within the parent's source. `None` for normal items. Old `.klipp`
    /// files without this field deserialize to `None`.
    #[serde(default)]
    pub parent_media_id: Option<MediaId>,
    /// Unix epoch seconds when item was added to the project (0 for pre-existing items)
    #[serde(default)]
    pub added_at: u64,
    /// Content fingerprint: FNV-1a hash of first 64KB + file_size.
    /// Used for content-based deduplication (Prompt 18).
    #[serde(default)]
    pub fingerprint: Option<u64>,
    /// Filesystem modification time (Unix epoch seconds).
    #[serde(default)]
    pub file_modified_at: Option<u64>,
    /// Container creation_time from ffmpeg metadata (Unix epoch seconds). More accurate shoot date.
    #[serde(default)]
    pub media_created_at: Option<u64>,
    /// Visual colour label for organisation.
    #[serde(default)]
    pub color_label: ColorLabel,
    /// Detected colour space / HDR format.
    #[serde(default)]
    pub colour_space: ColourSpace,
    /// Camera log encoding format the user picked (for IDT conversion).
    /// Meaningful only when `log_format_user_set`; see
    /// [`MediaItem::effective_log_format`].
    #[serde(default)]
    pub log_format: klipp_core::LogFormat,
    /// Whether `log_format` is the user's choice. Without this, choosing
    /// "sRGB / Rec.709" (the default value) could not override a detected
    /// log format: it read as "never chosen" and detection won, so a
    /// mis-detected clip stayed converted whatever the user picked.
    #[serde(default)]
    pub log_format_user_set: bool,
    /// Source rotation in degrees (0, 90, 180, 270) from container metadata / display matrix.
    /// Phone videos often have 90° or 270° rotation.
    #[serde(default)]
    pub source_rotation: i32,
    /// Camera make (e.g. "Canon", "Sony", "Apple"). From container metadata.
    #[serde(default)]
    pub camera_make: Option<String>,
    /// Camera model (e.g. "EOS R5", "FX6", "iPhone 15 Pro"). From container metadata.
    #[serde(default)]
    pub camera_model: Option<String>,
    /// Encoder software (e.g. "Lavf60.3.100", "HandBrake"). From container/stream metadata.
    #[serde(default)]
    pub encoder: Option<String>,
    /// Video bit depth (8, 10, 12). From pixel format.
    #[serde(default)]
    pub bit_depth: Option<u8>,
    /// Video bitrate in kbps. From stream or container.
    #[serde(default)]
    pub video_bitrate_kbps: Option<u32>,
    /// Audio bitrate in kbps.
    #[serde(default)]
    pub audio_bitrate_kbps: Option<u32>,
    /// Color primaries name (e.g. "BT.709", "BT.2020"). From ffmpeg.
    #[serde(default)]
    pub color_primaries: Option<String>,
    /// Color transfer name (e.g. "sRGB", "PQ", "HLG", "S-Log3"). From ffmpeg.
    #[serde(default)]
    pub color_transfer: Option<String>,
    /// Pixel format string (e.g. "yuv420p", "yuv420p10le"). From ffmpeg.
    #[serde(default)]
    pub pixel_format: Option<String>,
    /// Auto-detected log format based on color_transfer, camera make/model, and bit depth.
    /// Used to suggest InputTransform when clips are added to the timeline.
    #[serde(default)]
    pub detected_log_format: klipp_core::LogFormat,
    /// Timecode, reel, lens, exposure and every tag the file carried.
    #[serde(default)]
    pub shot: klipp_core::ShotMetadata,
    /// The import probe has read `shot`. False for an item saved before
    /// the probe read it, which is probed once more when its project
    /// opens; see [`MediaItem::wants_metadata_probe`].
    #[serde(default)]
    pub metadata_read: bool,
    /// The duration the file's header claims, when the file holds less:
    /// a copy cut short. `duration` is then what it holds.
    #[serde(default)]
    pub truncated_from_secs: Option<f64>,
    /// What the editor logged about the clip.
    #[serde(default)]
    pub log: klipp_core::ClipLog,
    /// The separate recording paired with this clip, and its offset:
    /// placed under the clip instead of the camera's own sound.
    #[serde(default)]
    pub synced_audio: Option<klipp_core::SyncedAudio>,
    /// Notes, favourite stretches and rejected ones the editor marked.
    #[serde(default)]
    pub markers: Vec<klipp_core::MediaMarker>,
    /// The lines the screen-text analysis read off the picture, each
    /// once, for finding and binning footage by what it showed. Empty
    /// until that analysis has run.
    #[serde(default)]
    pub screen_text: String,
    /// The role its sound was classified as (#2216), proposed for its
    /// clips. `None` until that analysis has run, or with nothing heard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_role: Option<klipp_core::RoleProposal>,
    /// Which part of a split take this is, when it is one.
    #[serde(default)]
    pub span: Option<klipp_core::ClipSpan>,
    /// Path to skim-optimised file (all-keyframe transcode for instant seeking)
    #[serde(default)]
    pub skim_file: Option<PathBuf>,
    /// File watcher detected external modification — needs background re-probe.
    #[serde(skip)]
    pub needs_reprobe: bool,
    /// Waveform min/max pairs for timeline display. Generated in background.
    #[serde(skip)]
    pub waveform_data: Option<Vec<(f32, f32)>>,
    /// Audio cache entry — set after background conforming completes.
    #[serde(skip)]
    pub audio_cache: Option<crate::audio_cache::AudioCacheEntry>,
}

impl MediaItem {
    /// What a probe of the file says of it, taken from `from`: the details
    /// a relink's re-probe replaces, given back when the relink is undone
    /// (#2327). What the person said of the item (rating, tags, notes,
    /// bin, colour label, log format) is kept.
    pub fn take_probed_details(&mut self, from: &MediaItem) {
        self.filename = from.filename.clone();
        self.duration = from.duration;
        self.fps = from.fps;
        self.resolution = from.resolution;
        self.video_codec = from.video_codec.clone();
        self.audio_codec = from.audio_codec.clone();
        self.has_audio = from.has_audio;
        self.file_size_bytes = from.file_size_bytes;
        self.sample_rate = from.sample_rate;
        self.audio_channels = from.audio_channels;
        self.is_still = from.is_still;
        self.fingerprint = from.fingerprint;
        self.file_modified_at = from.file_modified_at;
        self.media_created_at = from.media_created_at;
        self.colour_space = from.colour_space;
        self.source_rotation = from.source_rotation;
        self.camera_make = from.camera_make.clone();
        self.camera_model = from.camera_model.clone();
        self.encoder = from.encoder.clone();
        self.bit_depth = from.bit_depth;
        self.video_bitrate_kbps = from.video_bitrate_kbps;
        self.audio_bitrate_kbps = from.audio_bitrate_kbps;
        self.color_primaries = from.color_primaries.clone();
        self.color_transfer = from.color_transfer.clone();
        self.pixel_format = from.pixel_format.clone();
        self.detected_log_format = from.detected_log_format;
        self.shot = from.shot.clone();
        self.metadata_read = from.metadata_read;
        self.truncated_from_secs = from.truncated_from_secs;
    }

    /// True when this item is a subclip — a zone-within-parent virtual
    /// reference. Subclips share the parent's `path` but carry their own
    /// `default_source_in/out` describing the zone.
    pub fn is_subclip(&self) -> bool {
        self.parent_media_id.is_some()
    }

    /// Rotation (degrees) the decoder should apply when reading
    /// `decode_path`. Returns `0` when the path is this item's skim or
    /// proxy and `source_rotation` otherwise.
    ///
    /// The skim transcode pipeline now autorotates: portrait sources
    /// (`source_rotation != 0`) are routed through the libx264 path
    /// where libavformat's CPU autorotate works normally, so the
    /// resulting skim file has upright pixels and needs no further
    /// rotation at read time. Landscape sources keep the fast
    /// nvenc/CUDA path; their skim is also upright (rotation = 0
    /// either way). The skim cache key was bumped at the same time
    /// (`.r{deg}` marker on portrait clips) so stale sideways skims
    /// from the pre-autorotate era do not get reused.
    ///
    /// Original-file paths still need `source_rotation` because the
    /// source carries the displaymatrix metadata and pixels stay in
    /// the camera's stored orientation.
    pub fn effective_rotation_for(&self, decode_path: &std::path::Path) -> i32 {
        let is_optimised = self
            .skim_file
            .as_deref()
            .is_some_and(|p| p == decode_path)
            || self
                .proxy_path
                .as_deref()
                .is_some_and(|p| p == decode_path);
        if is_optimised {
            0
        } else {
            self.source_rotation
        }
    }

    /// True when this item is a synthetic project asset (title /
    /// generator etc.) — its underlying data lives in a project-
    /// scoped store (e.g. `AppState::title_assets`) rather than on
    /// disk. Callers that walk the media pool to do file-only work
    /// (probing codecs, reading frames, generating thumbnails from
    /// disk) should skip these.
    pub fn is_synthetic(&self) -> bool {
        !matches!(self.kind, MediaItemKind::File)
    }

    /// Build a `MediaItem` representing a synthetic title asset.
    /// Duration and FPS come from the host sequence so a dropped
    /// instance lands at a sensible default length on the timeline.
    /// `path` is stored as a sentinel ("`<title-asset:UUID>`") so any
    /// path-comparison or `path.exists()` check fails closed instead
    /// of accidentally pointing at an unrelated file.
    pub fn new_synthetic_title(
        id: MediaId,
        display_name: String,
        duration: Timecode,
        fps: Fps,
    ) -> Self {
        let sentinel = PathBuf::from(format!("<title-asset:{}>", id.0));
        Self {
            id,
            path: sentinel,
            kind: MediaItemKind::SyntheticTitle,
            filename: display_name,
            duration,
            fps,
            resolution: Resolution { width: 1920, height: 1080 },
            video_codec: String::new(),
            audio_codec: String::new(),
            has_audio: false,
            file_size_bytes: 0,
            sample_rate: None,
            audio_channels: None,
            offline: false,
            is_still: false,
            proxy_path: None,
            proxy_ready: false,
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: Vec::new(),
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            parent_media_id: None,
            added_at: 0,
            fingerprint: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: ColorLabel::default(),
            colour_space: ColourSpace::default(),
            log_format: klipp_core::LogFormat::default(),
            source_rotation: 0,
            camera_make: None,
            camera_model: None,
            encoder: None,
            bit_depth: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            color_primaries: None,
            color_transfer: None,
            pixel_format: None,
            detected_log_format: klipp_core::LogFormat::default(),
            log_format_user_set: false,
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            audio_role: None,
            span: None,
        }
    }

    /// Detect the likely log format from metadata (color_transfer, camera, bit depth, pixel format).
    /// What the decoder should treat this file as: the user's choice
    /// when they made one, otherwise what detection found.
    pub fn effective_log_format(&self) -> klipp_core::LogFormat {
        if self.log_format_user_set || self.log_format != klipp_core::LogFormat::default() {
            self.log_format
        } else {
            self.detected_log_format
        }
    }

    /// Whether the file's frames decode scene-referred: HDR (PQ, HLG) or
    /// camera log.
    pub fn scene_referred(&self) -> bool {
        self.scene_transfer().is_some()
    }

    /// The transfer the file's frames decode scene-referred through: its
    /// camera log curve when it decodes as one, else PQ or HLG. `None` for
    /// an SDR file. A media source's display step (#1857) takes it, as a
    /// clip's takes its decoded frame's.
    pub fn scene_transfer(&self) -> Option<klipp_color::hdr::HdrTransfer> {
        use klipp_color::hdr::HdrTransfer;
        if let Some((transfer, _)) = self.camera_log_format().and_then(klipp_core::LogFormat::camera_colourspace) {
            return Some(HdrTransfer::Log(transfer));
        }
        match self.colour_space {
            ColourSpace::Hdr10 => Some(HdrTransfer::Pq),
            ColourSpace::Hlg => Some(HdrTransfer::Hlg),
            _ => None,
        }
    }

    /// The camera log format the file decodes through, when it is one:
    /// what export's decoders and a media source's reader (#1746) take.
    pub fn camera_log_format(&self) -> Option<klipp_core::LogFormat> {
        Some(self.effective_log_format()).filter(|f| f.camera_colourspace().is_some())
    }

    /// What this file's metadata says about being camera log, for the
    /// colour tools' flat-picture test (#504).
    pub fn log_prior(&self) -> klipp_color::measure::LogPrior {
        use klipp_color::measure::LogPrior;
        // Known to be 8-bit, from the probe's depth or a plain 8-bit pixel
        // format: a clip not probed yet is not ruled out (#607 review).
        let eight_bit = match self.bit_depth {
            Some(d) => d <= 8,
            None => self.pixel_format.as_deref().is_some_and(|pf| {
                matches!(pf, "yuv420p" | "yuvj420p" | "yuv422p" | "yuvj422p" | "yuv444p" | "yuvj444p" | "nv12" | "nv21" | "rgb24" | "bgr24" | "rgba" | "bgra")
            }),
        };
        let make = self.camera_make.as_deref().unwrap_or("").to_lowercase();
        let model = self.camera_model.as_deref().unwrap_or("").to_lowercase();
        if eight_bit && (make.contains("apple") || model.contains("iphone")) {
            // Apple Log is 10-bit ProRes or HEVC only.
            return LogPrior::NeverLog;
        }
        if self.color_transfer.as_deref().is_some_and(|t| is_sdr_transfer(&t.to_lowercase())) {
            return LogPrior::DisplayTagged;
        }
        // Stock-footage re-encodes can discard every colour tag (#1569).
        // Their known 8-bit 4:2:0 H.264 encoding is weak display evidence,
        // never proof: cameras can also record log into this format.
        let untagged = self.color_transfer.as_deref().is_none_or(|t| {
            matches!(t.trim().to_ascii_lowercase().as_str(), "" | "unknown" | "unspecified")
        });
        let consumer_eight_bit = matches!(self.bit_depth, None | Some(8))
            && self.pixel_format.as_deref().is_some_and(|pf| {
                matches!(pf, "yuv420p" | "yuvj420p" | "nv12" | "nv21")
            });
        if untagged && consumer_eight_bit && self.video_codec.eq_ignore_ascii_case("h264")
            && self.effective_log_format() == klipp_core::LogFormat::Srgb
        {
            return LogPrior::DisplayLikely;
        }
        LogPrior::Unknown
    }

    pub fn detect_log_format(&self) -> klipp_core::LogFormat {
        use klipp_core::LogFormat;

        // Check explicit transfer characteristics from ffmpeg first
        if let Some(ref trc) = self.color_transfer {
            let t = trc.to_lowercase();
            if t.contains("pq") || t.contains("st 2084") || t.contains("smpte2084") {
                return LogFormat::PQ;
            }
            if t.contains("hlg") || t.contains("arib") || t.contains("bt.2100") {
                return LogFormat::HLG;
            }
            if t.contains("s-log") || t.contains("slog") {
                return LogFormat::SLog3;
            }
            if t.contains("log") && t.contains("c") {
                return LogFormat::LogC3;
            }
            if t.contains("v-log") || t.contains("vlog") {
                return LogFormat::VLog;
            }
            // An explicit SDR transfer is authoritative: a 10-bit Sony or
            // iPhone clip shot in a Rec.709 profile says so here, and the
            // camera guesses below would otherwise call it log. Only an
            // unknown or unspecified tag falls through to them.
            if is_sdr_transfer(&t) {
                return LogFormat::Srgb;
            }
        }

        // For camera-specific detection, only suggest if bit depth > 8 (10-bit+ = likely log)
        let is_high_bit = self.bit_depth.unwrap_or(8) > 8
            || self.pixel_format.as_deref().is_some_and(|pf|
                pf.contains("10") || pf.contains("12") || pf.contains("16"));

        if !is_high_bit {
            return LogFormat::Srgb;
        }

        // Camera make/model heuristics
        let make = self.camera_make.as_deref().unwrap_or("").to_lowercase();
        let model = self.camera_model.as_deref().unwrap_or("").to_lowercase();

        if make.contains("arri") || model.contains("alexa") || model.contains("amira") {
            return if model.contains("35") { LogFormat::LogC4 } else { LogFormat::LogC3 };
        }
        if make.contains("sony") || model.contains("fx") || model.contains("a7") || model.contains("venice") {
            return LogFormat::SLog3;
        }
        // Leica's one log is L-Log (#1225), in BT.709 primaries on the SL
        // (Typ 601) and BT.2020 on every later body (#1642).
        if make.contains("leica") {
            return if model.contains("typ 601") { LogFormat::LeicaLLogBt709 } else { LogFormat::LeicaLLog };
        }
        if make.contains("panasonic") || model.contains("gh") || model.contains("s1") || model.contains("s5") {
            return LogFormat::VLog;
        }
        if make.contains("canon") || model.contains("eos") || model.contains("c70") || model.contains("c300") {
            return LogFormat::CanonLog3;
        }
        if make.contains("red") || model.contains("dsmc") || model.contains("komodo") || model.contains("raptor") {
            return LogFormat::RedLog3G10;
        }
        if make.contains("blackmagic") || model.contains("bmpcc") || model.contains("ursa") {
            return LogFormat::BmdFilmGen5;
        }
        if make.contains("nikon") || model.contains("z 8") || model.contains("z 9") {
            return LogFormat::NLog;
        }
        if make.contains("fuji") || model.contains("x-h") || model.contains("x-t") {
            return LogFormat::FLog;
        }
        if make.contains("dji") || model.contains("mavic") || model.contains("inspire") {
            return LogFormat::DJIDLog;
        }
        if make.contains("gopro") {
            return LogFormat::GoProFlat;
        }
        if make.contains("apple") || model.contains("iphone") {
            return LogFormat::AppleLog;
        }

        // Last-ditch encoder-string heuristic for files with no
        // make/model metadata. Many cameras strip those tags but
        // still write a recognisable encoder signature; without
        // this, "Sony A6700 / A7S / FX-series 10-bit HEVC straight
        // out of camera" lands as Srgb because the container
        // carries `color_transfer=unknown` AND `camera_make=` empty
        // — the heuristic above has nothing to match. Cross-check
        // with bit depth so we don't claim S-Log3 for an 8-bit
        // GoPro that happens to use the same encoder name.
        let encoder = self.encoder.as_deref().unwrap_or("").to_lowercase();
        if !encoder.is_empty() {
            // "HEVC Coding" — Sony's signature on consumer / prosumer
            // HEVC out-of-camera (A6700, A7S III, A1, FX3, FX30, etc.)
            // when no other metadata is written. Always paired with
            // 10-bit (already gated above), the user is overwhelmingly
            // likely to be shooting S-Log3 if they bothered with the
            // 10-bit setting.
            if encoder.contains("hevc coding") {
                return LogFormat::SLog3;
            }
        }

        LogFormat::Srgb
    }
}

/// Whether a lowercased `color_transfer` name is an SDR display curve.
/// Takes both the probe's display names ("BT.709", "SMPTE 170M",
/// "Gamma 2.2", "sRGB", "BT.2020 10-bit") and ffmpeg's short names
/// ("bt709", "smpte170m", "gamma22", "iec61966-2-1", "bt2020-10").
fn is_sdr_transfer(trc: &str) -> bool {
    let key: String = trc.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    matches!(
        key.as_str(),
        "bt709" | "bt1886" | "bt1361" | "bt1361e" | "smpte170m" | "smpte240m"
            | "bt470m" | "bt470bg" | "gamma22" | "gamma28" | "srgb" | "iec6196621"
            | "bt202010" | "bt202012" | "bt202010bit" | "bt202012bit"
    )
}

#[cfg(test)]
mod subclip_tests {
    //! Data-layer coverage for the subclip feature. The full handler lives
    //! on KlippApp and is tested via app-level tests; these exercise the
    //! MediaItem + MediaPool shapes that the handler relies on.
    use super::*;
    use crate::MediaPool;
    use klipp_core::{Fps, MediaId, RationalTime, Resolution};
    use std::path::PathBuf;

    fn make_parent() -> MediaItem {
        MediaItem {
            id: MediaId::new(),
            path: PathBuf::from("/tmp/parent.mp4"),
            kind: MediaItemKind::File,
            filename: "parent.mp4".into(),
            duration: RationalTime { value: 240, rate: 24 }, // 10 seconds at 24fps
            fps: Fps::film_24(),
            resolution: Resolution { width: 1920, height: 1080 },
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            has_audio: true,
            file_size_bytes: 1_000_000,
            sample_rate: Some(48000),
            audio_channels: Some(2),
            offline: false,
            is_still: false,
            proxy_path: None,
            proxy_ready: false,
            default_source_in: Some(RationalTime { value: 48, rate: 24 }),   // 2s
            default_source_out: Some(RationalTime { value: 168, rate: 24 }), // 7s
            rating: 0,
            tags: Vec::new(),
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            parent_media_id: None,
            added_at: 0,
            fingerprint: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: ColorLabel::default(),
            colour_space: ColourSpace::default(),
            log_format: klipp_core::LogFormat::default(),
            source_rotation: 0,
            camera_make: None,
            camera_model: None,
            encoder: None,
            bit_depth: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            color_primaries: None,
            color_transfer: None,
            pixel_format: None,
            detected_log_format: klipp_core::LogFormat::default(),
            log_format_user_set: false,
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            audio_role: None,
            span: None,
        }
    }

    /// Replicates the core of `create_subclip_from_zone`'s data work so the
    /// data invariants are covered without spinning up a KlippApp.
    fn build_subclip(parent: &MediaItem, name: &str) -> MediaItem {
        let zone_in = parent.default_source_in.unwrap();
        let zone_out = parent.default_source_out.unwrap();
        let mut s = parent.clone();
        s.id = MediaId::new();
        s.filename = name.into();
        s.duration = zone_out - zone_in;
        s.default_source_in = Some(zone_in);
        s.default_source_out = Some(zone_out);
        s.parent_media_id = Some(parent.id);
        s.file_size_bytes = 0;
        s
    }

    #[test]
    fn subclip_inherits_parent_fps_and_codec() {
        let parent = make_parent();
        let sub = build_subclip(&parent, "parent_sub1");
        assert_eq!(sub.fps, parent.fps);
        assert_eq!(sub.video_codec, parent.video_codec);
        assert_eq!(sub.audio_codec, parent.audio_codec);
        assert_eq!(sub.has_audio, parent.has_audio);
        assert_eq!(sub.resolution.width, parent.resolution.width);
    }

    #[test]
    fn subclip_duration_matches_zone() {
        let parent = make_parent();
        let sub = build_subclip(&parent, "parent_sub1");
        // zone = 48..168 @ 24fps = 120 frames = 5s.
        assert_eq!(sub.duration.value, 120);
        assert_eq!(sub.duration.rate, 24);
    }

    #[test]
    fn subclip_is_subclip_flag_true() {
        let parent = make_parent();
        let sub = build_subclip(&parent, "parent_sub1");
        assert!(!parent.is_subclip());
        assert!(sub.is_subclip());
        assert_eq!(sub.parent_media_id, Some(parent.id));
    }

    #[test]
    fn auto_naming_counter_logic() {
        // Mirrors the counter increment inside the handler.
        let parent = make_parent();
        let mut pool = MediaPool::default();
        let parent_id = parent.id;
        pool.add(parent);

        let n1 = pool.all().filter(|i| i.parent_media_id == Some(parent_id)).count() + 1;
        assert_eq!(n1, 1);
        let parent_ref = pool.get(&parent_id).cloned().unwrap();
        let sub1 = build_subclip(&parent_ref, &format!("parent_sub{n1}"));
        pool.add(sub1);

        let n2 = pool.all().filter(|i| i.parent_media_id == Some(parent_id)).count() + 1;
        assert_eq!(n2, 2);
        let sub2 = build_subclip(&parent_ref, &format!("parent_sub{n2}"));
        pool.add(sub2);

        let names: Vec<String> = pool
            .all()
            .filter(|i| i.parent_media_id == Some(parent_id))
            .map(|i| i.filename.clone())
            .collect();
        assert!(names.contains(&"parent_sub1".to_string()));
        assert!(names.contains(&"parent_sub2".to_string()));
    }

    #[test]
    fn cascade_delete_removes_subclips() {
        // Mirrors the cascade pass in the RemoveFromPool handler.
        let parent = make_parent();
        let parent_id = parent.id;
        let mut pool = MediaPool::default();
        pool.add(parent);
        let p_ref = pool.get(&parent_id).cloned().unwrap();
        pool.add(build_subclip(&p_ref, "parent_sub1"));
        pool.add(build_subclip(&p_ref, "parent_sub2"));
        assert_eq!(pool.all().count(), 3);

        let child_ids: Vec<MediaId> = pool
            .all()
            .filter(|i| i.parent_media_id == Some(parent_id))
            .map(|i| i.id)
            .collect();
        for cid in &child_ids {
            pool.remove(cid);
        }
        pool.remove(&parent_id);
        assert_eq!(pool.all().count(), 0);
    }

    #[test]
    fn subclip_of_subclip_is_rejected_by_flag() {
        // The handler early-exits when `parent.is_subclip()` is true. This
        // test asserts that flag, which is the decision point.
        let parent = make_parent();
        let sub = build_subclip(&parent, "parent_sub1");
        assert!(sub.is_subclip());
        // A real handler call with source_media_id == sub.id would hit the
        // "Cannot create a subclip from a subclip." branch.
    }
}

#[cfg(test)]
mod effective_rotation_tests {
    //! Pin the path-aware rotation contract used by every
    //! `VideoDecoder::open(path, rotation)` site in klipp-engine and the
    //! source-viewer dispatch in klipp-ui. Skim/proxy paths must report 0
    //! because ffmpeg autorotate consumes the source's display matrix
    //! during transcode; reapplying source_rotation would double-rotate
    //! (the symptom: portrait phone clips played sideways).
    use super::*;
    use klipp_core::{Fps, MediaId, RationalTime, Resolution};

    fn item_with(source_rotation: i32, skim: Option<&str>, proxy: Option<&str>) -> MediaItem {
        MediaItem {
            id: MediaId::new(),
            path: PathBuf::from("/tmp/orig.mp4"),
            kind: MediaItemKind::File,
            filename: "orig.mp4".into(),
            duration: RationalTime { value: 240, rate: 24 },
            fps: Fps::film_24(),
            resolution: Resolution { width: 1920, height: 1080 },
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            has_audio: true,
            file_size_bytes: 1_000_000,
            sample_rate: None,
            audio_channels: None,
            offline: false,
            is_still: false,
            proxy_path: proxy.map(PathBuf::from),
            proxy_ready: proxy.is_some(),
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: Vec::new(),
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            parent_media_id: None,
            added_at: 0,
            fingerprint: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: ColorLabel::default(),
            colour_space: ColourSpace::default(),
            log_format: klipp_core::LogFormat::default(),
            source_rotation,
            camera_make: None,
            camera_model: None,
            encoder: None,
            bit_depth: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            color_primaries: None,
            color_transfer: None,
            pixel_format: None,
            detected_log_format: klipp_core::LogFormat::default(),
            log_format_user_set: false,
            skim_file: skim.map(PathBuf::from),
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            audio_role: None,
            span: None,
        }
    }

    #[test]
    fn original_path_returns_source_rotation() {
        let item = item_with(90, Some("/tmp/skim.mkv"), None);
        assert_eq!(item.effective_rotation_for(&PathBuf::from("/tmp/orig.mp4")), 90);
    }

    #[test]
    fn skim_path_returns_zero_after_transcode_autorotate() {
        // Portrait sources are routed through libx264 + CPU autorotate
        // at skim transcode time, so the on-disk skim file has upright
        // pixels and needs no further rotation at read time. Cache key
        // bump in `app.rs` (`.r{deg}` marker) ensures we never read a
        // stale pre-autorotate sideways skim with this assumption.
        let item = item_with(90, Some("/tmp/skim.mkv"), None);
        assert_eq!(item.effective_rotation_for(&PathBuf::from("/tmp/skim.mkv")), 0);
    }

    #[test]
    fn proxy_path_returns_zero() {
        // Same autorotate-at-transcode contract as skim.
        let item = item_with(180, None, Some("/tmp/proxy.mov"));
        assert_eq!(item.effective_rotation_for(&PathBuf::from("/tmp/proxy.mov")), 0);
    }

    #[test]
    fn unrelated_path_returns_source_rotation() {
        let item = item_with(270, Some("/tmp/skim.mkv"), None);
        assert_eq!(item.effective_rotation_for(&PathBuf::from("/somewhere/else.mp4")), 270);
    }

    #[test]
    fn no_rotation_metadata_returns_zero_for_any_path() {
        let item = item_with(0, Some("/tmp/skim.mkv"), None);
        assert_eq!(item.effective_rotation_for(&PathBuf::from("/tmp/orig.mp4")), 0);
        assert_eq!(item.effective_rotation_for(&PathBuf::from("/tmp/skim.mkv")), 0);
    }
}

/// Whether a fresh probe of `fresh` describes the same file `old` was
/// probed from: same path, same size, same modification time.
pub fn same_file_as_probed(old: &MediaItem, fresh: &MediaItem) -> bool {
    old.path == fresh.path
        && old.file_size_bytes == fresh.file_size_bytes
        && old.file_modified_at == fresh.file_modified_at
}

impl MediaItem {
    /// Whether the editor left a marker of this kind on the clip.
    pub fn has_marker(&self, kind: klipp_core::MediaMarkerKind) -> bool {
        self.markers.iter().any(|m| m.kind == kind)
    }

    /// The reel the clip goes under: the editor's, or the file's own.
    pub fn reel(&self) -> Option<&str> {
        if !self.log.reel.is_empty() {
            Some(self.log.reel.as_str())
        } else {
            self.shot.reel_name.as_deref()
        }
    }

    /// Whether the item's file should be probed again for its shot
    /// metadata: a file on disk whose metadata was never read.
    pub fn wants_metadata_probe(&self) -> bool {
        matches!(self.kind, MediaItemKind::File) && !self.offline && !self.metadata_read && self.path.is_file()
    }
}

/// A duration for people: "5.9 s", "1:24", "9:15" — minutes and seconds
/// past a minute, seconds with a decimal under it.
pub fn describe_secs(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let whole = secs.round() as u64;
        format!("{}:{:02}", whole / 60, whole % 60)
    }
}

/// Carry what the user and the app know about `old` over to `fresh`, a
/// re-probe of the same media item (a relink, the open's re-read, or a
/// file edited outside Klipp and reloaded). The probe supplies the file's
/// facts; this keeps everything the file cannot tell us — rating, tags,
/// notes, bin, colour label, log-format choice, in/out defaults, when it
/// was added, what it is a subclip of — and, when the file itself is
/// unchanged, what was made from it: the caches, the screen text and
/// sound role analysis found, and a still's length. A changed or
/// different file gets none of those: they were made from other pixels.
///
/// It used to replace the item wholesale, so relinking a clip cost its
/// rating, tags and bin; and a proxy keyed by media id survived a relink
/// to a different file. Leaving out the screen text and sound role had
/// every open's re-read blank them, which marked the project unsaved
/// (#2626). Every field is named below, so a new one cannot be left out
/// without choosing which side it is on.
pub fn merge_reprobed(old: &MediaItem, fresh: &mut MediaItem) {
    let same_file = same_file_as_probed(old, fresh);
    let MediaItem {
        // The file's facts, as the probe found them.
        id: _,
        path: _,
        filename: _,
        duration,
        fps,
        resolution: _,
        video_codec: _,
        audio_codec: _,
        has_audio: _,
        file_size_bytes: _,
        sample_rate: _,
        audio_channels: _,
        offline: _,
        is_still,
        fingerprint: _,
        file_modified_at: _,
        media_created_at: _,
        colour_space: _,
        source_rotation: _,
        camera_make: _,
        camera_model: _,
        encoder: _,
        bit_depth: _,
        video_bitrate_kbps: _,
        audio_bitrate_kbps: _,
        color_primaries: _,
        color_transfer: _,
        pixel_format: _,
        detected_log_format: _,
        shot: _,
        metadata_read: _,
        truncated_from_secs: _,
        needs_reprobe: _,
        // What the user and the app said about it.
        rating,
        tags,
        user_notes,
        bin,
        bin_id,
        color_label,
        log_format,
        log_format_user_set,
        default_source_in,
        default_source_out,
        added_at,
        parent_media_id,
        kind,
        log,
        synced_audio,
        markers,
        span,
        // Made from the file: kept while it is the same file.
        proxy_path,
        proxy_ready,
        skim_file,
        waveform_data,
        audio_cache,
        screen_text,
        audio_role,
    } = old;
    fresh.rating = *rating;
    fresh.tags = tags.clone();
    fresh.user_notes = user_notes.clone();
    fresh.bin = bin.clone();
    fresh.bin_id = *bin_id;
    fresh.color_label = *color_label;
    fresh.log_format = *log_format;
    fresh.log_format_user_set = *log_format_user_set;
    fresh.default_source_in = *default_source_in;
    fresh.default_source_out = *default_source_out;
    fresh.added_at = *added_at;
    fresh.parent_media_id = *parent_media_id;
    fresh.kind = kind.clone();
    fresh.log = log.clone();
    fresh.synced_audio = synced_audio.clone();
    fresh.markers = markers.clone();
    fresh.span = *span;
    if same_file {
        fresh.proxy_path = proxy_path.clone();
        fresh.proxy_ready = *proxy_ready;
        fresh.skim_file = skim_file.clone();
        fresh.waveform_data = waveform_data.clone();
        fresh.audio_cache = audio_cache.clone();
        fresh.screen_text = screen_text.clone();
        fresh.audio_role = *audio_role;
        // A still's length and rate are the editor's, not the file's:
        // the probe gives the default ones.
        if *is_still && fresh.is_still {
            fresh.duration = *duration;
            fresh.fps = *fps;
        }
    }
    fresh.needs_reprobe = false;
}

impl MediaItem {
    /// A fully populated item for tests, at `path`, with an optional
    /// ready proxy.
    #[doc(hidden)]
    pub fn synthetic_for_test(path: std::path::PathBuf, proxy: Option<&str>) -> MediaItem {
        MediaItem {
            id: MediaId::new(),
            filename: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            path,
            kind: MediaItemKind::File,
            duration: klipp_core::RationalTime { value: 240, rate: 24 },
            fps: klipp_core::Fps::film_24(),
            resolution: klipp_core::Resolution { width: 1920, height: 1080 },
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            has_audio: true,
            file_size_bytes: 1_000_000,
            sample_rate: None,
            audio_channels: None,
            offline: false,
            is_still: false,
            proxy_path: proxy.map(std::path::PathBuf::from),
            proxy_ready: proxy.is_some(),
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: Vec::new(),
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            added_at: 0,
            fingerprint: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: ColorLabel::default(),
            colour_space: ColourSpace::default(),
            log_format: klipp_core::LogFormat::default(),
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            source_rotation: 0,
            camera_make: None,
            camera_model: None,
            encoder: None,
            bit_depth: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            color_primaries: None,
            color_transfer: None,
            pixel_format: None,
            detected_log_format: klipp_core::LogFormat::default(),
            log_format_user_set: false,
            parent_media_id: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            audio_role: None,
            span: None,
        }
    }
}

#[cfg(test)]
mod reprobe_tests {
    use super::*;

    #[test]
    fn a_reprobe_keeps_what_the_user_said_and_drops_caches_of_another_file() {
        let mut old = MediaItem::synthetic_for_test(std::path::PathBuf::from("/media/a.mp4"), Some("/cache/id.mp4"));
        old.rating = 4;
        old.tags = vec!["hero".into()];
        old.user_notes = "keep".into();
        old.log_format_user_set = true;
        old.skim_file = Some("/cache/skim.mkv".into());
        old.file_size_bytes = 10;
        old.file_modified_at = Some(1);

        // Same file, re-probed: everything survives.
        let mut same = MediaItem::synthetic_for_test(std::path::PathBuf::from("/media/a.mp4"), None);
        same.file_size_bytes = 10;
        same.file_modified_at = Some(1);
        same.needs_reprobe = true;
        merge_reprobed(&old, &mut same);
        assert_eq!((same.rating, same.tags.len(), same.user_notes.as_str()), (4, 1, "keep"));
        assert!(same.log_format_user_set && !same.needs_reprobe);
        assert!(same.proxy_ready && same.proxy_path.is_some() && same.skim_file.is_some(), "the caches of the unchanged file stay");

        // Relinked to another file: the user's metadata survives, the
        // caches made from the old pixels do not.
        let mut other = MediaItem::synthetic_for_test(std::path::PathBuf::from("/media/b.mp4"), None);
        other.file_size_bytes = 20;
        merge_reprobed(&old, &mut other);
        assert_eq!(other.rating, 4);
        assert!(!other.proxy_ready && other.proxy_path.is_none() && other.skim_file.is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A media source's display step (#1857) takes the item's transfer:
    /// its camera log when it decodes as one, PQ or HLG by its colour
    /// space, none for an SDR file (the control).
    #[test]
    fn an_items_scene_transfer_is_how_its_frames_decode() {
        use klipp_color::hdr::HdrTransfer;
        let mut item = MediaItem::synthetic_for_test(PathBuf::from("plate.mov"), None);
        assert_eq!(item.scene_transfer(), None, "an SDR file");
        assert!(!item.scene_referred());
        item.colour_space = ColourSpace::Hlg;
        assert_eq!(item.scene_transfer(), Some(HdrTransfer::Hlg));
        item.colour_space = ColourSpace::Hdr10;
        assert_eq!(item.scene_transfer(), Some(HdrTransfer::Pq));
        assert!(item.scene_referred());
        item.colour_space = ColourSpace::Unknown;
        item.log_format = klipp_core::LogFormat::SLog3;
        item.log_format_user_set = true;
        assert_eq!(item.scene_transfer(), Some(HdrTransfer::Log(klipp_core::ColorTransfer::SLog3)));
    }

    #[test]
    fn untagged_eight_bit_h264_haze_is_not_enough_to_call_camera_log() {
        use klipp_color::measure::{looks_like_log, FrameMeasure};
        // #1569: the two reported aerials have these range/chroma readings,
        // 8-bit yuv420p H.264, and an Unspecified transfer. The remaining
        // histogram values deliberately satisfy the broad log heuristic.
        for (range, chroma) in [(27.0, 8.0), (49.0, 9.0)] {
            let mut item = MediaItem::synthetic_for_test(PathBuf::from("aerial.mp4"), None);
            item.video_codec = "H264".into();
            item.bit_depth = Some(8);
            item.pixel_format = Some("yuv420p".into());
            item.color_transfer = Some("Unspecified".into());
            let reading = FrameMeasure {
                contrast_range: range,
                chroma_mean: chroma,
                l_p1: 20.0,
                l_p99: 80.0,
                log_prior: item.log_prior(),
                ..Default::default()
            };
            assert!(!looks_like_log(&reading), "ordinary haze was rejected: {reading:?}");
        }
    }

    #[test]
    fn eight_bit_video_can_still_have_a_log_like_histogram() {
        use klipp_color::measure::{looks_like_log, FrameMeasure, LogPrior};
        let mut item = MediaItem::synthetic_for_test(PathBuf::from("camera.mp4"), None);
        item.bit_depth = Some(8);
        item.pixel_format = Some("yuv420p".into());
        let reading = FrameMeasure {
            contrast_range: 49.0, chroma_mean: 2.0, l_p1: 20.0, l_p99: 75.0,
            log_prior: item.log_prior(), ..Default::default()
        };
        assert!(looks_like_log(&reading), "8-bit camera log must remain detectable");

        // Missing/contradictory probe data and explicit non-display encodings
        // must not acquire the consumer-video prior from a filename alone.
        for (depth, format, transfer, log) in [
            (None, None, None, klipp_core::LogFormat::Srgb),
            (Some(10), Some("yuv420p"), None, klipp_core::LogFormat::Srgb),
            (Some(8), Some("yuv420p"), Some("log100"), klipp_core::LogFormat::Srgb),
            (Some(8), Some("yuv420p"), None, klipp_core::LogFormat::SLog3),
        ] {
            item.bit_depth = depth;
            item.pixel_format = format.map(str::to_string);
            item.color_transfer = transfer.map(str::to_string);
            item.detected_log_format = log;
            assert_eq!(item.log_prior(), LogPrior::Unknown);
        }
    }

    /// #504: an 8-bit iPhone clip can never be log; one not probed yet,
    /// or a 10-bit one (Apple Log), is not ruled out (#607 review); a
    /// display transfer tag is said.
    #[test]
    fn a_file_says_what_it_can_be() {
        use klipp_color::measure::LogPrior;
        let iphone = |depth: Option<u8>, pf: Option<&str>, trc: Option<&str>| {
            let mut item = MediaItem::synthetic_for_test(PathBuf::from("/tmp/IMG_0001.MOV"), None);
            (item.camera_make, item.camera_model) = (Some("Apple".into()), Some("iPhone 15 Pro".into()));
            item.bit_depth = depth;
            item.pixel_format = pf.map(str::to_string);
            item.color_transfer = trc.map(str::to_string);
            item.log_prior()
        };
        assert_eq!(iphone(Some(8), None, Some("bt709")), LogPrior::NeverLog);
        assert_eq!(iphone(None, Some("yuv420p"), None), LogPrior::NeverLog);
        assert_eq!(iphone(None, None, None), LogPrior::Unknown, "not probed yet");
        assert_eq!(iphone(Some(10), Some("yuv420p10le"), None), LogPrior::Unknown, "Apple Log is 10-bit");
        assert_eq!(iphone(Some(10), Some("yuv420p10le"), Some("bt709")), LogPrior::DisplayTagged);
    }

    fn ten_bit_hevc(make: &str, model: &str, encoder: &str, trc: Option<&str>) -> MediaItem {
        let mut item = MediaItem::synthetic_for_test(std::path::PathBuf::from("/tmp/c.mp4"), None);
        item.camera_make = (!make.is_empty()).then(|| make.to_string());
        item.camera_model = (!model.is_empty()).then(|| model.to_string());
        item.encoder = (!encoder.is_empty()).then(|| encoder.to_string());
        item.bit_depth = Some(10);
        item.pixel_format = Some("yuv420p10le".into());
        item.color_transfer = trc.map(str::to_string);
        item
    }

    #[test]
    fn a_bt709_tagged_ten_bit_sony_clip_is_sdr() {
        use klipp_core::LogFormat;
        for trc in ["BT.709", "bt709"] {
            let item = ten_bit_hevc("Sony", "ILCE-6700", "HEVC Coding", Some(trc));
            assert_eq!(item.detect_log_format(), LogFormat::Srgb, "{trc}");
        }
        let item = ten_bit_hevc("", "", "HEVC Coding", Some("BT.709"));
        assert_eq!(item.detect_log_format(), LogFormat::Srgb, "encoder guess");
    }

    #[test]
    fn an_untagged_ten_bit_sony_clip_is_still_slog3() {
        use klipp_core::LogFormat;
        for trc in [None, Some("Unspecified"), Some("unknown")] {
            let item = ten_bit_hevc("Sony", "", "", trc);
            assert_eq!(item.detect_log_format(), LogFormat::SLog3, "{trc:?}");
            let item = ten_bit_hevc("", "", "HEVC Coding", trc);
            assert_eq!(item.detect_log_format(), LogFormat::SLog3, "{trc:?}");
        }
    }

    /// #1225: a 10-bit clip from a Leica camera is L-Log, Leica's only
    /// log, in BT.709 primaries from the SL (Typ 601) (#1642); 8-bit from
    /// it stays SDR.
    #[test]
    fn a_leica_clip_is_l_log() {
        use klipp_core::{ColorPrimaries, LogFormat};
        for model in ["LEICA SL2-S", "LEICA SL3", "LEICA Q3"] {
            let item = ten_bit_hevc("Leica Camera AG", model, "", None);
            assert_eq!(item.detect_log_format(), LogFormat::LeicaLLog, "{model}");
        }
        let typ601 = ten_bit_hevc("Leica Camera AG", "LEICA SL (Typ 601)", "", None);
        assert_eq!(typ601.detect_log_format(), LogFormat::LeicaLLogBt709);
        assert_eq!(LogFormat::LeicaLLogBt709.camera_colourspace().map(|(_, p)| p), Some(ColorPrimaries::Bt709));
        assert_eq!(LogFormat::LeicaLLog.camera_colourspace().map(|(_, p)| p), Some(ColorPrimaries::Bt2020));
        let mut sdr = ten_bit_hevc("Leica Camera AG", "LEICA SL3", "", None);
        sdr.bit_depth = Some(8);
        sdr.pixel_format = Some("yuv420p".into());
        assert_eq!(sdr.detect_log_format(), LogFormat::Srgb);
    }

    #[test]
    fn an_iphone_clip_follows_its_transfer_tag() {
        use klipp_core::LogFormat;
        let sdr = ten_bit_hevc("Apple", "iPhone 15 Pro", "", Some("BT.709"));
        assert_eq!(sdr.detect_log_format(), LogFormat::Srgb);
        let hlg = ten_bit_hevc("Apple", "iPhone 15 Pro", "", Some("HLG (ARIB STD-B67)"));
        assert_eq!(hlg.detect_log_format(), LogFormat::HLG);
        let untagged = ten_bit_hevc("Apple", "iPhone 15 Pro", "", None);
        assert_eq!(untagged.detect_log_format(), LogFormat::AppleLog);
    }

    #[test]
    fn every_sdr_transfer_name_is_recognised() {
        for trc in ["BT.709", "Gamma 2.2", "Gamma 2.8", "SMPTE 170M", "sRGB", "BT.2020 10-bit",
            "BT.2020 12-bit", "smpte170m", "bt470bg", "iec61966-2-1", "gamma22", "bt2020-10"]
        {
            assert!(is_sdr_transfer(&trc.to_lowercase()), "{trc}");
        }
        for trc in ["Unspecified", "unknown", "PQ (ST 2084)", "Linear", ""] {
            assert!(!is_sdr_transfer(&trc.to_lowercase()), "{trc}");
        }
    }

    #[test]
    fn a_duration_reads_as_seconds_under_a_minute_and_minutes_past_it() {
        assert_eq!(describe_secs(5.93), "5.9 s");
        assert_eq!(describe_secs(84.0), "1:24");
        assert_eq!(describe_secs(554.97), "9:15");
    }

    #[test]
    fn only_an_online_file_whose_metadata_was_never_read_wants_a_probe() {
        let dir = std::env::temp_dir().join(format!("klipp-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("clip.mp4");
        std::fs::write(&file, b"").unwrap();
        let mut item = MediaItem::synthetic_for_test(file.clone(), None);
        item.kind = MediaItemKind::File;
        item.metadata_read = false;
        item.offline = false;
        assert!(item.wants_metadata_probe(), "an online file never read");
        item.metadata_read = true;
        assert!(!item.wants_metadata_probe(), "read once is enough");
        item.metadata_read = false;
        item.offline = true;
        assert!(!item.wants_metadata_probe(), "an offline file cannot be read");
        item.offline = false;
        item.path = dir.join("gone.mp4");
        assert!(!item.wants_metadata_probe(), "a missing file cannot be read");
        // An item saved before the field existed loads as never read.
        let json = serde_json::to_string(&item).unwrap().replace("\"metadata_read\":false,", "");
        assert!(!json.contains("metadata_read"));
        let back: MediaItem = serde_json::from_str(&json).unwrap();
        assert!(!back.metadata_read);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every field of a media item is named here as what an undone relink
    /// does with it (#2327), so a new one can't be missed: this stops
    /// compiling until it is put in a group, and a probed one must be
    /// copied by `take_probed_details` too, which the second half checks.
    #[test]
    fn every_field_is_probed_the_persons_or_the_relinks() {
        let item = MediaItem::synthetic_for_test("/tmp/fields.mov".into(), None);
        let MediaItem {
            // Set by the relink itself, path and offline flag both ways.
            id: _, path: _, offline: _,
            // Read from the file: given back by `take_probed_details`.
            filename: _, duration: _, fps: _, resolution: _, video_codec: _, audio_codec: _, has_audio: _, file_size_bytes: _,
            sample_rate: _, audio_channels: _, is_still: _, fingerprint: _, file_modified_at: _, media_created_at: _,
            colour_space: _, source_rotation: _, camera_make: _, camera_model: _, encoder: _, bit_depth: _,
            video_bitrate_kbps: _, audio_bitrate_kbps: _, color_primaries: _, color_transfer: _, pixel_format: _,
            detected_log_format: _, shot: _, metadata_read: _, truncated_from_secs: _,
            // The person's, or the project's: kept.
            kind: _, default_source_in: _, default_source_out: _, rating: _, tags: _, user_notes: _, bin: _, bin_id: _,
            parent_media_id: _, added_at: _, color_label: _, log_format: _, log_format_user_set: _, log: _,
            synced_audio: _, markers: _, screen_text: _, audio_role: _, span: _,
            // Made from the file, and made again by the relink's own
            // pictures and proxies: left to that.
            proxy_path: _, proxy_ready: _, skim_file: _, needs_reprobe: _, waveform_data: _, audio_cache: _,
        } = item.clone();

        // The probed group, each made to differ, comes over.
        let mut from = item.clone();
        from.filename = "other.mov".into();
        from.duration = klipp_core::RationalTime::from_secs_f64(9.0, 24);
        from.video_codec = "prores".into();
        from.file_size_bytes = 99;
        from.metadata_read = !item.metadata_read;
        from.rating = 5;
        let mut to = item.clone();
        to.take_probed_details(&from);
        assert_eq!((to.filename.as_str(), to.duration, to.video_codec.as_str(), to.file_size_bytes, to.metadata_read), ("other.mov", from.duration, "prores", 99, from.metadata_read));
        assert_eq!(to.rating, item.rating, "the person's kept");
    }
}
