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
use klipp_media::frame_reader::fitted_size;

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
    /// The decoder failed: what it said. Reported once by
    /// [`ExportFrameSource::end_frame`], which fails the export.
    error: Option<String>,
    /// The file ended before the clip did and its last frame is being
    /// held; said once per clip.
    held_warned: bool,
    path: std::path::PathBuf,
    /// The size the decoder makes: the picture fitted into the frame,
    /// upright, with its own aspect. The renderer bars the rest.
    width: u32,
    height: u32,
}

impl ClipStream {
    fn pull(&mut self, gpu: &mut crate::clip_decoder::GpuConverters) -> Option<Decoded> {
        if self.ended {
            return None;
        }
        match self.decoder.next_frame_with_pts(gpu) {
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
                let at = self.current.as_ref().map_or(0.0, |c| c.pts);
                warn!("export decode failed: {:?} after {at:.3}s: {e}", self.path);
                self.ended = true;
                self.error = Some(format!("{} after {at:.3}s: {e}", self.path.display()));
                None
            }
        }
    }

    /// Advance to the last frame at or before `secs` (half a *source*
    /// frame of tolerance). Before the first frame, the first frame;
    /// past the end, the last — a transition holding an edge frame.
    fn seek_forward(&mut self, secs: f64, gpu: &mut crate::clip_decoder::GpuConverters) {
        let half_frame = 0.5 / self.decoder.source_fps().max(1.0);
        if self.current.is_none() {
            self.current = self.pull(gpu);
        }
        loop {
            if self.next.is_none() {
                self.next = self.pull(gpu);
            }
            match &self.next {
                Some(n) if n.pts <= secs + half_frame => self.current = self.next.take(),
                _ => break,
            }
        }
        // Holding the last frame past the file's end: say so once, so
        // a still where motion was expected can be traced to the file
        // being shorter than the timeline says.
        // A clip whose file gave no frame at all would export black and
        // say it succeeded (#978): it fails the export instead, naming it.
        if self.ended && self.error.is_none() && self.current.is_none() {
            self.error = Some(format!("{} gave no picture for the clip at {secs:.3}s: it could not be decoded there", self.path.display()));
        }
        // A still's one frame is its picture throughout, not a file
        // that ended early: nothing to say.
        if self.ended && self.error.is_none() && !self.held_warned && !self.decoder.is_still() {
            if let Some(c) = &self.current {
                if secs > c.pts + 2.0 * half_frame {
                    self.held_warned = true;
                    warn!(
                        "export: {:?} ends at {:.3}s but the timeline asks for {secs:.3}s; holding its last frame",
                        self.path, c.pts
                    );
                }
            }
        }
    }
}

pub struct ExportFrameSource<'a> {
    media: &'a HashMap<MediaId, MediaFile>,
    fps: f64,
    working: WorkingColorspace,
    streams: HashMap<ClipId, ClipStream>,
    /// A reversed clip's frames (#617), read backward from their own
    /// file: the clip's decoder reads forward only. With the rotation
    /// they are turned by.
    reversed: HashMap<ClipId, (klipp_media::frame_reader::BackwardFrameReader<Vec<f32>>, i32)>,
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
    gpu: crate::clip_decoder::GpuConverters,
    /// Time spent pulling frames out of the decoders, and time spent
    /// turning what they hand back into the renderer's linear frames.
    /// Reported by the export's per-frame breakdown, because "render is
    /// slow" is not actionable and "the source conversion is slow" is.
    pub(crate) t_decode: std::time::Duration,
    pub(crate) t_convert: std::time::Duration,
    /// Readers opened on media: a clip's stream, a neighbour reader. A
    /// sequential export opens one a clip; a frame blend reads its next
    /// frame from the stream, opening none (M4 step 7).
    opened: std::cell::Cell<usize>,
    /// Media sources' readers (#1746), kept across frames.
    media_readers: klipp_media::frame_reader::MediaReaders,
}

impl<'a> ExportFrameSource<'a> {
    pub fn new(media: &'a HashMap<MediaId, MediaFile>, fps: f64, working: WorkingColorspace) -> Self {
        Self {
            media,
            fps,
            working,
            streams: HashMap::new(),
            reversed: HashMap::new(),
            media_readers: klipp_media::frame_reader::MediaReaders::new(klipp_compose::MEDIA_READERS),
            used: HashSet::new(),
            gpu: crate::clip_decoder::GpuConverters::shared(),
            t_decode: std::time::Duration::ZERO,
            t_convert: std::time::Duration::ZERO,
            opened: std::cell::Cell::new(0),
        }
    }

    /// Readers this source has opened on media, all told.
    pub fn readers_opened(&self) -> usize {
        self.opened.get()
    }

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

    /// Close the decoders of clips the frame just rendered did not use.
    ///
    /// Fails if any decoder the frame used has failed: the export stops
    /// there instead of freezing that clip's last good frame to its end.
    pub fn end_frame(&mut self) -> Result<(), crate::error::RenderError> {
        let used = std::mem::take(&mut self.used);
        self.streams.retain(|id, _| used.contains(id));
        self.reversed.retain(|id, _| used.contains(id));
        if let Some(e) = self.streams.values_mut().find_map(|s| s.error.take()) {
            return Err(crate::error::RenderError::Source(e));
        }
        Ok(())
    }

    /// Hardware decode in use, for the render queue's info line.
    pub(crate) fn hw_accel_name(&self) -> Option<&'static str> {
        self.streams.values().next().map(|s| s.decoder.hw_accel_name())
    }

    fn open(&self, clip: &Clip, file: &MediaFile, secs: f64, width: u32, height: u32) -> Option<ClipStream> {
        self.opened.set(self.opened.get() + 1);
        // The clip's rotation when it has one, else the media's. A clip
        // created by a path that did not copy the media's rotation would
        // otherwise render sideways while playing upright, since the
        // viewer reads the media pool and this read only the clip.
        // (A user who deliberately sets a rotated file to 0 gets the
        // media's value back — distinguishing "unset" from "set to zero"
        // needs a flag on the clip, as `log_format_user_set` does for
        // the analogous problem with log formats.)
        let rotation = if clip.source_rotation != 0 { clip.source_rotation } else { file.source_rotation };
        // Decoded at the picture's own aspect, not the frame's. Scaled
        // to the frame it was stretched: a portrait phone clip in a
        // landscape sequence came out three times too wide, while the
        // viewer — handed the clip at its own size — pillarboxed it.
        let (width, height) = crate::clip_decoder::display_size(&file.path, rotation)
            .map_or((width, height), |(sw, sh)| fitted_size(sw, sh, width, height));
        let decoder = RenderDecoder::open(&file.path, secs.max(0.0), f64::INFINITY, width, height, self.fps, rotation)
            .map_err(|e| warn!("export: cannot open {:?}: {e}", file.path))
            .ok()?
            .with_log_format(file.log_format)
            .with_working_colorspace(self.working);
        let m = klipp_color::camera_log::primaries_to_working_matrix(decoder.source_primaries(), self.working);
        let identity = m == [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        Some(ClipStream {
            decoder,
            to_working: (!identity).then_some(m),
            current: None,
            next: None,
            ended: false,
            error: None,
            held_warned: false,
            path: file.path.clone(),
            width,
            height,
        })
    }
}

impl ExportFrameSource<'_> {
    /// A reversed clip's frame at `source_secs` (#617): from a reader
    /// that decodes a window back from the time asked for, turned and
    /// sized as the clip's forward decoder would have it.
    fn reversed_frame(&mut self, clip: &Clip, file: &MediaFile, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let t0 = std::time::Instant::now();
        if !self.reversed.contains_key(&clip.id) {
            let rotation = if clip.source_rotation != 0 { clip.source_rotation } else { file.source_rotation };
            let reader = klipp_media::frame_reader::BackwardFrameReader::open_linear(&file.path, file.log_format, self.working)
                .or_else(|| {
                    warn!("export: cannot open {:?} to read it backward", file.path);
                    None
                })?;
            self.reversed.insert(clip.id, (reader, rotation));
        }
        let (reader, rotation) = self.reversed.get_mut(&clip.id)?;
        let (pixels, w, h, pts) = reader.decode_upright(source_secs, (width, height), *rotation)?;
        self.t_decode += t0.elapsed();
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(pixels)), width: w, height: h, pts: Some(pts) })
    }
}

impl FrameSource for ExportFrameSource<'_> {
    fn frame(&mut self, clip: &Clip, source_secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        let file = self.media.get(media_id)?;
        self.used.insert(clip.id);
        if clip.is_reversed() {
            return self.reversed_frame(clip, file, source_secs, width, height);
        }
        if !self.streams.contains_key(&clip.id) {
            let s = self.open(clip, file, source_secs, width, height)?;
            self.streams.insert(clip.id, s);
        }
        let t0 = std::time::Instant::now();
        // Split the borrow: the stream and the shared converter are
        // different fields, and the decode needs both at once.
        let Self { streams, gpu, .. } = self;
        let stream = streams.get_mut(&clip.id)?;
        stream.seek_forward(source_secs, gpu);
        let (to_working, w, h) = (stream.to_working, stream.width, stream.height);
        let decoded = stream.current.as_ref();
        self.t_decode += t0.elapsed();
        let t1 = std::time::Instant::now();
        let out = decoded.map(|d| source_frame(d, to_working, w, h));
        self.t_convert += t1.elapsed();
        out
    }

    /// The decoder's next frame, already pulled by `frame`'s seek.
    fn frame_after(&mut self, clip: &Clip, _source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        let Self { streams, gpu, .. } = self;
        let stream = streams.get_mut(&clip.id)?;
        if stream.next.is_none() {
            stream.next = stream.pull(gpu);
        }
        let (to_working, w, h) = (stream.to_working, stream.width, stream.height);
        stream.next.as_ref().map(|d| source_frame(d, to_working, w, h))
    }

    /// Frames at other times, for OFX plugins that read them.
    ///
    /// Export had no implementation and took the trait's `None`, which
    /// runs those plugins on the current frame alone. The viewer does
    /// implement it. So a retime, a frame blend or a slit-scan showed
    /// one picture while editing and wrote a different one to the file
    /// — and the host advertises `OfxImageEffectPropTemporalClipAccess`
    /// as supported, so a plugin that asks is being told yes and handed
    /// the wrong thing.
    ///
    /// The main decoder cannot serve this: it is positioned on the
    /// frame being written and reads forward, so seeking it would
    /// derail the export. This holds a second, independent reader on
    /// the same file, which decides per request whether to seek or read
    /// forward — so a plugin walking N-1, N, N+1 alongside an export
    /// that is itself walking forward never seeks at all. It replaced
    /// a helper that opened, seeked, decoded and closed the file on
    /// every request.
    fn temporal_frames(&mut self, clip: &Clip) -> Option<klipp_plugin::FrameProviderFn> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        let file = self.media.get(media_id)?;
        let (path, log_format) = (file.path.clone(), file.log_format);
        let source_in = clip.source_in.to_secs_f64();
        // Colour-managed, with the log format export already knows per
        // clip. Plain swscale here rendered log flat and PQ washed
        // out, so a plugin reading neighbours saw a different picture
        // from the one the viewer gave it.
        self.opened.set(self.opened.get() + 1);
        let reader = std::cell::RefCell::new(
            klipp_media::frame_reader::SeekableFrameReader::open_colour_managed(&path, log_format)?,
        );
        Some(Box::new(move |t: f64| {
            reader.borrow_mut().decode_at((source_in + t).max(0.0))
        }))
    }

    /// The original file's subject matte (#985 phase 3).
    fn subject_matte_dir(&mut self, clip: &Clip, model: u32) -> Option<std::path::PathBuf> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        let file = self.media.get(media_id)?;
        klipp_compose::subject_matte::media_dir(&file.path, klipp_compose::subject_matte::decode_rotation(clip, file.source_rotation), model)
    }

    /// The original file's analysis key, at the analysis width and the
    /// media's rate (M4 step 9).
    fn analysis_key(&mut self, clip: &Clip, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        media_analysis_key(self.media.get(media_id)?, kind)
    }

    /// The whole media, from its start to its duration.
    fn media_range(&mut self, clip: &Clip) -> Option<(f64, f64)> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        Some((0.0, self.media.get(media_id)?.duration_secs?))
    }

    fn media_fps(&mut self, clip: &Clip) -> Option<f64> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        let (num, den) = self.media.get(media_id)?.fps?;
        (den > 0).then(|| num as f64 / den as f64)
    }

    fn temporal_frames_linear_at_size(&mut self, clip: &Clip, working: WorkingColorspace, width: u32, height: u32) -> Option<klipp_compose::LinearFrameProviderFn> {
        self.temporal_frames_linear(clip, working)
            .map(|reader| klipp_compose::render::fit_linear_reader(reader, width, height))
    }

    /// The transfer the snapshot says the file's frames decode through.
    fn media_transfer(&mut self, media: &str) -> Option<klipp_color::hdr::HdrTransfer> {
        klipp_compose::resolve::media_id(media).and_then(|id| self.media.get(&id))?.scene_transfer
    }

    /// From the snapshot's media and readers kept across the export.
    fn media_frame(&mut self, media: &str, secs: f64, working: WorkingColorspace, width: u32, height: u32) -> Result<Vec<f32>, klipp_nodes::side_picture::SideMissing> {
        use klipp_nodes::side_picture::SideMissing;
        let file = klipp_compose::resolve::media_id(media).and_then(|id| self.media.get(&id)).ok_or_else(|| SideMissing::File(media.to_owned()))?;
        if !file.path.exists() {
            return Err(SideMissing::File(file.path.display().to_string()));
        }
        if !self.media_readers.is_open(&file.path) {
            self.opened.set(self.opened.get() + 1);
        }
        let (pixels, w, h) = self.media_readers.read(&file.path, file.log_format, working, secs).ok_or(SideMissing::Unavailable)?;
        klipp_compose::render::fit_linear(pixels, w, h, width, height).ok_or(SideMissing::Unavailable)
    }

    /// The same frames in linear light, for the renderer's own use.
    fn temporal_frames_linear(
        &mut self,
        clip: &Clip,
        working: WorkingColorspace,
    ) -> Option<klipp_compose::LinearFrameProviderFn> {
        let ClipKind::Media { media_id } = &clip.kind else { return None };
        let file = self.media.get(media_id)?;
        let (path, log_format) = (file.path.clone(), file.log_format);
        let source_in = clip.source_in.to_secs_f64();
        self.opened.set(self.opened.get() + 1);
        let mut reader =
            klipp_media::frame_reader::SeekableFrameReader::open_linear(&path, log_format, working)?;
        Some(Box::new(move |t: f64| reader.decode_at((source_in + t).max(0.0)).map(|(px, w, h)| (Arc::new(px), w, h))))
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
    // The GPU bridge's table too: the two decodes are one.
    let table = klipp_effects::gpu::srgb16_decode_table();
    let (lut, alpha) = table.split_at(65536);
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
            dst[3] = alpha[px[3] as usize];
        });
    out
}

#[cfg(test)]
mod fitted_size_tests {
    use super::fitted_size;

    #[test]
    fn a_picture_the_frames_shape_fills_it_and_another_keeps_its_own() {
        assert_eq!(fitted_size(3840, 2160, 1920, 1080), (1920, 1080));
        // A 2.35 scope picture: letterboxed.
        assert_eq!(fitted_size(1920, 818, 1920, 1080), (1920, 818));
        // An upright phone clip in a landscape frame: pillarboxed.
        assert_eq!(fitted_size(2160, 3840, 1920, 1080), (608, 1080));
        // Rounding that only moves a pixel is the same shape.
        assert_eq!(fitted_size(1280, 721, 1280, 720), (1280, 720));
        // An odd picture keeps its odd side.
        assert_eq!(fitted_size(1920, 803, 1920, 1080), (1920, 803));
    }
}

#[cfg(test)]
mod temporal_frames_tests {
    use super::*;
    use klipp_timeline::{Clip, ClipKind};

    /// `testsrc` puts a frame counter and a moving pattern in the
    /// picture, so two times are two visibly different frames — which
    /// is what tells a real seek from the same frame handed back twice.
    fn test_clip_file(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let out = dir.join("temporal.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "testsrc=size=160x90:rate=24:duration=3",
                "-c:v", "mpeg4", "-q:v", "3",
            ])
            .arg(&out)
            .status()
            .ok()?;
        ok.success().then_some(out)
    }

    fn source_and_clip(
        path: std::path::PathBuf,
        source_in_frames: i64,
    ) -> (HashMap<MediaId, MediaFile>, Clip) {
        let id = MediaId::new();
        let mut media = HashMap::new();
        media.insert(id, MediaFile { path, log_format: None, source_rotation: 0, ..Default::default() });
        let tc = |f: i64| klipp_core::RationalTime::from_frames(f, 24);
        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id: id },
            timeline_in: tc(0),
            timeline_out: tc(48),
            source_in: tc(source_in_frames),
            source_out: tc(source_in_frames + 48),
            ..Clip::gap(tc(0), tc(48))
        };
        (media, clip)
    }

    /// #985 phase 3: the subject matte is read by the turn the clip is
    /// decoded by: its own when it has one, else its media's, as `open`
    /// decodes it, never the media's alone.
    #[test]
    fn a_subject_matte_is_read_by_the_clips_turn() {
        let dir = std::env::temp_dir().join(format!("klipp-matte-turn-{}", ClipId::new().0));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("take.mov");
        std::fs::write(&file, b"not a film").unwrap();
        let (mut media, mut clip) = source_and_clip(file.clone(), 0);
        let id = clip.media_id().unwrap();
        media.get_mut(&id).unwrap().source_rotation = 270;
        let mut src = ExportFrameSource::new(&media, 24.0, klipp_core::WorkingColorspace::Rec709Linear);
        let at = |turn: i32| klipp_compose::subject_matte::media_dir(&file, turn, 0);
        assert_eq!(src.subject_matte_dir(&clip, 0), at(270), "the media's turn when the clip has none");
        clip.source_rotation = 90;
        assert_eq!(src.subject_matte_dir(&clip, 0), at(90), "the clip's own turn");
        if let (Some(a), Some(b)) = (at(90), at(270)) {
            assert_ne!(a, b);
            assert!(a.to_string_lossy().ends_with("-r90"), "{}", a.display());
        }
        assert_eq!(klipp_compose::subject_matte::decode_rotation(&clip, 270), 90);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Export used to take the trait's `None` here, so an OFX plugin
    /// that reads other frames got only the current one and wrote
    /// something different from what the viewer showed.
    #[test]
    fn export_gives_a_plugin_the_frames_it_asks_for() {
        let dir = std::env::temp_dir().join(format!("klipp-tf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(path) = test_clip_file(&dir) else {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        };
        let (media, clip) = source_and_clip(path, 0);
        let mut src =
            ExportFrameSource::new(&media, 24.0, klipp_core::WorkingColorspace::Rec709Linear);

        let frames = src.temporal_frames(&clip).expect("export must provide frames");

        let (a, w, h) = frames(0.0).expect("frame at t=0");
        assert!(w > 0 && h > 0 && a.len() == (w * h * 4) as usize);

        // A different time has to be a different picture, or the
        // provider is handing back one frame under several names.
        let (b, _, _) = frames(1.0).expect("frame at t=1");
        assert_ne!(a, b, "t=0 and t=1 returned the same picture");

        // And asking again for one already seen gives exactly what it
        // gave before — the memo, which is what keeps a plugin reading
        // N-1, N, N+1 to one decode per frame.
        let (a_again, _, _) = frames(0.0).expect("frame at t=0 again");
        assert_eq!(a, a_again, "the same time gave two different pictures");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Export's provider is colour-managed now, so the display-
    /// referred fallback has to be exactly the old swscale path — a
    /// colour-managed reader that quietly changed ordinary footage
    /// would be a regression dressed as a fix.
    ///
    /// The scene-referred half (PQ, HLG, camera log) is not covered
    /// here: it needs footage with those tags, and `testsrc` has none.
    #[test]
    fn colour_management_leaves_display_referred_footage_alone() {
        let dir = std::env::temp_dir().join(format!("klipp-tf-cm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(path) = test_clip_file(&dir) else {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        };
        let mut plain =
            klipp_media::frame_reader::SeekableFrameReader::open_rgba(&path).expect("plain");
        let mut managed =
            klipp_media::frame_reader::SeekableFrameReader::open_colour_managed(&path, None)
                .expect("managed");
        for t in [0.0f64, 0.5, 1.25] {
            let a = plain.decode_at(t).expect("plain frame");
            let b = managed.decode_at(t).expect("managed frame");
            assert_eq!(a.1, b.1, "width differs at t={t}");
            assert_eq!(a.2, b.2, "height differs at t={t}");
            assert_eq!(a.0, b.0, "pixels differ at t={t}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The linear provider has to render a frame the way the main
    /// path renders the one beside it, or anything averaging the two
    /// is comparing pictures rather than samples. That is the whole
    /// reason it exists separately from the OFX one.
    ///
    /// For a display-referred source in a Rec.709 working space the
    /// two are the same conversion, so this holds `open_linear`
    /// against decoding the 8-bit frame itself. A scene-referred
    /// source takes a different branch, which `testsrc` cannot
    /// produce and this does not claim to cover.
    #[test]
    fn the_linear_provider_decodes_what_the_srgb_one_encodes() {
        let dir = std::env::temp_dir().join(format!("klipp-tf-lin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(path) = test_clip_file(&dir) else {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        };
        let mut bytes =
            klipp_media::frame_reader::SeekableFrameReader::open_rgba(&path).expect("u8 reader");
        let mut linear = klipp_media::frame_reader::SeekableFrameReader::open_linear(
            &path,
            None,
            WorkingColorspace::Rec709Linear,
        )
        .expect("linear reader");

        for t in [0.0f64, 0.75, 1.5] {
            let (u8s, w, h) = bytes.decode_at(t).expect("u8 frame");
            let (lin, lw, lh) = linear.decode_at(t).expect("linear frame");
            assert_eq!((w, h), (lw, lh), "size differs at t={t}");
            assert_eq!(lin.len(), u8s.len(), "channel count differs at t={t}");
            let want = klipp_effects::srgb_u8_to_linear_f32_rgba(&u8s);
            let worst = lin
                .iter()
                .zip(want.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(worst < 1e-5, "t={t}: linear and decoded sRGB differ by {worst}");
        }

        // And it is a real picture, not zeroes agreeing with zeroes.
        let (lin, _, _) = linear.decode_at(0.75).expect("linear frame");
        let bright = lin.chunks_exact(4).filter(|p| p[0] + p[1] + p[2] > 0.05).count();
        assert!(bright > 100, "the linear frame is essentially empty");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The time a plugin asks for is clip time; the file is read at
    /// `source_in + t`. Getting this wrong reads the right frame of the
    /// wrong part of the media, which looks plausible and is not.
    #[test]
    fn the_provider_reads_from_the_clips_source_in() {
        let dir = std::env::temp_dir().join(format!("klipp-tf-si-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(path) = test_clip_file(&dir) else {
            eprintln!("skipping: ffmpeg unavailable");
            return;
        };

        // One clip starting at the file's head, one 24 frames in.
        let (media_a, clip_a) = source_and_clip(path.clone(), 0);
        let mut src_a =
            ExportFrameSource::new(&media_a, 24.0, klipp_core::WorkingColorspace::Rec709Linear);
        let frames_a = src_a.temporal_frames(&clip_a).unwrap();

        let (media_b, clip_b) = source_and_clip(path, 24);
        let mut src_b =
            ExportFrameSource::new(&media_b, 24.0, klipp_core::WorkingColorspace::Rec709Linear);
        let frames_b = src_b.temporal_frames(&clip_b).unwrap();

        // Clip B at t=0 is the file at 1.0 s, which is clip A at t=1.0.
        let (b_at_zero, _, _) = frames_b(0.0).expect("b at 0");
        let (a_at_one, _, _) = frames_a(1.0).expect("a at 1");
        assert_eq!(
            b_at_zero, a_at_one,
            "a clip starting 24 frames in did not offset its reads by source_in",
        );

        // And it is not simply ignoring the offset.
        let (a_at_zero, _, _) = frames_a(0.0).expect("a at 0");
        assert_ne!(b_at_zero, a_at_zero, "source_in had no effect at all");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The key of `kind`'s analysis of `file`: the original file's, at the
/// analysis width and the media's rate (M4 step 9). `None` when its rate
/// or size isn't known.
pub fn media_analysis_key(file: &crate::MediaFile, kind: klipp_analysis::assets::AssetKind) -> Option<klipp_analysis::assets::AssetKey> {
    // The viewer's formula (`asset_key`): both read one asset.
    klipp_analysis::assets::key_at(kind, &file.path, file.resolution?, file.fps?)
}
