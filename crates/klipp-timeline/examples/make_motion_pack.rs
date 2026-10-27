//! Writes the built-in composition templates (#2757) to
//! `assets/templates/`: an authoring aid, run by hand when a template is
//! made or changed. What ships is the files it writes, read as data by
//! `composition_template::builtin_templates`; nothing here runs in Klipp.
//!
//! `cargo run -p klipp-timeline --example make_motion_pack`

use std::collections::BTreeMap;
use std::path::PathBuf;

use klipp_core::{ClipId, MediaId, RationalTime, Resolution, SequenceId, TemplateValue};
use klipp_effects::BuiltinEffect;
use klipp_timeline::composition_template::{CompositionTemplate, LinkMap, NumberRange, SlotKind, TemplateBinding, TemplateParameter, TemplateSlot};
use klipp_timeline::title_library::{self as lib, Fields, Layer, TitleTemplate};
use klipp_timeline::{Clip, ClipKind, Easing, FontStyle, ImageFit, ImageLayer, MotionKind, ObjectTransform, Sequence, StaggerUnit, TextAlignment, TitleData, TitleMotion, TitleObjectContent, TitlePosition, Track, TrackKind};
use uuid::Uuid;

const SECS: f64 = 5.0;

fn tc(s: f64) -> klipp_core::Timecode {
    RationalTime::from_secs_f64(s, 24)
}

/// A fixed id per name, so the files stay the same when made again.
fn id(name: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("klipp.motion-pack.{name}").as_bytes())
}

/// The three frames a built-in is laid out for.
const FRAMES: [(&str, u32, u32); 3] = [("wide", 1920, 1080), ("tall", 1080, 1920), ("square", 1080, 1080)];

/// A media slot filling the box `(x, y, w, h)` (pixels) of a `fw` × `fh`
/// frame whatever the footage's shape: cropped to cover the box's shape
/// (the renderer works it out from the source), then that cover, the
/// largest rectangle of the shape in the frame, placed on the box.
fn slot_clip(name: &str, fw: u32, fh: u32, b: (f32, f32, f32, f32)) -> Clip {
    let (fw, fh) = (fw as f32, fh as f32);
    let aspect = b.2 / b.3;
    let cover_w = if aspect > fw / fh { fw } else { fh * aspect };
    let scale = b.2 / cover_w;
    let mut c = Clip::media(MediaId(id(name)), tc(0.0), tc(SECS), tc(0.0), tc(SECS));
    c.id = ClipId(id(&format!("{name}.clip")));
    c.push_effect(BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect, focus_x: 0.5, focus_y: 0.5 });
    c.push_effect(BuiltinEffect::Transform {
        position_x: b.0 + b.2 / 2.0 - fw / 2.0,
        position_y: b.1 + b.3 / 2.0 - fh / 2.0,
        scale_x: scale,
        scale_y: scale,
        rotation: 0.0,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    });
    c
}

/// A title of plain rectangles (normalised centre and size), back to front.
fn shapes(name: &str, rects: &[((f32, f32), (f32, f32), [u8; 4])]) -> Clip {
    // From no layers: a default title holds a text of its own.
    let td = lib::layers(rects.iter().map(|(center, size, fill)| lib::rect(center.0, center.1, size.0, size.1, *fill, 0.0)).collect());
    let mut c = Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(SECS)) };
    c.id = ClipId(id(&format!("{name}.clip")));
    c
}

/// A sequence of `tracks` (bottom first) at `w` × `h`.
fn scene(name: &str, w: u32, h: u32, tracks: Vec<(TrackKind, Vec<Clip>)>) -> Sequence {
    let mut s = Sequence::new_default();
    s.id = SequenceId(id(name));
    s.name = name.into();
    s.resolution = Resolution { width: w, height: h };
    s.tracks = tracks.into_iter().enumerate().rev().map(|(n, (kind, clips))| {
        let mut t = Track::new(&format!("{} {}", if kind == TrackKind::Text { "T" } else { "V" }, n + 1), kind);
        t.clips = clips;
        t
    }).collect();
    s
}

/// Where the clip made as `name` is in `layout`, as `clip:<n>` counts:
/// every clip, track by track from the top.
fn clip_index(layout: &Sequence, name: &str) -> usize {
    layout.tracks.iter().flat_map(|t| &t.clips).position(|c| c.id == ClipId(id(name))).unwrap_or_else(|| panic!("{name} is in the layout"))
}

/// A template from its layout per frame: the wide one the main.
fn template(name: &str, category: &str, layouts: Vec<Sequence>, parameters: Vec<(&str, TemplateParameter)>, slots: Vec<(&str, &str)>, bindings: Vec<TemplateBinding>) -> CompositionTemplate {
    let mut t = CompositionTemplate::new(name, layouts[0].id.0);
    t.id = id(name);
    t.category = category.into();
    t.written_by = String::new();
    for (key, layout) in ["tall", "square"].iter().zip(&layouts[1..]) {
        t.formats.insert((*key).into(), layout.id.0);
    }
    t.sequences = layouts;
    t.parameters = parameters.into_iter().map(|(k, p)| (k.to_string(), p)).collect();
    t.slots = slots.into_iter().map(|(k, label)| (k.to_string(), TemplateSlot { label: label.into(), kind: SlotKind::Media })).collect();
    t.bindings = bindings;
    t.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
    t
}

/// Two pictures side by side (one above the other on a tall frame), a
/// line between them; the line's strength a control.
fn split_screen() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| {
        let (fw, fh) = (w as f32, h as f32);
        let (a, b, line) = if key == "tall" {
            ((0.0, 0.0, fw, fh / 2.0), (0.0, fh / 2.0, fw, fh / 2.0), ((0.5, 0.5), (1.0, 8.0 / fh)))
        } else {
            ((0.0, 0.0, fw / 2.0, fh), (fw / 2.0, 0.0, fw / 2.0, fh), ((0.5, 0.5), (8.0 / fw, 1.0)))
        };
        scene(&format!("Split Screen {key}"), w, h, vec![
            (TrackKind::Video, vec![slot_clip(&format!("split.{key}.a"), w, h, a)]),
            (TrackKind::Video, vec![slot_clip(&format!("split.{key}.b"), w, h, b)]),
            (TrackKind::Text, vec![shapes(&format!("split.{key}.line"), &[(line.0, line.1, [255, 255, 255, 255])])]),
        ])
    });
    let line = TemplateParameter { label: "Line".into(), default: TemplateValue::Number(1.0), range: Some(NumberRange { min: Some(0.0), max: Some(1.0) }), choices: Vec::new() };
    // A slot is bound by its clip's place in the layout, counted from the
    // top track down: found, not assumed.
    let first = clip_index(&layouts[0], "split.wide.a.clip");
    let second = clip_index(&layouts[0], "split.wide.b.clip");
    template(
        "Split Screen",
        "Layouts",
        layouts.into_iter().collect(),
        vec![("line", line)],
        vec![("slot_1", "First"), ("slot_2", "Second")],
        vec![
            TemplateBinding { parameter: "slot_1".into(), property: format!("clip:{first}.media"), link: None, take: None },
            TemplateBinding { parameter: "slot_2".into(), property: format!("clip:{second}.media"), link: None, take: None },
            TemplateBinding { parameter: "line".into(), property: "object:0.opacity".into(), link: Some(LinkMap { to: [0.0, 1.0], curve: klipp_core::LinkCurve::Linear, per: None }), take: None },
        ],
    )
}

// ── The rest of the set: titles designed in the library's own vocabulary
// and laid out for each frame by its own rules (`TitleTemplate::build_for`).

/// `create`'s title laid out for a `w` × `h` frame, as a built-in of
/// `category` grown from `anchor` is ("Utility" keeps a centred design at
/// its size on a tall frame, for one whose type is large already).
fn laid(create: fn() -> TitleData, category: &'static str, anchor: (f32, f32), w: u32, h: u32) -> TitleData {
    TitleTemplate { name: "", category, preview_text: "", create_data: create, fields: Fields { names: &[], accent: None, anchor } }.build_for(w, h)
}

/// A title clip of `td` from `from` to `to` seconds.
fn titled(name: &str, td: TitleData, from: f64, to: f64) -> Clip {
    let mut c = Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(from), tc(to)) };
    c.id = ClipId(id(&format!("{name}.clip")));
    c
}

/// A picture layer covering the box (normalised centre and size), on a
/// neutral backing so an empty one still shows where the picture goes.
fn photo(x: f32, y: f32, w: f32, h: f32, radius: f32) -> [Layer; 2] {
    let backing = lib::rect(x, y, w, h, [62, 64, 72, 255], radius);
    let image = (
        TitleObjectContent::Image(ImageLayer { path: String::new(), fit: ImageFit::Cover }),
        ObjectTransform { center: TitlePosition { x, y }, size: TitlePosition { x: w, y: h }, ..klipp_timeline::default_text_transform() },
        klipp_timeline::LayerFx::default(),
    );
    [backing, image]
}

/// Where object `k` of the title clip made as `clip` is, as `object:<i>`
/// counts: every title's objects, track by track from the top.
fn object_index(layout: &Sequence, clip: &str, k: usize) -> usize {
    let mut i = 0;
    for c in layout.tracks.iter().flat_map(|t| &t.clips) {
        if c.id == ClipId(id(&format!("{clip}.clip"))) {
            return i + k;
        }
        if let ClipKind::Title(td) = &c.kind {
            i += td.objects.len();
        }
    }
    panic!("{clip} is in the layout")
}

fn text_param(label: &str, default: &str) -> TemplateParameter {
    TemplateParameter { label: label.into(), default: TemplateValue::Text(default.into()), range: None, choices: Vec::new() }
}

fn control(label: &str, default: f64) -> TemplateParameter {
    TemplateParameter { label: label.into(), default: TemplateValue::Number(default), range: Some(NumberRange { min: Some(0.0), max: Some(1.0) }), choices: Vec::new() }
}

fn plain(parameter: &str, property: String) -> TemplateBinding {
    TemplateBinding { parameter: parameter.into(), property, link: None, take: None }
}

fn linked(parameter: &str, property: String, to: [f64; 2], curve: klipp_core::LinkCurve) -> TemplateBinding {
    TemplateBinding { parameter: parameter.into(), property, link: Some(LinkMap { to, curve, per: None }), take: None }
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const LINEAR: klipp_core::LinkCurve = klipp_core::LinkCurve::Linear;

/// A headline whose words rise into place one after another.
fn word_reveal_title() -> TitleData {
    lib::layers(vec![lib::text_at(
        lib::moved(lib::lifted(lib::upper(lib::face("Make every word land", lib::POSTER, FontStyle::Regular, 140.0, WHITE, TextAlignment::Center), 1.0)),
            TitleMotion::both(lib::enter(MotionKind::MaskRise, 0.5, Easing::OutCubic).stagger(StaggerUnit::Word, 0.09), lib::fade_out(0.3))),
        0.5, 0.5, 0.5,
    )])
}

fn word_reveal() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("Word Reveal {key}"), w, h, vec![(TrackKind::Text, vec![titled("word", laid(word_reveal_title, "Openers", (0.5, 0.5), w, h), 0.0, SECS)])]));
    let o = object_index(&layouts[0], "word", 0);
    template("Word Reveal", "Kinetic", layouts.into_iter().collect(),
        vec![("headline", text_param("Headline", "Make every word land")), ("pace", control("Pace", 0.4)), ("size", control("Size", 0.5))],
        vec![],
        vec![plain("headline", format!("object:{o}.text")),
             linked("pace", format!("object:{o}.motion.enter.duration_secs"), [0.25, 1.0], LINEAR),
             linked("size", format!("object:{o}.size"), [100.0, 180.0], LINEAR)])
}

/// Two lines, the first wiped on above a bar that draws out under it, the
/// second rising after.
fn line_wipe_title() -> TitleData {
    lib::layers(vec![
        lib::moving(lib::rect(0.5, 0.555, 0.30, 0.008, [74, 143, 231, 255], 0.0), TitleMotion::both(lib::enter(MotionKind::SlideRight, 0.5, Easing::OutCubic), lib::fade_out(0.3))),
        lib::text_at(lib::moved(lib::lifted(lib::upper(lib::face("Your first line", lib::DISPLAY, FontStyle::Regular, 120.0, WHITE, TextAlignment::Center), 2.0)),
            TitleMotion::both(lib::enter(MotionKind::MaskRise, 0.55, Easing::OutCubic).stagger(StaggerUnit::Line, 0.12), lib::fade_out(0.3))), 0.5, 0.46, 0.5),
        lib::text_at(lib::moved(lib::lifted(lib::face("and the line under it", lib::SANS, FontStyle::Bold, 50.0, WHITE, TextAlignment::Center)),
            TitleMotion::both(lib::enter(MotionKind::Rise, 0.5, Easing::OutCubic).delay(0.35), lib::fade_out(0.3))), 0.5, 0.63, 0.5),
    ])
}

fn line_wipe() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("Line Wipe {key}"), w, h, vec![(TrackKind::Text, vec![titled("wipe", laid(line_wipe_title, "Openers", (0.5, 0.5), w, h), 0.0, SECS)])]));
    let (bar, first, second) = (object_index(&layouts[0], "wipe", 0), object_index(&layouts[0], "wipe", 1), object_index(&layouts[0], "wipe", 2));
    template("Line Wipe", "Kinetic", layouts.into_iter().collect(),
        vec![("first", text_param("First line", "Your first line")), ("second", text_param("Second line", "and the line under it")), ("accent", TemplateParameter { label: "Accent colour".into(), default: TemplateValue::Color([74.0 / 255.0, 143.0 / 255.0, 231.0 / 255.0, 1.0]), range: None, choices: Vec::new() }), ("pace", control("Pace", 0.4))],
        vec![],
        vec![plain("first", format!("object:{first}.text")), plain("second", format!("object:{second}.text")), plain("accent", format!("object:{bar}.color")),
             linked("pace", format!("object:{first}.motion.enter.duration_secs"), [0.3, 1.1], LINEAR),
             linked("pace", format!("object:{second}.motion.enter.duration_secs"), [0.3, 1.0], LINEAR)])
}

/// A big number that pops in, a label under it.
fn number_card_title() -> TitleData {
    lib::layers(vec![
        lib::text_at(lib::moved(lib::lifted(lib::face("42", lib::POSTER, FontStyle::Regular, 300.0, [255, 214, 10, 255], TextAlignment::Center)),
            TitleMotion::both(lib::enter(MotionKind::Pop, 0.5, Easing::OutBack), lib::fade_out(0.3))), 0.5, 0.44, 0.5),
        lib::text_at(lib::moved(lib::lifted(lib::upper(lib::face("Days of sun a year", lib::LABEL, FontStyle::Bold, 44.0, WHITE, TextAlignment::Center), 3.0)),
            TitleMotion::both(lib::enter(MotionKind::Rise, 0.45, Easing::OutCubic).delay(0.25), lib::fade_out(0.3))), 0.5, 0.64, 0.5),
    ])
}

fn number_card() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("Number Card {key}"), w, h, vec![(TrackKind::Text, vec![titled("number", laid(number_card_title, "Utility", (0.5, 0.5), w, h), 0.0, SECS)])]));
    let (n, label) = (object_index(&layouts[0], "number", 0), object_index(&layouts[0], "number", 1));
    template("Number Card", "Numbers", layouts.into_iter().collect(),
        vec![("number", text_param("Number", "42")), ("label", text_param("Label", "Days of sun a year")), ("size", control("Size", 0.5))],
        vec![],
        vec![plain("number", format!("object:{n}.text")), plain("label", format!("object:{label}.text")),
             linked("size", format!("object:{n}.size"), [200.0, 400.0], LINEAR)])
}

fn count_title(digit: &'static str) -> fn() -> TitleData {
    match digit {
        "3" => || count_digit("3"),
        "2" => || count_digit("2"),
        _ => || count_digit("1"),
    }
}

fn count_digit(digit: &str) -> TitleData {
    lib::layers(vec![lib::text_at(
        lib::moved(lib::lifted(lib::face(digit, lib::POSTER, FontStyle::Regular, 380.0, WHITE, TextAlignment::Center)),
            TitleMotion::both(lib::enter(MotionKind::Slam, 0.3, Easing::OutCubic), lib::fade_out(0.2))),
        0.5, 0.5, 0.5,
    )])
}

/// Three, two, one: a second each.
fn countdown() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| {
        let clips = ["3", "2", "1"].iter().enumerate().map(|(n, d)| titled(&format!("count.{d}"), laid(count_title(d), "Openers", (0.5, 0.5), w, h), n as f64, n as f64 + 1.0)).collect();
        scene(&format!("Countdown {key}"), w, h, vec![(TrackKind::Text, clips)])
    });
    let objects: Vec<usize> = ["3", "2", "1"].iter().map(|d| object_index(&layouts[0], &format!("count.{d}"), 0)).collect();
    template("Countdown", "Numbers", layouts.into_iter().collect(),
        vec![("size", control("Size", 0.5))],
        vec![],
        objects.iter().map(|o| linked("size", format!("object:{o}.size"), [260.0, 500.0], LINEAR)).collect())
}

/// A heading and five lines that come in one after another.
fn top_five_title() -> TitleData {
    lib::layers(vec![
        lib::text_at(lib::moved(lib::lifted(lib::upper(lib::face("Top five", lib::DISPLAY, FontStyle::Regular, 96.0, [255, 214, 10, 255], TextAlignment::Left), 2.0)),
            TitleMotion::both(lib::enter(MotionKind::SlideRight, 0.45, Easing::OutCubic), lib::fade_out(0.3))), 0.12, 0.22, 0.0),
        lib::text_at(lib::moved(lib::lifted(lib::spaced(lib::face("1   The harbour at dawn\n2   The market\n3   The old town\n4   The promenade\n5   Sunset over the sea", lib::SANS, FontStyle::Bold, 52.0, WHITE, TextAlignment::Left), 1.35)),
            TitleMotion::both(lib::enter(MotionKind::Rise, 0.4, Easing::OutCubic).stagger(StaggerUnit::Line, 0.18).delay(0.3), lib::fade_out(0.3))), 0.12, 0.56, 0.0),
    ])
}

fn top_five() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("Top Five {key}"), w, h, vec![(TrackKind::Text, vec![titled("top", laid(top_five_title, "Captions", (0.12, 0.4), w, h), 0.0, SECS)])]));
    let (head, list) = (object_index(&layouts[0], "top", 0), object_index(&layouts[0], "top", 1));
    template("Top Five", "Numbers", layouts.into_iter().collect(),
        vec![("heading", text_param("Heading", "Top five")), ("list", text_param("List", "1   The harbour at dawn\n2   The market\n3   The old town\n4   The promenade\n5   Sunset over the sea")), ("pace", control("Pace", 0.4))],
        vec![],
        vec![plain("heading", format!("object:{head}.text")), plain("list", format!("object:{list}.text")),
             linked("pace", format!("object:{list}.motion.enter.duration_secs"), [0.25, 0.9], LINEAR)])
}

/// A full-frame picture, dimmed, with a hook line dropped over it.
fn hook_title() -> TitleData {
    lib::layers(vec![
        lib::rect(0.5, 0.5, 1.0, 1.0, [0, 0, 0, 255], 0.0),
        lib::text_at(lib::moved(lib::lifted(lib::upper(lib::face("Wait for the last one", lib::POSTER, FontStyle::Regular, 130.0, [255, 214, 10, 255], TextAlignment::Center), 1.5)),
            TitleMotion::both(lib::enter(MotionKind::Drop, 0.45, Easing::OutBack).distance(0.5).stagger(StaggerUnit::Word, 0.06), lib::fade_out(0.25))), 0.5, 0.42, 0.5),
    ])
}

fn hook() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("Hook Card {key}"), w, h, vec![
        (TrackKind::Video, vec![slot_clip(&format!("hook.{key}.picture"), w, h, (0.0, 0.0, w as f32, h as f32))]),
        (TrackKind::Text, vec![titled(&format!("hook.{key}"), laid(hook_title, "Social", (0.5, 0.5), w, h), 0.0, SECS)]),
    ]));
    let picture = clip_index(&layouts[0], "hook.wide.picture.clip");
    let (dim, line) = (object_index(&layouts[0], "hook.wide", 0), object_index(&layouts[0], "hook.wide", 1));
    template("Hook Card", "Social", layouts.into_iter().collect(),
        vec![("line", text_param("Hook line", "Wait for the last one")), ("dim", control("Dim", 0.7))],
        vec![("slot_1", "Picture")],
        vec![plain("slot_1", format!("clip:{picture}.media")), plain("line", format!("object:{line}.text")),
             linked("dim", format!("object:{dim}.opacity"), [0.0, 0.8], LINEAR)])
}

/// A photo, a closing line and a handle.
fn end_card_title() -> TitleData {
    let [backing, image] = photo(0.5, 0.36, 0.17, 0.30, 12.0);
    let mut items = vec![backing, image];
    items.push(lib::text_at(lib::moved(lib::lifted(lib::face("Thanks for watching", lib::HEADING, FontStyle::Regular, 76.0, WHITE, TextAlignment::Center)),
        TitleMotion::both(lib::enter(MotionKind::Rise, 0.5, Easing::OutCubic).delay(0.2), lib::fade_out(0.3))), 0.5, 0.62, 0.5));
    items.push(lib::text_at(lib::moved(lib::lifted(lib::face("@yourhandle", lib::GRO, FontStyle::Bold, 38.0, lib::MUTED, TextAlignment::Center)),
        TitleMotion::both(lib::enter(MotionKind::Fade, 0.5, Easing::OutCubic).delay(0.45), lib::fade_out(0.3))), 0.5, 0.73, 0.5));
    let mut td = lib::layers(items);
    for o in td.objects.iter_mut().take(2) {
        o.fx_mut().motion = TitleMotion::both(lib::enter(MotionKind::ScaleUp, 0.5, Easing::OutBack), lib::fade_out(0.3));
    }
    td
}

fn end_card() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("End Screen {key}"), w, h, vec![(TrackKind::Text, vec![titled("end", laid(end_card_title, "Utility", (0.5, 0.5), w, h), 0.0, SECS)])]));
    let (pic, message, handle) = (object_index(&layouts[0], "end", 1), object_index(&layouts[0], "end", 2), object_index(&layouts[0], "end", 3));
    template("End Screen", "Social", layouts.into_iter().collect(),
        vec![("photo", text_param("Photo", "")), ("message", text_param("Message", "Thanks for watching")), ("handle", text_param("Handle", "@yourhandle")), ("size", control("Size", 0.5))],
        vec![],
        vec![plain("photo", format!("object:{pic}.picture")), plain("message", format!("object:{message}.text")), plain("handle", format!("object:{handle}.text")),
             linked("size", format!("object:{message}.size"), [56.0, 96.0], LINEAR)])
}

/// A name bar with a square photo at its left.
fn photo_lower_third_title() -> TitleData {
    let [backing, image] = photo(0.115, 0.845, 0.065, 0.115, 6.0);
    let mut td = lib::layers(vec![
        lib::rect(0.2175, 0.845, 0.285, 0.115, [14, 14, 18, 215], 4.0),
        backing,
        image,
        lib::text_at(lib::moved(lib::keeps_line(lib::face("Your Name", lib::GRO, FontStyle::Bold, 46.0, WHITE, TextAlignment::Left)), TitleMotion::both(lib::enter(MotionKind::SlideRight, 0.45, Easing::OutCubic), lib::fade_out(0.25))), 0.165, 0.826, 0.0),
        lib::text_at(lib::moved(lib::upper(lib::face("Role or place", lib::LABEL, FontStyle::Regular, 28.0, lib::MUTED, TextAlignment::Left), 1.5), TitleMotion::both(lib::enter(MotionKind::Fade, 0.4, Easing::OutCubic).delay(0.2), lib::fade_out(0.25))), 0.165, 0.874, 0.0),
    ]);
    for o in td.objects.iter_mut().take(3) {
        o.fx_mut().motion = TitleMotion::both(lib::enter(MotionKind::SlideRight, 0.4, Easing::OutCubic), lib::fade_out(0.25));
    }
    td
}

/// A wide photo strip above a name and a role.
fn banner_lower_third_title() -> TitleData {
    let [backing, image] = photo(0.21, 0.70, 0.30, 0.12, 4.0);
    let mut td = lib::layers(vec![
        backing,
        image,
        lib::rect(0.21, 0.835, 0.30, 0.12, [250, 250, 252, 235], 4.0),
        lib::text_at(lib::moved(lib::keeps_line(lib::face("Your Name", lib::GRO, FontStyle::Bold, 44.0, lib::INK, TextAlignment::Left)), TitleMotion::both(lib::enter(MotionKind::Rise, 0.45, Easing::OutCubic), lib::fade_out(0.25))), 0.075, 0.818, 0.0),
        lib::text_at(lib::moved(lib::upper(lib::face("Role or place", lib::LABEL, FontStyle::Regular, 28.0, [70, 72, 80, 255], TextAlignment::Left), 1.5), TitleMotion::both(lib::enter(MotionKind::Fade, 0.4, Easing::OutCubic).delay(0.2), lib::fade_out(0.25))), 0.075, 0.862, 0.0),
    ]);
    for o in td.objects.iter_mut().take(3) {
        o.fx_mut().motion = TitleMotion::both(lib::enter(MotionKind::Rise, 0.4, Easing::OutCubic), lib::fade_out(0.25));
    }
    td
}

fn lower_third(name: &'static str, tag: &str, create: fn() -> TitleData, objects: (usize, usize, usize)) -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("{name} {key}"), w, h, vec![(TrackKind::Text, vec![titled(tag, laid(create, "Lower Thirds", (0.06, 0.845), w, h), 0.0, SECS)])]));
    let (pic, who, role) = (object_index(&layouts[0], tag, objects.0), object_index(&layouts[0], tag, objects.1), object_index(&layouts[0], tag, objects.2));
    template(name, "Lower Thirds", layouts.into_iter().collect(),
        vec![("photo", text_param("Photo", "")), ("name", text_param("Name", "Your Name")), ("role", text_param("Role", "Role or place"))],
        vec![],
        vec![plain("photo", format!("object:{pic}.picture")), plain("name", format!("object:{who}.text")), plain("role", format!("object:{role}.text"))])
}

/// Four pictures in a grid with a gap between them.
fn collage() -> CompositionTemplate {
    let gap = 12.0;
    let layouts = FRAMES.map(|(key, w, h)| {
        let (fw, fh) = (w as f32, h as f32);
        let (cw, ch) = ((fw - gap) / 2.0, (fh - gap) / 2.0);
        let boxes = [(0.0, 0.0), (cw + gap, 0.0), (0.0, ch + gap), (cw + gap, ch + gap)];
        let mut tracks = vec![(TrackKind::Text, vec![shapes(&format!("collage.{key}.ground"), &[((0.5, 0.5), (1.0, 1.0), [18, 18, 22, 255])])])];
        for (n, (x, y)) in boxes.iter().enumerate() {
            tracks.push((TrackKind::Video, vec![slot_clip(&format!("collage.{key}.{n}"), w, h, (*x, *y, cw, ch))]));
        }
        scene(&format!("Collage {key}"), w, h, tracks)
    });
    let clips: Vec<usize> = (0..4).map(|n| clip_index(&layouts[0], &format!("collage.wide.{n}.clip"))).collect();
    template("Collage", "Layouts", layouts.into_iter().collect(),
        vec![],
        vec![("slot_1", "First"), ("slot_2", "Second"), ("slot_3", "Third"), ("slot_4", "Fourth")],
        clips.iter().enumerate().map(|(n, c)| plain(&format!("slot_{}", n + 1), format!("clip:{c}.media"))).collect())
}

/// Two panels sweeping across the frame: a cut hidden under them.
fn stinger_title() -> TitleData {
    lib::layers(vec![
        lib::moving(lib::rect(0.5, 0.5, 1.0, 1.0, [74, 143, 231, 255], 0.0), TitleMotion::both(lib::enter(MotionKind::SlideRight, 0.35, Easing::OutCubic).distance(1.0), lib::enter(MotionKind::SlideRight, 0.35, Easing::InCubic).distance(1.0))),
        lib::moving(lib::rect(0.5, 0.5, 1.0, 1.0, [18, 18, 22, 255], 0.0), TitleMotion::both(lib::enter(MotionKind::SlideRight, 0.35, Easing::OutCubic).distance(1.0).delay(0.1), lib::enter(MotionKind::SlideRight, 0.3, Easing::InCubic).distance(1.0))),
    ])
}

fn stinger() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("Stinger {key}"), w, h, vec![(TrackKind::Text, vec![titled("sting", laid(stinger_title, "Utility", (0.5, 0.5), w, h), 0.0, 1.2)])]));
    let panel = object_index(&layouts[0], "sting", 0);
    template("Stinger", "Transitions", layouts.into_iter().collect(),
        vec![("colour", TemplateParameter { label: "Colour".into(), default: TemplateValue::Color([74.0 / 255.0, 143.0 / 255.0, 231.0 / 255.0, 1.0]), range: None, choices: Vec::new() })],
        vec![],
        vec![plain("colour", format!("object:{panel}.color"))])
}

/// A title coming into focus between bars that close in.
fn opener_title() -> TitleData {
    lib::layers(vec![
        lib::moving(lib::rect(0.5, 0.06, 1.0, 0.12, [0, 0, 0, 255], 0.0), TitleMotion::both(lib::enter(MotionKind::Drop, 0.6, Easing::OutCubic), lib::fade_out(0.4))),
        lib::moving(lib::rect(0.5, 0.94, 1.0, 0.12, [0, 0, 0, 255], 0.0), TitleMotion::both(lib::enter(MotionKind::Rise, 0.6, Easing::OutCubic), lib::fade_out(0.4))),
        lib::text_at(lib::moved(lib::lifted(lib::upper(lib::face("The Title", lib::HEADING, FontStyle::Regular, 120.0, WHITE, TextAlignment::Center), 1.0)),
            TitleMotion::both(lib::enter(MotionKind::Blur, 0.9, Easing::OutCubic).delay(0.3), lib::fade_out(0.4))), 0.5, 0.5, 0.5),
    ])
}

fn opener() -> CompositionTemplate {
    let layouts = FRAMES.map(|(key, w, h)| scene(&format!("Opener {key}"), w, h, vec![(TrackKind::Text, vec![titled("open", laid(opener_title, "Openers", (0.5, 0.5), w, h), 0.0, SECS)])]));
    let (top, bottom, title) = (object_index(&layouts[0], "open", 0), object_index(&layouts[0], "open", 1), object_index(&layouts[0], "open", 2));
    template("Opener", "Openers", layouts.into_iter().collect(),
        vec![("title", text_param("Title", "The Title")), ("bars", control("Bars", 1.0))],
        vec![],
        vec![plain("title", format!("object:{title}.text")),
             linked("bars", format!("object:{top}.opacity"), [0.0, 1.0], LINEAR),
             linked("bars", format!("object:{bottom}.opacity"), [0.0, 1.0], LINEAR)])
}

fn main() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/templates");
    std::fs::create_dir_all(&dir).expect("the templates folder");
    let all: BTreeMap<&str, CompositionTemplate> = [
        ("split_screen", split_screen()),
        ("word_reveal", word_reveal()),
        ("line_wipe", line_wipe()),
        ("number_card", number_card()),
        ("countdown", countdown()),
        ("top_five", top_five()),
        ("hook_card", hook()),
        ("end_screen", end_card()),
        ("photo_lower_third", lower_third("Photo Lower Third", "photo_lt", photo_lower_third_title, (2, 3, 4))),
        ("banner_lower_third", lower_third("Banner Lower Third", "banner_lt", banner_lower_third_title, (1, 3, 4))),
        ("collage", collage()),
        ("stinger", stinger()),
        ("opener", opener()),
    ]
    .into_iter()
    .collect();
    for (file, t) in all {
        let path = dir.join(format!("{file}.klipptemplate"));
        std::fs::write(&path, serde_json::to_string_pretty(&t).expect("a template writes") + "\n").expect("written");
        println!("wrote {}", path.display());
    }
}
