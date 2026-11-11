//! A cut while playing changes only the sound that changes there. Music
//! running on across a cut to another file keeps its source, its place
//! and the mix thread, while a sound clip ending on the same frame just
//! stops (#572). A card on a track above the primary, cut in while the
//! primary track has a gap under it, tore the sound down and opened it
//! all again on the engine's thread: a slow tick and dropped frames at
//! each of the card's ends.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

struct Engine {
    cmd: crossbeam_channel::Sender<AppCommand>,
    thread: Option<klipp_engine::RunningEngine>,
    moves: Arc<Mutex<Vec<(Instant, f64)>>>,
    shown: Arc<Mutex<FrameBuffer>>,
}

impl Engine {
    fn start(seq: Sequence, pool: MediaPool) -> Self {
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let moves = Arc::new(Mutex::new(Vec::new()));
        let m = Arc::clone(&moves);
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                if let AppEvent::PlayheadMoved(tc) = ev {
                    m.lock().unwrap().push((Instant::now(), tc.to_secs_f64()));
                }
            }
        });
        let shown = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&shown), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        let thread = Some(engine.spawn());
        Engine { cmd, thread, moves, shown }
    }

    /// The furthest playhead reported since `since`.
    fn furthest_since(&self, since: Instant) -> f64 {
        self.moves.lock().unwrap().iter().filter(|(t, _)| *t >= since).map(|m| m.1).fold(f64::MIN, f64::max)
    }

    /// Wait, at most `limit`, for a playhead at or past `secs` reported since `since`.
    fn wait_past(&self, since: Instant, secs: f64, limit: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < limit {
            if self.furthest_since(since) >= secs {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
        }
    }
}

fn picture(id: MediaId, src: &std::path::Path, fps: Fps) -> MediaItem {
    MediaItem {
        id,
        path: src.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, src.display().to_string(), common::frame_tc(common::RAMP_N as i64, fps), fps)
    }
}

fn sound(id: MediaId, src: &std::path::Path, fps: Fps, secs: i64) -> MediaItem {
    MediaItem {
        id,
        path: src.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        has_audio: true,
        audio_codec: "pcm_s16le".into(),
        audio_channels: Some(2),
        ..MediaItem::new_synthetic_title(id, src.display().to_string(), common::frame_tc(secs * fps.num as i64, fps), fps)
    }
}

/// Frames `from..to` on the timeline, from the file's start.
fn clip(id: MediaId, from: i64, to: i64, fps: Fps) -> Clip {
    let tc = |f| common::frame_tc(f, fps);
    Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) }
}

fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(name, kind);
    t.clips = clips;
    t
}

/// Two seconds of a tone, stereo, as a WAV in the temp folder.
fn tone(name: &str, hz: u32) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-cut-sound-{name}.wav"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "wav");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={hz}:sample_rate=48000:duration=2")])
        .args(["-ac", "2", "-c:a", "pcm_s16le"])
        .arg(&partial)
        .status()
        .ok()?
        .success();
    if !ok {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// What one play across the card counted, from 0.4 s, well into
/// playing, to 1.2 s, past both cuts (0.6 s and 1.0 s) and short of the
/// end (1.6 s).
struct Counted {
    /// The sound torn down whole, sources opened, sources reseeked.
    stops: u64,
    opens: u64,
    reseeks: u64,
    /// Shots taken over from their clip streams, and gap hand-overs that
    /// opened the file again.
    take_overs: u64,
    cold: u64,
}

/// The primary track cuts from shot A to a gap under `card` on the track
/// above, and back to shot C after it; music runs under all of it, and a
/// sound clip ends on the cut to the card. `None` without ffmpeg.
fn play_across(card: impl FnOnce(&mut MediaPool, Fps) -> Clip) -> Option<Counted> {
    // The counts are the process's: one play at a time.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (Some(shots), Some(music), Some(effect)) = (common::ffv1_avi_ramp(), tone("music", 440), tone("effect", 880)) else {
        eprintln!("skipping: no ffmpeg");
        return None;
    };
    let fps = Fps::new(common::FPS, 1);
    let (shot_id, music_id, effect_id) = (MediaId::new(), MediaId::new(), MediaId::new());
    let mut pool = MediaPool::default();
    pool.add(picture(shot_id, &shots, fps));
    pool.add(sound(music_id, &music, fps, 2));
    pool.add(sound(effect_id, &effect, fps, 2));
    let card = card(&mut pool, fps);

    // Frames: A 0–15 and C 25–40 on the primary (the last picture
    // track), the card 15–25 above it; the music 0–40, the effect 5–15.
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![
        track("V2", TrackKind::Video, vec![card]),
        track("V1", TrackKind::Video, vec![clip(shot_id, 0, 15, fps), clip(shot_id, 25, 40, fps)]),
        track("A1", TrackKind::Audio, vec![clip(music_id, 0, 40, fps)]),
        track("A2", TrackKind::Audio, vec![clip(effect_id, 5, 15, fps)]),
    ];
    let opens_at_start = klipp_engine::engine::sound_counts().1;
    let e = Engine::start(seq, pool);

    // Shown at 0.2 s, then played.
    let asked = Instant::now();
    e.cmd.send(AppCommand::SetPlayhead(RationalTime::from_secs_f64(0.2, fps.rate()))).unwrap();
    let on_screen = || {
        let fb = e.shown.lock().unwrap();
        fb.shown_presented().is_some_and(|at| (at.to_secs_f64() - 0.2).abs() < 0.5 / fps.as_f64()) && fb.latest_complete()
    };
    while asked.elapsed() < Duration::from_secs(30) && !on_screen() {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(on_screen(), "the seek to 0.2 s never put its frame on screen");
    let played = Instant::now();
    e.cmd.send(AppCommand::Play).unwrap();

    assert!(e.wait_past(played, 0.4, Duration::from_secs(30)), "playback never reached 0.4 s");
    let before = (klipp_engine::engine::sound_counts(), klipp_engine::engine::stream_take_overs(), klipp_engine::engine::gap_hand_overs().1);
    let reached = e.wait_past(played, 1.2, Duration::from_secs(30));
    let after = (klipp_engine::engine::sound_counts(), klipp_engine::engine::stream_take_overs(), klipp_engine::engine::gap_hand_overs().1);
    let _ = e.cmd.send(AppCommand::Pause);
    assert!(reached, "playback never got past the card: at {:.2} s", e.furthest_since(played));
    assert!(before.0 .1 > opens_at_start, "the sound never opened: nothing here was heard (no sound device?)");
    Some(Counted {
        stops: after.0 .0 - before.0 .0,
        opens: after.0 .1 - before.0 .1,
        reseeks: after.0 .2 - before.0 .2,
        take_overs: after.1 - before.1,
        cold: after.2 - before.2,
    })
}

/// While both cuts play the sound is never stopped whole, and no source
/// is opened again or sent to a new place.
fn assert_sound_ran_on(c: &Counted) {
    assert_eq!(c.stops, 0, "the sound was torn down whole {} time(s) at the cuts", c.stops);
    assert_eq!(c.opens, 0, "{} sound source(s) opened again at the cuts: the music runs on and nothing starts", c.opens);
    assert_eq!(c.reseeks, 0, "{} sound source(s) sent to a new place at the cuts: the music runs on", c.reseeks);
}

/// A card from another file: opened ahead as any layer's shot, and taken
/// over from that stream at both its ends, with the sound running on.
#[test]
fn a_cut_to_a_card_keeps_the_music_running() {
    let Some(card) = common::source("cut-card", W, H, common::SDR_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let Some(c) = play_across(|pool, fps| {
        let id = MediaId::new();
        pool.add(picture(id, &card, fps));
        clip(id, 15, 25, fps)
    }) else {
        return;
    };
    assert_sound_ran_on(&c);
    assert_eq!(c.cold, 0, "a shot was opened again at the card: its stream was ready");
    assert!(c.take_overs >= 2, "the card, and the shot after it, came from their streams: {} did", c.take_overs);
}

/// A title card on a picture track has no stream to take over from: the
/// picture is opened at the cut, and still the sound runs on.
#[test]
fn a_cut_to_a_title_card_keeps_the_music_running() {
    let Some(c) = play_across(|_, fps| {
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "2".into(), font_size: 80.0, ..Default::default() });
        Clip { kind: ClipKind::Title(td), ..Clip::gap(common::frame_tc(15, fps), common::frame_tc(25, fps)) }
    }) else {
        return;
    };
    assert_sound_ran_on(&c);
}
