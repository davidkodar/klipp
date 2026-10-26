//! A secondary changes what it keys (#2767): the colour ops' qualify on
//! skin and on foliage, and the highlight zone, each measured against the
//! same frame ungraded, on the export's CPU path and on the GPU stack the
//! viewer draws with. A missing GPU is reported as unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{RationalTime, Timecode};
use klipp_document::{apply::DocumentView, ops::{self, Op}};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use serde_json::json;

const W: u32 = 64;
const H: u32 = 16;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 25)
}

/// Four patches across the frame, linear light: a face's skin, leaves, a
/// sunlit sky, a shadow.
const PATCHES: [[f32; 3]; 4] = [[0.55, 0.30, 0.20], [0.10, 0.16, 0.03], [0.70, 0.78, 0.92], [0.02, 0.02, 0.025]];

struct Patches;

impl FrameSource for Patches {
    fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let px: Vec<f32> = (0..width * height).flat_map(|i| {
            let p = PATCHES[((i % width) * 4 / width) as usize];
            [p[0], p[1], p[2], 1.0]
        }).collect();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(px)), width, height, pts: None })
    }
}

/// A sequence of one clip with `op` applied by the document, as the op
/// would apply it in the app.
fn graded(op: serde_json::Value) -> Sequence {
    let mut pool = MediaPool::new();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/patches.mov".into(), None);
    item.duration = tc(4.0);
    item.video_codec = "h264".into();
    let media = pool.add(item);
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(25, 1);
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0))];
    let id = v.clips[0].id;
    seq.tracks = vec![v];
    let mut op = op;
    op["clip_id"] = json!(id.0.to_string());
    let ops: Vec<Op> = serde_json::from_value(json!([op])).unwrap();
    let plan = ops::resolve(&seq, &pool, &ops).unwrap_or_else(|r| panic!("refused: {}", r.why));
    let mut fps = seq.fps;
    let mut sequences = vec![seq];
    let mut doc = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
    for step in &plan.steps {
        doc.apply_forward(&step.edit);
    }
    sequences.remove(0)
}

/// Each patch's mean colour, on the CPU path and, when there is one, the
/// GPU stack.
fn patches(seq: &Sequence) -> (Vec<[f32; 3]>, Option<Vec<[f32; 3]>>) {
    let mean = |img: &[f32]| -> Vec<[f32; 3]> {
        (0..4).map(|k| {
            let (x0, x1) = (k * W / 4 + 2, (k + 1) * W / 4 - 2);
            let mut s = [0.0; 3];
            let mut n = 0.0;
            for y in 0..H {
                for x in x0..x1 {
                    let i = ((y * W + x) * 4) as usize;
                    for c in 0..3 { s[c] += img[i + c]; }
                    n += 1.0;
                }
            }
            s.map(|v| v / n)
        }).collect()
    };
    let mut cpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    cpu.set_gpu_enabled(false);
    let on_cpu = mean(&cpu.render(seq, tc(1.0), W, H, &mut Patches, RenderOptions::default()));
    let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    gpu.set_gpu_enabled(true);
    let on_gpu = gpu.render_linear_on_gpu(seq, tc(1.0), W, H, &mut Patches, RenderOptions::default()).map(|px| mean(&px));
    (on_cpu, on_gpu)
}

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// The patches after `op`, and as they were, on the CPU and the GPU
/// stack, which agree.
fn changed(op: serde_json::Value) -> Option<(Vec<[f32; 3]>, Vec<[f32; 3]>)> {
    let (base, base_gpu) = patches(&graded(json!({"op":"grade","exposure":0.0})));
    let (after, after_gpu) = patches(&graded(op.clone()));
    let (Some(base_gpu), Some(after_gpu)) = (base_gpu, after_gpu) else {
        assert!(!required(), "required, but the GPU stack declined");
        eprintln!("UNVERIFIED: no GPU stack");
        return None;
    };
    for (cpu, gpu) in [(&base, &base_gpu), (&after, &after_gpu)] {
        for (a, b) in cpu.iter().zip(gpu) {
            assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-3), "{op}: the viewer's GPU stack and the export's CPU path differ: {a:?} {b:?}");
        }
    }
    Some((base, after))
}

/// #2767: foliage as a camera sees it, olive to yellow-green, is keyed by
/// `foliage`, and a hue turn moves it; the face, sky and shadow stay.
#[test]
fn a_foliage_secondary_turns_the_leaves() {
    let Some((base, after)) = changed(json!({"op":"qualify","target":"foliage","hue_shift":40})) else { return };
    let moved = |k: usize| (0..3).map(|c| (base[k][c] - after[k][c]).abs()).sum::<f32>();
    assert!(moved(1) > 0.03, "the leaves turn: {:?} -> {:?}", base[1], after[1]);
    assert!(moved(0) < 1e-3 && moved(2) < 1e-3 && moved(3) < 1e-3, "nothing else does: {base:?} -> {after:?}");
}

/// #2767: on an SDR picture the highlight zone is its bright part: a
/// sunlit sky is taken down by most of what was asked (−0.7 stops is
/// about 38% darker), its shadows left alone.
#[test]
fn the_highlight_zone_takes_down_an_sdr_sky() {
    let Some((base, after)) = changed(json!({"op":"zones","zone":"highlight","exposure":-0.7})) else { return };
    let luma = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let drop = 1.0 - luma(after[2]) / luma(base[2]);
    assert!(drop > 0.25, "the sky comes down by {:.0}%, too little to see", drop * 100.0);
    assert!((luma(after[3]) - luma(base[3])).abs() < 1e-4, "the shadow stays: {:?} -> {:?}", base[3], after[3]);
}

/// The skin key itself works (a warmer skin is #2769's): more exposure
/// on skin is a brighter face and nothing else.
#[test]
fn a_skin_secondary_brightens_the_face() {
    let Some((base, after)) = changed(json!({"op":"qualify","target":"skin","exposure":1.0})) else { return };
    assert!(after[0][0] > base[0][0] * 1.5, "the face brightens: {:?} -> {:?}", base[0], after[0]);
    assert!((after[2][1] - base[2][1]).abs() < 1e-3, "the sky does not: {:?} -> {:?}", base[2], after[2]);
}

/// A PNG, decoded to linear light.
struct Still(Arc<Vec<f32>>);

impl FrameSource for Still {
    fn frame(&mut self, _: &Clip, _: f64, width: u32, height: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::clone(&self.0)), width, height, pts: None })
    }
}

/// What each secondary changes on real frames, reported, not asserted:
/// `KLIPP_SECONDARY_FRAMES=<a.png>:<b.png>` (frames of real footage),
/// run with `--nocapture`. Each line is a frame, an op, how many pixels
/// it changed and by how much on average.
#[test]
fn real_frames_report() {
    let Ok(list) = std::env::var("KLIPP_SECONDARY_FRAMES") else { return };
    for path in list.split(':') {
        let img = image::open(path).expect("frame").to_rgba8();
        let (w, h) = img.dimensions();
        let lin = |v: u8| { let x = f32::from(v) / 255.0; if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) } };
        let px = Arc::new(img.pixels().flat_map(|p| [lin(p[0]), lin(p[1]), lin(p[2]), 1.0]).collect::<Vec<f32>>());
        let draw = |op: serde_json::Value| {
            let mut seq = graded(op);
            seq.resolution = klipp_core::Resolution { width: w, height: h };
            let mut gpu = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            gpu.set_gpu_enabled(true);
            gpu.render_linear_on_gpu(&seq, tc(1.0), w, h, &mut Still(Arc::clone(&px)), RenderOptions::default()).expect("gpu")
        };
        let base = draw(json!({"op":"grade","exposure":0.0}));
        for op in [
            json!({"op":"qualify","target":"skin","exposure":0.5}),
            json!({"op":"qualify","target":"foliage","hue_shift":40}),
            json!({"op":"zones","zone":"highlight","exposure":-0.7}),
            json!({"op":"qualify","target":"sky","saturation":1.6}),
        ] {
            let out = draw(op.clone());
            let changed = base.chunks(4).zip(out.chunks(4)).filter(|(a, b)| (0..3).any(|c| (a[c] - b[c]).abs() > 0.01)).count();
            let mean = base.iter().zip(&out).map(|(a, b)| (a - b).abs()).sum::<f32>() / base.len() as f32;
            eprintln!("{path} {op}: {changed} px changed, mean |d| {mean:.4}");
        }
    }
}
