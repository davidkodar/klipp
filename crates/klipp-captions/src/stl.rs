//! EBU-STL captions (#1516): EBU Tech 3264 subtitle files, the binary
//! exchange format European broadcasters deliver and ask for, read and
//! written. A file is a 1024-byte General Subject Information block, then
//! a 128-byte Text and Timing Information block per subtitle (and per
//! extension of a long one). Files are written for Level-1 teletext in the
//! Latin character table (ISO 6937); teletext and open subtitles in that
//! table are read.

use crate::styled::{Placement, Span, SpanStyle};
use crate::SubtitleEntry;

const GSI_LEN: usize = 1024;
const TTI_LEN: usize = 128;
const TEXT_LEN: usize = 112;
/// The characters a teletext row holds after its control codes.
const ROW_TEXT: usize = 36;

/// The teletext alpha colours, by control code 0x00-0x07.
const TELETEXT: [[u8; 3]; 8] = [[0, 0, 0], [255, 0, 0], [0, 255, 0], [255, 255, 0], [0, 0, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255]];

/// ISO 6937's accented letters: a non-spacing diacritic byte, then the
/// base letter.
const ACCENTED: &[(char, u8, u8)] = &[
    ('À', 0xC1, b'A'), ('Á', 0xC2, b'A'), ('Â', 0xC3, b'A'), ('Ã', 0xC4, b'A'), ('Ä', 0xC8, b'A'), ('Å', 0xCA, b'A'), ('Ç', 0xCB, b'C'),
    ('È', 0xC1, b'E'), ('É', 0xC2, b'E'), ('Ê', 0xC3, b'E'), ('Ë', 0xC8, b'E'), ('Ì', 0xC1, b'I'), ('Í', 0xC2, b'I'), ('Î', 0xC3, b'I'),
    ('Ï', 0xC8, b'I'), ('Ñ', 0xC4, b'N'), ('Ò', 0xC1, b'O'), ('Ó', 0xC2, b'O'), ('Ô', 0xC3, b'O'), ('Õ', 0xC4, b'O'), ('Ö', 0xC8, b'O'),
    ('Ù', 0xC1, b'U'), ('Ú', 0xC2, b'U'), ('Û', 0xC3, b'U'), ('Ü', 0xC8, b'U'), ('Ý', 0xC2, b'Y'), ('à', 0xC1, b'a'), ('á', 0xC2, b'a'),
    ('â', 0xC3, b'a'), ('ã', 0xC4, b'a'), ('ä', 0xC8, b'a'), ('å', 0xCA, b'a'), ('ç', 0xCB, b'c'), ('è', 0xC1, b'e'), ('é', 0xC2, b'e'),
    ('ê', 0xC3, b'e'), ('ë', 0xC8, b'e'), ('ì', 0xC1, b'i'), ('í', 0xC2, b'i'), ('î', 0xC3, b'i'), ('ï', 0xC8, b'i'), ('ñ', 0xC4, b'n'),
    ('ò', 0xC1, b'o'), ('ó', 0xC2, b'o'), ('ô', 0xC3, b'o'), ('õ', 0xC4, b'o'), ('ö', 0xC8, b'o'), ('ù', 0xC1, b'u'), ('ú', 0xC2, b'u'),
    ('û', 0xC3, b'u'), ('ü', 0xC8, b'u'), ('ý', 0xC2, b'y'), ('ÿ', 0xC8, b'y'), ('Ā', 0xC5, b'A'), ('ā', 0xC5, b'a'), ('Ă', 0xC6, b'A'),
    ('ă', 0xC6, b'a'), ('Ą', 0xCE, b'A'), ('ą', 0xCE, b'a'), ('Ć', 0xC2, b'C'), ('ć', 0xC2, b'c'), ('Ĉ', 0xC3, b'C'), ('ĉ', 0xC3, b'c'),
    ('Ċ', 0xC7, b'C'), ('ċ', 0xC7, b'c'), ('Č', 0xCF, b'C'), ('č', 0xCF, b'c'), ('Ď', 0xCF, b'D'), ('ď', 0xCF, b'd'), ('Ē', 0xC5, b'E'),
    ('ē', 0xC5, b'e'), ('Ĕ', 0xC6, b'E'), ('ĕ', 0xC6, b'e'), ('Ė', 0xC7, b'E'), ('ė', 0xC7, b'e'), ('Ę', 0xCE, b'E'), ('ę', 0xCE, b'e'),
    ('Ě', 0xCF, b'E'), ('ě', 0xCF, b'e'), ('Ĝ', 0xC3, b'G'), ('ĝ', 0xC3, b'g'), ('Ğ', 0xC6, b'G'), ('ğ', 0xC6, b'g'), ('Ġ', 0xC7, b'G'),
    ('ġ', 0xC7, b'g'), ('Ģ', 0xCB, b'G'), ('ģ', 0xCB, b'g'), ('Ĥ', 0xC3, b'H'), ('ĥ', 0xC3, b'h'), ('Ĩ', 0xC4, b'I'), ('ĩ', 0xC4, b'i'),
    ('Ī', 0xC5, b'I'), ('ī', 0xC5, b'i'), ('Ĭ', 0xC6, b'I'), ('ĭ', 0xC6, b'i'), ('Į', 0xCE, b'I'), ('į', 0xCE, b'i'), ('İ', 0xC7, b'I'),
    ('Ĵ', 0xC3, b'J'), ('ĵ', 0xC3, b'j'), ('Ķ', 0xCB, b'K'), ('ķ', 0xCB, b'k'), ('Ĺ', 0xC2, b'L'), ('ĺ', 0xC2, b'l'), ('Ļ', 0xCB, b'L'),
    ('ļ', 0xCB, b'l'), ('Ľ', 0xCF, b'L'), ('ľ', 0xCF, b'l'), ('Ń', 0xC2, b'N'), ('ń', 0xC2, b'n'), ('Ņ', 0xCB, b'N'), ('ņ', 0xCB, b'n'),
    ('Ň', 0xCF, b'N'), ('ň', 0xCF, b'n'), ('Ō', 0xC5, b'O'), ('ō', 0xC5, b'o'), ('Ŏ', 0xC6, b'O'), ('ŏ', 0xC6, b'o'), ('Ő', 0xCD, b'O'),
    ('ő', 0xCD, b'o'), ('Ŕ', 0xC2, b'R'), ('ŕ', 0xC2, b'r'), ('Ŗ', 0xCB, b'R'), ('ŗ', 0xCB, b'r'), ('Ř', 0xCF, b'R'), ('ř', 0xCF, b'r'),
    ('Ś', 0xC2, b'S'), ('ś', 0xC2, b's'), ('Ŝ', 0xC3, b'S'), ('ŝ', 0xC3, b's'), ('Ş', 0xCB, b'S'), ('ş', 0xCB, b's'), ('Š', 0xCF, b'S'),
    ('š', 0xCF, b's'), ('Ţ', 0xCB, b'T'), ('ţ', 0xCB, b't'), ('Ť', 0xCF, b'T'), ('ť', 0xCF, b't'), ('Ũ', 0xC4, b'U'), ('ũ', 0xC4, b'u'),
    ('Ū', 0xC5, b'U'), ('ū', 0xC5, b'u'), ('Ŭ', 0xC6, b'U'), ('ŭ', 0xC6, b'u'), ('Ů', 0xCA, b'U'), ('ů', 0xCA, b'u'), ('Ű', 0xCD, b'U'),
    ('ű', 0xCD, b'u'), ('Ų', 0xCE, b'U'), ('ų', 0xCE, b'u'), ('Ŵ', 0xC3, b'W'), ('ŵ', 0xC3, b'w'), ('Ŷ', 0xC3, b'Y'), ('ŷ', 0xC3, b'y'),
    ('Ÿ', 0xC8, b'Y'), ('Ź', 0xC2, b'Z'), ('ź', 0xC2, b'z'), ('Ż', 0xC7, b'Z'), ('ż', 0xC7, b'z'), ('Ž', 0xCF, b'Z'), ('ž', 0xCF, b'z'),
];

/// ISO 6937's other characters, 0xA0-0xFF; `\0` where it has none.
const UPPER: [char; 96] = [
    '\u{a0}', '¡', '¢', '£', '$', '¥', '#', '§', '¤', '‘', '“', '«', '←', '↑', '→', '↓',
    '°', '±', '²', '³', '×', 'µ', '¶', '·', '÷', '’', '”', '»', '¼', '½', '¾', '¿',
    '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
    '―', '¹', '®', '©', '™', '♪', '¬', '¦', '\0', '\0', '\0', '\0', '⅛', '⅜', '⅝', '⅞',
    'Ω', 'Æ', 'Đ', 'ª', 'Ħ', '\0', 'Ĳ', 'Ŀ', 'Ł', 'Ø', 'Œ', 'º', 'Þ', 'Ŧ', 'Ŋ', 'ŉ',
    'ĸ', 'æ', 'đ', 'ð', 'ħ', 'ı', 'ĳ', 'ŀ', 'ł', 'ø', 'œ', 'ß', 'þ', 'ŧ', 'ŋ', '\u{ad}',
];

/// EBU's language codes for the languages a brief names most, by ISO
/// 639-1 code.
const LANGUAGES: &[(&str, u8)] = &[
    ("ca", 0x03), ("hr", 0x04), ("cy", 0x05), ("cs", 0x06), ("da", 0x07), ("de", 0x08), ("en", 0x09), ("es", 0x0A), ("et", 0x0C),
    ("eu", 0x0D), ("fr", 0x0F), ("ga", 0x11), ("gl", 0x13), ("is", 0x14), ("it", 0x15), ("lv", 0x18), ("lb", 0x19), ("lt", 0x1A),
    ("hu", 0x1B), ("mt", 0x1C), ("nl", 0x1D), ("no", 0x1E), ("nb", 0x1E), ("nn", 0x1E), ("pl", 0x20), ("pt", 0x21), ("ro", 0x22),
    ("sr", 0x24), ("sk", 0x25), ("sl", 0x26), ("fi", 0x27), ("sv", 0x28), ("tr", 0x29),
];

/// The language an EBU-STL file's general subject block names in its
/// language code (LC), as an ISO 639-1 tag; `None` when it isn't an STL
/// file or the code is unset or not one this writes (#2223).
pub fn stl_language(bytes: &[u8]) -> Option<String> {
    if bytes.len() < GSI_LEN || !bytes[3..6].eq_ignore_ascii_case(b"STL") {
        return None;
    }
    let lc = u8::from_str_radix(std::str::from_utf8(&bytes[14..16]).ok()?.trim(), 16).ok().filter(|lc| *lc != 0)?;
    LANGUAGES.iter().find(|l| l.1 == lc).map(|l| l.0.to_string())
}

/// A character as ISO 6937 bytes: one, or a diacritic and a letter; `?`
/// for one it has no form for. `$` goes as 0x24, as readers take it.
fn encode_char(c: char, out: &mut Vec<u8>) {
    match c {
        ' '..='~' => out.push(c as u8),
        _ => {
            if let Some((_, d, base)) = ACCENTED.iter().find(|a| a.0 == c) {
                out.extend([*d, *base]);
            } else if let Some(i) = UPPER.iter().position(|u| *u == c && *u != '\0') {
                out.push(0xA0 + i as u8);
            } else {
                out.push(b'?');
            }
        }
    }
}

/// A teletext style: the colour code in force, italics and underline.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Attr {
    colour: u8,
    italic: bool,
    underline: bool,
}

impl Attr {
    const PLAIN: Self = Self { colour: 7, italic: false, underline: false };

    fn of(st: SpanStyle) -> Self {
        let colour = st.color.map_or(7, |c| {
            (0..8u8)
                .min_by_key(|i| {
                    let k = TELETEXT[*i as usize];
                    (0..3).map(|j| (k[j] as i32 - c[j] as i32).pow(2)).sum::<i32>()
                })
                .unwrap_or(7)
        });
        Self { colour, italic: st.italic, underline: st.underline }
    }

    fn style(self) -> SpanStyle {
        SpanStyle { italic: self.italic, bold: false, underline: self.underline, color: (self.colour != 7).then(|| TELETEXT[self.colour as usize]) }
    }
}

/// A cue's text field: each line double height in a box, lines apart by
/// a doubled CR/LF; italics and underline switched where they change, a
/// colour at the space before its word (teletext draws a colour code as
/// a space), a word taking its first letter's colour. A line longer than
/// a teletext row holds wraps at a space.
fn text_field(sub: &SubtitleEntry) -> Vec<u8> {
    let mut chars: Vec<(char, Attr)> = Vec::new();
    for (seg, st) in crate::styled::segments(&sub.text, &sub.spans) {
        chars.extend(seg.chars().map(|c| (c, Attr::of(st))));
    }
    // 40 cells a row, less the height, box and colour codes.
    let mut since_break = 0;
    let mut last_space = None;
    for i in 0..chars.len() {
        match chars[i].0 {
            '\n' => (since_break, last_space) = (0, None),
            c => {
                if c == ' ' {
                    last_space = Some(i);
                }
                since_break += 1;
                if since_break > ROW_TEXT {
                    if let Some(at) = last_space.take() {
                        chars[at].0 = '\n';
                        since_break = i - at;
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut lines = chars.split(|(c, _)| *c == '\n').filter(|l| l.iter().any(|(c, _)| !c.is_whitespace())).peekable();
    while let Some(line) = lines.next() {
        let line: Vec<(char, Attr)> = {
            let a = line.iter().position(|(c, _)| !c.is_whitespace()).unwrap_or(0);
            let b = line.iter().rposition(|(c, _)| !c.is_whitespace()).map_or(0, |b| b + 1);
            line[a..b].to_vec()
        };
        // A word's colour is its first letter's.
        let mut word_colour = Vec::with_capacity(line.len());
        let mut colour = line[0].1.colour;
        for (i, (c, a)) in line.iter().enumerate() {
            if *c != ' ' && (i == 0 || line[i - 1].0 == ' ') {
                colour = a.colour;
            }
            word_colour.push(colour);
        }
        out.push(0x0D);
        if word_colour[0] != 7 {
            out.push(word_colour[0]);
        }
        out.extend([0x0B, 0x0B]);
        let (mut italic, mut underline, mut colour) = (false, false, word_colour[0]);
        for (i, (c, a)) in line.iter().enumerate() {
            if *c == ' ' && word_colour.get(i + 1).is_some_and(|n| *n != colour) {
                colour = word_colour[i + 1];
                out.push(colour);
                continue;
            }
            if a.italic != italic {
                italic = a.italic;
                out.push(if italic { 0x80 } else { 0x81 });
            }
            if a.underline != underline {
                underline = a.underline;
                out.push(if underline { 0x82 } else { 0x83 });
            }
            encode_char(*c, &mut out);
        }
        if italic {
            out.push(0x81);
        }
        if underline {
            out.push(0x83);
        }
        out.extend([0x0A, 0x0A]);
        if lines.peek().is_some() {
            out.extend([0x8A, 0x8A]);
        }
    }
    out
}

fn timecode(secs: f64, fps: u32) -> [u8; 4] {
    let n = (secs.max(0.0) * fps as f64).round() as u64;
    let fps = fps as u64;
    [(n / (fps * 3600)).min(23) as u8, (n / (fps * 60) % 60) as u8, (n / fps % 60) as u8, (n % fps) as u8]
}

/// Today as `YYMMDD`, for the file's creation and revision dates.
fn today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Days since 1970 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{:02}{m:02}{d:02}", y % 100)
}

/// `subs` as an EBU-STL file for Level-1 teletext at `fps` (25, or 30 for
/// 30 and 29.97 frame material) in `lang` (an ISO 639-1 code, or empty):
/// each cue in its own blocks, its lines double height, at the bottom of
/// the frame, or the top or middle when it is placed there, justified as
/// it is aligned. `title` names the programme.
pub fn generate_stl(subs: &[SubtitleEntry], fps: u32, lang: &str, title: &str) -> Vec<u8> {
    let fps = if fps == 30 { 30 } else { 25 };
    let mut sorted: Vec<&SubtitleEntry> = subs.iter().filter(|s| !s.text.trim().is_empty()).collect();
    sorted.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
    let mut blocks: Vec<[u8; TTI_LEN]> = Vec::new();
    let mut max_rows = 0usize;
    for (n, sub) in sorted.iter().enumerate() {
        let text = text_field(sub);
        let lines = text.iter().filter(|b| **b == 0x0D).count().max(1);
        max_rows = max_rows.max(lines);
        let align = sub.placement.align.unwrap_or(match sub.placement.pos {
            Some((_, y)) if y < 1.0 / 3.0 => 8,
            Some((_, y)) if y < 2.0 / 3.0 => 5,
            _ => 2,
        });
        // Double height: each line takes two of teletext's 23 rows.
        let vp = match align {
            7..=9 => 2,
            4..=6 => 12usize.saturating_sub(lines).max(1),
            _ => (23 - 2 * lines as i32).max(1) as usize,
        };
        let jc = match align % 3 {
            1 => 1,
            0 => 3,
            _ => 2,
        };
        let chunks: Vec<&[u8]> = if text.is_empty() { vec![&[][..]] } else { text.chunks(TEXT_LEN).collect() };
        for (k, chunk) in chunks.iter().enumerate() {
            let mut b = [0x8Fu8; TTI_LEN];
            b[0] = 0;
            b[1..3].copy_from_slice(&((n + 1) as u16).to_le_bytes());
            b[3] = if k + 1 == chunks.len() { 0xFF } else { k as u8 };
            b[4] = 0;
            b[5..9].copy_from_slice(&timecode(sub.start_secs, fps));
            b[9..13].copy_from_slice(&timecode(sub.end_secs, fps));
            b[13] = vp as u8;
            b[14] = jc;
            b[15] = 0;
            b[16..16 + chunk.len()].copy_from_slice(chunk);
            blocks.push(b);
        }
    }
    let mut gsi = vec![b' '; GSI_LEN];
    let mut put = |at: usize, s: &[u8], len: usize| {
        let n = s.len().min(len);
        gsi[at..at + n].copy_from_slice(&s[..n]);
    };
    let lang = lang.trim().to_ascii_lowercase();
    let lc = LANGUAGES.iter().find(|l| lang.split(['-', '_']).next() == Some(l.0)).map_or(0, |l| l.1);
    let mut title_bytes = Vec::new();
    for c in title.chars() {
        encode_char(c, &mut title_bytes);
    }
    let date = today();
    put(0, b"850", 3);
    put(3, if fps == 30 { b"STL30.01" } else { b"STL25.01" }, 8);
    put(11, b"1", 1);
    put(12, b"00", 2);
    put(14, format!("{lc:02X}").as_bytes(), 2);
    put(16, &title_bytes, 32);
    put(224, date.as_bytes(), 6);
    put(230, date.as_bytes(), 6);
    put(236, b"01", 2);
    put(238, format!("{:05}", blocks.len()).as_bytes(), 5);
    put(243, format!("{:05}", sorted.len()).as_bytes(), 5);
    put(248, b"001", 3);
    put(251, b"40", 2);
    put(253, b"23", 2);
    put(255, b"1", 1);
    put(256, b"00000000", 8);
    let first = sorted.first().map_or([0; 4], |s| timecode(s.start_secs, fps));
    put(264, format!("{:02}{:02}{:02}{:02}", first[0], first[1], first[2], first[3]).as_bytes(), 8);
    put(272, b"1", 1);
    put(273, b"1", 1);
    for b in blocks {
        gsi.extend_from_slice(&b);
    }
    gsi
}

/// A text field's lines and styles: teletext colours (drawn as a space),
/// italics and underline (kept over the lines of a cue), ISO 6937
/// characters with their accents, a CR/LF a new line (doubled ones and
/// blank lines dropped), and the fill bytes ignored.
fn read_text(field: &[u8]) -> (String, Vec<Span>) {
    let mut lines: Vec<Vec<(char, Attr)>> = vec![Vec::new()];
    let mut attr = Attr::PLAIN;
    let mut accent: Option<u8> = None;
    for &b in field {
        let line = lines.last_mut().expect("a line");
        match b {
            0x8A => {
                lines.push(Vec::new());
                attr.colour = 7;
            }
            0x8F => {}
            0x80 => attr.italic = true,
            0x81 => attr.italic = false,
            0x82 => attr.underline = true,
            0x83 => attr.underline = false,
            0x84..=0x9F => {}
            0x00..=0x1F => {
                if b <= 0x07 {
                    attr.colour = b;
                }
                // A spacing attribute: a space, once.
                if line.last().is_some_and(|(c, _)| *c != ' ') {
                    line.push((' ', Attr::PLAIN));
                }
            }
            0xC1..=0xCF => accent = Some(b),
            _ => {
                let c = match b {
                    0x24 => '$',
                    0x20..=0x7E => b as char,
                    0xA0..=0xFF => UPPER[(b - 0xA0) as usize],
                    _ => '\0',
                };
                let c = match accent.take() {
                    Some(d) => ACCENTED.iter().find(|a| a.1 == d && a.2 == b).map_or(c, |a| a.0),
                    None => c,
                };
                if c != '\0' {
                    line.push((if c == '\u{a0}' { ' ' } else { c }, attr));
                }
            }
        }
    }
    let mut text = String::new();
    let mut spans: Vec<Span> = Vec::new();
    for line in lines {
        let Some(a) = line.iter().position(|(c, _)| *c != ' ') else { continue };
        let b = line.iter().rposition(|(c, _)| *c != ' ').unwrap_or(a);
        if !text.is_empty() {
            text.push('\n');
        }
        for (c, attr) in &line[a..=b] {
            let start = text.len();
            text.push(*c);
            let st = attr.style();
            if !st.is_plain() {
                match spans.last_mut() {
                    Some(last) if last.style == st && last.end == start => last.end = text.len(),
                    _ => spans.push(Span { start, end: text.len(), style: st }),
                }
            }
        }
    }
    (text, spans)
}

/// Cues from an EBU-STL file: each subtitle's text (its extension blocks
/// joined) with its line breaks, italics, underline and colours, its
/// times (from the start of programme when every cue is after it), and
/// its vertical position's band of the frame and its justification.
/// Comments and user data are left out; a character table other than
/// Latin is refused.
pub fn parse_stl(bytes: &[u8]) -> Result<Vec<SubtitleEntry>, String> {
    if bytes.len() < GSI_LEN || !bytes[3..6].eq_ignore_ascii_case(b"STL") {
        return Err("not an EBU-STL file: it has no STL general subject block".into());
    }
    let field = |at: usize, len: usize| String::from_utf8_lossy(&bytes[at..at + len]).trim().to_string();
    let fps: f64 = match &field(3, 8)[..] {
        f if f.starts_with("STL30") => 30.0,
        f if f.starts_with("STL24") => 24.0,
        _ => 25.0,
    };
    let cct = field(12, 2);
    if !cct.is_empty() && cct != "00" {
        return Err(format!("EBU-STL character code table {cct} is not read; only Latin (00) is"));
    }
    let open = matches!(field(11, 1).as_str(), "" | "0");
    let rows: f64 = field(253, 2).parse().ok().filter(|r: &f64| *r > 0.0).unwrap_or(if open { 99.0 } else { 23.0 });
    let tc = |b: &[u8]| b[0] as f64 * 3600.0 + b[1] as f64 * 60.0 + b[2] as f64 + b[3] as f64 / fps;
    let start_of_programme = {
        let t = field(256, 8);
        let d: Vec<f64> = (0..4).filter_map(|i| t.get(i * 2..i * 2 + 2)?.parse().ok()).collect();
        if d.len() == 4 && field(255, 1) == "1" { d[0] * 3600.0 + d[1] * 60.0 + d[2] + d[3] / fps } else { 0.0 }
    };
    let mut subs: Vec<SubtitleEntry> = Vec::new();
    let mut text_of: Vec<u8> = Vec::new();
    for b in bytes[GSI_LEN..].chunks_exact(TTI_LEN) {
        // User data, and comments, are not subtitles.
        if b[3] == 0xFE || b[15] != 0 {
            continue;
        }
        text_of.extend_from_slice(&b[16..]);
        if b[3] != 0xFF {
            continue;
        }
        let (text, spans) = read_text(&std::mem::take(&mut text_of));
        let (start, end) = (tc(&b[5..9]), tc(&b[9..13]));
        if text.is_empty() || end <= start {
            continue;
        }
        let band = b[13] as f64 / rows;
        let row = if band < 1.0 / 3.0 { 6 } else if band < 0.5 { 3 } else { 0 };
        let side = match b[14] {
            1 => Some(1),
            3 => Some(3),
            _ => None,
        };
        let align = match (side, row) {
            (None, 0) => None,
            (side, row) => Some(side.unwrap_or(2) + row),
        };
        subs.push(SubtitleEntry { start_secs: start, end_secs: end, text, spans, placement: Placement { pos: None, align } });
    }
    if start_of_programme > 0.0 && subs.iter().all(|s| s.start_secs >= start_of_programme) {
        for s in &mut subs {
            s.start_secs -= start_of_programme;
            s.end_secs -= start_of_programme;
        }
    }
    Ok(subs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: f64, end: f64, text: &str) -> SubtitleEntry {
        SubtitleEntry { start_secs: start, end_secs: end, text: text.into(), ..Default::default() }
    }

    fn styled(spans: &[Span], i: usize) -> SpanStyle {
        spans.iter().find(|s| s.start <= i && i < s.end).map(|s| s.style).unwrap_or_default()
    }

    /// What we write reads back as the same cues: times on their frames,
    /// line breaks, italics and underline to the letter, teletext colours,
    /// accented and other Latin characters, a long cue over extension
    /// blocks, and a cue's placement and justification.
    #[test]
    fn an_stl_file_reads_back_as_the_cues_written() {
        let mut a = cue(1.0, 3.48, "She said no, twice.\nUnder gold");
        let it = SpanStyle { italic: true, ..Default::default() };
        let u = SpanStyle { underline: true, ..Default::default() };
        let yellow = SpanStyle { color: Some([255, 255, 0]), ..Default::default() };
        a.spans = vec![Span { start: 9, end: 11, style: it }, Span { start: 20, end: 25, style: u }, Span { start: 26, end: 30, style: yellow }];
        let mut top = cue(4.0, 5.0, "Ça coûte 5 € — non, 5 $ ½ ♪ Łódź Ærø");
        top.placement = Placement { pos: None, align: Some(7) };
        let long = cue(3600.0 + 61.0, 3600.0 + 62.0, "A long subtitle line that goes on. ".repeat(5).trim());
        let subs = vec![a, top, long];
        let stl = generate_stl(&subs, 25, "fr", "Le Film");
        assert_eq!(&stl[0..11], b"850STL25.01");
        assert_eq!(&stl[14..16], b"0F");
        assert_eq!(&stl[16..23], b"Le Film");
        assert_eq!((stl.len() - GSI_LEN) % TTI_LEN, 0);
        let blocks = (stl.len() - GSI_LEN) / TTI_LEN;
        assert!(blocks > 3, "the long cue takes extension blocks");
        assert_eq!(&stl[238..243], format!("{blocks:05}").as_bytes());
        let back = parse_stl(&stl).unwrap();
        assert_eq!(back.len(), 3, "{back:#?}");
        assert!((back[0].start_secs - 1.0).abs() < 1e-9 && (back[0].end_secs - 3.48).abs() < 1e-9);
        assert!((back[2].start_secs - 3661.0).abs() < 1e-9);
        assert_eq!(back[0].text, subs[0].text);
        assert_eq!(back[0].spans, subs[0].spans);
        assert_eq!(styled(&back[0].spans, 10), it);
        assert_eq!(back[0].placement, Placement::default());
        assert_eq!(back[1].text, "Ça coûte 5 ? — non, 5 $ ½ ♪ Łódź Ærø".replace('—', "?"), "no euro or dash in ISO 6937");
        assert_eq!(back[1].placement.align, Some(7));
        assert_eq!(back[2].text.replace('\n', " "), subs[2].text);
        assert!(back[2].text.lines().count() == 5 && back[2].text.lines().all(|l| l.len() <= ROW_TEXT), "{}", back[2].text);
    }

    /// A broadcaster's file: open subtitling at 30 frames, a start of
    /// programme at ten hours, a comment and a user-data block left out,
    /// teletext colour codes mid-line, double CR/LF, and a refused
    /// character table.
    #[test]
    fn a_broadcast_file_reads() {
        let mut gsi = vec![b' '; GSI_LEN];
        gsi[0..11].copy_from_slice(b"850STL30.01");
        gsi[11] = b'0';
        gsi[12..14].copy_from_slice(b"00");
        gsi[253..255].copy_from_slice(b"99");
        gsi[255] = b'1';
        gsi[256..264].copy_from_slice(b"10000000");
        let tti = |sn: u16, ebn: u8, cf: u8, tci: [u8; 4], tco: [u8; 4], vp: u8, jc: u8, text: &[u8]| {
            let mut b = vec![0x8Fu8; TTI_LEN];
            b[1..3].copy_from_slice(&sn.to_le_bytes());
            b[3] = ebn;
            b[4] = 0;
            b[5..9].copy_from_slice(&tci);
            b[9..13].copy_from_slice(&tco);
            b[13] = vp;
            b[14] = jc;
            b[15] = cf;
            b[16..16 + text.len()].copy_from_slice(text);
            b
        };
        let mut file = gsi;
        file.extend(tti(1, 0xFF, 0, [10, 0, 1, 15], [10, 0, 3, 0], 90, 2, b"\x0d\x0b\x0bPlain \x03yellow\x07white\x0a\x0a\x8a\x8a\x0d\x0b\x0b\x80Slanted\x81 \xc2e\x0a\x0a"));
        file.extend(tti(2, 0xFF, 1, [10, 0, 4, 0], [10, 0, 5, 0], 90, 2, b"A comment"));
        file.extend(tti(3, 0xFE, 0, [10, 0, 4, 0], [10, 0, 5, 0], 90, 2, b"user data"));
        file.extend(tti(4, 0xFF, 0, [10, 0, 6, 0], [10, 0, 7, 0], 5, 1, b"Top left"));
        let subs = parse_stl(&file).unwrap();
        assert_eq!(subs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Plain yellow white\nSlanted é", "Top left"]);
        assert!((subs[0].start_secs - 1.5).abs() < 1e-9 && (subs[0].end_secs - 3.0).abs() < 1e-9, "from the start of programme");
        assert_eq!(styled(&subs[0].spans, 6), SpanStyle { color: Some([255, 255, 0]), ..Default::default() });
        assert!(styled(&subs[0].spans, 13).is_plain());
        assert_eq!(styled(&subs[0].spans, 19), SpanStyle { italic: true, ..Default::default() });
        assert!(styled(&subs[0].spans, 27).is_plain());
        assert_eq!(subs[0].placement, Placement::default());
        assert_eq!(subs[1].placement.align, Some(7));

        let mut cyrillic = file.clone();
        cyrillic[12..14].copy_from_slice(b"01");
        assert!(parse_stl(&cyrillic).unwrap_err().contains("01"));
        assert!(parse_stl(b"WEBVTT\n").is_err());
    }
}
