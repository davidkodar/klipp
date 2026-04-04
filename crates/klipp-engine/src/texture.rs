use std::sync::{Arc, Mutex};

pub struct FrameBuffer {
    pub data: Vec<u8>,
    /// Pre-grade original frame for Before/After split view.
    pub original_data: Vec<u8>,
    /// Pre-grade decoded frame — used by UI thread to re-grade without engine round-trip.
    /// Shared via Arc to avoid expensive clones on the hot playback path.
    pub pregrade_data: Arc<Vec<u8>>,
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
    /// Render cache status per clip: (clip_id, cached_frame_count, total_frame_count).
    /// Updated periodically by the engine for timeline visualization.
    pub render_cache_ranges: Vec<(klipp_core::ClipId, u32, u32)>,
    // ── Double-buffer: engine writes to pending, UI swaps into data ──
    pub pending_data: Vec<u8>,
    pub pending_width: u32,
    pub pending_height: u32,
    pub pending_generation: u64,
    pub pending_ready: bool,
}

impl FrameBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            data: vec![0u8; (width * height * 4) as usize],
            original_data: Vec::new(),
            pregrade_data: Arc::new(Vec::new()),
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
            render_cache_ranges: Vec::new(),
            pending_data: Vec::new(),
            pending_width: 0,
            pending_height: 0,
            pending_generation: 0,
            pending_ready: false,
        }
    }

    /// Engine writes to pending buffer — fast, minimal lock time.
    pub fn write_frame(&mut self, rgba_data: Vec<u8>, width: u32, height: u32) {
        self.pending_data = rgba_data;
        self.pending_width = width;
        self.pending_height = height;
        self.pending_generation = self.generation.wrapping_add(1);
        self.pending_ready = true;
    }

    /// UI thread swaps pending into active — fast pointer swap, no allocation.
    pub fn swap_pending(&mut self) -> bool {
        if !self.pending_ready {
            return false;
        }
        std::mem::swap(&mut self.data, &mut self.pending_data);
        self.width = self.pending_width;
        self.height = self.pending_height;
        self.generation = self.pending_generation;
        self.dirty = true;
        self.pending_ready = false;
        true
    }

    pub fn clear(&mut self) {
        self.data.fill(0);
        self.dirty = true;
    }
}

pub type SharedFrameBuffer = Arc<Mutex<FrameBuffer>>;
