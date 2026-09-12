//! Export's [`klipp_compose::FrameSource`]: decoded frames for the shared
//! renderer, from sequential decoders.
//!
//! Export renders frames in timeline order, so each clip's source time
//! only moves forward: one decoder per clip, opened at the first time the
//! renderer asks for — which may lie before the clip's in point when a
//! transition runs into its handle — and advanced frame by frame. A clip's
//! decoder is closed once a frame renders without it.

use std::sync::Arc;
use std::collections::{HashMap, HashSet};

use klipp_compose::{FrameSource, SourceFrame, SourcePixels};
use klipp_core::{ClipId, MediaId, WorkingColorspace};
use klipp_timeline::{Clip, ClipKind};
use tracing::warn;

use crate::clip_decoder::{DecodedPixels, RenderDecoder, SceneReferredFrame};
use crate::snapshot::MediaFile;

struct Decoded {
    pts: f64,
    /// The frame as the decoder produced it — RGBA64 on the CPU, or
    /// linear f32 already on the GPU. The CPU one is shared rather than
    /// copied: the renderer seeds the chain from those samples, and at
    /// 4K the buffer is 66 MB.
    pixels: DecodedFrame,
    scene: Option<SceneReferredFrame>,
}

/// [`DecodedPixels`] with the CPU case made shareable.
enum DecodedFrame {
    Rgba64(Arc<Vec<u16>>),
    GpuLinear(Arc<wgpu::Buffer>),
}

impl From<DecodedPixels> for DecodedFrame {
    fn from(p: DecodedPixels) -> Self {
        match p {
            DecodedPixels::Rgba64(v) => DecodedFrame::Rgba64(Arc::new(v)),
            DecodedPixels::GpuLinear(b) => DecodedFrame::GpuLinear(b),
        }
    }
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
    fn pull(&mut self, nv12: &mut Option<klipp_effects::nv12_gpu::GpuNv12Converter>) -> Option<Decoded> {
        if self.ended {
            return None;
        }
        match self.decoder.next_frame_with_pts(nv12) {
            Ok(Some((pts, pixels))) => Some(Decoded {
                pts,
                pixels: pixels.into(),
                scene: self.decoder.take_scene_referred(),
            }),
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
    fn seek_forward(&mut self, secs: f64, half_frame: f64, nv12: &mut Option<klipp_effects::nv12_gpu::GpuNv12Converter>) {
        if self.current.is_none() {
            self.current = self.pull(nv12);
        }
        loop {
            if self.next.is_none() {
                self.next = self.pull(nv12);
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
    /// NV12 → linear working-space f32 on the GPU: one converter for the
    /// whole export, shared by every clip's decoder.
    ///
    /// One *per decoder* is what this was first, and it is the wrong
    /// shape twice over: a five-clip timeline built five compute
    /// pipelines with five sets of plane buffers to do the same job, and
    /// on the shared device that many at once exhausts the descriptor
    /// pool — which is a segfault, not an error. Built on
    /// `GpuContext::shared` because the buffers it produces have to be
    /// bindable by the grade chain.
    nv12: Option<klipp_effects::nv12_gpu::GpuNv12Converter>,
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
            nv12: klipp_effects::gpu_context::GpuContext::shared()
                .and_then(klipp_effects::nv12_gpu::GpuNv12Converter::with_context),
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
        // Split the borrow: the stream and the shared converter are
        // different fields, and the decode needs both at once.
        let Self { streams, nv12, .. } = self;
        let stream = streams.get_mut(&clip.id)?;
        stream.seek_forward(source_secs, half_frame, nv12);
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
        let Self { streams, nv12, .. } = self;
        let stream = streams.get_mut(&clip.id)?;
        if stream.next.is_none() {
            stream.next = stream.pull(nv12);
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

/// An export frame left as the scaler produced it, so the grade can
/// start on the GPU.
///
/// The renderer seeds its chain from these samples and a shader expands
/// them to linear light — the same route the viewer takes from a
/// hardware-decoded frame. What this replaces is expanding 8.3 million
/// pixels into a 133 MB f32 buffer on the CPU and uploading *that*:
/// twice the bus traffic to say the same thing, plus the pass that
/// builds it.
///
/// The samples stay 16 bits wide. An 8-bit source does not yield an
/// 8-bit RGBA64 buffer — YUV→RGB is a matrix, and measured on real
/// footage only 27% of samples come back byte-replicated — so narrowing
/// here to halve the upload again would quantise the delivered frame.
///
/// `read` is the way back for anything that needs the pixels on the CPU
/// — a node with no GPU dispatch, an effect the chain cannot carry.
struct SeedableFrame {
    rgba: Arc<Vec<u16>>,
    to_working: Option<[[f32; 3]; 3]>,
    linear: std::sync::OnceLock<Arc<Vec<f32>>>,
}

impl klipp_compose::GpuPixels for SeedableFrame {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        exec.prime_chain_from_rgba64(&self.rgba, width, height, seed_matrix(self.to_working))
    }

    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(
            self.linear
                .get_or_init(|| Arc::new(rgba64_to_linear(&self.rgba, self.to_working))),
        )
    }
}

/// The shader wants three rows of `vec4`; no basis change is the
/// identity, not a skipped multiply.
fn seed_matrix(to_working: Option<[[f32; 3]; 3]>) -> [[f32; 4]; 3] {
    let m = to_working.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    [
        [m[0][0], m[0][1], m[0][2], 0.0],
        [m[1][0], m[1][1], m[1][2], 0.0],
        [m[2][0], m[2][1], m[2][2], 0.0],
    ]
}

fn source_frame(
    d: &Decoded,
    to_working: Option<[[f32; 3]; 3]>,
    width: u32,
    height: u32,
) -> SourceFrame {
    let pixels = match (&d.scene, &d.pixels) {
        (Some(s), _) if s.width == width && s.height == height => SourcePixels::SceneReferred {
            codes: std::sync::Arc::clone(&s.codes),
            transfer: s.transfer,
            source_to_working: s.source_to_working,
        },
        // Already linear, already on the GPU, already in the working
        // space — the decoder's shader did the colour. Nothing to do but
        // hand it to the chain.
        (_, DecodedFrame::GpuLinear(buf)) => SourcePixels::Gpu(Arc::new(GpuLinearFrame {
            buf: Arc::clone(buf),
            width,
            height,
            linear: std::sync::OnceLock::new(),
        })),
        // Samples for the GPU to expand. The shader folds in the basis
        // change, so this is not gated on the working space; a frame the
        // renderer cannot seed from falls back to `read`, which is the
        // CPU expansion below.
        (_, DecodedFrame::Rgba64(rgba))
            if rgba.len() == (width as usize) * (height as usize) * 4 =>
        {
            SourcePixels::Gpu(Arc::new(SeedableFrame {
                rgba: Arc::clone(rgba),
                to_working,
                linear: std::sync::OnceLock::new(),
            }))
        }
        (_, DecodedFrame::Rgba64(rgba)) => {
            SourcePixels::Linear(Arc::new(rgba64_to_linear(rgba, to_working)))
        }
    };
    SourceFrame { pixels, width, height, pts: Some(d.pts) }
}

/// A frame the decoder converted on the GPU, waiting to become a grade
/// chain's input slot.
///
/// This is the end of the road the export has been walking all session:
/// NVDEC decodes on the GPU, the NV12 shader converts there, the grade
/// runs there. Nothing crosses the bus between the decoder's 12 MB of
/// planes going up and the finished frame coming down.
///
/// `read` exists for the cases that still need pixels on the CPU — an
/// effect with no GPU dispatch, a node the chain cannot carry — and is
/// memoised, because a graph with two such nodes would otherwise pull
/// the same 133 MB down twice.
struct GpuLinearFrame {
    buf: Arc<wgpu::Buffer>,
    width: u32,
    height: u32,
    linear: std::sync::OnceLock<Arc<Vec<f32>>>,
}

impl klipp_compose::GpuPixels for GpuLinearFrame {
    fn seed(&self, exec: &mut klipp_nodes::GraphExecutor, width: u32, height: u32) -> bool {
        (self.width, self.height) == (width, height)
            && exec.prime_chain_from_gpu_linear_buffer(&self.buf, width, height)
    }

    fn read(&self) -> Arc<Vec<f32>> {
        Arc::clone(
            self.linear
                .get_or_init(|| Arc::new(download_linear(&self.buf, self.width, self.height))),
        )
    }
}

/// Bring a linear f32 frame down from the shared GPU context.
///
/// Zeroes if there is no context, which cannot happen for a buffer that
/// came *from* it, but the renderer gets a correctly-sized frame rather
/// than a panic either way.
fn download_linear(buf: &wgpu::Buffer, width: u32, height: u32) -> Vec<f32> {
    let samples = (width as usize) * (height as usize) * 4;
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        warn!("export: a GPU frame needs reading back and the shared context is gone");
        return vec![0.0; samples];
    };
    let bytes = (samples * 4) as u64;
    let staging = ctx.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("export_gpu_frame_readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(buf, 0, &staging, 0, bytes.min(buf.size()));
    ctx.queue().submit(std::iter::once(enc.finish()));
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    ctx.device().poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let out = bytemuck::pod_collect_to_vec(&data);
    drop(data);
    staging.unmap();
    out
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
