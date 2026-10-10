//! A node that didn't draw what it shows, and why (M4 step 10).
//!
//! A plug-in node that can't run passes its input on; the run records it
//! here, so the viewer can say so and an export can refuse to write a
//! picture that is not the one shown. A node the person switched off, and
//! a node that ran with what it had by design (a frame padded to a size
//! frei0r takes, a denoise at the media's first frame with no frame
//! before it), are recorded too but are not failures.

use klipp_core::ClipId;

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

    fn kind_label(&self) -> &'static str {
        match self.kind {
            NodeKind::Frei0r => "frei0r",
            _ => "node",
        }
    }

    /// The viewer's words: "frei0r Delay0r: needs a frame size under 2048".
    pub fn plain(&self) -> String {
        format!("{} {}: {}", self.kind_label(), self.name, self.reason)
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

/// A frei0r effect's name from its id (`frei0r:<path>:<name>`), for a
/// plug-in that isn't there to ask.
pub fn frei0r_name(type_id: &str) -> String {
    type_id.rsplit(':').next().filter(|n| !n.is_empty()).unwrap_or(type_id).to_owned()
}
