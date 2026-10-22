//! TTML captions (#1516): IMSC 1.1 Text Profile written, the master
//! caption format of the streaming platforms, and the TTML family read —
//! TTML 1 and 2, DFXP, IMSC and Apple's iTT — with their timing, line
//! breaks, the inline styling a cue keeps (#1515) and the region it sits
//! in.

use crate::styled::{Placement, Span, SpanStyle};
use crate::SubtitleEntry;

const TT: &str = "http://www.w3.org/ns/ttml";
const TTS: &str = "http://www.w3.org/ns/ttml#styling";
const TTP: &str = "http://www.w3.org/ns/ttml#parameter";
/// DFXP's namespaces, from TTML's drafts, still in delivered files.
const TT_DRAFTS: [&str; 2] = ["http://www.w3.org/2006/10/ttaf1", "http://www.w3.org/2006/04/ttaf1"];
const TTS_DRAFTS: [&str; 2] = ["http://www.w3.org/2006/10/ttaf1#styling", "http://www.w3.org/2006/04/ttaf1#style"];

/// IMSC 1.1 Text Profile's designator.
pub const IMSC1_1_TEXT: &str = "http://www.w3.org/ns/ttml/profile/imsc1.1/text";

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A clock time, `HH:MM:SS.mmm`, from seconds, to the millisecond.
pub fn format_ttml_time(secs: f64) -> String {
    let ms = (secs.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// `subs` as an IMSC 1.1 Text Profile document in `lang` (a BCP 47 tag,
/// `en` when empty): white text on the bottom of the frame, a cue with a
/// placement in a region of its own there; each cue's italics, bold,
/// underline and colour as spans, its line breaks as `<br/>`.
pub fn generate_imsc(subs: &[SubtitleEntry], lang: &str) -> String {
    let lang = if lang.trim().is_empty() { "en" } else { lang.trim() };
    // A region a cue sits in: the bottom band, or one centred on its placement.
    let region_of = |p: Placement| -> Option<(f32, f32)> { p.pos };
    let mut placed: Vec<(f32, f32)> = Vec::new();
    for s in subs {
        if let Some(at) = region_of(s.placement) {
            if !placed.iter().any(|q| (q.0 - at.0).abs() < 1e-4 && (q.1 - at.1).abs() < 1e-4) {
                placed.push(at);
            }
        }
    }
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<tt xmlns=\"{TT}\" xmlns:tts=\"{TTS}\" xmlns:ttp=\"{TTP}\" ttp:contentProfiles=\"{IMSC1_1_TEXT}\" ttp:timeBase=\"media\" xml:lang=\"{}\">\n",
        escape(lang)
    ));
    out.push_str("  <head>\n    <styling>\n");
    out.push_str("      <style xml:id=\"s0\" tts:color=\"#ffffff\" tts:fontFamily=\"proportionalSansSerif\" tts:fontSize=\"100%\" tts:lineHeight=\"125%\" tts:textAlign=\"center\" tts:backgroundColor=\"#000000c2\"/>\n");
    out.push_str("    </styling>\n    <layout>\n");
    out.push_str("      <region xml:id=\"bottom\" tts:origin=\"10% 75%\" tts:extent=\"80% 20%\" tts:displayAlign=\"after\" tts:textAlign=\"center\"/>\n");
    out.push_str("      <region xml:id=\"top\" tts:origin=\"10% 5%\" tts:extent=\"80% 20%\" tts:displayAlign=\"before\" tts:textAlign=\"center\"/>\n");
    out.push_str("      <region xml:id=\"middle\" tts:origin=\"10% 40%\" tts:extent=\"80% 20%\" tts:displayAlign=\"center\" tts:textAlign=\"center\"/>\n");
    for (i, (x, y)) in placed.iter().enumerate() {
        // Centred on the placement: 80% wide and 20% tall, less near an
        // edge, so it stays on the frame.
        let (w, h) = ((2.0 * x.min(1.0 - x)).clamp(0.0, 0.8), (2.0 * y.min(1.0 - y)).clamp(0.0, 0.2));
        let (ox, oy) = ((x - w / 2.0) * 100.0, (y - h / 2.0) * 100.0);
        let (w, h) = (w * 100.0, h * 100.0);
        out.push_str(&format!(
            "      <region xml:id=\"at{i}\" tts:origin=\"{ox:.3}% {oy:.3}%\" tts:extent=\"{w:.3}% {h:.3}%\" tts:displayAlign=\"center\" tts:textAlign=\"center\"/>\n"
        ));
    }
    out.push_str("    </layout>\n  </head>\n  <body style=\"s0\">\n    <div>\n");
    for s in subs {
        let region = match region_of(s.placement) {
            Some(at) => format!("at{}", placed.iter().position(|q| (q.0 - at.0).abs() < 1e-4 && (q.1 - at.1).abs() < 1e-4).unwrap_or(0)),
            None => match s.placement.align {
                Some(7..=9) => "top".to_string(),
                Some(4..=6) => "middle".to_string(),
                _ => "bottom".to_string(),
            },
        };
        let align = match s.placement.align.map(|a| a % 3) {
            Some(1) => " tts:textAlign=\"left\"",
            Some(0) => " tts:textAlign=\"right\"",
            _ => "",
        };
        out.push_str(&format!(
            "      <p begin=\"{}\" end=\"{}\" region=\"{region}\"{align}>",
            format_ttml_time(s.start_secs),
            format_ttml_time(s.end_secs)
        ));
        for (seg, st) in crate::styled::segments(&s.text, &s.spans) {
            let body = seg.split('\n').map(escape).collect::<Vec<_>>().join("<br/>");
            if st.is_plain() {
                out.push_str(&body);
                continue;
            }
            let mut attrs = String::new();
            if st.italic {
                attrs.push_str(" tts:fontStyle=\"italic\"");
            }
            if st.bold {
                attrs.push_str(" tts:fontWeight=\"bold\"");
            }
            if st.underline {
                attrs.push_str(" tts:textDecoration=\"underline\"");
            }
            if let Some(c) = st.color {
                attrs.push_str(&format!(" tts:color=\"#{:02x}{:02x}{:02x}\"", c[0], c[1], c[2]));
            }
            out.push_str(&format!("<span{attrs}>{body}</span>"));
        }
        out.push_str("</p>\n");
    }
    out.push_str("    </div>\n  </body>\n</tt>\n");
    out
}

/// The document's timing parameters: frames, sub-frames and ticks a
/// second, for the times given in them.
#[derive(Clone, Copy)]
struct Clock {
    frame_rate: f64,
    sub_frame_rate: f64,
    tick_rate: f64,
}

/// A TTML time expression in seconds: clock time (`HH:MM:SS`, `.fff`
/// or `:FF` frames, `.SF` sub-frames) or an offset (`12.5s`, `500ms`,
/// `2m`, `1h`, `30f` frames, `900t` ticks).
fn parse_time(s: &str, clock: Clock) -> Option<f64> {
    let s = s.trim();
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        let h: f64 = parts.first()?.parse().ok()?;
        let m: f64 = parts.get(1)?.parse().ok()?;
        let sec: f64 = parts.get(2)?.parse().ok()?;
        let frames = match parts.get(3) {
            Some(f) => {
                let (f, sub) = f.split_once('.').unwrap_or((f, "0"));
                let (f, sub): (f64, f64) = (f.parse().ok()?, sub.parse().ok()?);
                (f + sub / clock.sub_frame_rate) / clock.frame_rate
            }
            None => 0.0,
        };
        return Some(h * 3600.0 + m * 60.0 + sec + frames);
    }
    let unit_at = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (n, unit) = s.split_at(unit_at);
    let n: f64 = n.parse().ok()?;
    Some(match unit {
        "h" => n * 3600.0,
        "m" => n * 60.0,
        "s" => n,
        "ms" => n / 1000.0,
        "f" => n / clock.frame_rate,
        "t" => n / clock.tick_rate,
        _ => return None,
    })
}

/// A TTML colour as RGB: `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(…)`,
/// `rgba(…)` or one of its named colours.
fn parse_colour(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(hex) = s.strip_prefix('#') {
        let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
        return match hex.len() {
            3 | 4 => {
                let d = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
                Some([d(0)?, d(1)?, d(2)?])
            }
            6 | 8 => Some([byte(0)?, byte(2)?, byte(4)?]),
            _ => None,
        };
    }
    if let Some(args) = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb(")).and_then(|a| a.strip_suffix(')')) {
        let v: Vec<u8> = args.split(',').filter_map(|n| n.trim().parse().ok()).collect();
        return (v.len() >= 3).then(|| [v[0], v[1], v[2]]);
    }
    Some(match s.as_str() {
        "white" => [255, 255, 255],
        "black" => [0, 0, 0],
        "red" => [255, 0, 0],
        "lime" => [0, 255, 0],
        "green" => [0, 128, 0],
        "blue" => [0, 0, 255],
        "yellow" => [255, 255, 0],
        "cyan" | "aqua" => [0, 255, 255],
        "magenta" | "fuchsia" => [255, 0, 255],
        "gray" | "grey" => [128, 128, 128],
        "silver" => [192, 192, 192],
        "maroon" => [128, 0, 0],
        "olive" => [128, 128, 0],
        "navy" => [0, 0, 128],
        "purple" => [128, 0, 128],
        "teal" => [0, 128, 128],
        _ => return None,
    })
}

/// The styling attributes a node carries, by local name: TTML's, or
/// DFXP's draft namespace's.
fn styling<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name && a.namespace().is_some_and(|ns| ns == TTS || TTS_DRAFTS.contains(&ns))).map(|a| a.value())
}

/// A style as the attributes that style text: what a `style` element, or
/// an element's own, says.
#[derive(Clone, Copy, Default)]
struct TextStyle {
    italic: Option<bool>,
    bold: Option<bool>,
    underline: Option<bool>,
    color: Option<[u8; 3]>,
}

impl TextStyle {
    fn of(n: roxmltree::Node) -> Self {
        Self {
            italic: styling(n, "fontStyle").map(|v| v == "italic" || v == "oblique"),
            bold: styling(n, "fontWeight").map(|v| v == "bold"),
            underline: styling(n, "textDecoration").map(|v| v.split_whitespace().any(|w| w == "underline")),
            color: styling(n, "color").and_then(parse_colour),
        }
    }

    /// `self` over `under`: what `self` sets, else what `under` does.
    fn over(self, under: Self) -> Self {
        Self { italic: self.italic.or(under.italic), bold: self.bold.or(under.bold), underline: self.underline.or(under.underline), color: self.color.or(under.color) }
    }
}

/// A document's named styles, each with the styles it refers to applied.
struct Styles(std::collections::HashMap<String, TextStyle>);

impl Styles {
    fn read(doc: &roxmltree::Document) -> Self {
        let id_of = |n: roxmltree::Node| n.attributes().find(|a| a.name() == "id").map(|a| a.value().to_string());
        let nodes: Vec<roxmltree::Node> = doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "style").collect();
        let mut map = std::collections::HashMap::new();
        // Referential styles: resolved in a few passes, since a style may
        // refer to one defined after it.
        for _ in 0..4 {
            for n in &nodes {
                let Some(id) = id_of(*n) else { continue };
                let mut st = TextStyle::of(*n);
                for r in n.attribute("style").unwrap_or("").split_whitespace() {
                    if let Some(under) = map.get(r) {
                        st = st.over(*under);
                    }
                }
                map.insert(id, st);
            }
        }
        Self(map)
    }

    /// An element's own style over the styles it names.
    fn of(&self, n: roxmltree::Node) -> TextStyle {
        let mut st = TextStyle::of(n);
        for r in n.attribute("style").unwrap_or("").split_whitespace() {
            if let Some(named) = self.0.get(r) {
                st = st.over(*named);
            }
        }
        st
    }
}

/// A region's place on the frame: the centre of its area, as fractions,
/// when given in percent, and its text alignment.
fn region_placement<'a>(n: roxmltree::Node<'a, 'a>) -> (Option<(f32, f32)>, Option<&'a str>) {
    let pair = |v: Option<&str>| -> Option<(f32, f32)> {
        let v: Vec<f32> = v?.split_whitespace().filter_map(|p| p.strip_suffix('%')?.parse().ok()).collect();
        (v.len() == 2).then(|| (v[0] / 100.0, v[1] / 100.0))
    };
    let at = match (pair(styling(n, "origin")), pair(styling(n, "extent"))) {
        (Some((ox, oy)), Some((ex, ey))) => Some((ox + ex / 2.0, oy + ey / 2.0)),
        _ => None,
    };
    (at, styling(n, "textAlign"))
}

/// The language a TTML document's root names in `xml:lang`, as a
/// lowercase tag; `None` when it isn't TTML or names none (#2223).
pub fn ttml_language(content: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(content).ok()?;
    let root = doc.root_element();
    (root.tag_name().name() == "tt").then_some(())?;
    root.attribute((roxmltree::NS_XML_URI, "lang")).map(|l| l.trim().to_ascii_lowercase()).filter(|l| !l.is_empty())
}

/// Cues from a TTML document of any of the family (TTML 1 and 2, DFXP,
/// IMSC, iTT): each `p`'s timing (its own, offset by the `body`'s and the
/// `div`s' it is in), its text with its `br`s as line breaks, its spans'
/// italics, bold, underline and colour (named styles followed), and the
/// region it sits in when that is not the frame's bottom band.
pub fn parse_ttml(content: &str) -> Result<Vec<SubtitleEntry>, String> {
    let doc = roxmltree::Document::parse(content).map_err(|e| format!("not a TTML document: {e}"))?;
    let root = doc.root_element();
    let ns_ok = |n: roxmltree::Node| n.tag_name().namespace().is_some_and(|ns| ns == TT || TT_DRAFTS.contains(&ns));
    if root.tag_name().name() != "tt" || !ns_ok(root) {
        return Err("not a TTML document: its root is not a TTML <tt>".into());
    }
    let param = |name: &str| root.attributes().find(|a| a.name() == name && a.namespace().is_some_and(|ns| ns.contains("ttml#parameter") || ns.contains("ttaf1#parameter"))).map(|a| a.value());
    let mut frame_rate: f64 = param("frameRate").and_then(|v| v.parse().ok()).unwrap_or(30.0);
    if let Some((n, d)) = param("frameRateMultiplier").and_then(|m| m.split_once(' ')) {
        if let (Ok(n), Ok(d)) = (n.trim().parse::<f64>(), d.trim().parse::<f64>()) {
            if d > 0.0 {
                frame_rate *= n / d;
            }
        }
    }
    let sub_frame_rate = param("subFrameRate").and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let tick_rate = param("tickRate").and_then(|v| v.parse().ok()).unwrap_or(if param("frameRate").is_some() { frame_rate } else { 1.0 });
    let clock = Clock { frame_rate, sub_frame_rate, tick_rate };
    let styles = Styles::read(&doc);
    let regions: std::collections::HashMap<&str, (Option<(f32, f32)>, Option<&str>)> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "region")
        .filter_map(|n| Some((n.attributes().find(|a| a.name() == "id")?.value(), region_placement(n))))
        .collect();

    let mut subs = Vec::new();
    for p in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "p" && ns_ok(*n)) {
        // The time the p's begin counts from: its ancestors' begins.
        let mut offset = 0.0;
        let mut inherited = TextStyle::default();
        let mut region = p.attribute("region");
        for a in p.ancestors().skip(1).filter(|a| a.is_element()) {
            if let Some(b) = a.attribute("begin").and_then(|b| parse_time(b, clock)) {
                offset += b;
            }
            inherited = inherited.over(styles.of(a));
            region = region.or(a.attribute("region"));
        }
        let begin = p.attribute("begin").and_then(|b| parse_time(b, clock));
        let end = p.attribute("end").and_then(|e| parse_time(e, clock));
        let dur = p.attribute("dur").and_then(|d| parse_time(d, clock));
        let (Some(begin), Some(end)) = (begin, end.or(begin.zip(dur).map(|(b, d)| b + d))) else { continue };
        let base = styles.of(p).over(inherited);
        let mut text = String::new();
        let mut spans: Vec<Span> = Vec::new();
        collect_text(p, base, &styles, &mut text, &mut spans);
        // Spans past the trimmed text's start shift with it.
        let lead = text.len() - text.trim_start_matches([' ', '\n']).len();
        let text = text.trim_matches([' ', '\n']).to_string();
        if text.is_empty() || end <= begin {
            continue;
        }
        let spans = spans
            .into_iter()
            .filter_map(|s| {
                let (start, end) = (s.start.saturating_sub(lead), s.end.saturating_sub(lead).min(text.len()));
                (start < end).then_some(Span { start, end, style: s.style })
            })
            .collect();
        let (pos, region_align) = region.and_then(|r| regions.get(r).copied()).unwrap_or((None, None));
        // A region across the frame's top, middle or bottom band is that
        // row, not a place of its own: the bottom is where a cue sits anyway.
        let (pos, row) = match pos {
            Some((x, y)) if (x - 0.5).abs() <= 0.02 && y >= 0.75 => (None, 0),
            Some((x, y)) if (x - 0.5).abs() <= 0.02 && y <= 0.25 => (None, 6),
            Some((x, y)) if (x - 0.5).abs() <= 0.02 && (0.4..=0.6).contains(&y) => (None, 3),
            Some((x, y)) => (Some((x, y)), if y < 1.0 / 3.0 { 6 } else if y < 2.0 / 3.0 { 3 } else { 0 }),
            None => (None, 0),
        };
        let side = styling(p, "textAlign").or(region_align).and_then(|a| match a {
            "left" | "start" => Some(1),
            "right" | "end" => Some(3),
            _ => None,
        });
        let align = match (side, row, pos) {
            (None, 0, _) | (None, _, Some(_)) => None,
            (side, row, _) => Some(side.unwrap_or(2) + row),
        };
        subs.push(SubtitleEntry { start_secs: offset + begin, end_secs: offset + end, text, spans, placement: Placement { pos, align } });
    }
    Ok(subs)
}

/// A p's text and its styled runs: text as it is, whitespace runs folded
/// to a space as TTML's default `xml:space` does, `br` a line break.
fn collect_text(n: roxmltree::Node, style: TextStyle, styles: &Styles, text: &mut String, spans: &mut Vec<Span>) {
    for c in n.children() {
        if c.is_text() {
            let raw = c.text().unwrap_or("");
            let mut folded = String::new();
            for c in raw.chars() {
                if !c.is_whitespace() {
                    folded.push(c);
                } else if !(folded.ends_with(' ') || folded.is_empty() && (text.is_empty() || text.ends_with([' ', '\n']))) {
                    folded.push(' ');
                }
            }
            let start = text.len();
            text.push_str(&folded);
            let st = SpanStyle { italic: style.italic.unwrap_or(false), bold: style.bold.unwrap_or(false), underline: style.underline.unwrap_or(false), color: style.color.filter(|c| *c != [255, 255, 255]) };
            if !st.is_plain() && text.len() > start {
                match spans.last_mut() {
                    Some(last) if last.style == st && last.end == start => last.end = text.len(),
                    _ => spans.push(Span { start, end: text.len(), style: st }),
                }
            }
        } else if c.is_element() {
            match c.tag_name().name() {
                "br" => {
                    while text.ends_with(' ') {
                        text.pop();
                    }
                    text.push('\n');
                }
                "span" => collect_text(c, styles.of(c).over(style), styles, text, spans),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(spans: &[Span], i: usize) -> SpanStyle {
        spans.iter().find(|s| s.start <= i && i < s.end).map(|s| s.style).unwrap_or_default()
    }

    fn cue(start: f64, end: f64, text: &str) -> SubtitleEntry {
        SubtitleEntry { start_secs: start, end_secs: end, text: text.into(), ..Default::default() }
    }

    /// What we write reads back as the same cues: timing to the
    /// millisecond, line breaks, every inline style, escaped text, and a
    /// cue's placement and alignment.
    #[test]
    fn an_imsc_document_reads_back_as_the_cues_written() {
        let mut styled_cue = cue(1.0, 3.5, "She said no, twice.\nUnder gold");
        let it = SpanStyle { italic: true, ..Default::default() };
        let b = SpanStyle { bold: true, ..Default::default() };
        let u = SpanStyle { underline: true, ..Default::default() };
        let gold = SpanStyle { color: Some([255, 204, 0]), ..Default::default() };
        styled_cue.spans = vec![
            Span { start: 9, end: 11, style: it },
            Span { start: 13, end: 18, style: b },
            Span { start: 20, end: 25, style: u },
            Span { start: 26, end: 30, style: gold },
        ];
        let mut top = cue(4.0, 5.0, "Up here");
        top.placement = Placement { pos: None, align: Some(8) };
        let mut placed = cue(5.0, 6.25, "A <sign> & a \"quote\"");
        placed.placement = Placement { pos: Some((0.25, 0.1)), align: Some(7) };
        let mut left = cue(3661.042, 3662.0, "Left");
        left.placement = Placement { pos: None, align: Some(1) };
        let subs = vec![styled_cue, top, placed, left];

        let doc = generate_imsc(&subs, "en-GB");
        assert!(doc.contains(IMSC1_1_TEXT) && doc.contains("xml:lang=\"en-GB\""), "{doc}");
        assert!(doc.contains("01:01:01.042"), "{doc}");
        let back = parse_ttml(&doc).unwrap();
        assert_eq!(back.len(), subs.len());
        for (w, r) in subs.iter().zip(&back) {
            assert!((w.start_secs - r.start_secs).abs() < 1e-6 && (w.end_secs - r.end_secs).abs() < 1e-6, "{w:?} vs {r:?}");
            assert_eq!(w.text, r.text);
            assert_eq!(w.spans, r.spans, "{}", w.text);
            assert_eq!(w.placement.align, r.placement.align, "{}", w.text);
            match (w.placement.pos, r.placement.pos) {
                (Some(a), Some(b)) => assert!((a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4, "{a:?} vs {b:?}"),
                (a, b) => assert_eq!(a, b),
            }
        }
        // Spans read by position too.
        assert_eq!(styled(&back[0].spans, 9), it);
        assert!(styled(&back[0].spans, 0).is_plain());
    }

    /// A DFXP file in the drafts' namespace, as broadcasters still deliver
    /// them: frame times at a 1000/1001 rate, styles by reference and
    /// chained, a `div` offset, `dur` instead of `end`, nested spans,
    /// indentation inside a `p`, and named colours.
    #[test]
    fn a_dfxp_file_with_frames_styles_and_offsets_reads() {
        let dfxp = r##"<?xml version="1.0" encoding="utf-8"?>
<tt xmlns="http://www.w3.org/2006/10/ttaf1" xmlns:tts="http://www.w3.org/2006/10/ttaf1#styling"
    xmlns:ttp="http://www.w3.org/2006/10/ttaf1#parameter" ttp:frameRate="30" ttp:frameRateMultiplier="1000 1001" xml:lang="en">
  <head>
    <styling>
      <style xml:id="base" tts:color="white" tts:fontFamily="Arial"/>
      <style xml:id="emph" style="base" tts:fontStyle="italic"/>
      <style xml:id="loud" tts:fontWeight="bold" tts:color="yellow"/>
    </styling>
    <layout>
      <region xml:id="r1" tts:origin="10% 80%" tts:extent="80% 15%"/>
      <region xml:id="r2" tts:origin="0% 0%" tts:extent="40% 20%" tts:textAlign="left"/>
    </layout>
  </head>
  <body style="base" region="r1">
    <div begin="00:00:10.000">
      <p begin="00:00:01:15" end="00:00:03:00">
        First line<br/>
        <span style="emph">second</span> line
      </p>
      <p begin="120f" dur="2s" region="r2"><span style="loud">LOUD <span tts:textDecoration="underline">under</span></span></p>
    </div>
  </body>
</tt>"##;
        let subs = parse_ttml(dfxp).unwrap();
        assert_eq!(subs.len(), 2);
        let fps = 30.0 * 1000.0 / 1001.0;
        assert!((subs[0].start_secs - (10.0 + 1.0 + 15.0 / fps)).abs() < 1e-9, "{}", subs[0].start_secs);
        assert!((subs[0].end_secs - (13.0)).abs() < 1e-9);
        assert_eq!(subs[0].text, "First line\nsecond line");
        assert_eq!(styled(&subs[0].spans, 11), SpanStyle { italic: true, ..Default::default() });
        assert!(styled(&subs[0].spans, 0).is_plain(), "white is the cue's own colour");
        assert_eq!(subs[0].placement, Placement::default());

        assert!((subs[1].start_secs - (10.0 + 120.0 / fps)).abs() < 1e-9);
        assert!((subs[1].end_secs - subs[1].start_secs - 2.0).abs() < 1e-9);
        assert_eq!(subs[1].text, "LOUD under");
        let loud = SpanStyle { bold: true, color: Some([255, 255, 0]), ..Default::default() };
        assert_eq!(styled(&subs[1].spans, 0), loud);
        assert_eq!(styled(&subs[1].spans, 6), SpanStyle { underline: true, ..loud });
        assert_eq!(subs[1].placement, Placement { pos: Some((0.2, 0.1)), align: Some(7) });
    }

    /// Offsets in ticks, milliseconds and hours, rgb() colours, and an
    /// iTT-style top region read; a document that is not TTML is refused.
    #[test]
    fn offset_times_and_other_documents() {
        let doc = r##"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" ttp:tickRate="10000000">
  <head><layout><region xml:id="top" tts:origin="0% 0%" tts:extent="100% 15%"/></layout></head>
  <body><div>
    <p begin="10000000t" end="25000000t">Ticks</p>
    <p begin="2500ms" end="0.001h" region="top"><span tts:color="rgb(0, 128, 255)">Blue</span></p>
    <p begin="5s" end="4s">Backwards</p>
    <p begin="6s" end="7s">   </p>
  </div></body>
</tt>"##;
        let subs = parse_ttml(doc).unwrap();
        assert_eq!(subs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Ticks", "Blue"]);
        assert!((subs[0].start_secs - 1.0).abs() < 1e-9 && (subs[0].end_secs - 2.5).abs() < 1e-9);
        assert!((subs[1].end_secs - 3.6).abs() < 1e-9);
        assert_eq!(subs[1].spans[0].style.color, Some([0, 128, 255]));
        assert_eq!(subs[1].placement, Placement { pos: None, align: Some(8) });

        assert!(parse_ttml("<html><body/></html>").is_err());
        assert!(parse_ttml("not xml").is_err());
    }
}
