//! Tracker-point picker primitive shared by the TRANSFORM LINK
//! and HYPERZOOM inspector sections.
//!
//! Both sections need a "list every tracker on every clip in the
//! active sequence; click one to bind it as a target". Without a
//! shared helper they'd reinvent the same list/click loop twice
//! and drift apart over time. This module owns the loop; callers
//! own the click semantics.
//!
//! The existing `show_tracking_section` (the TRACKING inspector
//! card) deliberately stays untouched — it wraps a richer
//! management surface (Place Tracker / Analyze / Delete / Link to
//! keyframes) that doesn't share a shape with these compact
//! pickers. Extracting that into a primitive too would be over-
//! lifting; the management surface and the picker surface have
//! different responsibilities.
//!
//! # Design
//!
//! `tracker_menu_items` is a thin renderer that paints one
//! `selectable_label` per `(clip, track_point)` pair. The caller
//! passes:
//!
//! * `seq` — the active sequence to walk.
//! * `is_selected(host_clip_id, point_index)` — predicate that
//!   highlights the currently-bound tracker (so the dropdown
//!   shows which one is active).
//! * `on_pick(host_clip_id, point_index, label)` — closure that
//!   fires when the user clicks. Called with the host clip's
//!   ClipId, the track-point index, and a human-readable label
//!   the caller can echo into the dropdown's selected_text.
//!
//! No state, no AppState dependency. Pure function over
//! `&Sequence` + closures — testable end-to-end without an egui
//! context for the iteration logic (see `enumerate_tracker_targets`).

use egui::Ui;
use klipp_core::ClipId;
use klipp_timeline::Sequence;

/// Pure-function counterpart to [`tracker_menu_items`] — walks
/// the sequence and yields every `(host_clip_id, point_index,
/// label)` triple in deterministic iteration order. Pulled out as
/// its own function so the menu's content can be tested without
/// an egui context.
///
/// Iteration order matches the inspector's existing tracker list
/// (clip-major, point-major) so users see trackers in the same
/// order across the TRACKING / TRANSFORM LINK / HYPERZOOM
/// surfaces. Track-point name is preferred when set; falls back
/// to `Track {N}` / `clip xxxxxx` when blank.
pub fn enumerate_tracker_targets(seq: &Sequence) -> Vec<(ClipId, u32, String)> {
    let mut out = Vec::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        let track_label = if track.name.is_empty() {
            format!("Track {}", ti + 1)
        } else {
            track.name.clone()
        };
        for clip in &track.clips {
            for (pi, tp) in clip.track_points.iter().enumerate() {
                let tp_name = if tp.name.is_empty() {
                    format!("Track {}", pi + 1)
                } else {
                    tp.name.clone()
                };
                let clip_short: String = clip.id.0.to_string().chars().take(6).collect();
                let label = format!("{} · clip {}… · {}", track_label, clip_short, tp_name);
                out.push((clip.id, pi as u32, label));
            }
        }
    }
    out
}

/// Render one `selectable_label` per tracker target. The label
/// reads "Track N · clip xxxxxx… · Tracker name". `is_selected`
/// pre-renders the dot for the currently-bound tracker; clicking
/// any item fires `on_pick` and the click is consumed.
///
/// Returns `true` iff the user clicked an item this paint —
/// callers can use this to short-circuit other dropdown handling.
pub fn tracker_menu_items<F>(
    ui: &mut Ui,
    seq: &Sequence,
    mut is_selected: impl FnMut(ClipId, u32) -> bool,
    mut on_pick: F,
) -> bool
where
    F: FnMut(ClipId, u32, &str),
{
    let mut clicked_any = false;
    for (host_clip_id, point_index, label) in enumerate_tracker_targets(seq) {
        let sel = is_selected(host_clip_id, point_index);
        if ui.selectable_label(sel, &label).clicked() && !sel {
            on_pick(host_clip_id, point_index, &label);
            clicked_any = true;
        }
    }
    clicked_any
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime, TrackPoint};
    use klipp_timeline::{Clip, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn clip_with_n_trackers(n: usize) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        for _ in 0..n {
            c.track_points.push(TrackPoint::default());
        }
        c
    }

    fn seq_with(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = tracks;
        s
    }

    fn track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    }

    #[test]
    fn enumerate_walks_every_tracker_on_every_clip() {
        // Two tracks, three clips total, varying tracker counts.
        let c1 = clip_with_n_trackers(2);
        let c2 = clip_with_n_trackers(0);
        let c3 = clip_with_n_trackers(1);
        let seq = seq_with(vec![
            track("V1", vec![c1.clone(), c2]),
            track("V2", vec![c3.clone()]),
        ]);
        let out = enumerate_tracker_targets(&seq);
        // c1 has 2 trackers, c2 has 0, c3 has 1 → 3 total.
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn enumerate_returns_clip_id_and_point_index_pairs() {
        let c = clip_with_n_trackers(2);
        let cid = c.id;
        let seq = seq_with(vec![track("V1", vec![c])]);
        let out = enumerate_tracker_targets(&seq);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, cid);
        assert_eq!(out[0].1, 0);
        assert_eq!(out[1].0, cid);
        assert_eq!(out[1].1, 1);
    }

    #[test]
    fn enumerate_iteration_order_is_clip_major_then_point_major() {
        // Two clips with overlapping tracker counts. Iteration
        // must visit clip A's trackers in order, then clip B's —
        // pin the order so the dropdown stays consistent.
        let a = clip_with_n_trackers(2);
        let b = clip_with_n_trackers(2);
        let aid = a.id;
        let bid = b.id;
        let seq = seq_with(vec![track("V1", vec![a, b])]);
        let out = enumerate_tracker_targets(&seq);
        assert_eq!(out[0].0, aid);
        assert_eq!(out[0].1, 0);
        assert_eq!(out[1].0, aid);
        assert_eq!(out[1].1, 1);
        assert_eq!(out[2].0, bid);
        assert_eq!(out[2].1, 0);
        assert_eq!(out[3].0, bid);
        assert_eq!(out[3].1, 1);
    }

    #[test]
    fn enumerate_returns_empty_when_no_trackers() {
        let c = clip_with_n_trackers(0);
        let seq = seq_with(vec![track("V1", vec![c])]);
        assert!(enumerate_tracker_targets(&seq).is_empty());
    }

    #[test]
    fn enumerate_label_includes_track_name_and_short_clip_id() {
        let c = clip_with_n_trackers(1);
        let cid = c.id;
        let seq = seq_with(vec![track("V1", vec![c])]);
        let out = enumerate_tracker_targets(&seq);
        // Label includes "V1" (track name) and the first 6 chars
        // of the clip id (short form, same convention as the
        // matte-source dropdown).
        let expected_short: String = cid.0.to_string().chars().take(6).collect();
        assert!(out[0].2.contains("V1"), "track name in label: {}", out[0].2);
        assert!(out[0].2.contains(&expected_short), "short clip id in label: {}", out[0].2);
    }

    #[test]
    fn enumerate_uses_track_point_name_when_set() {
        let mut c = clip_with_n_trackers(1);
        c.track_points[0].name = "Face".to_string();
        let seq = seq_with(vec![track("V1", vec![c])]);
        let out = enumerate_tracker_targets(&seq);
        assert!(out[0].2.contains("Face"), "tracker name in label: {}", out[0].2);
    }
}
