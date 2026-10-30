//! The built-in title library (#1724): the curated templates the Titles
//! browser shows by category and the template op places by name, each
//! designed on the bundled faces and the catalogue's (#2732), placed
//! inside title-safe, laid out for the frame's format, moved by the
//! motion model, and keeping a few fields editable once applied.
//!
//! The templates are files, `assets/titles/*.klipptitle` (#2886), each
//! the layers as designed with how they are laid out and published; this
//! module makes the title from them. Here, beside the title model, so the
//! app and the control socket name and build one set.

use crate::{
    default_text_transform, Easing, FontStyle, Motion, MotionKind, ObjectTransform, Shape, ShapeKind,
    TextAlignment, TitleAnimationType, TitleBackground, TitleData, TitleMotion, TitleObjectContent,
    TitlePosition, TitleShadow,
};

pub struct TitleTemplate {
    pub name: String,
    pub category: String,
    pub preview_text: String,
    /// Its layers as designed on a wide frame, before they are named and
    /// published.
    pub design: TitleData,
    /// What it keeps editable once applied (#1724).
    pub fields: Fields,
    /// Whether, centred, it grows into the room a tall or square frame
    /// has across it.
    pub grow: bool,
}

/// A built-in's published fields (#1724), simple first: each text by
/// its name, in the order its text layers stack; the accent's colour,
/// when it has one (the layer at `accent`); then Size, grown from
/// `anchor` (a point of the frame), and Speed when anything moves.
pub struct Fields {
    pub names: Vec<String>,
    pub accent: Option<usize>,
    pub anchor: (f32, f32),
}

impl TitleTemplate {
    /// The title the template makes, its layers named and its fields
    /// published (#1724), keeping its text readable over the picture
    /// (#2734), laid out for a wide frame.
    pub fn build(&self) -> TitleData {
        self.build_for(1920, 1080)
    }

    /// [`Self::build`] laid out for a `width` × `height` frame (#2734):
    /// see [`Format`].
    pub fn build_for(&self, width: u32, height: u32) -> TitleData {
        let (td, anchor) = self.laid_out(width, height);
        let mut td = publish(td, &self.fields, anchor);
        // A template with a backing of its own reads on it; a halo round
        // the backing would only smudge the picture.
        td.protect = if backed(&td) { crate::ContrastProtect::Off } else { crate::ContrastProtect::Auto };
        td
    }

    /// The template's layers for a `width` × `height` frame, and the
    /// point its Size grows from there. A wide frame takes them as they
    /// were designed. Elsewhere each layer keeps its distance from the
    /// template's anchor in frame heights of the design, so the layout
    /// is the same shape at the same type size, not stretched with the
    /// frame; the anchor moves by [`Format::place`]; a centred opener
    /// grows into the room a tall or square frame has across it; and
    /// each text is wrapped inside title-safe, and on a tall frame, low
    /// down, clear of the strip the platforms' buttons cover.
    fn laid_out(&self, width: u32, height: u32) -> (TitleData, (f32, f32)) {
        let mut td = self.design.clone();
        let a = self.fields.anchor;
        let format = Format::of(width, height);
        if format == Format::Wide {
            return (td, a);
        }
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        let short = w.min(h);
        let (sx, sy) = (DESIGN_ASPECT * short / w, short / h);
        let centred = (a.0 - 0.5).abs() < 0.05 && (a.1 - 0.5).abs() < 0.05;
        let grow = match (centred && self.grow, format) {
            (true, Format::Tall) => 1.25,
            (true, _) => 1.1,
            _ => 1.0,
        };
        let anchor = format.place(a);
        // How far right a layer at `y` may reach.
        let right = |y: f32| if format == Format::Tall && y > 0.5 { TALL_STRIP_LEFT } else { 1.0 - SAFE };
        let map_x = |x: f32| anchor.0 + (x - a.0) * sx * grow;
        let map_y = |y: f32| anchor.1 + (y - a.1) * sy * grow;
        for o in &mut td.objects {
            let t = &mut o.transform;
            let text = matches!(o.content, TitleObjectContent::TextBox(_));
            // A frame-sized shape stays the frame's; a text's box is where
            // its words start, whatever its size.
            if text || t.size.x < FULL {
                t.center.x = map_x(t.center.x);
                t.center_x_keyframes.iter_mut().for_each(|k| k.value = map_x(k.value));
            }
            if text || t.size.y < FULL {
                t.center.y = map_y(t.center.y);
                t.center_y_keyframes.iter_mut().for_each(|k| k.value = map_y(k.value));
            }
            match &mut o.content {
                TitleObjectContent::TextBox(tb) => {
                    tb.font_size *= grow;
                    let ax = t.anchor_norm.x.clamp(0.0, 1.0);
                    let room_left = if ax > 0.0 { (t.center.x - SAFE) / ax } else { f32::INFINITY };
                    // Its lowest line, about: the lines it was typed in, at
                    // the type's height in the frame.
                    let lines = tb.text.lines().count().max(1) as f32;
                    let low = t.center.y + lines * tb.font_size * 1.2 / 1080.0 * short / h / 2.0;
                    let room_right = if ax < 1.0 { (right(low) - t.center.x) / (1.0 - ax) } else { f32::INFINITY };
                    t.size.x = t.size.x.min(room_left).min(room_right).max(0.2);
                    // Lines set as lines (a slate, a cast list) keep them,
                    // smaller, rather than breaking inside one.
                    if tb.text.contains('\n') && tb.fit == crate::TextFit::Wrap {
                        tb.fit = crate::TextFit::Shrink { min_size: (tb.font_size * 0.6).round() };
                    }
                }
                TitleObjectContent::Shape(shape) if shape.fit.is_some() => {}
                _ => {
                    if t.size.x < FULL {
                        t.size.x *= sx * grow;
                    }
                    if t.size.y < FULL {
                        t.size.y *= sy * grow;
                    }
                    if t.size.x < FULL {
                        let over = t.center.x + (1.0 - t.anchor_norm.x) * t.size.x - right(t.center.y);
                        if over > 0.0 {
                            t.size.x = (t.size.x - over).max(0.01);
                        }
                    }
                }
            }
        }
        // Kept inside the frame as a whole: a text near an edge, set as
        // wide in pixels as it was designed, can reach past it on a
        // narrower frame.
        let shift = {
            let (mut lo, mut over) = (f32::INFINITY, f32::NEG_INFINITY);
            for o in &td.objects {
                let TitleObjectContent::TextBox(tb) = &o.content else { continue };
                let t = &o.transform;
                let chars = tb.text.lines().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
                let pad = td.objects.iter().find_map(|s| match &s.content {
                    TitleObjectContent::Shape(Shape { fit: Some(f), .. }) if f.target == o.id => Some(f.padding.x),
                    _ => None,
                }).unwrap_or(0.0);
                let wide = ((chars * 0.58 * tb.font_size).min(t.size.x * 1080.0 * w / short) + 2.0 * pad) / 1080.0 * short / w;
                let x0 = t.center.x - t.anchor_norm.x * wide;
                lo = lo.min(x0 - SAFE);
                over = over.max(x0 + wide - right(t.center.y));
            }
            if over > 0.0 { -over } else if lo < 0.0 { -lo } else { 0.0 }
        };
        if shift != 0.0 {
            for o in &mut td.objects {
                if o.transform.size.x < FULL || matches!(o.content, TitleObjectContent::TextBox(_)) {
                    o.transform.center.x += shift;
                }
            }
        }
        (td, anchor)
    }
}

/// Whether every text of `td` sits on a backing of its own: an opaque
/// shape fitted to it, or one its place is inside.
fn backed(td: &TitleData) -> bool {
    let backings: Vec<&crate::TitleObject> = td.objects.iter().filter(|o| matches!(&o.content, TitleObjectContent::Shape(s) if s.fill.is_some_and(|f| f[3] >= 150))).collect();
    td.objects.iter().filter(|o| matches!(o.content, TitleObjectContent::TextBox(_))).all(|text| {
        backings.iter().any(|b| {
            let TitleObjectContent::Shape(s) = &b.content else { return false };
            let (c, size, at) = (b.transform.center, b.transform.size, text.transform.center);
            s.fit.as_ref().is_some_and(|f| f.target == text.id)
                || ((at.x - c.x).abs() <= size.x / 2.0 && (at.y - c.y).abs() <= size.y / 2.0)
        })
    })
}

/// The frame a template was designed on, wide to high.
const DESIGN_ASPECT: f32 = 16.0 / 9.0;

/// Title-safe's margin, a share of the frame from each edge.
const SAFE: f32 = 0.05;

/// A layer at least this share of the frame across is frame-sized.
const FULL: f32 = 0.95;

/// The top of the band at the bottom of a tall frame that the platforms
/// cover with captions and their buttons (#2734), a share of its height.
pub const TALL_BAND_TOP: f32 = 0.80;

/// The left edge of the strip at the right of a tall frame's lower half
/// that the platforms' action buttons cover (#2734).
pub const TALL_STRIP_LEFT: f32 = 0.86;

/// The shape of the frame a template is laid out for (#2734).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// 16:9 and wider: the design.
    Wide,
    /// 9:16 and other portrait frames, for phones.
    Tall,
    /// 1:1 and near it.
    Square,
}

impl Format {
    /// The format of a `width` × `height` frame.
    pub fn of(width: u32, height: u32) -> Self {
        let aspect = width as f32 / height.max(1) as f32;
        if aspect >= 1.3 {
            Self::Wide
        } else if aspect <= 0.77 {
            Self::Tall
        } else {
            Self::Square
        }
    }

    /// Where a template anchored at `a` (a point of the frame) sits in
    /// this format. Low on a tall frame it comes up out of the band the
    /// platforms cover, and a corner mark goes to the top corner; low on
    /// a square frame it comes up a little; anything else stays.
    pub fn place(self, a: (f32, f32)) -> (f32, f32) {
        match self {
            Self::Wide => a,
            Self::Tall if a.1 > 0.88 && (a.0 < 0.2 || a.0 > 0.8) => (a.0, 1.0 - a.1),
            Self::Tall if a.1 > 0.6 => (a.0.min(TALL_STRIP_LEFT - 0.04), 0.5 + (a.1 - 0.5) * 0.62),
            Self::Square if a.1 > 0.6 => (a.0, 0.5 + (a.1 - 0.5) * 0.9),
            _ => a,
        }
    }
}

fn publish(mut td: TitleData, fields: &Fields, anchor: (f32, f32)) -> TitleData {
    use crate::{FieldKind, PublishedField};
    let texts: Vec<usize> = td.objects.iter().enumerate().filter(|(_, o)| matches!(o.content, TitleObjectContent::TextBox(_))).map(|(i, _)| i).collect();
    let mut published = Vec::new();
    for (&i, name) in texts.iter().zip(&fields.names) {
        td.objects[i].name = name.clone();
        published.push(PublishedField { label: name.clone(), kind: FieldKind::Text { object: td.objects[i].id } });
    }
    if let Some(o) = fields.accent.and_then(|a| td.objects.get(a)) {
        published.push(PublishedField { label: "Accent".into(), kind: FieldKind::Colour { object: o.id } });
    }
    let anchor = TitlePosition { x: anchor.0, y: anchor.1 };
    published.push(PublishedField { label: "Size".into(), kind: td.size_field(anchor) });
    published.extend(td.default_fields().into_iter().filter(|f| matches!(f.kind, FieldKind::Speed { .. })));
    td.published = published;
    td
}

// ---------------------------------------------------------------------------
// The design vocabulary: colours, faces and layer helpers the motion pack's
// authoring (#2757) designs in, as the built-ins were designed.
// ---------------------------------------------------------------------------

pub const WHITE: [u8; 4] = [255, 255, 255, 255];
pub const INK: [u8; 4] = [18, 18, 22, 255];
pub const MUTED: [u8; 4] = [225, 225, 230, 255];
pub const ACCENT: [u8; 4] = [74, 143, 231, 255];
pub const GRO: &str = "Inter";
pub const DISPLAY: &str = "Bebas Neue";
pub const MONO: &str = "JetBrains Mono";
// Faces from Klipp's catalogue (#2732), fetched when a title first uses
// them; the bundled ones stand in until they arrive.
pub const SANS: &str = "Manrope";
pub const LABEL: &str = "Work Sans";
pub const EDITORIAL: &str = "Playfair Display";
pub const CLASSIC: &str = "Cormorant Garamond";
pub const HEADING: &str = "DM Serif Display";
pub const POSTER: &str = "Anton";

/// A soft drop shadow that lifts text off footage without reading as
/// a shadow.
pub fn lift() -> TitleShadow {
    TitleShadow {
        color: [0, 0, 0, 120],
        offset_x: 0.0,
        offset_y: 3.0,
        blur: 12.0,
        enabled: true,
        offset_x_keyframes: Vec::new(),
        offset_y_keyframes: Vec::new(),
        blur_keyframes: Vec::new(),
    }
}

/// A text layer as a template composes it: the text box and the stack
/// its object will carry. With the helpers round it (`face`, `text_at`,
/// `rect`, `layers`, `enter`...), the vocabulary the built-ins and the
/// motion pack's authoring (#2757) design in.
pub struct Text {
    tb: crate::TextBox,
    fx: crate::LayerFx,
}

pub fn face(text: &str, family: &str, style: FontStyle, size: f32, color: [u8; 4], align: TextAlignment) -> Text {
    Text {
        tb: crate::TextBox {
            text: text.into(),
            font_size: size,
            font_name: family.into(),
            font_style: style,
            color,
            alignment: align,
            ..Default::default()
        },
        fx: crate::LayerFx::default(),
    }
}

pub fn upper(mut t: Text, tracking: f32) -> Text {
    t.tb.case_transform = crate::TextCase::Uppercase;
    t.tb.tracking = tracking;
    t
}

/// `t` at a weight of its family (100 thin to 900 black).
pub fn weight(mut t: Text, w: u16) -> Text {
    t.tb.font_weight = Some(w);
    t
}

/// A light fill from `top` to `bottom`: depth without a look of its own.
pub fn graded(mut t: Text, top: [u8; 4], bottom: [u8; 4]) -> Text {
    t.tb.gradient = Some(crate::TextGradient { top_color: top, bottom_color: bottom, enabled: true, stops: Vec::new(), kind: Default::default(), stop_keyframes: Vec::new() });
    t
}

/// `t` with its lines spaced by `spacing` (1 as set).
pub fn spaced(mut t: Text, spacing: f32) -> Text {
    t.tb.line_spacing = spacing;
    t
}

pub fn lifted(mut t: Text) -> Text {
    t.fx.shadows.push(lift());
    t
}

/// A name that keeps its line: a long one is set smaller to fit its box
/// rather than wrapping under the lines below it (#1722).
pub fn keeps_line(mut t: Text) -> Text {
    t.tb.fit = crate::TextFit::Shrink { min_size: (t.tb.font_size * 0.5).round() };
    t
}

/// [`text_at`] in a box `w` of the frame wide, for a layer that has to
/// stay inside a bar.
pub fn boxed_at(t: Text, x: f32, y: f32, w: f32) -> Layer {
    let mut layer = text_at(t, x, y, 0.0);
    layer.1.size.x = w;
    layer
}

pub fn moved(mut t: Text, motion: TitleMotion) -> Text {
    t.fx.motion = motion;
    t
}

/// Where a layer sits: `x`, `y` in 0..1 of the frame; the anchor
/// chooses which point of the layer lands there (0 = left/top).
pub fn at(x: f32, y: f32, anchor_x: f32) -> ObjectTransform {
    ObjectTransform {
        center: TitlePosition { x, y },
        anchor_norm: TitlePosition { x: anchor_x, y: 0.5 },
        ..default_text_transform()
    }
}

/// One layer of a template: what it is, where it sits, and its stack.
pub type Layer = (TitleObjectContent, ObjectTransform, crate::LayerFx);

pub fn rect(x: f32, y: f32, w: f32, h: f32, fill: [u8; 4], radius: f32) -> Layer {
    (
        TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: radius }, fit: None, fill: Some(fill), gradient: None, stroke: None }),
        ObjectTransform { center: TitlePosition { x, y }, size: TitlePosition { x: w, y: h }, ..default_text_transform() },
        crate::LayerFx::default(),
    )
}

/// Build a title from layers, back to front.
pub fn layers(items: Vec<Layer>) -> TitleData {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: TitleAnimationType::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    for (content, transform, fx) in items {
        let id = td.add_object(content, transform);
        td.object_mut(id).expect("just added").fx = Some(fx);
    }
    td
}

pub fn text_at(t: Text, x: f32, y: f32, anchor_x: f32) -> Layer {
    (TitleObjectContent::TextBox(t.tb), at(x, y, anchor_x), t.fx)
}

// ── Motion presets ──
pub fn enter(kind: MotionKind, secs: f32, easing: Easing) -> Motion {
    Motion::new(kind).duration(secs).easing(easing)
}
pub fn fade_out(secs: f32) -> Motion {
    Motion::new(MotionKind::Fade).duration(secs).easing(Easing::InCubic)
}

/// A backing shape that moves with its text (#1724): a bar that sits
/// still while the words slide in reads as pasted on.
pub fn moving(mut layer: Layer, motion: TitleMotion) -> Layer {
    layer.2.motion = motion;
    layer
}

/// Shape `shape` sized to text `text`, padded (1080p pixels): the backing
/// follows whatever is typed.
pub fn fit(td: &mut TitleData, shape: usize, text: usize, pad: (f32, f32)) {
    let target = td.objects[text].id;
    if let TitleObjectContent::Shape(s) = &mut td.objects[shape].content {
        s.fit = Some(crate::ShapeFit { target, padding: TitlePosition { x: pad.0, y: pad.1 } });
    }
}

/// A heavy outline that keeps a caption readable on any picture.
pub fn outlined(mut t: Text, width: f32) -> Text {
    t.fx.outlines.push(crate::TextOutline { enabled: true, color: [0, 0, 0, 255], width, width_keyframes: Vec::new() });
    t
}

/// Every built-in title template, in the order the browser shows them:
/// the files Klipp ships in `assets/titles` (#2886), each read and checked
/// as a saved one is.
pub static TITLES: std::sync::LazyLock<Vec<TitleTemplate>> = std::sync::LazyLock::new(|| from_files().collect());

/// The built-in title templates shipped as files (#2886).
const FILES: &[&str] = &[
    include_str!("../../../assets/titles/name_bar.klipptitle"),
    include_str!("../../../assets/titles/clean_line.klipptitle"),
    include_str!("../../../assets/titles/broadcast.klipptitle"),
    include_str!("../../../assets/titles/accent_block.klipptitle"),
    include_str!("../../../assets/titles/pill.klipptitle"),
    include_str!("../../../assets/titles/stacked.klipptitle"),
    include_str!("../../../assets/titles/documentary.klipptitle"),
    include_str!("../../../assets/titles/location.klipptitle"),
    include_str!("../../../assets/titles/cinematic.klipptitle"),
    include_str!("../../../assets/titles/impact.klipptitle"),
    include_str!("../../../assets/titles/editorial.klipptitle"),
    include_str!("../../../assets/titles/title_card.klipptitle"),
    include_str!("../../../assets/titles/minimal.klipptitle"),
    include_str!("../../../assets/titles/chapter.klipptitle"),
    include_str!("../../../assets/titles/chapter_number.klipptitle"),
    include_str!("../../../assets/titles/statement.klipptitle"),
    include_str!("../../../assets/titles/quote.klipptitle"),
    include_str!("../../../assets/titles/caption.klipptitle"),
    include_str!("../../../assets/titles/subtitle.klipptitle"),
    include_str!("../../../assets/titles/callout.klipptitle"),
    include_str!("../../../assets/titles/note.klipptitle"),
    include_str!("../../../assets/titles/credits_roll.klipptitle"),
    include_str!("../../../assets/titles/end_credits.klipptitle"),
    include_str!("../../../assets/titles/end_card.klipptitle"),
    include_str!("../../../assets/titles/cast.klipptitle"),
    include_str!("../../../assets/titles/thanks_for_watching.klipptitle"),
    include_str!("../../../assets/titles/up_next.klipptitle"),
    include_str!("../../../assets/titles/hook.klipptitle"),
    include_str!("../../../assets/titles/bold_caption.klipptitle"),
    include_str!("../../../assets/titles/highlight.klipptitle"),
    include_str!("../../../assets/titles/question.klipptitle"),
    include_str!("../../../assets/titles/tip.klipptitle"),
    include_str!("../../../assets/titles/handle.klipptitle"),
    include_str!("../../../assets/titles/subscribe.klipptitle"),
    include_str!("../../../assets/titles/slate.klipptitle"),
    include_str!("../../../assets/titles/timecode.klipptitle"),
    include_str!("../../../assets/titles/watermark.klipptitle"),
    include_str!("../../../assets/titles/label.klipptitle"),
];






fn from_files() -> impl Iterator<Item = TitleTemplate> {
    FILES.iter().filter_map(|json| {
        let read = crate::TemplateFile::parse(json).and_then(|t| match t.design {
            Some(how) => Ok(TitleTemplate::designed(&t.name, &t.category, t.data, how)),
            None => Err(format!("\"{}\" has no design", t.name)),
        });
        match read {
            Ok(t) => Some(t),
            Err(why) => {
                tracing::error!("a built-in title template doesn't read: {why}");
                debug_assert!(false, "a built-in title template doesn't read: {why}");
                None
            }
        }
    })
}


/// `td` placed by its layer `by` (#2886): that layer's centre, a point of
/// the frame as designed, put at the same point of the box `within`
/// ((left, top, width, height), fractions of the frame), or at `at`
/// for a coordinate `at` gives; every layer then moved as far as it
/// moved, so they keep their distances from it. A layer `td` hasn't got
/// leaves it as it is.
pub fn place_by(td: &mut TitleData, by: usize, at: [Option<f32>; 2], within: (f32, f32, f32, f32)) {
    let Some(c) = td.objects.get(by).map(|o| o.transform.center) else { return };
    let (left, top, width, height) = within;
    let to = (at[0].unwrap_or(left + width * c.x), at[1].unwrap_or(top + height * c.y));
    let (dx, dy) = (to.0 - c.x, to.1 - c.y);
    for o in &mut td.objects {
        let t = &mut o.transform;
        t.center.x += dx;
        t.center.y += dy;
        t.center_x_keyframes.iter_mut().for_each(|k| k.value += dx);
        t.center_y_keyframes.iter_mut().for_each(|k| k.value += dy);
    }
}

/// The titles Klipp's own ops build (#2886), by name: the layers as
/// designed and how they are made, from files in `assets/titles/ops`.
/// Not listed with the templates, and never one saved by the user.
pub fn op_title(name: &str) -> Option<&'static (TitleData, crate::TitleDesign)> {
    static OPS: std::sync::LazyLock<Vec<(String, (TitleData, crate::TitleDesign))>> = std::sync::LazyLock::new(|| {
        OP_FILES
            .iter()
            .filter_map(|json| match crate::TemplateFile::parse(json).and_then(|t| t.design.map(|d| (t.name, (t.data, d))).ok_or_else(|| "it has no design".to_string())) {
                Ok(t) => Some(t),
                Err(why) => {
                    tracing::error!("an op's title doesn't read: {why}");
                    debug_assert!(false, "an op's title doesn't read: {why}");
                    None
                }
            })
            .collect()
    });
    OPS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|(_, t)| t)
}

/// The titles of Klipp's own ops, as files (#2886).
const OP_FILES: &[&str] = &[include_str!("../../../assets/titles/ops/lower_third.klipptitle")];

/// Ordered list of categories for display.
pub const CATEGORIES: &[&str] = &["Lower Thirds", "Openers", "Captions", "Credits", "Social", "Utility"];

/// The smallest text a template makes, in pixels of a 1080-line frame
/// (#2734).
pub const MIN_TEXT_SIZE: f32 = 28.0;

/// A title just made from a template, laid out for a `width` × `height`
/// frame (#2734): a built-in made again for it, anything else (a saved
/// template, a title of its own) as it is.
pub fn for_frame(td: TitleData, width: u32, height: u32) -> TitleData {
    match td.template.as_deref().and_then(builtin) {
        Some(t) if Format::of(width, height) != Format::Wide => t.data_for(width, height),
        _ => td,
    }
}

/// The built-in template called `name`, in any case.
pub fn builtin(name: &str) -> Option<&'static TitleTemplate> {
    let name = name.trim();
    TITLES.iter().find(|t| t.name.eq_ignore_ascii_case(name))
}

impl TitleTemplate {
    /// The template a designed file holds (#2886): its layers `design`,
    /// laid out and published as `how` says, made as a built-in is.
    pub fn designed(name: &str, category: &str, design: TitleData, how: crate::TitleDesign) -> TitleTemplate {
        TitleTemplate {
            name: name.into(),
            category: category.into(),
            preview_text: how.preview,
            design,
            fields: Fields { names: how.fields, accent: how.accent, anchor: (how.anchor[0], how.anchor[1]) },
            grow: how.grow,
        }
    }

    /// The title the template makes, marked as made from it (#1716): what
    /// a drop from the browser and the template op place.
    pub fn data(&self) -> TitleData {
        self.data_for(1920, 1080)
    }

    /// [`Self::data`] laid out for a `width` × `height` frame (#2734).
    pub fn data_for(&self, width: u32, height: u32) -> TitleData {
        let mut td = self.build_for(width, height);
        td.template = Some(self.name.clone());
        // No text smaller than reads on a phone (#2734): a role line or a
        // label at 20 px of a 1080 frame was a few pixels tall there.
        for object in &mut td.objects {
            if let crate::TitleObjectContent::TextBox(tb) = &mut object.content {
                tb.font_size = tb.font_size.max(MIN_TEXT_SIZE);
            }
        }
        td
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2886: every built-in Klipp ships is listed, by name in its order:
    /// a file that doesn't read is logged and left out in a release, so
    /// this is where a dropped one fails. Each file reads and makes its
    /// title, its fields published, on each frame.
    #[test]
    fn every_built_in_file_reads_and_makes_its_title() {
        const ALL: [&str; 38] = [
            "Name Bar", "Clean Line", "Broadcast", "Accent Block", "Pill", "Stacked", "Documentary", "Location",
            "Cinematic", "Impact", "Editorial", "Title Card", "Minimal", "Chapter", "Chapter Number", "Statement",
            "Quote", "Caption", "Subtitle", "Callout", "Note",
            "Credits Roll", "End Credits", "End Card", "Cast", "Thanks for Watching", "Up Next",
            "Hook", "Bold Caption", "Highlight", "Question", "Tip", "Handle", "Subscribe",
            "Slate", "Timecode", "Watermark", "Label",
        ];
        assert_eq!(TITLES.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ALL);
        assert_eq!(from_files().count(), FILES.len(), "every file reads");
        for t in TITLES.iter().take(FILES.len()) {
            for (w, h) in [(1920, 1080), (1080, 1920), (1080, 1080)] {
                let td = t.data_for(w, h);
                let labels: Vec<&str> = td.published.iter().map(|f| f.label.as_str()).collect();
                assert!(t.fields.names.iter().all(|n| labels.contains(&n.as_str())), "{} at {w}x{h}: {labels:?}", t.name);
                assert_eq!(td.template.as_deref(), Some(t.name.as_str()));
            }
        }
    }

    #[test]
    fn a_frame_is_wide_tall_or_square() {
        assert_eq!(Format::of(1920, 1080), Format::Wide);
        assert_eq!(Format::of(1080, 1920), Format::Tall);
        assert_eq!(Format::of(1080, 1350), Format::Square);
        assert_eq!(Format::of(1080, 1080), Format::Square);
    }

    /// A template whose every text sits on a backing of its own keeps
    /// the picture as it is; one with a text on the picture protects it
    /// (#2734).
    #[test]
    fn a_backed_template_leaves_the_picture_alone() {
        let protect = |name: &str| builtin(name).unwrap().data().protect;
        for name in ["Name Bar", "Handle", "Subscribe", "Callout", "Slate", "Caption"] {
            assert_eq!(protect(name), crate::ContrastProtect::Off, "{name}");
        }
        for name in ["Pill", "Clean Line", "Impact", "Quote"] {
            assert_eq!(protect(name), crate::ContrastProtect::Auto, "{name}");
        }
    }

    /// Laid out for another frame, a template keeps the distance between
    /// its layers in pixels (#2734): a lower third's name and role are as
    /// far apart on a 9:16 frame as on a 16:9 one of the same short side.
    #[test]
    fn a_layout_keeps_its_spacing_in_pixels() {
        let t = builtin("Name Bar").unwrap();
        let gap = |w: u32, h: u32| {
            let td = t.build_for(w, h);
            let at = |name: &str| td.objects.iter().find(|o| o.name == name).unwrap().transform.center;
            let (a, b) = (at("Name"), at("Role"));
            ((b.x - a.x) * w as f32, (b.y - a.y) * h as f32)
        };
        let (wide, tall) = (gap(1920, 1080), gap(1080, 1920));
        assert!((wide.0 - tall.0).abs() < 0.5 && (wide.1 - tall.1).abs() < 0.5, "{wide:?} vs {tall:?}");
    }
}
