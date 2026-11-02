//! First Cut's dialog choices and its button's gate (#3308). The cut
//! itself is the document's `first_cut` op, the same an assistant sends;
//! this is what the dialog asks and where the button shows.
//!
//! A creator feature, so it stays out of the way (#3306): the toolbar's
//! button shows only while a clip with speech is on the timeline, and a
//! switch in Preferences hides it. Edit → First Cut… is always there.

use klipp_core::MediaId;
use klipp_document::ops::first_cut::Tightness;
use klipp_document::ops::{Op, Transcripts};
use klipp_media::MediaPool;
use klipp_timeline::{ClipKind, Sequence, TrackKind};

/// What the dialog asks, kept for the session: the last choices are the
/// next ones.
#[derive(Debug, Clone, PartialEq)]
pub struct Choices {
    pub silences: bool,
    pub fillers: bool,
    pub retakes: bool,
    pub punch: bool,
    pub captions: bool,
    /// A music bed, only when chosen.
    pub music: Option<MediaId>,
    pub tightness: Tightness,
}

impl Default for Choices {
    fn default() -> Self {
        Choices { silences: true, fillers: true, retakes: true, punch: true, captions: true, music: None, tightness: Tightness::Normal }
    }
}

impl Choices {
    /// The op these choices send.
    pub fn op(&self) -> Op {
        Op::FirstCut {
            silences: self.silences,
            fillers: self.fillers,
            retakes: self.retakes,
            punch: self.punch,
            captions: self.captions,
            music: self.music,
            tightness: self.tightness,
            exclude: Vec::new(),
        }
    }

    /// Whether any step is chosen.
    pub fn any(&self) -> bool {
        self.silences || self.fillers || self.retakes || self.punch || self.captions || self.music.is_some()
    }
}

/// Whether the timeline's First Cut button shows: speech on the timeline,
/// and the button not switched off in Preferences.
pub fn button_shown(speech: bool, switched_on: bool) -> bool {
    speech && switched_on
}

/// The sound files the dialog offers as a music bed, by name: files
/// with sound and no picture.
pub fn music_choices(pool: &MediaPool) -> Vec<(MediaId, String)> {
    let mut out: Vec<(MediaId, String)> = pool.all().filter(|m| m.has_audio && m.video_codec.is_empty()).map(|m| (m.id, m.filename.clone())).collect();
    out.sort_by_key(|(_, name)| name.to_lowercase());
    out
}

/// The files of clips that should speak but have no transcript, each
/// once, by name: what the dialog offers to transcribe first. A clip
/// whose role is speech, or a camera clip (picture and sound) with no role
/// at all, counts; music and effects don't.
pub fn untranscribed(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> Vec<(MediaId, String)> {
    let mut out: Vec<(MediaId, String)> = Vec::new();
    for clip in seq.tracks.iter().filter(|t| t.kind != TrackKind::Subtitle).flat_map(|t| &t.clips) {
        let ClipKind::Media { media_id } = &clip.kind else { continue };
        let Some(item) = pool.get(media_id) else { continue };
        if !item.has_audio || out.iter().any(|(m, _)| m == media_id) {
            continue;
        }
        let speaks = match &clip.role {
            Some(role) => role.is_speech(),
            None => !item.video_codec.is_empty(),
        };
        if speaks && transcripts.words(item).is_none() {
            out.push((*media_id, item.filename.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_shows_only_with_speech_and_the_switch_on() {
        assert!(button_shown(true, true));
        assert!(!button_shown(false, true), "no speech, no button");
        assert!(!button_shown(true, false), "switched off in Preferences");
    }

    #[test]
    fn the_choices_send_the_first_cut_op_with_music_only_when_chosen() {
        let plain = Choices::default();
        let Op::FirstCut { music, silences, captions, exclude, .. } = plain.op() else { panic!("a first_cut op") };
        assert!(music.is_none() && silences && captions && exclude.is_empty());
        let bed = MediaId::new();
        let chosen = Choices { music: Some(bed), punch: false, ..Choices::default() };
        let Op::FirstCut { music, punch, .. } = chosen.op() else { panic!("a first_cut op") };
        assert_eq!((music, punch), (Some(bed), false));
        let none = Choices { silences: false, fillers: false, retakes: false, punch: false, captions: false, music: None, tightness: Tightness::Gentle };
        assert!(!none.any() && Choices::default().any());
    }

    #[test]
    fn music_is_offered_from_sound_files_only() {
        let mut pool = MediaPool::new();
        let mut song = klipp_media::MediaItem::synthetic_for_test("/tmp/Bed.wav".into(), None);
        song.has_audio = true;
        song.video_codec = String::new();
        let mut camera = klipp_media::MediaItem::synthetic_for_test("/tmp/a-roll.mov".into(), None);
        camera.has_audio = true;
        camera.video_codec = "h264".into();
        let song_id = pool.add(song);
        pool.add(camera);
        assert_eq!(music_choices(&pool).into_iter().map(|(id, _)| id).collect::<Vec<_>>(), vec![song_id]);
    }
}
