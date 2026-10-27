//! Scenarist SCC captions (#1516): CEA-608 line 21 data, the closed
//! captions US broadcast and many deliveries still ask for, read and
//! written. A file is timecoded lines of 608 byte pairs at 29.97 frames a
//! second, one pair a frame. Captions are written pop-on (loaded off
//! screen, shown on their start frame, erased on their end frame); pop-on,
//! roll-up and paint-on captions are read, on channel CC1.

use crate::styled::{Placement, Span, SpanStyle};
use crate::SubtitleEntry;

/// 608's frame rate, 30000/1001.
const FPS: f64 = 30000.0 / 1001.0;
const COLUMNS: usize = 32;
const ROWS: u8 = 15;

/// The seven colours 608 draws text in, by attribute number.
const COLOURS: [[u8; 3]; 7] = [[255, 255, 255], [0, 255, 0], [0, 0, 255], [0, 255, 255], [255, 0, 0], [255, 255, 0], [255, 0, 255]];

/// A preamble address code's first byte and row bit, by row (1-15),
/// on channel 1.
const PAC_ROWS: [(u8, u8); 15] = [
    (0x11, 0x00),
    (0x11, 0x20),
    (0x12, 0x00),
    (0x12, 0x20),
    (0x15, 0x00),
    (0x15, 0x20),
    (0x16, 0x00),
    (0x16, 0x20),
    (0x17, 0x00),
    (0x17, 0x20),
    (0x10, 0x00),
    (0x13, 0x00),
    (0x13, 0x20),
    (0x14, 0x00),
    (0x14, 0x20),
];

/// The special characters, second byte 0x30-0x3F after 0x11.
const SPECIAL: [char; 16] = ['®', '°', '½', '¿', '™', '¢', '£', '♪', 'à', '\u{a0}', 'è', 'â', 'ê', 'î', 'ô', 'û'];

/// The extended characters, second byte 0x20-0x3F after 0x12, then 0x13.
/// Each replaces the character sent before it, a fallback for decoders
/// without them.
const EXTENDED: [[char; 32]; 2] = [
    [
        'Á', 'É', 'Ó', 'Ú', 'Ü', 'ü', '‘', '¡', '*', '’', '—', '©', '℠', '•', '“', '”', 'À', 'Â', 'Ç', 'È', 'Ê', 'Ë', 'ë', 'Î', 'Ï', 'ï', 'Ô', 'Ù', 'ù', 'Û', '«', '»',
    ],
    [
        'Ã', 'ã', 'Í', 'Ì', 'ì', 'Ò', 'ò', 'Õ', 'õ', '{', '}', '\\', '^', '_', '|', '~', 'Ä', 'ä', 'Ö', 'ö', 'ß', '¥', '¤', '¦', 'Å', 'å', 'Ø', 'ø', '┌', '┐', '└', '┘',
    ],
];

/// A basic character's glyph: ASCII, but for the ten codes 608 gives to
/// other characters.
fn basic_char(b: u8) -> char {
    match b {
        0x2A => 'á',
        0x5C => 'é',
        0x5E => 'í',
        0x5F => 'ó',
        0x60 => 'ú',
        0x7B => 'ç',
        0x7C => '÷',
        0x7D => 'Ñ',
        0x7E => 'ñ',
        0x7F => '█',
        b => b as char,
    }
}

/// How a character is sent: one basic byte, or a two-byte code (with
/// the basic byte sent before it for an extended one).
enum Code {
    Basic(u8),
    Special(u8),
    Extended(u8, u8, u8),
}

fn code_of(c: char) -> Code {
    if let Some(b) = (0x20u8..=0x7F).find(|b| basic_char(*b) == c) {
        return Code::Basic(b);
    }
    if let Some(i) = SPECIAL.iter().position(|s| *s == c) {
        return Code::Special(0x30 + i as u8);
    }
    for (t, table) in EXTENDED.iter().enumerate() {
        if let Some(i) = table.iter().position(|s| *s == c) {
            let fallback = match c {
                'Á' | 'À' | 'Â' | 'Ã' | 'Ä' | 'Å' => b'A',
                'É' | 'È' | 'Ê' | 'Ë' => b'E',
                'Í' | 'Ì' | 'Î' | 'Ï' => b'I',
                'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => b'O',
                'Ú' | 'Ù' | 'Û' | 'Ü' => b'U',
                'ü' | 'ù' => b'u',
                'ë' => b'e',
                'ï' | 'ì' => b'i',
                'ã' | 'ä' | 'å' => b'a',
                'ò' | 'õ' | 'ö' | 'ø' => b'o',
                'Ç' => b'C',
                'ß' => b's',
                '‘' | '’' => b'\'',
                '“' | '”' | '«' | '»' => b'"',
                '—' => b'-',
                '¡' => b'!',
                _ => b' ',
            };
            return Code::Extended(fallback, 0x12 + t as u8, 0x20 + i as u8);
        }
    }
    match c {
        '\u{2013}' => Code::Basic(b'-'),
        '\u{2026}' => Code::Basic(b'.'),
        _ => Code::Basic(b'?'),
    }
}

/// `b` with odd parity in its top bit, as 608 sends it.
fn parity(b: u8) -> u8 {
    if b.count_ones().is_multiple_of(2) {
        b | 0x80
    } else {
        b
    }
}

/// A frame number as drop-frame timecode, `HH:MM:SS;FF`.
fn drop_frame_timecode(n: u64) -> String {
    let (d, m) = (n / 17982, n % 17982);
    let n = n + 18 * d + if m > 1 { 2 * ((m - 2) / 1798) } else { 0 };
    format!("{:02}:{:02}:{:02};{:02}", n / 108_000, n / 1800 % 60, n / 30 % 60, n % 30)
}

/// A timecode's frame number: drop-frame with `;` (or `.`) before the
/// frames, non-drop with `:`, both counting 29.97 frames a second.
fn timecode_frame(tc: &str) -> Option<u64> {
    let drop = tc.contains([';', '.']);
    let parts: Vec<u64> = tc.split([':', ';', '.']).map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    let [h, m, s, f] = parts[..] else { return None };
    // A timecode's own ranges, so the frame count cannot overflow.
    if h > 99 || m > 59 || s > 59 || f > 29 {
        return None;
    }
    let n = ((h * 60 + m) * 60 + s) * 30 + f;
    let minutes = h * 60 + m;
    Some(if drop { n - 2 * (minutes - minutes / 10) } else { n })
}

/// A cue's text style as 608 has it: one of its seven colours or white
/// italics, underlined or not. Bold has no 608 form.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Attr {
    colour: u8,
    italic: bool,
    underline: bool,
}

impl Attr {
    fn of(st: SpanStyle) -> Self {
        // The nearest of the seven colours; italics are white in a PAC.
        let colour = st.color.map_or(0, |c| {
            (0..7u8)
                .min_by_key(|i| {
                    let k = COLOURS[*i as usize];
                    (0..3).map(|j| (k[j] as i32 - c[j] as i32).pow(2)).sum::<i32>()
                })
                .unwrap_or(0)
        });
        Self { colour: if st.italic { 0 } else { colour }, italic: st.italic, underline: st.underline }
    }

    fn style(self) -> SpanStyle {
        SpanStyle { italic: self.italic, bold: false, underline: self.underline, color: (self.colour != 0).then(|| COLOURS[self.colour as usize]) }
    }

    /// A PAC's or mid-row code's attribute bits: colour 0-6, 7 italics,
    /// then underline in bit 0.
    fn bits(self) -> u8 {
        ((if self.italic { 7 } else { self.colour }) << 1) | self.underline as u8
    }
}

/// One caption row to send: where it starts and its words, each with
/// the attribute it is drawn in.
struct Row {
    row: u8,
    column: usize,
    cells: Vec<(char, Attr)>,
}

/// A cue's text as rows of at most 32 columns: its lines, wrapped at
/// spaces, a style change taking the place of the space before it (608
/// draws a mid-row code as a space). A word takes its first letter's
/// style.
fn rows_of(sub: &SubtitleEntry) -> Vec<Row> {
    let styled: Vec<(char, Attr)> = {
        let mut v = Vec::new();
        for (seg, st) in crate::styled::segments(&sub.text, &sub.spans) {
            v.extend(seg.chars().map(|c| (c, Attr::of(st))));
        }
        v
    };
    let mut lines: Vec<Vec<(char, Attr)>> = Vec::new();
    for line in styled.split(|(c, _)| *c == '\n') {
        // Words, each in its first letter's style.
        let words: Vec<Vec<(char, Attr)>> = line
            .split(|(c, _)| *c == ' ')
            .filter(|w| !w.is_empty())
            .map(|w| {
                let a = w[0].1;
                w.iter().map(|(c, _)| (*c, a)).collect()
            })
            .collect();
        let mut cur: Vec<(char, Attr)> = Vec::new();
        for w in words {
            if !cur.is_empty() && cur.len() + 1 + w.len() > COLUMNS {
                lines.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push((' ', w[0].1));
            }
            cur.extend(w.into_iter().take(COLUMNS));
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }
    let n = lines.len().min(ROWS as usize) as u8;
    let lines = &lines[..n as usize];
    let align = sub.placement.align.unwrap_or(match sub.placement.pos {
        Some((_, y)) if y < 1.0 / 3.0 => 8,
        Some((_, y)) if y < 2.0 / 3.0 => 5,
        _ => 2,
    });
    let first = match align {
        7..=9 => 2,
        4..=6 => (8 - n / 2).max(1),
        _ => ROWS + 1 - n,
    };
    lines
        .iter()
        .enumerate()
        .map(|(i, cells)| {
            let column = match align % 3 {
                1 => 0,
                0 => COLUMNS - cells.len(),
                _ => (COLUMNS - cells.len()) / 2,
            };
            Row { row: first + i as u8, column, cells: cells.clone() }
        })
        .collect()
}

/// The 608 byte stream that loads a cue into the caption off screen:
/// resume caption loading, erase it, then each row's address and text.
/// Control codes are sent twice; an odd byte before one is padded.
fn load_words(sub: &SubtitleEntry) -> Vec<[u8; 2]> {
    let mut words: Vec<[u8; 2]> = Vec::new();
    let mut pending: Option<u8> = None;
    let flush = |words: &mut Vec<[u8; 2]>, pending: &mut Option<u8>| {
        if let Some(b) = pending.take() {
            words.push([b, 0]);
        }
    };
    let control = |words: &mut Vec<[u8; 2]>, pending: &mut Option<u8>, w: [u8; 2]| {
        flush(words, pending);
        words.push(w);
        words.push(w);
    };
    let basic = |words: &mut Vec<[u8; 2]>, pending: &mut Option<u8>, b: u8| match pending.take() {
        Some(a) => words.push([a, b]),
        None => *pending = Some(b),
    };
    control(&mut words, &mut pending, [0x14, 0x20]);
    control(&mut words, &mut pending, [0x14, 0x2E]);
    for row in rows_of(sub) {
        let (b1, row_bit) = PAC_ROWS[row.row as usize - 1];
        let first = row.cells.first().map(|c| c.1).unwrap_or_default();
        let white = Attr { underline: first.underline, ..Attr::default() };
        // A white PAC indents in fours and a tab offset the rest; a styled
        // one starts at column 0. Indented styled text starts after a
        // mid-row code in the column before it.
        let (b2, tab, attr) = if first == white {
            (0x40 | row_bit | ((8 + row.column / 4) as u8) << 1 | first.underline as u8, row.column % 4, first)
        } else if row.column < 4 {
            (0x40 | row_bit | first.bits(), row.column, first)
        } else {
            (0x40 | row_bit | ((8 + (row.column - 1) / 4) as u8) << 1, (row.column - 1) % 4, Attr::default())
        };
        control(&mut words, &mut pending, [b1, b2]);
        let mut attr = attr;
        if tab > 0 {
            control(&mut words, &mut pending, [0x17, 0x20 + tab as u8]);
        }
        let mut i = 0;
        while i < row.cells.len() {
            let (c, a) = row.cells[i];
            // A style change at a space, or at the row's start after an
            // indent: a mid-row code in that space.
            if a != attr && (c == ' ' || i == 0) {
                control(&mut words, &mut pending, [0x11, 0x20 | a.bits()]);
                attr = a;
                if c == ' ' {
                    i += 1;
                    continue;
                }
            }
            match code_of(c) {
                Code::Basic(b) => basic(&mut words, &mut pending, b),
                Code::Special(b) => control(&mut words, &mut pending, [0x11, b]),
                Code::Extended(fallback, b1, b2) => {
                    basic(&mut words, &mut pending, fallback);
                    control(&mut words, &mut pending, [b1, b2]);
                }
            }
            i += 1;
        }
    }
    flush(&mut words, &mut pending);
    words
}

/// 608's frame rate, 30000/1001: the rate [`cc1_pairs`] counts in.
pub const CC_FPS: f64 = FPS;

/// `subs` as CEA-608 CC1 data, pop-on: the byte pair sent in each 608
/// frame that sends one, by frame from 0 at 29.97, parity set. The SCC
/// file's pairs, and the pairs a video stream carries (#1663).
pub fn cc1_pairs(subs: &[SubtitleEntry]) -> std::collections::BTreeMap<u64, [u8; 2]> {
    cc1_words(subs).into_iter().map(|(f, w)| (f, [parity(w[0]), parity(w[1])])).collect()
}

/// `subs` as Scenarist SCC timing, pop-on on CC1: each cue loaded in the
/// frames before its start, shown on its start frame (end of caption)
/// and erased on its end frame, unless the next cue replaces it there.
/// A cue whose load does not fit before its start shows as soon as it
/// has loaded. Each 608 frame's word, without parity.
fn cc1_words(subs: &[SubtitleEntry]) -> std::collections::BTreeMap<u64, [u8; 2]> {
    let frame = |secs: f64| (secs.max(0.0) * FPS).round() as u64;
    let mut sorted: Vec<&SubtitleEntry> = subs.iter().filter(|s| !s.text.trim().is_empty()).collect();
    sorted.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));
    // Each frame's word, in order.
    let mut slots: std::collections::BTreeMap<u64, [u8; 2]> = std::collections::BTreeMap::new();
    let free = |slots: &std::collections::BTreeMap<u64, [u8; 2]>, from: u64, n: u64| slots.range(from..from + n).next().is_none();
    let (eoc, edm) = ([0x14, 0x2F], [0x14, 0x2C]);
    // Where the last caption's end of caption ended: the next load
    // starts after it, so its erase does not clear that caption. And the
    // first frame the next end of caption may take, after any erase.
    let (mut loaded_after, mut shown_after) = (0u64, 0u64);
    for (i, sub) in sorted.iter().enumerate() {
        let mut words = load_words(sub);
        let load = words.len() as u64;
        words.push(eoc);
        words.push(eoc);
        let mut at = frame(sub.start_secs).saturating_sub(load).max(loaded_after);
        while !free(&slots, at, load + 2) || at + load < shown_after {
            at += 1;
        }
        for (k, w) in words.into_iter().enumerate() {
            slots.insert(at + k as u64, w);
        }
        let shown = at + load;
        (loaded_after, shown_after) = (shown + 2, shown + 2);
        let end = frame(sub.end_secs).max(shown + 2);
        // The next caption showing on this one's end frame replaces it.
        if sorted.get(i + 1).is_none_or(|n| frame(n.start_secs) > end) {
            let mut e = end;
            while !free(&slots, e, 2) {
                e += 1;
            }
            slots.insert(e, edm);
            slots.insert(e + 1, edm);
            shown_after = e + 2;
        }
    }
    slots
}

/// `subs` as a Scenarist SCC file, pop-on on CC1 (`cc1_words`).
pub fn generate_scc(subs: &[SubtitleEntry]) -> String {
    let slots = cc1_words(subs);
    let mut out = String::from("Scenarist_SCC V1.0\n");
    let mut last: Option<u64> = None;
    for (f, w) in &slots {
        if last != Some(f.wrapping_sub(1)) {
            out.push_str(&format!("\n{}\t", drop_frame_timecode(*f)));
        } else {
            out.push(' ');
        }
        out.push_str(&format!("{:02x}{:02x}", parity(w[0]), parity(w[1])));
        last = Some(*f);
    }
    out.push('\n');
    out
}

/// A caption memory: rows of 32 cells, each a character and its style.
type Memory = std::collections::BTreeMap<u8, [Option<(char, Attr)>; COLUMNS]>;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    PopOn,
    RollUp(u8),
    PaintOn,
}

/// The CC1 decoder: what is on screen, what is being loaded, where the
/// cursor is, and the cues the screen showed.
struct Decoder {
    displayed: Memory,
    loading: Memory,
    mode: Mode,
    row: u8,
    column: usize,
    attr: Attr,
    /// Whether the last code was for channel 1; data after a channel 2
    /// code is that channel's.
    cc1: bool,
    last_control: Option<[u8; 2]>,
    /// The time of the word being decoded.
    now: f64,
    /// When the screen was first drawn on since it was last read.
    dirty_at: Option<f64>,
    open: Option<SubtitleEntry>,
    cues: Vec<SubtitleEntry>,
}

impl Decoder {
    fn memory(&mut self) -> &mut Memory {
        match self.mode {
            Mode::PopOn => &mut self.loading,
            _ => {
                self.dirty_at.get_or_insert(self.now);
                &mut self.displayed
            }
        }
    }

    fn put(&mut self, c: char) {
        let (row, column, attr) = (self.row, self.column, self.attr);
        if column < COLUMNS {
            self.memory().entry(row).or_insert([None; COLUMNS])[column] = Some((c, attr));
            self.column += 1;
        }
    }

    /// The screen as a cue: its rows top to bottom, each from its first
    /// to its last character, and their styles.
    fn screen(&self) -> Option<(String, Vec<Span>, u8)> {
        let mut text = String::new();
        let mut spans: Vec<Span> = Vec::new();
        let mut first_row = None;
        for (row, cells) in &self.displayed {
            let Some(a) = cells.iter().position(|c| c.is_some_and(|(c, _)| c != ' ' && c != '\u{a0}')) else { continue };
            let b = cells.iter().rposition(|c| c.is_some_and(|(c, _)| c != ' ' && c != '\u{a0}')).unwrap_or(a);
            if !text.is_empty() {
                text.push('\n');
            }
            first_row.get_or_insert(*row);
            for cell in &cells[a..=b] {
                let (c, attr) = cell.unwrap_or((' ', Attr::default()));
                let c = if c == '\u{a0}' { ' ' } else { c };
                let start = text.len();
                text.push(c);
                let st = attr.style();
                if !st.is_plain() {
                    match spans.last_mut() {
                        Some(last) if last.style == st && last.end == start => last.end = text.len(),
                        _ => spans.push(Span { start, end: text.len(), style: st }),
                    }
                }
            }
        }
        first_row.map(|r| (text, spans, r))
    }

    /// The screen changed at `secs`: the cue it showed ends, and what it
    /// shows now, if anything, begins.
    fn flush(&mut self, secs: f64) {
        self.dirty_at = None;
        let now = self.screen();
        if let (Some(open), Some((text, spans, _))) = (&self.open, &now) {
            if open.text == *text && open.spans == *spans {
                return;
            }
        }
        if let Some(mut open) = self.open.take() {
            open.end_secs = secs;
            if open.end_secs > open.start_secs {
                self.cues.push(open);
            }
        }
        if let Some((text, spans, row)) = now {
            let align = match row {
                1..=5 => Some(8),
                6..=10 => Some(5),
                _ => None,
            };
            self.open = Some(SubtitleEntry { start_secs: secs, end_secs: secs, text, spans, placement: Placement { pos: None, align } });
        }
    }

    fn control(&mut self, b1: u8, b2: u8, secs: f64) {
        let channel_bit = b1 & 0x08;
        self.cc1 = channel_bit == 0;
        if !self.cc1 {
            return;
        }
        let b1 = b1 & !0x08;
        match (b1, b2) {
            // Preamble address codes: a row, an indent or a style.
            (0x10..=0x17, 0x40..=0x7F) => {
                let row_bit = b2 & 0x20;
                let Some(i) = PAC_ROWS.iter().position(|p| *p == (b1, row_bit)) else { return };
                let row = i as u8 + 1;
                let code = (b2 & 0x1E) >> 1;
                let underline = b2 & 1 == 1;
                if let Mode::RollUp(depth) = self.mode {
                    // The roll-up window moves to the new base row.
                    if row != self.row {
                        let rows: Vec<_> = self.displayed.iter().map(|(r, c)| (*r, *c)).collect();
                        self.displayed.clear();
                        for (r, cells) in rows {
                            let to = r as i32 + row as i32 - self.row as i32;
                            if (1..=ROWS as i32).contains(&to) && to > row as i32 - depth as i32 {
                                self.displayed.insert(to as u8, cells);
                            }
                        }
                        self.dirty_at.get_or_insert(self.now);
                    }
                }
                self.row = row;
                if code >= 8 {
                    self.column = (code as usize - 8) * 4;
                    self.attr = Attr { underline, ..Attr::default() };
                } else {
                    self.column = 0;
                    self.attr = Attr { colour: if code == 7 { 0 } else { code }, italic: code == 7, underline };
                }
            }
            // Mid-row codes: a style, drawn as a space.
            (0x11, 0x20..=0x2F) => {
                let code = (b2 & 0x0E) >> 1;
                let underline = b2 & 1 == 1;
                self.attr = if code == 7 { Attr { italic: true, underline, ..self.attr } } else { Attr { colour: code, italic: false, underline } };
                let attr = self.attr;
                self.attr = Attr::default();
                self.put(' ');
                self.attr = attr;
            }
            (0x11, 0x30..=0x3F) => self.put(SPECIAL[(b2 - 0x30) as usize]),
            (0x12 | 0x13, 0x20..=0x3F) => {
                self.column = self.column.saturating_sub(1);
                self.put(EXTENDED[(b1 - 0x12) as usize][(b2 - 0x20) as usize]);
            }
            (0x17, 0x21..=0x23) => self.column = (self.column + (b2 - 0x20) as usize).min(COLUMNS - 1),
            (0x14, 0x20) => self.mode = Mode::PopOn,
            (0x14, 0x21) => {
                if self.column > 0 {
                    self.column -= 1;
                    let (row, column) = (self.row, self.column);
                    if let Some(cells) = self.memory().get_mut(&row) {
                        cells[column] = None;
                    }
                }
            }
            (0x14, 0x24) => {
                let (row, column) = (self.row, self.column);
                if let Some(cells) = self.memory().get_mut(&row) {
                    cells[column..].fill(None);
                }
            }
            (0x14, 0x25..=0x27) => {
                let depth = b2 - 0x23;
                if !matches!(self.mode, Mode::RollUp(_)) {
                    self.displayed.clear();
                    self.loading.clear();
                    self.dirty_at.get_or_insert(self.now);
                    self.row = ROWS;
                    self.column = 0;
                }
                self.mode = Mode::RollUp(depth);
            }
            (0x14, 0x29) => self.mode = Mode::PaintOn,
            (0x14, 0x2C) => {
                self.displayed.clear();
                self.flush(secs);
            }
            (0x14, 0x2D) => {
                if let Mode::RollUp(depth) = self.mode {
                    self.flush(secs);
                    let base = self.row;
                    let rows: Vec<_> = std::mem::take(&mut self.displayed).into_iter().collect();
                    for (r, cells) in rows {
                        if r > 1 && r - 1 > base.saturating_sub(depth) && r <= base {
                            self.displayed.insert(r - 1, cells);
                        }
                    }
                    self.dirty_at.get_or_insert(self.now);
                }
                self.column = 0;
                self.attr = Attr::default();
            }
            (0x14, 0x2E) => self.loading.clear(),
            (0x14, 0x2F) => {
                std::mem::swap(&mut self.displayed, &mut self.loading);
                self.mode = Mode::PopOn;
                self.flush(secs);
            }
            _ => {}
        }
    }

    fn word(&mut self, b1: u8, b2: u8, secs: f64) {
        let (b1, b2) = (b1 & 0x7F, b2 & 0x7F);
        self.now = secs;
        if (0x10..=0x1F).contains(&b1) {
            // A control code sent twice acts once.
            if self.last_control == Some([b1, b2]) {
                self.last_control = None;
                return;
            }
            self.last_control = Some([b1, b2]);
            self.control(b1, b2, secs);
            return;
        }
        self.last_control = None;
        if !self.cc1 {
            return;
        }
        for b in [b1, b2] {
            if b >= 0x20 {
                self.put(basic_char(b));
            }
        }
    }
}

/// Cues from a Scenarist SCC file's CC1 captions: each time the screen
/// changes (a pop-on caption shown or erased, a roll-up line rolled, a
/// paint-on caption painted) one cue ends and the next begins, with its
/// rows as lines, its italics, underline and colours, and its rows' band
/// of the frame.
pub fn parse_scc(content: &str) -> Result<Vec<SubtitleEntry>, String> {
    let mut lines = content.lines().map(str::trim).filter(|l| !l.is_empty());
    if !lines.next().is_some_and(|l| l.trim_start_matches('\u{feff}').starts_with("Scenarist_SCC")) {
        return Err("not an SCC file: it does not start Scenarist_SCC".into());
    }
    let mut d = Decoder {
        displayed: Memory::new(),
        loading: Memory::new(),
        mode: Mode::PopOn,
        row: ROWS,
        column: 0,
        attr: Attr::default(),
        cc1: true,
        last_control: None,
        now: 0.0,
        dirty_at: None,
        open: None,
        cues: Vec::new(),
    };
    let mut last_secs = 0.0;
    for line in lines {
        let Some((tc, data)) = line.split_once(char::is_whitespace) else { continue };
        let Some(frame) = timecode_frame(tc) else { return Err(format!("not an SCC timecode: {tc:?}")) };
        let mut secs = frame as f64 / FPS;
        for (k, w) in data.split_whitespace().enumerate() {
            let Ok(v) = u16::from_str_radix(w, 16) else { return Err(format!("not an SCC word: {w:?}")) };
            secs = (frame + k as u64) as f64 / FPS;
            d.word((v >> 8) as u8, v as u8, secs);
        }
        // A roll-up or paint-on caption shows from its first drawn word.
        if let Some(at) = d.dirty_at {
            d.flush(at);
        }
        last_secs = secs;
    }
    if let Some(mut open) = d.open.take() {
        open.end_secs = (open.start_secs + 3.0).max(last_secs);
        d.cues.push(open);
    }
    Ok(d.cues)
}

#[cfg(test)]
mod tests {

    /// A timecode out of its own ranges is no timecode: no overflow, the
    /// file refused, not a wrapped or panicking frame count.
    #[test]
    fn an_out_of_range_timecode_is_refused() {
        assert_eq!(timecode_frame("99999999999999999:00:00:00"), None);
        assert_eq!(timecode_frame("00:61:00:00"), None);
        assert_eq!(timecode_frame("00:00:00;30"), None);
        assert_eq!(timecode_frame("01:00:00:00"), Some(108_000));
        assert!(parse_scc("Scenarist_SCC V1.0\n\n99999999999999999:00:00:00\t9420 9420\n").is_err());
    }

    use super::*;

    fn cue(start: f64, end: f64, text: &str) -> SubtitleEntry {
        SubtitleEntry { start_secs: start, end_secs: end, text: text.into(), ..Default::default() }
    }

    fn styled(spans: &[Span], i: usize) -> SpanStyle {
        spans.iter().find(|s| s.start <= i && i < s.end).map(|s| s.style).unwrap_or_default()
    }

    #[test]
    fn timecodes_count_frames_both_ways() {
        for n in [0u64, 1, 1799, 1800, 17981, 17982, 107_892, 1_000_003] {
            assert_eq!(timecode_frame(&drop_frame_timecode(n)), Some(n), "{n}: {}", drop_frame_timecode(n));
        }
        assert_eq!(drop_frame_timecode(1800), "00:01:00;02", "frames 0 and 1 are dropped each minute");
        assert_eq!(drop_frame_timecode(17982), "00:10:00;00", "but not each tenth");
        assert_eq!(timecode_frame("00:00:01:00"), Some(30));
    }

    /// What we write reads back as the same cues: start and end on their
    /// frames, line breaks, italics, underline and the seven colours,
    /// characters outside ASCII, a cue moved to the top, and two cues
    /// back to back.
    #[test]
    fn an_scc_file_reads_back_as_the_cues_written() {
        let mut a = cue(2.0, 3.5, "She said no twice.\nUnder gold");
        let it = SpanStyle { italic: true, ..Default::default() };
        let u = SpanStyle { underline: true, ..Default::default() };
        let yellow = SpanStyle { color: Some([255, 255, 0]), ..Default::default() };
        a.spans = vec![Span { start: 9, end: 11, style: it }, Span { start: 19, end: 24, style: u }, Span { start: 25, end: 29, style: yellow }];
        let mut top = cue(3.5, 5.0, "Café, ¿sí? ♪ Ünd {so}");
        top.placement = Placement { pos: None, align: Some(8) };
        let late = cue(3600.0 + 61.0, 3600.0 + 62.0, "An hour in");
        let subs = vec![a, top, late];
        let scc = generate_scc(&subs);
        assert!(scc.starts_with("Scenarist_SCC V1.0\n\n"), "{scc}");
        let back = parse_scc(&scc).unwrap();
        assert_eq!(back.len(), 3, "{back:#?}\n{scc}");
        for (w, r) in subs.iter().zip(&back) {
            assert!((w.start_secs - r.start_secs).abs() <= 0.5 / FPS && (w.end_secs - r.end_secs).abs() <= 0.5 / FPS, "{w:?} vs {r:?}");
            assert_eq!(w.text, r.text);
        }
        assert_eq!(back[0].spans, subs[0].spans);
        assert_eq!(styled(&back[0].spans, 10), it);
        assert_eq!(back[0].placement, Placement::default());
        assert_eq!(back[1].placement.align, Some(8));
    }

    /// A long line wraps at 32 columns, a style starting mid-line takes
    /// the space before it, and text 608 cannot draw is replaced.
    #[test]
    fn long_lines_wrap_and_styles_take_a_space() {
        let mut long = cue(0.0, 2.0, "This line is far longer than the thirty-two columns a caption row holds");
        long.spans = vec![Span { start: 0, end: 4, style: SpanStyle { bold: true, ..Default::default() } }];
        let back = parse_scc(&generate_scc(&[long])).unwrap();
        assert_eq!(back[0].text, "This line is far longer than the\nthirty-two columns a caption row\nholds");
        assert!(back[0].spans.is_empty(), "bold has no 608 form");
        assert!(back[0].text.lines().all(|l| l.chars().count() <= 32));
        let back = parse_scc(&generate_scc(&[cue(0.0, 1.0, "日本 ok")])).unwrap();
        assert_eq!(back[0].text, "?? ok");
    }

    /// A cue whose load does not fit before it (one right after another)
    /// shows as soon as it has loaded, and cues overlapping cut over.
    #[test]
    fn crowded_cues_still_show_in_order() {
        let subs = [cue(0.0, 0.1, "First caption with some text"), cue(0.1, 0.2, "Second one also has text"), cue(0.15, 2.0, "Third")];
        let back = parse_scc(&generate_scc(&subs)).unwrap();
        assert_eq!(back.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["First caption with some text", "Second one also has text", "Third"]);
        assert!(back.windows(2).all(|w| w[0].end_secs <= w[1].start_secs + 1e-9), "{back:#?}");
        assert!(back[2].end_secs >= 2.0 - 0.5 / FPS, "the last ends no sooner than asked");
    }

    /// Real files: roll-up captions from a live broadcast, a paint-on
    /// caption, a PAC in italics, mid-row colour, a doubled code acting
    /// once, a CC2 caption not taken, and non-drop timecode.
    #[test]
    fn broadcast_roll_up_and_paint_on_read() {
        // Roll-up 2: "HELLO THERE" then CR then "HOW ARE YOU".
        let scc = "Scenarist_SCC V1.0\n\n\
00:00:00:00\t9425 9425 94ad 94ad 9470 9470 c845 4c4c cf20 5448 4552 4580\n\n\
00:00:02:00\t9425 9425 94ad 94ad 9470 9470 c84f 5720 c152 4520 594f d580\n\n\
00:00:04:00\t942c 942c\n\n\
00:00:05:00\t9429 9429 94ce 94ce 91ae 91ae 6869 9120 9120 6f6b\n\n\
00:00:07:00\t942c 942c\n\n\
00:00:08:00\t1c20 1c20 1c2f 1c2f\n";
        let subs = parse_scc(scc).unwrap();
        let texts: Vec<&str> = subs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["HELLO THERE", "HELLO THERE\nHOW ARE YOU", "hi ok"], "{subs:#?}");
        assert!((subs[0].start_secs - 6.0 / FPS * 1.0).abs() < 0.5, "{}", subs[0].start_secs);
        assert!((subs[1].start_secs - 2.0 * 1.001).abs() < 0.3);
        assert!((subs[1].end_secs - 4.0 * 1.001).abs() < 1e-9);
        assert_eq!(styled(&subs[2].spans, 0), SpanStyle { italic: true, ..Default::default() });
        assert!(styled(&subs[2].spans, 3).is_plain());
        assert!(parse_scc("WEBVTT\n").is_err());
    }
}
