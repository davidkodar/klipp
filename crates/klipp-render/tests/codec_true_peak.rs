//! A lossy delivery is held under its true-peak ceiling as it is written,
//! not only as it was mixed (#1930): an AAC or MP3 encode rings round
//! transients and peaks over what the mix measured, so the delivery's fit
//! measures through the codec, and the status reports the written file.
//! Checked here by ffmpeg's own EBU R128 meter on the file.
use klipp_core::*;
use klipp_render::snapshot::{AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::ExportJob;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64},
    Arc, Mutex,
};

const RATE: u32 = 48000;
const FRAMES: u64 = 150;

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Six seconds of transients, stereo, written once as `name`: a low tone
/// under snare-like bursts of decaying noise and short clicks, the
/// material a codec overshoots on. `full_band` keeps the noise to
/// Nyquist and the clicks square, which no sample-peak limiter can hold
/// to a true-peak ceiling; otherwise the snare is a snare (low-passed at
/// 14 kHz) and a click a sharp decaying 5 kHz burst.
///
/// Tests run side by side and two of them ask for `snare.wav`: written
/// once, by one writer, to a name of its own and renamed into place, so a
/// test never reads it half written (#2024). ffmpeg makes its output file
/// before it writes it; reading it then, a test exported silence ("mix:
/// -inf LUFS") and its ceiling check had nothing to measure.
fn transients(name: &str, full_band: bool) -> PathBuf {
    static WRITING: Mutex<()> = Mutex::new(());
    let _one_writer = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let out = klipp_testing::scratch::scratch_file("klipp-codec-peak", name);
    if out.exists() {
        return out;
    }
    let part = out.with_extension("part.wav");
    // Commas escaped: in a filter graph they separate filters.
    // Bright enough that an AAC encode of the limited mix runs half a
    // decibel over a -3 dBTP ceiling the mix met, as the bug report found.
    let (amp, freq, lp) = (1.2, 5000.0, 14000.0);
    let snare = format!(r"if(lt(mod(t\,0.25)\,0.05)\,(random(0)*2-1)*{amp}*exp(-mod(t\,0.25)*60)\,0)");
    let click: &str = if full_band {
        r"if(lt(mod(t+0.13\,0.25)\,0.0006)\,0.95\,0)"
    } else {
        &format!(r"if(lt(mod(t+0.13\,0.25)\,0.004)\,{amp}*sin(2*PI*{freq}*t)*exp(-mod(t+0.13\,0.25)*900)\,0)")
    };
    let wave = format!(r"0.2*sin(2*PI*110*t)+{snare}+{click}");
    let shape = if full_band { String::new() } else { format!(",lowpass=f={lp}") };
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc={wave}|{wave}:s=48000:d=6{shape}"))
        .args(["-c:a", "pcm_f32le"])
        .arg(&part)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "the transients");
    std::fs::rename(&part, &out).unwrap();
    out
}

fn snapshot(source: PathBuf) -> SequenceSnapshot {
    let clip = AudioClipSnapshot {
        timeline_start_frame: 0,
        timeline_end_frame: FRAMES,
        source_start_secs: 0.0,
        path: source,
        audio_effects: vec![],
        audio_effect_keyframes: Default::default(),
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
        audio_tracks: vec![AudioTrackSnapshot { volume: 1.0, pan: 0.0, muted: false, solo: false, clips: vec![clip], automation: Default::default() }],
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: WorkingColorspace::Rec709Linear,
        grade_encoding: GradeEncoding::Bt1886,
    }
}

/// Export `source` with `settings`; the status the queue would show.
fn export(source: PathBuf, settings: ExportSettings) -> String {
    let status = Arc::new(Mutex::new(None));
    ExportJob {
        id: uuid::Uuid::new_v4(),
        output_path: settings.output_path.clone(),
        settings,
        snapshot: snapshot(source),
        cancel: Arc::new(AtomicBool::new(false)),
        progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(),
        render_stage: Default::default(),
        loudness_result: Arc::clone(&status),
        timings: Default::default(),
        total_frames: FRAMES,
        analyser: None,
    }
    .run()
    .unwrap();
    let s = status.lock().unwrap().clone().expect("a normalised delivery has a status");
    s
}

/// (integrated LUFS, true peak dBTP) by ffmpeg's EBU R128 meter.
fn ebur128(path: &Path) -> (f64, f64) {
    let o = std::process::Command::new("ffmpeg").args(["-nostdin", "-i"]).arg(path).args(["-af", "ebur128=peak=true", "-f", "null", "-"]).output().unwrap();
    let text = String::from_utf8_lossy(&o.stderr);
    let summary = &text[text.rfind("Summary:").unwrap_or(0)..];
    let value = |key: &str| summary.lines().find_map(|l| l.trim().strip_prefix(key)).and_then(|v| v.split_whitespace().next()?.parse().ok()).unwrap_or(f64::NAN);
    (value("I:"), value("Peak:"))
}

#[test]
fn a_lossy_delivery_is_under_its_ceiling_as_written() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-codec-peak");
    let mut cases = vec![(ExportFormat::Mp4, ExportAudioCodec::Aac, "mp4")];
    if klipp_render::audio_only::has_encoder(ExportAudioCodec::Mp3) {
        cases.push((ExportFormat::Mp3, ExportAudioCodec::Mp3, "mp3"));
    }
    for (format, codec, ext) in cases {
        for ceiling in [-2.0f32, -3.0] {
            let out = dir.join(format!("delivery{ceiling}.{ext}"));
            let status = export(transients("snare.wav", false), ExportSettings {
                output_path: out.clone(),
                format,
                audio_codec: codec,
                audio_bitrate_kbps: Some(192),
                target_lufs: Some(-16.0),
                true_peak_dbtp: Some(ceiling),
                ..Default::default()
            });
            let (lufs, peak) = ebur128(&out);
            assert!(peak <= ceiling as f64 + 0.1, "{codec:?} at {ceiling} dBTP: ffmpeg reads the file at {peak} dBTP ({status})");
            assert!((lufs + 16.0).abs() < 1.0, "{codec:?} at {ceiling} dBTP: {lufs} LUFS for -16 ({status})");
            assert!(status.contains("written file"), "the status names the file it measured: {status}");
            assert!(!status.contains("over the"), "the status says the file is under: {status}");
        }
    }
}

/// A lossless delivery is measured as written too, and needs no trial
/// encode: what it decodes to is what was mixed.
#[test]
fn a_wav_delivery_reports_its_file() {
    if !ffmpeg_ok() {
        return;
    }
    let out = klipp_testing::scratch::scratch_dir("klipp-codec-peak").join("delivery.wav");
    let status = export(transients("snare.wav", false), ExportSettings {
        output_path: out.clone(),
        format: ExportFormat::Wav,
        audio_codec: ExportAudioCodec::Pcm24,
        target_lufs: Some(-16.0),
        true_peak_dbtp: Some(-2.0),
        ..Default::default()
    });
    let (_, peak) = ebur128(&out);
    assert!(peak <= -1.9, "{peak} dBTP ({status})");
    assert!(status.contains("written file"), "{status}");
}

/// Material no sample-peak limiter can hold to a true-peak ceiling (noise
/// to Nyquist, square clicks): the fit stops after its passes, and the
/// status says the file is over and by how much, with both measurements,
/// rather than claiming the ceiling.
#[test]
fn a_file_still_over_its_ceiling_says_so() {
    if !ffmpeg_ok() {
        return;
    }
    let out = klipp_testing::scratch::scratch_dir("klipp-codec-peak").join("harsh.mp4");
    let status = export(transients("harsh.wav", true), ExportSettings {
        output_path: out.clone(),
        audio_bitrate_kbps: Some(192),
        target_lufs: Some(-16.0),
        true_peak_dbtp: Some(-2.0),
        ..Default::default()
    });
    let (_, peak) = ebur128(&out);
    if peak <= -1.95 {
        // This build's limiter held it after all; nothing to say.
        return;
    }
    assert!(status.contains("mix:") && status.contains("written file:"), "both measurements: {status}");
    assert!(status.contains("over the -2.0 dBTP ceiling"), "the file at {peak} dBTP is said to be over: {status}");
}

/// The trial encodes measure what the export writes only if their encoder
/// opens as the export's does: the same codec, sample format, rate,
/// channels, frame size and bitrate, for each audio codec an export takes.
#[test]
fn a_trial_encoder_opens_as_the_exports_does() {
    use klipp_render::audio_only::{has_encoder, trial_muxer, AudioOnly};
    if !ffmpeg_ok() {
        return;
    }
    let dir = klipp_testing::scratch::scratch_dir("klipp-codec-peak-open");
    let snap = snapshot(transients("snare.wav", false));
    for (format, codec, kbps) in [
        (ExportFormat::Mp4, ExportAudioCodec::Aac, Some(192)),
        (ExportFormat::Mp4, ExportAudioCodec::Aac, None),
        (ExportFormat::Mkv, ExportAudioCodec::Mp3, Some(256)),
        (ExportFormat::Mkv, ExportAudioCodec::Opus, Some(128)),
        (ExportFormat::Mkv, ExportAudioCodec::Flac, None),
        (ExportFormat::Mov, ExportAudioCodec::Pcm16, None),
        (ExportFormat::Mov, ExportAudioCodec::Pcm24, None),
    ] {
        if !has_encoder(codec) {
            continue;
        }
        let settings = ExportSettings {
            output_path: dir.join(format!("open.{}", format.extension())),
            format,
            audio_codec: codec,
            audio_bitrate_kbps: kbps,
            ..Default::default()
        };
        let export = klipp_render::Encoder::open(&settings.output_path, &settings, &snap).unwrap_or_else(|e| panic!("{codec:?} in {format:?}: {e}"));
        let trial = AudioOnly::open_muxer(&dir.join("open.trial"), trial_muxer(codec).unwrap(), codec, kbps, snap.sample_rate).unwrap();
        let opened = export.opened_audio().expect("the export has sound");
        assert_eq!(trial.opened(), opened, "{codec:?} at {kbps:?} kb/s");
        assert_eq!(opened.rate, snap.sample_rate);
    }
}
