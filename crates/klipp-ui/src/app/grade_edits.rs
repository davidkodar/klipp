//! Grades taken from elsewhere: from a still, and attributes pasted from one
//! clip onto others, as the edits that make them (#890).

use klipp_timeline::GradeAccess;
use super::*;

/// The clip a grade still is taken from: the selected clip when it is
/// under the playhead, since that is the one the Colour panels show;
/// otherwise the topmost visible video clip there, which is what the
/// viewer shows. `None` over a gap.
pub(super) fn still_source_clip(
    seq: &klipp_timeline::Sequence,
    playhead: klipp_core::Timecode,
    selected: Option<klipp_core::ClipId>,
) -> Option<&klipp_timeline::Clip> {
    let video = || seq.tracks.iter().filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video) && t.visible);
    let selected_here = selected.and_then(|id| video().find_map(|t| t.clip_at_timecode(playhead).map(|(_, c)| c).filter(|c| c.id == id)));
    selected_here.or_else(|| video().find_map(|t| t.clip_at_timecode(playhead).map(|(_, c)| c)))
}

/// `secs` as HH:MM:SS:FF at a nominal `fps`, for display.
pub(super) fn record_timecode(secs: f64, fps: f64) -> String {
    let nominal = fps.round().max(1.0) as i64;
    let frames = (secs.max(0.0) * fps).round() as i64;
    let (ff, total) = (frames % nominal, frames / nominal);
    format!("{:02}:{:02}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60, ff)
}

/// A grade from a still or the library put on clips: the graph edits,
/// one per clip graded, why any target was passed over, once per reason,
/// and the grade's node colour labels on the nodes each clip took
/// (#1866), to go in the same step.
pub(super) struct GradeApplied {
    pub edits: Vec<EditCommand>,
    pub refused: Vec<klipp_nodes::CannotAppend>,
    pub labels: Option<EditCommand>,
}

/// `grade` onto each of `targets`, fresh node ids per clip, as graph
/// edits to apply together: in place of each clip's grade, or with
/// `append` after it, keeping the clip's own balance. Its labels
/// `labels` (by path inside it) go onto each clip's copy: in place of its
/// grade's labels, or beside them with `append`.
pub(super) fn apply_grade(
    seq: &klipp_timeline::Sequence,
    grade: &klipp_nodes::NodeGraph,
    labels: &[klipp_timeline::GradeNodeLabel],
    targets: &[klipp_core::ClipId],
    append: bool,
) -> GradeApplied {
    let mut refused = Vec::new();
    let mut node_labels = seq.node_labels.clone();
    let edits = seq
        .all_clips()
        .filter(|c| targets.contains(&c.id))
        .filter_map(|c| {
            let (new_graph, map) = if append {
                let mut g = c.grade().clone();
                match g.append_mapped(grade) {
                    Ok(map) => (g, map),
                    Err(why) => {
                        if !refused.contains(&why) {
                            refused.push(why);
                        }
                        return None;
                    }
                }
            } else {
                c.grade().with_grade_of_mapped(grade)
            };
            let moved = klipp_timeline::remap_grade_node_labels(labels, &map, &new_graph);
            node_labels = klipp_timeline::with_grade_node_labels(&node_labels, c.id, &moved, !append);
            Some(EditCommand::SetNodeGraph { clip_id: c.id, old_graph: c.grade().clone(), new_graph })
        })
        .collect();
    let labels = (node_labels != seq.node_labels).then(|| EditCommand::SetNodeLabels { old: seq.node_labels.clone(), new: node_labels });
    GradeApplied { edits, refused, labels }
}

/// How many other pieces of its shot the Colour page offers `clip`'s
/// grade to: only once it has a grade, and only when there are some.
pub(crate) fn shot_pieces_offer(seq: &klipp_timeline::Sequence, clip: klipp_core::ClipId) -> Option<usize> {
    let graded = seq.all_clips().find(|c| c.id == clip).is_some_and(|c| !c.grade_is_identity());
    let n = if graded { seq.ungraded_shot_pieces(clip).len() } else { 0 };
    (n > 0).then_some(n)
}

/// One line for a toast out of the reasons targets were passed over.
pub(super) fn refusals_line(refused: &[klipp_nodes::CannotAppend]) -> String {
    refused.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("; ")
}

/// The edits a Paste Attributes makes: each target clip replaced whole
/// by itself with the chosen attributes written on. Free of the app so
/// it can be tested without one.
/// And a pasted grade's node colour labels on the nodes each clip took
/// (#1866), in place of its grade's own: an edit for the same step.
pub(super) fn paste_attributes(
    seq: &klipp_timeline::Sequence,
    targets: &[klipp_core::ClipId],
    attrs: &crate::state::AttributesClipboard,
    what: crate::state::PasteChoice,
) -> (Vec<EditCommand>, Option<EditCommand>) {
    let mut node_labels = seq.node_labels.clone();
    let edits = seq
        .all_clips()
        .filter(|c| targets.contains(&c.id))
        .map(|c| {
            let mut new = c.clone();
            if let Some(map) = attrs.paste_onto_mapped(&mut new, what) {
                let moved = klipp_timeline::remap_grade_node_labels(&attrs.grade_labels, &map, new.grade());
                node_labels = klipp_timeline::with_grade_node_labels(&node_labels, c.id, &moved, true);
            }
            EditCommand::ReplaceClip { description: "Paste Attributes", old: Box::new(c.clone()), new: Box::new(new) }
        })
        .collect();
    let labels = (node_labels != seq.node_labels).then(|| EditCommand::SetNodeLabels { old: seq.node_labels.clone(), new: node_labels });
    (edits, labels)
}

impl crate::state::AppState {
    /// Save or switch a clip's grade version (`SaveGradeVersion`,
    /// `SwitchGradeVersion`): whether the clip changed. A version keeps its
    /// corrections' mix and mask channel (M3 step 17a, #1322).
    pub(super) fn grade_version_command(&mut self, cmd: &AppCommand) -> bool {
        let clip_id = match cmd {
            AppCommand::SaveGradeVersion { clip_id, .. } | AppCommand::SwitchGradeVersion { clip_id, .. } => *clip_id,
            _ => return false,
        };
        let seq = &mut self.sequences[self.active_sequence_idx];
        let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else { return false };
        // A version keeps its corrections' mix and mask channel (#1322).
        match cmd {
            AppCommand::SaveGradeVersion { name, .. } => {
                let version = klipp_timeline::GradeVersion::new(name.clone(), clip.grade_for_version());
                let index = clip.edit_grade_versions(|vs| {
                    vs.push(version);
                    vs.len() - 1
                });
                clip.set_active_grade_version(Some(index));
            }
            AppCommand::SwitchGradeVersion { version_index, .. } => {
                // Save current graph back to active version slot before switching
                let Some(grade) = clip.grade_versions().get(*version_index).map(|v| v.grade().clone()) else { return false };
                let before = clip.clone();
                if let Some(active_idx) = clip.active_grade_version() {
                    if active_idx < clip.grade_versions().len() {
                        let grade = clip.grade_for_version();
                        clip.edit_grade_versions(|vs| vs[active_idx].set_grade(grade));
                    }
                }
                // Load the target version
                if let Err(why) = clip.set_grade_from_version(grade) {
                    *clip = before;
                    self.toast.show(format!("That grade version can't be switched to: {why}"));
                    return false;
                }
                clip.set_active_grade_version(Some(*version_index));
            }
            _ => {}
        }
        self.dirty = true;
        true
    }
}

#[cfg(test)]
mod paste_attributes_tests {
    use super::*;
    use klipp_timeline::{Clip, Sequence};

    fn t(secs: i64) -> klipp_core::Timecode {
        klipp_core::Timecode { value: secs * 25, rate: 25 }
    }

    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(klipp_core::MediaId::new(), t(from), t(to), t(0), t(to - from))
    }

    #[test]
    fn a_paste_reaches_every_target_as_one_undoable_step() {
        let mut seq = Sequence::new_default();
        let source = {
            let mut c = clip(0, 2);
            c.set_grade(klipp_nodes::NodeGraph::default());
            c.volume = 0.25;
            c
        };
        // The first clip in the sequence is not a target: the old handler
        // stopped there and pasted nothing.
        let (a, b, c) = (clip(0, 2), clip(2, 4), clip(4, 6));
        let targets = vec![b.id, c.id];
        seq.tracks[0].clips = vec![a.clone(), b.clone(), c.clone()];
        let attrs = crate::state::AttributesClipboard::of(&source);
        let what = crate::state::PasteChoice { color_grade: true, audio: true, ..Default::default() };
        let (edits, _) = paste_attributes(&seq, &targets, &attrs, what);
        assert_eq!(edits.len(), 2);

        let mut sequences = vec![seq];
        let mut pool = klipp_media::MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in &edits {
            view.apply_forward(e);
        }
        let pasted: Vec<&Clip> = sequences[0].tracks[0].clips.iter().filter(|x| targets.contains(&x.id)).collect();
        assert!(pasted.iter().all(|x| x.volume == 0.25));
        // Fresh node ids on each clip, not the source's.
        let ids = |g: &klipp_nodes::NodeGraph| g.nodes.iter().map(|n| n.id).collect::<Vec<_>>();
        assert_ne!(ids(pasted[0].grade()), ids(source.grade()));
        assert_ne!(ids(pasted[0].grade()), ids(pasted[1].grade()));
        assert_eq!(sequences[0].tracks[0].clips[0].volume, a.volume, "a clip that was not a target is untouched");

        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in edits.iter().rev() {
            view.apply_inverse(e);
        }
        assert_eq!(sequences[0].tracks[0].clips[1].volume, b.volume);
        assert_eq!(sequences[0].tracks[0].clips[2].volume, c.volume);
    }

    /// A clip graded by `stops` of exposure on its Corrector.
    fn graded(mut c: Clip, stops: f32) -> Clip {
        let id = c.edit_grade(|g| g.ensure_corrector());
        c.edit_grade(|graph| {
            if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == id) {
                n.params.exposure = Some(stops);
            }
        });
        c
    }

    fn input_transforms(g: &klipp_nodes::NodeGraph) -> Vec<&klipp_nodes::Node> {
        g.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::InputTransform).collect()
    }

    fn exposure(g: &klipp_nodes::NodeGraph) -> Option<f32> {
        g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).and_then(|n| n.params.exposure)
    }

    /// A grade copied from a LogC shot, pasted onto a Sony shot: the
    /// Sony shot's decode stays its own, and the grade is the LogC
    /// shot's. It used to take the whole graph, ARRI curve included.
    #[test]
    fn a_pasted_grade_leaves_the_targets_input_transform_alone() {
        use klipp_core::LogFormat;
        let mut source = graded(clip(0, 2), -0.2);
        source.set_input_log_format(LogFormat::LogC3);
        let mut target = graded(clip(2, 4), 0.4);
        target.set_input_log_format(LogFormat::SLog3);
        let own = input_transforms(target.grade())[0].clone();

        let attrs = crate::state::AttributesClipboard::of(&source);
        let what = crate::state::PasteChoice { color_grade: true, ..Default::default() };
        attrs.paste_onto(&mut target, what);

        let kept = input_transforms(target.grade());
        assert_eq!(kept.len(), 1, "one Input Transform, the clip's own");
        assert_eq!(kept[0].kind, klipp_nodes::NodeKind::InputTransform);
        assert_eq!(kept[0].params, own.params, "the Sony decode, not ARRI's");
        assert_eq!(kept[0].params.log_format, Some(LogFormat::SLog3));
        assert_eq!(exposure(target.grade()), Some(-0.2), "the source's grade");
        assert!(
            target.grade().nodes.iter().all(|n| !source.grade().nodes.iter().any(|s| s.id == n.id)),
            "fresh ids"
        );
        assert!(target.grade().topo_sort().is_ok(), "still a graph the executor can run");
    }

    /// A grade copied from a Rec.709 shot has no Input Transform; the
    /// log shot it is pasted onto keeps its own rather than losing it.
    #[test]
    fn a_pasted_grade_without_an_input_transform_keeps_the_targets() {
        use klipp_core::LogFormat;
        let source = graded(clip(0, 2), -0.2);
        assert!(input_transforms(source.grade()).is_empty());
        let mut target = graded(clip(2, 4), 0.4);
        target.set_input_log_format(LogFormat::SLog3);
        let own = input_transforms(target.grade())[0].clone();

        let attrs = crate::state::AttributesClipboard::of(&source);
        let what = crate::state::PasteChoice { color_grade: true, ..Default::default() };
        attrs.paste_onto(&mut target, what);

        let kept = input_transforms(target.grade());
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].params, own.params);
        assert_eq!(exposure(target.grade()), Some(-0.2));
    }

    /// The other way round: a Rec.709 shot pasted a LogC shot's grade
    /// gets the grade and no decode it does not need.
    #[test]
    fn a_pasted_grade_brings_no_input_transform_to_a_clip_without_one() {
        let mut source = graded(clip(0, 2), -0.2);
        source.set_input_log_format(klipp_core::LogFormat::LogC3);
        let mut target = graded(clip(2, 4), 0.4);
        let attrs = crate::state::AttributesClipboard::of(&source);
        let what = crate::state::PasteChoice { color_grade: true, ..Default::default() };
        attrs.paste_onto(&mut target, what);
        assert!(input_transforms(target.grade()).is_empty());
        assert_eq!(exposure(target.grade()), Some(-0.2));
    }
}

#[cfg(test)]
mod grade_still_tests {
    use super::*;
    use klipp_timeline::{Clip, Sequence};

    fn t(secs: i64) -> klipp_core::Timecode {
        klipp_core::Timecode { value: secs * 25, rate: 25 }
    }

    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(klipp_core::MediaId::new(), t(from), t(to), t(0), t(to - from))
    }

    #[test]
    fn a_still_is_taken_from_the_selected_clip_only_when_it_is_under_the_playhead() {
        let mut seq = Sequence::new_default();
        let (a, b) = (clip(0, 4), clip(4, 8));
        seq.tracks[0].clips = vec![a.clone(), b.clone()];
        assert_eq!(still_source_clip(&seq, t(1), Some(a.id)).map(|c| c.id), Some(a.id));
        // Selected elsewhere: the clip the viewer shows.
        assert_eq!(still_source_clip(&seq, t(5), Some(a.id)).map(|c| c.id), Some(b.id));
        assert!(still_source_clip(&seq, t(9), Some(a.id)).is_none(), "a gap has no clip");
    }

    #[test]
    fn a_still_grade_goes_on_every_target_with_its_own_ids_and_undoes_as_one() {
        let mut seq = Sequence::new_default();
        let (a, b, c) = (clip(0, 2), clip(2, 4), clip(4, 6));
        seq.tracks[0].clips = vec![a.clone(), b.clone(), c.clone()];
        let grade = klipp_nodes::NodeGraph::default();
        let GradeApplied { edits, .. } = apply_grade(&seq, &grade, &[], &[a.id, c.id], false);
        assert_eq!(edits.len(), 2);
        let ids = |e: &EditCommand| match e {
            EditCommand::SetNodeGraph { new_graph, .. } => new_graph.nodes.iter().map(|n| n.id).collect::<Vec<_>>(),
            _ => panic!("a graph edit"),
        };
        assert_ne!(ids(&edits[0]), ids(&edits[1]), "each clip gets fresh node ids");
        assert_ne!(ids(&edits[0]), grade.nodes.iter().map(|n| n.id).collect::<Vec<_>>());
    }

    #[test]
    fn an_added_still_grade_keeps_each_clips_own_balance() {
        let mut seq = Sequence::new_default();
        let mut a = clip(0, 2);
        a.edit_grade(|g| g.ensure_corrector());
        let own: Vec<_> = a.grade().nodes.iter().map(|n| n.id).collect();
        seq.tracks[0].clips = vec![a.clone()];
        let mut look = klipp_nodes::NodeGraph::default();
        look.ensure_lut3d();
        let GradeApplied { edits, .. } = apply_grade(&seq, &look, &[], &[a.id], true);
        let EditCommand::SetNodeGraph { new_graph, .. } = &edits[0] else { panic!("a graph edit") };
        assert!(own.iter().all(|id| new_graph.nodes.iter().any(|n| n.id == *id)), "the clip's own nodes stay");
        assert!(new_graph.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Lut3D), "the look is added");
    }

    fn graded(c: &mut Clip) {
        c.edit_grade(|g| {
            let id = g.ensure_corrector();
            g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.exposure = Some(0.7);
        });
        assert!(!c.grade_is_identity());
    }

    #[test]
    fn a_split_shot_s_grade_goes_on_its_other_ungraded_pieces_as_one_step() {
        let mut seq = Sequence::new_default();
        let shot = Clip::media(klipp_core::MediaId::new(), t(0), t(9), t(10), t(19));
        let other = clip(9, 12);
        let id = shot.id;
        seq.tracks[0].clips = vec![shot, other.clone()];
        let (a, rest) = seq.tracks[0].split_at(id, t(3)).unwrap();
        let (b, c) = seq.tracks[0].split_at(rest.id, t(6)).unwrap();
        // The middle piece graded: the other two are its shot, the next
        // source's clip is not.
        graded(seq.tracks[0].find_clip_mut(b.id).unwrap());
        let mut pieces = seq.ungraded_shot_pieces(b.id);
        pieces.sort_by_key(|p| seq.all_clips().position(|c| c.id == *p));
        assert_eq!(pieces, vec![a.id, c.id]);
        assert_eq!(shot_pieces_offer(&seq, b.id), Some(2));
        assert_eq!(shot_pieces_offer(&seq, a.id), None, "an ungraded piece offers nothing");

        let grade = seq.all_clips().find(|x| x.id == b.id).unwrap().grade().clone();
        let GradeApplied { edits, .. } = apply_grade(&seq, &grade, &[], &pieces, false);
        let (after, undone) = apply(seq.clone(), &edits);
        let clip_of = |s: &Sequence, id| s.all_clips().find(|x| x.id == id).unwrap().clone();
        assert!(!clip_of(&after, a.id).grade_is_identity() && !clip_of(&after, c.id).grade_is_identity());
        assert!(clip_of(&after, other.id).grade_is_identity(), "another source's clip is left alone");
        assert!(clip_of(&undone, a.id).grade_is_identity() && clip_of(&undone, c.id).grade_is_identity(), "one undo takes it off both");
    }

    #[test]
    fn a_piece_with_its_own_grade_or_past_a_gap_is_not_offered_the_grade() {
        let mut seq = Sequence::new_default();
        let media = klipp_core::MediaId::new();
        let shot = Clip::media(media, t(0), t(6), t(0), t(6));
        let id = shot.id;
        // The same file again after a gap, its range carrying on.
        let later = Clip::media(media, t(7), t(9), t(6), t(8));
        seq.tracks[0].clips = vec![shot, later];
        let (a, b) = seq.tracks[0].split_at(id, t(3)).unwrap();
        graded(seq.tracks[0].find_clip_mut(a.id).unwrap());
        assert_eq!(seq.ungraded_shot_pieces(a.id), vec![b.id]);
        graded(seq.tracks[0].find_clip_mut(b.id).unwrap());
        assert!(seq.ungraded_shot_pieces(a.id).is_empty(), "a graded piece keeps its grade");
    }

    /// The edits applied, then undone, as the app's one step does.
    fn apply(seq: Sequence, edits: &[EditCommand]) -> (Sequence, Sequence) {
        let mut sequences = vec![seq];
        let mut pool = klipp_media::MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        let step = EditCommand::Batch(edits.to_vec());
        view.apply_forward(&step);
        let after = view.sequences[0].clone();
        view.apply_inverse(&step);
        (after, view.sequences[0].clone())
    }

    /// The node of `seq`'s clip `clip` labelled `colour`, if one is, and
    /// whether it is in its grade.
    fn labelled(seq: &Sequence, clip: klipp_core::ClipId, colour: u8) -> Option<klipp_nodes::NodeId> {
        let grade = klipp_timeline::grade_node_labels(&seq.node_labels, clip);
        let node = grade.iter().find(|g| g.colour == colour)?.nodes[0];
        seq.all_clips().find(|c| c.id == clip)?.grade().nodes.iter().any(|n| n.id == node).then_some(node)
    }

    /// #1866: a grade from the library or a still brings its node colour
    /// labels onto the nodes each clip took, in the same step: in place of
    /// the clip's own grade's labels, or beside them when added; undone,
    /// the clip has its own back.
    #[test]
    fn a_grades_node_labels_come_with_it_from_a_still_or_the_library() {
        use klipp_graph_format::path::{NodePath, Root};
        let mut seq = Sequence::new_default();
        let mut a = clip(0, 2);
        let own = a.edit_grade(|g| g.ensure_corrector());
        seq.tracks[0].clips = vec![a.clone()];
        seq.node_labels = vec![klipp_timeline::NodeLabel { node: NodePath::new(Root::Clip { clip: a.id.0 }, [klipp_timeline::grade_group_id(a.id), own]), colour: 1 }];
        let mut look = klipp_nodes::NodeGraph::default();
        let lut = look.ensure_lut3d();
        let labels = vec![klipp_timeline::GradeNodeLabel { nodes: vec![lut], colour: 5 }];

        let GradeApplied { edits, labels: label_edit, .. } = apply_grade(&seq, &look, &labels, &[a.id], false);
        assert!(label_edit.is_some(), "a labels edit for the same step");
        let (after, undone) = apply(seq.clone(), &edits.into_iter().chain(label_edit).collect::<Vec<_>>());
        let new = labelled(&after, a.id, 5).expect("the look's node labelled on the clip");
        assert_ne!(new, lut, "under the id it took there");
        assert!(labelled(&after, a.id, 1).is_none(), "the replaced grade's label is gone");
        assert_eq!(undone.node_labels, seq.node_labels, "undone, its own again");

        let GradeApplied { edits, labels: label_edit, .. } = apply_grade(&seq, &look, &labels, &[a.id], true);
        let (after, _) = apply(seq.clone(), &edits.into_iter().chain(label_edit).collect::<Vec<_>>());
        assert!(labelled(&after, a.id, 5).is_some() && labelled(&after, a.id, 1) == Some(own), "added: both");

        // A library entry or a still from before labels: applied cleanly,
        // added with no labels edit at all, put in place with only the
        // replaced grade's label taken off.
        let GradeApplied { edits, refused, labels: label_edit } = apply_grade(&seq, &look, &[], &[a.id], true);
        assert!(edits.len() == 1 && refused.is_empty() && label_edit.is_none());
        let GradeApplied { edits, labels: label_edit, .. } = apply_grade(&seq, &look, &[], &[a.id], false);
        let (after, _) = apply(seq.clone(), &edits.into_iter().chain(label_edit).collect::<Vec<_>>());
        assert!(after.node_labels.is_empty());
    }

    /// #1866: Paste Attributes brings the copied grade's node labels, by
    /// the ids its nodes took on each target, in the paste's step.
    #[test]
    fn pasted_attributes_bring_the_grades_node_labels() {
        use klipp_graph_format::path::{NodePath, Root};
        let mut seq = Sequence::new_default();
        let mut source = clip(0, 2);
        let node = source.edit_grade(|g| g.ensure_corrector());
        let target = clip(2, 4);
        seq.tracks[0].clips = vec![source.clone(), target.clone()];
        seq.node_labels = vec![klipp_timeline::NodeLabel { node: NodePath::new(Root::Clip { clip: source.id.0 }, [klipp_timeline::grade_group_id(source.id), node]), colour: 4 }];
        let attrs = crate::state::AttributesClipboard::of_in(&seq, &source);
        assert_eq!(attrs.grade_labels.len(), 1);
        let what = crate::state::PasteChoice { color_grade: true, ..Default::default() };
        let (edits, label_edit) = paste_attributes(&seq, &[target.id], &attrs, what);
        let (after, _) = apply(seq.clone(), &edits.into_iter().chain(label_edit).collect::<Vec<_>>());
        let new = labelled(&after, target.id, 4).expect("the pasted corrector is labelled");
        assert_ne!(new, node);
        let (_, none) = paste_attributes(&seq, &[target.id], &attrs, crate::state::PasteChoice { audio: true, ..Default::default() });
        assert!(none.is_none(), "no grade pasted, no labels");
    }

    /// A target that cannot take the grade is passed over with a reason
    /// to show, not counted as "nothing selected".
    #[test]
    fn a_still_grade_that_cannot_be_added_says_why() {
        use klipp_core::WorkingColorspace as Ws;
        let mut seq = Sequence::new_default();
        let mut a = clip(0, 2);
        a.edit_grade(|g| g.working_space_override = Some(Ws::Rec709Linear));
        seq.tracks[0].clips = vec![a.clone()];
        let mut look = klipp_nodes::NodeGraph::default();
        look.ensure_lut3d();
        look.working_space_override = Some(Ws::AcesCG);
        let GradeApplied { edits, refused, .. } = apply_grade(&seq, &look, &[], &[a.id], true);
        assert!(edits.is_empty());
        assert_eq!(refused, vec![klipp_nodes::CannotAppend::WorkingSpace { ours: Ws::Rec709Linear, theirs: Ws::AcesCG }]);
        assert!(refusals_line(&refused).contains("working space"));
    }

    #[test]
    fn a_record_timecode_reads_as_an_editor_would_say_it() {
        assert_eq!(record_timecode(3723.48, 25.0), "01:02:03:12");
    }
}

/// M3 step 17a, #1322: a grade version keeps its corrections' mix and
/// mask channel.
#[cfg(test)]
mod grade_versions_and_mix {
    use super::*;
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_graph_lift::ops::GraphOp;
    use klipp_timeline::GradeAccess;

    fn state() -> (crate::state::AppState, klipp_core::ClipId, NodePath) {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let mut g = klipp_nodes::NodeGraph::default();
        let corrector = g.ensure_corrector();
        clip.set_grade(g);
        let group = clip.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP).unwrap().id;
        let path = NodePath::new(Root::Clip { clip: clip.id.0 }, [group, corrector]);
        clip.apply_picture_ops(&[GraphOp::SetMix { path: path.clone(), mix: Some(0.25) }]).unwrap();
        let id = clip.id;
        let mut state = crate::state::AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequences[0].tracks[v1].clips.push(clip);
        (state, id, path)
    }

    fn clip(s: &crate::state::AppState, id: klipp_core::ClipId) -> &klipp_timeline::Clip {
        s.sequences[0].all_clips().find(|c| c.id == id).unwrap()
    }

    /// The corrector's mix and mask channel, as the clip's picture holds them.
    fn blend(s: &crate::state::AppState, id: klipp_core::ClipId, path: &NodePath) -> (Option<f32>, Option<klipp_graph_format::ParamValue>) {
        let picture = clip(s, id).picture_v2().unwrap();
        let group = picture.picture.graph.nodes.iter().find(|n| n.id == path.nodes[0]).unwrap();
        let n = group.group.as_ref().unwrap().graph.nodes.iter().find(|n| n.id == path.nodes[1]).unwrap();
        (n.mix, n.params.get(&klipp_graph_format::ParamKey::from("mask.channel")).cloned())
    }

    /// A grade version keeps its corrections' mix and mask channel
    /// (#1322): saved with them, each switch brings back the version's
    /// own (one with none clears the other's), and a saved and loaded
    /// project's versions still hold them.
    #[test]
    fn a_grade_version_keeps_its_corrections_mix_and_channel() {
        let (mut s, id, path) = state();
        let alpha = Some(klipp_graph_format::ParamValue::Text("alpha".into()));
        assert!(s.grade_version_command(&AppCommand::SaveGradeVersion { clip_id: id, name: "A".into() }));
        let c = s.sequences[0].all_clips_mut().find(|c| c.id == id).unwrap();
        c.apply_picture_ops(&[GraphOp::SetMix { path: path.clone(), mix: Some(0.75) }, GraphOp::SetMaskChannel { path: path.clone(), channel: Some("alpha".into()) }]).unwrap();
        assert!(s.grade_version_command(&AppCommand::SaveGradeVersion { clip_id: id, name: "B".into() }));
        let switch = |s: &mut crate::state::AppState, i: usize| assert!(s.grade_version_command(&AppCommand::SwitchGradeVersion { clip_id: id, version_index: i }), "{:?}", s.toast.message_for_test());
        for round in ["in the app", "saved and loaded"] {
            switch(&mut s, 0);
            assert_eq!(blend(&s, id, &path), (Some(0.25), None), "{round}: A's mix, no channel");
            switch(&mut s, 1);
            assert_eq!(blend(&s, id, &path), (Some(0.75), alpha.clone()), "{round}: B's");
            if round == "in the app" {
                let dir = std::env::temp_dir().join(format!("klipp-grade-versions-{}", uuid::Uuid::new_v4()));
                klipp_project::save_project(&dir, &s.sequences, &s.media_pool).unwrap();
                let (sequences, ..) = klipp_project::load_project_strict(&dir).unwrap();
                let _ = std::fs::remove_dir_all(&dir);
                s.sequences = sequences;
                assert_eq!(blend(&s, id, &path), (Some(0.75), alpha.clone()), "the clip as saved");
            }
        }
    }

    /// Copy Attributes notes a grade it can't carry whole, so its paste
    /// of the grade is refused rather than made at full strength.
    #[test]
    fn copied_attributes_know_the_grade_held_a_mix() {
        let (s, id, _) = state();
        assert!(crate::state::AttributesClipboard::of(clip(&s, id)).grade_mix_held);
        let plain = klipp_timeline::Clip::media(klipp_core::MediaId::new(), klipp_core::RationalTime::from_secs_f64(0.0, 25), klipp_core::RationalTime::from_secs_f64(1.0, 25), klipp_core::RationalTime::from_secs_f64(0.0, 25), klipp_core::RationalTime::from_secs_f64(1.0, 25));
        assert!(!crate::state::AttributesClipboard::of(&plain).grade_mix_held);
    }
}
