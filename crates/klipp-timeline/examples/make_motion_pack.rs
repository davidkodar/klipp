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
use klipp_timeline::{Clip, ClipKind, ObjectTransform, Sequence, Shape, ShapeKind, TitleData, TitleObjectContent, TitlePosition, Track, TrackKind};
use uuid::Uuid;

const SECS: f64 = 5.0;
const SOURCE_ASPECT: f32 = 16.0 / 9.0;

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
/// frame, as a 16:9 picture would: scaled to cover it and cropped to it.
fn slot_clip(name: &str, fw: u32, fh: u32, b: (f32, f32, f32, f32)) -> Clip {
    let (fw, fh) = (fw as f32, fh as f32);
    // The picture as it fits the frame, then covering the box.
    let (pw, ph) = if fw / fh > SOURCE_ASPECT { (fh * SOURCE_ASPECT, fh) } else { (fw, fw / SOURCE_ASPECT) };
    let scale = (b.2 / pw).max(b.3 / ph);
    let (sw, sh) = (pw * scale, ph * scale);
    let mut c = Clip::media(MediaId(id(name)), tc(0.0), tc(SECS), tc(0.0), tc(SECS));
    c.id = ClipId(id(&format!("{name}.clip")));
    c.push_effect(BuiltinEffect::Crop { left: (sw - b.2) / 2.0 / sw, right: (sw - b.2) / 2.0 / sw, top: (sh - b.3) / 2.0 / sh, bottom: (sh - b.3) / 2.0 / sh, feather: 0.0 });
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
    let mut td = TitleData::default();
    for (center, size, fill) in rects {
        td.add_object(
            TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fit: None, fill: Some(*fill), gradient: None, stroke: None }),
            ObjectTransform { center: TitlePosition { x: center.0, y: center.1 }, size: TitlePosition { x: size.0, y: size.1 }, ..klipp_timeline::default_text_transform() },
        );
    }
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
    template(
        "Split Screen",
        "Layouts",
        layouts.into_iter().collect(),
        vec![("line", line)],
        vec![("slot_1", "First"), ("slot_2", "Second")],
        vec![
            TemplateBinding { parameter: "slot_1".into(), property: "clip:0.media".into(), link: None },
            TemplateBinding { parameter: "slot_2".into(), property: "clip:1.media".into(), link: None },
            TemplateBinding { parameter: "line".into(), property: "object:0.opacity".into(), link: Some(LinkMap { to: [0.0, 1.0], curve: klipp_core::LinkCurve::Linear, per: None }) },
        ],
    )
}

fn main() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/templates");
    std::fs::create_dir_all(&dir).expect("the templates folder");
    let all: BTreeMap<&str, CompositionTemplate> = [("split_screen", split_screen())].into_iter().collect();
    for (file, t) in all {
        let path = dir.join(format!("{file}.klipptemplate"));
        std::fs::write(&path, serde_json::to_string_pretty(&t).expect("a template writes") + "\n").expect("written");
        println!("wrote {}", path.display());
    }
}
