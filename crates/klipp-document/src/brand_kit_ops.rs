//! A brand kit's intro, outro and watermark as ops (#2722): what
//! `apply_brand_kit` sends as one undo step, built here so the geometry is
//! one function a test can measure in a rendered frame.

use klipp_core::MediaId;
use klipp_timeline::Sequence;
use serde_json::json;

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
}
