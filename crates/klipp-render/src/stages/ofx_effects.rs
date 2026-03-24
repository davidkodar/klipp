use crate::stage::{FrameContext, ProcessingStage};
use klipp_plugin::OfxHostRuntime;

/// Processing stage that applies OFX plugin effects.
/// Owns its own OfxHostRuntime instance — not shared with the playback engine.
pub struct OFXEffectsStage {
    host: OfxHostRuntime,
}

impl Default for OFXEffectsStage {
    fn default() -> Self {
        Self::new()
    }
}

impl OFXEffectsStage {
    pub fn new() -> Self {
        Self {
            host: OfxHostRuntime::new(),
        }
    }
}

impl ProcessingStage for OFXEffectsStage {
    fn name(&self) -> &'static str {
        "OFX Effects"
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

        if clip.effects.is_empty() {
            return frame;
        }

        self.host
            .apply_effects_chain(&clip.effects, &frame, width, height, ctx.clip_time_secs)
    }
}
