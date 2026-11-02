//! A publish kit's text (#3312): chapters, a description and tags for an
//! export, written beside it as plain text. Klipp generates no prose: these
//! are drafts put together from the brief, the chapter markers and the
//! words heard, for a person or an assistant to rewrite. Nothing is uploaded.

use std::path::{Path, PathBuf};

use klipp_core::MarkerKind;
use klipp_timeline::{Brief, Marker};

use crate::ops::HeardWord;

/// A pause at least this long can start a proposed chapter.
const CHAPTER_PAUSE_SECS: f64 = 1.2;
/// Proposed chapters are at least this far apart: well over the ten seconds
/// YouTube asks for, so a chapter says something.
const PROPOSED_MIN_SECS: f64 = 30.0;
/// About one proposed chapter a minute at most.
const PROPOSED_EVERY_SECS: f64 = 60.0;
/// The words of a proposed chapter's label.
const LABEL_WORDS: usize = 6;
/// YouTube's rules for showing a chapter list.
const FEWEST_CHAPTERS: usize = 3;
const SHORTEST_CHAPTER_SECS: f64 = 10.0;
/// The limits of a description and of tags, the common platforms' own, and
/// what a client may hand in.
pub const MOST_DESCRIPTION_CHARS: usize = 5000;
pub const MOST_TAGS: usize = 15;
pub const MOST_TAG_CHARS: usize = 500;
pub const MOST_CHAPTERS: usize = 500;
pub const MOST_LABEL_CHARS: usize = 100;
/// The opening of the speech a draft description quotes.
const OPENING_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    /// Seconds from the start of what is exported.
    pub secs: f64,
    pub label: String,
}

/// Where a kit's chapters came from: only markers and a client's are more
/// than a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterSource {
    Markers,
    Proposed,
    Given,
}

impl ChapterSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Markers => "chapter_markers",
            Self::Proposed => "proposed_from_transcript",
            Self::Given => "given",
        }
    }
}

/// The chapters of the Chapter markers within `from..to`, in seconds from
/// `from`. YouTube requires the first at 0:00, so an "Intro" comes first when
/// the earliest is more than half a second in. Empty when there are none.
pub fn chapters_from_markers(markers: &[Marker], from: f64, to: f64) -> Vec<Chapter> {
    let mut chapters: Vec<Chapter> = markers
        .iter()
        .filter(|m| m.kind == MarkerKind::Chapter)
        .map(|m| (m.position.to_secs_f64(), m.label.trim()))
        .filter(|(at, _)| *at >= from && *at < to)
        .enumerate()
        .map(|(i, (at, label))| Chapter { secs: at - from, label: if label.is_empty() { format!("Chapter {}", i + 1) } else { label.to_string() } })
        .collect();
    if chapters.first().is_some_and(|c| c.secs > 0.5) {
        chapters.insert(0, Chapter { secs: 0.0, label: "Intro".to_string() });
    }
    chapters
}

/// Chapters proposed from the pauses in `words`, heard within `from..to`
/// and in order: the longest pauses first, kept apart, each labelled by the
/// words after it. Empty with no words.
pub fn propose_chapters(words: &[HeardWord], from: f64, to: f64) -> Vec<Chapter> {
    if words.is_empty() {
        return Vec::new();
    }
    let label = |at: usize| {
        let text = words[at..].iter().take(LABEL_WORDS).map(|w| w.text.trim()).collect::<Vec<_>>().join(" ");
        text.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string()
    };
    let mut pauses: Vec<(f64, usize)> = words.windows(2).enumerate().map(|(i, w)| (w[1].start - w[0].end, i + 1)).filter(|(gap, _)| *gap >= CHAPTER_PAUSE_SECS).collect();
    pauses.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let most = ((to - from) / PROPOSED_EVERY_SECS).floor().max(0.0) as usize;
    let mut starts: Vec<usize> = Vec::new();
    for (_, at) in pauses {
        if starts.len() >= most {
            break;
        }
        let t = words[at].start;
        if t - from >= PROPOSED_MIN_SECS && to - t >= PROPOSED_MIN_SECS && starts.iter().all(|&s| (words[s].start - t).abs() >= PROPOSED_MIN_SECS) {
            starts.push(at);
        }
    }
    starts.sort_unstable();
    std::iter::once(Chapter { secs: 0.0, label: label(0) }).chain(starts.into_iter().map(|at| Chapter { secs: words[at].start - from, label: label(at) })).collect()
}

/// The speech heard within `from..to` on the timeline, in order: no music's
/// lyrics, no effect's.
pub fn heard_speech(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, transcripts: &dyn crate::ops::Transcripts, from: f64, to: f64) -> Vec<HeardWord> {
    let mut words: Vec<HeardWord> = crate::ops::heard_words(seq, pool, transcripts).into_iter().filter(|w| w.start >= from && w.start < to && !crate::ops::not_speech(w.role.as_ref())).collect();
    words.sort_by(|a, b| a.start.total_cmp(&b.start));
    words
}

/// A client's chapters, checked: in order from 0 and within the limits, a
/// label on each, one line.
pub fn given_chapters(chapters: Vec<(f64, String)>) -> Result<Vec<Chapter>, String> {
    if chapters.len() > MOST_CHAPTERS {
        return Err(format!("{} chapters is too many: at most {MOST_CHAPTERS}", chapters.len()));
    }
    let mut last = -1.0;
    let mut out = Vec::with_capacity(chapters.len());
    for (secs, label) in chapters {
        let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
        if !secs.is_finite() || secs < 0.0 || secs <= last {
            return Err(format!("chapter {label:?} at {secs} s: give chapters in order, from 0, at different times"));
        }
        if label.is_empty() || label.chars().count() > MOST_LABEL_CHARS {
            return Err(format!("the chapter at {secs} s needs a label of 1 to {MOST_LABEL_CHARS} characters"));
        }
        last = secs;
        out.push(Chapter { secs, label });
    }
    Ok(out)
}

/// `H:MM:SS Label` a line, as a video description takes chapters.
pub fn chapters_text(chapters: &[Chapter]) -> String {
    chapters
        .iter()
        .map(|c| {
            let s = c.secs.max(0.0) as u64;
            format!("{}:{:02}:{:02} {}", s / 3600, (s % 3600) / 60, s % 60, c.label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What keeps a chapter list from showing on YouTube, in `duration` seconds:
/// said, not silently fixed.
pub fn chapter_warnings(chapters: &[Chapter], duration: f64) -> Vec<String> {
    let mut warnings = Vec::new();
    if chapters.is_empty() {
        return warnings;
    }
    if chapters.len() < FEWEST_CHAPTERS {
        warnings.push(format!("{} chapter(s): YouTube shows a chapter list from {FEWEST_CHAPTERS}.", chapters.len()));
    }
    if chapters[0].secs > 0.5 {
        warnings.push("The first chapter isn't at 0:00, where YouTube needs one.".to_string());
    }
    let ends = chapters.iter().skip(1).map(|c| c.secs).chain([duration]);
    if chapters.iter().zip(ends).any(|(c, end)| end - c.secs < SHORTEST_CHAPTER_SECS) {
        warnings.push(format!("A chapter is under {SHORTEST_CHAPTER_SECS:.0} s, shorter than YouTube allows."));
    }
    warnings
}

/// A draft description: the brief's title and what it's about, the opening
/// of the speech, the chapters, and what the viewer is asked to do.
pub fn description_draft(brief: Option<&Brief>, chapters: &[Chapter], words: &[HeardWord]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = brief {
        parts.extend([b.title.trim(), b.about.trim()].into_iter().filter(|s| !s.is_empty()).map(str::to_string));
    }
    let opening = opening_of(words);
    if !opening.is_empty() {
        parts.push(opening);
    }
    if chapters.len() >= 2 {
        parts.push(format!("Chapters:\n{}", chapters_text(chapters)));
    }
    if let Some(cta) = brief.map(|b| b.cta.trim()).filter(|s| !s.is_empty()) {
        parts.push(cta.to_string());
    }
    let text = parts.join("\n\n");
    text.chars().take(MOST_DESCRIPTION_CHARS).collect()
}

/// The speech's opening, whole sentences up to about [`OPENING_CHARS`].
fn opening_of(words: &[HeardWord]) -> String {
    let mut text = String::new();
    let mut whole = 0;
    for word in words {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(word.text.trim());
        if text.ends_with(['.', '!', '?']) {
            whole = text.len();
        }
        if text.chars().count() >= OPENING_CHARS {
            break;
        }
    }
    if whole > 0 {
        text.truncate(whole);
    }
    text
}

/// Draft tags, in this order: the brief's cast and spellings, then the names
/// said more than once (capitalised away from a sentence's start), then long
/// words said often. Without a list of each language's common words the last
/// can be plain words: it's a draft.
pub fn tags_draft(brief: Option<&Brief>, words: &[HeardWord]) -> Vec<String> {
    let mut named: Vec<(String, usize)> = Vec::new();
    let mut plain: Vec<(String, usize)> = Vec::new();
    let mut sentence_start = true;
    for word in words {
        let text = word.text.trim();
        let bare = text.trim_matches(|c: char| !c.is_alphanumeric());
        let capital = bare.chars().next().is_some_and(char::is_uppercase);
        let tally = if capital && !sentence_start { Some(&mut named) } else if bare.chars().count() >= 7 { Some(&mut plain) } else { None };
        if let Some(tally) = tally {
            let key = if capital && !sentence_start { bare.to_string() } else { bare.to_lowercase() };
            match tally.iter_mut().find(|(t, _)| t.eq_ignore_ascii_case(&key)) {
                Some((_, n)) => *n += 1,
                None => tally.push((key, 1)),
            }
        }
        sentence_start = text.ends_with(['.', '!', '?']);
    }
    let ranked = |mut tally: Vec<(String, usize)>, least: usize| {
        tally.retain(|(_, n)| *n >= least);
        tally.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        tally.into_iter().map(|(t, _)| t).collect::<Vec<_>>()
    };
    let briefed = brief.into_iter().flat_map(|b| b.cast.iter().map(|c| c.name.clone()).chain(b.vocabulary.iter().cloned()));
    tidy_tags(briefed.chain(ranked(named, 2)).chain(ranked(plain, 3)))
}

/// Tags trimmed, each once, within the limits, in the order given.
pub fn tidy_tags(tags: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut chars = 0;
    for tag in tags {
        // A comma would make two tags of one in the file.
        let tag = tag.replace([',', '\n', '\r'], " ").trim().to_string();
        let len = tag.chars().count();
        if tag.is_empty() || out.len() >= MOST_TAGS || chars + len > MOST_TAG_CHARS || out.iter().any(|t| t.to_lowercase() == tag.to_lowercase()) {
            continue;
        }
        chars += len;
        out.push(tag);
    }
    out
}

/// One text file of a kit.
#[derive(Debug, Clone, PartialEq)]
pub struct KitFile {
    /// "chapters", "description" or "tags".
    pub part: &'static str,
    /// Klipp's own draft rather than chapter markers or a client's text; its
    /// name says so.
    pub draft: bool,
    pub contents: String,
}

impl KitFile {
    /// Where this goes beside `video`: `film.description.draft.txt`.
    pub fn path_beside(&self, video: &Path) -> PathBuf {
        let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let draft = if self.draft { ".draft" } else { "" };
        video.with_file_name(format!("{stem}.{}{draft}.txt", self.part))
    }
}

/// Writes `files` beside `video`, every one or none when one is in the way:
/// a file already there is only replaced when `overwrite` says so. The paths
/// written, in order.
pub fn write_beside(video: &Path, files: &[KitFile], overwrite: bool) -> Result<Vec<PathBuf>, String> {
    let paths: Vec<PathBuf> = files.iter().map(|f| f.path_beside(video)).collect();
    if !overwrite {
        if let Some(there) = paths.iter().find(|p| p.exists()) {
            return Err(format!("{} exists: send overwrite true to replace it", there.display()));
        }
    }
    for (file, path) in files.iter().zip(&paths) {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true);
        if overwrite {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        let mut out = options.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        out.write_all(file.contents.as_bytes()).and_then(|_| out.write_all(b"\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(paths)
}

/// Whether a caption file stays beside `video`. A track's language names its
/// file, and a project can come from anywhere: a language holding a path
/// would write somewhere else.
pub fn stays_beside(file: &Path, video: &Path) -> bool {
    file.parent() == video.parent() && file.file_name().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{ClipId, MarkerColor, MarkerId, MediaId, Timecode};

    fn marker(secs: f64, label: &str, kind: MarkerKind) -> Marker {
        Marker { id: MarkerId::new(), position: Timecode::from_secs_f64(secs, 25), label: label.to_string(), color: MarkerColor::Orange, duration: None, comment: String::new(), kind, review: None }
    }

    fn said(start: f64, text: &str) -> HeardWord {
        HeardWord { start, end: start + 0.3, text: text.to_string(), after_cut: false, clip_id: ClipId::new(), media_id: MediaId::new(), role: None, clip_in: 0.0, clip_out: 1000.0 }
    }

    /// Words from `from`, one every half second.
    fn sentence(from: f64, text: &str) -> Vec<HeardWord> {
        text.split(' ').enumerate().map(|(i, w)| said(from + i as f64 * 0.5, w)).collect()
    }

    #[test]
    fn chapters_come_from_chapter_markers_only_within_the_range() {
        let markers = [marker(0.0, "Before", MarkerKind::Chapter), marker(10.0, "Note", MarkerKind::ToDo), marker(12.0, "Untyped", MarkerKind::None), marker(20.0, "Start", MarkerKind::Chapter), marker(40.0, "", MarkerKind::Chapter), marker(90.0, "After", MarkerKind::Chapter)];
        let c = chapters_from_markers(&markers, 20.0, 80.0);
        assert_eq!(c, [Chapter { secs: 0.0, label: "Start".into() }, Chapter { secs: 20.0, label: "Chapter 2".into() }]);
    }

    #[test]
    fn a_first_chapter_marker_after_the_start_gets_an_intro_and_none_stays_none() {
        let c = chapters_from_markers(&[marker(30.0, "Part", MarkerKind::Chapter)], 0.0, 60.0);
        assert_eq!(c, [Chapter { secs: 0.0, label: "Intro".into() }, Chapter { secs: 30.0, label: "Part".into() }]);
        assert!(chapters_from_markers(&[marker(30.0, "Note", MarkerKind::ToDo)], 0.0, 60.0).is_empty());
    }

    #[test]
    fn proposed_chapters_start_at_the_longest_pauses_kept_apart() {
        let mut words = sentence(0.0, "We open with the first part today");
        words.extend(sentence(40.0, "Then comes the second part of it"));
        words.extend(sentence(50.0, "A close pause here isn't a chapter"));
        words.extend(sentence(100.0, "Last we talk about the third"));
        let c = propose_chapters(&words, 0.0, 180.0);
        assert_eq!(c.iter().map(|c| c.secs).collect::<Vec<_>>(), [0.0, 40.0, 100.0]);
        assert_eq!(c[1].label, "Then comes the second part of");
    }

    #[test]
    fn proposed_chapters_are_relative_to_the_range_and_none_without_speech() {
        let mut words = sentence(100.0, "We open here");
        words.extend(sentence(160.0, "And go on here"));
        let c = propose_chapters(&words, 100.0, 300.0);
        assert_eq!(c.iter().map(|c| c.secs).collect::<Vec<_>>(), [0.0, 60.0]);
        assert!(propose_chapters(&[], 0.0, 300.0).is_empty());
    }

    #[test]
    fn chapter_text_and_its_warnings() {
        let c = [Chapter { secs: 0.0, label: "Intro".into() }, Chapter { secs: 83.5, label: "Main".into() }, Chapter { secs: 3661.0, label: "End".into() }];
        assert_eq!(chapters_text(&c), "0:00:00 Intro\n0:01:23 Main\n1:01:01 End");
        assert!(chapter_warnings(&c, 3700.0).is_empty());
        assert!(chapter_warnings(&c[..2], 3700.0)[0].contains("from 3"));
        assert!(chapter_warnings(&c, 3665.0)[0].contains("under 10 s"));
        assert!(chapter_warnings(&c[1..], 3700.0).iter().any(|w| w.contains("0:00")));
        assert!(chapter_warnings(&[], 10.0).is_empty());
    }

    #[test]
    fn a_description_draft_puts_the_brief_speech_chapters_and_ask_in_order() {
        let brief = Brief { title: "Film".into(), about: "About a river.".into(), cta: "Subscribe.".into(), ..Brief::default() };
        let c = [Chapter { secs: 0.0, label: "Intro".into() }, Chapter { secs: 60.0, label: "Next".into() }];
        let d = description_draft(Some(&brief), &c, &sentence(0.0, "The water runs. It runs far"));
        assert_eq!(d, "Film\n\nAbout a river.\n\nThe water runs.\n\nChapters:\n0:00:00 Intro\n0:01:00 Next\n\nSubscribe.");
        assert_eq!(description_draft(None, &[], &[]), "");
    }

    #[test]
    fn a_description_is_kept_within_the_limit() {
        let brief = Brief { about: "x".repeat(MOST_DESCRIPTION_CHARS * 2), ..Brief::default() };
        assert_eq!(description_draft(Some(&brief), &[], &[]).chars().count(), MOST_DESCRIPTION_CHARS);
    }

    #[test]
    fn tags_put_the_brief_first_then_names_then_long_words_said_often() {
        let brief = Brief { vocabulary: vec!["Omarchy".into(), "QML".into()], ..Brief::default() };
        let words = sentence(0.0, "Here Klipp cuts timeline and Klipp edits timeline and more. Timeline again with timeline. Nobody says Single once.");
        let tags = tags_draft(Some(&brief), &words);
        assert_eq!(tags, ["Omarchy", "QML", "Klipp", "timeline"]);
    }

    #[test]
    fn tags_are_trimmed_unique_comma_free_and_limited() {
        let t = tidy_tags(["a, b", " a  b ", "A, B", ""].map(String::from));
        assert_eq!(t, ["a  b"]);
        assert_eq!(tidy_tags((0..100).map(|i| format!("tag{i}"))).len(), MOST_TAGS);
        let long = tidy_tags((0..5).map(|i| format!("{i}{}", "x".repeat(200))));
        assert_eq!(long.len(), 2);
    }

    #[test]
    fn a_clients_chapters_are_checked() {
        let c = given_chapters(vec![(0.0, " Intro\nhere ".into()), (60.0, "Next".into())]).unwrap();
        assert_eq!(c[0].label, "Intro here");
        for bad in [vec![(-1.0, "A".to_string())], vec![(f64::NAN, "A".to_string())], vec![(5.0, "A".to_string()), (5.0, "B".to_string())], vec![(9.0, "A".to_string()), (5.0, "B".to_string())], vec![(0.0, "  ".to_string())], vec![(0.0, "x".repeat(MOST_LABEL_CHARS + 1))]] {
            assert!(given_chapters(bad.clone()).is_err(), "{bad:?}");
        }
        assert!(given_chapters((0..=MOST_CHAPTERS).map(|i| (i as f64, "A".to_string())).collect()).is_err());
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-publish-kit-{name}-{}", MediaId::new().0));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn kit() -> Vec<KitFile> {
        vec![KitFile { part: "chapters", draft: false, contents: "0:00:00 Intro".into() }, KitFile { part: "description", draft: true, contents: "Text".into() }]
    }

    #[test]
    fn files_are_written_beside_the_video_and_say_which_are_drafts() {
        let dir = scratch("write");
        let video = dir.join("film.mp4");
        let written = write_beside(&video, &kit(), false).unwrap();
        assert_eq!(written, [dir.join("film.chapters.txt"), dir.join("film.description.draft.txt")]);
        assert_eq!(std::fs::read_to_string(&written[1]).unwrap(), "Text\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// One file in the way stops them all, its contents kept, until asked.
    #[test]
    fn nothing_is_replaced_unless_asked() {
        let dir = scratch("keep");
        let video = dir.join("film.mp4");
        let there = dir.join("film.description.draft.txt");
        std::fs::write(&there, "mine").unwrap();
        assert!(write_beside(&video, &kit(), false).unwrap_err().contains("overwrite"));
        assert_eq!(std::fs::read_to_string(&there).unwrap(), "mine");
        assert!(!dir.join("film.chapters.txt").exists(), "none written");
        write_beside(&video, &kit(), true).unwrap();
        assert_eq!(std::fs::read_to_string(&there).unwrap(), "Text\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A project's track language can hold anything: `subtitle_export::files`
    /// makes it part of a file's name, and such a file doesn't stay beside.
    #[test]
    fn a_hostile_track_language_is_caught() {
        use klipp_timeline::{Clip, Sequence, Track, TrackKind};
        let video = Path::new("/out/film.mp4");
        assert!(stays_beside(Path::new("/out/film.de.srt"), video));
        let mut seq = Sequence::new_default();
        seq.tracks.clear();
        for language in ["de", "../../evil"] {
            let mut t = Track::new("T", TrackKind::Subtitle);
            t.language = Some(language.to_string());
            t.clips.push(Clip::gap(Timecode::from_frames(0, 25), Timecode::from_frames(25, 25)));
            seq.tracks.push(t);
        }
        let files = crate::subtitle_export::files(&seq, &video.with_extension("srt"));
        assert!(stays_beside(&files[0].path, video), "{:?}", files[0].path);
        assert!(!stays_beside(&files[1].path, video), "{:?}", files[1].path);
    }
}
