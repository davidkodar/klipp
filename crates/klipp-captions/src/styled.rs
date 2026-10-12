//! A cue's inline styling (#1515): the runs of its text that are italic,
//! bold, underlined or coloured, and where an ASS line puts it, read from
//! SRT and WebVTT tags and ASS override blocks and written back to them.

/// How a run of a cue's text is set off from the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanStyle {
    pub italic: bool,
    pub bold: bool,
    pub underline: bool,
    /// RGB; `None` is the cue's own colour.
    pub color: Option<[u8; 3]>,
}

impl SpanStyle {
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// A styled run of a cue's text, by byte offsets into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: SpanStyle,
}

/// Where a cue sits on the frame, when its file says: an ASS line's
/// `\pos`, as fractions of the frame, and its `\an` alignment (1-9, the
/// numeric keypad: 1 bottom left, 5 centre, 9 top right).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Placement {
    pub pos: Option<(f32, f32)>,
    pub align: Option<u8>,
}

/// Runs of `text`, each with its style: the spans' styles where they
/// cover it, plain elsewhere, in order, adjacent alike runs merged.
pub fn segments<'a>(text: &'a str, spans: &[Span]) -> Vec<(&'a str, SpanStyle)> {
    let mut cuts = vec![0, text.len()];
    for s in spans {
        cuts.push(s.start.min(text.len()));
        cuts.push(s.end.min(text.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<(usize, usize, SpanStyle)> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        let style = spans.iter().rev().find(|s| s.start <= a && b <= s.end).map(|s| s.style).unwrap_or_default();
        match out.last_mut() {
            Some(last) if last.2 == style && last.1 == a => last.1 = b,
            _ => out.push((a, b, style)),
        }
    }
    out.into_iter().map(|(a, b, s)| (&text[a..b], s)).collect()
}

/// Spans for runs of text appended one after another with their styles.
struct SpanBuilder {
    text: String,
    spans: Vec<Span>,
}

impl SpanBuilder {
    fn new() -> Self {
        Self { text: String::new(), spans: Vec::new() }
    }

    fn push(&mut self, s: &str, style: SpanStyle) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(s);
        if style.is_plain() {
            return;
        }
        match self.spans.last_mut() {
            Some(last) if last.style == style && last.end == start => last.end = self.text.len(),
            _ => self.spans.push(Span { start, end: self.text.len(), style }),
        }
    }
}

/// A colour by the names SRT and WebVTT captions use, or `#RRGGBB`.
fn named_colour(name: &str) -> Option<[u8; 3]> {
    let n = name.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
    if let Some(hex) = n.strip_prefix('#').or_else(|| n.strip_prefix("color")) {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let b = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
            return Some([b(0)?, b(2)?, b(4)?]);
        }
        return None;
    }
    Some(match n.as_str() {
        "white" => [255, 255, 255],
        "black" => [0, 0, 0],
        "red" => [255, 0, 0],
        "lime" => [0, 255, 0],
        "green" => [0, 128, 0],
        "blue" => [0, 0, 255],
        "yellow" => [255, 255, 0],
        "cyan" | "aqua" => [0, 255, 255],
        "magenta" | "fuchsia" => [255, 0, 255],
        _ => return None,
    })
}

/// SRT or WebVTT cue text with its markup read: `<i>`, `<b>`, `<u>`,
/// `<font color="…">`, and WebVTT's `<c.class>` with a colour class
/// (`yellow`, `colorRRGGBB`) become styled runs; WebVTT's voice,
/// language, ruby and timestamp tags are dropped, their text kept. Any
/// other `<…>` is text, as it was before.
pub fn parse_markup(text: &str) -> (String, Vec<Span>) {
    let mut out = SpanBuilder::new();
    let (mut italic, mut bold, mut underline) = (0u32, 0u32, 0u32);
    let mut colours: Vec<Option<[u8; 3]>> = Vec::new();
    // The open `<c>` and `<font>` tags, each with whether it set a colour.
    let mut classes: Vec<bool> = Vec::new();
    let mut fonts: Vec<bool> = Vec::new();
    let style = |i: u32, b: u32, u: u32, c: &[Option<[u8; 3]>]| SpanStyle { italic: i > 0, bold: b > 0, underline: u > 0, color: c.iter().rev().find_map(|c| *c) };
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        out.push(&rest[..lt], style(italic, bold, underline, &colours));
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            out.push(&rest[lt..], style(italic, bold, underline, &colours));
            rest = "";
            break;
        };
        let tag = after[..gt].trim();
        let lower = tag.to_ascii_lowercase();
        let (close, name) = match lower.strip_prefix('/') {
            Some(n) => (true, n.trim()),
            None => (false, lower.as_str()),
        };
        let head = name.split(|c: char| c.is_whitespace() || c == '.').next().unwrap_or("");
        let known = match (close, head) {
            (false, "i") => { italic += 1; true }
            (true, "i") => { italic = italic.saturating_sub(1); true }
            (false, "b") => { bold += 1; true }
            (true, "b") => { bold = bold.saturating_sub(1); true }
            (false, "u") => { underline += 1; true }
            (true, "u") => { underline = underline.saturating_sub(1); true }
            (false, "font") => {
                let colour = tag.split_once('=').filter(|(k, _)| k.to_ascii_lowercase().trim_end().ends_with("color")).and_then(|(_, v)| named_colour(v));
                colours.push(colour);
                fonts.push(true);
                true
            }
            (true, "font") => {
                if fonts.pop().is_some() {
                    colours.pop();
                }
                true
            }
            (false, "c") => {
                let colour = name.split('.').skip(1).find_map(named_colour);
                colours.push(colour);
                classes.push(true);
                true
            }
            (true, "c") => {
                if classes.pop().is_some() {
                    colours.pop();
                }
                true
            }
            (_, "v") | (_, "lang") | (_, "ruby") | (_, "rt") => true,
            (false, t) if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.') => true,
            _ => false,
        };
        if !known {
            out.push(&rest[lt..lt + 1 + gt + 1], style(italic, bold, underline, &colours));
        }
        rest = &after[gt + 1..];
    }
    out.push(rest, style(italic, bold, underline, &colours));
    (out.text, out.spans)
}

/// `text` with `spans` written as SRT's tags; unstyled text as it is.
pub fn srt_markup(text: &str, spans: &[Span]) -> String {
    if spans.iter().all(|s| s.style.is_plain()) {
        return text.to_string();
    }
    let mut out = String::new();
    for (seg, st) in segments(text, spans) {
        let (open, close) = tags(st, |c| (format!("<font color=\"#{:02x}{:02x}{:02x}\">", c[0], c[1], c[2]), "</font>".to_string()));
        out.push_str(&open);
        out.push_str(seg);
        out.push_str(&close);
    }
    out
}

/// `text` with `spans` written as WebVTT's tags, a colour as a
/// `colorRRGGBB` class (see [`vtt_colour_classes`]).
pub fn vtt_markup(text: &str, spans: &[Span]) -> String {
    if spans.iter().all(|s| s.style.is_plain()) {
        return text.to_string();
    }
    let mut out = String::new();
    for (seg, st) in segments(text, spans) {
        let (open, close) = tags(st, |c| (format!("<c.color{:02x}{:02x}{:02x}>", c[0], c[1], c[2]), "</c>".to_string()));
        out.push_str(&open);
        out.push_str(seg);
        out.push_str(&close);
    }
    out
}

/// The colour classes WebVTT's markup names, as a STYLE block's rules, so
/// a player that reads the file's styles shows them.
pub fn vtt_colour_classes(colours: impl IntoIterator<Item = [u8; 3]>) -> String {
    let mut seen: Vec<[u8; 3]> = Vec::new();
    for c in colours {
        if !seen.contains(&c) {
            seen.push(c);
        }
    }
    if seen.is_empty() {
        return String::new();
    }
    let mut out = String::from("STYLE\n");
    for c in seen {
        out.push_str(&format!("::cue(.color{0:02x}{1:02x}{2:02x}) {{ color: #{0:02x}{1:02x}{2:02x} }}\n", c[0], c[1], c[2]));
    }
    out.push('\n');
    out
}

fn tags(st: SpanStyle, colour: impl Fn([u8; 3]) -> (String, String)) -> (String, String) {
    let (mut open, mut close) = (String::new(), String::new());
    if st.bold {
        open.push_str("<b>");
    }
    if st.italic {
        open.push_str("<i>");
    }
    if st.underline {
        open.push_str("<u>");
    }
    if let Some(c) = st.color {
        let (o, c) = colour(c);
        open.push_str(&o);
        close.push_str(&c);
    }
    if st.underline {
        close.push_str("</u>");
    }
    if st.italic {
        close.push_str("</i>");
    }
    if st.bold {
        close.push_str("</b>");
    }
    (open, close)
}

/// An ASS Dialogue line's text with its override blocks read: `\i`, `\b`,
/// `\u` and `\c`/`\1c` as styled runs (`\r` back to the line's style),
/// `\pos(x,y)` over a `play_res` frame and `\an` as its placement; every
/// other override is dropped, as before. `\N` is a line break, `\n` and
/// `\h` spaces. The text is what [`crate::strip_ass_override_tags`] gives.
pub fn parse_ass_text(s: &str, play_res: (f32, f32)) -> (String, Vec<Span>, Placement) {
    let mut out = SpanBuilder::new();
    let mut st = SpanStyle::default();
    let mut placement = Placement::default();
    let mut chars = s.char_indices().peekable();
    let mut plain = String::new();
    while let Some((i, c)) = chars.next() {
        if c == '{' {
            out.push(&plain, st);
            plain.clear();
            let end = s[i + 1..].find('}').map_or(s.len(), |e| i + 1 + e);
            override_block(&s[i + 1..end], &mut st, &mut placement, play_res);
            while chars.peek().is_some_and(|(j, _)| *j <= end) {
                chars.next();
            }
            continue;
        }
        if c == '}' {
            continue;
        }
        if c == '\\' {
            match chars.peek().map(|(_, c)| *c) {
                Some('N') => { chars.next(); plain.push('\n'); }
                Some('n') | Some('h') => { chars.next(); plain.push(' '); }
                _ => plain.push(c),
            }
            continue;
        }
        plain.push(c);
    }
    out.push(&plain, st);
    (out.text, out.spans, placement)
}

fn override_block(block: &str, st: &mut SpanStyle, placement: &mut Placement, (rx, ry): (f32, f32)) {
    for tag in block.split('\\').map(str::trim).filter(|t| !t.is_empty()) {
        let num = |t: &str| t.trim().parse::<i64>().ok();
        if let Some(v) = tag.strip_prefix("pos(").and_then(|v| v.strip_suffix(')')) {
            let xy: Vec<f32> = v.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            if let [x, y] = xy[..] {
                if rx > 0.0 && ry > 0.0 {
                    placement.pos = Some((x / rx, y / ry));
                }
            }
        } else if let Some(n) = tag.strip_prefix("an").and_then(num) {
            if (1..=9).contains(&n) {
                placement.align = Some(n as u8);
            }
        } else if let Some(c) = tag.strip_prefix("1c").or_else(|| tag.strip_prefix('c')).filter(|c| c.is_empty() || c.starts_with('&')) {
            // `\c&H…&` or `\1c&H…&`; not `\clip(…)`.
            st.color = ass_colour(c);
        } else if tag == "r" || tag.starts_with('r') && !tag.starts_with("rnd") {
            *st = SpanStyle::default();
        } else if let Some(n) = tag.strip_prefix('i').and_then(num) {
            st.italic = n != 0;
        } else if let Some(n) = tag.strip_prefix('b').and_then(num) {
            // `\b1`, or a weight: 700 and over is bold.
            st.bold = n == 1 || n >= 700;
        } else if let Some(n) = tag.strip_prefix('u').and_then(num) {
            st.underline = n != 0;
        }
    }
}

/// `&HBBGGRR&` (or `&HAABBGGRR&`) as RGB; an empty one is the style's own.
fn ass_colour(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().trim_start_matches('&').trim_start_matches(['H', 'h']).trim_end_matches('&');
    if hex.is_empty() {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8])
}

/// `text` as an ASS Dialogue's text with `spans` and `placement` written
/// as override blocks over a `play_res` frame; a plain cue as it was.
/// The text's own braces become parentheses and its line breaks `\N`.
pub fn ass_markup(text: &str, spans: &[Span], placement: Placement, play_res: (f32, f32)) -> String {
    let escape = |s: &str| s.replace('\n', "\\N").replace('{', "(").replace('}', ")");
    let mut out = String::new();
    let mut head = String::new();
    if let Some(a) = placement.align {
        head.push_str(&format!("\\an{a}"));
    }
    if let Some((x, y)) = placement.pos {
        head.push_str(&format!("\\pos({},{})", (x * play_res.0).round(), (y * play_res.1).round()));
    }
    if !head.is_empty() {
        out.push_str(&format!("{{{head}}}"));
    }
    if spans.iter().all(|s| s.style.is_plain()) {
        out.push_str(&escape(text));
        return out;
    }
    let mut was = SpanStyle::default();
    for (seg, st) in segments(text, spans) {
        let mut o = String::new();
        if st.bold != was.bold {
            o.push_str(if st.bold { "\\b1" } else { "\\b0" });
        }
        if st.italic != was.italic {
            o.push_str(if st.italic { "\\i1" } else { "\\i0" });
        }
        if st.underline != was.underline {
            o.push_str(if st.underline { "\\u1" } else { "\\u0" });
        }
        if st.color != was.color {
            match st.color {
                Some(c) => o.push_str(&format!("\\c&H{:02X}{:02X}{:02X}&", c[2], c[1], c[0])),
                None => o.push_str("\\c"),
            }
        }
        if !o.is_empty() {
            out.push_str(&format!("{{{o}}}"));
        }
        out.push_str(&escape(seg));
        was = st;
    }
    out
}
