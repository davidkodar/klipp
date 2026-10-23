//! A review round trip without a service (#2214): a review copy of a
//! sequence with its timecode and round burned in, a comment template
//! keyed to the sequence and round, comments read back from that template
//! or the common layouts reviewers send, and the comments as review
//! markers that move with the edit.
use klipp_core::{Fps, MarkerColor, MarkerId, MarkerKind, ReviewStatus};
use klipp_timeline::{Marker, ReviewNote, Sequence, Track, TrackKind};

/// The review round a new review copy is: one past the latest round any
/// review marker came from.
pub fn next_round(seq: &Sequence) -> u32 {
    seq.markers.iter().filter_map(|m| m.review.as_ref()).map(|r| r.round).max().unwrap_or(0) + 1
}

/// `secs` as `HH:MM:SS`, as the review copy shows it.
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0).floor() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// `seq` with a review line burned in over it: a caption each second
/// reading "REVIEW · name · round N · HH:MM:SS", on a subtitle track of
/// its own above the others. The export burns subtitles in for it.
pub fn overlay(seq: &Sequence, round: u32) -> Sequence {
    let mut out = seq.clone();
    let fps = seq.fps;
    let end = seq.total_duration().to_secs_f64();
    let mut track = Track::new("Review", TrackKind::Subtitle);
    let mut t = 0.0;
    while t < end {
        let to = (t + 1.0).min(end);
        let text = format!("REVIEW \u{b7} {} \u{b7} round {round} \u{b7} {}", seq.name, clock(t));
        let clip = crate::ops::title_clip(crate::ops::caption_title(&text, seq.resolution), crate::ops::snap(t, fps), crate::ops::snap(to, fps), fps);
        track.clips.push(clip);
        t = to;
    }
    out.tracks.insert(0, track);
    out
}

/// The comment file sent with a review copy: which sequence and round it
/// is for, and the columns comments are read back from.
pub fn comment_template(seq: &Sequence, round: u32) -> String {
    format!(
        "# Klipp review comments\n# sequence: {}\n# round: {round}\n# One comment a line. Timecode as on the review copy (HH:MM:SS), or HH:MM:SS:FF, MM:SS or seconds.\ntimecode,author,comment\n",
        seq.name
    )
}

/// The round a comment file says it is for (the template's `# round:`).
pub fn file_round(text: &str) -> Option<u32> {
    text.lines().find_map(|l| l.trim().strip_prefix("# round:").and_then(|r| r.trim().parse().ok()))
}

/// One comment read back from a reviewer.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewComment {
    pub secs: f64,
    pub author: String,
    pub text: String,
}

/// Seconds from a timecode as reviewers write it: `HH:MM:SS:FF` (at
/// `fps`), `HH:MM:SS(.mmm)`, `MM:SS(.mmm)`, or plain seconds; a leading
/// `[` or trailing `]` is allowed.
pub fn parse_timecode(text: &str, fps: f64) -> Option<f64> {
    let t = text.trim().trim_start_matches('[').trim_end_matches(']').trim();
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = t.split(':').collect();
    let num = |s: &str| s.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0);
    let secs = match parts.as_slice() {
        [s] => num(s)?,
        [m, s] => num(m)? * 60.0 + num(s)?,
        [h, m, s] => num(h)? * 3600.0 + num(m)? * 60.0 + num(s)?,
        [h, m, s, f] => {
            let frames = num(f)?;
            if fps <= 0.0 || frames >= fps.ceil() {
                return None;
            }
            num(h)? * 3600.0 + num(m)? * 60.0 + num(s)? + frames / fps
        }
        _ => return None,
    };
    Some(secs)
}

/// The fields of one delimited line, with double-quoted fields kept whole.
fn fields(line: &str, delimiter: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == delimiter && !quoted => out.push(std::mem::take(&mut field).trim().to_string()),
            c => field.push(c),
        }
    }
    out.push(field.trim().to_string());
    out
}

/// Words a reviewer writes before a colon that label a comment rather
/// than sign it.
const LABELS: &[&str] = &[
    "note", "notes", "todo", "fix", "question", "idea", "comment", "re", "nb", "ps", "edit", "cut", "music", "audio", "sound", "mix", "colour", "color",
    "grade", "title", "titles", "text", "vfx", "gfx", "picture", "logo", "caption", "captions", "subtitle", "subtitles", "general", "overall", "client",
];

/// Whether `name` reads as a person's name: one to three words, each
/// starting with a capital letter, no digits, not a label word.
fn looks_like_a_name(name: &str) -> bool {
    let words: Vec<&str> = name.split_whitespace().collect();
    !words.is_empty()
        && words.len() <= 3
        && name.len() <= 32
        && words.iter().all(|w| w.chars().next().is_some_and(char::is_uppercase) && !w.contains(char::is_numeric))
        && !words.iter().any(|w| LABELS.contains(&w.to_lowercase().as_str()))
}

/// Comments read back from `text`: the comment template, or any
/// delimited table (comma, semicolon or tab) whose header names a time
/// column and a comment column, or plain lines that begin with a
/// timecode (`00:01:23 text`, `[1:23] Name: text`, `1:23 - text`). Lines
/// starting with `#` are notes. `Err` when nothing in it is a comment.
pub fn parse_comments(text: &str, fps: f64) -> Result<Vec<ReviewComment>, String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let mut out = Vec::new();
    let header = lines.first().copied().unwrap_or("");
    let delimiter = ['\t', ';', ','].into_iter().find(|d| header.contains(*d));
    let columns = delimiter.map(|d| fields(header, d).into_iter().map(|f| f.to_lowercase()).collect::<Vec<_>>());
    let find = |cols: &[String], names: &[&str]| cols.iter().position(|c| names.iter().any(|n| c.contains(n)));
    if let (Some(d), Some(cols)) = (delimiter, columns.as_ref()) {
        if let (Some(tc), Some(note)) = (find(cols, &["timecode", "time", "tc", "position"]), find(cols, &["comment", "note", "text", "feedback"])) {
            let author = find(cols, &["author", "name", "reviewer", "who"]);
            for (n, line) in lines.iter().enumerate().skip(1) {
                let f = fields(line, d);
                let Some(secs) = f.get(tc).and_then(|t| parse_timecode(t, fps)) else {
                    return Err(format!("line {}: no timecode in \"{line}\"", n + 1));
                };
                let text = f.get(note).cloned().unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                out.push(ReviewComment { secs, author: author.and_then(|a| f.get(a).cloned()).unwrap_or_default(), text });
            }
            return if out.is_empty() { Err("the table has no comments in it".into()) } else { Ok(out) };
        }
    }
    for line in &lines {
        let (stamp, rest) = match line.find(|c: char| c.is_whitespace() || c == ']') {
            Some(i) => line.split_at(i + usize::from(line.as_bytes()[i] == b']')),
            None => continue,
        };
        let Some(secs) = parse_timecode(stamp, fps) else { continue };
        let rest = rest.trim().trim_start_matches(['-', '\u{2013}', '\u{2014}', ':']).trim();
        // "Name: text" names its author when what comes before the colon
        // reads as a name, not a label such as "note:" or "music:".
        let (author, text) = match rest.split_once(':') {
            Some((name, said)) if looks_like_a_name(name) => (name.trim().to_string(), said.trim().to_string()),
            _ => (String::new(), rest.to_string()),
        };
        if !text.is_empty() {
            out.push(ReviewComment { secs, author, text });
        }
    }
    if out.is_empty() {
        Err("no line begins with a timecode, and there is no table with a time and a comment column".into())
    } else {
        Ok(out)
    }
}

/// `comments` as review markers of `round`, open, leaving out any the
/// sequence already has (the same round, text and moment).
pub fn markers(seq: &Sequence, comments: &[ReviewComment], round: u32) -> Vec<Marker> {
    let fps: Fps = seq.fps;
    let frame = 1.0 / fps.as_f64().max(1.0);
    comments
        .iter()
        .filter(|c| {
            !seq.markers.iter().any(|m| {
                m.review.as_ref().is_some_and(|r| r.round == round) && m.comment == c.text && (m.position.to_secs_f64() - c.secs).abs() < frame
            })
        })
        .map(|c| Marker {
            id: MarkerId::new(),
            position: crate::ops::snap(c.secs, fps),
            label: if c.author.is_empty() { format!("Review {round}") } else { c.author.clone() },
            color: MarkerColor::Red,
            duration: None,
            comment: c.text.clone(),
            kind: MarkerKind::Review,
            review: Some(ReviewNote { author: c.author.clone(), status: ReviewStatus::Open, round }),
        })
        .collect()
}

/// The review markers as a table for the next round: timecode, round,
/// author, status, comment, in timeline order.
pub fn review_list_csv(seq: &Sequence) -> String {
    let quote = |s: &str| if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    let mut rows: Vec<&Marker> = seq.markers.iter().filter(|m| m.review.is_some()).collect();
    rows.sort_by_key(|m| m.position);
    let mut out = String::from("timecode,round,author,status,comment\n");
    for m in rows {
        let r = m.review.as_ref().expect("filtered to review markers");
        out.push_str(&format!("{},{},{},{},{}\n", clock(m.position.to_secs_f64()), r.round, quote(&r.author), r.status.label(), quote(&m.comment)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecodes_in_the_forms_reviewers_write() {
        assert_eq!(parse_timecode("00:01:23", 25.0), Some(83.0));
        assert_eq!(parse_timecode("[1:23]", 25.0), Some(83.0));
        assert_eq!(parse_timecode("01:00:00:12", 24.0), Some(3600.5));
        assert_eq!(parse_timecode("83.5", 25.0), Some(83.5));
        assert_eq!(parse_timecode("00:00:01:30", 25.0), None, "frame 30 is past a 25 fps second");
        assert_eq!(parse_timecode("soon", 25.0), None);
    }

    #[test]
    fn the_template_and_other_tables_read_back() {
        let seq = Sequence::new_default();
        let mut file = comment_template(&seq, 2);
        file.push_str("00:00:05,Ana,\"Tighten this, it drags\"\n00:00:09,,Logo too small\n");
        let c = parse_comments(&file, 25.0).unwrap();
        assert_eq!(c, vec![
            ReviewComment { secs: 5.0, author: "Ana".into(), text: "Tighten this, it drags".into() },
            ReviewComment { secs: 9.0, author: String::new(), text: "Logo too small".into() },
        ]);
        let semi = "Time;Reviewer;Note\n1:02;Bo;Colour is off\n";
        assert_eq!(parse_comments(semi, 25.0).unwrap()[0], ReviewComment { secs: 62.0, author: "Bo".into(), text: "Colour is off".into() });
        assert!(parse_comments("time,comment\nlater,fix it\n", 25.0).unwrap_err().contains("line 2"));
    }

    #[test]
    fn plain_lines_that_begin_with_a_timecode_read_back() {
        let text = "Notes from Friday\n00:00:12 music too loud\n[0:30] Ana: cut the second shot\n1:05 - end on the logo\n";
        let c = parse_comments(text, 25.0).unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], ReviewComment { secs: 12.0, author: String::new(), text: "music too loud".into() });
        assert_eq!(c[1], ReviewComment { secs: 30.0, author: "Ana".into(), text: "cut the second shot".into() });
        assert_eq!(c[2], ReviewComment { secs: 65.0, author: String::new(), text: "end on the logo".into() });
        assert!(parse_comments("nothing here\n", 25.0).is_err());
        // A label before the colon is part of the comment, not its author.
        let c = parse_comments("0:10 note: shorter here\n0:20 Music: too loud\n0:30 ana: lower case\n0:40 Ana Lind: fine\n", 25.0).unwrap();
        assert_eq!(c.iter().map(|c| c.author.as_str()).collect::<Vec<_>>(), vec!["", "", "", "Ana Lind"]);
        assert_eq!(c[0].text, "note: shorter here");
    }

    #[test]
    fn comments_become_open_review_markers_once() {
        let mut seq = Sequence::new_default();
        let comments = vec![ReviewComment { secs: 3.0, author: "Ana".into(), text: "shorter".into() }];
        let first = markers(&seq, &comments, 1);
        assert_eq!(first.len(), 1);
        let m = &first[0];
        assert_eq!((m.kind, m.label.as_str(), m.comment.as_str()), (MarkerKind::Review, "Ana", "shorter"));
        assert_eq!(m.review, Some(ReviewNote { author: "Ana".into(), status: ReviewStatus::Open, round: 1 }));
        seq.markers.extend(first);
        assert!(markers(&seq, &comments, 1).is_empty(), "the same comment imported twice is one marker");
        assert_eq!(next_round(&seq), 2);
        assert!(review_list_csv(&seq).contains("00:00:03,1,Ana,Open,shorter"));
    }

    #[test]
    fn the_overlay_names_each_second_on_a_track_of_its_own() {
        let mut seq = Sequence::new_default();
        seq.name = "Cut".into();
        let mut track = Track::new("V", TrackKind::Video);
        track.clips.push(klipp_timeline::Clip::gap(klipp_core::Timecode::from_secs_f64(0.0, 24), klipp_core::Timecode::from_secs_f64(3.0, 24)));
        seq.tracks.push(track);
        let review = overlay(&seq, 3);
        assert_eq!(review.tracks.len(), seq.tracks.len() + 1);
        let t = &review.tracks[0];
        assert_eq!(t.kind, TrackKind::Subtitle);
        assert_eq!(t.clips.len(), 3, "one caption a second");
        let text = t.clips[1].title_data().and_then(|td| td.primary_text().map(|tb| tb.text.clone())).unwrap();
        assert_eq!(text, "REVIEW \u{b7} Cut \u{b7} round 3 \u{b7} 00:00:01");
        assert!(seq.tracks.iter().all(|t| t.name != "Review"), "the sequence itself is unchanged");
    }
}
