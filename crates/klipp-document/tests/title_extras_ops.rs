//! The title features the inspector has, through the ops (#1701):
//! animators, keys on a text's type and colour, and text on a path.

use klipp_core::title_motion::{SelectOrder, SelectUnit};
use klipp_document::{
    apply::DocumentView,
    ops::{self, Op},
};
use klipp_media::MediaPool;
use klipp_core::ClipId;
use klipp_timeline::{Sequence, TextPath, TitleData, TitleObjectContent};
use serde_json::{json, Value};

struct Doc {
    sequences: Vec<Sequence>,
    pool: MediaPool,
}

impl Doc {
    fn new() -> Self {
        let mut seq = Sequence::new_default();
        seq.tracks[0].clips.push(klipp_timeline::Clip::gap(klipp_core::Timecode::from_secs_f64(0.0, 24), klipp_core::Timecode::from_secs_f64(4.0, 24)));
        Doc { sequences: vec![seq], pool: MediaPool::new() }
    }

    /// Resolve and apply `ops`, or the refusal's why and hint.
    fn run(&mut self, ops: Value) -> Result<Vec<klipp_document::history::EditCommand>, String> {
        let ops: Vec<Op> = serde_json::from_value(ops).map_err(|e| e.to_string())?;
        let plan = ops::resolve(&self.sequences[0], &self.pool, &ops).map_err(|r| format!("{} | {}", r.why, r.hint))?;
        let mut fps = self.sequences[0].fps;
        let mut doc = DocumentView { sequences: &mut self.sequences, active: 0, media_pool: &mut self.pool, fps: &mut fps };
        let mut edits = Vec::new();
        for s in &plan.steps {
            doc.apply_forward(&s.edit);
            edits.push(s.edit.clone());
        }
        Ok(edits)
    }

    fn undo(&mut self, edits: &[klipp_document::history::EditCommand]) {
        let mut fps = self.sequences[0].fps;
        let mut doc = DocumentView { sequences: &mut self.sequences, active: 0, media_pool: &mut self.pool, fps: &mut fps };
        for e in edits.iter().rev() {
            doc.apply_inverse(e);
        }
    }

    fn title(&self) -> (ClipId, &TitleData) {
        let c = self.sequences[0].tracks.iter().flat_map(|t| &t.clips).find(|c| c.title_data().is_some()).expect("a title");
        (c.id, c.title_data().unwrap())
    }
}

fn titled(text: &str) -> (Doc, ClipId) {
    let mut d = Doc::new();
    d.run(json!([{"op":"title","text":text,"at":{"secs":0},"secs":4}])).unwrap();
    let id = d.title().0;
    (d, id)
}

/// The issue's case: one style op turns the middle word letter by letter
/// in a seeded random order and keys its tracking from 0 to 200; undo
/// gives the title back as it was.
#[test]
fn one_style_op_turns_the_middle_word_and_keys_its_tracking() {
    let (mut d, id) = titled("one MIDDLE two");
    let before = d.title().1.clone();
    let edits = d
        .run(json!([{"op":"style","clip_id":id,
            "animators":[{"unit":"char","order":"random","seed":7,"rotate":90,"within":{"start":0.25,"end":0.75},
                          "start_keys":[{"at":0,"value":0},{"at":2,"value":1}]}],
            "keys":{"tracking":[{"at":0,"value":0},{"at":2,"value":200}]}}]))
        .unwrap();
    let td = d.title().1;
    let a = &td.primary_fx().animators[0];
    assert_eq!((a.selector.unit, a.selector.order, a.props.rotation), (SelectUnit::Char, SelectOrder::Random { seed: 7 }, 90.0));
    assert_eq!(a.within.as_ref().map(|w| (w.start, w.end)), Some((0.25, 0.75)));
    assert_eq!(a.selector.start_keyframes.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>(), [(0.0, 0.0), (2.0, 1.0)]);
    let tb = td.primary_text().unwrap();
    assert_eq!(tb.tracking_keyframes.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>(), [(0.0, 0.0), (2.0, 200.0)]);
    d.undo(&edits);
    assert_eq!(d.title().1, &before);
}

/// A preset by name, a field over it; `[]` clears the animators.
#[test]
fn a_preset_takes_overrides_and_an_empty_list_clears() {
    let (mut d, id) = titled("Spin");
    d.run(json!([{"op":"style","clip_id":id,"animators":[{"preset":"spin_in","rotate":180},{"preset":"wave","enabled":false}]}])).unwrap();
    let fx = d.title().1.primary_fx().clone();
    let spin = klipp_core::title_motion::animator_preset("spin_in", 4.0).unwrap();
    assert_eq!(fx.animators[0].props.rotation, 180.0);
    assert_eq!((fx.animators[0].props.scale, &fx.animators[0].selector), (spin.props.scale, &spin.selector), "the rest is the preset's");
    assert!(!fx.animators[1].enabled);
    d.run(json!([{"op":"style","clip_id":id,"animators":[]}])).unwrap();
    assert!(d.title().1.primary_fx().animators.is_empty());
}

/// A title op keys its colour and its gradient's stops as it is made.
#[test]
fn a_new_title_keys_its_colour_and_gradient_stops() {
    let mut d = Doc::new();
    d.run(json!([{"op":"title","text":"Keyed","at":{"secs":0},"secs":4,
        "gradient":{"stops":[[0.0,"#ff0000"],[0.5,"#00ff00"],[1.0,"#0000ff"]]},
        "keys":{"color":[{"at":0,"color":"#ffffff"},{"at":3,"color":"#ff8800"}],
                "gradient_stops":{"2":[{"at":0,"color":"#0000ff"},{"at":2,"color":"#ffffff"}]},
                "size":[{"at":1,"value":60}],"line_spacing":[{"at":0,"value":1.0},{"at":2,"value":1.5}]}}]))
        .unwrap();
    let tb = d.title().1.primary_text().unwrap().clone();
    assert_eq!(tb.color_keyframes.r.len(), 2);
    let g = tb.gradient.as_ref().unwrap();
    assert!(g.stop_keyframes.get(2).is_some_and(|k| k.b.len() == 2) && g.stop_keyframes[..2].iter().all(|k| k.is_empty()));
    assert_eq!((tb.font_size_keyframes.len(), tb.line_spacing_keyframes.len()), (1, 2));
    // Halfway from blue to white, in linear light.
    assert_eq!(tb.at(1.0).gradient.unwrap().stops[2].color, [188, 188, 255, 255]);
}

/// Text on a path layer of the same title, by its id; "none" lays it
/// straight again.
#[test]
fn a_style_op_puts_the_text_on_a_path_layer() {
    let (mut d, id) = titled("Along the curve");
    d.run(json!([{"op":"vector_path","clip_id":id,"shape":"ellipse","x":0.5,"y":0.5,"w":0.6,"h":0.4}])).unwrap();
    let path_id = d.title().1.objects.iter().find(|o| matches!(o.content, TitleObjectContent::Shape(_))).unwrap().id;
    d.run(json!([{"op":"style","clip_id":id,"on_path":{"layer":path_id.0,"offset":0.2,"align":"center","offset_keys":[{"at":0,"value":0.2},{"at":4,"value":0.7}]}}])).unwrap();
    let tp = d.title().1.primary_text().unwrap().text_path.clone();
    assert!(
        matches!(&tp, Some(TextPath::Layer { layer, offset, align: klipp_timeline::PathTextAlign::Center, offset_keyframes, flip: false, contour: 0 }) if *layer == path_id && *offset == 0.2 && offset_keyframes.len() == 2),
        "{tp:?}"
    );
    d.run(json!([{"op":"style","clip_id":id,"on_path":"none"}])).unwrap();
    assert_eq!(d.title().1.primary_text().unwrap().text_path, None);
}

/// Every wrong value is refused before anything changes, with the
/// choices or the range.
#[test]
fn wrong_values_are_refused_with_the_choices() {
    let (mut d, id) = titled("Wrong");
    let before = d.title().1.clone();
    let style = |extra: Value| {
        let mut op = json!({"op":"style","clip_id":id});
        op.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        json!([op])
    };
    for (extra, says) in [
        (json!({"animators":[{"preset":"twirl"}]}), "wave, random_fade_in, spin_in"),
        (json!({"animators":[{"unit":"glyph"}]}), "char, word or line"),
        (json!({"animators":[{"shape":"zigzag"}]}), "ramp_up"),
        (json!({"animators":[{"order":"sideways"}]}), "centre or random"),
        (json!({"animators":[{"seed":3}]}), "random with the seed"),
        (json!({"animators":[{"order":"reverse","seed":3}]}), "random with the seed"),
        (json!({"animators":[{"amount":2}]}), "-1 to 1"),
        (json!({"animators":[{"start":1.5}]}), "start from 0 to 1"),
        (json!({"animators":[{"rotate":9999}]}), "rotate (degrees) from"),
        (json!({"animators":[{"easing":"wobbly"}]}), "cubic-bezier"),
        (json!({"animators":[{"color":"teal"}]}), "#rrggbb"),
        (json!({"keys":{"tracking":[{"at":9,"value":10}]}}), "after the title's end"),
        (json!({"keys":{"tracking":[{"at":-1,"value":10}]}}), "0 or later"),
        (json!({"keys":{"size":[{"at":1,"value":2}]}}), "size keys from 4"),
        (json!({"keys":{"gradient_stops":{"0":[{"at":1,"color":"#ffffff"}]}}}), "without a gradient"),
        (json!({"keys":{"gradient_stops":{"first":[{"at":1,"color":"#ffffff"}]}}}), "index from 0"),
        (json!({"on_path":{"layer":0}}), "it has none; add one with vector_path"),
        (json!({"on_path":"straight"}), "\"none\""),
        (json!({"on_path":{"layer":0,"align":"middle"}}), "start, center or end"),
    ] {
        let why = d.run(style(extra.clone())).expect_err(&format!("{extra} is refused"));
        assert!(why.contains(says), "{extra}: {why}");
        assert_eq!(d.title().1, &before, "{extra}: nothing changed");
    }
    // A gradient's stop that isn't there, and a layer that isn't a path.
    d.run(json!([{"op":"style","clip_id":id,"gradient":["#ff0000","#0000ff"]}])).unwrap();
    let why = d.run(style(json!({"keys":{"gradient_stops":{"2":[{"at":1,"color":"#ffffff"}]}}}))).unwrap_err();
    assert!(why.contains("has 2 stops; there is no stop 2"), "{why}");
    d.run(json!([{"op":"vector_path","clip_id":id,"shape":"ellipse"}])).unwrap();
    let text_id = d.title().1.objects[0].id.0;
    let why = d.run(style(json!({"on_path":{"layer":text_id}}))).unwrap_err();
    assert!(why.contains("is not a path layer of this title; its path layers are"), "{why}");
    let path_id = d.title().1.objects.last().unwrap().id.0;
    let why = d.run(style(json!({"on_path":{"layer":path_id,"contour":3}}))).unwrap_err();
    assert!(why.contains("there is no contour 3"), "{why}");
    // A new title has no path of its own.
    let why = d.run(json!([{"op":"title","text":"x","at":{"secs":0},"secs":2,"track":0,"on_path":{"layer":0}}])).unwrap_err();
    assert!(why.contains("vector_path"), "{why}");
}

/// A text layer blends with the layers below it and is masked by
/// another layer of its title (#1513); "none" takes the mask off.
#[test]
fn a_style_op_sets_the_texts_blend_and_mask() {
    let (mut d, id) = titled("Masked");
    d.run(json!([{"op":"vector_path","clip_id":id,"shape":"ellipse"}])).unwrap();
    let path = d.title().1.objects.last().unwrap().id;
    d.run(json!([{"op":"style","clip_id":id,"blend":"color_dodge","mask":{"layer":path.0,"kind":"inverted_luma"}}])).unwrap();
    let text = d.title().1.objects[0].clone();
    assert_eq!(text.blend, klipp_core::BlendMode::ColorDodge);
    assert_eq!(text.mask, Some(klipp_timeline::LayerMask { source: path, kind: klipp_core::layer_mask::LayerMaskKind::InvertedLuma }));
    assert_eq!(d.title().1.objects.last().unwrap().blend, klipp_core::BlendMode::Normal, "only the text's");
    d.run(json!([{"op":"style","clip_id":id,"mask":"none","blend":"Soft Light"}])).unwrap();
    let text = &d.title().1.objects[0];
    assert_eq!((text.mask, text.blend), (None, klipp_core::BlendMode::SoftLight));
    let before = d.title().1.clone();
    for (op, says) in [
        (json!({"blend":"glow"}), "normal, add, multiply"),
        (json!({"mask":{"layer":path.0,"kind":"shadow"}}), "alpha, inverted_alpha, luma or inverted_luma"),
        (json!({"mask":{"layer":99}}), "has no layer 99; its other layers are"),
        (json!({"mask":{"layer":text.id.0}}), "cannot mask itself"),
        (json!({"mask":"off"}), "\"none\""),
    ] {
        let mut o = json!({"op":"style","clip_id":id});
        o.as_object_mut().unwrap().extend(op.as_object().unwrap().clone());
        let why = d.run(json!([o])).expect_err(&format!("{op}"));
        assert!(why.contains(says), "{op}: {why}");
        assert_eq!(d.title().1, &before);
    }
    // A new title has only its text: a blend, but nothing to mask with.
    let mut fresh = Doc::new();
    fresh.run(json!([{"op":"title","text":"Add","at":{"secs":0},"secs":2,"blend":"screen"}])).unwrap();
    assert_eq!(fresh.title().1.objects[0].blend, klipp_core::BlendMode::Screen);
    let why = Doc::new().run(json!([{"op":"title","text":"x","at":{"secs":0},"secs":2,"mask":{"layer":5}}])).unwrap_err();
    assert!(why.contains("no other layer"), "{why}");
}

/// #1512: a style op turns the text 3D by a look with fields over it,
/// keys its turn, changes it again without losing the rest, and takes
/// it off; a new title can be 3D from the start.
#[test]
fn a_style_op_turns_a_text_3d() {
    let (mut d, id) = titled("Solid");
    d.run(json!([{"op":"style","clip_id":id,"three_d":{"look":"chrome","depth":0.4,"turn":20,"light":{"azimuth":-30,"elevation":40},"keys":{"turn":[{"at":0,"value":-45},{"at":3,"value":45}]}}}])).unwrap();
    let t = d.title().1.primary_text().unwrap().three_d.clone().expect("3D");
    let chrome = klipp_core::text3d::Text3d::default().with_look("chrome").unwrap();
    assert!(t.enabled);
    assert_eq!((t.depth, t.rotation[1], t.light.azimuth, t.light.elevation), (0.4, 20.0, -30.0, 40.0));
    assert_eq!((t.face_color, t.front_bevel, t.light.specular), (chrome.face_color, chrome.front_bevel, chrome.light.specular), "the rest is chrome's");
    assert_eq!(t.keys.rotate_y.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>(), [(0.0, -45.0), (3.0, 45.0)]);
    // Another op changes what it names and keeps the rest.
    d.run(json!([{"op":"style","clip_id":id,"three_d":{"bevel":"none","perspective":false,"side_color":"#102030"}}])).unwrap();
    let t2 = d.title().1.primary_text().unwrap().three_d.clone().unwrap();
    assert_eq!((t2.front_bevel, t2.camera.orthographic, t2.side_color, t2.depth, t2.keys.rotate_y.len()), (None, true, [16, 32, 48, 255], 0.4, 2));
    d.run(json!([{"op":"style","clip_id":id,"three_d":"none"}])).unwrap();
    assert_eq!(d.title().1.primary_text().unwrap().three_d, None);
    let mut fresh = Doc::new();
    fresh.run(json!([{"op":"title","text":"Block","at":{"secs":0},"secs":2,"three_d":{"look":"block"}}])).unwrap();
    assert_eq!(fresh.title().1.primary_text().unwrap().three_d.as_ref().and_then(|t| t.look()), Some("block"));
}

/// Wrong 3D values are refused with the choices or the range, nothing
/// changed.
#[test]
fn wrong_3d_values_are_refused() {
    let (mut d, id) = titled("Wrong 3D");
    let before = d.title().1.clone();
    for (three_d, says) in [
        (json!({"look":"marble"}), "block, bevelled, chrome"),
        (json!({"depth":5}), "depth (ems) from 0 to 2"),
        (json!({"bevel":{"size":0.5}}), "bevel size (ems) from 0 to 0.2"),
        (json!({"bevel":{"size":0.02,"profile":"ogee"}}), "straight or round"),
        (json!({"bevel":{"size":0.02,"segments":40}}), "segments from 1 to 12"),
        (json!({"fov":0}), "fov (degrees) from 1 to 170"),
        (json!({"light":{"elevation":120}}), "light elevation (degrees) from -90 to 90"),
        (json!({"light":{"glare":1}}), "unknown field"),
        (json!({"face_color":"gold"}), "#rrggbb"),
        (json!({"keys":{"spin":[{"at":0,"value":1}]}}), "key turn, tilt, roll"),
        (json!({"keys":{"turn":[{"at":9,"value":1}]}}), "after the title's end"),
        (json!("sideways"), "\"none\""),
        (json!({"bevel":{"size":0.02,"depth":1}}), "unknown field `depth`"),
    ] {
        let why = d.run(json!([{"op":"style","clip_id":id,"three_d":three_d}])).expect_err(&format!("{three_d}"));
        assert!(why.contains(says), "{three_d}: {why}");
        assert_eq!(d.title().1, &before);
    }
    // A name-or-fields spec's own error reaches the reply, not "did not
    // match any variant".
    for (field, value) in [("on_path", json!({"layer":0,"wobble":1})), ("mask", json!({"layer":0,"opacity":1}))] {
        let why = d.run(json!([{"op":"style","clip_id":id, field: value}])).unwrap_err();
        assert!(why.contains("unknown field") && !why.contains("any variant"), "{field}: {why}");
    }
}

/// #2706: a style op makes a text metal by a preset's name, lays fields
/// over it, and takes it off; the colour a frame without a GPU shows is
/// the metal's; a new title can be gold from the start.
#[test]
fn a_style_op_makes_a_text_metal() {
    use klipp_core::title_material::{MetalPreset, TitleMaterial};
    let (mut d, id) = titled("Klipp");
    d.run(json!([{"op":"style","clip_id":id,"material":"gold"}])).unwrap();
    let text = d.title().1.primary_text().unwrap().clone();
    assert_eq!(text.material, Some(TitleMaterial::preset(MetalPreset::Gold)));
    assert_eq!(text.color, TitleMaterial::preset(MetalPreset::Gold).fallback_color());
    // Fields over the text's metal; a light from the low sun on the right.
    d.run(json!([{"op":"style","clip_id":id,"material":{"roughness":0.4,"key_light":{"azimuth":-10,"elevation":15,"strength":1.5,"color":"#ffd8a0"},"rim_light":"none","environment":{"studio":0.2,"sky":1.0,"sky_tint":"#fff0e0"}}}])).unwrap();
    let m = d.title().1.primary_text().unwrap().material.unwrap();
    assert_eq!((m.preset, m.roughness, m.key_light.azimuth, m.key_light.elevation, m.key_light.strength, m.key_light.color), (MetalPreset::Gold, 0.4, -10.0, 15.0, 1.5, [255, 216, 160]));
    assert_eq!((m.rim_light, m.environment.studio, m.environment.sky, m.environment.sky_tint), (None, 0.2, 1.0, [255, 240, 224]));
    // A preset named starts from that metal.
    d.run(json!([{"op":"style","clip_id":id,"material":{"preset":"iron","glints":0}}])).unwrap();
    let m = d.title().1.primary_text().unwrap().material.unwrap();
    assert_eq!((m.preset, m.glints, m.roughness), (MetalPreset::Steel, 0.0, TitleMaterial::preset(MetalPreset::Steel).roughness));
    d.run(json!([{"op":"style","clip_id":id,"material":"none"}])).unwrap();
    assert_eq!(d.title().1.primary_text().unwrap().material, None);
    let mut fresh = Doc::new();
    fresh.run(json!([{"op":"title","text":"Klipp","at":{"secs":0},"secs":2,"material":{"preset":"chrome","tint":"#e0e8ff"}}])).unwrap();
    let m = fresh.title().1.primary_text().unwrap().material.unwrap();
    assert_eq!(m.preset, MetalPreset::Chrome);
    assert!(m.tint[2] > m.tint[0], "a blue-white tint: {:?}", m.tint);
}

/// Wrong metal values are refused with the choices or the range, nothing
/// changed; a 3D text is not made metal yet.
#[test]
fn wrong_metal_values_are_refused() {
    let (mut d, id) = titled("Wrong metal");
    let before = d.title().1.clone();
    for (material, says) in [
        (json!("tin"), "gold, silver, steel, copper, rose_gold, bronze, chrome, brushed_metal"),
        (json!({"roughness":2}), "roughness from 0 to 1"),
        (json!({"bevel":0.5}), "bevel from 0 to 0.3"),
        (json!({"key_light":{"strength":9}}), "key_light strength from 0 to 4"),
        (json!({"tint":"gold"}), "#rrggbb"),
        (json!({"rim_light":"soft"}), "\"none\""),
        (json!({"shine":1}), "unknown field"),
    ] {
        let why = d.run(json!([{"op":"style","clip_id":id,"material":material}])).unwrap_err();
        assert!(why.contains(says), "{material}: {why}");
    }
    assert_eq!(d.title().1, &before);
    d.run(json!([{"op":"style","clip_id":id,"three_d":{"look":"block"}}])).unwrap();
    let why = d.run(json!([{"op":"style","clip_id":id,"material":"gold"}])).unwrap_err();
    assert!(why.contains("flat text"), "{why}");
}
