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

/// The viewer's GPU frame, taken from [`FrameBuffer::gpu_output_snapshot`]
/// to be read back without holding the frame-buffer lock.
pub struct GpuOutputSnapshot {
    texture: Arc<wgpu::Texture>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    pub width: u32,
    pub height: u32,
    /// [`FrameBuffer::gpu_output_frame`] when it was taken.
    pub frame: u64,
    /// Where on the timeline the frame is, when the engine said.
    pub presented: Option<klipp_core::RationalTime>,
}

impl GpuOutputSnapshot {
    /// The frame's display-encoded RGBA bytes, `width * height * 4`,
    /// tightly packed: the same bytes a paused frame is published as.
    ///
    /// The copy is queued behind whatever the engine has submitted, so it
    /// reads a whole frame, never one half written.
    pub fn read(&self) -> Result<Vec<u8>, String> {
        let (w, h) = (self.width, self.height);
        let row = w * 4;
        // A copy's rows are 256-byte aligned; they are packed after.
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let size = padded as u64 * h as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("klipp_viewer_readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("klipp_viewer_readback") });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture { texture: &self.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|_| "the viewer readback was dropped".to_string())?
            .map_err(|e| format!("the viewer readback failed: {e}"))?;
        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity((row * h) as usize);
        for r in mapped.chunks_exact(padded as usize) {
            out.extend_from_slice(&r[..row as usize]);
        }
        drop(mapped);
        staging.unmap();
        Ok(out)
    }
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
    /// The pre-grade frame, for the UI thread to re-grade without an engine
    /// round trip — the "before" picture and the scope preview during a
    /// grade drag.
    ///
    /// The frame, not its bytes: [`crate::engine::Pregrade::display`] makes
    /// the sRGB picture on first ask and keeps it, so a paused frame nobody
    /// asks about costs nothing. Ask outside the frame-buffer lock — for an
    /// HDR source the encode is tens of milliseconds at 4K.
    pub pregrade: Option<Arc<crate::engine::Pregrade>>,
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
    /// colour. See [`crate::scopes::CIE_SAMPLES_MODE`].
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
    /// The drawn scopes, keyed by mode byte (0=waveform, 1=parade,
    /// 2=vectorscope, 3=histogram). The engine writes one entry per
    /// mode in `active_scope_modes` each scope pass; the main dock reads
    /// its active mode and each floating pane its own.
    pub scope_textures: HashMap<u8, ScopeTextureEntry>,
    /// Scope textures drawn for an off-screen reader (the agent's `scope`
    /// tool), kept apart from `scope_textures` so measuring a frame never
    /// disturbs what the viewer's own scopes show.
    pub snapshot_scope_textures: HashMap<u8, ScopeTextureEntry>,
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
    /// Whether the frame in `data` shows every clip under the playhead.
    /// `false` while the other clips' streams were still seeking when it
    /// rendered: the engine re-renders once they arrive, and anything
    /// that captures the viewer as a record of the timeline waits for
    /// that.
    pub complete: bool,
    pending_complete: bool,

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
    /// Whether the newest frame the engine has produced is in
    /// `gpu_output` rather than in `data` / `pending_data`: the GPU
    /// stack wrote it there and read nothing back. A frame written as
    /// bytes clears it. `gpu_output_frame` counts those frames.
    pub gpu_output_live: bool,
    pub gpu_output_frame: u64,
    /// Where on the timeline the frame in `gpu_output` is, when it was
    /// written there with no bytes. Kept apart from `presented`, which
    /// names the bytes: a zero-copy frame said nothing about them, and
    /// naming them with its time paired a reader's bytes with another
    /// frame's position.
    gpu_presented: Option<klipp_core::RationalTime>,
    /// The device and queue `gpu_output` lives on, to read it back.
    gpu_output_queue: Option<(Arc<wgpu::Device>, Arc<wgpu::Queue>)>,

    /// Per-node output thumbnails for the V2 canvas, keyed by node
    /// id. Each entry is a ~96×54 sRGB u8 RGBA buffer. Populated by
    /// the engine on paused / post-seek re-grades when the UI has
    /// sent a non-empty `SetVisibleNodeThumbs` list. `node_thumbs_gen`
    /// bumps whenever the map changes so the V2 panel can skip
    /// texture re-uploads between no-op updates.
    pub node_thumbs: std::collections::HashMap<klipp_nodes::NodeId, Vec<u8>>,
    pub node_thumbs_gen: u64,
}

/// The format to sample [`FrameBuffer::gpu_output`] through. The texture
/// holds display-encoded bytes (the GPU stack's display encode), and a
/// sampler reading a plain `Rgba8Unorm` view hands them to egui as if they
/// were linear light; egui's shader then encodes them again, and playback
/// showed every picture a whole sRGB curve too bright and too flat
/// (#455). An sRGB view decodes on sampling, so what egui re-encodes is
/// the picture the engine made.
pub const GPU_VIEWER_SAMPLE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

impl FrameBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            data: vec![0u8; (width * height * 4) as usize],
            original_data: Vec::new(),
            unstab_data: Vec::new(),
            compare_effect_data: Vec::new(),
            pregrade: None,
            scope_data: Vec::new(),
            scope_preview: Vec::new(),
            scope_preview_w: 0,
            scope_preview_h: 0,
            scope_textures: HashMap::new(),
            snapshot_scope_textures: HashMap::new(),
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
            complete: true,
            pending_complete: true,
            gpu_output: None,
            gpu_output_width: 0,
            gpu_output_height: 0,
            gpu_output_generation: 0,
            gpu_output_live: false,
            gpu_presented: None,
            gpu_output_queue: None,
            gpu_output_frame: 0,
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
    /// The texture takes a storage binding as well, for
    /// `GpuEffectsPipeline::encode_chain_to_display`, which stores
    /// texels rather than copying rows and so has no row-pitch
    /// condition on the width.
    /// Allocate (or reuse) the zero-copy viewer texture. Its bytes are
    /// display-encoded: sample it through a [`GPU_VIEWER_SAMPLE_FORMAT`]
    /// view, never the default one.
    pub fn ensure_gpu_output(
        &mut self,
        device: &std::sync::Arc<wgpu::Device>,
        queue: &std::sync::Arc<wgpu::Queue>,
        width: u32,
        height: u32,
    ) -> Option<std::sync::Arc<wgpu::Texture>> {
        if width == 0 || height == 0 {
            return None;
        }
        let reuse = self.gpu_output.is_some() && (self.gpu_output_width == width && self.gpu_output_height == height);
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
                // COPY_SRC so a screenshot can read back what the
                // viewer shows while it plays (#544).
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::STORAGE_BINDING,
                // The stack stores display-encoded bytes through the
                // plain view (a storage view cannot be sRGB); the viewer
                // samples through the sRGB one, which decodes them to the
                // linear light egui's shader expects (#455).
                view_formats: &[GPU_VIEWER_SAMPLE_FORMAT],
            });
            self.gpu_output = Some(std::sync::Arc::new(tex));
            self.gpu_output_queue = Some((Arc::clone(device), Arc::clone(queue)));
            self.gpu_output_width = width;
            self.gpu_output_height = height;
            self.gpu_output_generation = self.gpu_output_generation.wrapping_add(1);
        }
        self.gpu_output.as_ref().map(std::sync::Arc::clone)
    }

    /// The newest frame is in `gpu_output`, written there by the GPU
    /// stack; no bytes accompany it. The UI paints the texture until a
    /// frame of bytes follows.
    pub fn note_gpu_frame(&mut self) {
        self.gpu_output_live = true;
        self.gpu_output_frame = self.gpu_output_frame.wrapping_add(1);
    }

    /// The frame on screen when it is only in `gpu_output`, as it is
    /// while playing: a handle to read it back with
    /// [`GpuOutputSnapshot::read`] after the lock is released. `None`
    /// when the newest frame is in bytes, which [`Self::latest_frame`]
    /// has.
    pub fn gpu_output_snapshot(&self) -> Option<GpuOutputSnapshot> {
        if !self.gpu_output_live {
            return None;
        }
        let (device, queue) = self.gpu_output_queue.clone()?;
        Some(GpuOutputSnapshot {
            texture: Arc::clone(self.gpu_output.as_ref()?),
            device,
            queue,
            width: self.gpu_output_width,
            height: self.gpu_output_height,
            frame: self.gpu_output_frame,
            presented: self.gpu_presented,
        })
    }

    /// Engine writes to pending buffer — fast, minimal lock time.
    pub fn write_frame(&mut self, rgba_data: Vec<u8>, width: u32, height: u32) {
        self.gpu_output_live = false;
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

    /// Where on the timeline [`Self::latest_frame`] is: always the name of
    /// those bytes, never of a zero-copy frame newer than them.
    pub fn latest_presented(&self) -> Option<klipp_core::RationalTime> {
        if self.pending_ready { self.pending_presented } else { self.presented }
    }

    /// Whether [`Self::latest_frame`] shows every clip under its playhead.
    pub fn latest_complete(&self) -> bool {
        if self.pending_ready { self.pending_complete } else { self.complete }
    }

    /// Say which frame was just published. The engine calls this with
    /// every frame, right after writing it; a frame rendered before every
    /// clip's stream had its picture says so with `complete` false.
    ///
    /// A frame written as bytes names those bytes. A zero-copy frame
    /// (after [`Self::note_gpu_frame`]) has no bytes, so it names only
    /// itself ([`Self::shown_presented`]) and leaves the bytes' name as
    /// it was: naming older bytes with its time was how a reader waiting
    /// for "frame n" came away with another frame's picture.
    pub fn set_presented(&mut self, at: klipp_core::RationalTime, complete: bool) {
        if self.gpu_output_live {
            self.gpu_presented = Some(at);
            return;
        }
        self.pending_presented = Some(at);
        self.pending_complete = complete;
        // Nothing queued: the frame already on screen is the one named.
        if !self.pending_ready {
            self.presented = Some(at);
            self.complete = complete;
        }
    }

    /// Where on the timeline the picture on screen is: the zero-copy frame
    /// when the newest is one, else the bytes' ([`Self::latest_presented`]).
    /// For telling someone where the viewer is; a reader of the bytes pairs
    /// them with [`Self::latest_presented`] instead.
    pub fn shown_presented(&self) -> Option<klipp_core::RationalTime> {
        match (self.gpu_output_live, self.gpu_presented) {
            (true, Some(at)) => Some(at),
            _ => self.latest_presented(),
        }
    }

    /// Swap-based write — the engine provides a `&mut Vec<u8>` that is swapped
    /// with `pending_data`. After the call, `rgba_data` contains the previous
    /// `pending_data` buffer (which the engine can clear and reuse next frame,
    /// eliminating per-frame allocations). Same semantics as `write_frame`
    /// otherwise — the pending buffer becomes the engine's newest composite.
    pub fn write_frame_swap(&mut self, rgba_data: &mut Vec<u8>, width: u32, height: u32) {
        self.gpu_output_live = false;
        std::mem::swap(&mut self.pending_data, rgba_data);
        self.pending_width = width;
        self.pending_height = height;
        self.pending_generation = self.next_pending_generation();
        self.pending_ready = true;
    }

    /// Write a skim preview frame. The scope textures are left as they
    /// are: the UI dispatches `AppCommand::RenderScopesForFrame` right
    /// after this call, and the previous scope stays visible for the few
    /// milliseconds until the fresh one lands rather than flickering to
    /// nothing on every skim frame.
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
        self.complete = self.pending_complete;
        self.dirty = true;
        self.pending_ready = false;
        true
    }

    pub fn clear(&mut self) {
        self.data.fill(0);
        self.dirty = true;
    }

    /// Borrowed view of the currently-displayed frame as a `FrameRef`.
    /// Always `Cpu`: this buffer holds the display transform's output,
    /// which is CPU-side by construction. GPU-resident playback does
    /// not pass through here — it renders into the `gpu_output` texture
    /// the UI samples (see [`FrameBuffer::ensure_gpu_output`]).
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
    fn write_skim_frame_preserves_the_scope_textures() {
        // A skim frame must not clear the previous scope: the UI keeps
        // drawing it for the few milliseconds until the fresh one lands,
        // or every skim frame flickers to nothing. A regression here
        // brought that flicker back once (f5d11d6).
        let mut fb = FrameBuffer::new(4, 4);
        fb.scope_textures.insert(0, ScopeTextureEntry { data: vec![99u8; 720 * 256 * 4], w: 720, h: 256, generation: 1 });

        let skim_rgba = vec![42u8; 4 * 4 * 4];
        fb.write_skim_frame(skim_rgba, 4, 4);

        assert!(fb.pending_ready, "pending frame should be queued");
        assert_eq!(fb.pending_width, 4);
        assert_eq!(fb.pending_height, 4);
        let wave = fb.scope_textures.get(&0).expect("the scope survives a skim write");
        assert_eq!((wave.data.len(), wave.w, wave.h), (720 * 256 * 4, 720, 256));
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

    #[test]
    fn a_zero_copy_frame_does_not_rename_the_bytes() {
        let at = |f: i64| klipp_core::RationalTime::from_frames(f, 25);
        let mut fb = FrameBuffer::new(2, 2);
        fb.write_frame(vec![10; 16], 2, 2);
        fb.set_presented(at(10), true);
        // Playing on: frame 60 goes straight to the GPU texture, no bytes.
        fb.note_gpu_frame();
        fb.set_presented(at(60), true);
        let (data, ..) = fb.latest_frame();
        assert_eq!(data[0], 10);
        assert_eq!(fb.latest_presented(), Some(at(10)), "the bytes are still frame 10's");
        assert_eq!(fb.shown_presented(), Some(at(60)), "the screen shows 60");
        // Bytes of 61 arrive: the pair moves together, and they are what is shown.
        fb.write_frame(vec![61; 16], 2, 2);
        fb.set_presented(at(61), true);
        assert_eq!(fb.latest_frame().0[0], 61);
        assert_eq!(fb.latest_presented(), Some(at(61)));
        assert_eq!(fb.shown_presented(), Some(at(61)));
    }

    #[test]
    fn naming_a_queued_frame_does_not_rename_the_one_on_screen() {
        let at = |f: i64| klipp_core::RationalTime::from_frames(f, 25);
        let mut fb = FrameBuffer::new(2, 2);
        fb.write_frame(vec![1; 16], 2, 2);
        fb.set_presented(at(1), true);
        assert!(fb.swap_pending());
        fb.write_frame(vec![2; 16], 2, 2);
        fb.set_presented(at(2), true);
        // The UI has not swapped: `presented` still names `data`.
        assert_eq!(fb.presented, Some(at(1)));
        assert_eq!(fb.latest_presented(), Some(at(2)));
    }
}
