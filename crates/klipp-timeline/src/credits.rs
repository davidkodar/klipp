//! Rolling credits built from a list (#1514): sections, and roles with
//! their names, read from plain CSV or tab-separated text and laid out as
//! a title that scrolls them up through the frame.
//!
//! Two aligned columns, or one centred: each column is a text layer of
//! its own (roles right-aligned against the gap, names left-aligned from
//! it, headings centred over both), all at one size and line spacing and
//! all with the same number of lines, a blank row held by a no-break
//! space. So row n of every layer sits at one height, and the layers,
//! being as tall as each other, scroll as one. A speed of 0 carries the
//! credits from below the frame to above it over the clip: they fit the
//! duration the clip is given.

use crate::{default_text_transform, FontStyle, LayerFx, TextAlignment, TextBox, TextCase, TitleData, TitleObjectContent, TitlePosition};
use klipp_core::title_motion::{ScrollDirection, TitleLoop};

/// One line of a credits list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreditLine {
    /// A section's heading: a line with one field.
    Heading(String),
    /// A role and who held it; a line with more fields names more people.
    Entry { role: String, name: String },
    /// A blank line: space between sections.
    Gap,
}

/// How the credits are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreditsLayout {
    /// Roles right-aligned in a left column, names left-aligned in a right.
    #[default]
    Columns,
    /// Each role above its name, centred.
    Centred,
}

/// How the credits look and move.
#[derive(Debug, Clone, PartialEq)]
pub struct CreditsStyle {
    pub layout: CreditsLayout,
    pub font: String,
    pub size: f32,
    pub role_colour: [u8; 4],
    pub name_colour: [u8; 4],
    pub heading_colour: [u8; 4],
    /// Pixels a second; 0 fits the clip's duration.
    pub speed_px_per_sec: f32,
}

impl Default for CreditsStyle {
    fn default() -> Self {
        Self {
            layout: CreditsLayout::Columns,
            font: "Inter".into(),
            size: 36.0,
            role_colour: [190, 190, 190, 255],
            name_colour: [255, 255, 255, 255],
            heading_colour: [255, 210, 120, 255],
            speed_px_per_sec: 0.0,
        }
    }
}

/// A line split into its fields: tab-separated when it has a tab, else
/// comma-separated with CSV quoting (`"Smith, Jr."`, `""` for a quote).
fn fields(line: &str) -> Vec<String> {
    if line.contains('\t') {
        return line.split('\t').map(|f| f.trim().to_string()).collect();
    }
    let mut out = Vec::new();
    let (mut field, mut quoted, mut chars) = (String::new(), false, line.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field).trim().to_string()),
            c => field.push(c),
        }
    }
    out.push(field.trim().to_string());
    out
}

/// The credits a CSV or tab-separated list holds, a line each: one field
/// is a section heading, two or more a role and its names, a blank line a
/// gap. Gaps at the start and end, and runs of them, come to one or none.
pub fn parse_credits(text: &str) -> Vec<CreditLine> {
    let mut out: Vec<CreditLine> = Vec::new();
    for line in text.lines() {
        let f: Vec<String> = fields(line).into_iter().filter(|f| !f.is_empty()).collect();
        let next = match f.as_slice() {
            [] => CreditLine::Gap,
            [one] => CreditLine::Heading(one.clone()),
            [role, names @ ..] => CreditLine::Entry { role: role.clone(), name: names.join(", ") },
        };
        if next == CreditLine::Gap && matches!(out.last(), None | Some(CreditLine::Gap)) {
            continue;
        }
        out.push(next);
    }
    while out.last() == Some(&CreditLine::Gap) {
        out.pop();
    }
    out
}

/// The line spacing every credits layer shares: the font's own, so each
/// layer's rows are its font's line height apart.
const LINE_SPACING: f32 = 1.0;

/// A blank row's text: a no-break space keeps the line, and its height.
const BLANK: &str = "\u{00a0}";

/// The rows of each layer, role, name and heading, one per line of the
/// credits, blank where the layer has nothing there. Laid out centred, a
/// role takes its own row above its name.
pub fn credit_rows(lines: &[CreditLine], layout: CreditsLayout) -> [Vec<String>; 3] {
    let (mut roles, mut names, mut headings) = (Vec::new(), Vec::new(), Vec::new());
    let mut row = |r: &str, n: &str, h: &str| {
        roles.push(if r.is_empty() { BLANK.to_string() } else { r.to_string() });
        names.push(if n.is_empty() { BLANK.to_string() } else { n.to_string() });
        headings.push(if h.is_empty() { BLANK.to_string() } else { h.to_string() });
    };
    for line in lines {
        match (line, layout) {
            (CreditLine::Heading(h), _) => row("", "", h),
            (CreditLine::Gap, _) => row("", "", ""),
            (CreditLine::Entry { role, name }, CreditsLayout::Columns) => row(role, name, ""),
            (CreditLine::Entry { role, name }, CreditsLayout::Centred) => {
                row(role, "", "");
                row("", name, "");
            }
        }
    }
    [roles, names, headings]
}

/// The credits as a title: a layer per column, scrolling up together.
pub fn credits_title(lines: &[CreditLine], style: &CreditsStyle) -> TitleData {
    let [roles, names, headings] = credit_rows(lines, style.layout);
    // A box tall enough for every line: the renderer shrinks type to fit
    // its box, which would shrink a long list to one frame. A line is at
    // most 1.6 times the size, in a 1080-line frame's units; the renderer
    // scales type by the frame's short side, so this is enough for any
    // frame. Every layer gets the same box, so their rows stay together.
    const LINE_BOUND: f32 = 1.6;
    let box_h = (roles.len().max(1) as f32 * style.size * LINE_SPACING * LINE_BOUND / 1080.0).max(1.0);
    let (role_x, role_anchor, role_align, name_x, name_anchor, name_align) = match style.layout {
        CreditsLayout::Columns => (0.48, 1.0, TextAlignment::Right, 0.52, 0.0, TextAlignment::Left),
        CreditsLayout::Centred => (0.5, 0.5, TextAlignment::Center, 0.5, 0.5, TextAlignment::Center),
    };
    let text = |rows: Vec<String>, colour: [u8; 4], align: TextAlignment, heading: bool| TextBox {
        text: rows.join("\n"),
        font_size: style.size,
        font_name: style.font.clone(),
        font_style: if heading { FontStyle::Bold } else { FontStyle::Regular },
        color: colour,
        alignment: align,
        case_transform: if heading { TextCase::Uppercase } else { TextCase::None },
        line_spacing: LINE_SPACING,
        ..Default::default()
    };
    let scroll = LayerFx { loops: vec![TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: style.speed_px_per_sec, shape: Default::default() }], ..Default::default() };
    let mut td = TitleData::single_text(TextBox::default());
    td.objects.clear();
    for (rows, colour, x, anchor, align, heading, name) in [
        (roles, style.role_colour, role_x, role_anchor, role_align, false, "Roles"),
        (names, style.name_colour, name_x, name_anchor, name_align, false, "Names"),
        (headings, style.heading_colour, 0.5, 0.5, TextAlignment::Center, true, "Headings"),
    ] {
        let mut at = default_text_transform();
        at.center = TitlePosition { x, y: 0.5 };
        at.anchor_norm = TitlePosition { x: anchor, y: 0.5 };
        at.size = TitlePosition { x: 0.45, y: box_h };
        let id = td.add_object(TitleObjectContent::TextBox(text(rows, colour, align, heading)), at);
        let o = td.object_mut(id).expect("just added");
        o.name = name.to_string();
        o.fx = Some(scroll.clone());
    }
    td
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A heading is one field, an entry two or more (CSV quoting kept, a
    /// tab splitting before commas), a blank line a gap; gaps never lead,
    /// trail or repeat.
    #[test]
    fn a_list_reads_as_headings_entries_and_gaps() {
        let text = "\n\nCAST\nJane Doe,\"Smith, Jr.\"\n\n\n\"Said \"\"Hi\"\"\"\tSam\nCREW\nDirector,Ana,Bo\n\n";
        assert_eq!(parse_credits(text), [
            CreditLine::Heading("CAST".into()),
            CreditLine::Entry { role: "Jane Doe".into(), name: "Smith, Jr.".into() },
            CreditLine::Gap,
            CreditLine::Entry { role: "\"Said \"\"Hi\"\"\"".into(), name: "Sam".into() },
            CreditLine::Heading("CREW".into()),
            CreditLine::Entry { role: "Director".into(), name: "Ana, Bo".into() },
        ]);
    }

    /// Every layer has a row for every line, blank (but kept) where it has
    /// nothing, so row n is at one height in all three: in columns, a
    /// role and its name share a row; centred, the name is the row below.
    #[test]
    fn each_layer_has_a_row_for_every_line() {
        let lines = parse_credits("CAST\nJane,Doe\n\nCREW\nEditor,Ed");
        let [r, n, h] = credit_rows(&lines, CreditsLayout::Columns);
        assert_eq!((r.len(), n.len(), h.len()), (5, 5, 5));
        assert_eq!((r[1].as_str(), n[1].as_str(), h[1].as_str()), ("Jane", "Doe", BLANK));
        assert_eq!((r[0].as_str(), h[0].as_str()), (BLANK, "CAST"));
        let [r, n, _] = credit_rows(&lines, CreditsLayout::Centred);
        assert_eq!(r.len(), 7);
        assert_eq!((r[1].as_str(), n[1].as_str(), n[2].as_str()), ("Jane", BLANK, "Doe"));
        let td = credits_title(&lines, &CreditsStyle::default());
        let counts: Vec<usize> = td.objects.iter().map(|o| match &o.content {
            TitleObjectContent::TextBox(tb) => tb.text.lines().count(),
            _ => 0,
        }).collect();
        assert_eq!(counts, [5, 5, 5], "as tall as each other, so they scroll as one");
        assert!(td.objects.iter().all(|o| o.fx().loops == vec![TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0, shape: Default::default() }]), "fit to the clip");
    }
}
