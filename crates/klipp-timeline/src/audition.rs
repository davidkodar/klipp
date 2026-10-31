//! Auditions (#3209): one clip slot on the timeline holding several
//! alternatives, switched in place.
//!
//! The clip on the track is the active take, a plain clip: render,
//! export, playback and the caches see it and nothing else. The other
//! takes are kept on it as data, each a whole clip (so they may differ in
//! media, range, speed, grade, effects or title text) and the sound
//! clips linked to it. A switch puts a take's clip in the slot, keeping
//! the slot's id, start and track, so clips connected to it keep their
//! anchors, and stores the one it replaces as a take.

use klipp_core::{TakeId, TrackId};
use serde::{Deserialize, Serialize};

use crate::Clip;

/// The takes a clip's slot holds, in the order they were added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audition {
    pub takes: Vec<Take>,
    /// Which take is in the slot.
    pub active: TakeId,
}

/// One alternative.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Take {
    pub id: TakeId,
    pub name: String,
    /// The take's clip, when it isn't the one in the slot: `None` for the
    /// active take, whose clip is the slot itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<Box<Clip>>,
    /// The sound clips linked to it, each with the track it was on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sound: Vec<(TrackId, Clip)>,
}

impl Audition {
    /// The take named by `id`, or by its name, any case.
    pub fn find(&self, key: &str) -> Option<&Take> {
        self.takes.iter().find(|t| t.id.to_string() == key).or_else(|| self.takes.iter().find(|t| t.name.eq_ignore_ascii_case(key)))
    }

    /// The active take.
    pub fn active_take(&self) -> Option<&Take> {
        self.takes.iter().find(|t| t.id == self.active)
    }

    /// Every clip an inactive take holds, picture and sound: what still
    /// counts as in use (its media, its fonts) though it isn't on a track.
    pub fn stored_clips(&self) -> impl Iterator<Item = &Clip> {
        self.takes.iter().flat_map(|t| t.clip.as_deref().into_iter().chain(t.sound.iter().map(|(_, c)| c)))
    }

    /// A name for a new take: "Take N", the first N not taken.
    pub fn next_name(&self) -> String {
        (1..).map(|n| format!("Take {n}")).find(|name| !self.takes.iter().any(|t| t.name.eq_ignore_ascii_case(name))).expect("a free name")
    }
}

impl Clip {
    /// This clip and every clip its audition's other takes hold, picture
    /// and sound (#3209): what a walk over the media, ranges or fonts in
    /// use reads, as those takes are kept to be switched to.
    pub fn with_takes(&self) -> impl Iterator<Item = &Clip> {
        std::iter::once(self).chain(self.audition.iter().flat_map(|a| a.stored_clips()))
    }

    /// The clips this clip's audition's other takes hold, to change with
    /// it: a remap onto other files reaches them too.
    pub fn takes_mut(&mut self) -> impl Iterator<Item = &mut Clip> {
        self.audition.iter_mut().flat_map(|a| a.takes.iter_mut().flat_map(|t| t.clip.as_deref_mut().into_iter().chain(t.sound.iter_mut().map(|(_, c)| c))))
    }
}
