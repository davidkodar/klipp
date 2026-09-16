//! The analysers against a clip ffmpeg makes: two pictures joined at
//! two seconds, a tone with a gap in it. Skipped without ffmpeg.

use klipp_analysis::{audio, shots, AnalysisCtx, Analyser};
use std::path::PathBuf;

fn synthetic_clip() -> Option<PathBuf> {
    let ctx = AnalysisCtx::default();
    if !ctx.ffmpeg_available() {
        eprintln!("skipping: no ffmpeg");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("klipp-analysis-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join("two_shots.mp4");
    // Picture: 2 s of test bars, then 2 s of a solid colour → one cut at 2 s.
    // Sound: 1 s tone, 1.5 s silence, 1.5 s tone → one silence 1.0-2.5 s.
    let status = std::process::Command::new(&ctx.ffmpeg)
        .args([
            "-y", "-hide_banner", "-loglevel", "error",
            "-f", "lavfi", "-i", "testsrc=size=320x180:rate=25:duration=2",
            "-f", "lavfi", "-i", "color=c=blue:size=320x180:rate=25:duration=2",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
            "-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono:duration=1.5",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1.5",
            "-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0[v];[2:a][3:a][4:a]concat=n=3:v=0:a=1[a]",
            "-map", "[v]", "-map", "[a]", "-c:v", "mpeg4", "-q:v", "3", "-c:a", "aac", "-b:a", "128k",
        ])
        .arg(&out)
        .status()
        .ok()?;
    status.success().then_some(out)
}

fn item_for(path: &std::path::Path) -> klipp_media::MediaItem {
    let mut item = klipp_media::MediaItem::synthetic_for_test(path.to_path_buf(), None);
    item.duration = klipp_core::RationalTime::from_secs_f64(4.0, 25);
    item.video_codec = "mpeg4".into();
    item.has_audio = true;
    item
}

#[test]
fn shots_and_silence_are_found_in_a_synthetic_clip() {
    let Some(path) = synthetic_clip() else { return };
    let ctx = AnalysisCtx::default();
    let item = item_for(&path);

    let v = shots::ShotsAnalyser.run(&item, &ctx).expect("shots run");
    let s: shots::Shots = serde_json::from_value(v).unwrap();
    assert_eq!(s.shots.len(), 2, "one cut, two shots: {s:?}");
    assert!((s.boundaries[0] - 2.0).abs() < 0.15, "cut near 2 s: {:?}", s.boundaries);

    let v = audio::AudioAnalyser.run(&item, &ctx).expect("audio run");
    let a: audio::AudioEnvelope = serde_json::from_value(v).unwrap();
    assert_eq!(a.silences.len(), 1, "{a:?}");
    assert!((a.silences[0].start_secs - 1.0).abs() < 0.15 && (a.silences[0].end_secs - 2.5).abs() < 0.15, "{:?}", a.silences);
    assert!(a.integrated_lufs.is_some() && a.true_peak_dbtp.is_some(), "{a:?}");

    // Through the store and the service, and the text a model reads.
    std::env::set_var("XDG_CACHE_HOME", std::env::temp_dir().join(format!("klipp-analysis-cache-{}", std::process::id())));
    let report = klipp_analysis::service::run_now(&item, &ctx).expect("run now");
    assert!(report.shots.is_some() && report.audio.is_some());
    let text = report.text(4000);
    assert!(text.contains("Shots: 2"), "{text}");
    assert!(text.contains("silence 1."), "{text}");
    let line = report.clip_summary(0.0, 4.0);
    assert!(line.contains("shots 2") && line.contains("LUFS"), "{line}");
}

/// Every analyser on a real file named by `KLIPP_ANALYSIS_TRIAL`, the
/// report printed: a way to see the transcript on real speech, since
/// no synthesiser is at hand for a fixture. Skipped otherwise.
#[test]
fn trial_on_a_real_file_when_asked() {
    let Ok(path) = std::env::var("KLIPP_ANALYSIS_TRIAL") else { return };
    let path = PathBuf::from(path);
    let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
    // ffprobe for the duration: what import would have recorded.
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(&path)
        .output()
        .expect("ffprobe");
    let secs: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().expect("duration");
    item.duration = klipp_core::RationalTime::from_secs_f64(secs, 30);
    item.video_codec = "hevc".into();
    item.has_audio = true;
    let t = std::time::Instant::now();
    let report = klipp_analysis::service::run_now(&item, &AnalysisCtx::default()).expect("run now");
    eprintln!("analysed {} s of footage in {:.1} s\n{}", secs, t.elapsed().as_secs_f64(), report.text(6000));
    eprintln!("clip summary: {}", report.clip_summary(0.0, secs));
}
