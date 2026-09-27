//! An audio crossfade in the render: a transition on an audio track
//! plays the outgoing clip on into its tail and the incoming clip from
//! before its head, over the transition's window, on the constant-power
//! curves — built from a real sequence, through the same snapshot the
//! export uses.
//!
//! The fixtures are sounds whose sample value says what they are: a ramp
//! whose value is its source time (`v = t/4 - 1`), and constant levels,
//! so a test reads "which second of which file, at what gain" straight
//! off a sample. Needs the ffmpeg CLI to write them; skips without it.

use klipp_core::{ExportSettings, Timecode, TransitionAlignment, TransitionKind, WorkingColorspace};
use klipp_media::{MediaItem, MediaPool};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::AudioMixdown;
use klipp_timeline::{Clip, Sequence, Track, TrackKind, Transition};
use std::path::PathBuf;

const RATE: u32 = 48000;
const FPS: u32 = 24;

fn tc(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, FPS)
}

/// A stereo 8 s FLAC made by ffmpeg's `aevalsrc` from `expr` (per
/// channel), written once per process.
fn fixture(name: &'static str, expr: &'static str) -> Option<PathBuf> {
    let ok = std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success());
    if !ok {
        return None;
    }
    let out = klipp_testing::scratch::scratch_file("klipp-audio-crossfade", name);
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc={expr}|{expr}:s={RATE}:d=8"))
        .args(["-c:a", "flac", "-sample_fmt", "s16"])
        .arg(&out)
        .status()
        .ok()?
        .success()
        .then_some(out)
}

fn ramp() -> Option<PathBuf> {
    static F: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    F.get_or_init(|| fixture("ramp.flac", "t/4-1")).clone()
}
fn silence() -> Option<PathBuf> {
    static F: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    F.get_or_init(|| fixture("silence.flac", "0")).clone()
}
fn level(v: &'static str, name: &'static str, cell: &'static std::sync::OnceLock<Option<PathBuf>>) -> Option<PathBuf> {
    cell.get_or_init(|| fixture(name, v)).clone()
}

fn secs_of(v: f32) -> f64 {
    (v as f64 + 1.0) * 4.0
}

/// A sequence with one audio track: clip A at 0–3 s (source 1–4 s) and
/// clip B at 3–6 s (source 2–5 s), each with handles, joined by a
/// half-second crossfade centred on the cut — the window 2.75–3.25 s.
fn sequence(a_path: PathBuf, b_path: PathBuf) -> (Sequence, MediaPool) {
    let mut pool = MediaPool::new();
    let mut item = |p: PathBuf| {
        let mut m = MediaItem::synthetic_for_test(p, None);
        m.has_audio = true;
        m.duration = tc(8.0);
        pool.add(m)
    };
    let (a_media, b_media) = (item(a_path), item(b_path));
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(FPS, 1);
    let mut track = Track::new("A1", TrackKind::Audio);
    let a = Clip::media(a_media, tc(0.0), tc(3.0), tc(1.0), tc(4.0));
    let b = Clip::media(b_media, tc(3.0), tc(6.0), tc(2.0), tc(5.0));
    track.transitions.push(Transition {
        id: klipp_core::TransitionId::new(),
        kind: TransitionKind::CrossDissolve,
        duration: tc(0.5),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: TransitionAlignment::Center,
    });
    track.clips = vec![a, b];
    seq.tracks = vec![track];
    (seq, pool)
}

fn mixdown(seq: &Sequence, pool: &MediaPool) -> AudioMixdown {
    let snap = SequenceSnapshot::from_sequence_with_all(seq, std::slice::from_ref(seq), pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap();
    AudioMixdown::new(&snap).unwrap()
}

/// Constant-power gains at progress `p` through the window.
fn gains(p: f64) -> (f64, f64) {
    let cp = |t: f64| (t * std::f64::consts::FRAC_PI_2).sin();
    (cp(1.0 - p), cp(p))
}

/// Frame 70 begins at 70/24 s, a third of the way through the window
/// (2.9167 s), before the cut; frame 76 at 3.1667 s, after it.
const BEFORE_CUT: u64 = 70;
const AFTER_CUT: u64 = 76;

fn progress(frame: u64) -> f64 {
    (frame as f64 / FPS as f64 - 2.75) / 0.5
}

#[test]
fn the_outgoing_clip_plays_on_into_its_tail() {
    let (Some(r), Some(s)) = (ramp(), silence()) else { return };
    let (seq, pool) = sequence(r, s);
    let mut mix = mixdown(&seq, &pool);
    // Past the cut, A is heard from beyond its out point: source 4 s at
    // the cut, so 4.1667 s here, at the outgoing gain.
    let (ga, _) = gains(progress(AFTER_CUT));
    let v = mix.get_frame_audio(AFTER_CUT)[0] as f64 / ga;
    let heard = secs_of(v as f32);
    assert!((heard - (4.0 + 1.0 / 6.0)).abs() < 0.002, "A heard at source {heard:.4} s");
    // After the window A is gone.
    assert!(mix.get_frame_audio(80).iter().all(|&x| x.abs() < 1e-4), "A still sounding after the window");
}

#[test]
fn the_incoming_clip_starts_from_before_its_head() {
    let (Some(r), Some(s)) = (ramp(), silence()) else { return };
    let (seq, pool) = sequence(s, r);
    let mut mix = mixdown(&seq, &pool);
    // Before the cut B is already sounding, from before its in point:
    // source 2 s at the cut, so 1.9167 s here, at the incoming gain.
    let (_, gb) = gains(progress(BEFORE_CUT));
    let v = mix.get_frame_audio(BEFORE_CUT)[0] as f64 / gb;
    let heard = secs_of(v as f32);
    assert!((heard - (2.0 - 1.0 / 12.0)).abs() < 0.002, "B heard at source {heard:.4} s");
    assert!(mix.get_frame_audio(60).iter().all(|&x| x.abs() < 1e-4), "B sounding before the window");
}

#[test]
fn the_two_sides_follow_the_constant_power_curves_through_the_window() {
    static A: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    static B: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    let (Some(a), Some(b)) = (level("0.6", "a.flac", &A), level("0.8", "b.flac", &B)) else { return };
    let (seq, pool) = sequence(a, b);
    let mut mix = mixdown(&seq, &pool);
    for frame in [66u64, 70, 72, 74, 76, 78] {
        let (ga, gb) = gains(progress(frame));
        let want = 0.6 * ga + 0.8 * gb;
        let got = mix.get_frame_audio(frame)[0] as f64;
        assert!((got - want).abs() < 0.002, "frame {frame}: {got:.4}, want {want:.4}");
    }
    // Outside the window each clip is itself, at unity.
    assert!((mix.get_frame_audio(40)[0] as f64 - 0.6).abs() < 0.002);
    assert!((mix.get_frame_audio(100)[0] as f64 - 0.8).abs() < 0.002);
}

#[test]
fn a_crossfade_on_a_picture_track_leaves_the_sound_alone() {
    let (Some(r), Some(s)) = (ramp(), silence()) else { return };
    let (mut seq, pool) = sequence(r, s);
    seq.tracks[0].kind = TrackKind::Video;
    let snap = SequenceSnapshot::from_sequence_with_all(&seq, std::slice::from_ref(&seq), &pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap();
    let clips: Vec<(u64, u64)> = snap.audio_tracks.iter().flat_map(|t| t.clips.iter().map(|c| (c.timeline_start_frame, c.timeline_end_frame))).collect();
    assert_eq!(clips, vec![(0, 72), (72, 144)]);
}

/// A 30-rate clip on a 25 fps sequence: the crossfade's half is
/// computed in the clip's rate (12 frames at 25 fps rescale to 14 at
/// 30, 0.4667 s) and is not a whole sequence frame. The export moves
/// the clip's start back by whole frames, so the sample at the cut has
/// to be the clip's in point all the same, and everything after it in
/// step with the picture.
#[test]
fn a_clip_at_another_rate_than_the_sequence_stays_in_sync_after_the_cut() {
    let (Some(r), Some(s)) = (ramp(), silence()) else { return };
    const SEQ_FPS: u32 = 25;
    let t30 = |secs: f64| Timecode::from_secs_f64(secs, 30);
    let mut pool = MediaPool::new();
    let mut item = |p: PathBuf| {
        let mut m = MediaItem::synthetic_for_test(p, None);
        m.has_audio = true;
        m.duration = Timecode::from_secs_f64(8.0, SEQ_FPS);
        pool.add(m)
    };
    let (a_media, b_media) = (item(s), item(r));
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(SEQ_FPS, 1);
    let mut track = Track::new("A1", TrackKind::Audio);
    // A silent at 0–3 s, B the ramp at 3–6 s from source 2 s, both at
    // rate 30; a 24-frame crossfade centred on the cut.
    let a = Clip::media(a_media, t30(0.0), t30(3.0), t30(1.0), t30(4.0));
    let b = Clip::media(b_media, t30(3.0), t30(6.0), t30(2.0), t30(5.0));
    track.transitions.push(Transition {
        id: klipp_core::TransitionId::new(),
        kind: TransitionKind::CrossDissolve,
        duration: Timecode::from_frames(24, SEQ_FPS),
        clip_a_id: a.id,
        clip_b_id: b.id,
        user_params: Default::default(),
        alignment: TransitionAlignment::Center,
    });
    track.clips = vec![a, b];
    seq.tracks = vec![track];
    let mut mix = mixdown(&seq, &pool);
    // Frame 100 is 4 s, a second past the cut and well past the window:
    // B at unity, and a second into it — source 3 s.
    let heard = secs_of(mix.get_frame_audio(100)[0]);
    assert!((heard - 3.0).abs() < 0.002, "B heard at source {heard:.4} s a second after the cut, want 3.0000");
    // And at the cut itself, its in point, at the incoming gain of a
    // centred crossfade.
    let (_, gb) = gains(0.5);
    let heard = secs_of((mix.get_frame_audio(75)[0] as f64 / gb) as f32);
    assert!((heard - 2.0).abs() < 0.002, "B heard at source {heard:.4} s at the cut, want 2.0000");
}
