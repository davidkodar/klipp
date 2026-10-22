//! Subtitles and captions as text files: SRT, WebVTT and ASS/SSA read
//! and written (#890), moved out of the app so the control socket can
//! read and write them too (#1516).
#![forbid(unsafe_code)]

pub mod scc;
pub mod stl;
pub mod styled;
pub mod ttml;
pub use styled::{Placement, Span, SpanStyle};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubtitleEntry {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
    /// The text's styled runs (#1515); empty for plain text.
    pub spans: Vec<Span>,
    /// Where the cue sits, when its file says.
    pub placement: Placement,
}

/// A single Dialogue line from an ASS/SSA file.
#[derive(Debug, Clone)]
pub struct AssSubtitleEntry {
    pub start_secs: f64,
    pub end_secs: f64,
    /// Displayable text — ASS override tags stripped, `\N`/`\n`/`\h` normalized.
    pub text: String,
    /// Name of the V4+ style this dialogue references (empty if none).
    pub style_name: String,
    /// The text's styled runs from its override tags (#1515).
    pub spans: Vec<Span>,
    /// Its `\pos` and `\an`, when it has them.
    pub placement: Placement,
}

/// A [V4+ Styles] entry — just the fields we actually wire into TitleData.
#[derive(Debug, Clone, Default)]
pub struct AssStyle {
    #[allow(dead_code)]
    pub name: String,
    pub font_name: String,
    pub font_size: f32,
    /// RGBA. Converted from ASS `&HAABBGGRR` (alpha is inverted in ASS: 00 = opaque).
    pub primary_color: [u8; 4],
    pub outline: f32,
    pub shadow: f32,
    pub bold: bool,
    pub italic: bool,
}

/// Parse ASS/SSA. Reads `[V4+ Styles]` (both V4+ and V4 headers) and `[Events]`.
/// Returns `(entries, styles_by_name)`. The import handler maps
/// `entry.style_name` → style for TitleData population.
pub fn parse_ass(content: &str) -> (Vec<AssSubtitleEntry>, std::collections::HashMap<String, AssStyle>) {
    use std::collections::HashMap;
    let mut entries: Vec<AssSubtitleEntry> = Vec::new();
    let mut styles: HashMap<String, AssStyle> = HashMap::new();

    #[derive(PartialEq)]
    enum Section { None, Styles, Events, Other }
    let mut section = Section::None;
    let mut styles_fmt: Vec<String> = Vec::new();
    let mut events_fmt: Vec<String> = Vec::new();
    // The frame `\pos` is given over: the script's, else ASS's default.
    let mut play_res = (384.0f32, 288.0f32);

    for raw in content.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('!') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let header = line.trim_start_matches('[').trim_end_matches(']').trim();
            section = if header.eq_ignore_ascii_case("V4+ Styles")
                || header.eq_ignore_ascii_case("V4 Styles")
            {
                Section::Styles
            } else if header.eq_ignore_ascii_case("Events") {
                Section::Events
            } else {
                Section::Other
            };
            continue;
        }
        // Each line in V4+/Events is `Key: value`
        let Some(colon) = line.find(':') else { continue; };
        let key = line[..colon].trim();
        let value = line[colon + 1..].trim();
        if section != Section::Styles && section != Section::Events {
            if key.eq_ignore_ascii_case("PlayResX") {
                play_res.0 = value.parse().unwrap_or(play_res.0);
            } else if key.eq_ignore_ascii_case("PlayResY") {
                play_res.1 = value.parse().unwrap_or(play_res.1);
            }
        }
        match section {
            Section::Styles => {
                if key.eq_ignore_ascii_case("Format") {
                    styles_fmt = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect();
                } else if key.eq_ignore_ascii_case("Style") {
                    if styles_fmt.is_empty() { continue; }
                    // Styles have no free-text field, so a plain split is safe.
                    let fields: Vec<&str> = value.split(',').map(str::trim).collect();
                    if fields.len() < styles_fmt.len() { continue; }
                    let mut style = AssStyle::default();
                    for (i, fname) in styles_fmt.iter().enumerate() {
                        let v = fields[i];
                        match fname.as_str() {
                            "Name" => style.name = v.to_string(),
                            "Fontname" => style.font_name = v.to_string(),
                            "Fontsize" => {
                                if let Ok(n) = v.parse::<f32>() { style.font_size = n; }
                            }
                            "PrimaryColour" => style.primary_color = parse_ass_color(v),
                            "Outline" => {
                                if let Ok(n) = v.parse::<f32>() { style.outline = n; }
                            }
                            "Shadow" => {
                                if let Ok(n) = v.parse::<f32>() { style.shadow = n; }
                            }
                            "Bold" => style.bold = v != "0" && !v.is_empty(),
                            "Italic" => style.italic = v != "0" && !v.is_empty(),
                            _ => {}
                        }
                    }
                    if !style.name.is_empty() {
                        styles.insert(style.name.clone(), style);
                    }
                }
            }
            Section::Events => {
                if key.eq_ignore_ascii_case("Format") {
                    events_fmt = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect();
                } else if key.eq_ignore_ascii_case("Dialogue") {
                    if events_fmt.is_empty() { continue; }
                    // The last Format column is "Text" and may contain commas.
                    // Split into at most events_fmt.len() parts with splitn.
                    let fields: Vec<&str> = value
                        .splitn(events_fmt.len(), ',')
                        .collect();
                    if fields.len() < events_fmt.len() { continue; }
                    let mut start_s = 0.0;
                    let mut end_s = 0.0;
                    let mut style_name = String::new();
                    let mut text_raw = String::new();
                    for (i, fname) in events_fmt.iter().enumerate() {
                        let v = fields[i];
                        match fname.as_str() {
                            "Start" => start_s = parse_ass_time(v.trim()),
                            "End" => end_s = parse_ass_time(v.trim()),
                            "Style" => style_name = v.trim().to_string(),
                            "Text" => text_raw = v.to_string(), // keep whitespace
                            _ => {}
                        }
                    }
                    let (text, spans, placement) = styled::parse_ass_text(&text_raw, play_res);
                    if !text.is_empty() && end_s > start_s {
                        entries.push(AssSubtitleEntry {
                            start_secs: start_s,
                            end_secs: end_s,
                            text,
                            style_name,
                            spans,
                            placement,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    (entries, styles)
}

/// ASS time: `H:MM:SS.cc` where cc is centiseconds (NOT milliseconds).
pub fn parse_ass_time(s: &str) -> f64 {
    let s = s.trim();
    // Split into H, MM, SS.cc
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 { return 0.0; }
    let h: f64 = parts[0].parse().unwrap_or(0.0);
    let m: f64 = parts[1].parse().unwrap_or(0.0);
    let sec_cc = parts[2];
    // sec_cc like "23.45" — parse as seconds (dot separator is centiseconds or
    // ms; either way f64 parsing gets us the right numeric value).
    let sec: f64 = sec_cc.parse().unwrap_or(0.0);
    h * 3600.0 + m * 60.0 + sec
}

/// Parse an ASS colour literal into an RGBA [u8; 4].
/// ASS v4+ stores colours as `&HAABBGGRR&` or `&HBBGGRR&` (BGR order, alpha prefixed).
/// Alpha is INVERTED: `&H00` = fully opaque, `&HFF` = fully transparent.
/// Returns opaque white on parse failure.
pub fn parse_ass_color(s: &str) -> [u8; 4] {
    let s = s.trim();
    // Strip `&H` prefix and trailing `&`
    let hex = s
        .trim_start_matches("&H")
        .trim_start_matches("&h")
        .trim_end_matches('&');
    if hex.is_empty() {
        return [255, 255, 255, 255];
    }
    // Pad/truncate to handle 6- or 8-hex forms. ASS often omits leading zeros.
    let hex_upper = hex.to_ascii_uppercase();
    // Parse as u32 — gives us AABBGGRR (or 00BBGGRR if alpha omitted).
    let n = u32::from_str_radix(&hex_upper, 16).unwrap_or(0xFF_FF_FF_FF);
    let a_inv = ((n >> 24) & 0xFF) as u8;
    let b = ((n >> 16) & 0xFF) as u8;
    let g = ((n >> 8) & 0xFF) as u8;
    let r = (n & 0xFF) as u8;
    // If the hex was 6 chars (no alpha), a_inv = 0 = opaque, which is what we want.
    let a = 255u8.saturating_sub(a_inv);
    [r, g, b, a]
}

/// Strip ASS override blocks (`{...}`) from dialogue text, decode standard escapes.
/// - `\N` → hard line break
/// - `\n` → soft wrap (we treat as space since our renderer auto-wraps)
/// - `\h` → non-breaking space
pub fn strip_ass_override_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth: i32 = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            depth += 1;
            continue;
        }
        if c == '}' {
            if depth > 0 { depth -= 1; }
            continue;
        }
        if depth > 0 { continue; }
        if c == '\\' {
            match chars.peek() {
                Some('N') => { chars.next(); out.push('\n'); }
                Some('n') => { chars.next(); out.push(' '); }
                Some('h') => { chars.next(); out.push(' '); }
                _ => out.push(c),
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The language a WebVTT file's header names in a `Language:` line, as
/// a lowercase tag; `None` for SRT, or a header without one (#2223).
pub fn vtt_language(content: &str) -> Option<String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if !content.starts_with("WEBVTT") {
        return None;
    }
    content.lines().skip(1).take_while(|l| !l.trim().is_empty() && !l.contains("-->")).find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.trim().eq_ignore_ascii_case("language").then(|| value.trim().to_ascii_lowercase()).filter(|v| !v.is_empty())
    })
}

/// The language the subtitle file at `path`, read as `bytes`, names,
/// by its extension's format: TTML's `xml:lang`, EBU-STL's language code,
/// a WebVTT `Language:` header; `None` for a format with none (#2223).
pub fn file_language(path: &std::path::Path, bytes: &[u8]) -> Option<String> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    match ext.as_str() {
        "stl" => stl::stl_language(bytes),
        "ttml" | "dfxp" | "xml" | "itt" => ttml::ttml_language(std::str::from_utf8(bytes).ok()?),
        "vtt" => vtt_language(std::str::from_utf8(bytes).ok()?),
        _ => None,
    }
}

/// Parse SRT or VTT subtitle content into timed entries.
pub fn parse_srt_vtt(content: &str) -> Vec<SubtitleEntry> {
    let mut subs = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    let is_vtt = content.starts_with("WEBVTT");
    if is_vtt { i = 1; } // skip header

    while i < lines.len() {
        // Skip blank lines and sequence numbers
        let line = lines[i].trim();
        if line.is_empty() || line.parse::<u32>().is_ok() {
            i += 1;
            continue;
        }
        // Look for timestamp line: "00:00:01,000 --> 00:00:04,000" or "00:00:01.000 --> 00:00:04.000"
        if line.contains("-->") {
            let parts: Vec<&str> = line.split("-->").collect();
            if parts.len() == 2 {
                let start = parse_srt_timestamp(parts[0].trim());
                let end = parse_srt_timestamp(parts[1].trim());
                if let (Some(s), Some(e)) = (start, end) {
                    // Collect text lines until blank line
                    let mut text_lines = Vec::new();
                    i += 1;
                    while i < lines.len() && !lines[i].trim().is_empty() {
                        text_lines.push(lines[i].trim());
                        i += 1;
                    }
                    let text = text_lines.join("\n");
                    // Its tags as styled runs (#1515).
                    let (text, spans) = styled::parse_markup(&text);
                    if !text.is_empty() {
                        subs.push(SubtitleEntry { start_secs: s, end_secs: e, text, spans, placement: Placement::default() });
                    }
                    continue;
                }
            }
        }
        i += 1;
    }
    subs
}

/// Parse "HH:MM:SS,mmm" or "HH:MM:SS.mmm" or "MM:SS.mmm" to seconds.
pub fn parse_srt_timestamp(s: &str) -> Option<f64> {
    let s = s.replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        3 => {
            let h: f64 = parts[0].parse().ok()?;
            let m: f64 = parts[1].parse().ok()?;
            let sec: f64 = parts[2].parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + sec)
        }
        2 => {
            let m: f64 = parts[0].parse().ok()?;
            let sec: f64 = parts[1].parse().ok()?;
            Some(m * 60.0 + sec)
        }
        _ => None,
    }
}

/// Generate SRT format string from subtitle entries.
pub fn generate_srt(subs: &[SubtitleEntry]) -> String {
    let mut out = String::new();
    for (i, sub) in subs.iter().enumerate() {
        out.push_str(&format!("{}\n", i + 1));
        out.push_str(&format!("{} --> {}\n", format_srt_time(sub.start_secs), format_srt_time(sub.end_secs)));
        out.push_str(&styled::srt_markup(&sub.text, &sub.spans));
        out.push_str("\n\n");
    }
    out
}

pub fn format_srt_time(secs: f64) -> String {
    let h = (secs / 3600.0) as u32;
    let m = ((secs % 3600.0) / 60.0) as u32;
    let s = (secs % 60.0) as u32;
    let ms = ((secs % 1.0) * 1000.0) as u32;
    format!("{:02}:{:02}:{:02},{:03}", h, m, s, ms)
}

/// Generate WebVTT format string from subtitle entries.
pub fn generate_vtt(subs: &[SubtitleEntry]) -> String {
    generate_vtt_in(subs, None)
}

/// [`generate_vtt`], its header naming `language` (a BCP 47 tag) in a
/// `Language:` line when it is known (#2223).
pub fn generate_vtt_in(subs: &[SubtitleEntry], language: Option<&str>) -> String {
    let mut out = String::from("WEBVTT\n");
    if let Some(lang) = language.map(str::trim).filter(|l| !l.is_empty()) {
        out.push_str(&format!("Language: {lang}\n"));
    }
    out.push('\n');
    out.push_str(&styled::vtt_colour_classes(subs.iter().flat_map(|s| s.spans.iter().filter_map(|sp| sp.style.color))));
    for sub in subs {
        out.push_str(&format!("{} --> {}\n", format_vtt_time(sub.start_secs), format_vtt_time(sub.end_secs)));
        out.push_str(&styled::vtt_markup(&sub.text, &sub.spans));
        out.push_str("\n\n");
    }
    out
}

pub fn format_vtt_time(secs: f64) -> String {
    let h = (secs / 3600.0) as u32;
    let m = ((secs % 3600.0) / 60.0) as u32;
    let s = (secs % 60.0) as u32;
    let ms = ((secs % 1.0) * 1000.0) as u32;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}

/// Generate ASS v4+ subtitle file from entries. Used for both .ass and
/// .ssa extensions — modern players accept v4+ content under either name.
pub fn generate_ass(subs: &[SubtitleEntry]) -> String {
    let mut out = String::new();

    // [Script Info]
    out.push_str("[Script Info]\n");
    out.push_str("Title: Klipp Subtitles\n");
    out.push_str("ScriptType: v4.00+\n");
    out.push_str("WrapStyle: 0\n");
    out.push_str("ScaledBorderAndShadow: yes\n");
    out.push_str("PlayResX: 1920\n");
    out.push_str("PlayResY: 1080\n");
    out.push('\n');

    // [V4+ Styles]
    out.push_str("[V4+ Styles]\n");
    out.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, \
SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, \
StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, \
Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    out.push_str("Style: Default,Arial,48,&H00FFFFFF,&H000000FF,\
&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1\n");
    out.push('\n');

    // [Events]
    out.push_str("[Events]\n");
    out.push_str("Format: Layer, Start, End, Style, Name, \
MarginL, MarginR, MarginV, Effect, Text\n");

    for sub in subs {
        // \n → \N, the text's own braces → parentheses, its styling and
        // placement as override blocks (#1515).
        let text = styled::ass_markup(&sub.text, &sub.spans, sub.placement, (1920.0, 1080.0));
        out.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
            format_ass_time(sub.start_secs),
            format_ass_time(sub.end_secs),
            text
        ));
    }

    out
}

pub fn format_ass_time(secs: f64) -> String {
    // ASS timestamp format: H:MM:SS.cc (centiseconds, single-digit hour)
    let total_cs = (secs * 100.0).round() as u64;
    let cs = total_cs % 100;
    let total_s = total_cs / 100;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{}:{:02}:{:02}.{:02}", h, m, s, cs)
}

#[cfg(test)]
mod subtitle_tests {
    use super::*;

    // ── parse_srt_vtt tests ──

    #[test]
    fn parse_standard_srt() {
        let input = "\
1
00:00:01,000 --> 00:00:04,000
Hello world

2
00:00:05,000 --> 00:00:08,000
Second subtitle
";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 2);
        assert!((subs[0].start_secs - 1.0).abs() < 0.001);
        assert!((subs[0].end_secs - 4.0).abs() < 0.001);
        assert_eq!(subs[0].text, "Hello world");
        assert!((subs[1].start_secs - 5.0).abs() < 0.001);
        assert!((subs[1].end_secs - 8.0).abs() < 0.001);
        assert_eq!(subs[1].text, "Second subtitle");
    }

    #[test]
    fn parse_multiline_subtitle() {
        let input = "\
1
00:00:01,000 --> 00:00:04,000
Line one
Line two
Line three

";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].text, "Line one\nLine two\nLine three");
    }

    #[test]
    fn parse_html_tags_stripped() {
        let input = "\
1
00:00:01,000 --> 00:00:04,000
<i>Italic</i> and <b>bold</b> and <u>underline</u>

";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].text, "Italic and bold and underline");
    }

    #[test]
    fn parse_vtt_format() {
        let input = "\
WEBVTT

00:00:01.000 --> 00:00:04.000
Hello from VTT

00:00:05.000 --> 00:00:08.000
Second VTT cue
";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 2);
        assert!((subs[0].start_secs - 1.0).abs() < 0.001);
        assert_eq!(subs[0].text, "Hello from VTT");
        assert_eq!(subs[1].text, "Second VTT cue");
    }

    #[test]
    fn parse_empty_file() {
        let subs = parse_srt_vtt("");
        assert!(subs.is_empty());
    }

    #[test]
    fn parse_malformed_timestamps_skipped() {
        let input = "\
1
not:a:timestamp --> also:bad
This should be skipped

2
00:00:05,000 --> 00:00:08,000
This is valid
";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].text, "This is valid");
    }

    // ── parse_srt_timestamp tests ──

    #[test]
    fn parse_srt_timestamp_hhmmss_comma() {
        let result = parse_srt_timestamp("00:01:02,500").unwrap();
        assert!((result - 62.5).abs() < 0.001);
    }

    #[test]
    fn parse_vtt_timestamp_hhmmss_dot() {
        let result = parse_srt_timestamp("00:01:02.500").unwrap();
        assert!((result - 62.5).abs() < 0.001);
    }

    #[test]
    fn parse_short_timestamp_mmss() {
        let result = parse_srt_timestamp("01:02.500").unwrap();
        assert!((result - 62.5).abs() < 0.001);
    }

    #[test]
    fn parse_timestamp_invalid_returns_none() {
        assert!(parse_srt_timestamp("garbage").is_none());
        assert!(parse_srt_timestamp("").is_none());
        assert!(parse_srt_timestamp("00:xx:02,500").is_none());
    }

    // ── generate_srt tests ──

    #[test]
    fn generate_srt_single_entry() {
        let subs = vec![SubtitleEntry {
            start_secs: 1.0,
            end_secs: 4.0,
            text: "Hello".to_string(), ..Default::default()
        }];
        let output = generate_srt(&subs);
        assert!(output.starts_with("1\n"));
        assert!(output.contains("00:00:01,000 --> 00:00:04,000"));
        assert!(output.contains("Hello"));
    }

    #[test]
    fn generate_srt_multiple_entries_sequential_numbering() {
        let subs = vec![
            SubtitleEntry { start_secs: 1.0, end_secs: 4.0, text: "First".to_string(), ..Default::default() },
            SubtitleEntry { start_secs: 5.0, end_secs: 8.0, text: "Second".to_string(), ..Default::default() },
            SubtitleEntry { start_secs: 10.0, end_secs: 12.0, text: "Third".to_string(), ..Default::default() },
        ];
        let output = generate_srt(&subs);
        assert!(output.contains("1\n00:00:01,000 --> 00:00:04,000\nFirst"));
        assert!(output.contains("2\n00:00:05,000 --> 00:00:08,000\nSecond"));
        assert!(output.contains("3\n00:00:10,000 --> 00:00:12,000\nThird"));
    }

    // ── format_srt_time tests ──

    #[test]
    fn format_srt_time_zero() {
        assert_eq!(format_srt_time(0.0), "00:00:00,000");
    }

    #[test]
    fn format_srt_time_62_5() {
        assert_eq!(format_srt_time(62.5), "00:01:02,500");
    }

    #[test]
    fn format_srt_time_3661() {
        assert_eq!(format_srt_time(3661.0), "01:01:01,000");
    }

    // ── format_ass_time tests ──

    #[test]
    fn format_ass_time_zero() {
        assert_eq!(format_ass_time(0.0), "0:00:00.00");
    }

    #[test]
    fn format_ass_time_62_5() {
        assert_eq!(format_ass_time(62.5), "0:01:02.50");
    }

    #[test]
    fn format_ass_time_3661_99() {
        assert_eq!(format_ass_time(3661.99), "1:01:01.99");
    }

    // ── generate_ass tests ──

    #[test]
    fn generate_ass_single_entry() {
        let subs = vec![SubtitleEntry {
            start_secs: 1.0,
            end_secs: 4.0,
            text: "Hello".to_string(), ..Default::default()
        }];
        let output = generate_ass(&subs);
        assert!(output.contains("[Script Info]"));
        assert!(output.contains("ScriptType: v4.00+"));
        assert!(output.contains("[V4+ Styles]"));
        assert!(output.contains("[Events]"));
        assert!(output.contains(
            "Dialogue: 0,0:00:01.00,0:00:04.00,Default,,0,0,0,,Hello"
        ));
    }

    #[test]
    fn generate_ass_newline_escaped() {
        let subs = vec![SubtitleEntry {
            start_secs: 0.0,
            end_secs: 2.0,
            text: "Line one\nLine two".to_string(), ..Default::default()
        }];
        let output = generate_ass(&subs);
        assert!(output.contains("Line one\\NLine two"));
    }

    #[test]
    fn generate_ass_braces_escaped() {
        let subs = vec![SubtitleEntry {
            start_secs: 0.0,
            end_secs: 2.0,
            text: "{bold} text".to_string(), ..Default::default()
        }];
        let output = generate_ass(&subs);
        assert!(output.contains("(bold) text"));
    }

    // ── Roundtrip test ──

    #[test]
    fn roundtrip_generate_then_parse() {
        let original = vec![
            SubtitleEntry { start_secs: 1.5, end_secs: 3.0, text: "First cue".to_string(), ..Default::default() },
            SubtitleEntry { start_secs: 5.0, end_secs: 7.25, text: "Second cue".to_string(), ..Default::default() },
        ];
        let srt = generate_srt(&original);
        let parsed = parse_srt_vtt(&srt);
        assert_eq!(parsed.len(), 2);
        assert!((parsed[0].start_secs - 1.5).abs() < 0.001);
        assert!((parsed[0].end_secs - 3.0).abs() < 0.001);
        assert_eq!(parsed[0].text, "First cue");
        assert!((parsed[1].start_secs - 5.0).abs() < 0.001);
        assert!((parsed[1].end_secs - 7.25).abs() < 0.001);
        assert_eq!(parsed[1].text, "Second cue");
    }

    // ── ASS/SSA parser tests (Tier 1) ──

    const ASS_FIXTURE: &str = "\
[Script Info]
Title: Example
ScriptType: v4.00+
PlayResX: 1920
PlayResY: 1080

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,20,1
Style: Bold1,Helvetica,32,&H000000FF,&H000000FF,&H00000000,&H64000000,-1,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:04.00,Default,,0,0,0,,Hello world
Dialogue: 0,0:00:05.00,0:00:07.50,Bold1,,0,0,0,,{\\b1}Bold{\\b0} line with \\Nhard break
Dialogue: 0,0:00:09.00,0:00:11.00,Default,,0,0,0,,Text, with commas, inside
";

    #[test]
    fn parse_ass_basic_entry_count() {
        let (entries, _) = parse_ass(ASS_FIXTURE);
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn parse_ass_timing() {
        let (entries, _) = parse_ass(ASS_FIXTURE);
        assert!((entries[0].start_secs - 1.0).abs() < 0.001);
        assert!((entries[0].end_secs - 4.0).abs() < 0.001);
        assert!((entries[1].start_secs - 5.0).abs() < 0.001);
        assert!((entries[1].end_secs - 7.5).abs() < 0.001);
    }

    #[test]
    fn parse_ass_style_name_preserved() {
        let (entries, _) = parse_ass(ASS_FIXTURE);
        assert_eq!(entries[0].style_name, "Default");
        assert_eq!(entries[1].style_name, "Bold1");
    }

    #[test]
    fn parse_ass_strips_override_tags_and_preserves_text() {
        let (entries, _) = parse_ass(ASS_FIXTURE);
        // Override blocks removed; \N becomes newline.
        assert_eq!(entries[1].text, "Bold line with \nhard break");
    }

    #[test]
    fn parse_ass_text_with_commas() {
        // The Text column is last; commas inside must survive splitn.
        let (entries, _) = parse_ass(ASS_FIXTURE);
        assert_eq!(entries[2].text, "Text, with commas, inside");
    }

    #[test]
    fn parse_ass_styles_collected() {
        let (_, styles) = parse_ass(ASS_FIXTURE);
        assert!(styles.contains_key("Default"));
        assert!(styles.contains_key("Bold1"));
        let b = &styles["Bold1"];
        assert_eq!(b.font_name, "Helvetica");
        assert!((b.font_size - 32.0).abs() < 0.001);
        // Bold1 has Bold=-1 (SSA style for "true")
        assert!(b.bold);
        assert!(!b.italic);
    }

    #[test]
    fn strip_ass_override_tags_removes_braces() {
        // Plain override tag stripping
        assert_eq!(strip_ass_override_tags("{\\b1}hi{\\b0}"), "hi");
        // Nested-looking braces
        assert_eq!(strip_ass_override_tags("a{\\i1}b{\\i0}c"), "abc");
        // \N becomes newline
        assert_eq!(strip_ass_override_tags("line1\\Nline2"), "line1\nline2");
        // \h becomes space
        assert_eq!(strip_ass_override_tags("a\\hb"), "a b");
    }

    #[test]
    fn parse_ass_color_bgr_to_rgba() {
        // &H00FFFFFF = AABBGGRR = 00 FF FF FF → RGB=white, alpha=255 (00 inverted)
        assert_eq!(parse_ass_color("&H00FFFFFF"), [255, 255, 255, 255]);
        // &H000000FF = AABBGGRR = 00 00 00 FF → R=FF, G=00, B=00, A=255 (red)
        assert_eq!(parse_ass_color("&H000000FF"), [255, 0, 0, 255]);
        // &H00FF0000 = AABBGGRR = 00 FF 00 00 → R=0, G=0, B=FF, A=255 (blue)
        assert_eq!(parse_ass_color("&H00FF0000"), [0, 0, 255, 255]);
        // 6-hex form (no alpha) — alpha defaults to opaque
        assert_eq!(parse_ass_color("&HFFFFFF"), [255, 255, 255, 255]);
        // Alpha inverted: &H7F = semi-transparent (255 - 127 = 128)
        let c = parse_ass_color("&H7FFFFFFF");
        assert_eq!(c[3], 128);
    }

    #[test]
    fn parse_ass_time_centiseconds() {
        // H:MM:SS.cc — cc is centiseconds, so 0:01:23.45 = 83.45s
        assert!((parse_ass_time("0:01:23.45") - 83.45).abs() < 0.001);
        assert!((parse_ass_time("1:00:00.00") - 3600.0).abs() < 0.001);
        assert!((parse_ass_time("0:00:00.00") - 0.0).abs() < 0.001);
    }

    // ── Title animation helpers ──

    #[test]
    fn typewriter_write_on_calculation() {
        let chars = 10usize;
        let chars_per_sec = 5.0f64;
        let elapsed = 1.0f64;
        let write_on = (elapsed * chars_per_sec / chars as f64).clamp(0.0, 1.0) as f32;
        assert!((write_on - 0.5).abs() < 0.001);
    }

    #[test]
    fn typewriter_write_on_clamps_at_one() {
        let chars = 10usize;
        let chars_per_sec = 5.0f64;
        let elapsed = 10.0f64;
        let write_on = (elapsed * chars_per_sec / chars as f64).clamp(0.0, 1.0) as f32;
        assert_eq!(write_on, 1.0);
    }

}

#[cfg(test)]
mod same_bytes_tests {
    use super::*;

    /// #1516: moved out of the app, the writers write exactly what they
    /// wrote there, byte for byte, quirks included (a time like 3725.04 s
    /// floored to its millisecond below), and the readers read it back as
    /// they did.
    #[test]
    fn the_formats_are_the_bytes_the_app_wrote() {
        let subs = vec![
            SubtitleEntry { start_secs: 0.0, end_secs: 1.5, text: "Hello".into(), ..Default::default() },
            SubtitleEntry { start_secs: 61.25, end_secs: 63.999, text: "Two\nlines {with braces}".into(), ..Default::default() },
            SubtitleEntry { start_secs: 3725.04, end_secs: 3727.5, text: "Late, <i>tagged</i>".into(), ..Default::default() },
        ];
        let srt = generate_srt(&subs);
        assert_eq!(srt, "1\n00:00:00,000 --> 00:00:01,500\nHello\n\n2\n00:01:01,250 --> 00:01:03,999\nTwo\nlines {with braces}\n\n3\n01:02:05,039 --> 01:02:07,500\nLate, <i>tagged</i>\n\n");
        let vtt = generate_vtt(&subs);
        assert_eq!(vtt, "WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nHello\n\n00:01:01.250 --> 00:01:03.999\nTwo\nlines {with braces}\n\n01:02:05.039 --> 01:02:07.500\nLate, <i>tagged</i>\n\n");
        let ass = generate_ass(&subs);
        assert_eq!(ass, "[Script Info]\nTitle: Klipp Subtitles\nScriptType: v4.00+\nWrapStyle: 0\nScaledBorderAndShadow: yes\nPlayResX: 1920\nPlayResY: 1080\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.50,Default,,0,0,0,,Hello\nDialogue: 0,0:01:01.25,0:01:04.00,Default,,0,0,0,,Two\\Nlines (with braces)\nDialogue: 0,1:02:05.04,1:02:07.50,Default,,0,0,0,,Late, <i>tagged</i>\n");

        let texts = |e: Vec<SubtitleEntry>| e.into_iter().map(|e| e.text).collect::<Vec<_>>();
        assert_eq!(texts(parse_srt_vtt(&srt)), ["Hello", "Two\nlines {with braces}", "Late, tagged"]);
        assert_eq!(texts(parse_srt_vtt(&vtt)), ["Hello", "Two\nlines {with braces}", "Late, tagged"]);
        let (events, styles) = parse_ass(&ass);
        assert_eq!(events.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(), ["Hello", "Two\nlines (with braces)", "Late, <i>tagged</i>"]);
        assert!(styles.contains_key("Default"));
    }
}

#[cfg(test)]
mod inline_styling_tests {
    use super::*;

    fn styled(spans: &[Span], i: usize) -> SpanStyle {
        spans.iter().find(|s| s.start <= i && i < s.end).map(|s| s.style).unwrap_or_default()
    }

    /// #1515: an SRT cue's italics, bold, underline and colour come in as
    /// styled runs of its text and go back out as the same tags.
    #[test]
    fn an_srt_keeps_its_inline_styling_both_ways() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\nShe said <i>no</i>, <b>twice</b>.\n<u>Under</u> <font color=\"#ffcc00\">gold</font>\n\n";
        let subs = parse_srt_vtt(srt);
        let s = &subs[0];
        assert_eq!(s.text, "She said no, twice.\nUnder gold");
        let at = |w: &str| styled(&s.spans, s.text.find(w).unwrap());
        assert!(at("no").italic && !at("no").bold);
        assert!(at("twice").bold);
        assert!(at("Under").underline);
        assert_eq!(at("gold").color, Some([0xff, 0xcc, 0x00]));
        assert!(at("She").is_plain());
        let back = parse_srt_vtt(&generate_srt(&subs));
        assert_eq!(back, subs, "written and read again: the same cue");
    }

    /// WebVTT's colour classes and voice tags: a colour class is a styled
    /// run, written back with a STYLE block naming it; the voice tag goes.
    #[test]
    fn a_vtt_keeps_its_colour_and_drops_its_voice_tag() {
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<v Ana>Look, <c.yellow>there</c>!</v>\n\n";
        let subs = parse_srt_vtt(vtt);
        assert_eq!(subs[0].text, "Look, there!");
        assert_eq!(styled(&subs[0].spans, 6).color, Some([255, 255, 0]));
        let out = generate_vtt(&subs);
        assert!(out.contains("::cue(.colorffff00)"), "{out}");
        assert_eq!(parse_srt_vtt(&out), subs);
    }

    /// An ASS line's `\pos` and `\an` are its placement, as a fraction of
    /// the script's frame, and its `\i` and `\c` styled runs; written back
    /// over the export's 1920x1080 frame, they read the same.
    #[test]
    fn an_ass_line_keeps_its_position_and_styling() {
        let ass = "[Script Info]\nPlayResX: 1280\nPlayResY: 720\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an8\\pos(640,72)}Top {\\i1}lean{\\i0} {\\c&H0000FF&}red{\\c}\\Nnext {\\clip(0,0,10,10)}clip\n";
        let (events, _) = parse_ass(ass);
        let e = &events[0];
        assert_eq!(e.text, "Top lean red\nnext clip");
        assert_eq!(e.placement, Placement { pos: Some((0.5, 0.1)), align: Some(8) });
        assert!(styled(&e.spans, 4).italic, "lean");
        assert_eq!(styled(&e.spans, 9).color, Some([255, 0, 0]), "red, from BGR");
        assert!(styled(&e.spans, e.text.find("clip").unwrap()).is_plain(), "\\clip is no colour");
        let sub = SubtitleEntry { start_secs: e.start_secs, end_secs: e.end_secs, text: e.text.clone(), spans: e.spans.clone(), placement: e.placement };
        let (again, _) = parse_ass(&generate_ass(&[sub]));
        assert_eq!((again[0].text.as_str(), again[0].placement, &again[0].spans), (e.text.as_str(), e.placement, &e.spans));
    }
}

#[cfg(test)]
mod language_tests {
    use super::*;

    fn cues() -> Vec<SubtitleEntry> {
        vec![SubtitleEntry { start_secs: 1.0, end_secs: 2.5, text: "Guten Morgen".into(), ..Default::default() }]
    }

    /// Each format that can say its language says what it was written
    /// with, and is read back (#2223); SRT says none.
    #[test]
    fn a_files_language_is_written_and_read_back() {
        let vtt = generate_vtt_in(&cues(), Some("de"));
        assert!(vtt.starts_with("WEBVTT\nLanguage: de\n\n"), "{vtt}");
        assert_eq!(vtt_language(&vtt).as_deref(), Some("de"));
        assert_eq!(parse_srt_vtt(&vtt), parse_srt_vtt(&generate_vtt(&cues())), "the header line is not a cue");
        assert_eq!(ttml::ttml_language(&ttml::generate_imsc(&cues(), "fr")).as_deref(), Some("fr"));
        assert_eq!(stl::stl_language(&stl::generate_stl(&cues(), 25, "sv", "t")).as_deref(), Some("sv"));
        assert_eq!(vtt_language(&generate_srt(&cues())), None);
        assert_eq!(vtt_language(&generate_vtt(&cues())), None);
    }

    /// Without a language the WebVTT file is as it always was.
    #[test]
    fn a_vtt_without_a_language_is_unchanged() {
        assert!(generate_vtt(&cues()).starts_with("WEBVTT\n\n00:00:01.000 --> "));
        assert_eq!(generate_vtt(&cues()), generate_vtt_in(&cues(), Some("  ")));
    }
}
