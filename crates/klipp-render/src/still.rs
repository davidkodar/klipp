//! One clip's frame as the export draws it, small: a graded thumbnail
//! (#899). The clip alone on a sequence the thumbnail's size, its frame at
//! a time read by export's decoders (a camera log or HDR file decoded as
//! it plays) and drawn by the shared renderer with its grade and effects,
//! then encoded for display as the export encodes it.

use std::collections::HashMap;

use klipp_core::{RationalTime, Resolution, WorkingColorspace};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

use crate::{ExportFrameSource, MediaFile};

/// A still: sRGB RGBA8, `width` by `height`.
#[derive(Debug, Clone, PartialEq)]
pub struct Still {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// `clip`'s frame at `at_secs` of its own time (0 is its in point), drawn
/// `width` wide at its media's aspect, in `working` space, with its grade
/// and its effects: what the viewer shows of that clip alone there. An
/// error for a clip that isn't a media clip, media the pool doesn't hold,
/// or a frame the file wouldn't give.
pub fn clip_still(clip: &Clip, pool: &klipp_media::MediaPool, at_secs: f64, width: u32, working: WorkingColorspace) -> Result<Still, String> {
    let ClipKind::Media { media_id } = &clip.kind else { return Err("not a media clip".into()) };
    let item = pool.get(media_id).ok_or("its media isn't in the pool")?;
    let (mw, mh) = (item.resolution.width.max(1), item.resolution.height.max(1));
    let (mw, mh) = if item.source_rotation.rem_euclid(180) == 90 { (mh, mw) } else { (mw, mh) };
    let width = width.max(2) & !1;
    let height = ((width as f64 * mh as f64 / mw as f64).round() as u32).max(2) & !1;

    // The clip alone, from the sequence's start.
    let mut one = clip.clone();
    let rate = one.timeline_in.rate;
    let duration = one.duration();
    one.timeline_in = RationalTime { value: 0, rate };
    one.timeline_out = RationalTime { value: duration.value, rate };
    let mut seq = Sequence::new_default();
    seq.resolution = Resolution { width, height };
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![one];
    seq.tracks = vec![track];

    let media: HashMap<_, _> = HashMap::from([(*media_id, MediaFile::of(item))]);
    let mut src = ExportFrameSource::new(&media, seq.fps.as_f64(), working);
    let mut renderer = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    renderer.set_gpu_enabled(false);
    renderer.set_working_colorspace(working);
    let at = at_secs.clamp(0.0, (duration.to_secs_f64() - 1.0 / seq.fps.as_f64()).max(0.0));
    let linear = renderer.render(&seq, RationalTime::from_secs_f64(at, rate), width, height, &mut src, klipp_compose::RenderOptions::default());
    src.end_frame().map_err(|e| e.to_string())?;

    let display = klipp_color::display::ExportDisplay::new(klipp_core::ToneMapOp::None, working, klipp_core::OdtChoice::MatchViewer);
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let rgba = linear
        .chunks_exact(4)
        .flat_map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            [byte(e[0]), byte(e[1]), byte(e[2]), 255]
        })
        .collect();
    Ok(Still { rgba, width, height })
}
