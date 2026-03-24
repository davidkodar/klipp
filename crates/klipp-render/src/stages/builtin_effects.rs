use crate::stage::{FrameContext, ProcessingStage};

/// Processing stage that applies built-in GPU/CPU effects (blur, crop, transform, etc.).
/// Stateless — all effect data comes from the clip snapshot.
pub struct BuiltinEffectsStage;

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

        klipp_effects::apply_effects(&clip.builtin_effects, frame, width, height)
    }
}
