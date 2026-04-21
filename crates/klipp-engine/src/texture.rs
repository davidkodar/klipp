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

    // ── GPU-resident viewer path ──
    // When the engine and UI share one `wgpu::Device` (installed via
    // `GpuContext::install_shared`), the grade chain can encode its
    // final sRGB-u8 output directly into this texture via
    // `GpuEffectsPipeline::encode_grade_to_texture`. The UI then
    // samples it through an egui `TextureId` registered on
    // `egui_wgpu::Renderer::register_native_texture` — zero PCIe
    // download on the present path.
    //
    // `None` means either the shared device wasn't installed or the
    // engine is in a state where the GPU path isn't active; the UI
    // falls back to the existing `data: Vec<u8>` upload.
    //
    // Format is always `Rgba8Unorm` (not sRGB — the shader output
    // bytes are already sRGB-encoded, so the texture stores them
    // byte-for-byte). `gpu_output_generation` bumps whenever the
    // texture's backing `wgpu::Texture` is recreated (first use or
    // size change); the UI uses that to decide whether to
    // re-register with egui.
    pub gpu_output: Option<std::sync::Arc<wgpu::Texture>>,
    pub gpu_output_width: u32,
    pub gpu_output_height: u32,
    pub gpu_output_generation: u64,
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
            gpu_output: None,
            gpu_output_width: 0,
            gpu_output_height: 0,
            gpu_output_generation: 0,
        }
    }

    /// Ensure `gpu_output` is a `wgpu::Texture` of at least
    /// `(width, height)` bytes on the supplied device. Creates the
    /// texture on first call or when dimensions change; the existing
    /// handle is reused otherwise.
    ///
    /// Returns a cheap `Arc` clone of the current texture handle so
    /// the caller (engine post_process) can hand it to
    /// `GpuEffectsPipeline::encode_grade_to_texture` without holding
    /// the FrameBuffer lock across the submit.
    ///
    /// Width must satisfy
    /// `width * 4 % COPY_BYTES_PER_ROW_ALIGNMENT == 0` (256) —
    /// same constraint `encode_grade_to_texture` enforces. Real
    /// viewer frames are ≥ 1920 px wide so this holds by
    /// construction.
    pub fn ensure_gpu_output(
        &mut self,
        device: &std::sync::Arc<wgpu::Device>,
        width: u32,
        height: u32,
    ) -> Option<std::sync::Arc<wgpu::Texture>> {
        if width == 0 || height == 0 {
            return None;
        }
        let reuse = matches!(
            &self.gpu_output,
            Some(_) if self.gpu_output_width == width && self.gpu_output_height == height
        );
        if !reuse {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("klipp_gpu_viewer_output"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.gpu_output = Some(std::sync::Arc::new(tex));
            self.gpu_output_width = width;
            self.gpu_output_height = height;
            self.gpu_output_generation = self.gpu_output_generation.wrapping_add(1);
        }
        self.gpu_output.as_ref().map(std::sync::Arc::clone)
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

    /// Write a skim preview frame *and* clear the GPU scope texture.
    ///
    /// The UI dispatches `AppCommand::RenderScopesForFrame` right after this
    /// call (see `app.rs` — `browser_skim_pending` handler), so the engine
    /// refills `scope_texture` with a fresh GPU-rendered scope within ~1
    /// tick. We deliberately do NOT clear `scope_texture` here: the stale
    /// playback scope stays visible during the handful of milliseconds
    /// before the new one lands, which avoids the CPU-fallback flicker the
    /// previous clear caused when the user moved the cursor quickly over a
    /// clip (gap → CPU render → engine refill → swap, repeated per skim
    /// frame). For modes without a GPU kernel (e.g. vectorscope) the engine
    /// explicitly clears `scope_texture` so the UI still falls back to CPU
    /// in that branch.
    pub fn write_skim_frame(&mut self, rgba_data: Vec<u8>, width: u32, height: u32) {
        self.write_frame(rgba_data, width, height);
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

    /// Borrowed view of the currently-displayed frame as a `FrameRef`.
    /// Always returns `Cpu` today; phase 3 of the GPU-residency plan
    /// will return `Gpu` when NVDEC produces GPU-resident frames
    /// directly. Callers that don't need raw `data` access should
    /// prefer this accessor — it's the seam the rest of the plan
    /// migrates to.
    pub fn frame_ref(&self) -> crate::frame::FrameRef<'_> {
        crate::frame::FrameRef::Cpu(crate::frame::CpuFrameRef {
            data: &self.data,
            width: self.width,
            height: self.height,
        })
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
    fn write_skim_frame_preserves_gpu_scope_texture() {
        // The CURRENT invariant (committed in f5d11d6 "Match scope
        // quality during skim to playback"): write_skim_frame must
        // NOT clear the previous GPU scope texture. The UI keeps
        // displaying the stale scope for the handful of ms before
        // the fresh scope lands, which avoids the CPU-fallback
        // flicker the old clear-on-skim path caused when the user
        // moved the cursor quickly over a clip. A fresh scope is
        // then dispatched by the app via AppCommand::RenderScopesForFrame
        // (see app.rs — browser_skim_pending handler) against the
        // same RGBA, so the skim scope ends up matching playback
        // pixel-for-pixel.
        //
        // The PREVIOUS invariant (clear on skim) is intentionally
        // gone; keep this test to prevent a regression that
        // re-introduces the flicker.
        let mut fb = FrameBuffer::new(4, 4);
        fb.scope_texture = vec![99u8; 720 * 256 * 4];
        fb.scope_texture_w = 720;
        fb.scope_texture_h = 256;
        fb.scope_texture_mode = 0; // waveform

        let skim_rgba = vec![42u8; 4 * 4 * 4];
        fb.write_skim_frame(skim_rgba, 4, 4);

        // Pending frame swapped in correctly.
        assert!(fb.pending_ready, "pending frame should be queued");
        assert_eq!(fb.pending_width, 4);
        assert_eq!(fb.pending_height, 4);

        // Scope texture must be PRESERVED so the UI can keep
        // drawing the previous scope until the fresh one arrives.
        assert_eq!(
            fb.scope_texture.len(), 720 * 256 * 4,
            "GPU scope texture must survive skim write — clearing it caused flicker"
        );
        assert_eq!(fb.scope_texture_w, 720);
        assert_eq!(fb.scope_texture_h, 256);
        assert_eq!(fb.scope_texture_mode, 0);
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
