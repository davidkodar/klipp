//! #3212: a whole frame drawn ahead into the frame cache, read back and
//! put on screen, is the frame drawn live, within one display level: for
//! layers blended over each other, a transition and a title. And an edit
//! asks for another frame.

use std::sync::{Arc, Mutex};

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode, TransitionAlignment, TransitionId, TransitionKind, WorkingColorspace};
use klipp_engine::frame_cache::{FrameOutcome, FrameRequest, Keying, NAMESPACE};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind, Transition};

const W: u32 = 320;
const H: u32 = 180;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn media(from: f64, to: f64) -> Clip {
    Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(from), tc(to)) }
}

/// Each clip its own ramp, so layers never look alike.
struct Pictures;

impl FrameSource for Pictures {
    fn frame(&mut self, c: &Clip, secs: f64, w: u32, h: u32) -> Option<SourceFrame> {
        let seed = (c.id.0.as_u128() % 97) as f32 / 97.0;
        let px: Vec<f32> = (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [(x as f32 / w as f32 * 0.8 + seed * 0.2).min(1.0), (y as f32 / h as f32 * 0.7 + (secs as f32 * 0.1).fract() * 0.3).min(1.0), 0.2 + seed * 0.6, 1.0]))
            .collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width: w, height: h, pts: None })
    }
}

fn sequence(tracks: Vec<Track>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    seq.fps = klipp_core::Fps::new(24, 1);
    seq.tracks = tracks;
    seq
}

fn track(clips: Vec<Clip>) -> Track {
    let mut t = Track::new("V", TrackKind::Video);
    t.clips = clips;
    t
}

fn cases() -> Vec<(&'static str, Sequence, f64)> {
    let mut top = track(vec![media(0.0, 2.0)]);
    top.opacity = 0.6;
    let blended = sequence(vec![top, track(vec![media(0.0, 2.0)])]);
    let (a, b) = (media(0.0, 2.0), media(2.0, 4.0));
    let mut cut = track(vec![a.clone(), b.clone()]);
    cut.transitions.push(Transition { id: TransitionId::new(), kind: TransitionKind::CrossDissolve, duration: tc(1.0), clip_a_id: a.id, clip_b_id: b.id, user_params: Default::default(), alignment: TransitionAlignment::Center });
    let dissolve = sequence(vec![cut]);
    let td = TitleData::single_text(TextBox { text: "Kept".into(), font_size: 40.0, ..Default::default() });
    let title = Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(2.0)) };
    let titled = sequence(vec![track(vec![title]), track(vec![media(0.0, 2.0)])]);
    vec![("two layers blended", blended, 0.5), ("a dissolve", dissolve, 2.0), ("a title over a clip", titled, 0.5)]
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.set_working_colorspace(WorkingColorspace::Rec709Linear);
    r
}

#[test]
fn a_frame_kept_ahead_shows_as_the_live_frame() {
    if renderer().executor().gpu_pipeline_or_init().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let root = std::env::temp_dir().join(format!("klipp-kept-frames-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Mutex::new(klipp_engine::cache_store::CacheStore::open(&root, 1 << 30));
    for (name, seq, secs) in cases() {
        let at = tc(secs);
        let req = FrameRequest { seq: Arc::new(seq.clone()), frames: vec![at], sizes: vec![(W, H)], working: WorkingColorspace::Rec709Linear, keying: Keying { media: Arc::default(), environment: 7 } };
        assert_eq!(req.draw(&mut renderer(), &store, at, (W, H), &mut Pictures), FrameOutcome::Stored, "{name}");
        assert_eq!(req.draw(&mut renderer(), &store, at, (W, H), &mut Pictures), FrameOutcome::Current, "{name}: kept already");
        let place = req.keying.place(&seq, at, (W, H));
        let kept = klipp_engine::cache_store::read_stored(&root, NAMESPACE, place).expect("kept");
        let shown = klipp_color::display::display_transform_ws_dithered_to(&kept, klipp_color::display::ToneMapOp::None, WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb);
        let (live, _) = renderer().render_display_u8_on_gpu(&seq, at, W, H, &mut Pictures, RenderOptions::default()).expect("drawn live");
        let worst = shown.iter().zip(&live).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap_or(0);
        eprintln!("MEASURE {name}: kept vs live, worst {worst} levels");
        assert!(worst <= 1, "{name}: {worst} levels");
        // An edit: another frame asked for, the kept one not found.
        let mut edited = seq.clone();
        edited.tracks[0].opacity = 0.3;
        assert_ne!(req.keying.place(&edited, at, (W, H)).key, place.key, "{name}: an edit asks for another frame");
    }
    let _ = std::fs::remove_dir_all(&root);
}

mod common;

/// The engine tests one at a time: frames shown from the cache are
/// counted for the whole process.
static ONE_ENGINE: Mutex<()> = Mutex::new(());

/// A 4:4:4 FFV1 source, so no chroma is upsampled on the way: playback's
/// decoder and the export's, which the frame cache draws with, upsample
/// 4:2:0 chroma apart at hard colour edges.
fn source_444() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-kept-444-{}.mkv", std::process::id()));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=24:d=2"), "-c:v", "ffv1", "-pix_fmt", "yuv444p"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

/// The engine shows a range rendered ahead from the frame cache: parked
/// on a frame of it, the frame comes from the cache, as the frame shown
/// live there within two levels.
#[test]
fn the_engine_shows_a_rendered_range_from_the_cache() {
    let _one = ONE_ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(src) = source_444() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let p = common::Parked::start(src, W, H);
    let root = std::env::temp_dir().join(format!("klipp-kept-range-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    p.send(klipp_core::AppCommand::SetFreezeFolder(Some(root.clone())));
    let at = |f: i64| common::frame_tc(f, p.fps);
    let park = |f: i64| {
        let g = p.generation();
        p.send(klipp_core::AppCommand::SeekTo(at(f)));
        p.settle_after(g, std::time::Duration::from_secs(10));
    };
    park(6);
    park(5);
    let live = p.frame();
    p.send(klipp_core::AppCommand::RenderRange { from: at(0), to: at(12) });
    let store = klipp_engine::cache_store::shared_at(&root, 1 << 30);
    let started = std::time::Instant::now();
    while store.lock().unwrap().frames(NAMESPACE).len() < 12 {
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "the range was rendered: {} frames", store.lock().unwrap().frames(NAMESPACE).len());
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let before = klipp_engine::frames_shown_kept();
    park(6);
    park(5);
    let shown = klipp_engine::frames_shown_kept() - before;
    let kept = p.frame();
    let worst = live.iter().zip(&kept).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap_or(0);
    eprintln!("MEASURE parked from the cache: {shown} frames shown kept, worst {worst} levels from live");
    assert!(shown >= 1, "parked on a rendered frame, it came from the cache");
    assert_eq!(live.len(), kept.len());
    // Two levels: the live frame is display-encoded and dithered on the
    // GPU, a kept one by the CPU's encoder; the frame itself keeps within
    // one level (`a_frame_kept_ahead_shows_as_the_live_frame`).
    assert!(worst <= 2, "{worst} levels from live");
    let _ = std::fs::remove_dir_all(&root);
}

/// A frame kept ahead gives way to every view that isn't the program
/// frame: with the range rendered and each view on, a parked frame is
/// drawn live, not taken from the cache. With each view off again the
/// kept frame is shown, so the cache was there to take.
#[test]
fn a_kept_frame_gives_way_to_every_other_view() {
    let _one = ONE_ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    use klipp_core::AppCommand as C;
    let Some(src) = source_444() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let p = common::Parked::start(src, W, H);
    let root = std::env::temp_dir().join(format!("klipp-kept-views-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    p.send(C::SetFreezeFolder(Some(root.clone())));
    let at = |f: i64| common::frame_tc(f, p.fps);
    let park = |f: i64| {
        let g = p.generation();
        p.send(C::SeekTo(at(f)));
        p.settle_after(g, std::time::Duration::from_secs(10));
    };
    park(6);
    p.send(C::RenderRange { from: at(0), to: at(12) });
    let store = klipp_engine::cache_store::shared_at(&root, 1 << 30);
    let started = std::time::Instant::now();
    while store.lock().unwrap().frames(NAMESPACE).len() < 12 {
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "the range was rendered");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let (clip, node) = (p.clip_id, p.exposure_node);
    let node_at = || klipp_compose::inspect::grade_node(clip, node);
    let views: Vec<(&str, Vec<C>, Vec<C>)> = vec![
        ("a grade dragged", vec![C::BeginNodeParamEdit { clip_id: clip }], vec![C::EndNodeParamEdit { clip_id: clip }]),
        ("a node soloed", vec![C::SetNodeSolo { at: Some(node_at()) }], vec![C::SetNodeSolo { at: None }]),
        ("the alpha channel", vec![C::SetViewChannel { channel: klipp_core::ViewChannel::Alpha }], vec![C::SetViewChannel { channel: klipp_core::ViewChannel::Picture }]),
        ("the matte", vec![C::SetMattePreview(true)], vec![C::SetMattePreview(false)]),
        ("the HDR picture", vec![C::SetHdrMonitor(klipp_core::HdrMonitor::Preview)], vec![C::SetHdrMonitor(klipp_core::HdrMonitor::Off)]),
        ("before the grade", vec![C::ToggleBeforeAfter], vec![C::ToggleBeforeAfter]),
        ("the stabilisation split", vec![C::ToggleStabilizationCompare], vec![C::ToggleStabilizationCompare]),
        ("a node's split", vec![C::SetSplitCompareNode { clip_id: Some(clip), path: Some(node_at().path) }], vec![C::SetSplitCompareNode { clip_id: None, path: None }]),
        ("one clip measured", vec![C::SetMeasureOnlyClip(Some(clip))], vec![C::SetMeasureOnlyClip(None)]),
        ("the inspector's matte tile", vec![C::SetInspectorMattePreviewTarget(Some(clip))], vec![C::SetInspectorMattePreviewTarget(None)]),
        ("node thumbnails", vec![C::SetVisibleNodeThumbs { request: 1, clip_id: clip, at: vec![node_at()] }], vec![C::SetVisibleNodeThumbs { request: 2, clip_id: clip, at: vec![] }]),
    ];
    let mut frame = 5;
    for (name, on, off) in views {
        for c in on {
            p.send(c);
        }
        let before = klipp_engine::frames_shown_kept();
        park(frame);
        frame = 11 - frame;
        let shown = klipp_engine::frames_shown_kept() - before;
        for c in off {
            p.send(c);
        }
        // Off again, after what turning it off redraws: the kept frame.
        let before = klipp_engine::frames_shown_kept();
        park(frame);
        frame = 11 - frame;
        let back = klipp_engine::frames_shown_kept() - before;
        eprintln!("MEASURE {name}: {shown} frames from the cache while on, {back} once off");
        assert_eq!(shown, 0, "{name}: drawn live, not the kept frame");
        assert!(back >= 1, "{name} off: the kept frame is shown again");
    }
    // A node's input sampled: the frame the sample asks for is drawn live.
    let before = klipp_engine::frames_shown_kept();
    let g = p.generation();
    p.send(C::SampleNodeInput { clip_id: clip, node_id: node, at: node_at().path, nx: 0.5, ny: 0.5 });
    p.settle_after(g, std::time::Duration::from_secs(10));
    assert_eq!(klipp_engine::frames_shown_kept() - before, 0, "a node's input sampled: drawn live");
    let _ = std::fs::remove_dir_all(&root);
}
