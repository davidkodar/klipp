//! Export's [`klipp_compose::FrameSource`]: decoded frames for the shared
//! renderer, from sequential decoders.
//!
//! Export renders frames in timeline order, so each clip's source time
//! only moves forward: one decoder per clip, opened at the first time the
//! renderer asks for — which may lie before the clip's in point when a
//! transition runs into its handle — and advanced frame by frame. A clip's
//! decoder is closed once a frame renders without it.

use std::collections::{HashMap, HashSet};

use klipp_compose::{FrameSource, SourceFrame, SourcePixels};
use klipp_core::{ClipId, MediaId, WorkingColorspace};
use klipp_timeline::{Clip, ClipKind};
use tracing::warn;

use crate::clip_decoder::{RenderDecoder, SceneReferredFrame};
use crate::snapshot::MediaFile;

struct Decoded {
    pts: f64,
    /// sRGB-encoded RGBA64 (empty for scene-referred frames).
    rgba: Vec<u16>,
    scene: Option<SceneReferredFrame>,
}

struct ClipStream {
    decoder: RenderDecoder,
    /// Source primaries → working space for display-referred frames, when
    /// not the identity.
    to_working: Option<[[f32; 3]; 3]>,
    current: Option<Decoded>,
    next: Option<Decoded>,
    ended: bool,
}

impl ClipStream {
    fn pull(&mut self) -> Option<Decoded> {
        if self.ended {
            return None;
        }
        match self.decoder.next_frame_with_pts() {
            Ok(Some((pts, rgba))) => Some(Decoded { pts, rgba, scene: self.decoder.take_scene_referred() }),
            Ok(None) => {
                self.ended = true;
                None
            }
            Err(e) => {
                warn!("export decode failed: {e}");
                self.ended = true;
                None
            }
        }
    }

    /// Advance to the last frame at or before `secs` (half a frame of
    /// tolerance). Before the first frame, the first frame; past the end,
    /// the last — a transition holding an edge frame.
    fn seek_forward(&mut self, secs: f64, half_frame: f64) {
        if self.current.is_none() {
            self.current = self.pull();
        }
        loop {
            if self.next.is_none() {
                self.next = self.pull();
            }
            match &self.next {
                Some(n) if n.pts <= secs + half_frame => self.current = self.next.take(),
                _ => break,
            }
        }
    }
}

pub(crate) struct ExportFrameSource<'a> {
    media: &'a HashMap<MediaId, MediaFile>,
    fps: f64,
    working: WorkingColorspace,
    streams: HashMap<ClipId, ClipStream>,
    used: HashSet<ClipId>,
    /// Time spent pulling frames out of the decoders, and time spent
    /// turning what they hand back into the renderer's linear frames.
    /// Reported by the export's per-frame breakdown, because "render is
    /// slow" is not actionable and "the source conversion is slow" is.
    pub(crate) t_decode: std::time::Duration,
    pub(crate) t_convert: std::time::Duration,
}

impl<'a> ExportFrameSource<'a> {
    pub(crate) fn new(media: &'a HashMap<MediaId, MediaFile>, fps: f64, working: WorkingColorspace) -> Self {
        Self {
            media,
            fps,
            working,
            streams: HashMap::new(),
            used: HashSet::new(),
            t_decode: std::time::Duration::ZERO,
            t_convert: std::time::Duration::ZERO,
        }
    }

    /// Close the decoders of clips the frame just rendered did not use.
    /// The decoders' own breakdown: download plus colour conversion, and
    /// rotation, summed over every stream this export has opened.
    pub(crate) fn decoder_stage_times(&self) -> (std::time::Duration, std::time::Duration) {
        self.streams.values().fold(
            (std::time::Duration::ZERO, std::time::Duration::ZERO),
            |(c, r), s| {
                let (sc, sr) = s.decoder.stage_times();
                (c + sc, r + sr)
            },
        )
    }

    pub(crate) fn end_frame(&mut self) {
        let used = std::mem::take(&mut self.used);
        self.streams.retain(|id, _| used.contains(id));
    }

    /// Hardware decode in use, for the render queue's info line.
    pub(crate) fn hw_accel_name(&self) -> Option<&'static str> {
        self.streams.values().next().map(|s| s.decoder.hw_accel_name())
    }

    fn open(&self, clip: &Clip, file: &MediaFile, secs: f64, width: u32, height: u32) -> Option<ClipStream> {
        // The clip's rotation when it has one, else the media's. A clip
        // created by a path that did not copy the media's rotation would
        // otherwise render sideways while playing upright, since the
        // viewer reads the media pool and this read only the clip.
        // (A user who deliberately sets a rotated file to 0 gets the
        // media's value back — distinguishing "unset" from "set to zero"
        // needs a flag on the clip, as `log_format_user_set` does for
        // the analogous problem with log formats.)
        let rotation = if clip.source_rotation != 0 { clip.source_rotation } else { file.source_rotation };
        let decoder = RenderDecoder::open(&file.path, secs.max(0.0), f64::INFINITY, width, height, self.fps, rotation)
            .map_err(|e| warn!("export: cannot open {:?}: {e}", file.path))
            .ok()?
            .with_log_format(file.log_format)
            .with_working_colorspace(self.working);
        let m = klipp_color::camera_log::primaries_to_working_matrix(decoder.source_primaries(), self.working);
        let identity = m == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        Some(ClipStream { decoder, to_working: (!identity).then_some(m), current: None, next: None, ended: false })
    }
}

impl FrameSource for ExportFrameSource<'_> {
    fn frame(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        let file = self.media.get(media_id)?;
        self.used.insert(clip.id);
        if !self.streams.contains_key(&clip.id) {
            let s = self.open(clip, file, source_secs, width, height)?;
            self.streams.insert(clip.id, s);
        }
        let half_frame = 0.5 / self.fps.max(1.0);
        let t0 = std::time::Instant::now();
        let stream = self.streams.get_mut(&clip.id)?;
        stream.seek_forward(source_secs, half_frame);
        let to_working = stream.to_working;
        let decoded = stream.current.as_ref();
        self.t_decode += t0.elapsed();
        let t1 = std::time::Instant::now();
        let out = decoded.map(|d| source_frame(d, to_working, width, height));
        self.t_convert += t1.elapsed();
        out
    }

    /// The decoder's next frame, already pulled by `frame`'s seek.
    fn frame_after(&mut self, clip: &Clip, _source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let stream = self.streams.get_mut(&clip.id)?;
        if stream.next.is_none() {
            stream.next = stream.pull();
        }
        let to_working = stream.to_working;
        stream.next.as_ref().map(|d| source_frame(d, to_working, width, height))
    }

    fn motion_frame(&mut self, clip: &Clip, source_secs: f64, max_width: u32) -> Option<(Vec<u8>, u32, u32)> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        let file = self.media.get(media_id)?;
        crate::clip_decoder::decode_frame_at_size(&file.path, source_secs.max(0.0), max_width)
    }
}

fn source_frame(d: &Decoded, to_working: Option<[[f32; 3]; 3]>, width: u32, height: u32) -> SourceFrame {
    let pixels = match &d.scene {
        Some(s) if s.width == width && s.height == height => SourcePixels::SceneReferred {
            codes: std::sync::Arc::clone(&s.codes),
            transfer: s.transfer,
            source_to_working: s.source_to_working,
        },
        _ => SourcePixels::Linear(std::sync::Arc::new(rgba64_to_linear(&d.rgba, to_working))),
    };
    SourceFrame { pixels, width, height, pts: Some(d.pts) }
}

/// sRGB-encoded RGBA64 → linear RGBA f32, through a 65536-entry table:
/// the source's full 10/12-bit precision reaches the grade.
/// 16-bit RGBA to the renderer's linear f32, and into the working basis
/// in the same pass when the source needs one.
///
/// One rayon pass over a pre-sized buffer. It used to be a
/// single-threaded `flat_map().collect()`, with the basis change as a
/// second single-threaded pass after it: 33 million table lookups and a
/// 133 MB allocation on one core, at 4K vertical, per frame. That was
/// 34 ms of the export's 241 ms per frame — on a machine with 32 of
/// them idle.
fn rgba64_to_linear(rgba: &[u16], to_working: Option<[[f32; 3]; 3]>) -> Vec<f32> {
    use rayon::prelude::*;
    use std::sync::OnceLock;
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    let lut = LUT.get_or_init(|| {
        (0..=u16::MAX).map(|v| klipp_color::transfer::srgb_to_linear(v as f32 / 65535.0)).collect()
    });
    let mut out = vec![0.0f32; rgba.len()];
    out.par_chunks_exact_mut(4)
        .zip(rgba.par_chunks_exact(4))
        .for_each(|(dst, px)| {
            let (r, g, b) = (lut[px[0] as usize], lut[px[1] as usize], lut[px[2] as usize]);
            match to_working {
                Some(m) => {
                    dst[0] = m[0][0] * r + m[0][1] * g + m[0][2] * b;
                    dst[1] = m[1][0] * r + m[1][1] * g + m[1][2] * b;
                    dst[2] = m[2][0] * r + m[2][1] * g + m[2][2] * b;
                }
                None => {
                    dst[0] = r;
                    dst[1] = g;
                    dst[2] = b;
                }
            }
            dst[3] = px[3] as f32 / 65535.0;
        });
    out
}
