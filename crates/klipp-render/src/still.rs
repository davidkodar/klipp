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
    still_from_linear(&linear, width, height, working)
}

/// The timeline's frame at `secs`, `width` × `height`, read from the files
/// at their own size and drawn as the export draws it, on the GPU stack
/// where it takes the frame: a frame larger than the viewer's preview,
/// which is decoded at the playback size (#2491). An error for a frame a
/// file would not give or a node the render refused.
pub fn timeline_still(snapshot: &crate::SequenceSnapshot, secs: f64, width: u32, height: u32) -> Result<Still, String> {
    let timeline = &snapshot.video.sequence;
    let mut renderer = crate::export_frames::export_renderer(snapshot, "frame");
    renderer.set_gpu_enabled(true);
    let mut src = ExportFrameSource::new(&snapshot.video.media, snapshot.fps, snapshot.working_colorspace);
    let time = RationalTime::from_secs_f64(secs.max(0.0), snapshot.fps_num);
    let opts = klipp_compose::RenderOptions::default();
    let linear = match renderer.render_linear_on_gpu(timeline, time, width, height, &mut src, opts) {
        Some(linear) => linear,
        None => {
            let (linear, found) = renderer.render_inspected(timeline, time, width, height, &mut src, opts, Default::default());
            if let Some(why) = crate::export_frames::refused(&found.declines) {
                return Err(why);
            }
            linear
        }
    };
    src.end_frame().map_err(|e| e.to_string())?;
    still_from_linear(&linear, width, height, snapshot.working_colorspace)
}

/// The still of a rendered linear frame, `width × height`, encoded for
/// display; an error, not a still, when the frame is not that size
/// (#1913): a thumbnail of the wrong size crashed the app.
pub fn still_from_linear(linear: &[f32], width: u32, height: u32, working: WorkingColorspace) -> Result<Still, String> {
    let want = width as usize * height as usize * 4;
    if linear.len() != want {
        return Err(format!("the render gave {} values for a {width}x{height} picture, which takes {want}", linear.len()));
    }
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

/// A file format a still is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StillFormat {
    Png,
    Jpeg,
}

impl StillFormat {
    /// The format a path's extension names: .png, .jpg or .jpeg.
    pub fn of_path(path: &std::path::Path) -> Option<StillFormat> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => Some(StillFormat::Png),
            "jpg" | "jpeg" => Some(StillFormat::Jpeg),
            _ => None,
        }
    }
}

/// The JPEG qualities tried, best first, for a file under a size limit.
const JPEG_QUALITIES: [u8; 8] = [95, 90, 85, 80, 75, 70, 65, 60];

/// `still` encoded as `format`, its bytes and, for a JPEG, the quality it
/// took: the best of [`JPEG_QUALITIES`] under `max_bytes`. An error, saying
/// how large it came out, for a file over the limit at the lowest
/// quality, or a PNG over it.
pub fn encode(still: &Still, format: StillFormat, max_bytes: Option<u64>) -> Result<(Vec<u8>, Option<u8>), String> {
    let image = image::RgbImage::from_fn(still.width, still.height, |x, y| {
        let k = (y as usize * still.width as usize + x as usize) * 4;
        image::Rgb([still.rgba[k], still.rgba[k + 1], still.rgba[k + 2]])
    });
    let fits = |bytes: &[u8]| max_bytes.is_none_or(|m| bytes.len() as u64 <= m);
    let mb = |n: usize| n as f64 / 1_000_000.0;
    match format {
        StillFormat::Png => {
            let mut out = std::io::Cursor::new(Vec::new());
            image.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
            let out = out.into_inner();
            if !fits(&out) {
                return Err(format!("the PNG is {:.2} MB, over the {:.2} MB the size takes; write it as a JPEG", mb(out.len()), mb(max_bytes.unwrap_or(0) as usize)));
            }
            Ok((out, None))
        }
        StillFormat::Jpeg => {
            let mut smallest = usize::MAX;
            for q in JPEG_QUALITIES {
                let mut out = Vec::new();
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q).encode_image(&image).map_err(|e| e.to_string())?;
                if fits(&out) {
                    return Ok((out, Some(q)));
                }
                smallest = smallest.min(out.len());
            }
            Err(format!("the JPEG is {:.2} MB even at quality {}, over the {:.2} MB the size takes", mb(smallest), JPEG_QUALITIES[JPEG_QUALITIES.len() - 1], mb(max_bytes.unwrap_or(0) as usize)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A noisy picture, which compresses badly.
    fn noise(width: u32, height: u32) -> Still {
        let mut seed = 0x2545_f491_u32;
        let rgba = (0..width * height * 4).map(|k| if k % 4 == 3 { 255 } else { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; seed as u8 }).collect();
        Still { rgba, width, height }
    }

    /// #3311: a still is written as the format asks, PNG or JPEG; a JPEG
    /// over the size's limit steps its quality down until it fits, and one
    /// that never fits, or a PNG over it, is refused saying how large.
    #[test]
    fn a_cover_is_written_under_its_size_limit() {
        assert_eq!(StillFormat::of_path(std::path::Path::new("/a/b.JPG")), Some(StillFormat::Jpeg));
        assert_eq!(StillFormat::of_path(std::path::Path::new("/a/b.webp")), None);
        let still = noise(256, 144);
        let (png, q) = encode(&still, StillFormat::Png, None).unwrap();
        assert!(png.starts_with(b"\x89PNG") && q.is_none());
        let (best, q) = encode(&still, StillFormat::Jpeg, None).unwrap();
        assert!(best.starts_with(&[0xff, 0xd8]) && q == Some(95));
        let limit = best.len() as u64 * 3 / 4;
        let (fitted, q) = encode(&still, StillFormat::Jpeg, Some(limit)).unwrap();
        assert!(fitted.len() as u64 <= limit && q.is_some_and(|q| q < 95), "{} bytes at {q:?}", fitted.len());
        let err = encode(&still, StillFormat::Jpeg, Some(1_000)).unwrap_err();
        assert!(err.contains("even at quality 60"), "{err}");
        assert!(encode(&still, StillFormat::Png, Some(1_000)).unwrap_err().contains("write it as a JPEG"));
    }

    /// #1913: a render of the wrong size is an error, not a still whose
    /// bytes don't fill it.
    #[test]
    fn a_render_of_the_wrong_size_is_no_still() {
        assert!(still_from_linear(&[], 192, 108, WorkingColorspace::Rec709Linear).is_err());
        assert!(still_from_linear(&[0.5; 4 * 3], 2, 2, WorkingColorspace::Rec709Linear).is_err());
        let still = still_from_linear(&[0.5; 4 * 4], 2, 2, WorkingColorspace::Rec709Linear).expect("the right size");
        assert_eq!(still.rgba.len(), 16);
    }
}
