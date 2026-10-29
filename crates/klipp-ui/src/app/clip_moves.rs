//! Clips moved from the keyboard (#917): the selection nudged a frame or
//! ten (Alt+, / Alt+., with Shift ten), by a typed +N / -N, or to the
//! next track of its kind above or below (Alt+Up / Alt+Down). Each is
//! the document's own move, so a move that would overlap a clip is
//! refused with the reason, and each is one undo step.

use super::*;
use klipp_document::ops::{Anchor, ClipRef, Op, TrackRef};

impl KlippApp {
    /// The keyboard-move commands: `false` for any other.
    pub(super) fn handle_clip_move_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::NudgeClips { frames } => {
                self.nudge_selected_clips(*frames);
                true
            }
            AppCommand::MoveClipsToTrack { up } => {
                self.move_selected_clips_to_track(*up);
                true
            }
            AppCommand::MoveClips { moves } => {
                let ops = drag_move_ops(self.state.sequence(), moves, self.state.linked_selection);
                self.apply_move_ops(ops, "Move Clips");
                true
            }
            _ => false,
        }
    }

    /// The selected clips, each pair once when its partner follows.
    fn clips_to_move(&self) -> Vec<klipp_timeline::Clip> {
        let linked = self.state.linked_selection;
        let mut ids: Vec<ClipId> = self.state.selected_clips.clone();
        if let Some(id) = self.state.selected_clip {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let seq = self.state.sequence();
        let mut out: Vec<klipp_timeline::Clip> = Vec::new();
        for id in ids {
            let Some(clip) = seq.all_clips().find(|c| c.id == id) else { continue };
            if linked && clip.linked_clip_id.is_some_and(|p| out.iter().any(|c| c.id == p)) {
                continue;
            }
            out.push(clip.clone());
        }
        out
    }

    fn apply_move_ops(&mut self, ops: Vec<Op>, label: &str) {
        if ops.is_empty() {
            self.state.toast.show("Select clips to move first");
            return;
        }
        match klipp_document::ops::resolve(self.state.sequence(), &self.state.media_pool, &ops) {
            Ok(plan) => {
                let edits: Vec<EditCommand> = plan.steps.into_iter().map(|s| s.edit).collect();
                self.apply_edits_as_one(&edits, label);
            }
            Err(refusal) => self.state.toast.show(format!("Move unchanged: {}", refusal.why)),
        }
    }

    /// The selected clips moved `frames` later, the one moving into the
    /// space first so they never meet on the way.
    fn nudge_selected_clips(&mut self, frames: i64) {
        if frames == 0 {
            return;
        }
        let fps = self.state.sequence().fps.as_f64();
        let linked = self.state.linked_selection;
        let mut clips = self.clips_to_move();
        clips.sort_by(|a, b| a.timeline_in.to_secs_f64().total_cmp(&b.timeline_in.to_secs_f64()));
        if frames > 0 {
            clips.reverse();
        }
        let ops = clips
            .iter()
            .map(|c| {
                let at = (c.timeline_in.to_secs_f64() * fps).round() as i64 + frames;
                Op::Move { clip_id: ClipRef::Id(c.id), to: Anchor::Frame(at.max(0)), track: None, linked }
            })
            .collect();
        self.apply_move_ops(ops, "Nudge");
    }

    /// The selected clips moved to the next track of their kind above
    /// (`up`) or below, at the same time; locked tracks are passed over.
    fn move_selected_clips_to_track(&mut self, up: bool) {
        let fps = self.state.sequence().fps.as_f64();
        let linked = self.state.linked_selection;
        let seq = self.state.sequence();
        let mut ops = Vec::new();
        for clip in self.clips_to_move() {
            let Some((ti, _)) = seq.find_clip(clip.id) else { continue };
            let kind = seq.tracks[ti].kind;
            let candidates: Vec<usize> = if up { (0..ti).rev().collect() } else { (ti + 1..seq.tracks.len()).collect() };
            let Some(dest) = candidates.into_iter().find(|&t| seq.tracks[t].kind == kind && !seq.tracks[t].locked) else {
                self.state.toast.show(format!("No unlocked {:?} track {} it", kind, if up { "above" } else { "below" }));
                return;
            };
            let at = (clip.timeline_in.to_secs_f64() * fps).round() as i64;
            ops.push(Op::Move { clip_id: ClipRef::Id(clip.id), to: Anchor::Frame(at), track: Some(TrackRef::Index(dest)), linked });
        }
        self.apply_move_ops(ops, if up { "Move Up a Track" } else { "Move Down a Track" });
    }
}

/// A drag's moves as the document's moves (#3060), the clip moving into
/// the space taken first so none meets another on the way: the latest
/// first when they move later, the earliest first when earlier.
pub(super) fn drag_move_ops(seq: &klipp_timeline::Sequence, moves: &[(ClipId, klipp_core::Timecode, Option<usize>)], linked: bool) -> Vec<Op> {
    let fps = seq.fps.as_f64();
    let mut moves: Vec<(ClipId, klipp_core::Timecode, Option<usize>, f64)> = moves
        .iter()
        .filter_map(|&(id, to, track)| seq.all_clips().find(|c| c.id == id).map(|c| (id, to, track, c.timeline_in.to_secs_f64())))
        .collect();
    let later = moves.first().is_some_and(|m| m.1.to_secs_f64() > m.3);
    moves.sort_by(|a, b| a.3.total_cmp(&b.3));
    if later {
        moves.reverse();
    }
    moves
        .into_iter()
        .map(|(id, to, track, _)| Op::Move { clip_id: ClipRef::Id(id), to: Anchor::Frame((to.to_secs_f64() * fps).round() as i64), track: track.map(TrackRef::Index), linked })
        .collect()
}
