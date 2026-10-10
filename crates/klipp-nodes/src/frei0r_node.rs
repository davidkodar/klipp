//! A frei0r node's instances, kept across frames (M4 step 5).
//!
//! A frei0r instance is bound to one frame size and may keep history: a
//! delay, a feedback, an accumulation (`klipp_frei0r::temporal`). The
//! render's [`PluginInstances`] owner keeps one [`Frei0rSlot`] per clip and
//! node; [`render`] decides from the slot's last stamp whether the history
//! it holds is this frame's to build on, and starts it again when not:
//!
//! | Since the slot's last frame | |
//! |---|---|
//! | 1 to [`CONTINUATION_FRAMES`] frames on | play: carried |
//! | further on, or any step back | a seek: started again |
//! | the same frame, same parameters and picture | a repaint: the last output, nothing run |
//! | the same frame, anything changed | started again, then rendered |
//! | another size, another source, the plug-in changed | started again |
//! | the node was switched off in between | started again |
//!
//! A repaint must not run the plug-in: a history plug-in would advance
//! and the paused picture would change at rest. An edit while paused shows
//! the plug-in's first-frame picture, the same every time.

use std::sync::Arc;

use klipp_core::ClipId;
use klipp_frei0r::{Fallback, Frei0rInstanceSet, Frei0rPlugin, Frei0rValue, Refusal, Rendered};

use crate::frame_context::{PluginInstances, SlotBytes, SlotStamp, CONTINUATION_FRAMES};
use crate::NodeId;

/// What a plug-in is, as far as its instances' state goes: a reloaded or
/// updated plug-in, or one whose parameters changed shape, starts again.
#[derive(Debug, Clone, PartialEq)]
struct PluginStamp {
    path: std::path::PathBuf,
    name: String,
    version: (i32, i32),
    params: Vec<klipp_frei0r::ParamKind>,
}

impl PluginStamp {
    fn of(plugin: &Frei0rPlugin) -> Self {
        let info = &plugin.info;
        Self {
            path: info.path.clone(),
            name: info.name.clone(),
            version: (info.major_version, info.minor_version),
            params: info.params.iter().map(|p| p.kind).collect(),
        }
    }
}

/// The frame last rendered, for a repaint.
struct Memo {
    params: Vec<Frei0rValue>,
    input: Vec<u8>,
    output: Rendered,
}

/// One frei0r node's instances on one clip, in a render's
/// [`PluginInstances`].
#[derive(Default)]
pub struct Frei0rSlot {
    set: Option<(Frei0rInstanceSet, PluginStamp)>,
    memo: Option<Memo>,
}

impl SlotBytes for Frei0rSlot {
    fn bytes(&self) -> usize {
        let set = self.set.as_ref().map_or(0, |(s, _)| s.bytes());
        let memo = self.memo.as_ref().map_or(0, |m| m.input.len() + m.output.rgba.len());
        set + memo
    }
}

impl Frei0rSlot {
    /// Forget everything kept: the next frame starts again.
    fn clear(&mut self) {
        self.set = None;
        self.memo = None;
    }
}

/// Whether the history a slot holds, last stamped `last`, is `now`'s to
/// build on (see the module's table; the same frame is the memo's).
fn continues(last: &SlotStamp, now: &SlotStamp) -> bool {
    let step = now.frame - last.frame;
    (0..=CONTINUATION_FRAMES).contains(&step)
        && (last.width, last.height) == (now.width, now.height)
        && last.source == now.source
        && last.enabled
}

/// Render one frame of the frei0r `node` on `clip`: `rgba` (sRGB RGBA8,
/// `now.width × now.height`) through `plugin` with `params`, at the clip's
/// time. Kept across frames in `instances`; see the module's table.
pub fn render(
    instances: &mut PluginInstances,
    clip: ClipId,
    node: NodeId,
    now: SlotStamp,
    plugin: &Arc<Frei0rPlugin>,
    params: &[Frei0rValue],
    rgba: &[u8],
) -> Result<Rendered, Refusal> {
    let slot = instances.slot(clip, node, now, Frei0rSlot::default);
    let (frame, last) = (slot.instance, slot.last);
    let stamp = PluginStamp::of(plugin);
    let same_plugin = frame.set.as_ref().is_some_and(|(_, s)| *s == stamp);
    let carried = same_plugin && last.as_ref().is_some_and(|last| continues(last, &now));

    if carried && last.is_some_and(|last| last.frame == now.frame) {
        // A repaint of the frame last rendered: the same output, nothing
        // run. Anything changed at this frame starts again.
        if let Some(memo) = frame.memo.as_ref().filter(|m| m.params == params && m.input == rgba) {
            return Ok(memo.output.clone());
        }
        frame.clear();
    } else if !carried {
        frame.clear();
    }

    if frame.set.is_none() {
        frame.set = Some((Frei0rInstanceSet::new(Arc::clone(plugin), now.width, now.height)?, stamp));
    }
    let (set, _) = frame.set.as_mut().expect("made above");
    set.set_params(params);
    let output = match set.render(now.clip_time_secs, rgba) {
        Ok(output) => output,
        Err(refusal) => {
            frame.clear();
            return Err(refusal);
        }
    };
    frame.memo = Some(Memo { params: params.to_vec(), input: rgba.to_vec(), output: output.clone() });
    Ok(output)
}

/// A frei0r node switched off this frame: its slot is marked so, and the
/// next frame it renders starts again (its history went stale while off).
pub fn switched_off(instances: &mut PluginInstances, clip: ClipId, node: NodeId, now: SlotStamp) {
    let slot = instances.slot(clip, node, SlotStamp { enabled: false, ..now }, Frei0rSlot::default);
    slot.instance.clear();
}

/// Whether a render fell back (a size frei0r can't take, padded), for the
/// caller to count.
pub fn fell_back(output: &Rendered) -> bool {
    matches!(output.fallback, Some(Fallback::Padded { .. }))
}
