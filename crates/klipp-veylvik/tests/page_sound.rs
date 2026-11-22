//! The Veylvik page plays the clip's sound as Edit plays it: the sound
//! linked to the clip, its gain and fade, mixed by Klipp's own mixdown,
//! sample for sample at the media time Veylvik asks for, the picture's
//! offset from its sound (an L-cut) kept, and silence where it has none.

use klipp_core::{MediaId, RationalTime};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use klipp_veylvik::clip_sound::ClipSound;

const RATE: u32 = 48_000;

fn at(f: i64) -> RationalTime {
    RationalTime { value: f, rate: 24 }
}

/// A 3 s tone, `None` without ffmpeg.
fn tone(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let path = dir.join("tone-3s.wav");
    let made = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3"])
        .arg(&path)
        .status()
        .ok()?;
    made.success().then_some(path)
}

/// A sequence of one picture clip, 1 s to 3 s on the timeline from 0.5 s of
/// its media, and its sound a quarter second later, half as loud, fading in.
fn scene(media: MediaId) -> (Sequence, klipp_core::ClipId) {
    let picture = Clip::media(media, at(24), at(72), at(12), at(60));
    let mut sound = Clip::media(media, at(30), at(72), at(18), at(60));
    sound.gain = 0.5;
    sound.fade_in_secs = 0.5;
    sound.linked_clip_id = Some(picture.id);
    let id = picture.id;
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![picture];
    let mut a = Track::new("A1", TrackKind::Audio);
    a.clips = vec![sound];
    a.volume = 0.3;
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(24, 1);
    seq.tracks = vec![v, a];
    (seq, id)
}

#[test]
fn the_page_hears_the_clips_sound_as_edit_plays_it() {
    let dir = std::env::temp_dir().join(format!("klipp-page-sound-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(path) = tone(&dir) else {
        eprintln!("UNVERIFIED: no ffmpeg");
        return;
    };
    let mut pool = MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
    item.duration = at(72);
    item.has_audio = true;
    item.audio_codec = "pcm_s16le".into();
    item.video_codec.clear();
    let media = pool.add(item);
    let (seq, clip) = scene(media);

    // Edit's mix of the clip's sound alone, from the timeline's start.
    let snap = klipp_render::stem::clip_sound_snapshot(&seq, &[], &pool, clip).unwrap().expect("the clip has sound");
    let edit: Vec<f32> = klipp_render::stem::render_stem(&snap, &mut |_| true).unwrap().unwrap();

    let sound = ClipSound::new(&seq, &[], &pool, clip).expect("the page has the clip's sound");
    // Media 0.5 s to 2.5 s is timeline 1 s to 3 s.
    let pcm = sound.pcm([0.5, 2.5], RATE).expect("sound");
    assert_eq!((pcm.rate, pcm.start), (RATE, 0.5));
    assert_eq!(pcm.samples.len(), 2 * 2 * RATE as usize);
    let from = RATE as usize * 2;
    let want = &edit[from..from + pcm.samples.len()];
    let worst = pcm.samples.iter().zip(want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    eprintln!("MEASURE the page's sound vs Edit's mix: worst {worst:e}");
    assert!(edit.iter().any(|s| s.abs() > 0.01), "Edit hears the clip");
    assert!(worst < 1e-6, "{worst}");
    // The quarter second before its sound starts is silent, then it sounds.
    let quarter = RATE as usize / 4 * 2;
    assert!(pcm.samples[..quarter].iter().all(|s| *s == 0.0), "sound before the sound clip starts");
    assert!(pcm.samples[quarter..].iter().any(|s| s.abs() > 0.01), "the clip's sound is heard");
    // The track's fader (0.3) isn't the clip's: unity, as a bounced
    // track hears it. The tone peaks at 0.125, the clip's gain halves it.
    let loudest = pcm.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    eprintln!("MEASURE loudest {loudest}");
    assert!(loudest > 0.03, "{loudest}");

    // Past the media's end, and before the clip: silence, the length asked.
    let beyond = sound.pcm([2.5, 3.5], 44_100).expect("sound");
    assert_eq!(beyond.samples.len(), 2 * 44_100);
    assert!(beyond.samples.iter().all(|s| *s == 0.0));

    // A retimed clip, and one with nothing linked, give none.
    let mut fast = seq.clone();
    fast.tracks[0].clips[0].speed = 2.0;
    assert!(ClipSound::new(&fast, &[], &pool, clip).is_none());
    let mut bare = seq.clone();
    bare.tracks[1].clips.clear();
    assert!(ClipSound::new(&bare, &[], &pool, clip).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
