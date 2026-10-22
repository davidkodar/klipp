//! A title's image layer on the GPU (#2077): its picture fitted to its
//! box once, by the CPU's own sampling, and placed as a sprite. The
//! viewer's GPU stack and the CPU renderer an export runs draw the same
//! picture, at rest, faded, moving and with a file changed under it,
//! and nothing of it is rastered per frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::title_motion::{Motion, MotionKind};
use klipp_core::{ClipId, Interpolation, Keyframe, RationalTime, Timecode};
use klipp_effects::residency::{self, Work};
use klipp_timeline::*;

const W: u32 = 320;
const H: u32 = 180;

/// How far the viewer may be from the export, in display levels: both
/// lay the same byte pixels over the picture, the GPU in half floats.
const TOLERANCE: i32 = 1;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn clip(kind: ClipKind, from: f64, to: f64) -> Clip {
    Clip { kind, ..Clip::gap(tc(from), tc(to)) }
}

#[derive(Default)]
struct Flat(HashMap<ClipId, Arc<Vec<f32>>>);

impl FrameSource for Flat {
    fn frame(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        self.0.get(&clip.id).map(|p| SourceFrame { pixels: SourcePixels::Linear(Arc::clone(p)), width, height, pts: None })
    }
}

fn picture() -> Arc<Vec<f32>> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[fx * 0.8, fy * 0.6, (fx + fy) * 0.35, 1.0]);
        }
    }
    Arc::new(v)
}

fn display(linear: &[f32]) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(linear, klipp_color::display::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::SdrTransfer::Srgb)
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp-title-image-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A picture with a soft round edge: opaque in the middle, fading to
/// nothing, its clear pixels a loud green that must not bleed in.
fn soft_png(path: &Path, tint: [u8; 3]) {
    let (w, h) = (64u32, 48u32);
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let (dx, dy) = ((x as f32 + 0.5) / w as f32 - 0.5, (y as f32 + 0.5) / h as f32 - 0.5);
        let r = (dx * dx + dy * dy).sqrt() * 2.0;
        let a = ((1.0 - r) * 2.5).clamp(0.0, 1.0);
        if a == 0.0 {
            return image::Rgba([0, 255, 0, 0]);
        }
        image::Rgba([tint[0], (x * 4) as u8, (tint[2] as u32 * y / h) as u8, (a * 255.0).round() as u8])
    });
    img.save(path).unwrap();
}

/// An opaque photograph-like JPEG.
fn jpeg(path: &Path) {
    let img = image::RgbImage::from_fn(80, 50, |x, y| image::Rgb([(x * 3) as u8, (y * 5) as u8, ((x + y) * 2) as u8]));
    img.save(path).unwrap();
}

fn image_title(path: &Path, fit: ImageFit, opacity: f32) -> TitleData {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None };
    let mut xf = default_text_transform();
    xf.center = TitlePosition { x: 0.45, y: 0.5 };
    xf.size = TitlePosition { x: 0.5, y: 0.6 };
    let id = td.add_object(TitleObjectContent::Image(ImageLayer { path: path.to_string_lossy().into_owned(), fit }), xf);
    td.object_mut(id).unwrap().opacity = opacity;
    td
}

fn sequence(td: TitleData) -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    text.clips = vec![clip(ClipKind::Title(td), 0.0, 4.0)];
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 4.0);
    let mut src = Flat::default();
    src.0.insert(b.id, picture());
    base.clips = vec![b];
    seq.tracks = vec![text, base];
    (seq, src)
}

fn renderer(gpu: bool) -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(gpu);
    r
}

/// The viewer's frame at `t` off the GPU stack, the work it recorded, and
/// the CPU's for the same frame. `None` when the machine has no GPU.
fn both(gpu: &mut Renderer, cpu: &mut Renderer, seq: &Sequence, src: &mut Flat, t: f64) -> Option<(Vec<u8>, Vec<residency::Event>, Vec<u8>)> {
    let (viewer, _) = gpu.render_display_u8_on_gpu(seq, tc(t), W, H, src, RenderOptions::default())?;
    let events = residency::last_frame().events;
    let export = display(&cpu.render(seq, tc(t), W, H, src, RenderOptions::default()));
    Some((viewer, events, export))
}

fn no_gpu() -> Option<()> {
    assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
    eprintln!("skipping: the GPU stack declined (no usable GPU?)");
    None
}

fn no_fallback(events: &[residency::Event], what: &str) {
    assert!(!events.iter().any(|e| e.work == Work::Fallback), "{what}: {events:?}");
}

#[test]
fn an_image_layer_is_the_same_on_the_gpu_as_in_the_export() {
    let dir = folder("parity");
    let (png, jpg) = (dir.join("soft.png"), dir.join("photo.jpg"));
    soft_png(&png, [250, 0, 200]);
    jpeg(&jpg);
    let mut gpu = renderer(true);
    let mut cpu = renderer(false);
    for path in [&png, &jpg] {
        for fit in [ImageFit::Contain, ImageFit::Cover, ImageFit::Stretch] {
            for opacity in [1.0, 0.6] {
                let (seq, mut src) = sequence(image_title(path, fit, opacity));
                let what = format!("{} {fit:?} at {opacity}", path.display());
                let Some((viewer, events, export)) = both(&mut gpu, &mut cpu, &seq, &mut src, 1.0) else { return no_gpu().unwrap_or(()) };
                no_fallback(&events, &what);
                let d = worst(&viewer, &export);
                assert!(d <= TOLERANCE, "{what}: the viewer differs from the export by {d} levels");
                // And the picture is there, not the background alone.
                let (bare, mut src) = sequence(image_title(&dir.join("missing.png"), fit, opacity));
                let bare = display(&cpu.render(&bare, tc(1.0), W, H, &mut src, RenderOptions::default()));
                assert!(worst(&export, &bare) > 40, "{what}: the image shows");
                // Kept: the same frame again fits nothing anew.
                let _ = both(&mut gpu, &mut cpu, &seq, &mut src, 1.0).unwrap();
                let again = residency::last_frame().events;
                assert!(!again.iter().any(|e| e.feature == "title.image"), "{what}: kept, {again:?}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_moving_image_layer_is_placed_not_rastered() {
    let dir = folder("moving");
    let png = dir.join("soft.png");
    soft_png(&png, [250, 0, 200]);
    let mut gpu = renderer(true);
    let mut cpu = renderer(false);
    // Rising in: the resting picture posed on the GPU.
    let mut rising = image_title(&png, ImageFit::Contain, 1.0);
    rising.objects[0].fx_mut().motion = TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.8));
    // Keyed across the frame and grown: fitted again each frame, on the
    // CPU, and placed.
    let mut keyed = image_title(&png, ImageFit::Cover, 1.0);
    let key = |time_secs: f64, value: f32| Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    keyed.objects[0].transform.center_x_keyframes = vec![key(0.0, 0.3), key(2.0, 0.7)];
    keyed.objects[0].transform.size_x_keyframes = vec![key(0.0, 0.3), key(2.0, 0.6)];
    for (name, td) in [("rising", rising), ("keyed", keyed)] {
        let (seq, mut src) = sequence(td);
        for t in [0.2, 0.5, 1.3, 3.0] {
            let Some((viewer, events, export)) = both(&mut gpu, &mut cpu, &seq, &mut src, t) else { return no_gpu().unwrap_or(()) };
            no_fallback(&events, &format!("{name} at {t}"));
            let d = worst(&viewer, &export);
            assert!(d <= TOLERANCE + 1, "{name} at {t} s: the viewer differs from the export by {d} levels");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_changed_file_is_read_again() {
    let dir = folder("changed");
    let png = dir.join("soft.png");
    soft_png(&png, [250, 0, 200]);
    let mut gpu = renderer(true);
    let mut cpu = renderer(false);
    let (seq, mut src) = sequence(image_title(&png, ImageFit::Contain, 1.0));
    let Some((before, _, _)) = both(&mut gpu, &mut cpu, &seq, &mut src, 1.0) else { return no_gpu().unwrap_or(()) };
    // Another picture written over it, a moment later so its time moves.
    std::thread::sleep(std::time::Duration::from_millis(20));
    soft_png(&png, [10, 0, 60]);
    let (after, events, export) = both(&mut gpu, &mut cpu, &seq, &mut src, 1.0).unwrap();
    assert!(events.iter().any(|e| e.feature == "title.image"), "fitted anew: {events:?}");
    assert!(worst(&before, &after) > 40, "the new picture shows");
    let d = worst(&after, &export);
    assert!(d <= TOLERANCE, "the viewer differs from the export by {d} levels");
    let _ = std::fs::remove_dir_all(dir);
}

/// The export's own GPU route, when it takes a title at all, carries the
/// viewer's picture: its luma against the CPU frame's.
#[test]
fn the_export_route_draws_the_viewer_picture() {
    let dir = folder("export");
    let png = dir.join("soft.png");
    soft_png(&png, [250, 0, 200]);
    let (seq, mut src) = sequence(image_title(&png, ImageFit::Contain, 0.8));
    let target = klipp_effects::gpu::Yuv420Target {
        tone_map_op: 0,
        odt_tag: 0,
        scene: None,
        gamut_op: 0,
        hdr_op: 0,
        peak_nits: 0.0,
        luma_coefs: [0.2126, 0.7152, 0.0722],
        semi_planar: false,
        bits: 8,
        sample_shift: 0,
    };
    let mut gpu = renderer(true);
    let Some((planes, _)) = gpu.render_yuv420_on_gpu(&seq, tc(1.0), W, H, &mut src, RenderOptions::default(), target) else {
        // Declined: the export renders through the CPU frame the viewer
        // test above checks against.
        let _ = std::fs::remove_dir_all(dir);
        return;
    };
    no_fallback(&residency::last_frame().events, "export");
    let px = renderer(false).render(&seq, tc(1.0), W, H, &mut src, RenderOptions::default());
    let display = klipp_color::display::ExportDisplay::new(klipp_core::ToneMapOp::None, klipp_core::WorkingColorspace::Rec709Linear, klipp_core::OdtChoice::MatchViewer);
    let luma: Vec<u8> = px
        .chunks_exact(4)
        .map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            let y = 0.2126 * e[0] + 0.7152 * e[1] + 0.0722 * e[2];
            (16.0 + y * 219.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect();
    let d = worst(&planes[..(W * H) as usize], &luma);
    assert!(d <= 2, "the export's luma differs from the CPU frame's by {d} levels");
    let _ = std::fs::remove_dir_all(dir);
}
