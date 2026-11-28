//! What a composition names the things of Klipp's it lays in by: the
//! asset of a footage node, which Klipp answers as the composition's host.
//! Each kind of name is read and written here only, and both of Klipp's
//! hosts (the renders' in `compose` and the Veylvik page's
//! `editor_footage::TabHost`) route a name by matching on it, so a kind
//! added here is one each host has to answer, or say it doesn't.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use klipp_core::{ClipId, MediaId};
use veylvik_document::{Composition, Operation};

/// A name of Klipp's in a composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostKey {
    /// The clip's own media: what a composition made for a clip names its
    /// footage by ([`crate::CLIP_MEDIA_KEY`]).
    ClipMedia,
    /// What is below the clip in the frame Klipp renders: the tracks under
    /// it, composited ([`crate::BELOW_KEY`]).
    Below,
    /// A clip of a precomposition's scene, by its id as the scene keeps it
    /// (`crate::clips`).
    Clip(ClipId),
    /// Media of Klipp's pool (`crate::pool`).
    Media(MediaId),
}

impl HostKey {
    /// The name `key` is, if it is one of Klipp's.
    pub fn parse(key: &str) -> Option<Self> {
        let id = |rest: &str| uuid::Uuid::parse_str(rest).ok();
        match key {
            crate::CLIP_MEDIA_KEY => Some(Self::ClipMedia),
            crate::BELOW_KEY => Some(Self::Below),
            _ => {
                if let Some(rest) = key.strip_prefix("clip:") {
                    id(rest).map(|u| Self::Clip(ClipId(u)))
                } else {
                    key.strip_prefix("media:").and_then(id).map(|u| Self::Media(MediaId(u)))
                }
            }
        }
    }

    /// The name as a composition writes it.
    pub fn name(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for HostKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClipMedia => f.write_str(crate::CLIP_MEDIA_KEY),
            Self::Below => f.write_str(crate::BELOW_KEY),
            Self::Clip(id) => write!(f, "clip:{}", id.0),
            Self::Media(id) => write!(f, "media:{}", id.0),
        }
    }
}

/// Every name of Klipp's `composition` has footage of, each once.
pub fn host_keys(composition: &Composition) -> HashSet<HostKey> {
    host_names(composition).into_values().collect()
}

/// Every name of Klipp's `composition` has footage of, as it writes it (a
/// host answers by that), with what it names.
pub fn host_names(composition: &Composition) -> BTreeMap<String, HostKey> {
    composition
        .nodes
        .values()
        .filter_map(|n| match &n.operation {
            Operation::Footage(f) => HostKey::parse(&f.asset).map(|k| (f.asset.clone(), k)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each kind is read back from the name it writes, and nothing else is
    /// taken for one: a file of the composition's own, a broken id, a
    /// prefix alone.
    #[test]
    fn each_kind_of_name_reads_back_as_itself_and_nothing_else_reads_as_one() {
        let kinds = [HostKey::ClipMedia, HostKey::Below, HostKey::Clip(ClipId::new()), HostKey::Media(MediaId::new())];
        for k in kinds {
            assert_eq!(HostKey::parse(&k.name()), Some(k), "{k}");
        }
        for not in ["footage/a.mov", "clip:", "clip:nope", "media:", "media:42", "Clip", "below ", ""] {
            assert_eq!(HostKey::parse(not), None, "{not:?}");
        }
        assert_eq!(HostKey::ClipMedia.name(), "clip", "the clip's media is the bare word, apart from a scene's clips");
    }

    /// A host answers a composition by the name as it wrote it: one with
    /// an id in capitals is found by that, not by the name Klipp would
    /// write.
    #[test]
    fn a_name_is_kept_as_the_composition_wrote_it() {
        let id = MediaId::new();
        let written = format!("media:{}", id.0.to_string().to_uppercase());
        let mut c = Composition::from_host_footage(crate::CLIP_MEDIA_KEY, "Clip", None, [64, 36], veylvik_document::Time { ticks: 24, rate: 1 }, 24).unwrap();
        let with = crate::pool::with_media(&serde_json::to_vec(&c).unwrap(), id, "a", std::path::Path::new("/a.mov")).unwrap();
        c = serde_json::from_slice(&String::from_utf8(with.to_vec()).unwrap().replace(&HostKey::Media(id).name(), &written).into_bytes()).unwrap();
        let names = host_names(&c);
        assert_eq!(names.get(&written), Some(&HostKey::Media(id)));
        assert_eq!(names.get(crate::CLIP_MEDIA_KEY), Some(&HostKey::ClipMedia));
        assert_eq!(crate::pool::pool_keys(&c), std::collections::BTreeSet::from([written]));
    }
}
