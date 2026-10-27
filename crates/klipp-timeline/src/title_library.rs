//! The built-in title library (#1724): the curated templates the Titles
//! browser shows by category and the template op places by name, each
//! designed on the bundled faces and the catalogue's (#2732), placed
//! inside title-safe, laid out for the frame's format, moved by the
//! motion model, and keeping a few fields editable once applied.
//!
//! Here, beside the title model, so the app and the control socket name
//! and build one set.

use crate::{
    default_text_transform, Easing, FontStyle, Motion, MotionKind, ObjectTransform, ScrollDirection, Shape, ShapeKind,
    StaggerUnit, TextAlignment, TitleAnimationType, TitleBackground, TitleData, TitleLoop, TitleMotion, TitleObjectContent,
    TitlePosition, TitleShadow,
};

pub struct TitleTemplate {
    pub name: &'static str,
    pub category: &'static str,
    pub preview_text: &'static str,
    pub create_data: fn() -> TitleData,
    /// What it keeps editable once applied (#1724).
    pub fields: Fields,
}

/// A built-in's published fields (#1724), simple first: each text by
/// its name, in the order its text layers stack; the accent's colour,
/// when it has one (the layer at `accent`); then Size, grown from
/// `anchor` (a point of the frame), and Speed when anything moves.
pub struct Fields {
    pub names: &'static [&'static str],
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
        let mut td = (self.create_data)();
        let a = self.fields.anchor;
        let format = Format::of(width, height);
        if format == Format::Wide {
            return (td, a);
        }
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        let short = w.min(h);
        let (sx, sy) = (DESIGN_ASPECT * short / w, short / h);
        let centred = (a.0 - 0.5).abs() < 0.05 && (a.1 - 0.5).abs() < 0.05;
        let grow = match (centred && self.category != "Utility", format) {
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
    for (&i, name) in texts.iter().zip(fields.names) {
        td.objects[i].name = (*name).to_owned();
        published.push(PublishedField { label: (*name).to_owned(), kind: FieldKind::Text { object: td.objects[i].id } });
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
// The curated set: every template designed on the bundled and catalogue faces, placed
// inside title-safe, and moved by the motion model; backing shapes move
// with their text. Each keeps a few fields editable once applied (#1724):
// its texts, its accent, Size and Speed, so one drag and two sliders make
// it the user's own. Everything else is restyled from the Inspector.
// ---------------------------------------------------------------------------


const WHITE: [u8; 4] = [255, 255, 255, 255];
const INK: [u8; 4] = [18, 18, 22, 255];
const MUTED: [u8; 4] = [225, 225, 230, 255];
const ACCENT: [u8; 4] = [74, 143, 231, 255];
const GRO: &str = "Inter";
const DISPLAY: &str = "Bebas Neue";
const MONO: &str = "JetBrains Mono";
// Faces from Klipp's catalogue (#2732), fetched when a title first uses
// them; the bundled ones stand in until they arrive.
const SANS: &str = "Manrope";
const LABEL: &str = "Work Sans";
const EDITORIAL: &str = "Playfair Display";
const CLASSIC: &str = "Cormorant Garamond";
const HEADING: &str = "DM Serif Display";
const POSTER: &str = "Anton";

/// A soft drop shadow that lifts text off footage without reading as
/// a shadow.
fn lift() -> TitleShadow {
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
/// its object will carry.
struct Text {
    tb: crate::TextBox,
    fx: crate::LayerFx,
}

fn face(text: &str, family: &str, style: FontStyle, size: f32, color: [u8; 4], align: TextAlignment) -> Text {
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

fn upper(mut t: Text, tracking: f32) -> Text {
    t.tb.case_transform = crate::TextCase::Uppercase;
    t.tb.tracking = tracking;
    t
}

/// `t` at a weight of its family (100 thin to 900 black).
fn weight(mut t: Text, w: u16) -> Text {
    t.tb.font_weight = Some(w);
    t
}

/// A light fill from `top` to `bottom`: depth without a look of its own.
fn graded(mut t: Text, top: [u8; 4], bottom: [u8; 4]) -> Text {
    t.tb.gradient = Some(crate::TextGradient { top_color: top, bottom_color: bottom, enabled: true, stops: Vec::new(), kind: Default::default(), stop_keyframes: Vec::new() });
    t
}

/// `t` with its lines spaced by `spacing` (1 as set).
fn spaced(mut t: Text, spacing: f32) -> Text {
    t.tb.line_spacing = spacing;
    t
}

fn lifted(mut t: Text) -> Text {
    t.fx.shadows.push(lift());
    t
}

/// A name that keeps its line: a long one is set smaller to fit its box
/// rather than wrapping under the lines below it (#1722).
fn keeps_line(mut t: Text) -> Text {
    t.tb.fit = crate::TextFit::Shrink { min_size: (t.tb.font_size * 0.5).round() };
    t
}

/// [`text_at`] in a box `w` of the frame wide, for a layer that has to
/// stay inside a bar.
fn boxed_at(t: Text, x: f32, y: f32, w: f32) -> Layer {
    let mut layer = text_at(t, x, y, 0.0);
    layer.1.size.x = w;
    layer
}

fn moved(mut t: Text, motion: TitleMotion) -> Text {
    t.fx.motion = motion;
    t
}

/// Where a layer sits: `x`, `y` in 0..1 of the frame; the anchor
/// chooses which point of the layer lands there (0 = left/top).
fn at(x: f32, y: f32, anchor_x: f32) -> ObjectTransform {
    ObjectTransform {
        center: TitlePosition { x, y },
        anchor_norm: TitlePosition { x: anchor_x, y: 0.5 },
        ..default_text_transform()
    }
}

/// One layer of a template: what it is, where it sits, and its stack.
type Layer = (TitleObjectContent, ObjectTransform, crate::LayerFx);

fn rect(x: f32, y: f32, w: f32, h: f32, fill: [u8; 4], radius: f32) -> Layer {
    (
        TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: radius }, fit: None, fill: Some(fill), gradient: None, stroke: None }),
        ObjectTransform { center: TitlePosition { x, y }, size: TitlePosition { x: w, y: h }, ..default_text_transform() },
        crate::LayerFx::default(),
    )
}

/// Build a title from layers, back to front.
fn layers(items: Vec<Layer>) -> TitleData {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: TitleAnimationType::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    for (content, transform, fx) in items {
        let id = td.add_object(content, transform);
        td.object_mut(id).expect("just added").fx = Some(fx);
    }
    td
}

fn text_at(t: Text, x: f32, y: f32, anchor_x: f32) -> Layer {
    (TitleObjectContent::TextBox(t.tb), at(x, y, anchor_x), t.fx)
}

// ── Motion presets ──
fn enter(kind: MotionKind, secs: f32, easing: Easing) -> Motion {
    Motion::new(kind).duration(secs).easing(easing)
}
fn fade_out(secs: f32) -> Motion {
    Motion::new(MotionKind::Fade).duration(secs).easing(Easing::InCubic)
}

/// A backing shape that moves with its text (#1724): a bar that sits
/// still while the words slide in reads as pasted on.
fn moving(mut layer: Layer, motion: TitleMotion) -> Layer {
    layer.2.motion = motion;
    layer
}

/// Shape `shape` sized to text `text`, padded (1080p pixels): the backing
/// follows whatever is typed.
fn fit(td: &mut TitleData, shape: usize, text: usize, pad: (f32, f32)) {
    let target = td.objects[text].id;
    if let TitleObjectContent::Shape(s) = &mut td.objects[shape].content {
        s.fit = Some(crate::ShapeFit { target, padding: TitlePosition { x: pad.0, y: pad.1 } });
    }
}

/// A heavy outline that keeps a caption readable on any picture.
fn outlined(mut t: Text, width: f32) -> Text {
    t.fx.outlines.push(crate::TextOutline { enabled: true, color: [0, 0, 0, 255], width, width_keyframes: Vec::new() });
    t
}

// ── Lower thirds ──
fn lt_name_bar() -> TitleData {
    layers(vec![
        rect(0.30, 0.845, 0.48, 0.115, [14, 14, 18, 210], 4.0),
        // Both lines inside the bar, from 0.085 to its right edge less
        // the same margin.
        boxed_at(
            moved(keeps_line(face("[Name]", GRO, FontStyle::Bold, 44.0, WHITE, TextAlignment::Left)),
                TitleMotion::both(enter(MotionKind::MaskRise, 0.5, Easing::OutExpo).delay(0.1), fade_out(0.3))),
            0.085, 0.822, 0.43,
        ),
        boxed_at(
            moved(keeps_line(face("[Title or role]", GRO, FontStyle::Regular, 26.0, MUTED, TextAlignment::Left)),
                TitleMotion::both(enter(MotionKind::MaskRise, 0.5, Easing::OutExpo).delay(0.25), fade_out(0.3))),
            0.085, 0.872, 0.43,
        ),
    ])
}

fn lt_clean_line() -> TitleData {
    layers(vec![
        rect(0.19, 0.847, 0.22, 0.006, ACCENT, 0.0),
        text_at(
            moved(keeps_line(lifted(face("[Name]", GRO, FontStyle::Bold, 46.0, WHITE, TextAlignment::Left))),
                TitleMotion::both(enter(MotionKind::SlideRight, 0.5, Easing::OutQuint).distance(0.4), fade_out(0.3))),
            0.08, 0.805, 0.0,
        ),
        text_at(
            moved(lifted(face("[Title or role]", GRO, FontStyle::Regular, 26.0, MUTED, TextAlignment::Left)),
                TitleMotion::both(enter(MotionKind::SlideRight, 0.5, Easing::OutQuint).distance(0.4).delay(0.12), fade_out(0.3))),
            0.08, 0.882, 0.0,
        ),
    ])
}

fn lt_broadcast() -> TitleData {
    let mut td = layers(vec![
        rect(0.25, 0.815, 0.38, 0.07, [26, 84, 196, 255], 0.0),
        rect(0.25, 0.874, 0.38, 0.048, [244, 244, 246, 255], 0.0),
        text_at(
            moved(keeps_line(upper(face("[Name]", DISPLAY, FontStyle::Regular, 50.0, WHITE, TextAlignment::Left), 2.0)),
                TitleMotion::both(enter(MotionKind::SlideRight, 0.4, Easing::OutExpo).distance(0.8), fade_out(0.25))),
            0.075, 0.815, 0.0,
        ),
        text_at(
            moved(upper(face("[Title or role]", GRO, FontStyle::Bold, 20.0, INK, TextAlignment::Left), 2.5),
                TitleMotion::both(enter(MotionKind::SlideRight, 0.4, Easing::OutExpo).distance(0.8).delay(0.1), fade_out(0.25))),
            0.075, 0.874, 0.0,
        ),
    ]);
    // The two backing strips grow independently with their text.
    for (shape_index, text_index) in [(0, 2), (1, 3)] {
        let target = td.objects[text_index].id;
        if let TitleObjectContent::Shape(shape) = &mut td.objects[shape_index].content {
            shape.fit = Some(crate::ShapeFit {
                target, padding: TitlePosition { x: 28.0, y: 12.0 },
            });
        }
        td.objects[text_index].transform.size.x = 0.85;
    }
    td
}

fn lt_documentary() -> TitleData {
    layers(vec![
        text_at(
            moved(keeps_line(lifted(face("[Name]", EDITORIAL, FontStyle::Italic, 52.0, WHITE, TextAlignment::Left))),
                TitleMotion::both(enter(MotionKind::Rise, 0.8, Easing::OutCubic).distance(0.3), fade_out(0.5))),
            0.08, 0.815, 0.0,
        ),
        text_at(
            moved(lifted(upper(weight(face("[Title or role]", LABEL, FontStyle::Regular, 28.0, MUTED, TextAlignment::Left), 500), 4.0)),
                TitleMotion::both(enter(MotionKind::Rise, 0.8, Easing::OutCubic).distance(0.3).delay(0.15), fade_out(0.5))),
            0.08, 0.868, 0.0,
        ),
    ])
}

fn lt_location() -> TitleData {
    layers(vec![text_at(
        moved(lifted(upper(face("[City, Country]\n[14:32]", MONO, FontStyle::Regular, 26.0, WHITE, TextAlignment::Left), 3.0)),
            TitleMotion::both(Motion::new(MotionKind::Typewriter).duration(0.02).stagger(StaggerUnit::Char, 0.035), fade_out(0.3))),
        0.06, 0.12, 0.0,
    )])
}

// ── Openers ──
fn op_cinematic() -> TitleData {
    layers(vec![text_at(
        moved(lifted(upper(weight(face("[Title]", CLASSIC, FontStyle::Regular, 112.0, WHITE, TextAlignment::Center), 500), 18.0)),
            TitleMotion::both(enter(MotionKind::Blur, 1.4, Easing::OutCubic), Motion::new(MotionKind::Blur).duration(0.9).easing(Easing::InCubic))),
        0.5, 0.5, 0.5,
    )])
}

fn op_impact() -> TitleData {
    layers(vec![text_at(
        moved(lifted(graded(upper(face("[Big title]", POSTER, FontStyle::Regular, 180.0, WHITE, TextAlignment::Center), 2.0), WHITE, [214, 220, 230, 255])),
            TitleMotion::both(
                enter(MotionKind::MaskRise, 0.55, Easing::OutExpo).stagger(StaggerUnit::Word, 0.08),
                Motion::new(MotionKind::MaskDrop).duration(0.4).easing(Easing::InCubic),
            )),
        0.5, 0.5, 0.5,
    )])
}

fn op_editorial() -> TitleData {
    layers(vec![
        text_at(
            moved(lifted(weight(face("[Headline]", EDITORIAL, FontStyle::Bold, 112.0, WHITE, TextAlignment::Center), 700)),
                TitleMotion::both(enter(MotionKind::Rise, 0.7, Easing::OutQuint).distance(0.35).stagger(StaggerUnit::Word, 0.06), fade_out(0.4))),
            0.5, 0.45, 0.5,
        ),
        moving(rect(0.5, 0.548, 0.05, 0.004, ACCENT, 0.0),
            TitleMotion::both(enter(MotionKind::Fade, 0.6, Easing::OutCubic).delay(0.3), fade_out(0.4))),
        text_at(
            moved(lifted(upper(weight(face("[Subtitle]", LABEL, FontStyle::Regular, 32.0, MUTED, TextAlignment::Center), 500), 6.0)),
                TitleMotion::both(enter(MotionKind::Fade, 0.6, Easing::OutCubic).delay(0.35), fade_out(0.4))),
            0.5, 0.6, 0.5,
        ),
    ])
}

fn op_minimal() -> TitleData {
    layers(vec![text_at(
        moved(lifted(upper(weight(face("[Title]", SANS, FontStyle::Regular, 76.0, WHITE, TextAlignment::Center), 300), 16.0)),
            TitleMotion::both(enter(MotionKind::Fade, 1.5, Easing::InOutCubic), fade_out(1.0))),
        0.5, 0.5, 0.5,
    )])
}

fn op_chapter() -> TitleData {
    layers(vec![
        text_at(
            moved(lifted(upper(weight(face("Chapter One", LABEL, FontStyle::Regular, 30.0, MUTED, TextAlignment::Center), 500), 8.0)),
                TitleMotion::both(Motion::new(MotionKind::Typewriter).duration(0.02).stagger(StaggerUnit::Char, 0.04), fade_out(0.4))),
            0.5, 0.41, 0.5,
        ),
        text_at(
            moved(lifted(face("[Chapter title]", HEADING, FontStyle::Regular, 104.0, WHITE, TextAlignment::Center)),
                TitleMotion::both(enter(MotionKind::MaskRise, 0.6, Easing::OutExpo).delay(0.5).stagger(StaggerUnit::Word, 0.1), fade_out(0.4))),
            0.5, 0.53, 0.5,
        ),
    ])
}

fn op_statement() -> TitleData {
    layers(vec![
        rect(0.17, 0.60, 0.18, 0.012, ACCENT, 0.0),
        text_at(
            moved(face("[BOLD\nSTATEMENT]", DISPLAY, FontStyle::Regular, 130.0, WHITE, TextAlignment::Left),
                TitleMotion::both(enter(MotionKind::SlideLeft, 0.5, Easing::OutExpo).distance(0.5).stagger(StaggerUnit::Line, 0.1), fade_out(0.3))),
            0.08, 0.44, 0.0,
        ),
    ])
}

// ── Captions and quotes ──
fn cap_quote() -> TitleData {
    layers(vec![
        text_at(
            moved(lifted(spaced(face("\u{201c}[Quote]\u{201d}", EDITORIAL, FontStyle::Italic, 68.0, WHITE, TextAlignment::Center), 1.15)),
                TitleMotion::both(enter(MotionKind::Fade, 0.8, Easing::OutCubic).stagger(StaggerUnit::Line, 0.15), fade_out(0.5))),
            0.5, 0.46, 0.5,
        ),
        text_at(
            moved(lifted(upper(weight(face("\u{2014} [Who said it]", LABEL, FontStyle::Regular, 30.0, MUTED, TextAlignment::Center), 500), 5.0)),
                TitleMotion::both(enter(MotionKind::Fade, 0.6, Easing::OutCubic).delay(0.5), fade_out(0.5))),
            0.5, 0.6, 0.5,
        ),
    ])
}

fn cap_caption() -> TitleData {
    // A box the size of the words, not a bar the width of the frame.
    let mut td = layers(vec![
        rect(0.5, 0.885, 0.3, 0.07, [0, 0, 0, 185], 12.0),
        text_at(
            moved(weight(face("[Caption]", SANS, FontStyle::Regular, 42.0, WHITE, TextAlignment::Center), 600),
                TitleMotion::both(enter(MotionKind::Fade, 0.3, Easing::OutCubic), fade_out(0.25))),
            0.5, 0.885, 0.5,
        ),
    ]);
    fit(&mut td, 0, 1, (26.0, 12.0));
    td
}

fn cap_subtitle() -> TitleData {
    let mut tb = face("[Subtitle]", GRO, FontStyle::Bold, 40.0, WHITE, TextAlignment::Center);
    tb.fx.shadows.push(TitleShadow { color: [0, 0, 0, 200], offset_x: 0.0, offset_y: 2.0, blur: 4.0, enabled: true, offset_x_keyframes: Vec::new(), offset_y_keyframes: Vec::new(), blur_keyframes: Vec::new() });
    tb.fx.shadows.push(lift());
    layers(vec![text_at(tb, 0.5, 0.90, 0.5)])
}

fn cap_callout() -> TitleData {
    let mut td = layers(vec![
        rect(0.80, 0.14, 0.26, 0.08, [38, 99, 200, 245], 40.0),
        text_at(
            moved(weight(face("[Callout]", SANS, FontStyle::Bold, 34.0, WHITE, TextAlignment::Center), 700),
                TitleMotion::both(enter(MotionKind::ScaleUp, 0.35, Easing::OutBack), fade_out(0.25))),
            0.80, 0.14, 0.5,
        ),
    ]);
    fit(&mut td, 0, 1, (32.0, 20.0));
    td
}

// ── Credits ──
fn cr_roll() -> TitleData {
    let mut td = layers(vec![text_at(
        lifted(spaced(face("Director\n[Name]\n\nProducer\n[Name]\n\nEditor\n[Name]", SANS, FontStyle::Regular, 44.0, WHITE, TextAlignment::Center), 1.2)),
        0.5, 0.5, 0.5,
    )]);
    if let Some(o) = td.primary_object_mut() {
        o.fx_mut().loops.push(TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0, shape: Default::default() });
    }
    td
}

fn cr_end_card() -> TitleData {
    layers(vec![text_at(
        moved(lifted(upper(weight(face("The End", CLASSIC, FontStyle::Regular, 104.0, WHITE, TextAlignment::Center), 500), 16.0)),
            TitleMotion::both(enter(MotionKind::Fade, 2.0, Easing::InOutCubic), fade_out(1.5))),
        0.5, 0.5, 0.5,
    )])
}

fn cr_cast() -> TitleData {
    layers(vec![
        text_at(
            moved(lifted(upper(weight(face("[Role]", LABEL, FontStyle::Regular, 30.0, MUTED, TextAlignment::Center), 500), 6.0)),
                TitleMotion::both(enter(MotionKind::Rise, 0.6, Easing::OutQuint).distance(0.4), fade_out(0.4))),
            0.5, 0.43, 0.5,
        ),
        text_at(
            moved(lifted(face("[Name]", HEADING, FontStyle::Regular, 84.0, WHITE, TextAlignment::Center)),
                TitleMotion::both(enter(MotionKind::Rise, 0.6, Easing::OutQuint).distance(0.4).delay(0.12), fade_out(0.4))),
            0.5, 0.525, 0.5,
        ),
    ])
}

// ── Social ──
fn so_hook() -> TitleData {
    layers(vec![text_at(
        moved(lifted(graded(upper(face("[Hook line]", POSTER, FontStyle::Regular, 150.0, [255, 214, 10, 255], TextAlignment::Center), 1.5), [255, 226, 70, 255], [255, 190, 0, 255])),
            TitleMotion::both(enter(MotionKind::Drop, 0.45, Easing::OutBack).distance(0.5).stagger(StaggerUnit::Word, 0.06), fade_out(0.25))),
        0.5, 0.45, 0.5,
    )])
}

fn so_handle() -> TitleData {
    layers(vec![
        rect(0.5, 0.90, 0.30, 0.075, [250, 250, 252, 255], 40.0),
        text_at(
            moved(face("@yourhandle", GRO, FontStyle::Bold, 34.0, INK, TextAlignment::Center),
                TitleMotion::both(enter(MotionKind::ScaleUp, 0.4, Easing::OutBack), fade_out(0.25))),
            0.5, 0.90, 0.5,
        ),
    ])
}

fn so_subscribe() -> TitleData {
    layers(vec![
        rect(0.5, 0.86, 0.30, 0.09, [230, 40, 40, 255], 6.0),
        text_at(
            moved(upper(face("Subscribe", GRO, FontStyle::Bold, 40.0, WHITE, TextAlignment::Center), 2.0),
                TitleMotion::both(enter(MotionKind::SlideRight, 0.4, Easing::OutExpo).distance(0.8), Motion::new(MotionKind::SlideLeft).duration(0.3).easing(Easing::InCubic).distance(0.8))),
            0.5, 0.86, 0.5,
        ),
    ])
}

// ── Utility ──
fn ut_slate() -> TitleData {
    layers(vec![
        rect(0.5, 0.5, 1.0, 1.0, [12, 12, 14, 255], 0.0),
        text_at(
            moved(face("PROJECT   [Project]\nSCENE     [1]\nTAKE      [1]\nDATE      [2026-01-01]", MONO, FontStyle::Bold, 40.0, WHITE, TextAlignment::Left),
                TitleMotion::enter(Motion::new(MotionKind::Typewriter).duration(0.02).stagger(StaggerUnit::Line, 0.18))),
            // Its left edge where the block of lines comes out centred.
            0.36, 0.5, 0.0,
        ),
    ])
}

fn ut_timecode() -> TitleData {
    layers(vec![text_at(lifted(face("00:00:00:00", MONO, FontStyle::Regular, 26.0, WHITE, TextAlignment::Left)), 0.06, 0.93, 0.0)])
}

fn ut_watermark() -> TitleData {
    layers(vec![text_at(face("[Brand]", GRO, FontStyle::Bold, 28.0, [255, 255, 255, 110], TextAlignment::Right), 0.94, 0.92, 1.0)])
}

fn ut_label() -> TitleData {
    layers(vec![
        rect(0.14, 0.10, 0.18, 0.06, [0, 0, 0, 180], 4.0),
        text_at(
            moved(upper(face("[Label]", GRO, FontStyle::Bold, 22.0, WHITE, TextAlignment::Center), 3.0),
                TitleMotion::both(enter(MotionKind::Fade, 0.3, Easing::OutCubic), fade_out(0.25))),
            0.14, 0.10, 0.5,
        ),
    ])
}


// ── More lower thirds (#1724) ──
fn lt_accent_block() -> TitleData {
    let words = |d: f32| TitleMotion::both(enter(MotionKind::SlideRight, 0.55, Easing::OutQuint).distance(0.25).delay(d), fade_out(0.3));
    layers(vec![
        moving(rect(0.072, 0.85, 0.008, 0.105, ACCENT, 0.0),
            TitleMotion::both(enter(MotionKind::MaskRise, 0.45, Easing::OutExpo), fade_out(0.3))),
        text_at(moved(lifted(face("[Name]", GRO, FontStyle::Bold, 48.0, WHITE, TextAlignment::Left)), words(0.12)), 0.088, 0.822, 0.0),
        text_at(moved(lifted(face("[Title or role]", GRO, FontStyle::Regular, 26.0, MUTED, TextAlignment::Left)), words(0.22)), 0.088, 0.878, 0.0),
    ])
}

fn lt_pill() -> TitleData {
    let mut td = layers(vec![
        moving(rect(0.18, 0.835, 0.2, 0.07, WHITE, 30.0),
            TitleMotion::both(enter(MotionKind::ScaleUp, 0.4, Easing::OutBack), fade_out(0.25))),
        text_at(
            moved(face("[Name]", GRO, FontStyle::Bold, 40.0, INK, TextAlignment::Left),
                TitleMotion::both(enter(MotionKind::Fade, 0.3, Easing::OutCubic).delay(0.15), fade_out(0.25))),
            0.08, 0.835, 0.0,
        ),
        text_at(
            moved(lifted(face("[Title or role]", GRO, FontStyle::Regular, 26.0, WHITE, TextAlignment::Left)),
                TitleMotion::both(enter(MotionKind::Rise, 0.45, Easing::OutCubic).distance(0.3).delay(0.3), fade_out(0.25))),
            0.08, 0.905, 0.0,
        ),
    ]);
    fit(&mut td, 0, 1, (30.0, 14.0));
    td.objects[1].transform.size.x = 0.85;
    td
}

fn lt_stacked() -> TitleData {
    layers(vec![
        moving(rect(0.13, 0.857, 0.1, 0.005, ACCENT, 0.0),
            TitleMotion::both(enter(MotionKind::SlideRight, 0.5, Easing::OutExpo).distance(0.5).delay(0.2), fade_out(0.3))),
        text_at(
            moved(lifted(upper(face("[Name]", DISPLAY, FontStyle::Regular, 72.0, WHITE, TextAlignment::Left), 3.0)),
                TitleMotion::both(enter(MotionKind::MaskRise, 0.55, Easing::OutExpo).stagger(StaggerUnit::Char, 0.02), fade_out(0.3))),
            0.08, 0.81, 0.0,
        ),
        text_at(
            moved(lifted(upper(face("[Title or role]", GRO, FontStyle::Bold, 20.0, MUTED, TextAlignment::Left), 4.0)),
                TitleMotion::both(enter(MotionKind::Fade, 0.5, Easing::OutCubic).delay(0.4), fade_out(0.3))),
            0.08, 0.89, 0.0,
        ),
    ])
}

// ── More openers and chapters (#1724) ──
fn op_chapter_number() -> TitleData {
    layers(vec![
        text_at(
            moved(lifted(face("01", DISPLAY, FontStyle::Regular, 200.0, [120, 175, 255, 255], TextAlignment::Center)),
                TitleMotion::both(enter(MotionKind::MaskRise, 0.7, Easing::OutExpo), fade_out(0.4))),
            0.5, 0.40, 0.5,
        ),
        text_at(
            moved(lifted(upper(face("[Chapter title]", GRO, FontStyle::Bold, 44.0, WHITE, TextAlignment::Center), 8.0)),
                TitleMotion::both(enter(MotionKind::Fade, 0.6, Easing::OutCubic).delay(0.45).stagger(StaggerUnit::Char, 0.015), fade_out(0.4))),
            0.5, 0.60, 0.5,
        ),
    ])
}

fn op_title_card() -> TitleData {
    layers(vec![
        moving(rect(0.5, 0.5, 1.0, 1.0, [8, 8, 10, 170], 0.0),
            TitleMotion::both(enter(MotionKind::Fade, 0.8, Easing::OutCubic), fade_out(0.8))),
        text_at(
            moved(face("[Title]", EDITORIAL, FontStyle::Regular, 100.0, WHITE, TextAlignment::Center),
                TitleMotion::both(enter(MotionKind::FocusIn, 1.1, Easing::OutCubic).delay(0.3), fade_out(0.6))),
            0.5, 0.47, 0.5,
        ),
        text_at(
            moved(upper(weight(face("[Subtitle]", LABEL, FontStyle::Regular, 30.0, MUTED, TextAlignment::Center), 500), 8.0),
                TitleMotion::both(enter(MotionKind::Fade, 0.8, Easing::OutCubic).delay(0.9), fade_out(0.6))),
            0.5, 0.585, 0.5,
        ),
    ])
}

// ── More end cards and credits (#1724) ──
fn cr_end_credits() -> TitleData {
    let col = |text: &str, x: f32, anchor: f32, align: TextAlignment, colour: [u8; 4], style: FontStyle| text_at(
        moved(lifted(face(text, if style == FontStyle::Bold { SANS } else { LABEL }, style, 32.0, colour, align)),
            TitleMotion::both(enter(MotionKind::Fade, 0.8, Easing::OutCubic).delay(0.6).stagger(StaggerUnit::Line, 0.12), fade_out(0.6))),
        x, 0.62, anchor,
    );
    let mut roles = col("Directed by\nProduced by\nEdited by\nMusic by", 0.485, 1.0, TextAlignment::Right, MUTED, FontStyle::Regular);
    let mut names = col("[Name]\n[Name]\n[Name]\n[Name]", 0.515, 0.0, TextAlignment::Left, WHITE, FontStyle::Bold);
    for l in [&mut roles, &mut names] {
        l.1.size.x = 0.45;
        if let TitleObjectContent::TextBox(tb) = &mut l.0 {
            tb.line_spacing = 1.35;
        }
    }
    layers(vec![
        text_at(
            moved(lifted(upper(weight(face("[Film title]", CLASSIC, FontStyle::Regular, 88.0, WHITE, TextAlignment::Center), 500), 12.0)),
                TitleMotion::both(enter(MotionKind::Fade, 1.2, Easing::InOutCubic), fade_out(0.8))),
            0.5, 0.30, 0.5,
        ),
        roles,
        names,
    ])
}

fn cr_thanks() -> TitleData {
    layers(vec![
        text_at(
            moved(lifted(face("Thanks for watching", GRO, FontStyle::Bold, 72.0, WHITE, TextAlignment::Center)),
                TitleMotion::both(enter(MotionKind::Rise, 0.7, Easing::OutQuint).distance(0.3).stagger(StaggerUnit::Word, 0.08), fade_out(0.5))),
            0.5, 0.46, 0.5,
        ),
        text_at(
            moved(lifted(face("[@yourhandle \u{00b7} yoursite.com]", GRO, FontStyle::Regular, 30.0, MUTED, TextAlignment::Center)),
                TitleMotion::both(enter(MotionKind::Fade, 0.6, Easing::OutCubic).delay(0.5), fade_out(0.5))),
            0.5, 0.56, 0.5,
        ),
    ])
}

fn cr_up_next() -> TitleData {
    let mut td = layers(vec![
        moving(rect(0.12, 0.70, 0.1, 0.05, ACCENT, 4.0),
            TitleMotion::both(enter(MotionKind::SlideRight, 0.4, Easing::OutExpo).distance(0.3), fade_out(0.3))),
        text_at(
            moved(upper(face("Up next", GRO, FontStyle::Bold, 22.0, WHITE, TextAlignment::Left), 4.0),
                TitleMotion::both(enter(MotionKind::Fade, 0.3, Easing::OutCubic).delay(0.15), fade_out(0.3))),
            0.08, 0.70, 0.0,
        ),
        text_at(
            moved(lifted(face("[Next video title]", GRO, FontStyle::Bold, 58.0, WHITE, TextAlignment::Left)),
                TitleMotion::both(enter(MotionKind::MaskRise, 0.55, Easing::OutExpo).delay(0.25).stagger(StaggerUnit::Word, 0.06), fade_out(0.3))),
            0.08, 0.79, 0.0,
        ),
    ]);
    fit(&mut td, 0, 1, (16.0, 8.0));
    td
}

// ── More social, made for any frame shape (#1724) ──
fn so_bold_caption() -> TitleData {
    layers(vec![text_at(
        moved(outlined(upper(face("[Say it big]", DISPLAY, FontStyle::Regular, 112.0, WHITE, TextAlignment::Center), 1.0), 6.0),
            TitleMotion::both(enter(MotionKind::Pop, 0.35, Easing::OutBack).stagger(StaggerUnit::Word, 0.12), fade_out(0.2))),
        0.5, 0.64, 0.5,
    )])
}

fn so_highlight() -> TitleData {
    let mut td = layers(vec![
        moving(rect(0.5, 0.7, 0.4, 0.08, [255, 214, 10, 255], 10.0),
            TitleMotion::both(enter(MotionKind::ScaleUp, 0.35, Easing::OutBack), fade_out(0.2))),
        text_at(
            moved(face("[Key words]", GRO, FontStyle::Bold, 52.0, INK, TextAlignment::Center),
                TitleMotion::both(enter(MotionKind::Fade, 0.25, Easing::OutCubic).delay(0.12), fade_out(0.2))),
            0.5, 0.7, 0.5,
        ),
    ]);
    fit(&mut td, 0, 1, (26.0, 12.0));
    td.objects[1].transform.size.x = 0.9;
    td
}

fn so_question() -> TitleData {
    layers(vec![
        text_at(
            moved(upper(face("Question", GRO, FontStyle::Bold, 24.0, [255, 214, 10, 255], TextAlignment::Center), 6.0),
                TitleMotion::both(enter(MotionKind::Fade, 0.3, Easing::OutCubic), fade_out(0.25))),
            0.5, 0.36, 0.5,
        ),
        text_at(
            moved(lifted(face("[What would you do?]", GRO, FontStyle::Bold, 76.0, WHITE, TextAlignment::Center)),
                TitleMotion::both(enter(MotionKind::Slam, 0.4, Easing::OutCubic).delay(0.15).stagger(StaggerUnit::Word, 0.07), fade_out(0.25))),
            0.5, 0.48, 0.5,
        ),
    ])
}

fn so_tip() -> TitleData {
    let mut td = layers(vec![
        moving(rect(0.11, 0.16, 0.08, 0.06, [255, 214, 10, 255], 10.0),
            TitleMotion::both(enter(MotionKind::ScaleUp, 0.35, Easing::OutBack), fade_out(0.25))),
        text_at(
            moved(upper(face("Tip", GRO, FontStyle::Bold, 24.0, INK, TextAlignment::Left), 3.0),
                TitleMotion::both(enter(MotionKind::Fade, 0.25, Easing::OutCubic).delay(0.1), fade_out(0.25))),
            0.08, 0.16, 0.0,
        ),
        text_at(
            moved(lifted(face("[Your tip in a line]", GRO, FontStyle::Bold, 36.0, WHITE, TextAlignment::Left)),
                TitleMotion::both(enter(MotionKind::SlideRight, 0.45, Easing::OutQuint).distance(0.2).delay(0.2), fade_out(0.25))),
            0.08, 0.235, 0.0,
        ),
    ]);
    fit(&mut td, 0, 1, (18.0, 8.0));
    td
}

// ── Callout (#1724) ──
fn cap_note() -> TitleData {
    let mut td = layers(vec![
        moving(rect(0.25, 0.16, 0.3, 0.1, [16, 16, 20, 225], 8.0),
            TitleMotion::both(enter(MotionKind::Fade, 0.3, Easing::OutCubic), fade_out(0.25))),
        moving(rect(0.105, 0.16, 0.005, 0.07, ACCENT, 0.0),
            TitleMotion::both(enter(MotionKind::MaskRise, 0.35, Easing::OutExpo).delay(0.1), fade_out(0.25))),
        text_at(
            moved(face("[A note on screen]", GRO, FontStyle::Regular, 30.0, WHITE, TextAlignment::Left),
                TitleMotion::both(enter(MotionKind::Fade, 0.35, Easing::OutCubic).delay(0.15), fade_out(0.25))),
            0.12, 0.16, 0.0,
        ),
    ]);
    fit(&mut td, 0, 2, (40.0, 22.0));
    td
}

const fn f(names: &'static [&'static str], accent: Option<usize>, anchor: (f32, f32)) -> Fields {
    Fields { names, accent, anchor }
}

/// Lower thirds grow from their left edge; centred titles from the
/// middle of the frame.
const LT: (f32, f32) = (0.08, 0.85);
const MID: (f32, f32) = (0.5, 0.5);
const NAME_ROLE: &[&str] = &["Name", "Role"];

pub const TITLES: &[TitleTemplate] = &[
    TitleTemplate { name: "Name Bar", category: "Lower Thirds", preview_text: "[Name]", create_data: lt_name_bar, fields: f(NAME_ROLE, Some(0), LT) },
    TitleTemplate { name: "Clean Line", category: "Lower Thirds", preview_text: "[Name]", create_data: lt_clean_line, fields: f(NAME_ROLE, Some(0), LT) },
    TitleTemplate { name: "Broadcast", category: "Lower Thirds", preview_text: "[Name]", create_data: lt_broadcast, fields: f(NAME_ROLE, Some(0), LT) },
    TitleTemplate { name: "Accent Block", category: "Lower Thirds", preview_text: "[Name]", create_data: lt_accent_block, fields: f(NAME_ROLE, Some(0), LT) },
    TitleTemplate { name: "Pill", category: "Lower Thirds", preview_text: "[Name]", create_data: lt_pill, fields: f(NAME_ROLE, Some(0), LT) },
    TitleTemplate { name: "Stacked", category: "Lower Thirds", preview_text: "[Name]", create_data: lt_stacked, fields: f(NAME_ROLE, Some(0), LT) },
    TitleTemplate { name: "Documentary", category: "Lower Thirds", preview_text: "[Name]", create_data: lt_documentary, fields: f(NAME_ROLE, None, LT) },
    TitleTemplate { name: "Location", category: "Lower Thirds", preview_text: "[City, Country]", create_data: lt_location, fields: f(&["Place and time"], None, (0.06, 0.12)) },
    TitleTemplate { name: "Cinematic", category: "Openers", preview_text: "[Title]", create_data: op_cinematic, fields: f(&["Title"], None, MID) },
    TitleTemplate { name: "Impact", category: "Openers", preview_text: "[BIG TITLE]", create_data: op_impact, fields: f(&["Title"], None, MID) },
    TitleTemplate { name: "Editorial", category: "Openers", preview_text: "[Headline]", create_data: op_editorial, fields: f(&["Headline", "Subtitle"], None, MID) },
    TitleTemplate { name: "Title Card", category: "Openers", preview_text: "[Title]", create_data: op_title_card, fields: f(&["Title", "Subtitle"], None, MID) },
    TitleTemplate { name: "Minimal", category: "Openers", preview_text: "[Title]", create_data: op_minimal, fields: f(&["Title"], None, MID) },
    TitleTemplate { name: "Chapter", category: "Openers", preview_text: "[Chapter title]", create_data: op_chapter, fields: f(&["Chapter", "Chapter title"], None, MID) },
    TitleTemplate { name: "Chapter Number", category: "Openers", preview_text: "[Chapter title]", create_data: op_chapter_number, fields: f(&["Number", "Chapter title"], Some(0), MID) },
    TitleTemplate { name: "Statement", category: "Openers", preview_text: "[BOLD STATEMENT]", create_data: op_statement, fields: f(&["Statement"], Some(0), (0.08, 0.44)) },
    TitleTemplate { name: "Quote", category: "Captions", preview_text: "[Quote]", create_data: cap_quote, fields: f(&["Quote", "Who said it"], None, MID) },
    TitleTemplate { name: "Caption", category: "Captions", preview_text: "[Caption]", create_data: cap_caption, fields: f(&["Caption"], Some(0), (0.5, 0.885)) },
    TitleTemplate { name: "Subtitle", category: "Captions", preview_text: "[Subtitle]", create_data: cap_subtitle, fields: f(&["Subtitle"], None, (0.5, 0.9)) },
    TitleTemplate { name: "Callout", category: "Captions", preview_text: "[Callout]", create_data: cap_callout, fields: f(&["Callout"], Some(0), (0.8, 0.14)) },
    TitleTemplate { name: "Note", category: "Captions", preview_text: "[A note on screen]", create_data: cap_note, fields: f(&["Note"], Some(1), (0.1, 0.16)) },
    TitleTemplate { name: "Credits Roll", category: "Credits", preview_text: "Director", create_data: cr_roll, fields: f(&["Credits"], None, MID) },
    TitleTemplate { name: "End Credits", category: "Credits", preview_text: "[Film title]", create_data: cr_end_credits, fields: f(&["Film title", "Roles", "Names"], None, MID) },
    TitleTemplate { name: "End Card", category: "Credits", preview_text: "The End", create_data: cr_end_card, fields: f(&["Title"], None, MID) },
    TitleTemplate { name: "Cast", category: "Credits", preview_text: "[Name]", create_data: cr_cast, fields: f(&["Role", "Name"], None, MID) },
    TitleTemplate { name: "Thanks for Watching", category: "Credits", preview_text: "Thanks for watching", create_data: cr_thanks, fields: f(&["Title", "Where to find you"], None, MID) },
    TitleTemplate { name: "Up Next", category: "Credits", preview_text: "[Next video title]", create_data: cr_up_next, fields: f(&["Label", "Next title"], Some(0), (0.08, 0.75)) },
    TitleTemplate { name: "Hook", category: "Social", preview_text: "[HOOK LINE]", create_data: so_hook, fields: f(&["Hook"], None, (0.5, 0.45)) },
    TitleTemplate { name: "Bold Caption", category: "Social", preview_text: "[Say it big]", create_data: so_bold_caption, fields: f(&["Caption"], None, (0.5, 0.64)) },
    TitleTemplate { name: "Highlight", category: "Social", preview_text: "[Key words]", create_data: so_highlight, fields: f(&["Words"], Some(0), (0.5, 0.7)) },
    TitleTemplate { name: "Question", category: "Social", preview_text: "[What would you do?]", create_data: so_question, fields: f(&["Label", "Question"], None, MID) },
    TitleTemplate { name: "Tip", category: "Social", preview_text: "[Your tip in a line]", create_data: so_tip, fields: f(&["Label", "Tip"], Some(0), (0.08, 0.2)) },
    TitleTemplate { name: "Handle", category: "Social", preview_text: "@yourhandle", create_data: so_handle, fields: f(&["Handle"], Some(0), (0.5, 0.9)) },
    TitleTemplate { name: "Subscribe", category: "Social", preview_text: "Subscribe", create_data: so_subscribe, fields: f(&["Button"], Some(0), (0.5, 0.86)) },
    TitleTemplate { name: "Slate", category: "Utility", preview_text: "PROJECT", create_data: ut_slate, fields: f(&["Slate"], None, MID) },
    TitleTemplate { name: "Timecode", category: "Utility", preview_text: "00:00:00:00", create_data: ut_timecode, fields: f(&["Timecode"], None, (0.06, 0.93)) },
    TitleTemplate { name: "Watermark", category: "Utility", preview_text: "[Brand]", create_data: ut_watermark, fields: f(&["Brand"], None, (0.94, 0.92)) },
    TitleTemplate { name: "Label", category: "Utility", preview_text: "[Label]", create_data: ut_label, fields: f(&["Label"], Some(0), (0.14, 0.10)) },
];

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
    /// The title the template makes, marked as made from it (#1716): what
    /// a drop from the browser and the template op place.
    pub fn data(&self) -> TitleData {
        self.data_for(1920, 1080)
    }

    /// [`Self::data`] laid out for a `width` × `height` frame (#2734).
    pub fn data_for(&self, width: u32, height: u32) -> TitleData {
        let mut td = self.build_for(width, height);
        td.template = Some(self.name.to_string());
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
