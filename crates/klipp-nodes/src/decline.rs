//! A node that didn't draw what it shows, and why (M4 step 10).
//!
//! A node that can't run passes its input on; the run records it here, so
//! the viewer can say so and an export can refuse to write a picture that
//! is not the one shown. The plug-in nodes record their refusals (frei0r,
//! OFX), and every node that reads the clip's source at other times (the
//! frame blend, temporal noise reduction, a temporal OFX plug-in) records
//! a frame it couldn't read. A node the person switched off, and a node
//! that ran with what it had by design (a frame padded to a size frei0r
//! takes, a denoise at the media's first frame with no frame before it),
//! are recorded too but are not failures.

use klipp_core::ClipId;

use crate::frame_context::Missing;
use crate::{NodeId, NodeKind};

/// Why a node didn't draw as asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclineOutcome {
    /// Switched off: the input passed on, as asked.
    Bypass,
    /// Ran with what it had, by design: a padded frame, a neighbour past
    /// the media's real edge.
    Fallback,
    /// A neighbouring frame inside the media couldn't be read.
    NeighbourMissing,
    /// The plug-in isn't installed.
    PluginMissing,
    /// The plug-in can't take this frame (its size, its history).
    Unsupported,
    /// The plug-in failed: no instance, or its render failed.
    RenderFailed,
    /// A file the node reads (a LUT's) couldn't be read.
    FileMissing,
}

impl DeclineOutcome {
    /// Whether a picture with this decline in it is not the one asked for:
    /// an export refuses to write it.
    pub fn fails(self) -> bool {
        !matches!(self, Self::Bypass | Self::Fallback)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bypass => "bypassed",
            Self::Fallback => "fallback",
            Self::NeighbourMissing => "neighbour frame missing",
            Self::PluginMissing => "plug-in missing",
            Self::Unsupported => "unsupported",
            Self::RenderFailed => "render failed",
            Self::FileMissing => "file missing",
        }
    }
}

/// One node's decline in a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decline {
    /// The clip whose grade ran it: set by the render, `None` from the
    /// executor alone.
    pub clip: Option<ClipId>,
    pub node: NodeId,
    pub kind: NodeKind,
    pub outcome: DeclineOutcome,
    /// The effect as the person knows it: the plug-in's name.
    pub name: String,
    /// Why, in plain words: "needs a frame size under 2048".
    pub reason: String,
    /// The detail, for the log and an export's error.
    pub why: String,
}

impl Decline {
    pub fn fails(&self) -> bool {
        self.outcome.fails()
    }

    /// Whether the viewer names it: a node switched off is as asked.
    pub fn shown(&self) -> bool {
        self.outcome != DeclineOutcome::Bypass
    }

    /// What kind of plug-in it is, before its name; none for a built-in
    /// effect, whose name says what it is.
    fn kind_label(&self) -> Option<&'static str> {
        match self.kind {
            NodeKind::Frei0r => Some("frei0r"),
            NodeKind::Ofx => Some("OFX"),
            NodeKind::Effect | NodeKind::EffectSegment => None,
            _ => Some("node"),
        }
    }

    /// The viewer's words: "frei0r Delay0r: needs a frame size under 2048",
    /// "Temporal Noise Reduction: no frame past the media's edge".
    pub fn plain(&self) -> String {
        match self.kind_label() {
            Some(kind) => format!("{kind} {}: {}", self.name, self.reason),
            None => format!("{}: {}", self.name, self.reason),
        }
    }
}

impl std::fmt::Display for Decline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}; node {}: {})", self.plain(), self.outcome.label(), self.node, self.why)
    }
}

/// The outcome a frei0r refusal declines with, and its reason in plain
/// words.
pub fn frei0r_outcome(refusal: &klipp_frei0r::Refusal) -> (DeclineOutcome, &'static str) {
    use klipp_frei0r::Refusal;
    match refusal {
        Refusal::TemporalAboveMax { .. } => (DeclineOutcome::Unsupported, "needs a frame size under 2048"),
        Refusal::EmptyFrame => (DeclineOutcome::Unsupported, "can't render an empty frame"),
        Refusal::Construct(_) => (DeclineOutcome::RenderFailed, "the plug-in wouldn't start"),
        Refusal::BufferSize { .. } => (DeclineOutcome::RenderFailed, "the plug-in was handed a wrong-sized frame"),
    }
}

/// The outcome a node that reads the clip's source at other times declines
/// with, given the frames it couldn't read, and its reason in plain words:
/// `None` when it read them all. Past the media's edge is a fallback (the
/// node ran with the frames there are, as it always will there); a frame
/// inside the media unread, of the wrong size, or over the node's budget
/// is not the picture asked for.
pub fn neighbour_outcome(missed: &[Missing]) -> Option<(DeclineOutcome, &'static str)> {
    let worst = missed.iter().find(|m| **m != Missing::OutOfRange).or(missed.first())?;
    Some(match worst {
        Missing::OutOfRange => (DeclineOutcome::Fallback, "no frame past the media's edge"),
        Missing::Unavailable => (DeclineOutcome::NeighbourMissing, "a frame it reads couldn't be decoded"),
        Missing::WrongSize { .. } => (DeclineOutcome::NeighbourMissing, "a frame it reads came at another size"),
        Missing::OverBudget => (DeclineOutcome::NeighbourMissing, "it asked for more frames than it may read"),
    })
}

/// The outcome an OFX node's refusal declines with, and its reason in
/// plain words.
pub fn ofx_outcome(refusal: &crate::plugin::Refusal) -> (DeclineOutcome, &'static str) {
    use crate::plugin::Refusal;
    match refusal {
        Refusal::Missing => (DeclineOutcome::PluginMissing, "plug-in missing"),
        Refusal::Unsupported(_) => (DeclineOutcome::Unsupported, "can't run here"),
        Refusal::Failed(_) => (DeclineOutcome::RenderFailed, "its render failed"),
    }
}

/// A built-in effect's name as the browser shows it.
pub fn effect_name(effect: &klipp_effects::BuiltinEffect) -> String {
    let kind = std::mem::discriminant(effect);
    klipp_effects::registry::effect_types()
        .find(|t| std::mem::discriminant(&(t.sample)()) == kind)
        .map_or_else(|| "effect".to_owned(), |t| t.desc.name.to_owned())
}

/// A frei0r effect's name from its id (`frei0r:<path>:<name>`), for a
/// plug-in that isn't there to ask.
pub fn frei0r_name(type_id: &str) -> String {
    type_id.rsplit(':').next().filter(|n| !n.is_empty()).unwrap_or(type_id).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_context::Missing;

    /// Past the media's edge is a fallback; any frame inside it unread is
    /// a frame missing, and it outweighs an edge in the same node.
    #[test]
    fn a_neighbour_past_the_edge_falls_back_and_one_inside_is_missing() {
        assert_eq!(neighbour_outcome(&[]), None);
        assert_eq!(neighbour_outcome(&[Missing::OutOfRange]).map(|(o, _)| o), Some(DeclineOutcome::Fallback));
        for m in [Missing::Unavailable, Missing::WrongSize { width: 2, height: 2 }, Missing::OverBudget] {
            assert_eq!(neighbour_outcome(&[m]).map(|(o, _)| o), Some(DeclineOutcome::NeighbourMissing), "{m:?}");
            assert_eq!(neighbour_outcome(&[Missing::OutOfRange, m]).map(|(o, _)| o), Some(DeclineOutcome::NeighbourMissing), "{m:?} with an edge");
        }
        assert!(!DeclineOutcome::Fallback.fails() && DeclineOutcome::NeighbourMissing.fails());
    }

    /// An OFX refusal's outcome, and the viewer's words: a plug-in's kind
    /// before its name; a built-in effect by its name alone.
    #[test]
    fn ofx_refusals_and_the_viewers_words() {
        use crate::plugin::Refusal;
        assert_eq!(ofx_outcome(&Refusal::Missing).0, DeclineOutcome::PluginMissing);
        assert_eq!(ofx_outcome(&Refusal::Unsupported("context".into())).0, DeclineOutcome::Unsupported);
        assert_eq!(ofx_outcome(&Refusal::Failed(3)).0, DeclineOutcome::RenderFailed);
        let d = |kind, name: &str| Decline { clip: None, node: NodeId::nil(), kind, outcome: DeclineOutcome::Fallback, name: name.into(), reason: "why".into(), why: String::new() };
        assert_eq!(d(NodeKind::Ofx, "org.example.Blur").plain(), "OFX org.example.Blur: why");
        assert_eq!(d(NodeKind::Frei0r, "Delay0r").plain(), "frei0r Delay0r: why");
        let name = effect_name(&klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.1 });
        assert_eq!(d(NodeKind::Effect, &name).plain(), format!("{name}: why"));
        assert_ne!(name, "effect", "the registry names it");
    }
}
