//! Layer stacks in the project file: where they are written, under
//! which tag, what an older reader does with them, and what happens
//! when the old text-box fields and a stack meet in one file.

use klipp_core::title_motion::{Motion, MotionKind, TitleLoop};
use klipp_core::Timecode;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::*;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn glow() -> TextGlow {
    TextGlow { enabled: true, color: [80, 200, 255, 255], radius: 12.0, radius_keyframes: vec![], intensity: 1.0, intensity_keyframes: vec![] }
}

fn lit() -> LayerFx {
    LayerFx {
        motion: TitleMotion::enter(Motion::new(MotionKind::Rise).duration(0.4)),
        loops: vec![TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.5 }],
        glows: vec![glow()],
        ..Default::default()
    }
}

fn text() -> TitleObjectContent {
    TitleObjectContent::TextBox(TextBox { text: "Layer".into(), ..Default::default() })
}
fn rectangle() -> TitleObjectContent {
    TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 8.0 }, fill: Some([40, 60, 90, 255]), gradient: None, stroke: None, fit: None })
}
fn image() -> TitleObjectContent {
    TitleObjectContent::Image(ImageLayer { path: "/nonexistent/logo.png".into(), fit: Default::default() })
}
fn path(trim: bool) -> TitleObjectContent {
    let mut p = VectorPath::default();
    if trim {
        p.trim = Some(PathTrim { range: TrimRange { start: 0.0, end: 0.5, offset: 0.0 }, keyframes: vec![] });
    }
    TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(p), fill: None, gradient: None, stroke: Some(ShapeStroke { color: [255; 4], width: 3.0 }), fit: None })
}

fn project_with(objects: Vec<(TitleObjectContent, LayerFx)>) -> Sequence {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    for (content, fx) in objects {
        let id = td.add_object(content, default_text_transform());
        td.object_mut(id).unwrap().fx = Some(fx);
    }
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let mut clip = Clip::gap(tc(0.0), tc(4.0));
    clip.kind = ClipKind::Title(td);
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip];
    seq
}

/// The title clip's kind as written, ids and all.
fn kind_of(dir: &std::path::Path) -> serde_json::Value {
    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    written["sequences"][0]["tracks"][0]["clips"][0]["kind"].clone()
}

fn tag_of(dir: &std::path::Path) -> String {
    kind_of(dir)["type"].as_str().unwrap().to_string()
}

fn loaded_title(dir: &std::path::Path) -> TitleData {
    let (sequences, ..) = load_project(dir).unwrap();
    let ClipKind::Title(td) = &sequences[0].tracks[0].clips[0].kind else { panic!("a title") };
    td.clone()
}

/// Every kind of layer, with nothing on it and with a stack on it,
/// comes back as saved — and the tag says what the file holds: `title`
/// for still text, `vector_title` for a path without a stack, and
/// `title_v2` the moment any layer carries one.
#[test]
fn every_layer_kind_round_trips_with_an_empty_and_a_lit_stack() {
    let cases: Vec<(&str, TitleObjectContent, &str, &str)> = vec![
        ("text", text(), "title", "title_v2"),
        ("rectangle", rectangle(), "title", "title_v2"),
        ("image", image(), "title", "title_v2"),
        ("path", path(false), "vector_title", "title_v2"),
        ("trimmed path", path(true), "vector_title", "title_v2"),
    ];
    for (name, content, empty_tag, lit_tag) in cases {
        for (fx, tag) in [(LayerFx::default(), empty_tag), (lit(), lit_tag)] {
            let dir = scratch(&format!("fx_{}", name.replace(' ', "_")));
            let seq = project_with(vec![(content.clone(), fx.clone())]);
            save_project(&dir, &[seq], &MediaPool::new()).unwrap();
            assert_eq!(tag_of(&dir), tag, "{name} with {} stack", if fx.is_default() { "an empty" } else { "a lit" });
            let td = loaded_title(&dir);
            assert_eq!(td.objects[0].fx.as_ref(), Some(&fx), "{name}: the stack comes back as saved");
            assert_eq!(std::mem::discriminant(&td.objects[0].content), std::mem::discriminant(&content), "{name}: the layer is still that kind");
            if let TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(p), .. }) = &td.objects[0].content {
                let TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(saved), .. }) = &content else { unreachable!() };
                assert_eq!(p.trim.is_some(), saved.trim.is_some(), "{name}: the trim is kept");
            }
            // A second save writes the same title: one shape, once.
            let first = kind_of(&dir);
            save_project(&dir, &[project_with(vec![(td.objects[0].content.clone(), td.objects[0].fx.clone().unwrap())])], &MediaPool::new()).unwrap();
            assert_eq!(kind_of(&dir), first, "{name}: the second save is the first");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// An explicitly empty stack beside old text-box effects in one file:
/// the stack wins, the old fields are dropped on save, and the second
/// load finds the stack still empty. The old fields are never a second
/// source.
#[test]
fn an_explicitly_empty_stack_wins_over_old_fields_beside_it() {
    let dir = scratch("fx_both");
    // Written by hand, as no build writes both: a `title_data` whose
    // text box still carries a glow the old way, and whose object has
    // an empty stack the new way.
    let mut td = TitleData::single_text(TextBox { text: "Both".into(), ..Default::default() });
    td.objects[0].fx = Some(LayerFx::default());
    let mut value = serde_json::to_value(&td).unwrap();
    value["objects"][0]["content"]["TextBox"]["glows"] = serde_json::to_value(vec![glow()]).unwrap();
    value["objects"][0]["content"]["TextBox"]["loops"] = serde_json::to_value(vec![TitleLoop::Pulse { rate_hz: 2.0, min_opacity: 0.2 }]).unwrap();
    let seq = project_with(vec![(text(), LayerFx::default())]);
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let mut file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    file["sequences"][0]["tracks"][0]["clips"][0]["kind"]["title_data"] = value;
    std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&file).unwrap()).unwrap();

    let td = loaded_title(&dir);
    assert_eq!(td.objects[0].fx.as_ref(), Some(&LayerFx::default()), "the explicit empty stack wins");
    let TitleObjectContent::TextBox(tb) = &td.objects[0].content else { unreachable!() };
    assert!(tb.legacy_fx.is_default(), "the old fields are gone from the text box");

    save_project(&dir, &[project_with(vec![(td.objects[0].content.clone(), LayerFx::default())])], &MediaPool::new()).unwrap();
    let kind = kind_of(&dir);
    let text_box = &kind["title_data"]["objects"][0]["content"]["TextBox"];
    assert!(text_box.get("glows").is_none() && text_box.get("loops").is_none(), "a save writes no old fields on the text box: {text_box}");
    assert_eq!(kind["title_data"]["objects"][0]["fx"], serde_json::json!({}), "the explicit empty stack is written");
    assert_eq!(kind["glows"], serde_json::json!([]), "and the flat fields an older reader looks at are empty");
    assert_eq!(tag_of(&dir), "title", "still and bare, under the old tag");
    let again = loaded_title(&dir);
    assert_eq!(again.objects[0].fx.as_ref(), Some(&LayerFx::default()), "and stays empty on the second load");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The old text-box fields, read straight from a `title_data` value
/// (as an asset or a payload would be), come up onto the object when
/// the title is wrapped or migrated — not rendered from where they lay.
#[test]
fn old_text_box_fields_read_directly_are_hoisted_onto_the_object() {
    let legacy = serde_json::json!({
        "text": "Old", "font_size": 72.0, "font_name": "Inter", "color": [255, 255, 255, 255], "alignment": "Center",
        "glows": [glow()],
        "motion": { "enter": { "kind": "Rise", "duration_secs": 0.4, "delay_secs": 0.0, "easing": "OutCubic", "stagger": "None", "stagger_secs": 0.05, "distance": 1.0 }, "exit": null }
    });
    let tb: TextBox = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(tb.legacy_fx.glows.len(), 1, "the reader keeps them");
    let td = TitleData::single_text(tb);
    assert_eq!(td.objects[0].fx().glows.len(), 1, "wrapping hoists them");
    assert!(td.objects[0].fx().motion.enter.is_some());

    // A whole title read directly, with no stacks at all, migrates once.
    let mut whole: TitleData = serde_json::from_value(serde_json::json!({
        "objects": [{ "id": 0, "transform": default_text_transform(), "content": { "TextBox": legacy } }],
        "next_object_id": 1
    })).unwrap();
    assert!(whole.objects[0].fx.is_none(), "read directly, the stack is not there yet");
    let m = whole.migrate_fx();
    assert!(m.changed && m.dropped_legacy == 0);
    assert_eq!(whole.objects[0].fx().glows.len(), 1);
    assert!(!whole.migrate_fx().changed, "and only once");
}

/// A saved template or a text box written by any code that still
/// writes the old fields cannot keep effects only there: the renderer
/// reads the object, and the object takes them.
#[test]
fn nothing_renders_from_the_old_place() {
    let mut tb = TextBox::default();
    tb.legacy_fx.glows.push(glow());
    let td = TitleData::single_text(tb);
    assert_eq!(td.glows().len(), 1, "the accessors the renderer uses see the glow on the object");
    let TitleObjectContent::TextBox(inner) = &td.objects[0].content else { unreachable!() };
    assert!(inner.legacy_fx.is_default(), "and nothing stays behind");
}

/// #1509: a gradient of more stops at an angle on a text, a radial one
/// on a shape, and the old two colours beside them, through a real save
/// and a strict load; the two colours are written as they always were.
#[test]
fn gradient_stops_round_trip() {
    let stop = |position: f32, color: [u8; 4]| GradientStop { position, color };
    let three = TextGradient::with_stops(&[stop(0.0, [255, 0, 0, 255]), stop(0.4, [0, 255, 0, 200]), stop(1.0, [0, 0, 255, 255])], GradientKind::Linear { angle_deg: 45.0 }, true);
    let radial = TextGradient::with_stops(&[stop(0.0, [255; 4]), stop(1.0, [10, 20, 30, 0])], GradientKind::Radial { centre: [0.3, 0.6], radius: 0.7 }, false);
    let pair = TextGradient::pair([1, 2, 3, 255], [4, 5, 6, 255], true);
    let with = |g: &TextGradient| TitleObjectContent::TextBox(TextBox { text: "Fill".into(), gradient: Some(g.clone()), ..Default::default() });
    let shape = TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: None, gradient: Some(radial.clone()), stroke: None, fit: None });
    let seq = project_with(vec![(with(&three), LayerFx::default()), (shape, LayerFx::default()), (with(&pair), LayerFx::default())]);
    let dir = scratch("gradient_stops");
    klipp_project::save_project(&dir, std::slice::from_ref(&seq), &MediaPool::new()).unwrap();
    let written = kind_of(&dir);
    let objects = &written["title_data"]["objects"];
    assert_eq!(objects[2]["content"]["TextBox"]["gradient"], serde_json::json!({"top_color": [1, 2, 3, 255], "bottom_color": [4, 5, 6, 255], "enabled": true}), "the pair as it was");
    assert_eq!(objects[0]["content"]["TextBox"]["gradient"]["stops"].as_array().map(Vec::len), Some(3));
    let (sequences, ..) = klipp_project::load_project_strict(&dir).expect("a strict reload");
    let ClipKind::Title(td) = &sequences[0].tracks[0].clips[0].kind else { panic!("a title") };
    let ClipKind::Title(saved) = &seq.tracks[0].clips[0].kind else { unreachable!() };
    assert_eq!(td.objects, saved.objects);
    let TitleObjectContent::TextBox(tb) = &td.objects[0].content else { panic!("text") };
    assert_eq!(tb.gradient.as_ref(), Some(&three));
    let TitleObjectContent::Shape(s) = &td.objects[1].content else { panic!("a shape") };
    assert_eq!(s.gradient.as_ref(), Some(&radial));
    let _ = std::fs::remove_dir_all(&dir);
}
