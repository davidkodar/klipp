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

        LogFormat::Srgb
    }
}
