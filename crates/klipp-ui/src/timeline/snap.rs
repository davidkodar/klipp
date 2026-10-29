//! One snap for every timeline drag (#3056): the targets, the distance
//! and Shift are the same whether a clip is moved, trimmed or dropped.
//!
//! A [`Snapper`] is made once a frame. [`Snapper::snap`] takes the edges
//! being dragged (a moved clip's in and out, a trimmed clip's one edge)
//! and the clips that move with them, which are never targets, and gives
//! the nearest target any edge is within the distance of. A snap, once
//! made, holds until its edge is past [`HOLD`] times the distance from
//! it, so a small movement of the hand doesn't let it go.

use crate::state::AppState;
use klipp_core::{ClipId, RationalTime, SnapConfig, SnapTargetKind};

/// How far past the snap distance a held snap lets go, as a multiple.
pub const HOLD: f64 = 1.5;

/// A point an edge snaps to: when, whose (a clip's edge names its clip),
/// and what it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapTarget {
    pub at: RationalTime,
    pub owner: Option<ClipId>,
    pub kind: SnapTargetKind,
}

/// A snap made: which of the dragged edges (an index into the edges
/// given), the time it lands on, and the target's kind and clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snap {
    pub edge: usize,
    pub at: RationalTime,
    pub kind: SnapTargetKind,
    pub owner: Option<ClipId>,
}

/// The snap a drag holds, kept in the drag's state from frame to frame;
/// `None` when it starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapHold(Snap);

/// The targets of one frame, the snap distance at its zoom, and whether
/// it snaps at all (snapping on and Shift not held).
pub struct Snapper {
    targets: Vec<SnapTarget>,
    /// The snap distance in seconds: the Preferences distance, in
    /// interface points, at this zoom.
    pub threshold_secs: f64,
    grid_secs: Option<f64>,
    rate: u32,
    config: SnapConfig,
    pub enabled: bool,
    zoom: f32,
    scroll: f32,
    /// Each clip's linked partner and group, for leaving out every clip
    /// that moves with one excluded.
    links: Vec<(ClipId, Option<ClipId>, Option<klipp_core::GroupId>)>,
    linked_selection: bool,
}

impl Snapper {
    /// The sequence's targets at `zoom` (points per second); `shift`
    /// held, or snapping off, it never snaps.
    pub fn of(state: &AppState, zoom: f32, shift: bool) -> Self {
        let seq = state.sequence();
        let rate = seq.fps.rate();
        let mut targets = Vec::new();
        let mut links = Vec::new();
        for clip in seq.all_clips() {
            for at in [clip.timeline_in, clip.timeline_out] {
                targets.push(SnapTarget { at, owner: Some(clip.id), kind: SnapTargetKind::ClipEdge });
            }
            links.push((clip.id, clip.linked_clip_id, clip.group_id));
        }
        let mut at = |at: RationalTime, kind| targets.push(SnapTarget { at, owner: None, kind });
        at(state.playhead, SnapTargetKind::Playhead);
        at(RationalTime { value: 0, rate }, SnapTargetKind::SequenceStart);
        if let Some(i) = seq.in_point {
            at(i, SnapTargetKind::InPoint);
        }
        if let Some(o) = seq.out_point {
            at(o, SnapTargetKind::OutPoint);
        }
        // A range marker's end too.
        for m in &seq.markers {
            at(m.position, SnapTargetKind::Marker);
            if let Some(d) = m.duration {
                at(m.position + d, SnapTargetKind::Marker);
            }
        }
        let zoom = zoom.max(1e-6);
        Snapper {
            targets,
            threshold_secs: state.snap_threshold_px.max(0.0) as f64 / zoom as f64,
            grid_secs: crate::timeline::grid_snap::resolve_grid_interval_secs(state.grid_snap_interval, rate, zoom),
            rate,
            config: state.snap,
            enabled: state.snap.master && !shift,
            zoom,
            scroll: state.timeline_scroll_x,
            links,
            linked_selection: state.linked_selection,
        }
    }

    /// The clips that move when `clip` is dragged: itself, its linked
    /// partner while the Linked switch is on, and its group.
    pub fn moving_with(state: &AppState, clip: ClipId) -> Vec<ClipId> {
        let seq = state.sequence();
        let Some(c) = seq.all_clips().find(|c| c.id == clip) else { return vec![clip] };
        let mut out = vec![clip];
        out.extend(c.linked_clip_id.filter(|_| state.linked_selection));
        if let Some(g) = c.group_id {
            out.extend(seq.all_clips().filter(|o| o.group_id == Some(g) && !out.contains(&o.id)).map(|o| o.id).collect::<Vec<_>>());
        }
        out
    }

    /// `ids`, with every clip linked or grouped with one of them.
    fn with_partners(&self, ids: &[ClipId]) -> Vec<ClipId> {
        let mut out = ids.to_vec();
        let groups: Vec<klipp_core::GroupId> = self.links.iter().filter(|(id, _, _)| out.contains(id)).filter_map(|(_, _, g)| *g).collect();
        for &(id, partner, group) in &self.links {
            let linked = self.linked_selection && (partner.is_some_and(|p| ids.contains(&p)) || (ids.contains(&id) && partner.is_some()));
            if linked {
                for p in [Some(id), partner].into_iter().flatten() {
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
            if group.is_some_and(|g| groups.contains(&g)) && !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// The nearest target to any of `edges` within the snap distance,
    /// leaving out the edges of `exclude` and of every clip linked or
    /// grouped with one of them; or the snap `hold` keeps, while its edge
    /// is within [`HOLD`] times the distance of it. `hold` is updated.
    pub fn snap(&self, edges: &[RationalTime], exclude: &[ClipId], hold: &mut Option<SnapHold>) -> Option<Snap> {
        if !self.enabled {
            *hold = None;
            return None;
        }
        let exclude = self.with_partners(exclude);
        if let Some(SnapHold(held)) = *hold {
            let still = edges.get(held.edge).is_some_and(|e| (e.to_secs_f64() - held.at.to_secs_f64()).abs() <= self.threshold_secs * HOLD);
            if still {
                return Some(held);
            }
        }
        let mut best: Option<(f64, Snap)> = None;
        let mut offer = |d: f64, s: Snap| {
            if d < self.threshold_secs && best.as_ref().is_none_or(|(b, _)| d < *b) {
                best = Some((d, s));
            }
        };
        for (edge, e) in edges.iter().enumerate() {
            let secs = e.to_secs_f64();
            for t in &self.targets {
                if t.owner.is_some_and(|o| exclude.contains(&o)) || !self.config.is_kind_enabled(t.kind) {
                    continue;
                }
                offer((secs - t.at.to_secs_f64()).abs(), Snap { edge, at: t.at, kind: t.kind, owner: t.owner });
            }
            if let Some(interval) = self.grid_secs.filter(|i| *i > 0.0 && self.config.grid) {
                let nearest = (secs / interval).round() * interval;
                offer((secs - nearest).abs(), Snap { edge, at: RationalTime::from_secs_f64(nearest, self.rate), kind: SnapTargetKind::Grid, owner: None });
            }
        }
        *hold = best.map(|(_, s)| SnapHold(s));
        best.map(|(_, s)| s)
    }

    /// Where the snap line for `snap` is drawn, in the timeline's
    /// `snap_line_x`: `content_left` is the tracks' left edge on screen.
    pub fn line(&self, snap: &Snap, content_left: f32) -> (f32, SnapTargetKind) {
        (content_left + snap.at.to_secs_f64() as f32 * self.zoom - self.scroll, snap.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Clip, TrackKind};

    const RATE: u32 = 24;

    fn t(secs: f64) -> RationalTime {
        RationalTime::from_secs_f64(secs, RATE)
    }

    /// Two clips on the first video track, 0–2 s and 5–7 s, at 100
    /// points a second and a 10-point distance (0.1 s).
    fn state() -> (AppState, ClipId, ClipId) {
        let mut state = AppState { snap_threshold_px: 10.0, ..Default::default() };
        let v = state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let a = Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let b = Clip::media(klipp_core::MediaId::new(), t(5.0), t(7.0), t(0.0), t(2.0));
        let (ia, ib) = (a.id, b.id);
        state.sequences[0].tracks[v].clips.extend([a, b]);
        state.playhead = t(20.0);
        (state, ia, ib)
    }

    #[test]
    fn the_nearest_edge_snaps_and_shift_never_does() {
        let (state, a, b) = state();
        let s = Snapper::of(&state, 100.0, false);
        let mut hold = None;
        let got = s.snap(&[t(4.95), t(6.95)], &[a], &mut hold).unwrap();
        assert_eq!((got.edge, got.at, got.owner), (0, t(5.0), Some(b)));
        assert!(Snapper::of(&state, 100.0, true).snap(&[t(4.95)], &[a], &mut hold).is_none());
        assert_eq!(hold, None, "Shift lets a held snap go");
        assert!(s.snap(&[t(4.8)], &[a], &mut None).is_none(), "0.2 s is past a 0.1 s distance");
    }

    /// Held, a snap stays while its edge is within 1.5 times the distance
    /// of it, back and forth; past that it lets go. Not held, the same
    /// edge 0.12 s away doesn't snap.
    #[test]
    fn a_snap_holds_until_past_one_and_a_half_times_the_distance() {
        let (state, a, _) = state();
        let s = Snapper::of(&state, 100.0, false);
        let mut hold = None;
        assert!(s.snap(&[t(5.05)], &[a], &mut hold).is_some());
        for secs in [5.12, 4.88, 5.14, 5.0, 4.86] {
            assert_eq!(s.snap(&[t(secs)], &[a], &mut hold).map(|s| s.at), Some(t(5.0)), "held at {secs}");
        }
        assert!(s.snap(&[t(5.12)], &[a], &mut None).is_none(), "not held: past the distance");
        assert!(s.snap(&[t(5.17)], &[a], &mut hold).is_none(), "past 1.5x it lets go");
        assert_eq!(hold, None);
    }

    /// Markers are targets; the clip dragged, its linked partner and its
    /// group never are, nor what's selected with it once excluded.
    #[test]
    fn markers_snap_and_the_clips_that_move_together_dont() {
        let (mut state, a, b) = state();
        state.sequences[0].markers.push(klipp_timeline::Marker { id: klipp_core::MarkerId::new(), position: t(10.0), label: "m".into(), color: klipp_core::MarkerColor::Red, duration: Some(t(2.0)), comment: String::new(), kind: Default::default(), review: None });
        let s = Snapper::of(&state, 100.0, false);
        let got = s.snap(&[t(10.04)], &[a], &mut None).unwrap();
        assert_eq!((got.at, got.kind), (t(10.0), SnapTargetKind::Marker));
        assert_eq!(s.snap(&[t(11.97)], &[a], &mut None).map(|g| g.at), Some(t(12.0)), "a range marker's end");

        // b linked to a: moving a, b's edges aren't targets.
        for c in state.sequences[0].all_clips_mut() {
            if c.id == a {
                c.linked_clip_id = Some(b);
            }
        }
        state.linked_selection = true;
        assert!(Snapper::of(&state, 100.0, false).snap(&[t(5.02)], &[a], &mut None).is_none(), "the linked partner");
        assert_eq!(Snapper::moving_with(&state, a), [a, b]);
        state.linked_selection = false;
        assert!(Snapper::of(&state, 100.0, false).snap(&[t(5.02)], &[a], &mut None).is_some(), "the Linked switch off: b stays, a target");

        // Grouped instead: the same.
        let g = klipp_core::GroupId::new();
        for c in state.sequences[0].all_clips_mut() {
            c.linked_clip_id = None;
            c.group_id = Some(g);
        }
        assert!(Snapper::of(&state, 100.0, false).snap(&[t(5.02)], &[a], &mut None).is_none(), "the group");
        assert_eq!(Snapper::moving_with(&state, a), [a, b]);
    }
}
