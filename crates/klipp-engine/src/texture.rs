use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One rendered scope texture, keyed by mode in [`FrameBuffer::scope_textures`].
///
/// The map lets the engine publish multiple scope outputs in parallel
/// per tick — required for the floating multi-scope window where each
/// pane shows a different mode. `generation` bumps whenever the entry
/// is rewritten so the UI can short-circuit re-upload of unchanged
/// textures (same shape as `FrameBuffer::generation`).
#[derive(Clone)]
pub struct ScopeTextureEntry {
    pub data: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub generation: u64,
}

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
    /// While a CIE chromaticity scope is open: the picture as the renderer
    /// made it — linear light, in the project's working space — one RGB
    /// triple per `cie_samples_w * cie_samples_h` grid point. Empty
    /// otherwise.
    ///
    /// A chromaticity diagram is absolute, so it has to measure the picture
    /// and not the view of it: `data` has been through the display
    /// transform, which in a wide-gamut project has already mapped the
    /// gamut, and plotting that would draw the compression rather than the
    /// colour. See [`crate::ffmpeg_scope::CIE_SAMPLES_MODE`].
    pub cie_samples: Vec<f32>,
    pub cie_samples_w: u32,
    pub cie_samples_h: u32,
    /// While HDR monitoring is on: the picture's light in cd/m², one
    /// sample per `nit_map_step` pixels, of the brightest channel — what
    /// the readout under the pointer shows. Empty otherwise.
    pub nit_map: Vec<f32>,
    pub nit_map_w: u32,
    pub nit_map_h: u32,
    pub nit_map_step: u32,
    /// GPU-rendered scope texture (RGBA) — ready for display.
    /// Legacy singular-scope path: holds whichever mode the main dock
    /// most recently requested. Kept for back-compat with the in-tree
    /// GPU/CPU path readers; new code should prefer `scope_textures`.
    pub scope_texture: Vec<u8>,
    pub scope_texture_w: u32,
    pub scope_texture_h: u32,
    /// Which scope mode was rendered (0=waveform, 1=parade, 2=vectorscope, 3=histogram).
    pub scope_texture_mode: u8,
    /// Per-mode rendered scope textures, keyed by mode byte. Engine
    /// populates one entry per mode in `active_scope_modes` each tick.
    /// Main dock reads its single active mode; floating multi-scope
    /// window panes each read their own mode. Replaces the per-pane
    /// in-UI-thread scope rendering that bypassed the engine entirely.
    pub scope_textures: HashMap<u8, ScopeTextureEntry>,
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
    /// Where on the timeline the frame in `data` is — what is on screen.
    /// Written with the frame, so a reader holding the lock cannot pair a
    /// frame with another one's position (during playback the playhead
    /// moves on while a frame is still being shown).
    pub presented: Option<klipp_core::RationalTime>,
    pending_presented: Option<klipp_core::RationalTime>,

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

    /// Per-node output thumbnails for the V2 canvas, keyed by node
    /// id. Each entry is a ~96×54 sRGB u8 RGBA buffer. Populated by
    /// the engine on paused / post-seek re-grades when the UI has
    /// sent a non-empty `SetVisibleNodeThumbs` list. `node_thumbs_gen`
    /// bumps whenever the map changes so the V2 panel can skip
    /// texture re-uploads between no-op updates.
    pub node_thumbs: std::collections::HashMap<klipp_nodes::NodeId, Vec<u8>>,
    pub node_thumbs_gen: u64,
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
            scope_textures: HashMap::new(),
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
            cie_samples: Vec::new(),
            cie_samples_w: 0,
            cie_samples_h: 0,
            nit_map: Vec::new(),
            nit_map_w: 0,
            nit_map_h: 0,
            nit_map_step: 1,
            presented: None,
            pending_presented: None,
            gpu_output: None,
            gpu_output_width: 0,
            gpu_output_height: 0,
            gpu_output_generation: 0,
            node_thumbs: std::collections::HashMap::new(),
            node_thumbs_gen: 0,
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
        self.pending_generation = self.next_pending_generation();
        self.pending_ready = true;
    }

    /// Generation for a newly written pending frame: one past the newest
    /// frame *either* buffer holds.
    ///
    /// It used to be `generation + 1`, which gave every frame written
    /// between two UI swaps the same number — so a second write while the
    /// window was idle (the paused-regrade case) was indistinguishable
    /// from the first. Anything waiting for "a frame newer than N" could
    /// never see it. Strictly monotonic per write fixes that and changes
    /// nothing for consumers that only compare for inequality.
    fn next_pending_generation(&self) -> u64 {
        let newest = if self.pending_ready {
            self.generation.max(self.pending_generation)
        } else {
            self.generation
        };
        newest.wrapping_add(1)
    }

    /// Generation of the newest frame the engine has produced, whether or
    /// not the UI has swapped it in yet.
    pub fn latest_generation(&self) -> u64 {
        if self.pending_ready {
            self.pending_generation
        } else {
            self.generation
        }
    }

    /// The newest frame the engine has produced: the queued pending frame
    /// if there is one, else the one the UI last swapped in. While paused
    /// the UI repaints rarely, so `data` alone can be several frames stale.
    pub fn latest_frame(&self) -> (&[u8], u32, u32, u64) {
        if self.pending_ready {
            (&self.pending_data, self.pending_width, self.pending_height, self.pending_generation)
        } else {
            (&self.data, self.width, self.height, self.generation)
        }
    }

    /// The picture's light in cd/m² at `(nx, ny)` — both in `0..=1` over
    /// the frame — from [`Self::nit_map`]. `None` unless HDR monitoring is
    /// on, which is the only thing that fills it.
    pub fn nits_at(&self, nx: f32, ny: f32) -> Option<f32> {
        if self.nit_map.is_empty() || self.nit_map_w == 0 || self.nit_map_h == 0 {
            return None;
        }
        let x = ((nx.clamp(0.0, 1.0) * (self.nit_map_w - 1) as f32).round() as u32).min(self.nit_map_w - 1);
        let y = ((ny.clamp(0.0, 1.0) * (self.nit_map_h - 1) as f32).round() as u32).min(self.nit_map_h - 1);
        self.nit_map.get((y * self.nit_map_w + x) as usize).copied()
    }

    /// Where on the timeline [`Self::latest_frame`] is.
    pub fn latest_presented(&self) -> Option<klipp_core::RationalTime> {
        if self.pending_ready { self.pending_presented } else { self.presented }
    }

    /// Say which frame the next write is of. The engine calls this with
    /// every frame it publishes.
    pub fn set_presented(&mut self, at: klipp_core::RationalTime) {
        self.pending_presented = Some(at);
        self.presented = Some(at);
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
        self.pending_generation = self.next_pending_generation();
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
        self.presented = self.pending_presented;
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
    /// Two frames written before the UI swaps must get distinct, rising
    /// generations. With `generation + 1` both got the same number, so a
    /// waiter could never tell the second (the one reflecting an edit)
    /// from the first.
    #[test]
    fn frames_written_between_swaps_get_distinct_generations() {
        let mut fb = FrameBuffer::new(2, 2);
        let start = fb.latest_generation();
        fb.write_frame(vec![1; 16], 2, 2);
        let first = fb.latest_generation();
        fb.write_frame(vec![2; 16], 2, 2);
        let second = fb.latest_generation();
        assert!(first > start, "first write must advance the generation");
        assert!(second > first, "second write before a swap must advance it again");

        // And the newest frame is the one reported, pixels and all.
        let (data, w, h, gen) = fb.latest_frame();
        assert_eq!((w, h, gen), (2, 2, second));
        assert_eq!(data[0], 2, "latest_frame must return the newest pending pixels");

        // After the swap the numbers keep rising from there.
        assert!(fb.swap_pending());
        assert_eq!(fb.latest_generation(), second);
        fb.write_frame(vec![3; 16], 2, 2);
        assert!(fb.latest_generation() > second);
    }

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
