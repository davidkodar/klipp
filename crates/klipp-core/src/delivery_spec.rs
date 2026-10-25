//! Delivery specifications as data (#2650): what a platform or a
//! broadcaster asks a finished piece to be, and a check of a piece
//! against one.
//!
//! A spec holds the export settings that picking it sets, and its
//! requirements: container, codec, frame size and rate, bit depth,
//! colour (SDR or an HDR transfer), video bitrate, audio codec, sample
//! rate and channels, loudness and true peak, captions. The check reads
//! the facts of a piece — a finished file, or the timeline as it would
//! be exported — and gives every requirement as pass, warn or fail, with
//! what was found, what is wanted, and what to change.
//!
//! The built-in specs' numbers come from each platform's or standards
//! body's own public page, cited beside them. Where a platform states a
//! recommendation rather than a requirement, missing it is a warning.

use crate::events::{ExportAudioCodec, ExportFormat, ExportVideoCodec, HdrMode};
use crate::export_preset::ExportPresetSettings;
use crate::types::Resolution;
use serde::{Deserialize, Serialize};

/// A delivery specification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeliverySpec {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub built_in: bool,
    /// Where the numbers come from: the public pages they were read on.
    #[serde(default)]
    pub sources: Vec<String>,
    /// What picking the spec sets on the Export page.
    pub export: ExportPresetSettings,
    /// The colour the export is made in.
    #[serde(default)]
    pub hdr: HdrMode,
    pub requirements: Requirements,
}

/// What a delivery has to be. An empty list or `None` asks nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Requirements {
    pub containers: Vec<ExportFormat>,
    pub video_codecs: Vec<ExportVideoCodec>,
    /// The frame sizes accepted, any one of them.
    pub frame_sizes: Vec<Resolution>,
    /// The frame's shape, width to height, e.g. 9:16.
    pub aspect: Option<(u32, u32)>,
    /// At least this many lines.
    pub min_height: Option<u32>,
    /// The frame rates accepted, in frames a second.
    pub frame_rates: Vec<f64>,
    /// Or any frame rate from the first to the second.
    pub fps_range: Option<(f64, f64)>,
    /// At most this many pixels on the longer side.
    pub max_side: Option<u32>,
    /// At least this many bits a channel.
    pub min_bit_depth: Option<u8>,
    /// The colour the picture is in.
    pub transfer: Option<Transfer>,
    /// At most this video bitrate, kbps.
    pub max_video_kbps: Option<u32>,
    pub audio_codecs: Vec<ExportAudioCodec>,
    pub sample_rates: Vec<u32>,
    /// The channel counts accepted.
    pub channels: Vec<u8>,
    pub loudness: Option<Loudness>,
    pub captions: Captions,
    /// Whether the size, rate, bitrate and audio asked are what the
    /// platform recommends rather than requires: missing them warns.
    pub recommended: bool,
}

/// The colour of a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transfer {
    /// Rec. 709 or sRGB standard dynamic range.
    Sdr,
    /// HDR in PQ (SMPTE ST 2084) or HLG; either.
    Hdr,
    Pq,
    Hlg,
}

impl Transfer {
    pub fn label(self) -> &'static str {
        match self {
            Transfer::Sdr => "SDR",
            Transfer::Hdr => "HDR (PQ or HLG)",
            Transfer::Pq => "HDR PQ",
            Transfer::Hlg => "HDR HLG",
        }
    }

    /// Whether `found` is this.
    fn accepts(self, found: Transfer) -> bool {
        self == found || (self == Transfer::Hdr && matches!(found, Transfer::Pq | Transfer::Hlg))
    }
}

/// A loudness requirement.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Loudness {
    /// Integrated loudness, LUFS (LKFS).
    pub target: f32,
    /// How far either side of the target is still in, LU.
    pub tolerance: f32,
    /// The highest true peak, dBTP.
    pub true_peak_max: f32,
    /// Measured on dialogue only (dialogue-gated), which Klipp's meter is
    /// not: its reading is a guide, and a miss warns rather than fails.
    #[serde(default)]
    pub dialogue_gated: bool,
}

/// Whether captions are asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Captions {
    #[default]
    Any,
    Recommended,
    Required,
}

/// What a piece is: a finished file read, or the timeline with its export
/// settings. `None` is not known.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    pub container: Option<ExportFormat>,
    pub video_codec: Option<ExportVideoCodec>,
    /// The codec's name when it is none Klipp writes.
    pub video_codec_name: Option<String>,
    pub size: Option<Resolution>,
    pub fps: Option<f64>,
    pub bit_depth: Option<u8>,
    pub transfer: Option<Transfer>,
    pub video_kbps: Option<u32>,
    pub audio_codec: Option<ExportAudioCodec>,
    pub audio_codec_name: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub integrated_lufs: Option<f32>,
    pub true_peak_dbtp: Option<f32>,
    pub captions: Option<bool>,
}

/// How a requirement came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

/// One requirement, checked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// What is asked, e.g. "Frame rate".
    pub what: String,
    pub status: Status,
    pub found: String,
    pub wanted: String,
    /// What to change; empty when it passes.
    pub fix: String,
}

/// `facts` against `spec`: every requirement the spec has, in order.
pub fn check(spec: &DeliverySpec, facts: &Facts) -> Vec<Finding> {
    let r = &spec.requirements;
    let soft = if r.recommended { Status::Warn } else { Status::Fail };
    let mut out = Vec::new();
    let mut push = |what: &str, ok: Option<bool>, miss: Status, found: String, wanted: String, fix: &str| {
        let status = match ok {
            Some(true) => Status::Pass,
            Some(false) => miss,
            // Not known: said, not failed.
            None => Status::Warn,
        };
        let fix = if status == Status::Pass { String::new() } else if ok.is_none() { "Could not be read; check it by hand.".into() } else { fix.to_string() };
        out.push(Finding { what: what.into(), status, found, wanted, fix });
    };
    let unknown = || "not known".to_string();
    if !r.containers.is_empty() {
        let found = facts.container.map(|c| c.label().to_string()).unwrap_or_else(unknown);
        push("Container", facts.container.map(|c| r.containers.contains(&c)), Status::Fail, found, list(r.containers.iter().map(|c| c.label())), "Export in one of these containers.");
    }
    if !r.video_codecs.is_empty() {
        let found = facts.video_codec.map(|c| c.label().to_string()).or_else(|| facts.video_codec_name.clone()).unwrap_or_else(unknown);
        let ok = facts.video_codec.map(|c| r.video_codecs.contains(&c)).or(facts.video_codec_name.as_ref().map(|_| false));
        push("Video codec", ok, Status::Fail, found, list(r.video_codecs.iter().map(|c| c.label())), "Export with one of these codecs.");
    }
    if !r.frame_sizes.is_empty() {
        let found = facts.size.map(size_text).unwrap_or_else(unknown);
        push("Frame size", facts.size.map(|s| r.frame_sizes.contains(&s)), soft, found, list(r.frame_sizes.iter().map(|s| size_text(*s))), "Set the export's frame size to one of these.");
    }
    if let Some((w, h)) = r.aspect {
        let found = facts.size.map(size_text).unwrap_or_else(unknown);
        let ok = facts.size.map(|s| (s.width as u64 * h as u64).abs_diff(s.height as u64 * w as u64) <= (s.height as u64 * w as u64) / 100);
        push("Frame shape", ok, Status::Fail, found, format!("{w}:{h}"), "Reframe the sequence to this shape.");
    }
    if let Some(min) = r.min_height {
        let found = facts.size.map(size_text).unwrap_or_else(unknown);
        push("Resolution", facts.size.map(|s| s.height.min(s.width) >= min), soft, found, format!("at least {min} lines"), "Export at a larger frame size.");
    }
    if !r.frame_rates.is_empty() {
        let found = facts.fps.map(fps_text).unwrap_or_else(unknown);
        let ok = facts.fps.map(|f| r.frame_rates.iter().any(|w| (w - f).abs() < 0.01));
        push("Frame rate", ok, Status::Fail, found, list(r.frame_rates.iter().map(|f| fps_text(*f))), "Export at one of these frame rates; a sequence shot at another needs conforming.");
    }
    if let Some((lo, hi)) = r.fps_range {
        let found = facts.fps.map(fps_text).unwrap_or_else(unknown);
        push("Frame rate", facts.fps.map(|f| f >= lo - 0.01 && f <= hi + 0.01), Status::Fail, found, format!("{} to {}", fps_text(lo), fps_text(hi)), "Export at a frame rate in this range.");
    }
    if let Some(max) = r.max_side {
        let found = facts.size.map(size_text).unwrap_or_else(unknown);
        push("Largest side", facts.size.map(|s| s.width.max(s.height) <= max), Status::Fail, found, format!("at most {max} pixels"), "Export at a smaller frame size.");
    }
    if let Some(min) = r.min_bit_depth {
        let found = facts.bit_depth.map(|b| format!("{b}-bit")).unwrap_or_else(unknown);
        push("Bit depth", facts.bit_depth.map(|b| b >= min), Status::Fail, found, format!("{min}-bit or more"), "Export at 10-bit (or ProRes).");
    }
    if let Some(t) = r.transfer {
        let found = facts.transfer.map(|f| f.label().to_string()).unwrap_or_else(unknown);
        push("Colour", facts.transfer.map(|f| t.accepts(f)), Status::Fail, found, t.label().to_string(), if t == Transfer::Sdr { "Export in SDR." } else { "Export in HDR (Export → HDR)." });
    }
    if let Some(max) = r.max_video_kbps {
        let found = facts.video_kbps.map(|k| format!("{k} kbps")).unwrap_or_else(unknown);
        push("Video bitrate", facts.video_kbps.map(|k| k <= max), soft, found, format!("at most {max} kbps"), "Lower the export's bitrate.");
    }
    if !r.audio_codecs.is_empty() {
        let found = facts.audio_codec.map(|c| c.label().to_string()).or_else(|| facts.audio_codec_name.clone()).unwrap_or_else(unknown);
        let ok = facts.audio_codec.map(|c| r.audio_codecs.contains(&c)).or(facts.audio_codec_name.as_ref().map(|_| false));
        push("Audio codec", ok, soft, found, list(r.audio_codecs.iter().map(|c| c.label())), "Export the sound with one of these codecs.");
    }
    if !r.sample_rates.is_empty() {
        let found = facts.sample_rate.map(|s| format!("{} kHz", s as f32 / 1000.0)).unwrap_or_else(unknown);
        push("Sample rate", facts.sample_rate.map(|s| r.sample_rates.contains(&s)), soft, found, list(r.sample_rates.iter().map(|s| format!("{} kHz", *s as f32 / 1000.0))), "Export the sound at this sample rate.");
    }
    if !r.channels.is_empty() {
        let found = facts.channels.map(channels_text).unwrap_or_else(unknown);
        push("Audio channels", facts.channels.map(|c| r.channels.contains(&c)), soft, found, list(r.channels.iter().map(|c| channels_text(*c))), "Mix to this channel layout.");
    }
    if let Some(l) = r.loudness {
        // A dialogue-gated target is read here over the whole mix: a guide.
        let miss = if l.dialogue_gated { Status::Warn } else { Status::Fail };
        let found = facts.integrated_lufs.map(|v| format!("{v:.1} LUFS{}", if l.dialogue_gated { " (whole mix, not dialogue-gated)" } else { "" })).unwrap_or_else(unknown);
        let ok = facts.integrated_lufs.map(|v| (v - l.target).abs() <= l.tolerance + 0.05);
        push("Loudness", ok, miss, found, format!("{} LUFS \u{00B1}{}{}", l.target, l.tolerance, if l.dialogue_gated { ", dialogue-gated" } else { "" }), "Normalise the export to this loudness (Export → Loudness).");
        let found = facts.true_peak_dbtp.map(|v| format!("{v:.1} dBTP")).unwrap_or_else(unknown);
        push("True peak", facts.true_peak_dbtp.map(|v| v <= l.true_peak_max + 0.05), Status::Fail, found, format!("at most {} dBTP", l.true_peak_max), "Set the export's true-peak ceiling to this.");
    }
    match r.captions {
        Captions::Any => {}
        Captions::Recommended | Captions::Required => {
            let miss = if r.captions == Captions::Required { Status::Fail } else { Status::Warn };
            let found = facts.captions.map(|c| if c { "captions" } else { "none" }.to_string()).unwrap_or_else(unknown);
            push("Captions", facts.captions, miss, found, if miss == Status::Fail { "required" } else { "recommended" }.into(), "Add captions (a subtitle track) and export them with the file or beside it.");
        }
    }
    out
}

/// The worst of `findings`: what the delivery comes to.
pub fn verdict(findings: &[Finding]) -> Status {
    if findings.iter().any(|f| f.status == Status::Fail) {
        Status::Fail
    } else if findings.iter().any(|f| f.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Pass
    }
}

fn list(items: impl Iterator<Item = impl AsRef<str>>) -> String {
    items.map(|i| i.as_ref().to_string()).collect::<Vec<_>>().join(" or ")
}

fn size_text(s: Resolution) -> String {
    format!("{}\u{00D7}{}", s.width, s.height)
}

fn fps_text(f: f64) -> String {
    if (f - f.round()).abs() < 0.001 { format!("{f:.0} fps") } else { format!("{f:.3} fps") }
}

fn channels_text(c: u8) -> String {
    match c {
        1 => "mono".into(),
        2 => "stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n} channels"),
    }
}

/// The facts of the timeline as an export with `export` (and `hdr`)
/// would make it: the frame size and rate the export sets, else the
/// sequence's (`size`, `fps`); whether it has captions; and its sound,
/// normalised to the export's loudness target where it has one, else as
/// the mix measures (`mix`: integrated LUFS and true peak dBTP).
pub fn export_facts(export: &ExportPresetSettings, hdr: HdrMode, size: Resolution, fps: f64, captions: bool, mix: Option<(f32, f32)>) -> Facts {
    let size = export.resolution.unwrap_or(size);
    let fps = export.fps.map(|f| f.as_f64()).unwrap_or(fps);
    let (integrated_lufs, true_peak_dbtp) = match (export.target_lufs, mix) {
        // Normalised there, under the ceiling (-1 when none is set).
        (Some(target), _) => (Some(target), Some(export.true_peak_dbtp.unwrap_or(-1.0))),
        (None, Some((lufs, peak))) => (Some(lufs), Some(peak)),
        (None, None) => (None, None),
    };
    Facts {
        container: Some(export.format),
        video_codec: Some(export.video_codec),
        size: Some(size),
        fps: Some(fps),
        bit_depth: Some(if export.is_10bit() { 10 } else { 8 }),
        transfer: Some(match hdr {
            HdrMode::Sdr => Transfer::Sdr,
            HdrMode::Pq => Transfer::Pq,
            HdrMode::Hlg => Transfer::Hlg,
        }),
        video_kbps: export.video_bitrate_kbps,
        audio_codec: Some(export.audio_codec),
        // The export mixes to 48 kHz stereo.
        sample_rate: Some(48_000),
        channels: Some(2),
        integrated_lufs,
        true_peak_dbtp,
        captions: Some(captions),
        ..Default::default()
    }
}

/// Where a person's own specs are kept:
/// `$XDG_CONFIG_HOME/klipp/delivery-specs.json`, else `~/.config/klipp/`.
pub fn user_specs_path() -> Option<std::path::PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(config.join("klipp").join("delivery-specs.json"))
}

/// A person's own specs; none when there are none or they cannot be read.
pub fn load_user_specs() -> Vec<DeliverySpec> {
    user_specs_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Vec<DeliverySpec>>(&t).ok()).unwrap_or_default().into_iter().map(|mut s| {
        s.built_in = false;
        s
    }).collect()
}

/// Keep `specs` as the person's own, written beside and moved into place.
pub fn save_user_specs(specs: &[DeliverySpec]) -> Result<(), String> {
    let path = user_specs_path().ok_or("no home directory")?;
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mine: Vec<&DeliverySpec> = specs.iter().filter(|s| !s.built_in).collect();
    let text = serde_json::to_string_pretty(&mine).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".delivery-specs.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Every spec: the built-in ones, then the person's own.
pub fn all_specs() -> Vec<DeliverySpec> {
    let mut all = builtin_specs();
    all.extend(load_user_specs());
    all
}

/// The spec named `key`, by id or by name (any case).
pub fn find_spec(key: &str) -> Option<DeliverySpec> {
    let key = key.trim();
    all_specs().into_iter().find(|s| s.id == key || s.name.eq_ignore_ascii_case(key))
}

/// The frame rates the broadcast and streaming specs list.
const BROADCAST_RATES: [f64; 9] = [24000.0 / 1001.0, 24.0, 25.0, 30000.0 / 1001.0, 30.0, 48.0, 50.0, 60000.0 / 1001.0, 60.0];

fn web(format: ExportFormat, video: ExportVideoCodec, size: Option<Resolution>, crf: u32, audio: ExportAudioCodec, audio_kbps: u32, bit_depth: u8, lufs: Option<f32>, peak: Option<f32>) -> ExportPresetSettings {
    ExportPresetSettings {
        format,
        video_codec: video,
        resolution: size,
        fps: None,
        crf,
        preset: crate::events::EncoderPreset::Medium,
        audio_codec: audio,
        audio_bitrate_kbps: Some(audio_kbps),
        video_bitrate_kbps: None,
        rate_control: crate::events::RateControl::Crf,
        target_lufs: lufs,
        true_peak_dbtp: peak,
        bit_depth,
    }
}

fn r(width: u32, height: u32) -> Resolution {
    Resolution { width, height }
}

/// The built-in specs, with fixed ids so a project or a preference that
/// names one keeps it across versions. Every number is from the page
/// cited beside it, read 2026-10-25; a platform's recommendation is
/// checked as one (`recommended`: a miss warns).
pub fn builtin_specs() -> Vec<DeliverySpec> {
    use ExportAudioCodec as A;
    use ExportFormat as F;
    use ExportVideoCodec as V;
    let spec = |id: &str, name: &str, sources: &[&str], export: ExportPresetSettings, hdr: HdrMode, requirements: Requirements| DeliverySpec {
        id: id.into(),
        name: name.into(),
        built_in: true,
        sources: sources.iter().map(|s| s.to_string()).collect(),
        export,
        hdr,
        requirements,
    };
    vec![
        // YouTube's recommended upload settings: MP4, H.264, BT.709, AAC
        // or Opus at 48 kHz, stereo or 5.1; all recommendations. No
        // loudness target is published.
        // https://support.google.com/youtube/answer/1722171
        spec(
            "delivery-youtube-sdr",
            "YouTube (SDR)",
            &["https://support.google.com/youtube/answer/1722171"],
            web(F::Mp4, V::H264, Some(r(1920, 1080)), 18, A::Aac, 384, 0, Some(crate::export_preset::WEB_LOUDNESS_LUFS), None),
            HdrMode::Sdr,
            Requirements {
                containers: vec![F::Mp4],
                video_codecs: vec![V::H264],
                transfer: Some(Transfer::Sdr),
                audio_codecs: vec![A::Aac, A::Opus],
                sample_rates: vec![48_000],
                channels: vec![1, 2, 6],
                recommended: true,
                ..Default::default()
            },
        ),
        // YouTube HDR: PQ or HLG, Rec. 2020, 10- or 12-bit, 720p to 2160p,
        // these frame rates (requirements); HEVC, VP9 or AV1 in MP4, MOV
        // or MKV recommended, ProRes accepted.
        // https://support.google.com/youtube/answer/7126552
        spec(
            "delivery-youtube-hdr",
            "YouTube (HDR)",
            &["https://support.google.com/youtube/answer/7126552", "https://support.google.com/youtube/answer/1722171"],
            web(F::Mp4, V::H265, Some(r(3840, 2160)), 18, A::Aac, 384, 10, Some(crate::export_preset::WEB_LOUDNESS_LUFS), None),
            HdrMode::Pq,
            Requirements {
                containers: vec![F::Mp4, F::Mov, F::Mkv],
                video_codecs: vec![V::H265, V::Vp9, V::Av1, V::ProRes422, V::ProRes422HQ, V::ProRes4444],
                min_height: Some(720),
                frame_rates: BROADCAST_RATES.to_vec(),
                min_bit_depth: Some(10),
                transfer: Some(Transfer::Hdr),
                audio_codecs: vec![A::Aac, A::Opus],
                sample_rates: vec![48_000],
                ..Default::default()
            },
        ),
        // Vimeo's compression guidelines: H.264 High, H.265 or ProRes 422
        // HQ; AAC-LC at 48 kHz; all recommendations.
        // https://help.vimeo.com/hc/en-us/articles/12426043233169-Video-and-audio-compression-guidelines
        spec(
            "delivery-vimeo",
            "Vimeo",
            &["https://help.vimeo.com/hc/en-us/articles/12426043233169-Video-and-audio-compression-guidelines"],
            web(F::Mp4, V::H264, Some(r(1920, 1080)), 18, A::Aac, 320, 0, Some(crate::export_preset::WEB_LOUDNESS_LUFS), None),
            HdrMode::Sdr,
            Requirements {
                video_codecs: vec![V::H264, V::H265, V::ProRes422HQ],
                audio_codecs: vec![A::Aac],
                sample_rates: vec![48_000],
                recommended: true,
                ..Default::default()
            },
        ),
        // Meta's published specs for Instagram Reels: MP4 or MOV, 9:16,
        // 1440×2560, H.264, stereo AAC at 128 kbps or more
        // (recommendations, from its Reels ads guide).
        // https://www.facebook.com/business/ads-guide/update/video/instagram-reels
        spec(
            "delivery-instagram-reels",
            "Instagram Reels (vertical)",
            &["https://www.facebook.com/business/ads-guide/update/video/instagram-reels"],
            web(F::Mp4, V::H264, Some(r(1440, 2560)), 18, A::Aac, 192, 0, Some(crate::export_preset::WEB_LOUDNESS_LUFS), None),
            HdrMode::Sdr,
            Requirements {
                containers: vec![F::Mp4, F::Mov],
                video_codecs: vec![V::H264],
                aspect: Some((9, 16)),
                audio_codecs: vec![A::Aac],
                channels: vec![2],
                recommended: true,
                ..Default::default()
            },
        ),
        // TikTok's published upload limits (its Content Posting API):
        // MP4, WebM or MOV; H.264, H.265 or VP9; 23 to 60 fps; each side
        // 360 to 4096 pixels.
        // https://developers.tiktok.com/doc/content-posting-api-media-transfer-guide
        spec(
            "delivery-tiktok",
            "TikTok (vertical)",
            &["https://developers.tiktok.com/doc/content-posting-api-media-transfer-guide"],
            web(F::Mp4, V::H264, Some(r(1080, 1920)), 18, A::Aac, 192, 0, Some(crate::export_preset::WEB_LOUDNESS_LUFS), None),
            HdrMode::Sdr,
            Requirements {
                containers: vec![F::Mp4, F::WebM, F::Mov],
                video_codecs: vec![V::H264, V::H265, V::Vp9],
                min_height: Some(360),
                max_side: Some(4096),
                fps_range: Some((23.0, 60.0)),
                ..Default::default()
            },
        ),
        // EBU R 128: programme loudness -23.0 LUFS over the whole
        // programme, true peak at most -1 dBTP. Tolerance: EBU's R 128
        // page gives ±0.5 LU; the v5 text keeps ±1.0 LU for live
        // programmes and ±0.2 LU as a measurement tolerance.
        // https://tech.ebu.ch/files/live/sites/tech/files/shared/r/r128.pdf
        // https://tech.ebu.ch/publications/r128
        spec(
            "delivery-ebu-r128",
            "Broadcast: EBU R 128",
            &["https://tech.ebu.ch/files/live/sites/tech/files/shared/r/r128.pdf", "https://tech.ebu.ch/publications/r128"],
            web(F::Mov, V::ProRes422HQ, None, 18, A::Pcm24, 0, 10, Some(-23.0), Some(-1.0)),
            HdrMode::Sdr,
            Requirements { loudness: Some(Loudness { target: -23.0, tolerance: 0.5, true_peak_max: -1.0, dialogue_gated: false }), ..Default::default() },
        ),
        // ATSC A/85: -24 LKFS, about ±2 dB, true peak below -2 dBTP; long
        // programmes measured on dialogue.
        // https://www.atsc.org/atsc-documents/a85-techniques-for-establishing-and-maintaining-audio-loudness/
        spec(
            "delivery-atsc-a85",
            "Broadcast: ATSC A/85",
            &["https://www.atsc.org/atsc-documents/a85-techniques-for-establishing-and-maintaining-audio-loudness/"],
            web(F::Mov, V::ProRes422HQ, None, 18, A::Pcm24, 0, 10, Some(-24.0), Some(-2.0)),
            HdrMode::Sdr,
            Requirements { loudness: Some(Loudness { target: -24.0, tolerance: 2.0, true_peak_max: -2.0, dialogue_gated: true }), ..Default::default() },
        ),
        // A streaming platform's master: the sound at -27 LKFS ±2 LU,
        // dialogue-gated, true peak at most -2 dBTP, 48 kHz 24-bit PCM in
        // stereo or 5.1; the picture HD or UHD at the listed rates,
        // 10-bit. Exported as a ProRes 422 HQ mezzanine; a packaged (IMF)
        // delivery is not made here.
        // https://studiopartner.netflix.net/studio/branded-sound-mix-spec-and-best-practices
        // https://studiopartner.netflix.net/studio/branded-imf-delivery-specifications
        spec(
            "delivery-streaming-master",
            "Streaming master",
            &["https://studiopartner.netflix.net/studio/branded-sound-mix-spec-and-best-practices", "https://studiopartner.netflix.net/studio/branded-imf-delivery-specifications"],
            web(F::Mov, V::ProRes422HQ, Some(r(3840, 2160)), 18, A::Pcm24, 0, 10, Some(-27.0), Some(-2.0)),
            HdrMode::Sdr,
            Requirements {
                frame_sizes: vec![r(3840, 2160), r(1920, 1080)],
                frame_rates: vec![24000.0 / 1001.0, 24.0, 25.0, 30000.0 / 1001.0, 30.0, 50.0, 60000.0 / 1001.0, 60.0],
                min_bit_depth: Some(10),
                audio_codecs: vec![A::Pcm24],
                sample_rates: vec![48_000],
                channels: vec![2, 6],
                loudness: Some(Loudness { target: -27.0, tolerance: 2.0, true_peak_max: -2.0, dialogue_gated: true }),
                ..Default::default()
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str) -> DeliverySpec {
        builtin_specs().into_iter().find(|s| s.id == id).expect("the spec")
    }

    /// Every built-in spec round-trips through JSON, as a user's saved
    /// one does, and has a source.
    #[test]
    fn every_builtin_spec_round_trips() {
        let specs = builtin_specs();
        assert!(specs.len() >= 8);
        for s in &specs {
            let back: DeliverySpec = serde_json::from_str(&serde_json::to_string(s).unwrap()).unwrap();
            assert_eq!(&back, s, "{}", s.name);
            assert!(!s.sources.is_empty(), "{} cites its source", s.name);
        }
        let ids: std::collections::HashSet<_> = specs.iter().map(|s| &s.id).collect();
        assert_eq!(ids.len(), specs.len(), "ids are unique");
    }

    fn youtube_hdr_file() -> Facts {
        Facts {
            container: Some(ExportFormat::Mp4),
            video_codec: Some(ExportVideoCodec::H265),
            size: Some(r(3840, 2160)),
            fps: Some(24000.0 / 1001.0),
            bit_depth: Some(10),
            transfer: Some(Transfer::Pq),
            audio_codec: Some(ExportAudioCodec::Aac),
            sample_rate: Some(48_000),
            channels: Some(2),
            captions: Some(false),
            ..Default::default()
        }
    }

    /// A file that meets a spec passes every requirement.
    #[test]
    fn a_file_that_meets_the_spec_passes() {
        let found = check(&spec("delivery-youtube-hdr"), &youtube_hdr_file());
        assert_eq!(verdict(&found), Status::Pass, "{found:#?}");
    }

    /// One at a frame rate the spec does not list fails, saying what to
    /// change; in SDR it fails on its colour.
    #[test]
    fn a_wrong_frame_rate_or_colour_fails() {
        let mut f = youtube_hdr_file();
        f.fps = Some(23.0);
        let found = check(&spec("delivery-youtube-hdr"), &f);
        let rate = found.iter().find(|x| x.what == "Frame rate").unwrap();
        assert_eq!(rate.status, Status::Fail);
        assert!(rate.fix.contains("frame rate"), "{rate:?}");
        f.fps = Some(25.0);
        f.transfer = Some(Transfer::Sdr);
        let found = check(&spec("delivery-youtube-hdr"), &f);
        assert_eq!(found.iter().find(|x| x.what == "Colour").unwrap().status, Status::Fail);
    }

    /// Loudness off the target fails for EBU R 128; a true peak over the
    /// ceiling fails too. A dialogue-gated target, read over the whole
    /// mix, only warns.
    #[test]
    fn loudness_off_target_fails() {
        let ebu = spec("delivery-ebu-r128");
        let at = |lufs, peak| Facts { integrated_lufs: Some(lufs), true_peak_dbtp: Some(peak), ..Default::default() };
        assert_eq!(verdict(&check(&ebu, &at(-23.2, -1.5))), Status::Pass);
        let loud = check(&ebu, &at(-18.0, -1.5));
        let l = loud.iter().find(|x| x.what == "Loudness").unwrap();
        assert_eq!(l.status, Status::Fail);
        assert!(l.found.contains("-18.0") && l.wanted.contains("-23"), "{l:?}");
        assert_eq!(check(&ebu, &at(-23.0, -0.2)).iter().find(|x| x.what == "True peak").unwrap().status, Status::Fail);
        let master = check(&spec("delivery-streaming-master"), &at(-20.0, -3.0));
        assert_eq!(master.iter().find(|x| x.what == "Loudness").unwrap().status, Status::Warn, "a guide, not a verdict");
    }

    /// Captions a spec requires fail when missing; a user spec can ask.
    #[test]
    fn missing_captions_fail_where_required() {
        let mut s = spec("delivery-vimeo");
        s.requirements.captions = Captions::Required;
        let f = Facts { captions: Some(false), ..Default::default() };
        let c = check(&s, &f).into_iter().find(|x| x.what == "Captions").unwrap();
        assert_eq!(c.status, Status::Fail);
        assert!(c.fix.contains("captions"), "{c:?}");
    }

    /// A recommendation missed warns: Vimeo's codecs, TikTok's range.
    #[test]
    fn a_missed_recommendation_warns_and_a_range_is_checked() {
        let f = Facts { video_codec: Some(ExportVideoCodec::Vp9), audio_codec: Some(ExportAudioCodec::Aac), sample_rate: Some(48_000), ..Default::default() };
        let v = check(&spec("delivery-vimeo"), &f);
        assert_eq!(v.iter().find(|x| x.what == "Video codec").unwrap().status, Status::Fail, "a codec is not a recommendation to bend");
        let t = |fps| Facts { fps: Some(fps), ..Default::default() };
        let tiktok = spec("delivery-tiktok");
        assert_eq!(check(&tiktok, &t(30.0)).iter().find(|x| x.what == "Frame rate").unwrap().status, Status::Pass);
        assert_eq!(check(&tiktok, &t(120.0)).iter().find(|x| x.what == "Frame rate").unwrap().status, Status::Fail);
    }
}
