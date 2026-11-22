//! A clip shown through a Veylvik composition: the clip names it by id,
//! the project keeps it (`klipp_project::compositions`), and the clip's
//! picture comes from it with the clip's own media as its footage.
//!
//! Every clip has a composition of its own. A copy of a clip, by split,
//! duplicate, paste or a duplicated sequence, names the same one until
//! [`fork_shared_compositions`] gives it a copy; the project does that
//! before it saves, and the Veylvik tab before it opens a clip, so working
//! on one never changes another.
//!
//! A clip may keep several compositions, its versions, and show one of
//! them; the others are kept with the project to switch back to.

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

/// One of a clip's compositions, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositionVersion {
    pub name: String,
    pub id: CompositionId,
}

/// `clip`'s compositions, the one it shows among them: its versions, or
/// its one composition as "Version 1", or none.
pub fn versions(clip: &crate::Clip) -> Vec<CompositionVersion> {
    if !clip.composition_versions.is_empty() {
        return clip.composition_versions.clone();
    }
    clip.composition.map(|id| vec![CompositionVersion { name: version_name(1), id }]).unwrap_or_default()
}

/// The name a version is given: "Version 2" for the second.
pub fn version_name(n: usize) -> String {
    format!("Version {n}")
}

/// A name for a new version of `clip`: the first "Version N" not taken.
pub fn next_version_name(clip: &crate::Clip) -> String {
    let have = versions(clip);
    (1..).map(version_name).find(|n| !have.iter().any(|v| &v.name == n)).unwrap_or_default()
}

/// Give every clip that shares a composition with one before it a new id
/// of its own, and say which: `(the one it shared, its own)`. The caller
/// copies each composition under its new id. The first clip naming one,
/// in sequence, track and clip order, keeps it; the takes an audition
/// holds count as clips.
pub fn fork_shared_compositions(sequences: &mut [Sequence]) -> Vec<(CompositionId, CompositionId)> {
    let mut seen = std::collections::HashSet::new();
    let mut forks = Vec::new();
    let mut claim_one = |slot: &mut Option<CompositionId>| {
        if let Some(id) = *slot {
            if !seen.insert(id) {
                let own = CompositionId::new();
                seen.insert(own);
                forks.push((id, own));
                *slot = Some(own);
            }
        }
    };
    // A clip's versions each, and the one it shows as the version it is.
    let mut claim = |clip: &mut crate::Clip| {
        let shown = clip.composition;
        let mut shown_now = None;
        for v in &mut clip.composition_versions {
            let was = v.id;
            let mut slot = Some(was);
            claim_one(&mut slot);
            v.id = slot.unwrap_or(was);
            if shown == Some(was) && shown_now.is_none() {
                shown_now = Some(v.id);
            }
        }
        match shown_now {
            Some(id) => clip.composition = Some(id),
            None => claim_one(&mut clip.composition),
        }
    };
    for sequence in sequences {
        for track in &mut sequence.tracks {
            for clip in &mut track.clips {
                claim(clip);
                if let Some(audition) = clip.audition.as_mut() {
                    for take in &mut audition.takes {
                        if let Some(c) = take.clip.as_mut() {
                            claim(c);
                        }
                        for (_, c) in &mut take.sound {
                            claim(c);
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
        let mut of = |c: &crate::Clip| {
            used.extend(c.composition);
            used.extend(c.composition_versions.iter().map(|v| v.id));
        };
        of(clip);
        for take in clip.audition.iter().flat_map(|a| &a.takes) {
            if let Some(c) = &take.clip {
                of(c);
            }
            take.sound.iter().for_each(|(_, c)| of(c));
        }
    }
    used
}
