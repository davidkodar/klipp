//! Ids for the entries a conversion turns into nodes that had none of
//! their own: effects, plug-in instances, and the grade group around a
//! grade graph. Each is a UUID v5 of the store that owns it, the list it
//! sat in and its slot there, so converting the same file twice gives the
//! same ids and re-saving never churns a file.

use uuid::Uuid;

/// The namespace of every derived id. Fixed forever: changing it changes
/// every derived id in every converted file.
pub const NAMESPACE: Uuid = Uuid::from_u128(0x6b6c_6970_702d_6772_6170_682d_6964_7331);

/// What owns a picture graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootKind {
    Clip,
    Track,
    Sequence,
    GradeGroup,
    /// A project Look, rooted at its own id.
    Look,
    /// A grade still or a colour preset: neither has an id, so the root is
    /// the nil id and the list carries its file name.
    Still,
    Preset,
}

impl RootKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Clip => "clip",
            Self::Track => "track",
            Self::Sequence => "sequence",
            Self::GradeGroup => "grade-group",
            Self::Look => "look",
            Self::Still => "still",
            Self::Preset => "preset",
        }
    }
}

/// The id of the entry at `slot` of `list` in the store `root` of `kind`:
/// `uuid v5(NAMESPACE, "<kind>/<root>/<list>/<slot>")`.
///
/// Lists are `grade` (slot 0: the group node around a grade graph), `ofx`
/// and `effects`. A store inside another gets a prefixed list: a clip's
/// grade version `n` uses the clip as root and `version/<n>/grade`; a
/// group's stages use `pre/grade` and `post/grade`; the timeline grade
/// uses the sequence and `timeline/grade`. A Look uses its own id and
/// `grade`; a still and a colour preset, which have no id, use the nil id
/// and `<file name>/grade`.
pub fn entry_id(kind: RootKind, root: Uuid, list: &str, slot: usize) -> Uuid {
    Uuid::new_v5(&NAMESPACE, format!("{}/{}/{}/{}", kind.tag(), root, list, slot).as_bytes())
}
