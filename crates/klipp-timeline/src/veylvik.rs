//! A clip shown through a Veylvik composition: the clip names it by id,
//! the project keeps it (`klipp_project::compositions`), and the clip's
//! picture comes from it with the clip's own media as its footage.
//!
//! Every clip has a composition of its own. A copy of a clip, by split,
//! duplicate, paste or a duplicated sequence, names the same one until
//! [`fork_shared_compositions`] gives it a copy; the project does that
//! before it saves, and the Veylvik tab before it opens a clip, so working
//! on one never changes another.

use serde::{Deserialize, Serialize};

use crate::Sequence;

/// The id of a clip's composition; also the name of its folder in the
/// project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CompositionId(pub uuid::Uuid);

impl CompositionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for CompositionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CompositionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Give every clip that shares a composition with one before it a new id
/// of its own, and say which: `(the one it shared, its own)`. The caller
/// copies each composition under its new id. The first clip naming one,
/// in sequence, track and clip order, keeps it; the takes an audition
/// holds count as clips.
pub fn fork_shared_compositions(sequences: &mut [Sequence]) -> Vec<(CompositionId, CompositionId)> {
    let mut seen = std::collections::HashSet::new();
    let mut forks = Vec::new();
    let mut claim = |slot: &mut Option<CompositionId>| {
        if let Some(id) = *slot {
            if !seen.insert(id) {
                let own = CompositionId::new();
                seen.insert(own);
                forks.push((id, own));
                *slot = Some(own);
            }
        }
    };
    for sequence in sequences {
        for track in &mut sequence.tracks {
            for clip in &mut track.clips {
                claim(&mut clip.composition);
                if let Some(audition) = clip.audition.as_mut() {
                    for take in &mut audition.takes {
                        if let Some(c) = take.clip.as_mut() {
                            claim(&mut c.composition);
                        }
                        for (_, c) in &mut take.sound {
                            claim(&mut c.composition);
                        }
                    }
                }
            }
        }
    }
    forks
}

/// Every composition the sequences' clips and takes name, each once.
pub fn compositions_used(sequences: &[Sequence]) -> std::collections::BTreeSet<CompositionId> {
    let mut used = std::collections::BTreeSet::new();
    for clip in sequences.iter().flat_map(|s| &s.tracks).flat_map(|t| &t.clips) {
        used.extend(clip.composition);
        for take in clip.audition.iter().flat_map(|a| &a.takes) {
            used.extend(take.clip.as_ref().and_then(|c| c.composition));
            used.extend(take.sound.iter().filter_map(|(_, c)| c.composition));
        }
    }
    used
}
