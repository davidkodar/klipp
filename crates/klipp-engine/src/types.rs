use klipp_core::RationalTime;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A decoded video frame with its presentation timestamp.
///
/// `data` is RGBA8. HW paths (NVDEC / VAAPI) download + convert to RGBA before
/// reaching this struct (see `klipp-engine::video_decoder` and the GPU NV12
/// converter in `klipp-effects::nv12_gpu`).
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
