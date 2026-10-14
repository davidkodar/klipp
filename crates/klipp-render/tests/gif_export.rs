//! The Animated GIF preset exports (#1142): a GIF at the preset's size and
//! rate. Its encoder used to fail at open, asking swscale for an RGBA →
//! PAL8 scaler it has no use for, so no GIF was ever written.
use klipp_core::*;
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::{Clip, ClipKind, Sequence};
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc,
};

#[test]
fn the_animated_gif_preset_writes_a_gif() {
    if !std::process::Command::new("ffprobe").arg("-version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping: no ffprobe");
        return;
    }
    let preset = builtin_presets().into_iter().find(|p| p.settings.format == ExportFormat::Gif).expect("a GIF preset");
    let dir = klipp_testing::scratch::scratch_dir("klipp-gif-export");
    let out = dir.join("clip.gif");
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.resolution = Resolution { width: 1920, height: 1080 };
    let tc = |s: f64| Timecode::from_secs_f64(s, 25);
    seq.tracks[0].clips = vec![Clip { kind: ClipKind::Generator(GeneratorData::SolidColor { color: [200, 60, 40, 255] }), ..Clip::gap(tc(0.0), tc(1.0)) }];
    let s = &preset.settings;
    let settings = ExportSettings {
        output_path: out.clone(),
        format: s.format,
        video_codec: s.video_codec,
        resolution: s.resolution,
        fps: s.fps,
        audio_codec: s.audio_codec,
        ..Default::default()
    };
    let pool = MediaPool::new();
    let snapshot = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, WorkingColorspace::Rec709Linear).unwrap();
    let frames = snapshot.duration_frames;
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: out.clone(),
        settings,
        snapshot,
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(),
        render_stage: Default::default(),
        loudness_result: Default::default(),
        timings: Default::default(),
        total_frames: frames,
        analyser: None,
    }
    .run()
    .expect("the GIF exports");
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v", "-show_entries", "stream=codec_name,width,height,nb_read_frames", "-of", "default=nw=1"])
        .arg(&out)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout);
    let want = s.resolution.unwrap();
    for field in ["codec_name=gif".to_string(), format!("width={}", want.width), format!("height={}", want.height)] {
        assert!(text.contains(&field), "no {field} in {text}");
    }
    let n: u32 = text.lines().find_map(|l| l.strip_prefix("nb_read_frames=")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    let fps = s.fps.unwrap();
    assert!(n.abs_diff(fps.num / fps.den) <= 1, "a second at {} fps is {n} frames", fps.num / fps.den);
}
