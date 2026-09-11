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
}

impl<'a> ExportFrameSource<'a> {
    pub(crate) fn new(media: &'a HashMap<MediaId, MediaFile>, fps: f64, working: WorkingColorspace) -> Self {
        Self { media, fps, working, streams: HashMap::new(), used: HashSet::new() }
    }

    /// Close the decoders of clips the frame just rendered did not use.
    pub(crate) fn end_frame(&mut self) {
        let used = std::mem::take(&mut self.used);
        self.streams.retain(|id, _| used.contains(id));
    }

    /// Hardware decode in use, for the render queue's info line.
    pub(crate) fn hw_accel_name(&self) -> Option<&'static str> {
        self.streams.values().next().map(|s| s.decoder.hw_accel_name())
    }

    fn open(&self, clip: &Clip, file: &MediaFile, secs: f64, width: u32, height: u32) -> Option<ClipStream> {
        let decoder = RenderDecoder::open(&file.path, secs.max(0.0), f64::INFINITY, width, height, self.fps, clip.source_rotation)
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
        let stream = self.streams.get_mut(&clip.id)?;
        stream.seek_forward(source_secs, half_frame);
        let cur = stream.current.as_ref()?;
        let pixels = match &cur.scene {
            Some(s) if s.width == width && s.height == height => SourcePixels::SceneReferred {
                codes: s.codes.clone(),
                transfer: s.transfer,
                source_to_working: s.source_to_working,
            },
            _ => {
                let mut linear = rgba64_to_linear(&cur.rgba);
                if let Some(m) = stream.to_working {
                    for px in linear.chunks_exact_mut(4) {
                        let (r, g, b) = (px[0], px[1], px[2]);
                        px[0] = m[0][0] * r + m[0][1] * g + m[0][2] * b;
                        px[1] = m[1][0] * r + m[1][1] * g + m[1][2] * b;
                        px[2] = m[2][0] * r + m[2][1] * g + m[2][2] * b;
                    }
                }
                SourcePixels::Linear(linear)
            }
        };
        Some(SourceFrame { pixels, width, height })
    }

    fn motion_frame(&mut self, clip: &Clip, source_secs: f64, max_width: u32) -> Option<(Vec<u8>, u32, u32)> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        let file = self.media.get(media_id)?;
        crate::clip_decoder::decode_frame_at_size(&file.path, source_secs.max(0.0), max_width)
    }
}

/// sRGB-encoded RGBA64 → linear RGBA f32, through a 65536-entry table:
/// the source's full 10/12-bit precision reaches the grade.
fn rgba64_to_linear(rgba: &[u16]) -> Vec<f32> {
    use std::sync::OnceLock;
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    let lut = LUT.get_or_init(|| {
        (0..=u16::MAX).map(|v| klipp_color::transfer::srgb_to_linear(v as f32 / 65535.0)).collect()
    });
    rgba.chunks_exact(4)
        .flat_map(|px| [lut[px[0] as usize], lut[px[1] as usize], lut[px[2] as usize], px[3] as f32 / 65535.0])
        .collect()
}
