//! Every built-in title template drawn on the GPU through its entrance,
//! at rest and through its exit, over a picture, one digest of the
//! display bytes per frame: run before and after a change to the title
//! passes that should leave every picture as it was, and compare the two
//! lists line by line. A tool, ignored in the suite.
//!
//! `cargo test --release -p klipp-compose --test title_gpu_digests -- --ignored --nocapture 2>&1 | grep '^digest'`

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ClipId, RationalTime, Timecode};
use klipp_timeline::{title_library, Clip, ClipKind, Sequence, TitleData, Track, TrackKind};

const W: u32 = 1280;
const H: u32 = 720;
const LENGTH: f64 = 5.0;
/// Entrance, rest, exit.
const TIMES: [f64; 7] = [0.1, 0.35, 0.7, 1.2, 2.5, 4.5, 4.85];

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 30)
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

/// `td` over a ramp with a light and a dark band, from 0 s to `LENGTH`.
fn over_a_picture(td: TitleData) -> (Sequence, Flat) {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut text = Track::new("Text", TrackKind::Text);
    text.clips = vec![clip(ClipKind::Title(td), 0.0, LENGTH)];
    let mut base = Track::new("V1", TrackKind::Video);
    let b = clip(ClipKind::Media { media_id: klipp_core::MediaId::new() }, 0.0, 10.0);
    let picture: Vec<f32> = (0..W * H).flat_map(|i| {
        let (fx, fy) = ((i % W) as f32 / W as f32, (i / W) as f32 / H as f32);
        let band = if (i / W) % 180 < 40 { 0.9 } else if (i / W) % 180 < 80 { 0.02 } else { 0.0 };
        [fx * 0.8 + band, fy * 0.6 + band, (fx + fy) * 0.35 + band, 1.0]
    }).collect();
    let mut src = Flat::default();
    src.0.insert(b.id, Arc::new(picture));
    base.clips = vec![b];
    seq.tracks = vec![text, base];
    (seq, src)
}

#[test]
#[ignore = "a tool: digests to compare between two builds"]
fn every_template_digest_on_the_gpu() {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    let mut frames = 0;
    for t in title_library::TITLES.iter() {
        let td = title_library::for_frame(t.build(), W, H);
        let (seq, mut src) = over_a_picture(td);
        for at in TIMES {
            let Some((bytes, _)) = r.render_display_u8_on_gpu(&seq, tc(at), W, H, &mut src, RenderOptions::default()) else {
                println!("digest {:24} {at:4.2} declined", t.name);
                continue;
            };
            let mut h = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut h);
            println!("digest {:24} {at:4.2} {:016x}", t.name, h.finish());
            frames += 1;
        }
    }
    eprintln!("{frames} frames");
}
