//! #3230, the issue's own steps: a file whose container runs past its
//! picture's length (the picture starts late) and whose sound ends a
//! little earlier, probed by the engine, imported twice and appended twice
//! to a 30 fps sequence with its linked sound, as the Append command does
//! it. Every clip starts and ends on a frame, none overlaps the next, and
//! the first one retimed to -1 without ripple applies; it was refused as
//! running over the next clip. Skips without ffmpeg.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::{AppCommand, AppEvent, Fps, MediaId, Timecode};
use klipp_timeline::{Clip, Sequence, TrackKind};

use super::linked_audio::attach_linked_audio_to_clip;
use super::media::append_whole_file;

/// The issue's file in small: 473 frames at 29.97 starting 0.066733 s in
/// (so the container's 15.782433 s runs past the picture's 15.7157 s), and
/// 15.765333 s of sound from the start.
fn issue_file() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("klipp-3230-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join("spill.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-itsoffset", "0.066733", "-f", "lavfi", "-i", "testsrc2=s=160x90:r=30000/1001:d=15.7157", "-f", "lavfi", "-i", "sine=f=440:d=15.765333"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

/// `path` probed by a headless engine, as an import probes it, made into
/// the pool's item as the UI makes it of the facts the probe gives.
fn probed(path: &Path) -> klipp_media::MediaItem {
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(64, 64)));
    let engine = klipp_engine::PlaybackEngine::new(
        cmd_rx,
        ev_tx,
        Arc::clone(&fb),
        Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(64, 64))),
        Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default())),
        Arc::new(RwLock::new(klipp_media::MediaPool::default())),
        Box::new(|| {}),
        Arc::new(Mutex::new(None)),
        Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    let media_id = MediaId::new();
    cmd.send(AppCommand::LoadMediaFile { path: path.to_path_buf(), media_id }).unwrap();
    let p = loop {
        match ev_rx.recv_timeout(Duration::from_secs(20)) {
            Ok(AppEvent::MediaLoaded(p)) if p.media_id == media_id => break *p,
            Ok(_) => continue,
            Err(_) => panic!("no MediaLoaded for {}", path.display()),
        }
    };
    cmd.send(AppCommand::Quit).unwrap();
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    let mut item = klipp_media::MediaItem::synthetic_for_test(path.to_path_buf(), None);
    (item.duration, item.fps, item.video_codec, item.audio_codec, item.has_audio, item.sample_rate, item.stream_ends) =
        (p.duration, p.fps, p.video_codec, p.audio_codec, p.has_audio, p.sample_rate, p.stream_ends);
    item
}

fn on_frame(t: Timecode) -> bool {
    let f = t.to_secs_f64() * 30.0;
    (f - f.round()).abs() < 1e-6
}

#[test]
fn the_issue_s_appends_end_on_frames_meet_and_retime() {
    let Some(path) = issue_file() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let one = probed(&path);
    eprintln!("MEASURE probed: container {:.6} s, picture {:?}, sound {:?}", one.duration.to_secs_f64(), one.stream_ends.picture, one.stream_ends.sound);
    assert!(one.stream_ends.picture.is_some() && one.stream_ends.sound.is_some(), "{:?}", one.stream_ends);
    let mut two = one.clone();
    two.id = MediaId::new();

    let mut state = crate::state::AppState::default();
    let seq = &mut state.sequences[0];
    seq.fps = Fps::new(30, 1);
    let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).expect("a video track");
    let mut appended = Vec::new();
    for item in [&one, &two] {
        state.media_pool.add(item.clone());
        let seq = &mut state.sequences[0];
        let clip = append_whole_file(seq, item, v).expect("appended");
        attach_linked_audio_to_clip(seq, clip, None).expect("its sound");
        appended.push(clip);
    }

    let seq = &state.sequences[0];
    for (ti, t) in seq.tracks.iter().enumerate() {
        let mut clips: Vec<&Clip> = t.clips.iter().collect();
        clips.sort_by_key(|c| c.timeline_in);
        for c in &clips {
            assert!(on_frame(c.timeline_in) && on_frame(c.timeline_out), "track {ti}: {:.6}-{:.6} s off the frames", c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
        }
        for w in clips.windows(2) {
            assert!(w[0].timeline_out <= w[1].timeline_in, "track {ti}: {:.6} s runs over {:.6} s", w[0].timeline_out.to_secs_f64(), w[1].timeline_in.to_secs_f64());
        }
    }
    let first = seq.tracks[v].clips.iter().find(|c| c.id == appended[0]).unwrap();
    let picture_end = one.stream_ends.picture.unwrap();
    eprintln!("MEASURE first clip ends {:.6} s, the picture at {picture_end:.6} s", first.timeline_out.to_secs_f64());
    assert!(first.timeline_out.to_secs_f64() <= picture_end + 1e-9, "within the picture");

    // The issue's retime: -1, no ripple.
    let ops = [klipp_document::ops::Op::Retime { clip_id: klipp_document::ops::ClipRef::Id(appended[0]), speed: -1.0, fit: false, ripple: false, interpolation: None, length_secs: None, fit_range: false }];
    let plan = klipp_document::ops::resolve(seq, &state.media_pool, &ops);
    assert!(plan.is_ok(), "retime -1 without ripple applies: {:?}", plan.err());
}
