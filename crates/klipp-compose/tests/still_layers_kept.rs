//! A still image's picture is read and sent up to the GPU stack once, and
//! seeded from the GPU on later frames (#2092), while its file and the
//! sizes stand: five full-frame 4K RGBA cards no longer upload 130 MB
//! each, every frame. As in the viewer, each frame's decode is a new
//! picture; the file is what says it is the same.
use klipp_compose::{FrameSource, HeldFile, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Resolution};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use std::collections::HashMap;
use std::sync::Arc;

/// Stills by clip: each its picture and its file's stamp, decoded afresh
/// (a new `Arc`) every frame it is read, as the viewer's streams do; the
/// reads counted.
struct Stills {
    pictures: HashMap<klipp_core::ClipId, (Arc<Vec<f32>>, u32, u32, u64)>,
    still: bool,
    reads: usize,
}
impl FrameSource for Stills {
    fn frame(&mut self, clip: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        let (p, w, h, _) = self.pictures.get(&clip.id)?;
        self.reads += 1;
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(p.to_vec())), width: *w, height: *h, pts: None })
    }
    fn still_file(&mut self, clip: &Clip) -> Option<HeldFile> {
        let (.., len) = self.pictures.get(&clip.id)?;
        self.still.then(|| HeldFile {
            media: clip.media_id().expect("a media clip"),
            path: format!("/cards/{}.png", clip.id.0).into(),
            len: *len,
            modified: std::time::UNIX_EPOCH,
            log_format: None,
            proxy: None,
        })
    }
}

/// A card: a soft colour ramp, see-through in a band, different for each.
fn card(k: usize, w: u32, h: u32) -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let a = if ((fy * 5.0) as usize) % 5 == k { 0.9 } else if (fx * 5.0) as usize == k { 0.5 } else { 0.0 };
            v.extend_from_slice(&[0.1 + 0.15 * k as f32, fx * 0.8, fy * 0.6, a]);
        }
    }
    Arc::new(v)
}

fn scene(w: u32, h: u32, still: bool) -> (Sequence, Stills) {
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width: w, height: h };
    let tc = |s| RationalTime::from_secs_f64(s, seq.fps.rate());
    let mut pictures = HashMap::new();
    let mut tracks = Vec::new();
    for k in 0..5 {
        let c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, source_in: tc(0.0), source_out: tc(10.0), ..Clip::gap(tc(0.0), tc(10.0)) };
        pictures.insert(c.id, (card(k, w, h), w, h, 1000));
        let mut t = Track::new(&format!("V{}", 5 - k), TrackKind::Video);
        t.clips = vec![c];
        tracks.push(t);
    }
    seq.tracks = tracks;
    (seq, Stills { pictures, still, reads: 0 })
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r
}

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_dithered_to(linear, klipp_core::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap()
}

/// Five stacked 4K RGBA stills: the first frame uploads each, the second
/// puts each back from the GPU and uploads nothing, and both match the
/// CPU's composite within a level. A picture decoded again (a changed
/// file) is uploaded again; the others stay kept.
#[test]
fn five_stacked_stills_are_uploaded_once() {
    let (w, h) = (3840, 2160);
    let (seq, mut src) = scene(w, h, true);
    let mut r = renderer();
    let at = |s: f64| RationalTime::from_secs_f64(s, seq.fps.rate());
    let Some((first, stats)) = r.render_display_u8_on_gpu(&seq, at(1.0), w, h, &mut src, RenderOptions::default()) else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required: {:?}", r.last_decline());
        eprintln!("unverified: no GPU stack");
        return;
    };
    assert_eq!((stats.layers_uploaded, stats.stills_kept, src.reads), (5, 0, 5), "the first frame reads and uploads each");
    let (second, stats) = r.render_display_u8_on_gpu(&seq, at(1.5), w, h, &mut src, RenderOptions::default()).unwrap();
    assert_eq!((stats.layers_uploaded, stats.stills_kept, src.reads), (0, 5, 5), "the second reads and uploads nothing");
    assert_eq!(first, second, "and draws the same frame");
    let cpu = display(&r.render(&seq, at(1.5), w, h, &mut src, RenderOptions::default()));
    let d = worst(&cpu, &second);
    assert!(d <= 1, "the stack differs from the CPU's composite by {d} levels");

    // One file changed (its stamp): read and sent up again, the other four
    // stay.
    let changed = seq.tracks[2].clips[0].id;
    src.pictures.insert(changed, (card(4, w, h), w, h, 2000));
    let (third, stats) = r.render_display_u8_on_gpu(&seq, at(2.0), w, h, &mut src, RenderOptions::default()).unwrap();
    assert_eq!((stats.layers_uploaded, stats.stills_kept), (1, 4), "a new decode is uploaded again");
    assert_ne!(third, second, "and drawn");
    let cpu = display(&r.render(&seq, at(2.0), w, h, &mut src, RenderOptions::default()));
    assert!(worst(&cpu, &third) <= 1);

    // The control: a source that isn't a still uploads every frame.
    let (seq, mut moving) = scene(w, h, false);
    let mut r = renderer();
    for s in [1.0, 1.5] {
        let (_, stats) = r.render_display_u8_on_gpu(&seq, at(s), w, h, &mut moving, RenderOptions::default()).unwrap();
        assert_eq!((stats.layers_uploaded, stats.stills_kept), (5, 0), "not a still: uploaded at {s}");
    }
}

/// A render size changed (the viewer resized) is another layer.
#[test]
fn a_still_is_uploaded_again_at_another_size() {
    let (seq, mut src) = scene(320, 180, true);
    let mut r = renderer();
    let at = RationalTime::from_secs_f64(1.0, seq.fps.rate());
    let Some((_, stats)) = r.render_display_u8_on_gpu(&seq, at, 320, 180, &mut src, RenderOptions::default()) else { return };
    assert_eq!(stats.layers_uploaded, 5);
    let (_, stats) = r.render_display_u8_on_gpu(&seq, at, 160, 90, &mut src, RenderOptions::default()).unwrap();
    assert_eq!((stats.layers_uploaded, stats.stills_kept), (5, 0), "fitted at the new size, uploaded again");
    let (_, stats) = r.render_display_u8_on_gpu(&seq, at, 160, 90, &mut src, RenderOptions::default()).unwrap();
    assert_eq!((stats.layers_uploaded, stats.stills_kept), (0, 5));
}

/// The five-card case at 4K, a frame each way: run with `--ignored
/// --nocapture`.
#[test]
#[ignore = "4K performance measurement"]
fn measure_4k_five_stills() {
    let (w, h) = (3840, 2160);
    for still in [false, true] {
        let (seq, mut src) = scene(w, h, still);
        let mut r = renderer();
        let mut samples = Vec::new();
        for n in 0..23 {
            let at = RationalTime::from_secs_f64(1.0 + n as f64 / 24.0, seq.fps.rate());
            let t = std::time::Instant::now();
            r.render_display_u8_on_gpu(&seq, at, w, h, &mut src, RenderOptions::default()).expect("GPU stack");
            if n >= 3 { samples.push(t.elapsed().as_secs_f64() * 1000.0); }
        }
        samples.sort_by(f64::total_cmp);
        eprintln!("MEASURE 4K five stills, kept {still}: median {:.1} ms a frame (display readback included)", samples[10]);
    }
}

/// Cards placed by their own transform (moved, scaled: their effects run
/// as a plan): kept too, seeded from the GPU into their plan, and drawn
/// as the CPU draws them.
#[test]
fn placed_stills_are_kept_too() {
    use klipp_timeline::test_clip::TestClip;
    let (w, h) = (320, 180);
    let (mut seq, mut src) = scene(w, h, true);
    for (k, t) in seq.tracks.iter_mut().enumerate() {
        t.clips[0] = TestClip::of(t.clips[0].clone()).lane("scale", 0.0, 0.6).lane("pos_x", 0.0, -40.0 + 20.0 * k as f32).lane("pos_x", 10.0, 40.0).build();
    }
    let mut r = renderer();
    let at = |s: f64| RationalTime::from_secs_f64(s, seq.fps.rate());
    let Some((_, stats)) = r.render_display_u8_on_gpu(&seq, at(1.0), w, h, &mut src, RenderOptions::default()) else { return };
    assert_eq!((stats.layers_uploaded, src.reads), (5, 5));
    let (gpu, stats) = r.render_display_u8_on_gpu(&seq, at(2.0), w, h, &mut src, RenderOptions::default()).unwrap();
    assert_eq!((stats.layers_uploaded, stats.stills_kept, src.reads), (0, 5, 5), "moving cards: kept, not read");
    let cpu = display(&r.render(&seq, at(2.0), w, h, &mut src, RenderOptions::default()));
    assert!(worst(&cpu, &gpu) <= 1, "{} levels", worst(&cpu, &gpu));
}
