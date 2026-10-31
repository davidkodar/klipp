//! #3219: a clip whose sound ends a little before its picture, as most
//! camera files' does, exported reversed was silent: the clip's source
//! out is the picture's end, and read backward from there, past the
//! sound's last sample, its sound was nothing at all. Forward and
//! reversed, the export mixdown sounds over the clip; reversed, from
//! where the sound exists. The reverse goes through the retime op, which
//! is not refused for the sound's shorter length either.
//!
//! Needs the ffmpeg CLI to write the tone; skips without it.

use klipp_core::{ExportSettings, Fps, RationalTime, WorkingColorspace};
use klipp_document::apply::DocumentView;
use klipp_document::ops::retime_at;
use klipp_media::{MediaItem, MediaPool};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::AudioMixdown;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use std::path::PathBuf;

const FPS: u32 = 24;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, FPS)
}

/// 3.98 s of a 1 kHz tone: the sound of a file whose picture runs 4.0 s.
fn tone() -> Option<PathBuf> {
    let ok = std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("UNVERIFIED: no ffmpeg");
        return None;
    }
    let out = klipp_testing::scratch::scratch_file("klipp-reversed-short-sound", "tone-1k-48k-3.98s.wav");
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:duration=3.98", "-ac", "2", "-c:a", "pcm_s16le"])
        .arg(&out)
        .status()
        .ok()?
        .success()
        .then_some(out)
}

/// The whole file, 0 to 4.0 s as its picture says, on an audio track;
/// reversed by the retime op when `reversed`.
fn timeline(path: PathBuf, reversed: bool) -> (Sequence, MediaPool) {
    let mut pool = MediaPool::new();
    let mut m = MediaItem::synthetic_for_test(path, None);
    m.has_audio = true;
    m.duration = tc(4.0);
    let media = pool.add(m);
    let clip = Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
    let id = clip.id;
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS, 1);
    let mut track = Track::new("A1", TrackKind::Audio);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    if !reversed {
        return (seq, pool);
    }
    let (_, edit) = retime_at(&seq, &pool, id, -1.0, false).unwrap_or_else(|r| panic!("the reverse is refused: {}", r.why));
    let mut fps = seq.fps;
    let mut seqs = vec![seq];
    DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(&edit);
    (seqs.remove(0), pool)
}

/// The mixdown's loudness frame by frame over the clip's 4 s.
fn loudness(seq: &Sequence, pool: &MediaPool) -> Vec<f32> {
    let snap = SequenceSnapshot::from_sequence_with_all(seq, std::slice::from_ref(seq), pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap();
    let mut mix = AudioMixdown::new(&snap).unwrap();
    (0..(4 * FPS) as u64)
        .map(|f| {
            let frame = mix.get_frame_audio(f);
            let left: Vec<f32> = frame.iter().step_by(2).copied().collect();
            (left.iter().map(|s| s * s).sum::<f32>() / left.len().max(1) as f32).sqrt()
        })
        .collect()
}

#[test]
fn a_clip_whose_sound_ends_first_sounds_forward_and_reversed() {
    let Some(path) = tone() else { return };
    for reversed in [false, true] {
        let (seq, pool) = timeline(path.clone(), reversed);
        let rms = loudness(&seq, &pool);
        // Every frame but the one at the end the sound misses (the first,
        // reversed; the last, forward) carries the tone.
        let inner = &rms[1..rms.len() - 1];
        let quiet = inner.iter().filter(|r| **r < 0.02).count();
        eprintln!("reversed {reversed}: {quiet} of {} inner frames quiet; first {:.3}, last {:.3}", inner.len(), rms[0], rms[rms.len() - 1]);
        assert_eq!(quiet, 0, "reversed {reversed}: {quiet} of {} frames inside the clip are silent", inner.len());
    }
}
