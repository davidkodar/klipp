//! A title of several layers composes its scene on the GPU one layer at a
//! time: each placed, dressed, faded and laid over the others. Once its
//! entrance is over and before its exit begins nothing in it changes, so
//! that composed scene is kept and each frame at rest is the kept one:
//! no layer is drawn again, and the frame is the one a cold renderer
//! draws. While anything moves (the entrance, the exit, a loop) every
//! frame is composed afresh.

#[path = "support/cpu_reference.rs"]
#[allow(dead_code)]
mod cpu_reference;
use cpu_reference::no_gpu;
use std::collections::HashMap;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{title_library, Clip, ClipKind, Sequence, TitleData, TitleLoop, TitleObjectContent, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;
const LENGTH: f64 = 5.0;

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

/// `td` over a picture, from 0 s to `LENGTH`.
fn over_a_picture(td: TitleData) -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    text.clips = vec![clip(ClipKind::Title(td), 0.0, LENGTH)];
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let mut src = Flat::default();
    src.0.insert(b.id, picture());
    base.clips = vec![b];
    seq.tracks = vec![text, base];
    (seq, src)
}

fn renderer() -> Renderer {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r
}

fn template(name: &str) -> TitleData {
    let t = title_library::TITLES.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no template {name}"));
    title_library::for_frame(t.build(), W, H)
}

fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// The frame at `at` from `r`, and how many scenes it composed for it.
/// `None` when the stack declined.
fn frame(r: &mut Renderer, seq: &Sequence, src: &mut Flat, at: f64) -> Option<(Vec<u8>, u64)> {
    let before = r.title_scene_composes();
    let bytes = r.render_display_u8_on_gpu(seq, tc(at), W, H, src, RenderOptions::default())?.0;
    Some((bytes, r.title_scene_composes() - before))
}

/// Plays `td` through on one renderer, frame by frame at the times given,
/// and draws each of those frames cold too. Returns, for each time, how
/// many scenes the playing renderer composed. Each frame it played is the
/// cold one within one level in 255.
fn played(name: &str, td: &TitleData, times: &[f64]) -> Option<Vec<u64>> {
    let (seq, mut src) = over_a_picture(td.clone());
    let mut playing = renderer();
    let mut composes = Vec::new();
    for &at in times {
        let Some((bytes, composed)) = frame(&mut playing, &seq, &mut src, at) else {
            no_gpu(&format!("{name}: the GPU stack declined (no usable GPU?)"));
            return None;
        };
        let (cold, cold_composed) = frame(&mut renderer(), &seq, &mut src, at).expect("the stack drew it played");
        assert_eq!(cold_composed, 1, "{name} at {at} s: a cold renderer composes the scene once");
        let off = worst(&bytes, &cold);
        assert!(off <= 1, "{name} at {at} s: the played frame is {off} levels from the cold one");
        composes.push(composed);
    }
    Some(composes)
}

/// The templates of several layers that rest still: the scene is composed
/// for the entrance, once more on reaching rest, then kept until the exit.
#[test]
fn a_still_title_at_rest_composes_its_scene_once() {
    for name in ["Statement", "Clean Line"] {
        let td = template(name);
        assert!(td.objects.len() > 1, "{name} has several layers");
        // Entrance, rest (four frames), exit.
        let times = [0.05, 2.0, 2.5, 3.0, 3.5, LENGTH - 0.05];
        let Some(composes) = played(name, &td, &times) else { return };
        assert_eq!(composes[0], 1, "{name}: the entrance is composed");
        assert_eq!(composes[1], 1, "{name}: the first frame at rest is composed");
        assert_eq!(&composes[2..5], &[0, 0, 0], "{name}: later frames at rest are the kept scene");
        assert_eq!(composes[5], 1, "{name}: the exit is composed");
    }
}

/// A bar that pulses changes at every frame: nothing is kept, however
/// still its words are.
#[test]
fn a_pulsing_bar_is_composed_at_every_frame() {
    let mut td = template("Statement");
    let bar = td.objects.iter_mut().find(|o| matches!(o.content, TitleObjectContent::Shape(_))).expect("Statement has a bar");
    bar.fx_mut().loops.push(TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.3 });
    let Some(composes) = played("pulsing Statement", &td, &[2.0, 2.25, 2.5, 2.75]) else { return };
    assert_eq!(composes, [1, 1, 1, 1], "each frame of a pulsing bar is composed");
}

/// A title that is changed while at rest is not its kept scene: the
/// new words are drawn.
#[test]
fn a_changed_title_is_composed_again() {
    let td = template("Statement");
    let mut changed = td.clone();
    let text = changed.objects.iter_mut().find_map(|o| match &mut o.content {
        TitleObjectContent::TextBox(tb) => Some(tb),
        _ => None,
    });
    text.expect("Statement has words").text = "Something else entirely".into();
    let mut playing = renderer();
    let (seq, mut src) = over_a_picture(td);
    let Some(_) = frame(&mut playing, &seq, &mut src, 2.0) else {
        no_gpu("the GPU stack declined (no usable GPU?)");
        return;
    };
    let (seq2, mut src2) = over_a_picture(changed);
    let (bytes, composed) = frame(&mut playing, &seq2, &mut src2, 2.5).expect("drawn");
    assert_eq!(composed, 1, "the changed title is composed");
    let (cold, _) = frame(&mut renderer(), &seq2, &mut src2, 2.5).expect("drawn");
    assert!(worst(&bytes, &cold) <= 1, "and shows its new words");
}
