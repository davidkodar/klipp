//! Normalizing clips (#1843): what `klipp_render::normalize` measures of a
//! clip and the gain `klipp_core::normalize` gives it, checked by applying
//! that gain to the file with ffmpeg and measuring again.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use klipp_core::normalize::{gains, NormalizeMode, NormalizeSettings};
use klipp_render::normalize::{measure, ClipSound};

fn ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// `secs` of a 997 Hz tone at `db` dBFS, stereo, after `lead` seconds of
/// silence, as float WAV. Not 1 kHz: at 48 kHz its float samples repeat
/// every 384 bytes, and at some levels ffmpeg's probe reads the file as
/// MPEG-TS.
fn tone(name: &str, db: f64, secs: f64, lead: f64) -> PathBuf {
    let out = klipp_testing::scratch::scratch_file("normalize-clips", &format!("{name}.wav"));
    // Exactly `db` at its crest on both channels: ffmpeg's `sine` source
    // plays at 1/8 of full scale.
    let a = 10f64.powf(db / 20.0);
    let wave = format!("{a}*sin(2*PI*997*t)");
    let filter = format!("adelay={ms}|{ms},apad=whole_dur={whole}", ms = (lead * 1000.0) as u64, whole = lead + secs);
    let ok = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc={wave}|{wave}:s=48000:d={secs}"))
        .args(["-af", &filter, "-c:a", "pcm_f32le"])
        .arg(&out)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg made the tone");
    out
}

/// `src` with `db` of gain, as a clip's gain plays it.
fn with_gain(src: &Path, db: f64) -> PathBuf {
    let out = src.with_file_name(format!("{}-{db:.3}.wav", src.file_stem().unwrap().to_string_lossy()));
    let ok = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-af", &format!("volume={db}dB"), "-c:a", "pcm_f32le"])
        .arg(&out)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg applied the gain");
    out
}

fn whole(path: &Path, secs: f64) -> ClipSound {
    ClipSound { path: path.to_path_buf(), source_in_secs: 0.0, source_out_secs: secs }
}

fn measured(clips: &[ClipSound]) -> klipp_render::normalize::Measured {
    measure(clips, &AtomicBool::new(false)).expect("measured")
}

#[test]
fn a_normalized_clip_measures_at_its_target() {
    if !ffmpeg() {
        return;
    }
    let src = tone("quiet", -30.0, 6.0, 0.0);
    for target in [-14.0, -16.0, -23.0, -24.0] {
        let s = NormalizeSettings { target, ceiling_dbtp: -1.0, ..Default::default() };
        let m = measured(&[whole(&src, 6.0)]);
        let g = gains(&m.levels, &m.together, &s)[0].expect("a gain");
        assert!(!g.ceiling_limited);
        let after = measured(&[whole(&with_gain(&src, g.db), 6.0)]).levels[0];
        assert!((after.integrated_lufs - target).abs() < 0.1, "{target} LUFS asked, {after:?} measured after {g:?}");
    }
}

#[test]
fn peak_mode_brings_the_highest_sample_to_its_target() {
    if !ffmpeg() {
        return;
    }
    let src = tone("peak", -18.0, 3.0, 0.0);
    let s = NormalizeSettings { mode: NormalizeMode::Peak, target: -3.0, ceiling_dbtp: 0.0, group: false };
    let m = measured(&[whole(&src, 3.0)]);
    let g = gains(&m.levels, &m.together, &s)[0].unwrap();
    let after = measured(&[whole(&with_gain(&src, g.db), 3.0)]).levels[0];
    assert!((after.sample_peak_dbfs + 3.0).abs() < 0.05, "{after:?}");
}

#[test]
fn a_group_keeps_its_clips_apart_by_what_they_were() {
    if !ffmpeg() {
        return;
    }
    let (a, b) = (tone("group-a", -20.0, 4.0, 0.0), tone("group-b", -28.0, 4.0, 0.0));
    let s = NormalizeSettings { target: -16.0, ceiling_dbtp: -1.0, group: true, ..Default::default() };
    let m = measured(&[whole(&a, 4.0), whole(&b, 4.0)]);
    let before = m.levels[0].integrated_lufs - m.levels[1].integrated_lufs;
    let got = gains(&m.levels, &m.together, &s);
    let (ga, gb) = (got[0].unwrap(), got[1].unwrap());
    assert_eq!(ga, gb, "one gain for the group");
    let after = measured(&[whole(&with_gain(&a, ga.db), 4.0), whole(&with_gain(&b, gb.db), 4.0)]);
    let kept = after.levels[0].integrated_lufs - after.levels[1].integrated_lufs;
    assert!((kept - before).abs() < 0.05, "{before} LU apart before, {kept} after");
    assert!((after.together.integrated_lufs + 16.0).abs() < 0.1, "together at the target: {:?}", after.together);
}

#[test]
fn the_ceiling_holds_the_true_peak() {
    if !ffmpeg() {
        return;
    }
    // A stereo tone at -6 dBFS reads about -6 LUFS: 0 LUFS asks for +6 dB,
    // and its true peak has 5 dB of room under a -1 dBTP ceiling.
    let src = tone("loud", -6.0, 3.0, 0.0);
    let s = NormalizeSettings { target: 0.0, ceiling_dbtp: -1.0, ..Default::default() };
    let m = measured(&[whole(&src, 3.0)]);
    let g = gains(&m.levels, &m.together, &s)[0].unwrap();
    assert!(g.ceiling_limited, "{g:?} for {:?}", m.levels[0]);
    let after = measured(&[whole(&with_gain(&src, g.db), 3.0)]).levels[0];
    assert!(after.true_peak_dbtp <= -1.0 + 0.05, "{after:?}");
}

#[test]
fn only_the_clips_part_of_its_file_is_measured() {
    if !ffmpeg() {
        return;
    }
    // Four seconds of silence, then the tone: the clip from 4 s hears
    // only the tone, the whole file mostly silence, which the gate drops.
    let src = tone("lead", -24.0, 4.0, 4.0);
    let tone_only = measured(&[ClipSound { path: src.clone(), source_in_secs: 4.0, source_out_secs: 8.0 }]).levels[0];
    let reference = measured(&[whole(&tone("plain", -24.0, 4.0, 0.0), 4.0)]).levels[0];
    assert!((tone_only.integrated_lufs - reference.integrated_lufs).abs() < 0.1, "{tone_only:?} against {reference:?}");
    let silence = measured(&[ClipSound { path: src, source_in_secs: 0.0, source_out_secs: 3.5 }]).levels[0];
    assert_eq!(silence.integrated_lufs, f64::NEG_INFINITY, "the silent part is silence: {silence:?}");
}
