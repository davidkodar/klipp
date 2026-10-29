//! A brand kit's intro, outro and watermark as ops (#2722): what
//! `apply_brand_kit` sends as one undo step, built here so the geometry is
//! one function a test can measure in a rendered frame.

use klipp_core::MediaId;
use klipp_timeline::{Brief, CaptionStyle, ClipKind, Sequence, TrackKind};
use serde_json::json;

use crate::history::EditCommand;
use crate::ops::Op;

/// Where a watermark sits: a corner, a margin and a width as fractions of
/// the frame's width, and its opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Watermark {
    pub left: bool,
    pub top: bool,
    pub margin: f32,
    pub scale: f32,
    pub opacity: f32,
}

impl Watermark {
    /// The logo's centre and size as fractions of the frame, for a logo
    /// `logo` pixels in a frame `frame` pixels: `scale`
    /// of the frame's width wide, its own shape kept, `margin` of the
    /// frame's width from the two edges of its corner.
    pub fn pose(&self, logo: (u32, u32), frame: (u32, u32)) -> (f32, f32, f32, f32) {
        let (fw, fh) = (frame.0.max(1) as f32, frame.1.max(1) as f32);
        let w = self.scale;
        let h = w * (logo.1.max(1) as f32 / logo.0.max(1) as f32) * (fw / fh);
        let (mx, my) = (self.margin, self.margin * fw / fh);
        let x = if self.left { mx + w / 2.0 } else { 1.0 - mx - w / 2.0 };
        let y = if self.top { my + h / 2.0 } else { 1.0 - my - h / 2.0 };
        (x, y, w, h)
    }

    /// The ops that put the logo `media` on a track of its own above every
    /// other, held for `length_secs`, in its place.
    pub fn ops(&self, media: MediaId, logo: (u32, u32), frame: (u32, u32), length_secs: f64) -> Result<Vec<Op>, String> {
        let (x, y, w, h) = self.pose(logo, frame);
        // `compose` sizes the frame-sized layer the picture is fitted into,
        // so its box keeps the frame's shape: as wide as the logo for a
        // logo wider than the frame, as tall as it for one taller, the
        // fitted logo then exactly `w` by `h`, centred where the pose puts
        // it.
        let (width, height) = (w.max(h), w.max(h));
        ops(vec![
            json!({ "op": "add_track", "kind": "video", "name": "Logo" }),
            json!({ "op": "place", "media_id": media.0.to_string(), "track": 0, "at": { "secs": 0.0 }, "source_in": 0.0, "source_out": length_secs, "with_audio": false }),
            json!({ "op": "compose", "clip_id": "@1", "poses": [{ "secs": 0.0, "x": x, "y": y, "width": width, "height": height, "opacity": self.opacity }] }),
        ])
    }
}

/// What opens or closes the piece: a composition template by name, or a
/// clip of a media item.
#[derive(Debug, Clone, PartialEq)]
pub enum Bookend {
    Template(String),
    Media(MediaId),
}

/// The ops that open `seq` with `intro`, `length_secs` long: every clip
/// moved that much later, the last first so none lands on another, then
/// the intro at the start of the first video track.
pub fn intro_ops(seq: &Sequence, intro: &Bookend, length_secs: f64) -> Result<Vec<Op>, String> {
    let mut clips: Vec<(f64, String)> = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.timeline_in.to_secs_f64(), c.id.0.to_string())).collect();
    clips.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut out: Vec<serde_json::Value> = clips.into_iter().map(|(at, id)| json!({ "op": "move", "clip_id": id, "to": { "secs": at + length_secs }, "linked": false })).collect();
    out.push(bookend(intro, 0.0));
    ops(out)
}

/// The ops that close a piece ending at `end_secs` with `outro`.
pub fn outro_ops(end_secs: f64, outro: &Bookend) -> Result<Vec<Op>, String> {
    ops(vec![bookend(outro, end_secs)])
}

/// A kit's intro, outro and watermark as found: each bookend with its
/// length in seconds, the logo with its picture's size.
#[derive(Debug, Clone, Default)]
pub struct SequenceParts {
    pub intro: Option<(Bookend, f64)>,
    pub outro: Option<(Bookend, f64)>,
    pub watermark: Option<(Watermark, MediaId, (u32, u32))>,
}

/// The ops that put `parts` on `seq`: the intro first, the rest moved on,
/// the outro after the last clip, and the logo over all of it.
pub fn sequence_ops(seq: &Sequence, parts: &SequenceParts) -> Result<Vec<Op>, String> {
    let end = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.timeline_out.to_secs_f64()).fold(0.0, f64::max);
    let mut out = Vec::new();
    let mut total = end;
    if let Some((b, len)) = &parts.intro {
        out.extend(intro_ops(seq, b, *len)?);
        total += len;
    }
    if let Some((b, len)) = &parts.outro {
        out.extend(outro_ops(total, b)?);
        total += len;
    }
    if let Some((mark, media, size)) = &parts.watermark {
        out.extend(mark.ops(*media, *size, (seq.resolution.width, seq.resolution.height), total.max(0.1))?);
    }
    Ok(out)
}

/// A kit's look as edits of `seq`, one undo step: what the socket and the
/// app send alike.
#[derive(Debug, Default)]
pub struct LookEdits {
    pub edits: Vec<EditCommand>,
    /// Titles restyled because a look they follow changed.
    pub restyled_titles: usize,
    /// Subtitle tracks given the caption look.
    pub caption_tracks: usize,
}

/// The edits that take a kit's look into `seq`: `brief` (the kit taken
/// into the sequence's brief) as its brief, the titles that follow a look
/// it changes going with it, and `captions` on every subtitle track
/// without a look of its own.
pub fn look_edits(seq: &Sequence, brief: Option<Brief>, captions: Option<&CaptionStyle>) -> LookEdits {
    let mut out = LookEdits::default();
    if let Some(brief) = brief {
        let old = seq.brief.clone();
        let followers = crate::ops::follow_brief(seq, old.as_ref(), Some(&brief));
        out.restyled_titles = followers.len();
        out.edits.push(EditCommand::SetBrief { old: old.map(Box::new), new: Some(Box::new(brief)) });
        out.edits.extend(followers);
    }
    if let Some(look) = captions {
        for (i, track) in seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Subtitle && t.caption_style.is_none()) {
            let mut new = track.clone();
            for clip in &mut new.clips {
                if let ClipKind::Title(td) = &mut clip.kind {
                    *td = crate::ops::apply_caption_style(td, look);
                }
            }
            new.caption_style = Some(look.clone());
            out.edits.push(EditCommand::SetTrack { description: "Brand Kit Captions", track_index: i, old: Box::new(track.clone()), new: Box::new(new) });
            out.caption_tracks += 1;
        }
    }
    out
}

fn bookend(b: &Bookend, at: f64) -> serde_json::Value {
    match b {
        Bookend::Template(name) => json!({ "op": "template", "name": name, "kind": "composition", "at": { "secs": at } }),
        Bookend::Media(id) => json!({ "op": "place", "media_id": id.0.to_string(), "track": "video", "at": { "secs": at } }),
    }
}

fn ops(values: Vec<serde_json::Value>) -> Result<Vec<Op>, String> {
    values.into_iter().map(|v| serde_json::from_value(v).map_err(|e| format!("a brand kit op: {e}"))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2:1 logo, a tenth of a 16:9 frame's width, in each corner a
    /// margin of 3% of the width from both edges.
    #[test]
    fn a_watermark_sits_in_its_corner() {
        let w = |left, top| Watermark { left, top, margin: 0.03, scale: 0.1, opacity: 0.8 };
        let (x, y, width, height) = w(false, false).pose((200, 100), (1920, 1080));
        assert!((width - 0.1).abs() < 1e-6);
        assert!((height - 0.1 * 0.5 * 1920.0 / 1080.0).abs() < 1e-6, "its own shape: {height}");
        assert!((x + width / 2.0 - 0.97).abs() < 1e-6, "3% from the right");
        assert!((y + height / 2.0 - (1.0 - 0.03 * 1920.0 / 1080.0)).abs() < 1e-6, "as many pixels from the bottom");
        let (x, y, ..) = w(true, true).pose((200, 100), (1920, 1080));
        assert!((x - 0.05 - 0.03).abs() < 1e-6 && y < 0.5);
        assert!(w(true, true).ops(MediaId::new(), (200, 100), (1920, 1080), 10.0).unwrap().len() == 3);
    }

    /// A 10 s piece with a 3 s intro, a 2 s outro and a logo: the clip
    /// moved to 3 s, the intro at 0, the outro at 13 s after the moved
    /// clip, the logo held 15 s, over all of it.
    #[test]
    fn the_parts_land_in_order_and_the_logo_covers_them() {
        let mut seq = Sequence::new_default();
        let clip = klipp_timeline::test_clip::TestClip::media(10.0).build();
        let clip_id = clip.id.0.to_string();
        seq.tracks[0].clips.push(clip);
        let outro = MediaId::new();
        let parts = SequenceParts {
            intro: Some((Bookend::Template("Card".into()), 3.0)),
            outro: Some((Bookend::Media(outro), 2.0)),
            watermark: Some((Watermark { left: false, top: false, margin: 0.03, scale: 0.1, opacity: 0.8 }, MediaId::new(), (200, 100))),
        };
        let ops: Vec<serde_json::Value> = sequence_ops(&seq, &parts).unwrap().iter().map(|o| serde_json::to_value(o).unwrap()).collect();
        let secs = |v: &serde_json::Value| v["secs"].as_f64().unwrap();
        assert_eq!((ops[0]["op"].as_str(), ops[0]["clip_id"].as_str()), (Some("move"), Some(clip_id.as_str())));
        assert!((secs(&ops[0]["to"]) - 3.0).abs() < 1e-6, "{}", ops[0]);
        assert_eq!((ops[1]["op"].as_str(), ops[1]["name"].as_str()), (Some("template"), Some("Card")));
        assert!(secs(&ops[1]["at"]).abs() < 1e-6);
        assert_eq!(ops[2]["media_id"].as_str(), Some(outro.0.to_string().as_str()));
        assert!((secs(&ops[2]["at"]) - 13.0).abs() < 1e-6, "{}", ops[2]);
        let logo = ops.iter().find(|o| o["op"] == "place" && o["media_id"].as_str() != Some(outro.0.to_string().as_str())).unwrap();
        assert!((logo["source_out"].as_f64().unwrap() - 15.0).abs() < 1e-6, "{logo}");
    }

    /// The look: the brief set, and a subtitle track without a look of
    /// its own given the caption look; one with its own kept.
    #[test]
    fn the_look_sets_the_brief_and_the_bare_subtitle_tracks() {
        let mut seq = Sequence::new_default();
        let mut bare = klipp_timeline::Track::new("Subtitles", TrackKind::Subtitle);
        bare.caption_style = None;
        let mut styled = bare.clone();
        styled.caption_style = Some(CaptionStyle::default());
        seq.tracks.push(bare);
        seq.tracks.push(styled);
        let brief = Brief { brand_colors: vec!["#112233".into()], ..Default::default() };
        let look = CaptionStyle { font: "Inter".into(), ..CaptionStyle::default() };
        let out = look_edits(&seq, Some(brief), Some(&look));
        assert_eq!(out.caption_tracks, 1);
        assert!(matches!(&out.edits[0], EditCommand::SetBrief { new: Some(b), .. } if b.brand_colors == ["#112233"]));
        assert!(out.edits.iter().any(|e| matches!(e, EditCommand::SetTrack { track_index, new, .. } if *track_index == seq.tracks.len() - 2 && new.caption_style.as_ref().is_some_and(|c| c.font == "Inter"))));
        assert!(look_edits(&seq, None, None).edits.is_empty());
    }
}
