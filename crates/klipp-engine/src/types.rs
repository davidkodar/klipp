use klipp_color::transfer::{linear_to_srgb, srgb_to_linear};
use klipp_core::RationalTime;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A decoded video frame with its presentation timestamp.
///
/// `data` is RGBA8 (sRGB-encoded). HW paths (NVDEC / VAAPI) download + convert
/// to RGBA before reaching this struct (see `klipp-engine::video_decoder` and
/// the GPU NV12 converter in `klipp-effects::nv12_gpu`).
///
/// The pipeline works in linear light: the decoder emits
/// [`LinearFrame`] and every stage after it takes floats. This type is
/// the u8 sRGB representation kept for the edges that genuinely hold
/// bytes — readback from the conversion shader, and anything handing
/// pixels to a surface that wants sRGB.
/// [`DecodedFrame::to_linear`] and [`LinearFrame::to_srgb_u8`] convert
/// between the two.
pub struct DecodedFrame {
    pub pts: RationalTime,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub is_eof: bool,
}

impl DecodedFrame {
    /// Get the PTS as seconds (f64).
    pub fn pts_secs(&self) -> f64 {
        self.pts.to_secs_f64()
    }

    /// Clone the frame data as RGBA bytes.
    pub fn to_rgba(&self) -> Vec<u8> {
        self.data.clone()
    }

    /// Move the frame data out as RGBA bytes.
    pub fn into_rgba(self) -> Vec<u8> {
        self.data
    }

    /// Create an EOF sentinel frame.
    pub fn eof() -> Self {
        Self {
            pts: RationalTime::zero(),
            width: 0,
            height: 0,
            data: Vec::new(),
            is_eof: true,
        }
    }

    /// Decode this sRGB u8 frame to a linear-light f32 frame.
    ///
    /// The decoder emits [`LinearFrame`] itself, so this is for frames
    /// that arrive as bytes from somewhere else. The conversion is
    /// lossless within f32
    /// precision — `to_linear().to_srgb_u8()` round-trips to the same
    /// bytes modulo the quantisation noise introduced by the sRGB u8
    /// encoding.
    pub fn to_linear(&self) -> LinearFrame {
        let mut data = Vec::with_capacity(self.data.len());
        for chunk in self.data.chunks_exact(4) {
            data.push(srgb_to_linear(chunk[0] as f32 / 255.0));
            data.push(srgb_to_linear(chunk[1] as f32 / 255.0));
            data.push(srgb_to_linear(chunk[2] as f32 / 255.0));
            // Alpha stays linear per sRGB spec.
            data.push(chunk[3] as f32 / 255.0);
        }
        LinearFrame {
            pts: self.pts,
            width: self.width,
            height: self.height,
            data,
            is_eof: self.is_eof,
            gpu_source: None,
            // `DecodedFrame` is already sRGB u8; the resulting linear
            // frame is in linear-BT.709 working space — that's exactly
            // what `BT709_LIMITED_SDR.transfer = Bt709` represents
            // post-EOTF, so the matrix/primaries/transfer all sit at
            // their working-space defaults. The range field doesn't
            // apply post-decode; left at the safe default.
            colorspace: klipp_core::SourceColorspace::default(),
            hdr: None,
            seek_epoch: 0,
        }
    }
}

/// A decoded video frame in linear-light f32 RGBA.
///
/// Same shape as [`DecodedFrame`], but each channel is a linear-light
/// float: 0.18 is 18 % perceptual mid-grey, 1.0 is diffuse white, and
/// values may legitimately exceed 1.0 (highlight headroom — the whole
/// point of the linear-light pipeline). Alpha stays linear, per sRGB.
///
/// This is what the decoder produces and what effects, compositing and
/// the viewer's display transform consume; [`FrameQueue`] holds these.
/// [`DecodedFrame`] is the u8 counterpart for the byte-shaped edges.
pub struct LinearFrame {
    pub pts: RationalTime,
    pub width: u32,
    pub height: u32,
    /// Interleaved RGBA, row-major, linear-light. Length must be
    /// `width * height * 4`.
    pub data: Vec<f32>,
    pub is_eof: bool,
    /// GPU-resident mirror of `data`, attached by the decoder on a
    /// hardware decode unless `KLIPP_GPU_FRAMES=0` turns that off.
    ///
    /// When `Some`, the grade chain binds the underlying `wgpu::Buffer`
    /// as a compute-shader input without re-uploading `data`, and `data`
    /// stays empty until something asks for it
    /// ([`LinearFrame::ensure_cpu_data`]). The viewer's graded fast path
    /// needs this: `present_graded_frame_on_gpu` declines a frame that has
    /// neither a `gpu_source` nor GPU-resident HDR codes.
    ///
    /// `None` on a software decode, on a frame the hardware path could not
    /// hand over, and with the fallback flag set — downstream consumers
    /// then use `data`, which every one of them can.
    pub gpu_source: Option<std::sync::Arc<crate::frame::GpuFrame>>,
    /// Resolved source colourspace from the AVFrame's tagged range /
    /// matrix / primaries / transfer fields, with sensible heuristic
    /// fallbacks for "unspecified" tags. Carried through the pipeline
    /// so the YUV→RGB shader, gamut conversion, and EOTF decode
    /// dispatch correctly per-source.
    pub colorspace: klipp_core::SourceColorspace,
    /// HDR (PQ/HLG) sources only: the code values the grade reads, with
    /// `data` staying display-referred for everything else. See
    /// [`HdrInput`].
    pub hdr: Option<HdrInput>,
    /// Which seek this frame was decoded under —
    /// [`crate::video_decoder::VideoDecoder::requested_seek`] at the time
    /// the decode thread produced it, and 0 before any seek.
    ///
    /// A seek is a command on a channel, so a frame converted just before
    /// the decode thread takes one can still land in the queue after it has
    /// been cleared. Consumers used to guess at those from the timestamp —
    /// "more than a second from the target, so probably stale" — which
    /// cannot tell a stale frame from a legitimately distant one, and let a
    /// frame up to two seconds from the seek target be taken for the frame
    /// at it. Comparing this against the decoder's newest requested seek
    /// answers the question exactly.
    pub seek_epoch: u64,
}

impl LinearFrame {
    /// Bring GPU-resident HDR codes to the CPU, once, in place.
    ///
    /// Paths that both cache a frame and grade it would otherwise read the
    /// codes back twice. After this, every `HdrInput::cpu_codes` on the
    /// frame is an `Arc` clone.
    pub fn realise_hdr_on_cpu(&mut self) {
        if let Some(h) = self.hdr.as_mut() {
            if matches!(h.codes, HdrCodes::Gpu { .. }) {
                *h = h.to_cpu();
            }
        }
    }

    /// PTS as seconds. Thin wrapper for symmetry with
    /// [`DecodedFrame::pts_secs`].
    pub fn pts_secs(&self) -> f64 {
        self.pts.to_secs_f64()
    }

    /// Create an EOF sentinel frame.
    pub fn eof() -> Self {
        Self {
            pts: RationalTime::zero(),
            width: 0,
            height: 0,
            data: Vec::new(),
            is_eof: true,
            gpu_source: None,
            colorspace: klipp_core::SourceColorspace::default(),
            hdr: None,
            seek_epoch: 0,
        }
    }

    /// Attach a GPU-resident mirror of the frame data. The caller
    /// asserts `gpu_source`'s underlying `wgpu::Buffer` holds RGBA8
    /// pixels equivalent to `data`'s linear-f32 RGBA at the same
    /// `width × height`. Used by the decoder's GPU-resident path
    /// (phase 4c) so phase 4d's grade chain can bind the buffer
    /// directly.
    pub fn with_gpu_source(mut self, gpu: std::sync::Arc<crate::frame::GpuFrame>) -> Self {
        self.gpu_source = Some(gpu);
        self
    }

    /// Realise CPU `data` from `gpu_source` if it isn't already.
    ///
    /// The decoder may return GPU-resident frames with
    /// `data` empty — the underlying RGBA8 sits in `gpu_source`'s
    /// `wgpu::Buffer` and never gets read back unless something asks.
    /// Engine consumers that still need the linear-f32 CPU buffer
    /// (pregrade cache, render cache, scope preview, compare-mode
    /// snapshot) call this helper right before they touch `data`. It
    /// performs the GPU→CPU readback (`map_async` + `device.poll`),
    /// then linearises the resulting sRGB-u8 bytes into linear-f32
    /// RGBA in place.
    ///
    /// No-op when `data` is already populated (the SW decode path,
    /// the `KLIPP_GPU_FRAMES=0` fallback path, or a previous call to
    /// this method on the same frame).
    pub fn ensure_cpu_data(&mut self) {
        if !self.data.is_empty() || self.gpu_source.is_none() || self.is_eof {
            return;
        }
        // Arc clone so the borrow of `self.gpu_source` ends before the
        // assignment to `self.data`.
        let gpu = match self.gpu_source.clone() {
            Some(g) => g,
            None => return,
        };
        self.data = realise_linear_from_gpu(&gpu);
    }

    /// sRGB-encode this frame to raw RGBA u8 bytes. Mirrors the
    /// u8 [`DecodedFrame::to_rgba`] shape — used by consumers still
    /// on the legacy u8 byte-slice API during the multi-phase
    /// migration.
    pub fn to_rgba(&self) -> Vec<u8> {
        self.to_srgb_u8().data
    }

    /// Move form of [`Self::to_rgba`]. Currently identical (the
    /// linear data always needs conversion) but matches the legacy
    /// [`DecodedFrame::into_rgba`] API.
    pub fn into_rgba(self) -> Vec<u8> {
        self.to_srgb_u8().data
    }

    /// Encode this linear-light frame to sRGB u8 for display or
    /// back-compat with the legacy pipeline. Values outside
    /// `[0.0, 1.0]` are clamped at the byte boundary (*not* earlier
    /// — intermediate passes retain full float range).
    pub fn to_srgb_u8(&self) -> DecodedFrame {
        let mut data = Vec::with_capacity(self.data.len());
        for chunk in self.data.chunks_exact(4) {
            data.push((linear_to_srgb(chunk[0]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            data.push((linear_to_srgb(chunk[1]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            data.push((linear_to_srgb(chunk[2]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            data.push((chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
        DecodedFrame {
            pts: self.pts,
            width: self.width,
            height: self.height,
            data,
            is_eof: self.is_eof,
        }
    }
}

/// Read an `Arc<GpuFrame>`'s RGBA8 buffer back to a CPU `Vec<u8>`.
///
/// Mirrors `Frame::into_cpu`'s map-async pattern but takes a borrow
/// so the `gpu_source` Arc stays attached to the `LinearFrame`. Used
/// by `LinearFrame::ensure_cpu_data` to realise the CPU mirror on
/// demand without releasing the GPU buffer (the grade chain may
/// still consume it via `prime_chain_from_gpu_u8_buffer`).
/// How many times a GPU-resident frame has been read back to CPU, and
/// the total wall-clock spent doing it.
///
/// The GPU-residency plan's whole premise is that these round-trips
/// dominate the per-frame cost, but until now the readback was
/// unmeasured — `ensure_cpu_data` carried no timing and is called from
/// eight sites across the engine (playback, skim, cache pop, source
/// viewer). Deciding whether to restructure the hot path around it
/// needs the number first.
///
/// Cumulative for the process; the engine heartbeat logs deltas.
pub static CPU_READBACK_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static CPU_READBACK_US: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// An HDR frame's input, carried alongside its display-referred `data`.
///
/// HDR frames reach consumers two ways at once. `LinearFrame::data` is
/// display-referred — already tone-mapped — so every consumer that just
/// wants to show or analyse the picture (source viewer, overlays, export,
/// scene detection) keeps working unchanged. This is the other half: the
/// frame's packed 10-bit PQ/HLG code values, which the *grade* reads so it
/// can operate on scene-linear pixels with the highlights intact. The
/// grade's output is then tone-mapped as the clip's last step.
///
/// Packed RGB10 is 4 bytes a pixel — the size of an sRGB-u8 frame — so
/// carrying it costs a frame's readback once, not twice.
#[derive(Clone)]
pub struct HdrInput {
    pub codes: HdrCodes,
    pub transfer: klipp_color::hdr::HdrTransfer,
    /// Source primaries → working space, applied after the transfer curve.
    pub source_to_working: [[f32; 3]; 3],
    pub width: u32,
    pub height: u32,
}

/// Where an HDR frame's code values live. On the GPU they are the
/// decoder's 10-bit codes; on the CPU 10-bit, or 16-bit for a source
/// deeper than 10 bits (#1226).
#[derive(Clone)]
pub enum HdrCodes {
    Cpu(std::sync::Arc<klipp_color::hdr::SceneCodes>),
    Gpu {
        buffer: std::sync::Arc<wgpu::Buffer>,
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
    },
}

impl HdrInput {
    /// The packed codes on the CPU, reading them back if they are
    /// GPU-resident. The readback counts toward `CPU_READBACK_*` like any
    /// other, so the perf overlay reports it.
    pub fn cpu_codes(&self) -> std::sync::Arc<klipp_color::hdr::SceneCodes> {
        match &self.codes {
            HdrCodes::Cpu(c) => std::sync::Arc::clone(c),
            HdrCodes::Gpu { buffer, device, queue } => {
                let t0 = std::time::Instant::now();
                let bytes = readback_buffer(
                    device,
                    queue,
                    buffer,
                    (self.width as u64) * (self.height as u64) * 4,
                );
                let words: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&bytes).to_vec();
                use std::sync::atomic::Ordering::Relaxed;
                CPU_READBACK_COUNT.fetch_add(1, Relaxed);
                CPU_READBACK_US.fetch_add(t0.elapsed().as_micros() as u64, Relaxed);
                std::sync::Arc::new(klipp_color::hdr::SceneCodes::Packed10(words))
            }
        }
    }

    /// A copy whose codes are on the CPU, for caches that outlive the
    /// GPU frame.
    pub fn to_cpu(&self) -> HdrInput {
        HdrInput { codes: HdrCodes::Cpu(self.cpu_codes()), ..self.clone() }
    }

    /// Scene-linear RGBA f32 in the working space, 1.0 = 100 nits — the
    /// grade's input.
    pub fn scene_linear(&self) -> Vec<f32> {
        self.cpu_codes().to_scene_linear_rgba(self.transfer, self.source_to_working)
    }
}

/// Realise a GPU-resident frame as linear-f32 CPU pixels.
///
/// The same work [`LinearFrame::ensure_cpu_data`] performs, exposed so
/// the grade path can do it *on demand* — when the executor reports it
/// actually needs CPU pixels — instead of unconditionally before every
/// frame. Measured at 19.2 ms per frame on a 4K 10-bit HDR source, which
/// is more than a whole 59.94 fps budget, so whether it happens at all
/// is worth deciding rather than assuming.
///
/// Counts into the same [`CPU_READBACK_COUNT`] / [`CPU_READBACK_US`]
/// totals, so the perf overlay reports readbacks wherever they originate.
pub fn realise_linear_from_gpu(gpu: &crate::frame::GpuFrame) -> Vec<f32> {
    // Two expensive halves: the buffer readback (blocking on a device
    // poll) and a u8 -> linear-f32 expansion over width*height*4 floats.
    let t0 = std::time::Instant::now();
    let bytes = readback_gpu_frame_to_u8(gpu);
    let out = u8_rgba_to_linear_f32(&bytes);
    let us = t0.elapsed().as_micros() as u64;
    use std::sync::atomic::Ordering::Relaxed;
    CPU_READBACK_COUNT.fetch_add(1, Relaxed);
    CPU_READBACK_US.fetch_add(us, Relaxed);
    out
}

/// Realise a GPU-resident frame as display-ready sRGB-u8 RGBA bytes.
///
/// The decoder's conversion shaders already leave the frame as sRGB-u8
/// RGBA, so this is the readback with nothing added: 4 bytes per pixel
/// instead of the 16 that [`realise_linear_from_gpu`] produces, and no
/// linear expansion. At 4K that is 33 MB over PCIe rather than 132 MB
/// plus a per-pixel transfer-function evaluation.
///
/// Only correct for consumers that want *display* pixels. Anything that
/// grades, composites or blends needs linear light — use
/// [`realise_linear_from_gpu`] for those.
///
/// Counts into the same [`CPU_READBACK_COUNT`] / [`CPU_READBACK_US`]
/// totals so the perf overlay reports every readback, whatever its shape.
pub fn realise_srgb_u8_from_gpu(gpu: &crate::frame::GpuFrame) -> Vec<u8> {
    let t0 = std::time::Instant::now();
    let bytes = readback_gpu_frame_to_u8(gpu);
    let us = t0.elapsed().as_micros() as u64;
    use std::sync::atomic::Ordering::Relaxed;
    CPU_READBACK_COUNT.fetch_add(1, Relaxed);
    CPU_READBACK_US.fetch_add(us, Relaxed);
    bytes
}

fn readback_gpu_frame_to_u8(gpu: &crate::frame::GpuFrame) -> Vec<u8> {
    let byte_size = (gpu.width as u64) * (gpu.height as u64) * 4;
    readback_buffer(&gpu.device, &gpu.queue, &gpu.rgba_buffer, byte_size)
}

/// Copy the first `byte_size` bytes of a GPU buffer back to the CPU.
/// Zero-filled on failure (treated as device-lost-equivalent; the next
/// frame retries).
fn readback_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
    byte_size: u64,
) -> Vec<u8> {
    struct Ctx<'a> {
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        rgba_buffer: &'a wgpu::Buffer,
    }
    let gpu = Ctx { device, queue, rgba_buffer: buffer };
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("linear_frame_ensure_cpu_data_staging"),
        size: byte_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("linear_frame_ensure_cpu_data"),
    });
    encoder.copy_buffer_to_buffer(gpu.rgba_buffer, 0, &staging, 0, byte_size);
    gpu.queue.submit(std::iter::once(encoder.finish()));

    let slice = staging.slice(..byte_size);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    // Non-blocking poll loop instead of `Maintain::Wait` — see
    // `klipp_effects::gpu::poll_until_mapped` for why Wait
    // deadlocks on a UI-shared device. Falls back to a zero-filled
    // buffer if the map times out (engine treats as device-lost-
    // equivalent and the next frame retries).
    match klipp_effects::gpu::poll_until_mapped(gpu.device, &rx) {
        Ok(Ok(())) => {
            let mapped = slice.get_mapped_range();
            let v = mapped.to_vec();
            drop(mapped);
            staging.unmap();
            v
        }
        other => {
            tracing::warn!(
                "LinearFrame::ensure_cpu_data readback failed ({other:?}); returning zeros",
            );
            vec![0u8; byte_size as usize]
        }
    }
}

/// sRGB-u8 RGBA → linear-f32 RGBA. Matches the encoding the GPU
/// NV12/P010 converters emit (sRGB-encoded RGBA8) so a round-trip
/// through `ensure_cpu_data` is bit-for-bit equivalent to the
/// pre-Phase-4d eager path.
///
/// Uses 256-entry sRGB→linear and u8→[0,1] lookup tables instead of
/// the per-channel `srgb_to_linear` powf, mirroring the decode
/// thread's `rgba_u8_to_linear`. The naive powf path was ~55-70 ms
/// per 1080p frame on the engine thread (single-threaded, 8.3M powf
/// calls); the LUT path drops it to ~3-5 ms (single LUT lookup per
/// channel, auto-vectorised).
fn u8_rgba_to_linear_f32(rgba: &[u8]) -> Vec<f32> {
    let srgb_lut = srgb_u8_to_linear_lut();
    let a_lut = u8_div255_lut();
    let n_pixels = rgba.len() / 4;
    // Capacity reserved once and filled by appending. `set_len` over
    // uninitialised memory is what `clippy::uninit_vec` denies, and the
    // same change in `klipp_color::camera_log` measured *faster* than the
    // unsafe version it replaced, so there is nothing to trade away here.
    let mut out = Vec::with_capacity(n_pixels * 4);
    for chunk in rgba.chunks_exact(4) {
        out.extend_from_slice(&[
            srgb_lut[chunk[0] as usize],
            srgb_lut[chunk[1] as usize],
            srgb_lut[chunk[2] as usize],
            a_lut[chunk[3] as usize],
        ]);
    }
    out
}

/// 256-entry sRGB→linear LUT, computed once. The decode thread's
/// `rgba_u8_to_linear` keeps its own private copy; duplicating the
/// table here (rather than exposing the decoder's) avoids a public
/// dependency edge between the engine type and the decoder module.
fn srgb_u8_to_linear_lut() -> &'static [f32; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0f32; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = srgb_to_linear(i as f32 / 255.0);
        }
        lut
    })
}

/// 256-entry u8→[0,1] LUT for the alpha channel (no gamma decode).
fn u8_div255_lut() -> &'static [f32; 256] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0f32; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            *slot = i as f32 / 255.0;
        }
        lut
    })
}

#[cfg(test)]
mod linear_frame_tests {
    use super::*;

    fn make_u8_frame(data: Vec<u8>, w: u32, h: u32) -> DecodedFrame {
        DecodedFrame {
            pts: RationalTime::zero(),
            width: w,
            height: h,
            data,
            is_eof: false,
        }
    }

    #[test]
    fn empty_frame_converts_both_ways() {
        let f = make_u8_frame(Vec::new(), 0, 0);
        let lin = f.to_linear();
        assert!(lin.data.is_empty());
        let back = lin.to_srgb_u8();
        assert!(back.data.is_empty());
    }

    #[test]
    fn round_trip_preserves_u8_within_one_code_value() {
        // Build a 2×2 frame with varied values, round-trip through
        // linear, and require each channel is within ±1 code value.
        // The ±1 allowance absorbs the sRGB→linear→sRGB quantisation
        // that's unavoidable at 8 bits per channel.
        let src: Vec<u8> = vec![
            0, 64, 128, 255, //
            32, 96, 200, 128, //
            10, 77, 180, 64, //
            200, 50, 5, 255,
        ];
        let frame = make_u8_frame(src.clone(), 2, 2);
        let back = frame.to_linear().to_srgb_u8();
        assert_eq!(back.data.len(), src.len());
        for (i, (&a, &b)) in src.iter().zip(back.data.iter()).enumerate() {
            assert!(
                (a as i32 - b as i32).abs() <= 1,
                "channel {i}: {a} -> {b}",
            );
        }
    }

    #[test]
    fn mid_grey_round_trip_lands_on_18_percent_linear() {
        // sRGB 118 ≈ the canonical 18 % mid-grey sample.
        let frame = make_u8_frame(vec![118, 118, 118, 255], 1, 1);
        let lin = frame.to_linear();
        for i in 0..3 {
            assert!(
                (lin.data[i] - 0.18).abs() < 0.005,
                "channel {i}: {} vs 0.18",
                lin.data[i],
            );
        }
        assert_eq!(lin.data[3], 1.0, "alpha must not be gamma-encoded");
    }

    #[test]
    fn linear_frame_metadata_is_preserved() {
        let pts = RationalTime { value: 1234, rate: 48000 };
        let frame = DecodedFrame {
            pts,
            width: 2,
            height: 2,
            data: vec![0u8; 16],
            is_eof: false,
        };
        let lin = frame.to_linear();
        assert_eq!(lin.pts, pts);
        assert_eq!(lin.width, 2);
        assert_eq!(lin.height, 2);
        assert!(!lin.is_eof);

        let eof = LinearFrame::eof();
        assert!(eof.is_eof);
        assert_eq!(eof.width, 0);
    }

    #[test]
    fn highlights_above_one_survive_in_linear_and_clamp_at_u8_boundary() {
        // Build a linear frame with a highlight pixel above 1.0 and
        // verify (a) the float survives intact, (b) round-tripping
        // to u8 clamps at 255 rather than wrapping.
        let frame = LinearFrame {
            pts: RationalTime::zero(),
            width: 1,
            height: 1,
            data: vec![2.5, 0.5, 0.18, 1.0],
            is_eof: false,
            gpu_source: None,
            colorspace: klipp_core::SourceColorspace::default(),
            hdr: None,
            seek_epoch: 0,
        };
        assert_eq!(frame.data[0], 2.5);
        let back = frame.to_srgb_u8();
        assert_eq!(back.data[0], 255, "above-one should clamp at 255");
        // 0.5 linear encodes to ~188 sRGB.
        assert!((back.data[1] as i32 - 188).abs() <= 1);
    }
}

/// A bounded queue of decoded frames.
///
/// Stores [`LinearFrame`] (linear-light f32), which is what the
/// decoder produces and what consumers take: pop with
/// [`FrameQueue::pop`]. [`FrameQueue::pop_u8`] is the sRGB-byte
/// counterpart, for a consumer that wants bytes.
pub struct FrameQueue {
    frames: VecDeque<LinearFrame>,
    capacity: usize,
}

impl FrameQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Push a linear-light frame onto the queue. This is the
    /// canonical path; the decoder produces linear natively.
    pub fn push(&mut self, frame: LinearFrame) {
        if self.frames.len() < self.capacity {
            self.frames.push_back(frame);
        }
    }

    /// Pop the next linear frame — the normal path.
    pub fn pop(&mut self) -> Option<LinearFrame> {
        self.frames.pop_front()
    }

    /// Pop the next frame as sRGB bytes, converting it via
    /// [`LinearFrame::to_srgb_u8`]. For a consumer that wants bytes;
    /// nothing in the engine does today.
    pub fn pop_u8(&mut self) -> Option<DecodedFrame> {
        self.frames.pop_front().map(|f| f.to_srgb_u8())
    }

    pub fn front(&self) -> Option<&LinearFrame> {
        self.frames.front()
    }

    /// The frame behind the front, if there is one: what the source
    /// viewer looks at to decide whether the front can be skipped.
    pub fn second(&self) -> Option<&LinearFrame> {
        self.frames.get(1)
    }

    /// The PTS in seconds of the newest frame queued, if any: how far on
    /// the decoder has got.
    pub fn newest_pts_secs(&self) -> Option<f64> {
        self.frames.iter().rev().find(|f| !f.is_eof).map(|f| f.pts_secs())
    }

    /// Get the PTS in seconds of the front frame, or f64::MAX if empty.
    pub fn front_pts_secs(&self) -> f64 {
        self.frames.front().map(|f| f.pts_secs()).unwrap_or(f64::MAX)
    }

    /// Pop a frame whose PTS is closest to the given time (within
    /// 40ms). sRGB u8 conversion is applied so consumers on the
    /// legacy path can consume the result directly.
    pub fn pop_at(&mut self, target_secs: f64) -> Option<DecodedFrame> {
        if let Some(front) = self.frames.front() {
            if (front.pts_secs() - target_secs).abs() < 0.040 {
                return self.frames.pop_front().map(|f| f.to_srgb_u8());
            }
        }
        None
    }

    pub fn is_full(&self) -> bool {
        self.frames.len() >= self.capacity
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Keep the frames at or after `secs`, stamped with `epoch` — a
    /// forward seek that the decoder answers by decoding on, for which
    /// the frames already decoded past the new target are still right.
    /// Returns how many remain.
    pub fn keep_from(&mut self, secs: f64, epoch: u64) -> usize {
        self.frames.retain(|f| !f.is_eof && f.pts_secs() >= secs);
        for f in &mut self.frames {
            f.seek_epoch = epoch;
        }
        self.frames.len()
    }

    /// The presentation time of the oldest queued picture, if any.
    pub fn oldest_pts_secs(&self) -> Option<f64> {
        self.frames.iter().find(|f| !f.is_eof).map(|f| f.pts_secs())
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

/// Shared frame queue between decoder and display threads.
pub type SharedFrameQueue = Arc<Mutex<FrameQueue>>;

/// Audio sample ring buffer — f32 interleaved stereo.
pub type AudioRingBuf = Arc<ringbuf::HeapRb<f32>>;

#[cfg(test)]
mod readback_cost {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    /// Measure what `ensure_cpu_data` actually costs at 1080p, split
    /// into its two halves.
    ///
    /// `#[ignore]` because it is a measurement, not an assertion — GPU
    /// timings are machine-dependent and asserting on them would produce
    /// a test that fails for reasons nobody can act on. Run it
    /// deliberately:
    ///
    /// ```text
    /// cargo test --release -p klipp-engine --lib readback_cost \
    ///     -- --ignored --nocapture --test-threads=1
    /// ```
    ///
    /// Both flags are load-bearing. `--release` because the CPU half is a
    /// LUT loop that measures 2.84 ms under the dev profile's opt-level 1
    /// against 1.89 ms optimised. `--test-threads=1` because the two
    /// resolutions contend for the GPU when run in parallel — that inflated
    /// the 1080p reading from 3.87 ms to 7.88 ms, which is the same
    /// cross-measurement contamination that made the perf harness
    /// untrustworthy until b2094ef0.
    ///
    /// Why it exists: the GPU-residency plan assumes GPU→CPU round-trips
    /// dominate the per-frame cost, and `ensure_cpu_data` is the one on
    /// the playback path — called from eight sites in the engine — but it
    /// carried no instrumentation at all. Restructuring a 962-line hot
    /// path around an unmeasured cost is the wrong order of operations.
    #[test]
    #[ignore = "measurement, not an assertion — run with --ignored --nocapture"]
    fn ensure_cpu_data_cost_at_1080p() {
        measure_readback(1920, 1080);
    }

    /// 4K is where the plan says the app is unusable today, so it is the
    /// resolution the decision should be made at. Both halves scale with
    /// pixel count, so this is ~4x the 1080p figures — and at 4K a single
    /// readback is a whole 60 fps frame budget.
    #[test]
    #[ignore = "measurement, not an assertion — run with --ignored --nocapture"]
    fn ensure_cpu_data_cost_at_4k() {
        measure_readback(3840, 2160);
    }

    fn measure_readback(w: u32, h: u32) {
        const REPS: usize = 30;

        let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
            eprintln!("skipping: no usable GpuContext");
            return;
        };

        let byte_len = (w as usize) * (h as usize) * 4;
        let bytes: Vec<u8> = (0..byte_len).map(|i| (i % 251) as u8).collect();

        let buf = std::sync::Arc::new(ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback_cost_input"),
            size: byte_len as u64,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        ctx.queue.write_buffer(&buf, 0, &bytes);
        ctx.device.poll(wgpu::Maintain::Wait);

        let gpu = std::sync::Arc::new(crate::frame::GpuFrame::new(
            ctx.device(),
            ctx.queue(),
            std::sync::Arc::clone(&buf),
            w,
            h,
        ));

        // Warm the driver so the first sample does not dominate.
        for _ in 0..5 {
            let b = readback_gpu_frame_to_u8(&gpu);
            std::hint::black_box(b.len());
        }

        // Half 1: the PCIe readback + device poll.
        let mut readback_ms = Vec::with_capacity(REPS);
        let mut convert_ms = Vec::with_capacity(REPS);
        for _ in 0..REPS {
            let t = std::time::Instant::now();
            let raw = readback_gpu_frame_to_u8(&gpu);
            readback_ms.push(t.elapsed().as_secs_f64() * 1000.0);

            // Half 2: the u8 -> linear-f32 expansion on CPU.
            let t = std::time::Instant::now();
            let lin = u8_rgba_to_linear_f32(&raw);
            convert_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            std::hint::black_box(lin.len());
        }

        let med = |v: &mut Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let rb = med(&mut readback_ms);
        let cv = med(&mut convert_ms);
        eprintln!("ensure_cpu_data at {w}x{h}, median of {REPS}:");
        eprintln!("  GPU readback (PCIe + poll) : {rb:6.2} ms");
        eprintln!("  u8 -> linear f32 (CPU)     : {cv:6.2} ms");
        eprintln!("  total per frame            : {:6.2} ms", rb + cv);

        // Also exercise the instrumented path so the counters are proven
        // to move — that is an assertion worth making.
        let before = CPU_READBACK_COUNT.load(Relaxed);
        let mut frame = LinearFrame {
            pts: klipp_core::RationalTime::from_frames(0, 24),
            width: w,
            height: h,
            data: Vec::new(),
            is_eof: false,
            gpu_source: Some(std::sync::Arc::clone(&gpu)),
            colorspace: klipp_core::SourceColorspace::default(),
            hdr: None,
            seek_epoch: 0,
        };
        frame.ensure_cpu_data();
        assert_eq!(frame.data.len(), byte_len, "CPU mirror must be fully realised");
        assert_eq!(
            CPU_READBACK_COUNT.load(Relaxed),
            before + 1,
            "ensure_cpu_data must record the readback it performed",
        );
    }
}
