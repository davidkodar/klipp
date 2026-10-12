//! YouTube chapter lists (#890); subtitles and captions are read and written by `klipp-captions`.

#[allow(unused_imports)]
use super::*;
pub(super) use klipp_captions::*;

/// Format an absolute time for YouTube chapter lines. YouTube accepts both
/// `M:SS` and `H:MM:SS`; we always emit `H:MM:SS` for consistency regardless
/// of sequence length. Negative inputs are clamped to zero.
pub(super) fn format_youtube_chapter_time(total_secs: f64) -> String {
    let secs = total_secs.max(0.0) as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{}:{:02}:{:02}", h, m, s)
}

/// Render a sorted `(position_secs, label)` list as YouTube chapter text.
/// Blank labels are substituted with `Chapter N` (1-indexed across all
/// entries, including any synthetic "Intro" the caller prepends). No trailing
/// newline — callers choose whether to append one.
pub(super) fn generate_youtube_chapters(entries: &[(f64, String)]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(i, (secs, label))| {
            let tc = format_youtube_chapter_time(*secs);
            let display_label = if label.trim().is_empty() {
                format!("Chapter {}", i + 1)
            } else {
                label.clone()
            };
            format!("{} {}", tc, display_label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod youtube_chapter_tests {
    use super::*;

    #[test]
    fn format_youtube_chapter_time_under_one_hour() {
        assert_eq!(format_youtube_chapter_time(0.0),   "0:00:00");
        assert_eq!(format_youtube_chapter_time(83.0),  "0:01:23");
        assert_eq!(format_youtube_chapter_time(287.0), "0:04:47");
    }

    #[test]
    fn format_youtube_chapter_time_over_one_hour() {
        assert_eq!(format_youtube_chapter_time(3661.0), "1:01:01");
    }

    #[test]
    fn generate_youtube_chapters_blank_label_substituted() {
        let entries = vec![
            (0.0, "Intro".to_string()),
            (83.0, "".to_string()),
        ];
        let out = generate_youtube_chapters(&entries);
        assert!(out.contains("Chapter 2"), "expected blank label → 'Chapter 2' in: {out}");
        // "Intro" is not blank, so "Chapter 1" should NOT appear.
        assert!(!out.contains("Chapter 1"), "unexpected 'Chapter 1' in: {out}");
    }

    #[test]
    fn generate_youtube_chapters_format() {
        let entries = vec![
            (0.0, "Intro".to_string()),
            (83.0, "Main Content".to_string()),
            (287.0, "Outro".to_string()),
        ];
        let expected = "0:00:00 Intro\n0:01:23 Main Content\n0:04:47 Outro";
        assert_eq!(generate_youtube_chapters(&entries), expected);
    }

}

/// Where an imported cue sits when its file doesn't say: centred, near
/// the bottom of the frame.
pub(super) const CUE_AT: klipp_timeline::TitlePosition = klipp_timeline::TitlePosition { x: 0.5, y: 0.88 };

fn font_style_of(bold: bool, italic: bool) -> klipp_timeline::FontStyle {
    use klipp_timeline::FontStyle as F;
    match (bold, italic) {
        (false, false) => F::Regular,
        (true, false) => F::Bold,
        (false, true) => F::Italic,
        (true, true) => F::BoldItalic,
    }
}

/// A cue's styled runs as its text box's style runs (#1515): its italic
/// and bold on top of the box's own style, its colour and its underline.
/// klipp-title draws the runs, so they burn in; an underline across the
/// whole cue underlines the box, which it draws too, and one over part of
/// it is kept on its run for export until runs draw their own (#1668).
pub(super) fn cue_style_runs(tb: &mut klipp_timeline::TextBox, spans: &[klipp_captions::Span]) {
    use klipp_timeline::FontStyle as F;
    let (len, base) = (tb.text.len(), tb.font_style);
    let (base_bold, base_italic) = (matches!(base, F::Bold | F::BoldItalic), matches!(base, F::Italic | F::BoldItalic));
    for s in spans {
        let st = s.style;
        klipp_timeline::apply_style_to_range(&mut tb.style_runs, len, s.start, s.end, |r| {
            let fs = font_style_of(base_bold || st.bold, base_italic || st.italic);
            if fs != base {
                r.font_style = Some(fs);
            }
            if let Some(c) = st.color {
                r.color = Some([c[0], c[1], c[2], 255]);
            }
            if st.underline {
                r.underline = Some(true);
            }
        });
    }
    if len > 0 && spans.iter().any(|s| s.style.underline && s.start == 0 && s.end >= len) {
        tb.underline = true;
    }
}

/// A cue's placement as its title's position and its text's alignment:
/// an ASS `\pos` is where it sits, its `\an` the side it is set to, and
/// an `\an` alone the band of the frame (top, middle, bottom).
pub(super) fn place_cue(td: &mut klipp_timeline::TitleData, p: klipp_captions::Placement) {
    use klipp_timeline::TextAlignment as A;
    let align = p.align.unwrap_or(2);
    if let Some(tb) = td.primary_text_mut() {
        tb.alignment = match align % 3 {
            1 => A::Left,
            0 => A::Right,
            _ => A::Center,
        };
    }
    let at = match (p.pos, p.align) {
        (Some((x, y)), _) => klipp_timeline::TitlePosition { x, y },
        (None, Some(a)) => klipp_timeline::TitlePosition { x: 0.5, y: match a { 7..=9 => 0.12, 4..=6 => 0.5, _ => CUE_AT.y } },
        (None, None) => CUE_AT,
    };
    if let Some(o) = td.objects.first_mut() {
        o.transform.center = at;
    }
}

/// A subtitle clip as a cue to write: its text, its style runs and the
/// box's own style where they set the text off from plain, and its
/// placement where it isn't the default's (#1515).
pub(super) fn cue_of(start_secs: f64, end_secs: f64, td: &klipp_timeline::TitleData) -> SubtitleEntry {
    use klipp_timeline::{FontStyle as F, TextAlignment as A};
    let Some(tb) = td.primary_text() else {
        return SubtitleEntry { start_secs, end_secs, text: td.text(), ..Default::default() };
    };
    let style_of = |fs: F, colour: Option<[u8; 4]>, underline: bool| klipp_captions::SpanStyle {
        bold: matches!(fs, F::Bold | F::BoldItalic),
        italic: matches!(fs, F::Italic | F::BoldItalic),
        underline,
        color: colour.filter(|c| *c != tb.color).map(|c| [c[0], c[1], c[2]]),
    };
    let len = tb.text.len();
    let mut spans: Vec<klipp_captions::Span> = Vec::new();
    let mut runs = tb.style_runs.clone();
    runs.sort_by_key(|r| r.start);
    let mut at = 0;
    let base = style_of(tb.font_style, None, tb.underline);
    let mut push = |start: usize, end: usize, style: klipp_captions::SpanStyle| {
        if start < end && !style.is_plain() {
            spans.push(klipp_captions::Span { start, end, style });
        }
    };
    for r in runs.iter().filter(|r| r.start < r.end && r.end <= len) {
        push(at.min(r.start), r.start, base);
        let style = style_of(r.font_style.unwrap_or(tb.font_style), r.color, r.underline.unwrap_or(tb.underline));
        push(r.start, r.end, style);
        at = r.end;
    }
    push(at, len, base);
    let center = td.objects.first().map(|o| o.transform.center).unwrap_or(CUE_AT);
    let side = match tb.alignment {
        A::Left => 1,
        A::Right => 3,
        _ => 2,
    };
    let placement = if (center.x - CUE_AT.x).abs() < 1e-4 && (center.y - CUE_AT.y).abs() < 1e-4 && side == 2 {
        klipp_captions::Placement::default()
    } else {
        let band = if center.y < 1.0 / 3.0 { 6 } else if center.y < 2.0 / 3.0 { 3 } else { 0 };
        klipp_captions::Placement { pos: Some((center.x, center.y)), align: Some(side + band) }
    };
    SubtitleEntry { start_secs, end_secs, text: tb.text.clone(), spans, placement }
}

#[cfg(test)]
mod cue_styling_tests {
    use super::*;

    fn title_of(sub: &SubtitleEntry) -> klipp_timeline::TitleData {
        let mut tb = klipp_timeline::TextBox { text: sub.text.clone(), font_size: 32.0, alignment: klipp_timeline::TextAlignment::Center, ..Default::default() };
        cue_style_runs(&mut tb, &sub.spans);
        let mut td = klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default());
        place_cue(&mut td, sub.placement);
        td
    }

    /// #1515: an SRT's italics, bold, underline and colour, and an ASS
    /// line's position and alignment, imported as the app imports them and
    /// written back, are what they were.
    #[test]
    fn a_cues_styling_and_placement_survive_import_and_export() {
        let srt = parse_srt_vtt("1\n00:00:01,000 --> 00:00:02,000\nA <i>quiet</i> <b>word</b>, <u>marked</u> <font color=\"#00ff00\">green</font>\n\n");
        let td = title_of(&srt[0]);
        assert_eq!(cue_of(1.0, 2.0, &td).spans, srt[0].spans, "the runs come back as they went in");
        let whole = parse_srt_vtt("1\n00:00:01,000 --> 00:00:02,000\n<u>all of it</u>\n\n");
        assert!(title_of(&whole[0]).primary_text().unwrap().underline, "an underline over the whole cue underlines the box");

        let (ass, _) = parse_ass("[Script Info]\nPlayResX: 1920\nPlayResY: 1080\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an8\\pos(480,216)}Up here\n");
        let sub = SubtitleEntry { start_secs: 1.0, end_secs: 2.0, text: ass[0].text.clone(), spans: ass[0].spans.clone(), placement: ass[0].placement };
        let td = title_of(&sub);
        assert_eq!(td.objects[0].transform.center, klipp_timeline::TitlePosition { x: 0.25, y: 0.2 });
        assert_eq!(cue_of(1.0, 2.0, &td).placement, klipp_captions::Placement { pos: Some((0.25, 0.2)), align: Some(8) });
        let plain = title_of(&parse_srt_vtt("1\n00:00:01,000 --> 00:00:02,000\nPlain\n\n")[0]);
        assert_eq!(cue_of(1.0, 2.0, &plain), SubtitleEntry { start_secs: 1.0, end_secs: 2.0, text: "Plain".into(), ..Default::default() }, "a plain cue writes as before");
    }

    /// #1516: a TTML cue's styling and region come in as the app imports
    /// them, and the IMSC written from the clips reads back as the same.
    #[test]
    fn a_ttml_cue_survives_import_and_imsc_export() {
        let ttml = r##"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling">
  <head><layout><region xml:id="top" tts:origin="10% 5%" tts:extent="80% 15%"/><region xml:id="side" tts:origin="5% 40%" tts:extent="30% 20%" tts:textAlign="left"/></layout></head>
  <body><div>
    <p begin="00:00:01.000" end="00:00:02.000">A <span tts:fontStyle="italic">quiet</span><br/>word</p>
    <p begin="00:00:02.000" end="00:00:03.000" region="top">Up here</p>
    <p begin="00:00:03.000" end="00:00:04.000" region="side"><span tts:color="#00ff00">Aside</span></p>
  </div></body>
</tt>"##;
        let subs = klipp_captions::ttml::parse_ttml(ttml).unwrap();
        let clips: Vec<SubtitleEntry> = subs.iter().map(|s| cue_of(s.start_secs, s.end_secs, &title_of(s))).collect();
        assert_eq!(clips[0].spans, subs[0].spans);
        assert_eq!(clips[0].text, "A quiet\nword");
        assert_eq!(title_of(&subs[1]).objects[0].transform.center.y, 0.12, "the top band sits where a top cue does");
        let back = klipp_captions::ttml::parse_ttml(&klipp_captions::ttml::generate_imsc(&clips, "")).unwrap();
        for (a, b) in subs.iter().zip(&back) {
            assert_eq!((a.start_secs, a.end_secs, &a.text, &a.spans), (b.start_secs, b.end_secs, &b.text, &b.spans));
            assert_eq!(a.placement.align, b.placement.align, "{}", a.text);
        }
        assert_eq!(back[2].placement.pos.map(|(x, y)| ((x * 100.0).round(), (y * 100.0).round())), Some((20.0, 50.0)));
    }

    /// #1516: an SCC caption's styling and row come in as the app imports
    /// them, and the SCC written from the clips reads back as the same.
    #[test]
    fn an_scc_caption_survives_import_and_export() {
        // Pop-on: row 15 white, "Hi " then italic "there"; then row 2 "Top".
        let scc = "Scenarist_SCC V1.0\n\n00:00:00;00\t9420 9420 94ae 94ae 9470 9470 c8e9 91ae 91ae f468 e5f2 e580 942f 942f\n\n00:00:02;00\t9420 9420 94ae 94ae 9152 9152 54ef 7080 942f 942f\n\n00:00:04;00\t942c 942c\n";
        let subs = klipp_captions::scc::parse_scc(scc).unwrap();
        assert_eq!(subs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Hi there", "Top"]);
        let clips: Vec<SubtitleEntry> = subs.iter().map(|s| cue_of(s.start_secs, s.end_secs, &title_of(s))).collect();
        assert_eq!(clips[0].spans, subs[0].spans);
        assert_eq!(title_of(&subs[1]).objects[0].transform.center.y, 0.12, "a caption on row 2 sits where a top cue does");
        let back = klipp_captions::scc::parse_scc(&klipp_captions::scc::generate_scc(&clips)).unwrap();
        assert_eq!(back.iter().map(|s| (&s.text, &s.spans, s.placement.align)).collect::<Vec<_>>(), subs.iter().map(|s| (&s.text, &s.spans, s.placement.align)).collect::<Vec<_>>());
        assert!((back[1].end_secs - subs[1].end_secs).abs() < 1e-9);
    }

    /// #1516: an EBU-STL subtitle's styling and position come in as the
    /// app imports them, and the STL written from the clips reads back as
    /// the same.
    #[test]
    fn an_stl_subtitle_survives_import_and_export() {
        let mut top = SubtitleEntry { start_secs: 2.0, end_secs: 3.0, text: "Up here".into(), ..Default::default() };
        top.placement = klipp_captions::Placement { pos: None, align: Some(8) };
        let mut low = SubtitleEntry { start_secs: 1.0, end_secs: 2.0, text: "A quiet word".into(), ..Default::default() };
        low.spans = vec![klipp_captions::Span { start: 2, end: 7, style: klipp_captions::SpanStyle { italic: true, ..Default::default() } }];
        let subs = klipp_captions::stl::parse_stl(&klipp_captions::stl::generate_stl(&[low, top], 25, "en", "Talk")).unwrap();
        let clips: Vec<SubtitleEntry> = subs.iter().map(|s| cue_of(s.start_secs, s.end_secs, &title_of(s))).collect();
        assert_eq!(clips[0].spans, subs[0].spans);
        assert_eq!(title_of(&subs[1]).objects[0].transform.center.y, 0.12, "a top subtitle sits where a top cue does");
        let back = klipp_captions::stl::parse_stl(&klipp_captions::stl::generate_stl(&clips, 25, "en", "Talk")).unwrap();
        assert_eq!(back, subs);
    }

    /// The styling burns in, through the title renderer the subtitle track
    /// is drawn by: an italic run draws other pixels than the same run
    /// upright, and a cue placed by `\pos` draws its text there.
    #[test]
    fn a_cues_styling_and_placement_burn_in() {
        let (w, h) = (640u32, 360u32);
        let mut r = klipp_title::TitleRenderer::new();
        let upright = r.render(&title_of(&parse_srt_vtt("1\n00:00:01,000 --> 00:00:02,000\nleaning text\n\n")[0]), w, h);
        let italic = r.render(&title_of(&parse_srt_vtt("1\n00:00:01,000 --> 00:00:02,000\n<i>leaning text</i>\n\n")[0]), w, h);
        assert_ne!(upright, italic, "the italic run draws");
        let placed = SubtitleEntry { text: "Here".into(), placement: klipp_captions::Placement { pos: Some((0.25, 0.2)), align: Some(5) }, ..Default::default() };
        let px = r.render(&title_of(&placed), w, h);
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if px[((y * w + x) * 4 + 3) as usize] > 0 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
        }
        assert!(x1 > x0, "the cue drew");
        let (cx, cy) = ((x0 + x1) as f32 / 2.0 / w as f32, (y0 + y1) as f32 / 2.0 / h as f32);
        assert!((cx - 0.25).abs() < 0.05 && (cy - 0.2).abs() < 0.05, "drawn about (0.25, 0.2), at ({cx:.3}, {cy:.3})");
    }
}
