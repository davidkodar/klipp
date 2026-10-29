//! #3055: a 4.0 s clip of a 1 kHz tone stretched to 8.0 s sounds for
//! 8.0 s at the same pitch, its pitch kept by default; with its pitch let
//! go it sounds an octave down. Measured in the export mixdown: loudness
//! frame by frame, and upward zero crossings for the pitch.
//!
//! Needs the ffmpeg CLI to write the tone; skips without it.

use klipp_core::{ExportSettings, Fps, RationalTime, WorkingColorspace};
use klipp_document::apply::DocumentView;
use klipp_document::ops::{stretch_at, Edge, StretchTo};
use klipp_media::{MediaItem, MediaPool};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::AudioMixdown;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use std::path::PathBuf;

const FPS: u32 = 24;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, FPS)
}

fn tone() -> Option<PathBuf> {
    let ok = std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("UNVERIFIED: no ffmpeg");
        return None;
    }
    let out = klipp_testing::scratch::scratch_file("klipp-rate-stretch-audio", "tone-1k-48k-10s.wav");
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:duration=10", "-ac", "2", "-c:a", "pcm_s16le"])
        .arg(&out)
        .status()
        .ok()?
        .success()
        .then_some(out)
}

/// The tone's first 4 s on an audio track, stretched to 8 s.
fn stretched(path: PathBuf, keep_pitch: bool) -> (Sequence, MediaPool) {
    let mut pool = MediaPool::new();
    let mut m = MediaItem::synthetic_for_test(path, None);
    m.has_audio = true;
    m.duration = tc(10.0);
    let media = pool.add(m);
    let clip = Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let id = clip.id;
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS, 1);
    let mut track = Track::new("A1", TrackKind::Audio);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let (_, edit, _) = stretch_at(&seq, &pool, &[id], Edge::Out, StretchTo::Length(8.0), false, keep_pitch).expect("the stretch");
    let mut fps = seq.fps;
    let mut seqs = vec![seq];
    DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(&edit);
    (seqs.remove(0), pool)
}

fn frames(seq: &Sequence, pool: &MediaPool) -> Vec<Vec<f32>> {
    let snap = SequenceSnapshot::from_sequence_with_all(seq, std::slice::from_ref(seq), pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap();
    let mut mix = AudioMixdown::new(&snap).unwrap();
    (0..(9 * FPS) as u64).map(|f| mix.get_frame_audio(f)).collect()
}

fn rms(frame: &[f32]) -> f32 {
    let left: Vec<f32> = frame.iter().step_by(2).copied().collect();
    (left.iter().map(|s| s * s).sum::<f32>() / left.len().max(1) as f32).sqrt()
}

/// Upward zero crossings of the left channel over `frames`, per second.
fn hz(frames: &[Vec<f32>]) -> f64 {
    let left: Vec<f32> = frames.iter().flat_map(|f| f.iter().step_by(2).copied()).collect();
    let crossings = left.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    crossings as f64 * FPS as f64 / frames.len() as f64
}

#[test]
fn a_stretched_tone_lasts_twice_as_long_at_its_pitch() {
    let Some(path) = tone() else { return };
    let (seq, pool) = stretched(path.clone(), true);
    let read = frames(&seq, &pool);
    let end = (8 * FPS) as usize;
    let middle = rms(&read[end / 2]);
    assert!(middle > 0.05, "the middle is silent ({middle})");
    // Every frame but the edges (ramped against clicks) sounds, to 8 s.
    for (f, frame) in read.iter().enumerate().take(end - 1).skip(1) {
        assert!(rms(frame) > middle * 0.5, "frame {f} ({:.2} s) has RMS {:.4}, the middle {middle:.4}", f as f64 / FPS as f64, rms(frame));
    }
    assert!(read[end + 2..].iter().all(|f| rms(f) < 1e-3), "silent after 8 s");
    let pitch = hz(&read[FPS as usize..end - FPS as usize]);
    eprintln!("stretched, pitch kept: {pitch:.1} Hz");
    assert!((pitch - 1000.0).abs() < 15.0, "its pitch kept: {pitch:.1} Hz");

    // Its pitch let go: tape-like, an octave down.
    let (seq, pool) = stretched(path, false);
    let read = frames(&seq, &pool);
    let pitch = hz(&read[FPS as usize..end - FPS as usize]);
    eprintln!("stretched, pitch following: {pitch:.1} Hz");
    assert!((pitch - 500.0).abs() < 10.0, "pitch following the speed: {pitch:.1} Hz");
}
