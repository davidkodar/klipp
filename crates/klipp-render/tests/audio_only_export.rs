//! An export of the sound alone (#1813): MP3, WAV and AAC in M4A, each one
//! audio stream and no video stream, as long as the timeline, at the
//! loudness the delivery asks for. The "Audio Only MP3" preset writes an
//! MP3, where it wrote a video file.
use klipp_core::*;
use klipp_render::snapshot::{AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::ExportJob;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

const RATE: u32 = 48000;
const FRAMES: u64 = 100;

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Six seconds of a 440 Hz tone at -12 dBFS, stereo.
fn tone() -> PathBuf {
    static TONE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    TONE.get_or_init(|| {
        let out = klipp_testing::scratch::scratch_file("klipp-audio-only", "tone.wav");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=f=440:r=48000:d=6,volume=-12dB,aformat=channel_layouts=stereo"])
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "the tone");
        out
    })
    .clone()
}

/// Four seconds at 25 fps of the tone on one track.
fn snapshot() -> SequenceSnapshot {
    let clip = AudioClipSnapshot {
        timeline_start_frame: 0,
        timeline_end_frame: FRAMES,
        source_start_secs: 0.0,
        path: tone(),
        audio_effects: vec![],
        audio_effect_keyframes: Default::default(),
        source_range: (0.0, 0.0),
        shape: Default::default(),
        varispeed: None,
        exact_secs: None,
    };
    SequenceSnapshot {
        fps: 25.0,
        fps_num: 25,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: RATE,
        duration_frames: FRAMES,
        export_range: None,
        video: Default::default(),
        audio_tracks: vec![AudioTrackSnapshot { volume: 1.0, pan: 0.0, muted: false, solo: false, clips: vec![clip], automation: Default::default() , effects: Vec::new()}],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: WorkingColorspace::Rec709Linear,
        grade_encoding: GradeEncoding::Bt1886,
    }
}

fn export(settings: ExportSettings) {
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: settings.output_path.clone(),
        settings,
        snapshot: snapshot(),
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(),
        render_stage: Default::default(),
        loudness_result: Default::default(),
        timings: Default::default(),
        total_frames: FRAMES,
        analyser: None,
    }
    .run()
    .unwrap();
}

/// (codec of each stream with its type, the file's duration in seconds).
fn streams(path: &Path) -> (Vec<String>, f64) {
    let o = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name:format=duration", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let mut out = Vec::new();
    let mut name = String::new();
    for l in text.lines() {
        if let Some(n) = l.strip_prefix("codec_name=") {
            name = n.to_string();
        }
        if let Some(t) = l.strip_prefix("codec_type=") {
            out.push(format!("{t}:{name}"));
        }
    }
    let dur = text.lines().find_map(|l| l.strip_prefix("duration=")).and_then(|d| d.parse().ok()).unwrap_or(0.0);
    (out, dur)
}

/// Integrated loudness by ffmpeg's EBU R128 meter, LUFS.
fn lufs(path: &Path) -> f64 {
    let o = std::process::Command::new("ffmpeg").args(["-nostdin", "-i"]).arg(path).args(["-af", "ebur128", "-f", "null", "-"]).output().unwrap();
    let text = String::from_utf8_lossy(&o.stderr);
    text.lines().rev().find_map(|l| l.trim().strip_prefix("I:")).and_then(|v| v.split_whitespace().next()?.parse().ok()).unwrap_or(f64::NAN)
}

#[test]
fn the_sound_alone_is_one_audio_stream_and_no_picture() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-audio-only");
    for (format, codec, name, want) in [
        (ExportFormat::Mp3, ExportAudioCodec::Mp3, "mix.mp3", "audio:mp3"),
        (ExportFormat::Wav, ExportAudioCodec::Pcm16, "mix.wav", "audio:pcm_s16le"),
        (ExportFormat::Wav, ExportAudioCodec::Pcm24, "mix24.wav", "audio:pcm_s24le"),
        (ExportFormat::M4a, ExportAudioCodec::Aac, "mix.m4a", "audio:aac"),
    ] {
        let out = dir.join(name);
        export(ExportSettings { output_path: out.clone(), format, audio_codec: codec, ..Default::default() });
        let (s, dur) = streams(&out);
        assert_eq!(s, [want], "{name}: one audio stream and nothing else");
        assert!((dur - 4.0).abs() < 0.1, "{name}: {dur} s for four of timeline");
    }
}

#[test]
fn the_audio_only_preset_writes_an_mp3_at_its_loudness() {
    if !ffmpeg_ok() {
        return;
    }
    let preset = builtin_presets().into_iter().find(|p| p.name == "Audio Only MP3").expect("the preset");
    assert!(preset.settings.format.is_audio_only(), "{:?}", preset.settings.format);
    let dir = klipp_testing::scratch::scratch_dir("klipp-audio-only-preset");
    let out = dir.join(format!("mix.{}", preset.settings.format.extension()));
    let s = &preset.settings;
    export(ExportSettings { output_path: out.clone(), format: s.format, audio_codec: s.audio_codec, audio_bitrate_kbps: s.audio_bitrate_kbps, ..Default::default() });
    assert_eq!(streams(&out).0, ["audio:mp3"], "the preset's file is an MP3");
    // A loudness target is met as it is in any export.
    let loud = dir.join("loud.mp3");
    export(ExportSettings { output_path: loud.clone(), format: ExportFormat::Mp3, audio_codec: ExportAudioCodec::Mp3, target_lufs: Some(-20.0), ..Default::default() });
    let got = lufs(&loud);
    assert!((got + 20.0).abs() < 1.0, "asked -20 LUFS, measured {got}");
}

/// An audio codec a format can't hold gives way to its own; MP3 is kept
/// where the build has its encoder.
#[test]
fn an_audio_only_export_takes_a_codec_it_can_write() {
    use klipp_render::audio_only::resolve;
    assert_eq!(resolve(ExportFormat::Wav, ExportAudioCodec::Aac).1, ExportAudioCodec::Pcm16);
    assert_eq!(resolve(ExportFormat::M4a, ExportAudioCodec::Mp3).1, ExportAudioCodec::Aac);
    if klipp_render::audio_only::has_encoder(ExportAudioCodec::Mp3) {
        assert_eq!(resolve(ExportFormat::Mp3, ExportAudioCodec::Mp3), (ExportFormat::Mp3, ExportAudioCodec::Mp3, None));
    } else {
        assert_eq!(resolve(ExportFormat::Mp3, ExportAudioCodec::Mp3).0, ExportFormat::Wav, "without MP3, WAV");
    }
}

/// The top-level boxes of an ISO media file, in order.
fn top_boxes(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    let (mut at, mut names) = (0usize, Vec::new());
    while at + 8 <= bytes.len() {
        let size32 = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        names.push(String::from_utf8_lossy(&bytes[at + 4..at + 8]).into_owned());
        let size = match size32 {
            0 => bytes.len() - at,
            1 => u64::from_be_bytes(bytes[at + 8..at + 16].try_into().unwrap()) as usize,
            n => n,
        };
        if size < 8 {
            break;
        }
        at += size;
    }
    names
}

/// #1901: an MP4 or M4A has its index before its media, so a web player
/// can start before the whole file has arrived; the streams are as they
/// were.
#[test]
fn a_web_file_has_its_index_before_its_media() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-faststart");
    for (format, name, want) in [
        (ExportFormat::Mp4, "web.mp4", vec!["video:h264", "audio:aac"]),
        (ExportFormat::M4a, "web.m4a", vec!["audio:aac"]),
    ] {
        let out = dir.join(name);
        export(ExportSettings { output_path: out.clone(), format, audio_codec: ExportAudioCodec::Aac, ..Default::default() });
        let boxes = top_boxes(&out);
        let (moov, mdat) = (boxes.iter().position(|b| b == "moov"), boxes.iter().position(|b| b == "mdat"));
        assert!(moov.is_some() && moov < mdat, "{name}: moov before mdat, got {boxes:?}");
        let (streams, dur) = streams(&out);
        let mut streams = streams;
        streams.sort();
        let mut want = want;
        want.sort();
        assert_eq!(streams, want, "{name}");
        assert!((dur - 4.0).abs() < 0.1, "{name}: {dur} s");
    }
}
