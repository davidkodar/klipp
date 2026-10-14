//! A sound placed by its transient (#1928): a file silent but for one
//! click at 0.43 s, placed with `sync_source_secs` 0.43 on a moment off
//! the frame grid, sounds its click on that moment, to the sample, in
//! the export's mix. Needs the ffmpeg CLI to write the file; skips
//! without it.

use klipp_core::*;
use klipp_document::ops::{resolve, Anchor, Op, TrackRef};
use klipp_render::{AudioMixdown, SequenceSnapshot};
use klipp_timeline::{Sequence, Track, TrackKind};

const RATE: u32 = 48_000;

#[test]
fn a_sound_placed_by_its_transient_clicks_on_the_moment() {
    if !std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    let click = klipp_testing::scratch::scratch_file("klipp-place-by-sync", "click.wav");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc=eq(n\\,20640)*0.9|eq(n\\,20640)*0.9:s={RATE}:d=2"))
        .args(["-c:a", "pcm_s16le"])
        .arg(&click)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg wrote the click");
    let mut pool = klipp_media::MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test(click.clone(), None);
    item.duration = RationalTime::from_secs_f64(2.0, RATE);
    item.has_audio = true;
    item.video_codec.clear();
    let sound = pool.add(item);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    seq.tracks = vec![Track::new("V1", TrackKind::Video), Track::new("SFX", TrackKind::Audio)];
    // A moment between frames and between samples' halves: 1.71234 s.
    let on = 1.71234;
    let op = Op::Place { media_id: sound, track: TrackRef::Index(1), at: Anchor::Secs(on), source_in: None, source_out: None, insert: false, with_audio: true, sync_source_secs: Some(0.43) };
    let plan = resolve(&seq, &pool, &[op]).expect("placed");
    let mut seqs = vec![seq];
    let mut fps = seqs[0].fps;
    let mut view = klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
    for step in &plan.steps {
        view.apply_forward(&step.edit);
    }
    let snapshot = SequenceSnapshot::from_sequence(&seqs[0], &pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).expect("a snapshot");
    let mut mix = AudioMixdown::new(&snapshot).expect("a mix");
    let mut heard = None;
    for frame in 0..(3.0 * snapshot.fps) as u64 {
        let first = mix.frame_sample_range(frame).0;
        let pcm = mix.get_frame_audio(frame);
        if let Some(k) = pcm.chunks(2).position(|s| s[0].abs() > 0.3) {
            heard = Some(first + k as u64);
            break;
        }
    }
    let heard = heard.expect("the click is in the mix");
    let want = (on * RATE as f64).round() as i64;
    assert!((heard as i64 - want).abs() <= 1, "the click sounds at sample {heard}, the moment is {want}: {}", plan.text());
}
