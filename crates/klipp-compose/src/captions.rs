//! Captions kept off the lines a title is on (#2567). A caption sits
//! where its track's style puts it, near the bottom of the frame, and so
//! does a lower third: shown together they shared a line and read as one
//! block. While both are on screen the caption moves below the title, or
//! above it when there is no room below, inside the action-safe area.

use klipp_core::{Resolution, Timecode};
use klipp_timeline::{ClipKind, Sequence, TrackKind};

/// The action-safe margin, a fraction of the frame on each side.
const SAFE: f32 = 0.05;
/// The gap kept between a caption and the title it moves round.
const GAP: f32 = 0.015;

/// How far down to move a caption whose box is `caption` (left, top,
/// width, height, fractions of the frame) so it shares no line with any
/// of `titles`: below the lowest it shares a line with, or above the
/// highest when there is no room below. `None` when it shares none, or
/// fits on neither side.
pub fn caption_shift(caption: [f32; 4], titles: &[[f32; 4]]) -> Option<f32> {
    let shares = |top: f32, b: &[f32; 4]| top < b[1] + b[3] && top + caption[3] > b[1];
    let mut hit: Vec<[f32; 4]> = titles.iter().copied().filter(|b| shares(caption[1], b)).collect();
    if hit.is_empty() {
        return None;
    }
    // Moving below one title can bring the caption onto another's line:
    // take in every title the band it would cross.
    loop {
        let below = hit.iter().map(|b| b[1] + b[3]).fold(f32::MIN, f32::max) + GAP;
        let more: Vec<[f32; 4]> = titles.iter().copied().filter(|b| !hit.contains(b) && shares(below, b)).collect();
        if more.is_empty() {
            break;
        }
        hit.extend(more);
    }
    let below = hit.iter().map(|b| b[1] + b[3]).fold(f32::MIN, f32::max) + GAP;
    if below + caption[3] <= 1.0 - SAFE {
        return Some(below - caption[1]);
    }
    let above = hit.iter().map(|b| b[1]).fold(f32::MAX, f32::min) - GAP - caption[3];
    (above >= SAFE && !titles.iter().any(|b| shares(above, b))).then(|| above - caption[1])
}

/// `seq` as shown at `time` with its captions moved off the titles'
/// lines, as a [`crate::frame_snapshot`]; `None` when no caption shown
/// then shares a line with a title.
pub fn captions_clear_of_titles(seq: &Sequence, time: Timecode, width: u32, height: u32) -> Option<Sequence> {
    let frame = Resolution { width, height };
    let shown = |kind: TrackKind| {
        seq.tracks.iter().filter(move |t| t.kind == kind && t.visible && !t.muted).flat_map(move |t| {
            t.clips.iter().filter(move |c| time >= c.timeline_in && time < c.timeline_out).filter_map(|c| match &c.kind {
                ClipKind::Title(td) => Some((c.id, td)),
                _ => None,
            })
        })
    };
    let titles: Vec<[f32; 4]> = [TrackKind::Video, TrackKind::Text].into_iter().flat_map(shown).filter_map(|(_, td)| klipp_timeline::title_box::title_box(td, frame)).collect();
    if titles.is_empty() {
        return None;
    }
    let moves: Vec<(klipp_core::ClipId, f32)> = shown(TrackKind::Subtitle)
        .filter_map(|(id, td)| klipp_timeline::title_box::title_box(td, frame).and_then(|b| caption_shift(b, &titles)).map(|dy| (id, dy)))
        .collect();
    if moves.is_empty() {
        return None;
    }
    let mut snap = crate::frame_snapshot(seq, time);
    for track in snap.tracks.iter_mut().filter(|t| t.kind == TrackKind::Subtitle) {
        for clip in &mut track.clips {
            let Some((_, dy)) = moves.iter().find(|(id, _)| *id == clip.id) else { continue };
            if let ClipKind::Title(td) = &mut clip.kind {
                for o in &mut td.objects {
                    o.transform.center.y += dy;
                }
            }
        }
    }
    Some(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lower third's name across the bottom left, and a caption at
    /// the same height: the caption goes below it, inside the safe area.
    #[test]
    fn a_caption_on_a_titles_line_goes_below_it() {
        let lower_third = [0.05, 0.78, 0.4, 0.1];
        let caption = [0.3, 0.82, 0.4, 0.05];
        let dy = caption_shift(caption, &[lower_third]).expect("moved");
        let top = caption[1] + dy;
        assert!(top >= 0.88 && top + caption[3] <= 0.95, "below the title and inside the safe area: top {top}");
    }

    /// No room below: the caption goes above the title instead.
    #[test]
    fn with_no_room_below_it_goes_above() {
        let low = [0.05, 0.84, 0.4, 0.1];
        let caption = [0.3, 0.86, 0.4, 0.05];
        let dy = caption_shift(caption, &[low]).expect("moved");
        assert!(caption[1] + dy + caption[3] <= 0.84, "above the title: {}", caption[1] + dy);
    }

    /// A caption on its own line stays where its style put it.
    #[test]
    fn a_caption_clear_of_titles_stays() {
        assert_eq!(caption_shift([0.3, 0.86, 0.4, 0.05], &[[0.05, 0.1, 0.4, 0.1]]), None);
    }
}
