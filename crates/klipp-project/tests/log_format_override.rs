//! A media item's log format survives save/load, and an explicit choice —
//! including sRGB, the default value — beats detection.
//!
//! The bug this pins: "user chose" was encoded as "log_format is not the
//! default", so choosing sRGB for a clip detected as S-Log3 read as "never
//! chose" and the decoder kept converting it.

use std::path::PathBuf;

use klipp_core::{Fps, LogFormat, MediaId, RationalTime};
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_project::{load_project, save_project_with_color, ColorSettings};
use klipp_timeline::Sequence;

fn project_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_logfmt_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A 10-bit file whose make is Sony: detection calls it S-Log3.
fn make_media_item(path: PathBuf) -> MediaItem {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "media".to_string());
    MediaItem {
        id: MediaId::new(),
        path,
        kind: MediaItemKind::File,
        filename,
        duration: RationalTime::from_frames(120, 24),
        fps: Fps::new(24, 1),
        resolution: klipp_core::Resolution { width: 320, height: 240 },
        video_codec: "hevc".into(),
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
        cache_key: None,
        file_modified_at: None,
        media_created_at: None,
        color_label: klipp_media::ColorLabel::default(),
        colour_space: klipp_media::ColourSpace::default(),
        log_format: klipp_core::LogFormat::default(),
        source_rotation: 0,
        camera_make: Some("Sony".into()),
        camera_model: None,
        encoder: None,
        bit_depth: Some(10),
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

fn round_trip(label: &str, edit: impl FnOnce(&mut MediaItem)) -> MediaItem {
    let dir = project_dir(label);
    let media = dir.join("clip.mp4");
    std::fs::write(&media, b"not really video").unwrap();
    let mut item = make_media_item(media);
    edit(&mut item);
    let mut pool = MediaPool::new();
    pool.add(item);
    save_project_with_color(&dir, &[Sequence::new_default()], &pool, &ColorSettings::default(), None, None, "")
        .unwrap();
    let (_seqs, pool, ..) = load_project(&dir).unwrap();
    let loaded = pool.all().next().expect("media item survives the round trip").clone();
    let _ = std::fs::remove_dir_all(&dir);
    loaded
}

#[test]
fn without_a_choice_detection_decides() {
    let m = round_trip("detect", |_| {});
    assert_eq!(m.detected_log_format, LogFormat::SLog3, "fixture should detect as S-Log3");
    assert!(!m.log_format_user_set);
    assert_eq!(m.effective_log_format(), LogFormat::SLog3);
}

#[test]
fn choosing_srgb_overrides_a_detected_log_and_survives_save_load() {
    let m = round_trip("override", |m| {
        m.log_format = LogFormat::Srgb;
        m.log_format_user_set = true;
    });
    assert!(m.log_format_user_set, "the explicit choice must be saved");
    assert_eq!(
        m.effective_log_format(),
        LogFormat::Srgb,
        "choosing sRGB must stop the decoder converting a clip detected as S-Log3",
    );
}

/// Projects saved before the flag existed: a non-default format there was
/// necessarily the user's choice.
#[test]
fn legacy_non_default_format_loads_as_a_choice() {
    let m = round_trip("legacy", |m| {
        m.log_format = LogFormat::VLog;
        m.log_format_user_set = false;
    });
    assert!(m.log_format_user_set);
    assert_eq!(m.effective_log_format(), LogFormat::VLog);
}

/// Detection runs again on load, so a project saved while a Rec.709-tagged
/// 10-bit Sony clip was being called S-Log3 opens with it as SDR.
#[test]
fn a_bt709_tagged_sony_clip_reloads_as_sdr() {
    let m = round_trip("bt709", |m| {
        m.color_transfer = Some("BT.709".into());
        m.detected_log_format = LogFormat::SLog3;
    });
    assert_eq!(m.detected_log_format, LogFormat::Srgb);
    assert_eq!(m.effective_log_format(), LogFormat::Srgb);
}
