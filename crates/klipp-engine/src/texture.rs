use std::sync::{Arc, Mutex};

pub struct FrameBuffer {
    pub data: Vec<u8>,
    /// Pre-grade original frame for Before/After split view.
    pub original_data: Vec<u8>,
    /// Pre-grade decoded frame — used by UI thread to re-grade without engine round-trip.
    pub pregrade_data: Vec<u8>,
    /// Tapped node output for scope display (empty = use main data).
    pub scope_data: Vec<u8>,
    /// Fast reduced-resolution graded preview for instant scope feedback.
    pub scope_preview: Vec<u8>,
    pub scope_preview_w: u32,
    pub scope_preview_h: u32,
    /// GPU-rendered scope texture (RGBA) — ready for display.
    pub scope_texture: Vec<u8>,
    pub scope_texture_w: u32,
    pub scope_texture_h: u32,
    /// Which scope mode was rendered (0=waveform, 1=parade, 2=vectorscope, 3=histogram).
    pub scope_texture_mode: u8,
    pub width: u32,
    pub height: u32,
    pub dirty: bool,
    /// Monotonically increasing counter — bumped on every write_frame().
    /// Scopes use this instead of hashing frame data to detect changes.
    pub generation: u64,
}

impl FrameBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            data: vec![0u8; (width * height * 4) as usize],
            original_data: Vec::new(),
            pregrade_data: Vec::new(),
            scope_data: Vec::new(),
            scope_preview: Vec::new(),
            scope_preview_w: 0,
            scope_preview_h: 0,
            scope_texture: Vec::new(),
            scope_texture_w: 0,
            scope_texture_h: 0,
            scope_texture_mode: 255, // invalid = no texture
            width,
            height,
            dirty: false,
            generation: 0,
        }
    }

    pub fn write_frame(&mut self, rgba_data: Vec<u8>, width: u32, height: u32) {
        self.data = rgba_data;
        self.width = width;
        self.height = height;
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn clear(&mut self) {
        self.data.fill(0);
        self.dirty = true;
    }
}

pub type SharedFrameBuffer = Arc<Mutex<FrameBuffer>>;
