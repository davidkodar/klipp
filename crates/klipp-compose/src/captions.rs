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

/// The gap kept between a caption and the player's controls under it.
const PLAYER_GAP: f32 = 0.02;

/// `td`, a caption laid out on a `from` frame, laid out for a `to` frame
/// of another shape, as a caption made for that frame is: the size
/// and the height of `to`'s caption look, as far from them as the person
/// put it, then lifted until it ends above `players`' controls along the
/// bottom (never past the top margin). Its font, backing and colours stay;
/// the renderer wraps its lines to the new width.
pub fn caption_for_frame(td: &klipp_timeline::TitleData, from: Resolution, to: Resolution, players: &[&klipp_core::platform_overlays::PlatformOverlay]) -> klipp_timeline::TitleData {
    use klipp_timeline::title_box::{caption_look, title_box};
    let ((from_size, from_y), (to_size, to_y)) = (caption_look(from), caption_look(to));
    let k = to_size / from_size;
    let mut new = td.clone();
    let shift = |td: &mut klipp_timeline::TitleData, dy: f32| {
        for o in &mut td.objects {
            o.transform.center.y += dy;
            for key in &mut o.transform.center_y_keyframes {
                key.value += dy;
            }
        }
    };
    for o in &mut new.objects {
        if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut o.content {
            tb.font_size *= k;
            for key in &mut tb.font_size_keyframes {
                key.value *= k;
            }
        }
    }
    if let klipp_timeline::TitleBackground::Color { padding, .. } = &mut new.background {
        *padding *= k;
    }
    shift(&mut new, to_y - from_y);
    if let Some([x, y, w, h]) = title_box(&new, to) {
        if let Some(limit) = players.iter().filter_map(|p| p.clear_above(x, x + w)).reduce(f32::min) {
            let over = (y + h) - (limit - PLAYER_GAP);
            if over > 0.0 {
                shift(&mut new, -over.min((y - SAFE).max(0.0)));
            }
        }
    }
    new
}

/// The captions of `seq`, a timeline of another shape, as an export to a
/// `to` frame draws them over its picture: its shown subtitle tracks
/// alone, each caption laid out for `to` ([`caption_for_frame`]) clear of
/// the player of the platform its brief names, or of every player made
/// for that shape when it names none. `None` with no caption to draw.
pub fn captions_for_frame(seq: &Sequence, to: Resolution) -> Option<Sequence> {
    use klipp_core::platform_overlays as po;
    let from = seq.resolution;
    let platform = seq.brief.as_ref().map(|b| b.platform.as_str()).unwrap_or("");
    let players: Vec<&po::PlatformOverlay> = match po::player_for(platform, to.width, to.height) {
        Some(p) => vec![p],
        None => po::all().iter().filter(|p| p.fits(to.width as f32, to.height as f32)).collect(),
    };
    let mut captions = seq.clone();
    captions.resolution = to;
    captions.tracks.retain(|t| t.kind == TrackKind::Subtitle && !t.muted && !t.clips.is_empty());
    if captions.tracks.is_empty() {
        return None;
    }
    for track in &mut captions.tracks {
        track.visible = true;
        for clip in &mut track.clips {
            if let ClipKind::Title(td) = &mut clip.kind {
                *td = caption_for_frame(td, from, to, &players);
            }
        }
    }
    Some(captions)
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

    /// A caption made for a wide frame, laid out for a tall one,
    /// takes the tall frame's size, in its own colour, and ends above the
    /// Shorts player's controls from 65% down.
    #[test]
    fn a_wide_caption_laid_out_for_a_tall_frame_grows_and_clears_the_player() {
        use klipp_timeline::title_box::{caption_look, title_box};
        let (wide, tall) = (Resolution { width: 1920, height: 1080 }, Resolution { width: 1080, height: 1920 });
        let (size, y) = caption_look(wide);
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hello there".into(), font_size: size, color: [255, 220, 0, 255], ..Default::default() });
        td.background = klipp_timeline::TitleBackground::Color { color: [0, 0, 0, 235], padding: 12.0, lines: None };
        td.objects[0].transform.center.y = y;
        let shorts = klipp_core::platform_overlays::by_id("youtube_shorts").unwrap();
        let new = caption_for_frame(&td, wide, tall, &[shorts]);
        let klipp_timeline::TitleObjectContent::TextBox(tb) = &new.objects[0].content else { panic!("a text box") };
        assert_eq!((tb.font_size, tb.color), (caption_look(tall).0, [255, 220, 0, 255]), "the tall frame's size, its own colour");
        let [_, top, _, h] = title_box(&new, tall).unwrap();
        assert!(top + h <= 0.65 - PLAYER_GAP + 1e-4 && top >= SAFE, "above the player, inside the top margin: {top}..{}", top + h);
    }

    /// A caption on its own line stays where its style put it.
    #[test]
    fn a_caption_clear_of_titles_stays() {
        assert_eq!(caption_shift([0.3, 0.86, 0.4, 0.05], &[[0.05, 0.1, 0.4, 0.1]]), None);
    }
}
