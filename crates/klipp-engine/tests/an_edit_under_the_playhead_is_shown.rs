//! An edit that changes what is under the parked playhead shows the new
//! picture without the playhead moving.
//!
//! The UI writes an edited sequence into the shared lock and, for many
//! edits, says nothing else. The engine went on showing the frame it had
//! parked on — the clip that was there before the edit — and when it did
//! re-render, the primary decoder was matched to the clip by its index on
//! the track, so a clip inserted at that index was drawn with the old
//! clip's decoder. Only moving the playhead brought the right frame.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

/// Two seconds of one solid colour, lossless.
fn solid(name: &str, colour: &str) -> Option<PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-edit-under-playhead-{name}-{W}x{H}.mkv"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi"])
        .args(["-i", &format!("color=c={colour}:s={W}x{H}:r={}:d=2", common::FPS)])
        .args(["-vf", &format!("format=yuv420p,setparams=range=tv:{}", common::SDR_TAGS), "-c:v", "ffv1"])
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

fn item(id: MediaId, path: &std::path::Path, fps: Fps) -> MediaItem {
    MediaItem {
        id,
        path: path.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), common::frame_tc(50, fps), fps)
    }
}

fn clip_of(media_id: MediaId, fps: Fps) -> Clip {
    let tc = |f| common::frame_tc(f, fps);
    let mut clip = Clip { kind: ClipKind::Media { media_id }, ..Clip::gap(tc(0), tc(50)) };
    clip.source_in = tc(0);
    clip.source_out = tc(50);
    clip
}

struct Engine {
    cmd: crossbeam_channel::Sender<AppCommand>,
    fb: Arc<Mutex<FrameBuffer>>,
    seq: Arc<klipp_timeline::SharedSequence>,
    thread: Option<klipp_engine::RunningEngine>,
}

impl Engine {
    fn generation(&self) -> u64 {
        self.fb.lock().unwrap().latest_generation()
    }

    /// Wait for a frame newer than `since`, then for the picture to stop
    /// changing.
    fn settle_after(&self, since: u64) -> bool {
        let t0 = std::time::Instant::now();
        while self.generation() == since {
            if t0.elapsed() > Duration::from_secs(10) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut g = self.generation();
        loop {
            std::thread::sleep(common::QUIET);
            let now = self.generation();
            if now == g {
                return true;
            }
            g = now;
        }
    }

    /// The colour at the centre of the frame on screen.
    fn centre(&self) -> [u8; 3] {
        let fb = self.fb.lock().unwrap();
        let (px, w, h, _) = fb.latest_frame();
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            // A decode thread still in ffmpeg's teardown at exit crashes the
            // process (CUDA's context destroyed under it).
            klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
        }
    }
}

fn is_red(c: [u8; 3]) -> bool {
    c[0] > 200 && c[1] < 60 && c[2] < 60
}

fn is_blue(c: [u8; 3]) -> bool {
    c[2] > 200 && c[0] < 60 && c[1] < 60
}

/// The engine parked on frame 10 of a red clip, with a blue file (and a
/// third, `extra`) in its pool.
fn parked_on_red(extra: Option<(MediaId, PathBuf)>) -> Option<(Engine, MediaId)> {
    let (red, blue) = (solid("red", "red")?, solid("blue", "blue")?);
    let fps = Fps::new(common::FPS, 1);
    let (red_id, blue_id) = (MediaId::new(), MediaId::new());
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    let mut t = Track::new("V1", TrackKind::Video);
    t.clips = vec![clip_of(red_id, fps)];
    seq.tracks = vec![t];
    let mut pool = MediaPool::default();
    pool.add(item(red_id, &red, fps));
    pool.add(item(blue_id, &blue, fps));
    if let Some((id, path)) = &extra {
        pool.add(item(*id, path, fps));
    }

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let seq = Arc::new(klipp_timeline::SharedSequence::new(seq));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::clone(&seq), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = Some(engine.spawn());
    let e = Engine { cmd, fb, seq, thread };
    let g = e.generation();
    e.cmd.send(AppCommand::SetPlayhead(common::frame_tc(10, fps))).unwrap();
    assert!(e.settle_after(g), "no first frame");
    assert!(is_red(e.centre()), "parked on the red clip, showing {:?}", e.centre());
    Some((e, blue_id))
}

/// Put the blue clip where the red one was: at the same index on the same
/// track, as an insert or overwrite at the playhead does.
fn replace_with_blue(e: &Engine, blue_id: MediaId) {
    let mut seq = e.seq.write().unwrap();
    let fps = seq.fps;
    seq.tracks[0].clips = vec![clip_of(blue_id, fps)];
}

#[test]
fn an_edit_written_to_the_sequence_alone_is_shown() {
    let Some((e, blue)) = parked_on_red(None) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let g = e.generation();
    replace_with_blue(&e, blue);
    assert!(e.settle_after(g), "the edit rendered nothing");
    assert!(is_blue(e.centre()), "the clip now under the playhead is blue; the viewer shows {:?}", e.centre());
}

#[test]
fn an_edit_followed_by_sequence_updated_is_shown() {
    let Some((e, blue)) = parked_on_red(None) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let g = e.generation();
    replace_with_blue(&e, blue);
    e.cmd.send(AppCommand::SequenceUpdated).unwrap();
    assert!(e.settle_after(g), "the edit rendered nothing");
    assert!(is_blue(e.centre()), "the clip now under the playhead is blue; the viewer shows {:?}", e.centre());
}

/// An import probes its file on the engine thread. The probe used to take
/// over the engine's playback cursor — the clip index, the playhead, the
/// file it believed its decoder held — so the next edit was drawn from
/// the wrong decoder.
#[test]
fn an_import_while_parked_does_not_confuse_the_next_edit() {
    let Some(green) = solid("green", "lime") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let green_id = MediaId::new();
    let Some((e, blue)) = parked_on_red(Some((green_id, green.clone()))) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    e.cmd.send(AppCommand::LoadMediaFile { path: green, media_id: green_id }).unwrap();
    let g = e.generation();
    replace_with_blue(&e, blue);
    e.cmd.send(AppCommand::SequenceUpdated).unwrap();
    assert!(e.settle_after(g), "the edit rendered nothing");
    assert!(is_blue(e.centre()), "the clip now under the playhead is blue; the viewer shows {:?}", e.centre());
}
