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

/// A still's grade onto each of `targets`, fresh node ids per clip, as
/// graph edits to apply together: in place of each clip's grade, or with
/// `append` after it, keeping the clip's own balance. With the edits,
/// why any target was passed over, once per reason.
pub(super) fn grade_from_still_edits(
    seq: &klipp_timeline::Sequence,
    grade: &klipp_nodes::NodeGraph,
    targets: &[klipp_core::ClipId],
    append: bool,
) -> (Vec<EditCommand>, Vec<klipp_nodes::CannotAppend>) {
    let mut refused = Vec::new();
    let edits = seq
        .all_clips()
        .filter(|c| targets.contains(&c.id))
        .filter_map(|c| {
            let new_graph = if append {
                let mut g = c.grade().clone();
                if let Err(why) = g.append(grade) {
                    if !refused.contains(&why) {
                        refused.push(why);
                    }
                    return None;
                }
                g
            } else {
                c.grade().with_grade_of(grade)
            };
            Some(EditCommand::SetNodeGraph { clip_id: c.id, old_graph: c.grade().clone(), new_graph })
        })
        .collect();
    (edits, refused)
}

/// One line for a toast out of the reasons targets were passed over.
pub(super) fn refusals_line(refused: &[klipp_nodes::CannotAppend]) -> String {
    refused.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("; ")
}

/// The edits a Paste Attributes makes: each target clip replaced whole
/// by itself with the chosen attributes written on. Free of the app so
/// it can be tested without one.
pub(super) fn paste_attributes_edits(
    seq: &klipp_timeline::Sequence,
    targets: &[klipp_core::ClipId],
    attrs: &crate::state::AttributesClipboard,
    what: crate::state::PasteChoice,
) -> Vec<EditCommand> {
    seq.all_clips()
        .filter(|c| targets.contains(&c.id))
        .map(|c| {
            let mut new = c.clone();
            attrs.paste_onto(&mut new, what);
            EditCommand::ReplaceClip { description: "Paste Attributes", old: Box::new(c.clone()), new: Box::new(new) }
        })
        .collect()
}

impl crate::state::AppState {
    /// Save or switch a clip's grade version (`SaveGradeVersion`,
    /// `SwitchGradeVersion`): whether the clip changed. A clip whose
    /// picture holds a correction's mix or mask channel (M3 step 17a) is
    /// refused, saying why: a version keeps the grade's fields, never
    /// those, so it would come back without them.
    pub(super) fn grade_version_command(&mut self, cmd: &AppCommand) -> bool {
        let clip_id = match cmd {
            AppCommand::SaveGradeVersion { clip_id, .. } | AppCommand::SwitchGradeVersion { clip_id, .. } => *clip_id,
            _ => return false,
        };
        let seq = &mut self.sequences[self.active_sequence_idx];
        let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else { return false };
        if clip.holds_grade_mix() {
            self.toast.show("Grade versions don't keep a correction's mix or mask channel yet: set them back to whole and red first");
            return false;
        }
        match cmd {
            AppCommand::SaveGradeVersion { name, .. } => {
                let version = klipp_timeline::GradeVersion::new(name.clone(), clip.grade().clone());
                let index = clip.edit_grade_versions(|vs| {
                    vs.push(version);
                    vs.len() - 1
                });
                clip.set_active_grade_version(Some(index));
            }
            AppCommand::SwitchGradeVersion { version_index, .. } => {
                // Save current graph back to active version slot before switching
                if let Some(active_idx) = clip.active_grade_version() {
                    if active_idx < clip.grade_versions().len() {
                        let grade = clip.grade().clone();
                        clip.edit_grade_versions(|vs| vs[active_idx].set_grade(grade));
                    }
                }
                // Load the target version
                if let Some(grade) = clip.grade_versions().get(*version_index).map(|v| v.grade().clone()) {
                    clip.set_grade(grade);
                    clip.set_active_grade_version(Some(*version_index));
                }
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
        let edits = paste_attributes_edits(&seq, &targets, &attrs, what);
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
        let (edits, _) = grade_from_still_edits(&seq, &grade, &[a.id, c.id], false);
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
        let (edits, _) = grade_from_still_edits(&seq, &look, &[a.id], true);
        let EditCommand::SetNodeGraph { new_graph, .. } = &edits[0] else { panic!("a graph edit") };
        assert!(own.iter().all(|id| new_graph.nodes.iter().any(|n| n.id == *id)), "the clip's own nodes stay");
        assert!(new_graph.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Lut3D), "the look is added");
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
        let (edits, refused) = grade_from_still_edits(&seq, &look, &[a.id], true);
        assert!(edits.is_empty());
        assert_eq!(refused, vec![klipp_nodes::CannotAppend::WorkingSpace { ours: Ws::Rec709Linear, theirs: Ws::AcesCG }]);
        assert!(refusals_line(&refused).contains("working space"));
    }

    #[test]
    fn a_record_timecode_reads_as_an_editor_would_say_it() {
        assert_eq!(record_timecode(3723.48, 25.0), "01:02:03:12");
    }
}

/// M3 step 17a: grade versions don't keep a correction's mix or mask
/// channel, so a clip holding one refuses to save or switch a version,
/// saying why, with its picture and versions as they were; set back to
/// whole and red, it saves one.
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

    #[test]
    fn a_mixed_grade_refuses_to_save_or_switch_a_version() {
        let (mut s, id, path) = state();
        let (picture, versions) = (clip(&s, id).picture_v2().unwrap(), clip(&s, id).grade_versions().len());
        for cmd in [AppCommand::SaveGradeVersion { clip_id: id, name: "A".into() }, AppCommand::SwitchGradeVersion { clip_id: id, version_index: 0 }] {
            assert!(!s.grade_version_command(&cmd), "{cmd:?} is refused");
            assert!(s.toast.message_for_test().is_some_and(|m| m.contains("set them back to whole and red")), "{:?}", s.toast.message_for_test());
            assert_eq!(clip(&s, id).picture_v2().unwrap(), picture, "the picture as it was");
            assert_eq!(clip(&s, id).grade_versions().len(), versions, "and its versions");
        }
        // Set back to whole and red, as the refusal says: saved.
        let c = s.sequences[0].all_clips_mut().find(|c| c.id == id).unwrap();
        c.apply_picture_ops(&[GraphOp::SetMix { path, mix: Some(1.0) }]).unwrap();
        assert!(s.grade_version_command(&AppCommand::SaveGradeVersion { clip_id: id, name: "A".into() }));
        assert_eq!(clip(&s, id).grade_versions().len(), versions + 1);
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
