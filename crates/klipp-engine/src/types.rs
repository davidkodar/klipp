use klipp_core::RationalTime;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A decoded video frame with its presentation timestamp.
pub struct DecodedFrame {
    pub pts: RationalTime,
    pub width: u32,
    pub height: u32,
    pub data: FrameData,
    pub is_eof: bool,
}

pub enum FrameData {
    /// Software decode path — already converted to RGBA.
    Rgba(Vec<u8>),
    /// Hardware decode path — NV12 planes before GPU conversion.
    Nv12 { y: Vec<u8>, uv: Vec<u8> },
}

impl DecodedFrame {
    /// Get the PTS as seconds (f64).
    pub fn pts_secs(&self) -> f64 {
        self.pts.to_secs_f64()
    }

    /// Convert frame data to RGBA bytes, regardless of source format.
    pub fn to_rgba(&self) -> Vec<u8> {
        match &self.data {
            FrameData::Rgba(data) => data.clone(),
            FrameData::Nv12 { y, uv } => {
                nv12_to_rgba(y, uv, self.width, self.height)
            }
        }
    }

    /// Move frame data to RGBA bytes, avoiding a clone for the common RGBA case.
    pub fn into_rgba(self) -> Vec<u8> {
        match self.data {
            FrameData::Rgba(data) => data,
            FrameData::Nv12 { ref y, ref uv } => {
                nv12_to_rgba(y, uv, self.width, self.height)
            }
        }
    }

    /// Create an EOF sentinel frame.
    pub fn eof() -> Self {
        Self {
            pts: RationalTime::zero(),
            width: 0,
            height: 0,
            data: FrameData::Rgba(Vec::new()),
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

/// NV12 → RGBA conversion (BT.709).
fn nv12_to_rgba(y_plane: &[u8], uv_plane: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let y_stride = width;
    let uv_stride = width;

    for row in 0..height {
        for col in 0..width {
            let y = y_plane[(row * y_stride + col) as usize] as f32;
            let uv_row = row / 2;
            let uv_col = (col / 2) * 2;
            let u = uv_plane[(uv_row * uv_stride + uv_col) as usize] as f32;
            let v = uv_plane[(uv_row * uv_stride + uv_col + 1) as usize] as f32;

            // BT.709 YUV → RGB
            let y = y - 16.0;
            let u = u - 128.0;
            let v = v - 128.0;
            let r = (1.164 * y + 1.793 * v).clamp(0.0, 255.0) as u8;
            let g = (1.164 * y - 0.213 * u - 0.533 * v).clamp(0.0, 255.0) as u8;
            let b = (1.164 * y + 2.112 * u).clamp(0.0, 255.0) as u8;

            let i = ((row * width + col) * 4) as usize;
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 255;
        }
    }
    rgba
}
