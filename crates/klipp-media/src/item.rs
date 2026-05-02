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
    /// Camera log encoding format (for IDT conversion)
    #[serde(default)]
    pub log_format: klipp_core::LogFormat,
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
    /// True when this item is a subclip — a zone-within-parent virtual
    /// reference. Subclips share the parent's `path` but carry their own
    /// `default_source_in/out` describing the zone.
    pub fn is_subclip(&self) -> bool {
        self.parent_media_id.is_some()
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
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
        }
    }

    /// Detect the likely log format from metadata (color_transfer, camera, bit depth, pixel format).
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
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
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
