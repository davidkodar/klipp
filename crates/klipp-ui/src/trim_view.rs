//! Trim mode's two-up view (#914): while an edit point is selected and
//! the timeline is not playing, the program viewer shows the outgoing
//! clip's last frame and the incoming clip's first frame side by side,
//! each with how much media is left past the cut, and the side being
//! trimmed outlined. The frames are the media's filmstrip thumbnails, so
//! the view costs no decode.

use crate::edit_point::{EditPoint, EditSide};
use crate::state::AppState;
use klipp_core::MediaId;
use klipp_timeline::Sequence;

/// One side of the cut as the two-up shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Side {
    pub name: String,
    /// The media and the source time of the frame at the cut, when the
    /// clip is a media clip.
    pub frame: Option<(MediaId, f64)>,
    /// Frames of media past the cut that a trim can still reach.
    pub handle_frames: Option<i64>,
}

/// The outgoing and incoming sides of `edit`: the outgoing clip's last
/// frame and the media after it, the incoming clip's first frame and the
/// media before it. `media` gives a media's length in seconds and file
/// name, the name shown for a clip without one of its own.
pub fn sides(seq: &Sequence, edit: &EditPoint, media: impl Fn(MediaId) -> Option<(f64, String)>) -> (Option<Side>, Option<Side>) {
    let fps = seq.fps.as_f64();
    let find = |id| seq.all_clips().find(|c| c.id == id);
    let frames = |secs: f64| (secs * fps).round() as i64;
    let name = |c: &klipp_timeline::Clip| c.name.clone().or_else(|| c.media_id().and_then(&media).map(|(_, n)| n)).unwrap_or_default();
    let outgoing = edit.outgoing.and_then(find).map(|c| {
        let id = c.media_id();
        let last = (c.source_out.to_secs_f64() - 1.0 / fps).max(c.source_in.to_secs_f64());
        Side {
            name: name(c),
            frame: id.map(|m| (m, last)),
            handle_frames: id.and_then(&media).map(|(len, _)| frames(len - c.source_out.to_secs_f64()).max(0)),
        }
    });
    let incoming = edit.incoming.and_then(find).map(|c| {
        let id = c.media_id();
        Side {
            name: name(c),
            frame: id.map(|m| (m, c.source_in.to_secs_f64())),
            handle_frames: id.map(|_| frames(c.source_in.to_secs_f64()).max(0)),
        }
    });
    (outgoing, incoming)
}

/// The filmstrip thumbnail nearest `secs` into `media`.
fn nearest_thumb(state: &AppState, media: MediaId, secs: f64) -> Option<egui::TextureHandle> {
    let strip = state.filmstrip_textures.get(&media)?;
    strip
        .iter()
        .min_by(|a, b| (a.0 - secs).abs().total_cmp(&(b.0 - secs).abs()))
        .map(|(_, t)| t.clone())
}

/// Draw the two-up over `rect` when trim mode is on: an edit point
/// selected and the timeline still. Returns whether it drew.
pub fn draw(ui: &egui::Ui, state: &AppState, theme: &crate::theme::KlippTheme, rect: egui::Rect) -> bool {
    let Some(edit) = state.selected_edits.first() else { return false };
    if state.playback == klipp_engine::PlaybackState::Playing {
        return false;
    }
    let (out, inc) = sides(state.sequence(), edit, |m| state.media_pool.get(&m).map(|i| (i.duration.to_secs_f64(), i.filename.clone())));
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
    let gap = 8.0;
    let half_w = (rect.width() - gap) / 2.0;
    let caption_h = 34.0;
    let halves = [
        (egui::Rect::from_min_size(rect.min, egui::vec2(half_w, rect.height())), out, "OUT", matches!(edit.side, EditSide::Out | EditSide::Both)),
        (egui::Rect::from_min_size(egui::pos2(rect.min.x + half_w + gap, rect.min.y), egui::vec2(half_w, rect.height())), inc, "IN", matches!(edit.side, EditSide::In | EditSide::Both)),
    ];
    for (half, side, label, trimmed) in halves {
        let picture = egui::Rect::from_min_max(half.min, egui::pos2(half.max.x, half.max.y - caption_h));
        let font = egui::FontId::proportional(12.0);
        match &side {
            Some(s) => {
                match s.frame.and_then(|(m, secs)| nearest_thumb(state, m, secs)) {
                    Some(tex) => {
                        let size = tex.size_vec2();
                        let scale = (picture.width() / size.x).min(picture.height() / size.y);
                        let at = egui::Rect::from_center_size(picture.center(), size * scale);
                        painter.image(tex.id(), at, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    }
                    None => {
                        painter.text(picture.center(), egui::Align2::CENTER_CENTER, "No thumbnail yet", font.clone(), theme.text_secondary);
                    }
                }
                // Two lines, each cut to the half's width: the side and
                // the media left past the cut, then the clip's name.
                let handle = match s.handle_frames {
                    Some(n) => format!("  \u{00B7}  {n} fr of media {}", if label == "OUT" { "after" } else { "before" }),
                    None => String::new(),
                };
                let width = half.width() - 16.0;
                for (line, text, colour) in [(0.0, format!("{label}{handle}"), theme.text_primary), (1.0, s.name.clone(), theme.text_secondary)] {
                    let mut job = egui::text::LayoutJob::simple_singleline(text, font.clone(), colour);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
                    let galley = ui.fonts(|f| f.layout_job(job));
                    painter.galley(egui::pos2(half.min.x + 8.0, half.max.y - caption_h + 1.0 + line * 16.0), galley, colour);
                }
            }
            None => {
                painter.text(picture.center(), egui::Align2::CENTER_CENTER, if label == "OUT" { "Gap before the cut" } else { "Gap after the cut" }, font, theme.text_secondary);
            }
        }
        if trimmed && side.is_some() {
            painter.rect_stroke(half.shrink(1.0), 2.0, egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(120, 220, 120)));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{Clip, Track, TrackKind};

    #[test]
    fn each_side_shows_its_frame_at_the_cut_and_the_media_left() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 25);
        let (m1, m2) = (MediaId::new(), MediaId::new());
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let mut v = Track::new("V1", TrackKind::Video);
        // a plays 2-6 s of a 10 s file; b plays 1-3 s of its own.
        v.clips = vec![Clip::media(m1, tc(0.0), tc(4.0), tc(2.0), tc(6.0)), Clip::media(m2, tc(4.0), tc(6.0), tc(1.0), tc(3.0))];
        seq.tracks = vec![v];
        let edit = crate::edit_point::nearest(&seq, tc(4.0), |_| true).unwrap();
        let (out, inc) = sides(&seq, &edit, |m| Some((if m == m1 { 10.0 } else { 8.0 }, "file".to_string())));
        let (out, inc) = (out.unwrap(), inc.unwrap());
        assert_eq!(out.frame.map(|(m, s)| (m, (s * 25.0).round())), Some((m1, 149.0)), "the outgoing clip's last frame");
        assert_eq!(out.handle_frames, Some(100), "4 s of the file after it");
        assert_eq!(inc.frame, Some((m2, 1.0)), "the incoming clip's first frame");
        assert_eq!(inc.handle_frames, Some(25), "1 s before it");
    }
}
