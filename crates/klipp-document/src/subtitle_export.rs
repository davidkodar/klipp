//! Which subtitle files an export of a sequence's captions writes, and
//! from which tracks (#2223).
//!
//! Subtitle tracks are the captions; text tracks stand in only when there
//! are none, all of them into one file (with both, the text tracks hold
//! titles and lower thirds, and a speaker's name does not belong in a
//! subtitle file). One subtitle track writes the file asked for. More than
//! one, each a language of the same captions, write a file each beside
//! it, named by the track's language: `film.de.srt`, `film.fr.srt`; a
//! track with none, or with a language another track has too, by its
//! place among them instead: `film.2.srt`.

use std::path::{Path, PathBuf};

use klipp_timeline::{Sequence, TrackKind};

/// The languages a subtitle track's menu offers, by tag and name: the
/// translation pairs' first, then others captions are often in.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"), ("de", "German"), ("fr", "French"), ("es", "Spanish"), ("it", "Italian"), ("nl", "Dutch"),
    ("sv", "Swedish"), ("da", "Danish"), ("fi", "Finnish"), ("no", "Norwegian"), ("pt", "Portuguese"), ("pl", "Polish"),
    ("cs", "Czech"), ("hu", "Hungarian"), ("ro", "Romanian"), ("el", "Greek"), ("tr", "Turkish"), ("ru", "Russian"),
    ("uk", "Ukrainian"), ("ar", "Arabic"), ("he", "Hebrew"), ("hi", "Hindi"), ("ja", "Japanese"), ("ko", "Korean"), ("zh", "Chinese"),
];

/// One file an export writes.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleFile {
    pub path: PathBuf,
    /// The tracks whose captions it holds, by index.
    pub tracks: Vec<usize>,
    /// The language of its captions: its track's, when it is one track's.
    pub language: Option<String>,
}

/// The files an export of `seq`'s captions to `path` writes.
pub fn files(seq: &Sequence, path: &Path) -> Vec<SubtitleFile> {
    let subtitle: Vec<usize> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Subtitle && !t.clips.is_empty()).map(|(i, _)| i).collect();
    if subtitle.is_empty() {
        let text = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Text).map(|(i, _)| i).collect();
        return vec![SubtitleFile { path: path.to_path_buf(), tracks: text, language: None }];
    }
    let language = |i: usize| seq.tracks[i].language.clone().filter(|l| !l.trim().is_empty());
    if let [only] = subtitle[..] {
        return vec![SubtitleFile { path: path.to_path_buf(), tracks: vec![only], language: language(only) }];
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    subtitle
        .iter()
        .enumerate()
        .map(|(n, &i)| {
            let lang = language(i);
            let shared = lang.as_ref().is_some_and(|l| subtitle.iter().filter(|&&j| language(j).as_ref() == Some(l)).count() > 1);
            let suffix = match &lang {
                Some(l) if !shared => l.clone(),
                _ => (n + 1).to_string(),
            };
            let name = match &ext {
                Some(e) => format!("{stem}.{suffix}.{e}"),
                None => format!("{stem}.{suffix}"),
            };
            SubtitleFile { path: path.with_file_name(name), tracks: vec![i], language: lang }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{Clip, Track};

    fn track(kind: TrackKind, language: Option<&str>, cues: usize) -> Track {
        let mut t = Track::new("T", kind);
        t.language = language.map(str::to_string);
        let tc = |s: i64| RationalTime { value: s, rate: 1 };
        t.clips = (0..cues as i64).map(|i| Clip::gap(tc(i), tc(i + 1))).collect();
        t
    }

    fn seq(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = tracks;
        s
    }

    fn names(files: &[SubtitleFile]) -> Vec<String> {
        files.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    /// One subtitle track writes the file asked for, in its language.
    #[test]
    fn one_track_writes_the_file_asked_for() {
        let s = seq(vec![track(TrackKind::Video, None, 1), track(TrackKind::Subtitle, Some("de"), 2), track(TrackKind::Subtitle, Some("fr"), 0)]);
        let f = files(&s, Path::new("/out/film.srt"));
        assert_eq!(f, vec![SubtitleFile { path: "/out/film.srt".into(), tracks: vec![1], language: Some("de".into()) }]);
    }

    /// More than one: a file each, by language, or by place where a track
    /// has none or shares its language.
    #[test]
    fn more_tracks_write_a_file_each_named_by_language() {
        let s = seq(vec![track(TrackKind::Subtitle, Some("en"), 2), track(TrackKind::Subtitle, Some("de"), 2), track(TrackKind::Subtitle, None, 1)]);
        let f = files(&s, Path::new("/out/film.vtt"));
        assert_eq!(names(&f), ["film.en.vtt", "film.de.vtt", "film.3.vtt"]);
        assert_eq!(f.iter().map(|f| f.tracks.clone()).collect::<Vec<_>>(), [vec![0], vec![1], vec![2]]);
        let s = seq(vec![track(TrackKind::Subtitle, Some("de"), 1), track(TrackKind::Subtitle, Some("de"), 1)]);
        assert_eq!(names(&files(&s, Path::new("/out/film.srt"))), ["film.1.srt", "film.2.srt"]);
    }

    /// With no subtitle track, every text track into the one file, with no
    /// language of its own.
    #[test]
    fn text_tracks_stand_in_for_none() {
        let s = seq(vec![track(TrackKind::Text, Some("de"), 1), track(TrackKind::Text, None, 1), track(TrackKind::Subtitle, Some("fr"), 0)]);
        assert_eq!(files(&s, Path::new("/out/film.srt")), vec![SubtitleFile { path: "/out/film.srt".into(), tracks: vec![0, 1], language: None }]);
    }
}
