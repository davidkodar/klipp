use std::sync::{Arc, Mutex};

pub struct FrameBuffer {
    pub data: Vec<u8>,
    /// Pre-grade original frame for Before/After split view.
    pub original_data: Vec<u8>,
    /// Frame rendered with VideoStabilization bypassed, for stabilization compare
    /// split view. Empty when compare mode is off.
    pub unstab_data: Vec<u8>,
    /// Frame rendered with the user-selected effect bypassed, for the
    /// per-effect split compare view. Same single-write pattern as
    /// `unstab_data` — the engine writes it on each render when the
    /// compare mode is active, and clears it to an empty Vec when
    /// inactive to free memory.
    pub compare_effect_data: Vec<u8>,
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
            unstab_data: Vec::new(),
            compare_effect_data: Vec::new(),
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

    /// Swap-based write — the engine provides a `&mut Vec<u8>` that is swapped
    /// with `pending_data`. After the call, `rgba_data` contains the previous
    /// `pending_data` buffer (which the engine can clear and reuse next frame,
    /// eliminating per-frame allocations). Same semantics as `write_frame`
    /// otherwise — the pending buffer becomes the engine's newest composite.
    pub fn write_frame_swap(&mut self, rgba_data: &mut Vec<u8>, width: u32, height: u32) {
        std::mem::swap(&mut self.pending_data, rgba_data);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_frame_swap_exchanges_buffers() {
        let mut fb = FrameBuffer::new(4, 4);
        // Pre-seed pending_data with a marker
        fb.pending_data = vec![0xAA; 4 * 4 * 4];

        let mut engine_buf = vec![0x55; 4 * 4 * 4];
        fb.write_frame_swap(&mut engine_buf, 4, 4);

        // engine_buf now holds the old pending_data (0xAA)
        assert!(engine_buf.iter().all(|&b| b == 0xAA));
        // pending_data now holds what engine wrote (0x55)
        assert!(fb.pending_data.iter().all(|&b| b == 0x55));
        assert!(fb.pending_ready);
    }

    #[test]
    fn write_frame_swap_preserves_capacity_for_reuse() {
        let mut fb = FrameBuffer::new(4, 4);
        fb.pending_data = Vec::with_capacity(1000);
        fb.pending_data.resize(64, 0); // len=64, capacity=1000

        let mut engine_buf = vec![1u8; 64];
        let engine_cap_before = engine_buf.capacity();

        fb.write_frame_swap(&mut engine_buf, 4, 4);

        // engine_buf now has capacity from the original pending_data (1000)
        assert!(engine_buf.capacity() >= 1000);
        // pending_data has the original engine capacity
        assert_eq!(fb.pending_data.capacity(), engine_cap_before);
    }
}
