//! Scenes of several title layers, kept while nothing in them moves.
//!
//! A title of several layers (a bar and its words, a card) is composed
//! on the GPU one layer at a time: each placed, dressed, faded and laid
//! over the ones below it. Between its entrance and its exit nothing in
//! a still title changes, and that work came to 45 to 60 ms a frame on
//! the Apple GPU at 1080p, for the same picture each time. The composed
//! scene is kept here instead, and each later frame at rest copies it
//! back into its layer.
//!
//! Only the scene is kept: the clip's transform, its effects, its
//! contrast protection and its opacity act on that layer after it, at
//! each frame, as before.

use std::sync::Arc;

use klipp_core::WorkingColorspace;
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_timeline::TitleData;

/// At most this many scenes are kept: one per still title on screen,
/// with room for the few the playhead goes back and forth between.
const MOST: usize = 8;

/// And at most this many bytes of them. A scene is a whole layer, four
/// 32-bit floats a pixel: 33 MB at 1080p, 133 MB at 4K, so this keeps
/// three at 4K.
const BUDGET: u64 = 400 << 20;

struct Kept {
    title: TitleData,
    /// Its image layers' pictures, by the id each was decoded under (its
    /// path and the file's stamp).
    images: Vec<Option<u64>>,
    width: u32,
    height: u32,
    working: WorkingColorspace,
    buffer: Arc<wgpu::Buffer>,
}

impl Kept {
    fn is(&self, title: &TitleData, images: &[Option<u64>], width: u32, height: u32, working: WorkingColorspace) -> bool {
        self.width == width && self.height == height && self.working == working && self.images == images && self.title == *title
    }
}

/// The kept scenes, newest used first, on the device they were made on.
#[derive(Default)]
pub(crate) struct StillTitleScenes {
    device: Option<Arc<wgpu::Device>>,
    /// Font fetches finished when these were drawn: a face that arrived
    /// since may change any of them.
    fonts: u64,
    kept: Vec<Kept>,
}

impl StillTitleScenes {
    /// Drops every scene made on another device or a lost one, or drawn
    /// before a face arrived.
    fn current(&mut self, gpu: &GpuEffectsPipeline) {
        let device = gpu.device();
        let fonts = klipp_title::catalogue::fetches();
        let lost = gpu.is_device_lost();
        if lost || fonts != self.fonts || self.device.as_ref().is_none_or(|d| !Arc::ptr_eq(d, &device)) {
            self.kept.clear();
            self.device = (!lost).then_some(device);
            self.fonts = fonts;
        }
    }

    /// Layer `slot` made the scene kept for `title` with these pictures,
    /// at this size and working space. `false`, and the layer as it was,
    /// when none is kept.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(&mut self, gpu: &mut GpuEffectsPipeline, title: &TitleData, images: &[Option<u64>], slot: usize, width: u32, height: u32, working: WorkingColorspace) -> bool {
        self.current(gpu);
        let Some(i) = self.kept.iter().position(|k| k.is(title, images, width, height, working)) else {
            return false;
        };
        let kept = self.kept.remove(i);
        if !gpu.stack_layer_restore(slot, &kept.buffer) {
            return false;
        }
        self.kept.insert(0, kept);
        true
    }

    /// Keeps layer `slot`, the scene just composed for `title` with these
    /// pictures.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn keep(&mut self, gpu: &mut GpuEffectsPipeline, title: &TitleData, images: Vec<Option<u64>>, slot: usize, width: u32, height: u32, working: WorkingColorspace) {
        self.current(gpu);
        let Some(buffer) = gpu.stack_layer_keep(slot) else { return };
        if buffer.size() > BUDGET {
            return;
        }
        self.kept.retain(|k| !k.is(title, &images, width, height, working));
        self.kept.insert(0, Kept { title: title.clone(), images, width, height, working, buffer });
        while self.kept.len() > MOST || self.bytes() > BUDGET {
            self.kept.pop();
        }
    }

    /// The bytes the kept scenes hold on the GPU.
    pub(crate) fn bytes(&self) -> u64 {
        self.kept.iter().map(|k| k.buffer.size()).sum()
    }
}
