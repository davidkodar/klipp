//! Find and replace in title text (#1717): every text layer of every
//! title on a sequence's text tracks, and its subtitle tracks when asked.
//! A match is found as written or regardless of case, and anywhere or
//! as a whole word; a replacement keeps the text's styled runs where
//! they were, a run around the word growing or shrinking with it and a
//! run on the word itself covering what replaces it.

use klipp_core::ClipId;
use klipp_timeline::{Sequence, StyleRun, TextBox, TitleObjectContent, TitleObjectId, TrackKind};

use crate::history::EditCommand;

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextQuery {
    pub find: String,
    pub match_case: bool,
    pub whole_word: bool,
    /// The subtitle tracks' cues too, not only the titles.
    pub subtitles: bool,
}

/// One place the text is found: the title, its text layer, the bytes,
/// where the title starts, and the line it is on, for a list to show.
#[derive(Debug, Clone, PartialEq)]
pub struct TextMatch {
    pub clip_id: ClipId,
    pub object: TitleObjectId,
    pub start: usize,
    pub end: usize,
    pub at_secs: f64,
    pub line: String,
}

/// Where `q` is in `text`, as byte ranges, each after the last.
pub fn find_in_text(text: &str, q: &TextQuery) -> Vec<(usize, usize)> {
    let needle: Vec<char> = q.find.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let same = |a: char, b: char| if q.match_case { a == b } else { a == b || a.to_lowercase().eq(b.to_lowercase()) };
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= chars.len() {
        if (0..needle.len()).all(|k| same(chars[i + k].1, needle[k])) {
            let start = chars[i].0;
            let end = chars.get(i + needle.len()).map_or(text.len(), |c| c.0);
            let before = i.checked_sub(1).map(|j| chars[j].1);
            let after = chars.get(i + needle.len()).map(|c| c.1);
            if !q.whole_word || (!word(before) && !word(after)) {
                out.push((start, end));
                i += needle.len();
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Every match of `q` in `seq`'s titles, in timeline order.
pub fn find_in_titles(seq: &Sequence, q: &TextQuery) -> Vec<TextMatch> {
    let mut out = Vec::new();
    for track in seq.tracks.iter().filter(|t| t.kind == TrackKind::Text || (q.subtitles && t.kind == TrackKind::Subtitle)) {
        for clip in &track.clips {
            let Some(td) = clip.title_data() else { continue };
            for o in &td.objects {
                let TitleObjectContent::TextBox(tb) = &o.content else { continue };
                for (start, end) in find_in_text(&tb.text, q) {
                    let line_start = tb.text[..start].rfind('\n').map_or(0, |i| i + 1);
                    let line_end = tb.text[end..].find('\n').map_or(tb.text.len(), |i| end + i);
                    out.push(TextMatch { clip_id: clip.id, object: o.id, start, end, at_secs: clip.timeline_in.to_secs_f64(), line: tb.text[line_start..line_end].to_string() });
                }
            }
        }
    }
    out.sort_by(|a, b| a.at_secs.total_cmp(&b.at_secs).then(a.start.cmp(&b.start)));
    out
}

/// `with` put in place of `ranges` of `tb`'s text (ascending, apart),
/// its styled runs kept where they were.
pub fn replace_in_text(tb: &mut TextBox, ranges: &[(usize, usize)], with: &str) {
    for &(a, b) in ranges.iter().rev() {
        let len = with.len();
        // Where a byte of the old text lands: before the match as it was,
        // after it moved by the change, inside it no further than the
        // replacement reaches.
        let map = |x: usize| if x <= a { x } else if x >= b { x + len - (b - a) } else { a + (x - a).min(len) };
        tb.style_runs = tb
            .style_runs
            .iter()
            .filter_map(|r| {
                let (start, end) = (map(r.start), map(r.end));
                (start < end).then(|| StyleRun { start, end, ..r.clone() })
            })
            .collect();
        tb.text.replace_range(a..b, with);
    }
}

/// The edits that put `with` in place of `q` in `seq`'s titles: every
/// match, or only `only`. One edit per title changed.
pub fn replace_in_titles(seq: &Sequence, q: &TextQuery, with: &str, only: Option<&TextMatch>) -> Vec<EditCommand> {
    let mut edits = Vec::new();
    for track in seq.tracks.iter().filter(|t| !t.locked && (t.kind == TrackKind::Text || (q.subtitles && t.kind == TrackKind::Subtitle))) {
        for clip in &track.clips {
            let Some(td) = clip.title_data() else { continue };
            if only.is_some_and(|m| m.clip_id != clip.id) {
                continue;
            }
            let mut new = td.clone();
            for o in new.objects.iter_mut() {
                let id = o.id;
                let TitleObjectContent::TextBox(tb) = &mut o.content else { continue };
                let ranges: Vec<(usize, usize)> = find_in_text(&tb.text, q)
                    .into_iter()
                    .filter(|&(s, e)| only.is_none_or(|m| m.object == id && m.start == s && m.end == e))
                    .collect();
                replace_in_text(tb, &ranges, with);
            }
            if new != *td {
                edits.push(EditCommand::EditTitle { clip_id: clip.id, description: "Replace Text", old_data: td.clone(), new_data: new });
            }
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Fps, Timecode};
    use klipp_timeline::{Clip, ClipKind, TitleData, Track};

    fn q(find: &str) -> TextQuery {
        TextQuery { find: find.into(), ..Default::default() }
    }

    #[test]
    fn matches_by_case_and_by_word() {
        let text = "Jon met jon at Jonsson's";
        assert_eq!(find_in_text(text, &q("jon")).len(), 3, "any case, anywhere");
        assert_eq!(find_in_text(text, &TextQuery { match_case: true, ..q("jon") }), [(8, 11)]);
        assert_eq!(find_in_text(text, &TextQuery { whole_word: true, ..q("jon") }), [(0, 3), (8, 11)], "not inside Jonsson");
        assert_eq!(find_in_text("ÉCOLE école", &q("école")).len(), 2, "case beyond ASCII");
        assert!(find_in_text(text, &q("")).is_empty());
    }

    #[test]
    fn a_replacement_keeps_the_styled_runs_where_they_were() {
        let mut tb = TextBox { text: "Meet Jon Smith today".into(), ..Default::default() };
        let red = |s, e| StyleRun { color: Some([255, 0, 0, 255]), ..StyleRun::default_for_range(s, e) };
        // A run around the name, a run on the name alone, one after it.
        tb.style_runs = vec![red(0, 14), StyleRun { font_size: Some(90.0), ..StyleRun::default_for_range(5, 8) }, red(15, 20)];
        let at = find_in_text(&tb.text, &q("Jon"));
        replace_in_text(&mut tb, &at, "Jonathan");
        assert_eq!(tb.text, "Meet Jonathan Smith today");
        let spans: Vec<&str> = tb.style_runs.iter().map(|r| &tb.text[r.start..r.end]).collect();
        assert_eq!(spans, ["Meet Jonathan Smith", "Jonathan", "today"], "the run round it grows, the one on it covers it, the next moves");
        assert_eq!(tb.style_runs[1].font_size, Some(90.0));
    }

    /// #1717's done-when: a word replaced in five titles in one step, a
    /// styled run around it kept; only one replaced when asked; subtitles
    /// only when asked.
    #[test]
    fn a_word_is_replaced_in_every_title_and_undone_together() {
        let fps = Fps::new(25, 1);
        let tc = |s: f64| Timecode::from_secs_f64(s, 25);
        let title = |text: &str, k: f64| {
            let mut c = Clip::gap(tc(k), tc(k + 1.0));
            let mut tb = TextBox { text: text.into(), ..Default::default() };
            tb.style_runs = vec![StyleRun { color: Some([0, 255, 0, 255]), ..StyleRun::default_for_range(0, text.len()) }];
            c.kind = ClipKind::Title(TitleData::single_text(tb));
            c
        };
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        let mut text = Track::new("Text 1", TrackKind::Text);
        text.clips = (0..5).map(|k| title(&format!("Dr Jon Smyth, part {k}"), k as f64 * 2.0)).collect();
        let mut subs = Track::new("Subtitles", TrackKind::Subtitle);
        subs.clips = vec![title("Smyth speaks", 20.0)];
        seq.tracks = vec![text, subs];

        let query = q("Smyth");
        assert_eq!(find_in_titles(&seq, &query).len(), 5, "the titles, not the subtitles");
        assert_eq!(find_in_titles(&seq, &TextQuery { subtitles: true, ..query.clone() }).len(), 6);
        let edits = replace_in_titles(&seq, &query, "Smith", None);
        assert_eq!(edits.len(), 5);
        let before: Vec<_> = seq.tracks[0].clips.iter().map(|c| c.title_data().cloned()).collect();
        let step = EditCommand::Named { label: "Replace Text".into(), edit: Box::new(EditCommand::Batch(edits)) };
        let mut seqs = vec![seq];
        let (mut pool, mut fps2) = (klipp_media::MediaPool::new(), fps);
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps2 };
        view.apply_forward(&step);
        for (k, c) in view.sequences[0].tracks[0].clips.iter().enumerate() {
            let tb = c.title_data().unwrap().primary_text().unwrap();
            assert_eq!(tb.text, format!("Dr Jon Smith, part {k}"));
            assert_eq!((tb.style_runs[0].start, tb.style_runs[0].end), (0, tb.text.len()), "the run around it still covers the text");
        }
        assert_eq!(view.sequences[0].tracks[1].clips[0].title_data().unwrap().text(), "Smyth speaks", "subtitles untouched");
        view.apply_inverse(&step);
        let after: Vec<_> = view.sequences[0].tracks[0].clips.iter().map(|c| c.title_data().cloned()).collect();
        assert_eq!(after, before, "one undo restores them all");

        // Only the one asked for.
        let seq = &view.sequences[0];
        let third = find_in_titles(seq, &query)[2].clone();
        let one = replace_in_titles(seq, &query, "Smith", Some(&third));
        assert!(matches!(&one[..], [EditCommand::EditTitle { clip_id, .. }] if *clip_id == third.clip_id));
    }
}
