//! A sequence's covers (#3311): frames of it, with text, made into a
//! video's thumbnail. Several are kept so they can be compared. A cover
//! is drawn by the export renderer as the sequence itself at the cover's
//! size, its crop a still camera on the picture and its text on a track
//! of its own out of the camera's reach, so what is exported is the frame
//! the export would draw. With no crop chosen it fills its size, the
//! picture cropped rather than barred ([`fill_crop`]).

use serde::{Deserialize, Serialize};

use crate::{Clip, ClipKind, Sequence, TitleData, Track, TrackKind};
use klipp_core::{CoverId, Keyframe, RationalTime};

/// The most covers a sequence keeps.
pub const MAX_COVERS: usize = 24;

/// The longest a cover's name is, in characters.
pub const MAX_NAME_CHARS: usize = 80;

/// How far in a cover's crop goes.
pub const MAX_ZOOM: f32 = 8.0;

/// The latest frame a cover can be at, in seconds: a day. A file's frame
/// past it is no sequence's, and would overflow the frame clock.
pub const MAX_AT_SECS: f64 = 86_400.0;

/// One cover: a frame of the sequence, the part of it shown, the text
/// over it, and the size it is made at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cover {
    pub id: CoverId,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The frame, in seconds of the sequence.
    pub at_secs: f64,
    /// A [`klipp_core::cover_sizes`] id.
    pub size: String,
    /// The crop chosen, the whole frame included. `None` for none chosen:
    /// the cover fills its size, [`fill_crop`] on the picture at its frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<CoverCrop>,
    /// The words over the picture, laid out as a title is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TitleData>,
    /// Whether the sequence's own titles and captions show; a cover is
    /// the picture alone unless asked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub with_titles: bool,
}

/// The part of the frame a cover shows: its centre, as a share of the
/// frame across and down, and how far in, 1 being the whole frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoverCrop {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Default for CoverCrop {
    fn default() -> Self {
        Self { x: 0.5, y: 0.5, zoom: 1.0 }
    }
}

impl CoverCrop {
    pub fn is_whole(&self) -> bool {
        *self == Self::default()
    }

    /// This crop made sound: the zoom from 1 to [`MAX_ZOOM`], the centre
    /// where the part shown stays inside the frame.
    pub fn clamped(self) -> Self {
        let zoom = if self.zoom.is_finite() { self.zoom.clamp(1.0, MAX_ZOOM) } else { 1.0 };
        let edge = 0.5 / zoom;
        let centre = |v: f32| if v.is_finite() { v.clamp(edge, 1.0 - edge) } else { 0.5 };
        Self { x: centre(self.x), y: centre(self.y), zoom }
    }
}

impl Cover {
    /// This cover as a file may have written it, made sound: its name
    /// cut to length and stripped of control characters, its crop
    /// clamped. `None` for one whose frame isn't a time up to
    /// [`MAX_AT_SECS`].
    pub fn checked(self) -> Option<Cover> {
        if !self.at_secs.is_finite() || !(0.0..=MAX_AT_SECS).contains(&self.at_secs) {
            return None;
        }
        let name: String = self.name.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
        let size: String = self.size.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(32).collect();
        Some(Cover { name, size, crop: self.crop.map(CoverCrop::clamped), ..self })
    }
}

/// The cover `named` names among `covers`: by its id, the start of its id
/// (four characters or more), or its name in any case.
pub fn find(covers: &[Cover], named: &str) -> Option<usize> {
    let n = named.trim();
    covers
        .iter()
        .position(|c| c.id.to_string() == n)
        .or_else(|| (n.len() >= 4).then(|| covers.iter().position(|c| c.id.to_string().starts_with(&n.to_ascii_lowercase()))).flatten())
        .or_else(|| covers.iter().position(|c| !c.name.is_empty() && c.name.eq_ignore_ascii_case(n)))
}

/// `covers` as a file may have written them: each [`Cover::checked`], the
/// first of any id given twice, at most [`MAX_COVERS`].
pub fn checked(covers: Vec<Cover>) -> Vec<Cover> {
    let mut out: Vec<Cover> = Vec::new();
    for c in covers.into_iter().filter_map(Cover::checked) {
        if out.len() < MAX_COVERS && !out.iter().any(|o| o.id == c.id) {
            out.push(c);
        }
    }
    out
}

/// The crop that fills a `width` × `height` cover with a picture of `aspect`
/// (its width over its height), which the frame fits whole with bars beside
/// it: the camera in until the picture reaches both edges, on `focus` (a share
/// of the picture across and down) along the side with picture to spare.
pub fn fill_crop(aspect: f32, width: u32, height: u32, focus: (f32, f32)) -> CoverCrop {
    let frame = width as f32 / height.max(1) as f32;
    if !aspect.is_finite() || aspect <= 0.0 || (aspect / frame - 1.0).abs() < 0.01 {
        return CoverCrop::default();
    }
    // A hair over, so a rounded edge leaves no line of bar.
    let over = 1.002;
    let crop = if aspect > frame {
        CoverCrop { x: focus.0, y: 0.5, zoom: aspect / frame * over }
    } else {
        CoverCrop { x: 0.5, y: focus.1, zoom: frame / aspect * over }
    };
    crop.clamped()
}

/// What the export renderer draws for `cover` of `source`: the sequence
/// at `width` × `height`, its own titles and captions hidden unless the
/// cover keeps them, the crop as a camera held still on the frame, and
/// the cover's text on a track above everything at depth 0, where the
/// camera doesn't reach it. An error for a crop on a sequence with a
/// camera move of its own, which the crop would replace.
pub fn cover_sequence(source: &Sequence, cover: &Cover, width: u32, height: u32) -> Result<Sequence, String> {
    let crop = cover.crop.unwrap_or_default().clamped();
    if !crop.is_whole() && !source.camera.is_still() {
        return Err("the sequence has a camera move of its own; a cover of it shows the frame as its camera has it, so leave the crop out".into());
    }
    let mut seq = source.clone();
    seq.resolution = klipp_core::Resolution { width, height };
    seq.covers.clear();
    if !cover.with_titles {
        for t in seq.tracks.iter_mut().filter(|t| matches!(t.kind, TrackKind::Text | TrackKind::Subtitle)) {
            t.visible = false;
        }
    }
    if !crop.is_whole() {
        let key = |value: f32| vec![Keyframe { time_secs: cover.at_secs, value, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }];
        seq.camera.x_keyframes = key(crop.x);
        seq.camera.y_keyframes = key(crop.y);
        seq.camera.zoom_keyframes = key(crop.zoom);
        seq.camera.shutter_deg = 0.0;
    }
    if let Some(text) = &cover.text {
        // A still, so at rest: a title's entrance, which the title op gives
        // by default, has it not yet on screen at the one frame drawn.
        let mut text = text.clone();
        for fx in text.objects.iter_mut().filter_map(|o| o.fx.as_mut()) {
            fx.motion = Default::default();
        }
        let rate = seq.fps.rate();
        let frame = RationalTime::from_secs_f64(cover.at_secs, rate);
        let one = RationalTime::from_frames(1, rate);
        let mut words = Clip { kind: ClipKind::Title(text), source_in: RationalTime { value: 0, rate }, source_out: one, ..Clip::gap(frame, frame + one) };
        words.depth = 0.0;
        let mut track = Track::new("Cover", TrackKind::Text);
        track.clips.push(words);
        seq.tracks.insert(0, track);
    }
    Ok(seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cover(at: f64) -> Cover {
        Cover { id: CoverId::new(), name: "A".into(), at_secs: at, size: "youtube".into(), crop: None, text: None, with_titles: false }
    }

    /// A crop stays inside the frame and in range; a cover from a file
    /// keeps a sound name and size id; a frame that isn't a time, a
    /// cover given twice and covers past the most kept are dropped.
    #[test]
    fn a_cover_from_a_file_is_made_sound() {
        let c = CoverCrop { x: 0.0, y: 2.0, zoom: 2.0 }.clamped();
        assert_eq!((c.x, c.y, c.zoom), (0.25, 0.75, 2.0));
        assert_eq!(CoverCrop { x: f32::NAN, y: 0.5, zoom: 100.0 }.clamped(), CoverCrop { x: 0.5, y: 0.5, zoom: MAX_ZOOM });
        assert_eq!(CoverCrop { x: 0.1, y: 0.9, zoom: 0.2 }.clamped(), CoverCrop::default(), "under 1 is the whole frame");

        let mut odd = cover(1.0);
        odd.name = format!("  a\u{0007}b{}", "x".repeat(200));
        odd.size = "../../etc".into();
        let odd = odd.checked().unwrap();
        assert!(odd.name.starts_with("ab") && odd.name.chars().count() == MAX_NAME_CHARS, "{}", odd.name);
        assert_eq!(odd.size, "etc");
        assert!(cover(f64::NAN).checked().is_none() && cover(-1.0).checked().is_none() && cover(1e300).checked().is_none());

        let twice = cover(2.0);
        let mut many = vec![twice.clone(), twice];
        many.extend((0..40).map(|k| cover(k as f64)));
        let kept = checked(many);
        assert_eq!(kept.len(), MAX_COVERS);
        assert_eq!(kept.iter().filter(|c| c.at_secs == 2.0).count(), 2, "the copy dropped, the other cover at 2 s kept");
    }

    /// A crop chosen is kept, the whole frame too, apart from none chosen; a
    /// file from before, with none written, has none chosen.
    #[test]
    fn a_chosen_whole_frame_is_told_from_no_crop() {
        let mut c = cover(1.0);
        let none: Cover = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(none.crop, None);
        c.crop = Some(CoverCrop::default());
        let whole: Cover = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(whole.crop, Some(CoverCrop::default()));
        assert_eq!(whole.checked().unwrap().crop, Some(CoverCrop::default()));
    }

    /// 16:9 into tall and square goes in by the shapes' ratio, along the
    /// picture to the focus, held so the part shown stays on the picture;
    /// tall into wide the other way; the same shape needs nothing.
    #[test]
    fn a_crop_fills_the_size_on_the_focus() {
        let wide = 16.0 / 9.0;
        let tall = fill_crop(wide, 1080, 1920, (0.2, 0.9));
        assert!((tall.zoom - wide * 16.0 / 9.0 * 1.002).abs() < 1e-3, "{tall:?}");
        assert!((tall.x - 0.2).abs() < 1e-5 && tall.y == 0.5, "across only: {tall:?}");
        let edge = fill_crop(wide, 1080, 1080, (0.0, 0.5));
        assert!((edge.x - 0.5 / edge.zoom).abs() < 1e-5, "stays on the picture: {edge:?}");
        let across = fill_crop(9.0 / 16.0, 1280, 720, (0.5, 0.25));
        assert!(across.x == 0.5 && (across.y - 0.25).abs() < 1e-5 && across.zoom > 3.0, "{across:?}");
        assert!(fill_crop(wide, 1280, 720, (0.1, 0.1)).is_whole());
        assert!(fill_crop(0.0, 1280, 720, (0.1, 0.1)).is_whole());
    }

    /// The cover's sequence is the source at the cover's size, its titles
    /// hidden unless kept, the crop a still camera, and the text on a
    /// track of its own at depth 0 over the cover's frame. A crop on a
    /// sequence with a camera move is refused; without a crop it draws.
    #[test]
    fn a_cover_is_the_sequence_at_its_size_with_its_crop_and_words() {
        let mut source = Sequence::new_default();
        source.tracks.insert(0, Track::new("Text 1", TrackKind::Text));
        let mut c = cover(2.0);
        c.crop = Some(CoverCrop { x: 0.6, y: 0.4, zoom: 2.0 });
        c.text = Some(crate::TitleData::single_text(crate::TextBox { text: "Big News".into(), ..Default::default() }));
        let seq = cover_sequence(&source, &c, 1280, 720).unwrap();
        assert_eq!((seq.resolution.width, seq.resolution.height), (1280, 720));
        let words = &seq.tracks[0];
        assert_eq!((words.kind, words.clips.len()), (TrackKind::Text, 1));
        let clip = &words.clips[0];
        assert_eq!(clip.depth, 0.0, "out of the camera's reach");
        let at = RationalTime::from_secs_f64(2.0, seq.fps.rate());
        assert!(clip.timeline_in <= at && at < clip.timeline_out, "over the cover's frame");
        assert!(!seq.tracks[1].visible, "the sequence's own titles hidden");
        let v = |k: &[Keyframe]| k.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>();
        assert_eq!((v(&seq.camera.x_keyframes), v(&seq.camera.zoom_keyframes)), (vec![(2.0, 0.6)], vec![(2.0, 2.0)]));

        c.with_titles = true;
        assert!(cover_sequence(&source, &c, 1280, 720).unwrap().tracks[1].visible, "kept when asked");

        let mut moving = source.clone();
        moving.camera.zoom_keyframes = seq.camera.zoom_keyframes.clone();
        assert!(cover_sequence(&moving, &c, 1280, 720).unwrap_err().contains("camera move"));
        c.crop = None;
        assert_eq!(cover_sequence(&moving, &c, 1280, 720).unwrap().camera, moving.camera, "its own camera kept");
    }
}
