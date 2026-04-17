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
/// # Linear-light migration (CRIT-7)
///
/// This is the legacy u8 sRGB representation. A parallel [`LinearFrame`]
/// type carries the same payload as `Vec<f32>` linear-light RGBA and will
/// eventually replace this one. During the multi-phase migration both types
/// coexist and [`DecodedFrame::to_linear`] / [`LinearFrame::to_srgb_u8`]
/// shuttle between them at stage boundaries.
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
    /// Part of the CRIT-7 migration: until the decoder itself emits
    /// [`LinearFrame`] directly (Phase 3), callers that want linear
    /// data go through here. The conversion is lossless within f32
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
/// Phase 2 of the CRIT-7 migration introduces this type alongside
/// `DecodedFrame` without flipping any call sites. Phase 3 points the
/// decoder at it; later phases route effects, compositing, and the
/// viewer through it; Phase 10 renames it to `DecodedFrame` and
/// retires the u8 variant entirely.
pub struct LinearFrame {
    pub pts: RationalTime,
    pub width: u32,
    pub height: u32,
    /// Interleaved RGBA, row-major, linear-light. Length must be
    /// `width * height * 4`.
    pub data: Vec<f32>,
    pub is_eof: bool,
}

impl LinearFrame {
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
        }
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
        };
        assert_eq!(frame.data[0], 2.5);
        let back = frame.to_srgb_u8();
        assert_eq!(back.data[0], 255, "above-one should clamp at 255");
        // 0.5 linear encodes to ~188 sRGB.
        assert!((back.data[1] as i32 - 188).abs() <= 1);
    }
}

/// A bounded queue of decoded frames.
pub struct FrameQueue {
    frames: VecDeque<DecodedFrame>,
    capacity: usize,
}

impl FrameQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, frame: DecodedFrame) {
        if self.frames.len() < self.capacity {
            self.frames.push_back(frame);
        }
    }

    pub fn pop(&mut self) -> Option<DecodedFrame> {
        self.frames.pop_front()
    }

    pub fn front(&self) -> Option<&DecodedFrame> {
        self.frames.front()
    }

    /// Get the PTS in seconds of the front frame, or f64::MAX if empty.
    pub fn front_pts_secs(&self) -> f64 {
        self.frames.front().map(|f| f.pts_secs()).unwrap_or(f64::MAX)
    }

    /// Pop a frame whose PTS is closest to the given time (within 40ms).
    pub fn pop_at(&mut self, target_secs: f64) -> Option<DecodedFrame> {
        if let Some(front) = self.frames.front() {
            if (front.pts_secs() - target_secs).abs() < 0.040 {
                return self.frames.pop_front();
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

    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

/// Shared frame queue between decoder and display threads.
pub type SharedFrameQueue = Arc<Mutex<FrameQueue>>;

/// Audio sample ring buffer — f32 interleaved stereo.
pub type AudioRingBuf = Arc<ringbuf::HeapRb<f32>>;
