//! Rate stretch on the timeline (#3055): the grip at each top corner of
//! a clip, and the drag it starts, in which the clip keeps the part of
//! its file it shows and changes its speed to fill the new length.
//!
//! The drag previews through the same planner its release commits
//! (`klipp_document::ops::stretch_at`), against the sequence as it was
//! when the drag began, so what is drawn while dragging is what lands.

use klipp_core::{ClipId, Timecode};
use klipp_document::ops::{stretch_at, stretch_range, Edge, RefusalCode, StretchTo};
use klipp_timeline::{Clip, Sequence};

/// The grip: this wide in from the clip's edge, this tall from its top.
pub const GRIP_W: f32 = 12.0;
pub const GRIP_H: f32 = 16.0;
/// The narrowest clip that shows its grips: on a narrower one the Rate
/// Stretch tool and Ctrl on an edge still stretch it.
pub const GRIP_MIN_W: f32 = 48.0;
/// How far in from the clip's edge a fade with no length has its handle,
/// beside the grip rather than under it.
pub const FADE_INSET: f32 = GRIP_W + 2.0;

/// Whether a clip drawn in `cr` shows its grips.
pub fn grip_shown(cr: egui::Rect) -> bool {
    cr.width() >= GRIP_MIN_W && cr.height() >= GRIP_H + 8.0
}

/// The grip at the clip's end (`at_end`) or start.
pub fn grip_rect(cr: egui::Rect, at_end: bool) -> egui::Rect {
    let x = if at_end { cr.right() - GRIP_W } else { cr.left() };
    egui::Rect::from_min_size(egui::pos2(x, cr.top()), egui::vec2(GRIP_W, GRIP_H))
}

/// Which grip of a clip drawn in `cr` is at `pos`: `Some(true)` its end's.
pub fn grip_at(cr: egui::Rect, pos: egui::Pos2) -> Option<bool> {
    if !grip_shown(cr) {
        return None;
    }
    [true, false].into_iter().find(|at_end| grip_rect(cr, *at_end).contains(pos))
}

/// Where a clip's fade handles are drawn and taken, `fade_in_px` and
/// `fade_out_px` in from its edges: never closer to an edge than the
/// grip beside it.
pub fn fade_handle_xs(cr: egui::Rect, fade_in_px: f32, fade_out_px: f32) -> (f32, f32) {
    let inset = if grip_shown(cr) { FADE_INSET } else { 0.0 };
    (cr.left() + fade_in_px.max(inset), cr.right() - fade_out_px.max(inset))
}

/// What the hint by a grip says: a clip that plays at a speed changes
/// it; a title or still changes length only.
pub fn grip_hint(clip: &Clip, pool: &klipp_media::MediaPool) -> &'static str {
    if klipp_document::ops::plays_at_speed(clip, pool) { "Drag to change speed" } else { "Drag to change length" }
}

/// Where the drag's readout of `size` goes: above the track, to the
/// right of the pointer, or to its left where it would run past the
/// timeline's right edge; always inside the timeline's visible area.
pub fn readout_rect(content: egui::Rect, track: egui::Rect, pointer: egui::Pos2, size: egui::Vec2) -> egui::Rect {
    let right = pointer.x + 12.0;
    let x = if right + size.x <= content.right() { right } else { pointer.x - 12.0 - size.x };
    let x = x.min(content.right() - size.x).max(content.left());
    let y = (track.top() - size.y - 4.0).max(content.top()).min(content.bottom() - size.y);
    egui::Rect::from_min_size(egui::pos2(x, y), size)
}

/// The speed, in percent, of a clip a stretch left slowed with nothing
/// drawing its in-between frames: where smooth motion is offered, as the
/// clip's speed settings offer it. `None` for one at full speed or
/// faster, ramped, already blending, or without a frame rate.
pub fn smooth_offer(clip: &Clip, pool: &klipp_media::MediaPool) -> Option<f64> {
    let slowed = klipp_document::ops::plays_at_speed(clip, pool) && clip.speed.abs() < 1.0 - 1e-9 && clip.speed_keyframes.is_empty();
    (slowed && !clip.has_frame_blend_entry()).then(|| clip.speed.abs() * 100.0)
}

/// A stretch being dragged.
pub struct StretchState {
    /// The clip dragged, then the other selected clips whose same edge is
    /// at the same time.
    pub clip_ids: Vec<ClipId>,
    pub track_idx: usize,
    pub at_end: bool,
    pub start_x: f32,
    /// The dragged edge where the drag began.
    pub edge_was: Timecode,
    /// The sequence as it was: what each frame plans against, and what
    /// is put back on release.
    original: Sequence,
    /// Where the edge last landed with a plan, and that plan's ripple.
    pub to: Option<Timecode>,
    pub ripple: bool,
    pub keep_pitch: bool,
    /// Why the edge stopped short, said in the readout.
    pub note: Option<String>,
    pub hold: Option<crate::timeline::snap::SnapHold>,
}

impl StretchState {
    /// A stretch of `clip` from its end (`at_end`) or start, with the
    /// `selected` clips whose same edge is at the same time.
    pub fn begin(seq: &Sequence, clip: &Clip, track_idx: usize, at_end: bool, start_x: f32, selected: &[ClipId]) -> Self {
        let edge = |c: &Clip| if at_end { c.timeline_out } else { c.timeline_in };
        let half = klipp_timeline::one_frame(seq.fps).to_secs_f64() / 2.0;
        let mut clip_ids = vec![clip.id];
        for c in seq.all_clips().filter(|c| c.id != clip.id && selected.contains(&c.id) && !c.kind.is_gap()) {
            if (edge(c).to_secs_f64() - edge(clip).to_secs_f64()).abs() < half && Some(c.id) != clip.linked_clip_id {
                clip_ids.push(c.id);
            }
        }
        Self { clip_ids, track_idx, at_end, start_x, edge_was: edge(clip), original: seq.clone(), to: None, ripple: false, keep_pitch: true, note: None, hold: None }
    }

    fn edge(&self) -> Edge {
        if self.at_end { Edge::Out } else { Edge::In }
    }

    fn plan(&self, pool: &klipp_media::MediaPool, to: Timecode) -> Result<klipp_document::history::EditCommand, klipp_document::ops::Refusal> {
        stretch_at(&self.original, pool, &self.clip_ids, self.edge(), StretchTo::EdgeAt(to.to_secs_f64()), self.ripple, self.keep_pitch).map(|(_, edit, _)| edit)
    }

    /// The edge at `to`, held inside the speeds the clips can take: where
    /// it lands and, at a limit, which.
    fn within_limits(&self, pool: &klipp_media::MediaPool, to: Timecode) -> (Timecode, Option<&'static str>) {
        let Some(first) = self.original.all_clips().find(|c| c.id == self.clip_ids[0]) else { return (to, None) };
        let members: Vec<&Clip> = self.original.all_clips().filter(|c| self.clip_ids.contains(&c.id) || self.clip_ids.iter().any(|id| c.linked_clip_id == Some(*id))).collect();
        let (lo, hi) = stretch_range(members, pool);
        let (fixed, moving) = if self.at_end || self.ripple { (first.timeline_in, first.timeline_out) } else { (first.timeline_out, first.timeline_in) };
        let (a, e) = (fixed.to_secs_f64(), moving.to_secs_f64());
        let f = (to.to_secs_f64() - a) / (e - a);
        let fps = self.original.fps;
        // Put on the frame inside the limit, so the plan takes it.
        let at = |f: f64, inward: bool| {
            let frames = (a + (e - a) * f) * fps.as_f64();
            let frames = if inward == (e > a) { frames.floor() } else { frames.ceil() };
            Timecode::from_frames(frames as i64, fps.rate()).rescale_to(to.rate)
        };
        if f > hi {
            (at(hi, true), Some("at the 10% limit"))
        } else if f < lo {
            (at(lo, false), Some("at the 1600% limit"))
        } else {
            (to, None)
        }
    }

    /// The drag at `to`: `seq` drawn as the stretch would leave it, or as
    /// its last place that planned, with why it stopped there.
    pub fn preview(&mut self, seq: &mut Sequence, pool: &klipp_media::MediaPool, to: Timecode) {
        let (to, limit) = self.within_limits(pool, to);
        match self.plan(pool, to) {
            Ok(edit) => {
                self.show(seq, pool, &edit);
                self.to = Some(to);
                self.note = limit.map(str::to_owned);
            }
            Err(r) => {
                // Short of the clip it would run into, or the limit, the
                // last place that planned stays drawn.
                self.note = Some(match r.code {
                    Some(RefusalCode::WouldOverlap) => format!("Stops here: {}", r.why),
                    _ => r.why,
                });
            }
        }
    }

    fn show(&self, seq: &mut Sequence, pool: &klipp_media::MediaPool, edit: &klipp_document::history::EditCommand) {
        let mut seqs = vec![self.original.clone()];
        let (mut pool, mut fps) = (pool.clone(), self.original.fps);
        klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps }.apply_forward(edit);
        seq.tracks = seqs.remove(0).tracks;
    }

    /// The drag let go: the sequence put back as it was, and the stretch
    /// to commit, when the edge moved.
    pub fn finish(&self, seq: &mut Sequence) -> Option<klipp_core::AppCommand> {
        seq.tracks = self.original.tracks.clone();
        let to = self.to.filter(|t| t.to_secs_f64() != self.edge_was.to_secs_f64())?;
        Some(klipp_core::AppCommand::StretchClip { clip_ids: self.clip_ids.clone(), at_end: self.at_end, to, ripple: self.ripple, keep_pitch: self.keep_pitch })
    }

    /// The readout's lines for the drag as drawn in `seq`: the dragged
    /// clip's speed and length, then why it stopped or what follows.
    pub fn readout(&self, seq: &Sequence, pool: &klipp_media::MediaPool, tc: &klipp_core::TimecodeDisplay) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(c) = seq.all_clips().find(|c| c.id == self.clip_ids[0]) {
            if klipp_document::ops::plays_at_speed(c, pool) {
                out.push(if c.speed_keyframes.is_empty() { format!("Speed {:.1}%", c.speed.abs() * 100.0) } else { "Speed ramp scaled".into() });
            } else {
                out.push("Length only".into());
            }
            out.push(format!("Dur: {}", tc.duration(c.duration())));
        }
        if self.clip_ids.len() > 1 {
            out.push(format!("{} clips together", self.clip_ids.len()));
        }
        out.push(self.note.clone().unwrap_or_else(|| if self.ripple { "Ripple: later clips move".into() } else { "Alt: ripple".into() }));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(s: f64) -> Timecode {
        Timecode::from_secs_f64(s, 25)
    }

    /// A 4 s clip of a 10 s file on V1, a clip after it from 6 s.
    fn fixture() -> (Sequence, klipp_media::MediaPool, ClipId) {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/stretch-drag.mov".into(), None);
        item.duration = tc(10.0);
        let media = pool.add(item);
        let clip = Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let id = clip.id;
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let mut v = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v.clips = vec![clip, Clip::media(media, tc(6.0), tc(9.0), tc(5.0), tc(8.0))];
        seq.tracks = vec![v];
        (seq, pool, id)
    }

    /// The grips are the top corners, on a clip wide enough; a fade with
    /// no length has its handle beside the grip.
    #[test]
    fn the_grips_are_the_top_corners_and_the_fade_handles_sit_beside_them() {
        let cr = egui::Rect::from_min_size(egui::pos2(100.0, 20.0), egui::vec2(200.0, 46.0));
        assert_eq!(grip_at(cr, egui::pos2(295.0, 24.0)), Some(true), "the end's");
        assert_eq!(grip_at(cr, egui::pos2(104.0, 30.0)), Some(false), "the start's");
        assert_eq!(grip_at(cr, egui::pos2(295.0, 50.0)), None, "the edge below it trims");
        assert_eq!(grip_at(cr, egui::pos2(200.0, 24.0)), None, "the body");
        let narrow = egui::Rect::from_min_size(egui::pos2(100.0, 20.0), egui::vec2(30.0, 46.0));
        assert_eq!(grip_at(narrow, egui::pos2(125.0, 24.0)), None, "too narrow to show one");
        assert_eq!(fade_handle_xs(cr, 0.0, 0.0), (100.0 + FADE_INSET, 300.0 - FADE_INSET));
        assert_eq!(fade_handle_xs(cr, 40.0, 30.0), (140.0, 270.0), "a longer fade where it is");
    }

    /// Smooth motion is offered for a clip slowed with nothing blending,
    /// not for one at full speed or faster, or a title.
    #[test]
    fn smooth_motion_is_offered_for_a_slowed_clip_only() {
        let (seq, pool, id) = fixture();
        let mut c = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        assert_eq!(smooth_offer(&c, &pool), None, "full speed");
        c.speed = 0.5;
        assert_eq!(smooth_offer(&c, &pool), Some(50.0));
        c.speed = 2.0;
        assert_eq!(smooth_offer(&c, &pool), None, "faster");
        let blended = klipp_timeline::test_clip::TestClip::of(Clip { speed: 0.5, ..c.clone() }).effect(klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 2 }).build();
        assert_eq!(smooth_offer(&blended, &pool), None, "already following the motion");
    }

    /// The readout stays inside the timeline: to the right of the
    /// pointer where it fits, flipped to its left near the right edge,
    /// held inside on a timeline narrower than it.
    #[test]
    fn the_readout_stays_inside_the_timeline() {
        let content = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(900.0, 400.0));
        let track = egui::Rect::from_min_max(egui::pos2(100.0, 60.0), egui::pos2(900.0, 110.0));
        let size = egui::vec2(180.0, 50.0);
        for x in [101.0, 300.0, 700.0, 760.0, 890.0, 899.0] {
            let r = readout_rect(content, track, egui::pos2(x, 80.0), size);
            assert!(content.contains_rect(r), "at {x}: {r:?}");
            assert!(r.left() > x || r.right() < x, "beside the pointer at {x}, not under it: {r:?}");
        }
        assert!(readout_rect(content, track, egui::pos2(890.0, 80.0), size).right() < 890.0, "flipped left at the edge");
        let narrow = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(250.0, 400.0));
        assert!(readout_rect(narrow, track, egui::pos2(200.0, 80.0), size).left() >= 100.0);
    }

    /// The drag previews the planner's stretch, stops at the next clip
    /// without ripple and says so, holds at the speed limit, and puts the
    /// sequence back on release with the stretch to commit.
    #[test]
    fn the_drag_previews_what_lands_and_stops_where_it_must() {
        let (mut seq, pool, id) = fixture();
        let clip = seq.all_clips().find(|c| c.id == id).unwrap().clone();
        let mut drag = StretchState::begin(&seq, &clip, 0, true, 0.0, &[]);
        drag.preview(&mut seq, &pool, tc(5.0));
        let shown = seq.all_clips().find(|c| c.id == id).unwrap();
        assert!((shown.timeline_out.to_secs_f64() - 5.0).abs() < 1e-9 && (shown.speed - 0.8).abs() < 1e-9);
        let tc_display = klipp_core::TimecodeDisplay::plain(seq.fps);
        assert_eq!(drag.readout(&seq, &pool, &tc_display)[0], "Speed 80.0%");
        // Into the next clip: the last place stays, and why is said.
        drag.preview(&mut seq, &pool, tc(8.0));
        assert!((seq.all_clips().find(|c| c.id == id).unwrap().timeline_out.to_secs_f64() - 5.0).abs() < 1e-9);
        assert!(drag.note.as_deref().unwrap_or_default().starts_with("Stops here"), "{:?}", drag.note);
        // With ripple, past the slowest speed: held at 10%, 40 s long.
        drag.ripple = true;
        drag.preview(&mut seq, &pool, tc(100.0));
        let shown = seq.all_clips().find(|c| c.id == id).unwrap();
        assert!((shown.timeline_out.to_secs_f64() - 40.0).abs() < 1e-9 && (shown.speed - 0.1).abs() < 1e-9, "{} s at {}", shown.timeline_out.to_secs_f64(), shown.speed);
        assert_eq!(drag.note.as_deref(), Some("at the 10% limit"));
        let cmd = drag.finish(&mut seq);
        assert!(matches!(cmd, Some(klipp_core::AppCommand::StretchClip { at_end: true, ripple: true, .. })), "{cmd:?}");
        assert_eq!(seq.all_clips().find(|c| c.id == id).unwrap().timeline_out, tc(4.0), "put back for the commit");
    }
}
