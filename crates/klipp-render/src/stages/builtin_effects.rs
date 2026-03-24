use crate::stage::{FrameContext, ProcessingStage};
use klipp_effects::gpu::GpuEffectsPipeline;

/// Processing stage that applies built-in effects (blur, crop, transform, etc.).
/// Uses GPU (wgpu compute shaders) when available, CPU fallback otherwise.
pub struct BuiltinEffectsStage {
    gpu: Option<GpuEffectsPipeline>,
}

impl BuiltinEffectsStage {
    pub fn new() -> Self {
        let gpu = GpuEffectsPipeline::new();
        if gpu.is_some() {
            tracing::info!("BuiltinEffectsStage: GPU pipeline available");
        }
        Self { gpu }
    }
}

impl Default for BuiltinEffectsStage {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessingStage for BuiltinEffectsStage {
    fn name(&self) -> &'static str {
        "Built-in Effects"
    }

    fn process(
        &mut self,
        frame: Vec<u8>,
        width: u32,
        height: u32,
        ctx: &FrameContext,
    ) -> Vec<u8> {
        let clip = match &ctx.clip {
            Some(c) => c,
            None => return frame,
        };

        if clip.builtin_effects.is_empty() {
            return frame;
        }

        // Prefer GPU path
        if let Some(ref mut gpu) = self.gpu {
            return gpu.apply_effects(&clip.builtin_effects, frame, width, height);
        }

        // CPU fallback
        klipp_effects::apply_effects(&clip.builtin_effects, frame, width, height)
    }
}
