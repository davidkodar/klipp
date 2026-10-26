//! Measured sweep of the built-in export presets: each one renders a
//! second of a test picture and a steady sound, and the file is probed
//! for what the preset says: its codecs, size, rate and bit depth, its
//! loudness and true peak, and that the picture fills the frame. Needs
//! the ffmpeg CLI; skips without it. Report: `target/sweep/export.txt`.

use klipp_core::*;
use klipp_render::snapshot::{AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::{ExportJob, MediaFile, VideoTimeline};
use klipp_testing::sweep::Sweep;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};

/// Items known to fail, and the issue that holds each.
const KNOWN: &[(&str, u32)] = &[
    ("ProRes 422 loudness", 2783),
    ("DNxHD 115 loudness", 2783),
    ("Broadcast master (EBU R128) loudness", 2783),
    ("Instagram Square fills the frame", 2784),
    ("Instagram Vertical fills the frame", 2784),
    ("ProRes 422 ProRes profile", 2784),
    ("Broadcast master (EBU R128) ProRes profile", 2784),
];

const FRAMES: u64 = 25;
const SIZE: (u32, u32) = (320, 180);

fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-sweep-export", name)
}

/// A second of a moving test picture and of a steady two-tone sound with
/// a little noise, about -20 LUFS.
fn sources() -> Option<(PathBuf, PathBuf)> {
    let (video, audio) = (tmp("picture.mp4"), tmp("sound.wav"));
    let run = |args: &[&str], out: &Path| std::process::Command::new("ffmpeg").args(["-nostdin", "-y", "-v", "error"]).args(args).arg(out).status().is_ok_and(|s| s.success());
    let picture = run(&["-f", "lavfi", "-i", &format!("testsrc2=s={}x{}:r=25:d=1", SIZE.0, SIZE.1), "-c:v", "libx264", "-pix_fmt", "yuv420p"], &video);
    let sound = run(&["-f", "lavfi", "-i", "aevalsrc=0.07*sin(2*PI*220*t)+0.05*sin(2*PI*880*t)+0.01*(random(0)*2-1):s=48000:d=1.2", "-ac", "2", "-c:a", "pcm_f32le"], &audio);
    (picture && sound).then_some((video, audio))
}

fn snapshot(video_path: &Path, audio_path: &Path) -> SequenceSnapshot {
    let id = MediaId::new();
    let tc = |f| RationalTime::from_frames(f, 25);
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(25, 1);
    video.sequence.resolution = Resolution { width: SIZE.0, height: SIZE.1 };
    let mut t = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
    t.clips = vec![klipp_timeline::Clip::media(id, tc(0), tc(FRAMES as i64), tc(0), tc(FRAMES as i64))];
    video.sequence.tracks.push(t);
    video.media.insert(id, MediaFile { path: video_path.to_path_buf(), log_format: None, source_rotation: 0, ..Default::default() });
    let clip = AudioClipSnapshot {
        timeline_start_frame: 0,
        timeline_end_frame: FRAMES,
        source_start_secs: 0.0,
        path: audio_path.to_path_buf(),
        audio_effects: vec![],
        audio_effect_keyframes: Default::default(),
        source_range: (0.0, 1.0),
        shape: Default::default(),
        varispeed: None,
        exact_secs: None,
    };
    SequenceSnapshot {
        fps: 25.0,
        fps_num: 25,
        fps_den: 1,
        width: SIZE.0,
        height: SIZE.1,
        sample_rate: 48000,
        duration_frames: FRAMES,
        export_range: None,
        video,
        audio_tracks: vec![AudioTrackSnapshot { volume: 1.0, pan: 0.0, muted: false, solo: false, clips: vec![clip], automation: Default::default(), effects: Vec::new() }],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: WorkingColorspace::Rec709Linear,
        grade_encoding: GradeEncoding::Bt1886,
    }
}

/// The export settings a preset gives, as the app applies it by name.
fn settings_of(p: &ExportPresetSettings, out: &Path) -> ExportSettings {
    ExportSettings {
        output_path: out.to_path_buf(),
        format: p.format,
        video_codec: p.video_codec,
        resolution: p.resolution,
        fps: p.fps,
        crf: p.crf,
        preset: p.preset,
        audio_codec: p.audio_codec,
        audio_bitrate_kbps: p.audio_bitrate_kbps,
        video_bitrate_kbps: p.video_bitrate_kbps,
        rate_control: p.rate_control,
        target_lufs: p.target_lufs,
        true_peak_dbtp: p.true_peak_dbtp,
        bit_depth: p.bit_depth,
        ..Default::default()
    }
}

fn export(snapshot: SequenceSnapshot, settings: ExportSettings) -> Result<(), String> {
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: settings.output_path.clone(),
        settings,
        snapshot,
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(),
        render_stage: Default::default(),
        loudness_result: Arc::new(Mutex::new(None)),
        timings: Default::default(),
        total_frames: FRAMES,
        analyser: None,
    }
    .run()
    .map_err(|e| e.to_string())
}

/// One stream's fields by name, by ffprobe.
fn probe(path: &Path, stream: &str, fields: &str) -> std::collections::HashMap<String, String> {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", stream, "-show_entries", &format!("stream={fields}"), "-of", "default=nw=1"])
        .arg(path)
        .output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// Integrated loudness and true peak, by ffmpeg's EBU R128 meter.
fn ebur128(path: &Path) -> (f64, f64) {
    let o = std::process::Command::new("ffmpeg").args(["-nostdin", "-i"]).arg(path).args(["-af", "ebur128=peak=true", "-f", "null", "-"]).output();
    let text = o.map(|o| String::from_utf8_lossy(&o.stderr).to_string()).unwrap_or_default();
    let summary = &text[text.rfind("Summary:").unwrap_or(0)..];
    let value = |key: &str| summary.lines().find_map(|l| l.trim().strip_prefix(key)).and_then(|v| v.split_whitespace().next()?.parse().ok()).unwrap_or(f64::NAN);
    (value("I:"), value("Peak:"))
}

/// The share of the middle frame's rows that are black bars.
fn bar_share(path: &Path, (w, h): (u32, u32)) -> f32 {
    let o = std::process::Command::new("ffmpeg").args(["-nostdin", "-v", "error", "-ss", "0.5", "-i"]).arg(path).args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"]).output();
    let Ok(o) = o else { return 1.0 };
    if w == 0 || h == 0 {
        return 1.0;
    }
    let rows = o.stdout.chunks(w as usize).take(h as usize);
    let dark = rows.filter(|r| r.iter().map(|v| *v as u32).sum::<u32>() / (r.len().max(1) as u32) < 20).count();
    dark as f32 / h.max(1) as f32
}

fn video_codec_name(c: ExportVideoCodec) -> &'static str {
    match c {
        ExportVideoCodec::H264 => "h264",
        ExportVideoCodec::H265 => "hevc",
        ExportVideoCodec::ProRes422 | ExportVideoCodec::ProRes422HQ | ExportVideoCodec::ProRes4444 => "prores",
        ExportVideoCodec::DNxHD115 | ExportVideoCodec::DNxHD145 => "dnxhd",
        ExportVideoCodec::Av1 => "av1",
        ExportVideoCodec::Vp9 => "vp9",
        ExportVideoCodec::Gif => "gif",
    }
}

fn audio_codec_name(c: ExportAudioCodec) -> &'static str {
    match c {
        ExportAudioCodec::Aac => "aac",
        ExportAudioCodec::Mp3 => "mp3",
        ExportAudioCodec::Flac => "flac",
        ExportAudioCodec::Opus => "opus",
        ExportAudioCodec::Pcm16 => "pcm_s16le",
        ExportAudioCodec::Pcm24 => "pcm_s24le",
        _ => "?",
    }
}

#[test]
fn sweep_export() {
    let Some((video, audio)) = sources() else {
        eprintln!("skipping: ffmpeg could not write the sources");
        return;
    };
    let mut sweep = Sweep::new("export", KNOWN);
    // The mix's own loudness, which a preset without a target keeps.
    let (mix_lufs, _) = ebur128(&audio);
    let presets = builtin_presets();
    // Rendered four at a time.
    let results: Vec<(ExportPreset, PathBuf, Result<(), String>)> = presets
        .chunks(4)
        .flat_map(|group| {
            std::thread::scope(|s| {
                let jobs: Vec<_> = group
                    .iter()
                    .map(|p| {
                        let out = tmp(&format!("{}.{}", p.id, p.settings.format.extension()));
                        let _ = std::fs::remove_file(&out);
                        let (snapshot, settings) = (snapshot(&video, &audio), settings_of(&p.settings, &out));
                        s.spawn(move || (p.clone(), out, export(snapshot, settings)))
                    })
                    .collect();
                jobs.into_iter().map(|j| j.join().unwrap()).collect::<Vec<_>>()
            })
        })
        .collect();
    for (p, out, done) in results {
        let (name, s) = (&p.name, &p.settings);
        if let Err(why) = done {
            sweep.check(&format!("{name} exports"), format!("refused: {why}"), false);
            continue;
        }
        let has_video = s.format != ExportFormat::Mp3;
        if has_video {
            let v = probe(&out, "v:0", "codec_name,width,height,r_frame_rate,pix_fmt,profile");
            let field = |i: usize| v.get(["codec_name", "width", "height", "r_frame_rate", "pix_fmt", "profile"][i]).cloned().unwrap_or_default();
            let want_codec = video_codec_name(s.video_codec);
            sweep.check(&format!("{name} video codec"), format!("{} (wanted {want_codec})", field(0)), field(0) == want_codec);
            let want = s.resolution.map(|r| (r.width, r.height)).unwrap_or(SIZE);
            let got = (field(1).parse().unwrap_or(0), field(2).parse().unwrap_or(0));
            sweep.check(&format!("{name} size"), format!("{}x{} (wanted {}x{})", got.0, got.1, want.0, want.1), got == want);
            let want_rate = s.fps.map(|f| format!("{}/{}", f.num, f.den)).unwrap_or_else(|| "25/1".into());
            sweep.check(&format!("{name} rate"), format!("{} (wanted {want_rate})", field(3)), field(3) == want_rate);
            if s.bit_depth == 10 {
                sweep.check(&format!("{name} 10-bit"), field(4), field(4).contains("10"));
            }
            if s.video_codec == ExportVideoCodec::ProRes422 {
                sweep.check(&format!("{name} ProRes profile"), format!("{} (wanted Standard)", field(5)), !field(5).contains("HQ"));
            }
            let bars = bar_share(&out, got);
            sweep.check(&format!("{name} fills the frame"), format!("{:.0}% of rows black bars", bars * 100.0), bars < 0.05);
        }
        if s.format == ExportFormat::Gif {
            continue;
        }
        let a = probe(&out, "a:0", "codec_name").get("codec_name").cloned().unwrap_or_default();
        let want_audio = audio_codec_name(s.audio_codec);
        sweep.check(&format!("{name} audio codec"), format!("{a} (wanted {want_audio})"), a == want_audio);
        let (lufs, peak) = ebur128(&out);
        match s.target_lufs {
            Some(target) => {
                sweep.check(&format!("{name} loudness"), format!("{lufs:.1} LUFS (wanted {target})"), (lufs - target as f64).abs() <= 1.0);
                let ceiling = s.true_peak_dbtp.unwrap_or(-1.0) as f64;
                sweep.check(&format!("{name} true peak"), format!("{peak:+.1} dBTP (ceiling {ceiling})"), peak <= ceiling + 0.3);
            }
            None => sweep.check(&format!("{name} loudness"), format!("{lufs:.1} LUFS (the mix {mix_lufs:.1})"), (lufs - mix_lufs).abs() <= 1.0),
        }
    }
    sweep.finish();
}
