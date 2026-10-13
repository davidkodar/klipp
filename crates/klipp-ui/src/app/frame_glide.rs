//! Frame Glide (#611): a clip's move between two framings, set from the
//! viewer and the Video inspector. The rules are the timeline's
//! (`klipp_timeline::frame_glide`); here each change is one undo step,
//! and the viewer shows the glided clip without its move while it is
//! being framed.

use super::*;

impl KlippApp {
    pub(super) fn handle_frame_glide_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::SetFrameGlide { clip_id, glide, replace_animation } => {
                if self.refused_by_lock(clip_id) {
                    return true;
                }
                let seq = self.state.sequence();
                let (w, h) = (seq.resolution.width as f32, seq.resolution.height as f32);
                let Some(old) = seq.all_clips().find(|c| c.id == *clip_id).cloned() else { return true };
                match klipp_timeline::frame_glide::set(&old, *glide, *replace_animation, w, h) {
                    Ok(new) if changed(&old, &new) => {
                        let description = if glide.is_some() { "Frame Glide" } else { "Remove Frame Glide" };
                        self.apply_edits_as_one(&[EditCommand::ReplaceClip { description, old: Box::new(old), new: Box::new(new) }], description);
                        if glide.is_none() && self.state.frame_glide_clip == Some(*clip_id) {
                            self.state.frame_glide_clip = None;
                            self.sync_sequence_to_engine();
                        }
                        let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
                    }
                    Ok(_) => {}
                    Err(why) => self.state.toast.show(format!("Frame Glide not set: {why}")),
                }
                true
            }
            AppCommand::SetFrameGlideMode(clip) => {
                self.state.frame_glide_clip = *clip;
                self.state.frame_glide_drag = None;
                // The clip framed is the one on screen: a playhead outside
                // it goes to its first frame.
                if let Some(c) = clip.and_then(|id| self.state.sequence().all_clips().find(|c| c.id == id)) {
                    if self.state.playhead < c.timeline_in || self.state.playhead >= c.timeline_out {
                        self.state.playhead = c.timeline_in;
                    }
                }
                // The viewer shows the clip without its move while it is
                // framed, and with it again after.
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
                true
            }
            _ => false,
        }
    }
}

/// Whether a glide edit changed anything it can change: no undo step for
/// a drag let go where it began.
fn changed(old: &klipp_timeline::Clip, new: &klipp_timeline::Clip) -> bool {
    use klipp_timeline::frame_glide::lane;
    old.frame_glide != new.frame_glide || ["pos_x", "pos_y", "scale", "scale_y"].iter().any(|k| lane(old, k) != lane(new, k))
}

/// The viewer's copy of `flat` with the clip being framed shown without
/// its move (#611): its position and scale held at rest, so both
/// framings are drawn over the whole picture. The clip itself is
/// unchanged.
pub(super) fn show_unmoved(flat: &mut klipp_timeline::Sequence, framing: Option<klipp_core::ClipId>) {
    let Some(id) = framing else { return };
    if let Some(clip) = flat.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == id) {
        klipp_timeline::frame_glide::hold_at_rest(clip);
    }
}

#[cfg(test)]
mod tests {
    use super::super::viewer_harness::Viewer;
    use klipp_core::frame_glide::{FrameGlide, GlideFrame};
    use klipp_core::AppCommand;

    /// A viewer framing a media clip's glide (#611).
    fn framing() -> (Viewer, klipp_core::ClipId) {
        let mut v = Viewer::new();
        v.state.active_tool = klipp_core::Tool::Select;
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let id = clip.id;
        v.state.sequences[0].tracks[0].clips = vec![clip];
        v.state.frame_glide_clip = Some(id);
        v.frame(Vec::new());
        (v, id)
    }

    /// #611: in the viewer, the Start framing's corner dragged to the
    /// frame's middle sizes it about the opposite corner, sent as one edit
    /// when let go; a drag in the End framing moves it, and Esc leaves.
    #[test]
    fn dragging_a_framing_sets_the_glide_in_one_edit() {
        let (mut v, id) = framing();
        let r = v.image_rect();
        // Both framings start as the whole frame: its top-left corner is
        // End's, which lies over Start.
        v.press(r.left_top() + egui::vec2(2.0, 2.0));
        for k in 1..=4 {
            let to = r.left_top() + (r.center() - r.left_top()) * (k as f32 / 4.0);
            v.frame(vec![egui::Event::PointerMoved(to)]);
        }
        assert!(v.sent.iter().all(|c| !matches!(c, AppCommand::SetFrameGlide { .. })), "nothing sent before the release");
        v.release(r.center());
        let glides: Vec<&Option<FrameGlide>> = v.sent.iter().filter_map(|c| match c {
            AppCommand::SetFrameGlide { clip_id, glide, replace_animation: false } if *clip_id == id => Some(glide),
            _ => None,
        }).collect();
        let [Some(g)] = glides[..] else { panic!("one edit: {:?}", v.sent) };
        assert_eq!(g.start, GlideFrame::FULL, "Start untouched");
        assert!((g.end.width - 0.5).abs() < 0.02 && (g.end.x - 0.25).abs() < 0.02 && (g.end.y - 0.25).abs() < 0.02, "End sized to the bottom-right quarter: {:?}", g.end);

        // A press and release where it was pressed sends nothing.
        let sent = v.sent.len();
        v.click(r.center());
        assert_eq!(v.sent.len(), sent, "{:?}", &v.sent[sent..]);

        v.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        assert!(v.sent.iter().any(|c| matches!(c, AppCommand::SetFrameGlideMode(None))), "Esc leaves the mode");
    }
}
