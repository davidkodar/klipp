//! The timeline grade's stand-in: the proxy track that carries a grade on the
//! whole timeline, settled, stood in and stood down (#890).

use klipp_timeline::GradeAccess;
#[allow(unused_imports)]
use super::*;

/// The hidden track the timeline grade stands in on while the Colour
/// workspace grades it; fixed for the sequence.
pub(super) fn timeline_grade_proxy_track_id(seq: &klipp_timeline::Sequence) -> klipp_core::TrackId {
    const STAND_IN: u128 = 0x7374_616e_642d_696e_2074_7261_636b_0001;
    klipp_core::TrackId(uuid::Uuid::from_u128(seq.id.0.as_u128() ^ STAND_IN))
}

/// Take the timeline grade's stand-in track off `seq`, if it is there,
/// keeping its graph as the timeline grade when something was done to
/// it. True when the grade changed.
///
/// Visiting is not an edit: `fresh` is the default graph the stand-in
/// was made with when the sequence had no grade, and a stand-in still
/// equal to it leaves the sequence without one, so looking at the
/// Timeline tab neither dirties the project nor saves a graph that does
/// nothing. With no `fresh`, the stand-in is measured against the
/// sequence's own grade, and a sequence without one takes only a grade
/// that does something.
pub(super) fn settle_timeline_grade_proxy(seq: &mut klipp_timeline::Sequence, fresh: Option<&klipp_nodes::NodeGraph>) -> bool {
    let track_id = timeline_grade_proxy_track_id(seq);
    let Some(pos) = seq.tracks.iter().position(|t| t.id == track_id) else { return false };
    let track = seq.tracks.remove(pos);
    let Some(proxy) = track.clips.into_iter().next() else { return false };
    let Some(target) = GradeStandIn::of(seq, proxy.id) else { return false };
    let unchanged = match fresh.or(target.stored(seq)) {
        Some(given) => given == proxy.grade(),
        None => proxy.grade().is_identity(),
    };
    if unchanged {
        return false;
    }
    target.store(seq, proxy.grade().clone());
    true
}

/// What the Colour workspace grades through a stand-in clip rather than
/// a clip of the timeline: the timeline grade, a grade group's pre-clip
/// or post-clip grade (#237), or a video track's own grade (#1408). Either stands in on the same
/// hidden track, under the id its edits are routed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GradeStandIn {
    Timeline,
    Group(klipp_core::GradeGroupId, klipp_timeline::GradeStage),
    /// A video track's own grade (#1408).
    Track(klipp_core::TrackId),
}

impl GradeStandIn {
    /// Which one `clip_id` stands for, if it is a stand-in's id.
    pub(super) fn of(seq: &klipp_timeline::Sequence, clip_id: klipp_core::ClipId) -> Option<Self> {
        if clip_id == seq.timeline_grade_clip_id() {
            return Some(Self::Timeline);
        }
        seq.grade_group_stage_of(clip_id).map(|(g, s)| Self::Group(g, s)).or_else(|| seq.track_grade_of(clip_id).map(Self::Track))
    }

    pub(super) fn id(self, seq: &klipp_timeline::Sequence) -> Option<klipp_core::ClipId> {
        match self {
            Self::Timeline => Some(seq.timeline_grade_clip_id()),
            Self::Group(g, s) => seq.grade_group(g).map(|g| g.stage_clip_id(s)),
            Self::Track(t) => seq.tracks.iter().any(|x| x.id == t && x.kind == klipp_timeline::TrackKind::Video).then(|| klipp_timeline::Sequence::track_grade_clip_id(t)),
        }
    }

    pub(super) fn stored(self, seq: &klipp_timeline::Sequence) -> Option<&klipp_nodes::NodeGraph> {
        match self {
            Self::Timeline => seq.timeline_grade(),
            Self::Group(g, s) => seq.grade_group(g).and_then(|g| g.stage(s)),
            Self::Track(t) => seq.tracks.iter().find(|x| x.id == t).and_then(|x| x.effects.track_grade()),
        }
    }

    pub(super) fn store(self, seq: &mut klipp_timeline::Sequence, graph: klipp_nodes::NodeGraph) {
        match self {
            Self::Timeline => seq.timeline_grade_slot().set(Some(graph)),
            Self::Group(g, s) => {
                if let Some(g) = seq.grade_groups.iter_mut().find(|x| x.id == g) {
                    g.stage_slot(s).set(Some(graph));
                }
            }
            Self::Track(t) => {
                if let Some(x) = seq.tracks.iter_mut().find(|x| x.id == t) {
                    x.effects.grade_slot().set(Some(graph));
                }
            }
        }
    }

    /// Where its nodes are by path (M3 step 19): the timeline grade's or
    /// the group stage's own root, never the stand-in clip's.
    pub(super) fn root(self, seq: &klipp_timeline::Sequence) -> klipp_graph_format::path::Root {
        use klipp_graph_format::lifted::Stage;
        use klipp_graph_format::path::Root;
        match self {
            Self::Timeline => Root::TimelineGrade { sequence: seq.id.0 },
            Self::Group(g, s) => Root::GroupStage { group: g.0, stage: if s == klipp_timeline::GradeStage::Pre { Stage::Pre } else { Stage::Post } },
            Self::Track(t) => Root::Track { track: t.0, part: klipp_graph_format::path::TrackPart::Grade },
        }
    }

    /// What the panels call it.
    pub(super) fn title(self, seq: &klipp_timeline::Sequence) -> String {
        match self {
            Self::Timeline => "Timeline Grade".into(),
            Self::Group(g, s) => {
                let name = seq.grade_group(g).map_or("Group", |g| g.name.as_str());
                format!("{name} \u{00b7} {}", if s == klipp_timeline::GradeStage::Pre { "Pre-clip" } else { "Post-clip" })
            }
            Self::Track(t) => format!("Track grade \u{00b7} {}", seq.tracks.iter().find(|x| x.id == t).map_or("Track", |x| x.name.as_str())),
        }
    }
}

/// Put the timeline grade's stand-in on `seq`, on its hidden track, and
/// say which clip it is; `None` when it is already there. While the
/// sequence has no grade the stand-in shows `fresh`, made the first
/// time and kept for this sequence, so it has the same node ids each
/// time it stands in: the inspector dispatches on the ids it drew.
///
/// The same for a grade group's stage (#237). `fresh` is kept by the
/// stand-in's id.
pub(super) fn stand_in_grade_on(
    seq: &mut klipp_timeline::Sequence,
    which: GradeStandIn,
    fresh: &mut Option<(klipp_core::ClipId, klipp_nodes::NodeGraph)>,
) -> Option<klipp_core::ClipId> {
    let track_id = timeline_grade_proxy_track_id(seq);
    if seq.tracks.iter().any(|t| t.id == track_id) {
        return None;
    }
    let stand_in_id = which.id(seq)?;
    let mut proxy = klipp_timeline::Clip::timeline_grade_proxy(seq);
    proxy.id = stand_in_id;
    match which.stored(seq) {
        Some(g) => proxy.set_grade(g.clone()),
        None => {
            let given = match fresh {
                Some((id, g)) if *id == stand_in_id => g,
                _ => &fresh.insert((stand_in_id, klipp_nodes::NodeGraph::default())).1,
            };
            proxy.set_grade(given.clone());
        }
    }
    let id = proxy.id;
    let mut track = klipp_timeline::Track::new("Timeline Grade", klipp_timeline::TrackKind::Video);
    track.id = track_id;
    track.visible = false;
    track.muted = true;
    track.clips.push(proxy);
    seq.tracks.push(track);
    Some(id)
}

/// Take the stand-in off `seq` again, keeping its graph as the timeline
/// grade if it was edited. The stand-in is measured against `fresh`
/// only while the sequence has no grade, and `fresh` is let go the
/// moment one begins: from then on the stand-in carries the sequence's
/// own grade, and that is what a visit that changes nothing must match.
/// Measured against the fresh graph instead, an existing grade would
/// differ on every command, if only by its node ids, and looking at it
/// would be an edit again (#368).
pub(super) fn stand_down_timeline_grade_on(
    seq: &mut klipp_timeline::Sequence,
    fresh: &mut Option<(klipp_core::ClipId, klipp_nodes::NodeGraph)>,
) -> TimelineGradeSettled {
    let track_id = timeline_grade_proxy_track_id(seq);
    let Some(stand_in_id) = seq.tracks.iter().find(|t| t.id == track_id).and_then(|t| t.clips.first()).map(|c| c.id) else {
        return TimelineGradeSettled::Unchanged;
    };
    let had_grade = GradeStandIn::of(seq, stand_in_id).is_some_and(|w| w.stored(seq).is_some());
    let given = fresh.as_ref().filter(|(id, _)| *id == stand_in_id && !had_grade).map(|(_, g)| g);
    match (settle_timeline_grade_proxy(seq, given), had_grade) {
        (false, _) => TimelineGradeSettled::Unchanged,
        (true, true) => TimelineGradeSettled::Changed,
        (true, false) => {
            *fresh = None;
            TimelineGradeSettled::Begun
        }
    }
}

/// What standing the timeline grade's stand-in down did to the grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineGradeSettled {
    Unchanged,
    Changed,
    /// A grade where the sequence had none.
    Begun,
}

#[cfg(test)]
mod timeline_grade_stand_in_tests {
    use super::*;

    /// The stand-in on its track, as [`KlippApp::stand_in_timeline_grade`]
    /// puts it there; the graph it was given comes back for the settle.
    fn stand_in(seq: &mut klipp_timeline::Sequence) -> (klipp_nodes::NodeGraph, &mut klipp_timeline::Clip) {
        let proxy = klipp_timeline::Clip::timeline_grade_proxy(seq);
        let given = proxy.grade().clone();
        let mut track = klipp_timeline::Track::new("Timeline Grade", klipp_timeline::TrackKind::Video);
        track.id = timeline_grade_proxy_track_id(seq);
        track.clips.push(proxy);
        seq.tracks.push(track);
        (given, &mut seq.tracks.last_mut().unwrap().clips[0])
    }

    #[test]
    fn the_stand_in_track_never_outlives_its_graph() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let tracks = seq.tracks.len();
        let (given, proxy) = stand_in(&mut seq);
        let nodes = proxy.edit_grade(|graph| {
            let corrector = graph.ensure_corrector();
            graph.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.5);
            graph.nodes.len()
        });
        assert!(settle_timeline_grade_proxy(&mut seq, Some(&given)));
        assert_eq!(seq.tracks.len(), tracks);
        assert_eq!(seq.timeline_grade().map(|g| g.nodes.len()), Some(nodes));
    }

    /// Opening the Timeline tab and looking is not an edit: the sequence
    /// keeps having no grade, and nothing is marked changed.
    #[test]
    fn a_bare_visit_leaves_a_sequence_without_a_grade() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let (given, _) = stand_in(&mut seq);
        assert!(!settle_timeline_grade_proxy(&mut seq, Some(&given)));
        assert!(seq.timeline_grade().is_none());
        assert!(!seq.tracks.iter().any(|t| t.id == timeline_grade_proxy_track_id(&seq)));
    }

    /// A node added and left at its defaults is still an edit the user
    /// made, and must not be lost between two commands.
    #[test]
    fn a_node_added_at_its_defaults_is_kept() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let (given, proxy) = stand_in(&mut seq);
        proxy.edit_grade(|g| g.ensure_lut3d());
        assert!(settle_timeline_grade_proxy(&mut seq, Some(&given)));
        assert!(seq.timeline_grade().is_some_and(|g| g.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Lut3D)));
    }

    /// With a grade already there, a visit that changes nothing changes
    /// nothing.
    #[test]
    fn an_unchanged_stand_in_leaves_an_existing_grade_alone() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut grade = klipp_nodes::NodeGraph::default();
        let corrector = grade.ensure_corrector();
        grade.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.5);
        seq.timeline_grade_slot().set(Some(grade.clone()));
        let (_, _) = stand_in(&mut seq);
        assert!(!settle_timeline_grade_proxy(&mut seq, None));
        assert_eq!(seq.timeline_grade(), Some(&grade));
    }

    /// #237: a grade group's stage stands in the same way: graded through
    /// its stand-in, its graph lands on the group's stage; a visit that
    /// changes nothing leaves the stage without a graph.
    #[test]
    fn a_grade_group_stage_stands_in_and_settles_on_the_group() {
        use klipp_timeline::{GradeGroup, GradeStage};
        let mut seq = klipp_timeline::Sequence::new_default();
        let group = GradeGroup::new("Night");
        let gid = group.id;
        seq.grade_groups.push(group);
        let which = GradeStandIn::Group(gid, GradeStage::Post);
        let mut fresh = None;

        let id = stand_in_grade_on(&mut seq, which, &mut fresh).expect("stood in");
        assert_eq!(GradeStandIn::of(&seq, id), Some(which));
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Unchanged, "a look is not an edit");
        assert!(seq.grade_groups[0].stage(klipp_timeline::GradeStage::Post).is_none());

        stand_in_grade_on(&mut seq, which, &mut fresh).expect("stood in");
        let track_id = timeline_grade_proxy_track_id(&seq);
        let proxy = seq.tracks.iter_mut().find(|t| t.id == track_id).unwrap().clips.first_mut().unwrap();
        let lut = proxy.edit_grade(|g| g.ensure_lut3d());
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Begun);
        assert!(seq.grade_groups[0].stage(klipp_timeline::GradeStage::Post).is_some_and(|g| g.nodes.iter().any(|n| n.id == lut)), "the edit landed on the group's post-clip grade");
        assert!(seq.grade_groups[0].stage(klipp_timeline::GradeStage::Pre).is_none() && seq.timeline_grade().is_none(), "and nowhere else");
        assert!(!seq.tracks.iter().any(|t| t.id == track_id), "the stand-in is gone");
    }

    /// #1408: a video track's own grade stands in the same way: graded
    /// through its stand-in, its graph lands on the track; a visit that
    /// changes nothing leaves the track without a grade.
    #[test]
    fn a_track_grade_stands_in_and_settles_on_the_track() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let tid = seq.tracks.iter().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().id;
        let which = GradeStandIn::Track(tid);
        let mut fresh = None;
        let track_grade = |seq: &klipp_timeline::Sequence| seq.tracks.iter().find(|t| t.id == tid).unwrap().effects.track_grade().cloned();

        let id = stand_in_grade_on(&mut seq, which, &mut fresh).expect("stood in");
        assert_eq!(GradeStandIn::of(&seq, id), Some(which));
        assert!(which.title(&seq).starts_with("Track grade"), "{}", which.title(&seq));
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Unchanged, "a look is not an edit");
        assert!(track_grade(&seq).is_none());

        stand_in_grade_on(&mut seq, which, &mut fresh).expect("stood in");
        let track_id = timeline_grade_proxy_track_id(&seq);
        let proxy = seq.tracks.iter_mut().find(|t| t.id == track_id).unwrap().clips.first_mut().unwrap();
        let lut = proxy.edit_grade(|g| g.ensure_lut3d());
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Begun);
        assert!(track_grade(&seq).is_some_and(|g| g.nodes.iter().any(|n| n.id == lut)), "the edit landed on the track's grade");
        assert!(seq.timeline_grade().is_none(), "and nowhere else");
        assert!(!seq.tracks.iter().any(|t| t.id == track_id), "the stand-in is gone");
    }

    /// The whole round, as [`KlippApp::stand_in_timeline_grade`] and
    /// [`KlippApp::stand_down_timeline_grade`] make it: a first visit
    /// that edits begins the grade, and the next visit that only looks
    /// leaves it as it is. The fresh graph the first stand-in was made
    /// with must not be what the second is measured against, or the
    /// grade, with its own node ids, differs from it on every command.
    #[test]
    fn a_visit_after_the_grade_began_is_not_an_edit() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut fresh = None;
        let id = stand_in_grade_on(&mut seq, GradeStandIn::Timeline, &mut fresh).expect("stood in");
        assert!(fresh.as_ref().is_some_and(|(seq_id, _)| *seq_id == seq.timeline_grade_clip_id()));
        let track_id = timeline_grade_proxy_track_id(&seq);
        seq.tracks.iter_mut().find(|t| t.id == track_id).unwrap().clips[0].edit_grade(|graph| {
            let corrector = graph.ensure_corrector();
            graph.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.5);
        });
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Begun);
        let grade = seq.timeline_grade().cloned().expect("the grade began");
        assert!(fresh.is_none(), "the fresh graph is let go once the grade begins");

        assert_eq!(stand_in_grade_on(&mut seq, GradeStandIn::Timeline, &mut fresh), Some(id));
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Unchanged);
        assert_eq!(seq.timeline_grade(), Some(&grade), "the grade is as it was");
        assert!(!seq.tracks.iter().any(|t| t.id == track_id));
    }

    /// The same, for a grade the sequence already had when the fresh
    /// graph was made for it: only a sequence without a grade is
    /// measured against the fresh graph.
    #[test]
    fn a_stale_fresh_graph_is_not_held_against_an_existing_grade() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut grade = klipp_nodes::NodeGraph::default();
        let corrector = grade.ensure_corrector();
        grade.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.5);
        seq.timeline_grade_slot().set(Some(grade.clone()));
        let mut fresh = Some((seq.timeline_grade_clip_id(), klipp_nodes::NodeGraph::default()));
        stand_in_grade_on(&mut seq, GradeStandIn::Timeline, &mut fresh).expect("stood in");
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Unchanged);
        assert_eq!(seq.timeline_grade(), Some(&grade));
    }
}
