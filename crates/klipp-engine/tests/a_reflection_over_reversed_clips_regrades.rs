//! A title with a reflection over 4K clips playing reversed, parked and
//! regraded, keeps its engine: in the person's project the paused regrade
//! panicked in the reflection's split ("mid > len"), the layer below it
//! having come back at another size, and the engine thread was gone.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::*;
use klipp_effects::BuiltinEffect;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, TextBox, TitleData, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;

/// Three seconds of 4K at 29.97, as the project's sea footage is. `None`
/// without ffmpeg.
fn uhd() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-uhd-testsrc2-2997-3s.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=30000/1001:d=3")])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
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

fn reflection() -> BuiltinEffect {
    BuiltinEffect::Reflection {
        waterline: 0.55, gap: 0.0, ripple_amount: 0.3, ripple_size: 60.0, ripple_speed: 0.5, breakup: 0.0, smear: 0.0, fade: 0.4, opacity: 0.8, darken: 0.2, contact: 0.0,
        blend: 0, tint: [0, 0, 0], tint_amount: 0.0, tint_auto: false, darken_auto: false, water: [0, 0, 0], water_key: 0, submerge: 0.0, ripple_from: 0, seed: 0, time: 0.0,
    }
}

#[test]
fn a_reflection_over_reversed_clips_regrades_and_the_engine_lives() {
    let Some(path) = uhd() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(30000, 1001);
    let tc = |s: f64| RationalTime::from_secs_f64(s, fps.num);
    let media = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id: media,
        path: path.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still: false,
        fps,
        has_audio: false,
        ..MediaItem::new_synthetic_title(media, path.display().to_string(), tc(3.0), fps)
    });
    // The footage three times, reversed at full speed, at 0.35 and at
    // 0.10, each with grain, as on the project's video track.
    let mut at = 0.0;
    let spans: Vec<(f64, f64, f64)> = vec![(-1.0, 0.0, 3.0), (-0.35, 0.0, 3.0), (-0.10, 0.0, 3.0)];
    let reversed: Vec<Clip> = spans
        .into_iter()
        .map(|(speed, from, to): (f64, f64, f64)| {
            let len = (to - from) / speed.abs();
            let clip = Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(from), source_out: tc(to), speed, ..Clip::gap(tc(at), tc(at + len)) };
            at += len;
            let mut clip = klipp_timeline::test_clip::TestClip::of(clip).effects([BuiltinEffect::FilmGrain { amount: 0.3, size: 1.5, color: 0, shadows: 0.6, seed: 0 }]).build();
            clip.set_grade(common::exposure_graph(0.0));
            clip
        })
        .collect();
    let title = Clip {
        kind: ClipKind::Title(TitleData::single_text(TextBox { text: "Klipp".into(), font_size: 300.0, ..Default::default() })),
        ..Clip::gap(tc(0.0), tc(at))
    };
    let mut title = klipp_timeline::test_clip::TestClip::of(title).effects([reflection()]).build();
    title.set_grade(common::exposure_graph(0.0));
    // The Colour page's node graph shows a clip's nodes as thumbnails.
    // The Composite page's graph dock shows a clip's picture graph, its
    // source, effects and grade as nodes, each with its preview.
    let thumbs_of = |c: &Clip| {
        let picture = c.picture_v2().expect("a picture graph");
        let root = klipp_graph_format::path::Root::Clip { clip: c.id.0 };
        (c.id, picture.picture.graph.nodes.iter().map(|n| klipp_graph_format::path::NodeOutput::out(klipp_graph_format::path::NodePath::new(root, [n.id]))).collect::<Vec<_>>())
    };
    let title_thumbs = thumbs_of(&title);
    let mut text = Track::new("T1", TrackKind::Text);
    text.clips = vec![title];
    let ids: Vec<ClipId> = reversed.iter().map(|c| c.id).collect();
    let clip_thumbs: Vec<_> = reversed.iter().map(thumbs_of).collect();
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = reversed;
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![text, v1];

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    // The scopes and the node graph open, as the person had them.
    cmd.send(AppCommand::SetScopesVisible(true)).unwrap();
    // Parked on each clip, then regraded where it stands, as a paused
    // grade edit does; the engine draws the next park after each.
    // At each playback quality, played a moment and paused, as the
    // person works, so what playing decoded is what the regrade has.
    for quality in [PlaybackQuality::Half, PlaybackQuality::Quarter, PlaybackQuality::Full] {
        cmd.send(AppCommand::SetPlaybackQuality(quality)).unwrap();
        for secs in [0.5, 4.0, 13.0] {
            let g = common::generation(&fb);
            cmd.send(AppCommand::SetPlayhead(tc(secs))).unwrap();
            assert!(common::wait_new(&fb, g, Duration::from_secs(60)).is_some(), "no frame was drawn parked at {secs} s");
            common::settle(&fb);
            // The node graph on the clip under the playhead, then on the
            // title, each regrading the parked frame with thumbnails.
            let under = clip_thumbs[if secs < 3.0 { 0 } else if secs < 11.6 { 1 } else { 2 }].clone();
            for (n, (clip_id, at)) in [under, title_thumbs.clone()].into_iter().enumerate() {
                let g = common::generation(&fb);
                cmd.send(AppCommand::SetVisibleNodeThumbs { request: n as u64 + 1, clip_id, at }).unwrap();
                let _ = common::wait_new(&fb, g, Duration::from_secs(20));
                common::settle(&fb);
            }
            cmd.send(AppCommand::Play).unwrap();
            std::thread::sleep(Duration::from_millis(700));
            cmd.send(AppCommand::Pause).unwrap();
            common::settle(&fb);
            let g = common::generation(&fb);
            cmd.send(AppCommand::SetGlobalLutOpacity(0.5)).unwrap();
            cmd.send(AppCommand::SetGlobalLutOpacity(1.0)).unwrap();
            let _ = common::wait_new(&fb, g, Duration::from_secs(10));
            common::settle(&fb);
            // And as a grade knob's drag regrades it: drawn at half size.
            for id in &ids {
                cmd.send(AppCommand::BeginNodeParamEdit { clip_id: *id }).unwrap();
                let g = common::generation(&fb);
                cmd.send(AppCommand::SetGlobalLutOpacity(0.7)).unwrap();
                let _ = common::wait_new(&fb, g, Duration::from_secs(10));
                cmd.send(AppCommand::EndNodeParamEdit { clip_id: *id }).unwrap();
            }
            common::settle(&fb);
            let g = common::generation(&fb);
            cmd.send(AppCommand::SetPlayhead(tc(secs + 0.5))).unwrap();
            assert!(common::wait_new(&fb, g, Duration::from_secs(60)).is_some(), "the engine draws no more after a regrade at {secs} s ({quality:?})");
        }
    }
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(tc(2.0))).unwrap();
    assert!(common::wait_new(&fb, g, Duration::from_secs(60)).is_some(), "the engine draws no more after the regrades");
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
}
