//! A title edited while playing, before it starts, is warmed again as it
//! now is: its first frame finds its resting renders already made. The
//! warm-up remembered the clip and not what it held, so an edit to a
//! title's text or style was rastered on the frame's thread at its start.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, SharedSequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 30)
}

/// The viewer frames, by their time, that rastered a resting render on
/// the frame's thread: what the latest viewer frame says, read often
/// enough to see each frame. A frame that rasters is a slow one.
#[derive(Default)]
struct Rasters(std::collections::BTreeMap<u64, u64>);

impl Rasters {
    fn look(&mut self) {
        if let Some(f) = klipp_effects::residency::snapshot().latest_viewer_frame {
            let n: u64 = f.events.iter().filter(|e| e.feature == "title.resting").map(|e| e.count).sum();
            if n > 0 {
                self.0.insert(f.seconds.to_bits(), n);
            }
        }
    }

    fn total(&self) -> u64 {
        self.0.values().sum()
    }
}

fn retext(shared: &SharedSequence, text: &str) {
    let mut seq = shared.write().unwrap();
    let ClipKind::Title(td) = &mut seq.tracks[0].clips[0].kind else { unreachable!() };
    td.primary_text_mut().expect("the template has text").text = text.into();
}

/// Plays until the playhead the engine reports is at `secs` or later,
/// counting the frames' rasters on the way.
fn play_to(playhead: &Mutex<f64>, secs: f64, rasters: &mut Rasters) {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        rasters.look();
        if *playhead.lock().unwrap() >= secs {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("playback never reached {secs} s: the playhead was last at {} s", playhead.lock().unwrap());
}

#[test]
fn a_title_edited_before_it_starts_is_warmed_as_it_now_is() {
    // A shape and its text, as most lower thirds are: each warmed on its own.
    let td = klipp_timeline::title_library::TITLES[0].data();
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(30, 1);
    seq.resolution = Resolution { width: W, height: H };
    let mut text = Track::new("T1", TrackKind::Text);
    text.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(1.5), tc(4.0)) }];
    // Another title on from the start, so there is something to play
    // up to it.
    let mut other = Track::new("T2", TrackKind::Text);
    let caption = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "On from the start".into(), font_size: 30.0, ..Default::default() });
    other.clips = vec![Clip { kind: ClipKind::Title(caption), ..Clip::gap(tc(0.0), tc(4.0)) }];
    seq.tracks = vec![text, other];
    let shared = Arc::new(SharedSequence::new(seq));

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let playhead = Arc::new(Mutex::new(0.0));
    let heard = Arc::clone(&playhead);
    std::thread::spawn(move || {
        while let Ok(ev) = ev_rx.recv() {
            if let AppEvent::PlayheadMoved(at) = ev {
                *heard.lock().unwrap() = at.to_secs_f64();
            }
        }
    });
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::clone(&shared), Arc::new(RwLock::new(MediaPool::default())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    cmd.send(AppCommand::SetPlayhead(tc(0.0))).unwrap();
    common::settle(&fb);
    cmd.send(AppCommand::Play).unwrap();
    // Warmed as it was, then edited a second before it starts.
    play_to(&playhead, 0.4, &mut Rasters::default());
    retext(&shared, "Edited before it starts");
    let mut at_start = Rasters::default();
    play_to(&playhead, 2.0, &mut at_start);
    // Edited on screen: nothing can warm that, so its frame rasters it,
    // and the count sees rasters on this route.
    retext(&shared, "Edited on screen");
    let mut on_screen = Rasters::default();
    play_to(&playhead, 2.6, &mut on_screen);
    let (at_start, on_screen) = (at_start.total(), on_screen.total());
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    eprintln!("rastered at its start {at_start}, after the edit on screen {on_screen}");
    if on_screen == 0 {
        let why = "no resting renders counted: the GPU stack did not draw the title";
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required: {why}");
        return eprintln!("skipping: {why}");
    }
    assert_eq!(at_start, 0, "edited before it started, the title's first frame rastered {at_start} resting renders");
}
