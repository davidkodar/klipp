//! The analysers against a clip ffmpeg makes: two pictures joined at
//! two seconds, a tone with a gap in it. Skipped without ffmpeg.

use klipp_analysis::{audio, shots, AnalysisCtx, Analyser};
#[allow(unused_imports)]
use klipp_analysis::motion;
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

/// A contact sheet and an excerpt of the synthetic clip.
#[test]
fn a_contact_sheet_and_an_excerpt_come_out_of_a_clip() {
    let Some(path) = synthetic_clip() else { return };
    let ctx = AnalysisCtx::default();
    let dir = path.parent().unwrap();
    let sheet = klipp_analysis::sheet::contact_sheet(&ctx.ffmpeg, &path, 0.0, 4.0, 8, 4, 160, &dir.join("sheet.jpg")).expect("sheet");
    assert_eq!(sheet.tiles_secs.len(), 8);
    assert_eq!((sheet.cols, sheet.rows), (4, 2));
    assert!(sheet.width >= 640 && sheet.height > 0, "{sheet:?}");
    assert!((sheet.tiles_secs[1] - 0.5).abs() < 1e-9);
    let wav = klipp_analysis::sheet::audio_excerpt(&ctx.ffmpeg, &path, 1.5, 3.0, &dir.join("ex.wav")).expect("excerpt");
    let bytes = std::fs::metadata(&wav).unwrap().len();
    // 1.5 s at 16 kHz mono 16-bit is 48 000 bytes plus a header.
    assert!((44_000..60_000).contains(&bytes), "{bytes}");

    // Motion: the picture is two still images, so almost none moves.
    let item = item_for(&path);
    let v = klipp_analysis::motion::MotionAnalyser.run(&item, &ctx).expect("motion");
    let m: klipp_analysis::motion::Motion = serde_json::from_value(v).unwrap();
    assert_eq!(m.per_second.len(), 4);
}

/// Every analyser on a real file named by `KLIPP_ANALYSIS_TRIAL`, the
/// report printed: a way to see the transcript on real speech, since
/// no synthesiser is at hand for a fixture. Skipped otherwise.
#[test]
fn trial_on_a_real_file_when_asked() {
    let Ok(path) = std::env::var("KLIPP_ANALYSIS_TRIAL") else { return };
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::new("info")).with_writer(std::io::stderr).try_init();
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
    // How things are spelt, as the brief would say it: KLIPP_ANALYSIS_VOCAB="Omarchy, DHH".
    #[cfg(feature = "transcript")]
    if let Ok(words) = std::env::var("KLIPP_ANALYSIS_VOCAB") {
        klipp_analysis::transcript::set_vocabulary(words.split(',').map(|w| w.to_string()).collect());
    }
    let t = std::time::Instant::now();
    let report = klipp_analysis::service::run_now(&item, &AnalysisCtx::default()).expect("run now");
    eprintln!("analysed {} s of footage in {:.1} s\n{}", secs, t.elapsed().as_secs_f64(), report.text(6000));
    eprintln!("clip summary: {}", report.clip_summary(0.0, secs));
    #[cfg(feature = "transcript")]
    if let Some(tr) = &report.transcript {
        eprintln!("model {} vocabulary {:?}", tr.model, tr.vocabulary);
        let line: Vec<String> = tr.words.iter().take(60).map(|w| format!("{}@{:.2}-{:.2}", w.text, w.start_secs, w.end_secs)).collect();
        eprintln!("first words: {}", line.join(" "));
    }
}

/// A sheet across a long real file, timed: `KLIPP_SHEET_TRIAL=<file>`.
#[test]
fn a_sheet_of_a_long_file_when_asked() {
    let Ok(path) = std::env::var("KLIPP_SHEET_TRIAL") else { return };
    let ctx = AnalysisCtx::default();
    let out = std::env::temp_dir().join("klipp-sheet-trial.jpg");
    let t = std::time::Instant::now();
    let sheet = klipp_analysis::sheet::contact_sheet(&ctx.ffmpeg, std::path::Path::new(&path), 0.0, 1680.0, 24, 6, 320, &out).expect("sheet");
    eprintln!("24 tiles across 28 minutes in {:.1} s: {}x{} at {}", t.elapsed().as_secs_f64(), sheet.width, sheet.height, out.display());
}

/// The whole sync path against ffmpeg: the same noise delayed by half a
/// second must come back as a +0.5 s offset with a confident match.
#[test]
fn align_audio_recovers_a_known_delay() {
    let ctx = AnalysisCtx::default();
    if !ctx.ffmpeg_available() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let dir = std::env::temp_dir().join(format!("klipp-sync-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let refp = dir.join("ref.wav");
    let tgtp = dir.join("tgt.wav");
    // Deterministic noise: a strong, unambiguous correlation peak.
    let ok = std::process::Command::new(&ctx.ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "anoisesrc=seed=12345:d=4:sample_rate=8000"])
        .arg(&refp).status().unwrap().success();
    assert!(ok, "ffmpeg made the reference");
    // The same noise, delayed 500 ms → content starts half a second later.
    let ok = std::process::Command::new(&ctx.ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "anoisesrc=seed=12345:d=4:sample_rate=8000", "-af", "adelay=500"])
        .arg(&tgtp).status().unwrap().success();
    assert!(ok, "ffmpeg made the delayed target");

    use klipp_analysis::audio_sync::{decode_mono_pcm, find_sync_offset_scored};
    let sr = 8_000u32;
    let r = decode_mono_pcm(&ctx.ffmpeg, &refp, sr, 0.0, 10.0).unwrap();
    let t = decode_mono_pcm(&ctx.ffmpeg, &tgtp, sr, 0.0, 10.0).unwrap();
    let (off, conf) = find_sync_offset_scored(&r, &t, sr, 2.0);
    let off_secs = off as f64 / sr as f64;
    assert!((off_secs - 0.5).abs() < 0.02, "expected +0.5 s, got {off_secs:.3} s");
    assert!(conf > 0.5, "the match should be confident, got {conf:.2}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The beat detector against a real ffmpeg click track: a 1 kHz pulse
/// every 0.6 s is 100 BPM, and detect() should land near it.
#[test]
fn beats_detect_a_100_bpm_click_track() {
    let ctx = AnalysisCtx::default();
    if !ctx.ffmpeg_available() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let dir = std::env::temp_dir().join(format!("klipp-beats-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("click100.wav");
    let ok = std::process::Command::new(&ctx.ffmpeg)
        .args([
            "-y", "-hide_banner", "-loglevel", "error",
            "-f", "lavfi",
            "-i", "aevalsrc=if(lt(mod(t\\,0.6)\\,0.02)\\,sin(2*PI*1000*t)\\,0):s=22050:d=10",
            "-ac", "1",
        ])
        .arg(&wav).status().unwrap().success();
    assert!(ok, "ffmpeg made the click track");
    let b = klipp_analysis::beats::detect(&ctx.ffmpeg, &wav, 10.0).unwrap();
    assert!((b.bpm - 100.0).abs() < 6.0, "expected ~100 BPM, got {}", b.bpm);
    assert!(b.confidence > 0.4, "a clean click is confident, got {}", b.confidence);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A music file fitted to a length (#2210): a 120 BPM click track of 64 s
/// comes to within a bar of the 40 s asked, by whole bars.
#[test]
fn fit_media_cuts_a_click_track_to_a_length_by_whole_bars() {
    let ctx = AnalysisCtx::default();
    if !ctx.ffmpeg_available() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let dir = std::env::temp_dir().join(format!("klipp-fit-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("click120.wav");
    let ok = std::process::Command::new(&ctx.ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi",
            "-i", "aevalsrc=if(lt(mod(t\\,0.5)\\,0.02)\\,sin(2*PI*1000*t)\\,0):s=22050:d=64", "-ac", "1"])
        .arg(&wav).status().unwrap().success();
    assert!(ok, "ffmpeg made the click track");
    let (fit, start) = klipp_analysis::music_structure::fit_media(&ctx.ffmpeg, &wav, 64.0, 40.0, 0.0, false).expect("a whole-bar fit");
    assert_eq!(start, 0.0);
    assert!(fit.bars_changed < 0, "bars taken out: {fit:?}");
    assert!((fit.length_secs - 40.0).abs() <= 2.0, "within a bar of 40 s: {}", fit.length_secs);
    let played: f64 = fit.segments.iter().map(|(a, b)| b - a).sum();
    assert!((played - fit.length_secs).abs() < 1e-6);
    assert!(klipp_analysis::music_structure::fit_media(&ctx.ffmpeg, &wav, 64.0, 0.0, 0.0, false).is_err(), "no length, no fit");
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a sound is (#2216), with the PANNs weights at `KLIPP_PANNS_CNN10`:
/// a synthesised tune reads as music and, where espeak-ng is installed,
/// spoken words as speech.
#[cfg(feature = "audio_role")]
#[test]
fn a_tune_reads_as_music_and_words_as_speech() {
    use klipp_analysis::audio_role::{score, SoundKind};
    let ctx = AnalysisCtx::default();
    let Ok(weights) = std::env::var("KLIPP_PANNS_CNN10") else {
        eprintln!("skipping: set KLIPP_PANNS_CNN10 to the weights");
        return;
    };
    if !ctx.ffmpeg_available() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let model = klipp_ml_candle::panns::Cnn10::load(std::path::Path::new(&weights)).unwrap();
    let dir = std::env::temp_dir().join(format!("klipp-role-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rate = klipp_ml_candle::panns::SAMPLE_RATE;
    let read = |path: &std::path::Path| klipp_analysis::audio_sync::decode_mono_pcm(&ctx.ffmpeg, path, rate, 0.0, 60.0).unwrap();
    let tune = dir.join("tune.wav");
    let ok = std::process::Command::new(&ctx.ffmpeg)
        .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
            "aevalsrc='0.2*sin(2*PI*261.6*t)*(1-mod(t,0.5))+0.2*sin(2*PI*329.6*t)*(1-mod(t+0.25,0.5))+0.15*sin(2*PI*392*t)+0.3*sin(2*PI*65.4*t)*exp(-8*mod(t,0.5))':s=32000:d=12"])
        .arg(&tune).status().unwrap().success();
    assert!(ok);
    let roles = score(&read(&tune), &mut |b| model.classify(b)).unwrap();
    assert_eq!(roles.label(None).unwrap().kind, SoundKind::Music, "{roles:?}");
    let words = dir.join("words.wav");
    let spoke = std::process::Command::new("espeak-ng")
        .arg("-w").arg(&words)
        .arg("Good morning. Today we talk about how the river changed the town, and why the bridge was built where it stands now.")
        .status().is_ok_and(|s| s.success());
    if spoke {
        let roles = score(&read(&words), &mut |b| model.classify(b)).unwrap();
        let label = roles.label(None).unwrap();
        assert_eq!(label.kind, SoundKind::Speech, "{roles:?}");
        assert!(label.confident, "{label:?}");
    } else {
        eprintln!("no espeak-ng: speech not checked");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
