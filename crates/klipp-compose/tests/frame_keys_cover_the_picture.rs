//! The frame keys (#3265) — `frame_key`, which keys a kept frame, and
//! `slot_key`, a frozen layer's — cover everything that changes a picture.
//!
//! They hash a clip's saved form, so a saved field is covered by being
//! saved; what isn't saved is covered only if the key reads it some other
//! way. The gap found before (#3227) was the picture graph, not saved with
//! the clip, which the key didn't read: a kept frame was shown after a
//! grade edit. So:
//!
//! 1. Every field the timeline, node and core types keep out of the saved
//!    form (`#[serde(skip)]`, `skip_serializing`) is listed here with how
//!    the keys cover it. A new one fails until it is.
//! 2. Every saved leaf of a clip, every effect in the registry with each of
//!    its parameters, and every grade node kind, changed on a test clip,
//!    moves the frame key. A track's opacity and a clip's blend, laid over
//!    a frozen layer live, move the frame key and leave the layer's.

use std::collections::BTreeMap;
use std::path::Path;

use klipp_compose::freeze::{frame_key, slot_key, FreezeEnv};
use klipp_core::RationalTime;
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, TextBox, TitleData, Track, TrackKind};
use serde_json::Value;

/// How the keys cover a field kept out of the saved form.
#[derive(Debug)]
enum Cover {
    /// Hashed by the keys another way, named.
    Hashed(&'static str),
    /// Worked out from saved data the keys hash, named.
    Derived(&'static str),
    /// Changes no pixel, and why.
    NoPixels(&'static str),
}

/// Every field kept out of the saved form, by type and field.
const SKIPPED: &[(&str, Cover)] = &[
    ("ClipPictureFields.lifted", Cover::Hashed("the clip's picture, by picture_v2 in clip_hash")),
    ("ClipPictureFields.truth", Cover::Hashed("the clip's picture, by picture_v2 in clip_hash")),
    ("ClipPictureFields.effect_ids", Cover::NoPixels("the session's ids for the built-in effects, not picture data")),
    ("ClipPictureFields.stray", Cover::NoPixels("writes to a picture-truth clip that skipped its guard, dropped")),
    ("PictureView.own_node_graph", Cover::Derived("a render copy's own grade, baked from the saved grade groups")),
    ("PictureView.own_effects", Cover::Derived("a render copy's own effects, baked from the saved track effects")),
    ("Clip.precomp", Cover::Hashed("the nested scene, by sequence_hash in clip_hash")),
    ("Clip.baked_own", Cover::Derived("which of a render copy's effects are its own, from the saved track effects")),
    ("Track.pre_collapse_height", Cover::NoPixels("the track's height in the timeline before it was collapsed")),
    ("Track.precomposition_sound", Cover::NoPixels("a runtime track of a precomposition's sound")),
    ("TextBox.legacy_fx", Cover::NoPixels("old files' effects, moved onto the object at load and never drawn")),
    ("NodeParams.effect_blend", Cover::Hashed("a run's mix, from the clip's picture, hashed by picture_v2")),
    ("NodeParams.correction_blend", Cover::Hashed("a run's gate mix, from the clip's picture, hashed by picture_v2")),
    ("NodeParams.scene_to_display", Cover::Derived("a run's display step, from the working space and encoding in the environment")),
    ("NodeParams.side_picture", Cover::Hashed("a run's side output, from the clip's picture, hashed by picture_v2")),
    ("MaskKeyframes.warp", Cover::Derived("an evaluated window's tracking map, from the saved perspective track")),
];

/// The `Type.field` names of every field in `dir`'s sources kept out of
/// the saved form: a `#[serde(...)]` with `skip` or `skip_serializing`
/// (not `skip_serializing_if`) on a field.
fn skipped_fields(dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            skipped_fields(&path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let mut owner = String::new();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            for kw in ["struct ", "enum "] {
                if let Some(at) = t.find(kw).filter(|&at| t[..at].split_whitespace().all(|w| w == "pub" || w.starts_with("pub("))) {
                    owner = t[at + kw.len()..].split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("").to_string();
                }
            }
            let Some(attr) = t.strip_prefix("#[serde(").and_then(|a| a.strip_suffix(")]")) else { continue };
            if !attr.split(',').map(str::trim).any(|o| o == "skip" || o == "skip_serializing") {
                continue;
            }
            let field = lines[i + 1..].iter().map(|l| l.trim_start()).find(|l| !l.starts_with('#') && !l.starts_with("//"));
            let name = field
                .map(|l| l.trim_start_matches("pub(crate) ").trim_start_matches("pub "))
                .and_then(|l| l.split(':').next())
                .unwrap_or("?")
                .trim()
                .to_string();
            out.insert(format!("{owner}.{name}"), format!("{}:{}", path.display(), i + 1));
        }
    }
}

/// Part 1: every skipped field is classified, and every one classified is
/// still there.
#[test]
fn every_field_kept_out_of_the_saved_form_is_covered() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = BTreeMap::new();
    for krate in ["klipp-timeline", "klipp-nodes", "klipp-core"] {
        skipped_fields(&root.join(krate).join("src"), &mut found);
    }
    let listed: Vec<&str> = SKIPPED.iter().map(|(f, _)| *f).collect();
    let new: Vec<String> = found.iter().filter(|(f, _)| !listed.contains(&f.as_str())).map(|(f, at)| format!("{f} ({at})")).collect();
    assert!(new.is_empty(), "fields kept out of the saved form that the keys may not see; say in SKIPPED how each is covered: {new:#?}");
    let gone: Vec<&str> = listed.iter().copied().filter(|f| !found.contains_key(*f)).collect();
    assert!(gone.is_empty(), "listed but no longer skipped (or renamed): {gone:?}");
    for (field, cover) in SKIPPED {
        let why = match cover {
            Cover::Hashed(w) | Cover::Derived(w) | Cover::NoPixels(w) => w,
        };
        assert!(!why.is_empty(), "{field}: no reason given");
    }
}

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn keys(seq: &Sequence) -> (u64, Option<u64>) {
    let media = |_: &str| Some(1);
    let plugin = |_: &str| Some(1);
    let env = FreezeEnv { sequences: &[], media: &media, plugin: &plugin, environment: 1 };
    (frame_key(seq, tc(1.0), &env), slot_key(seq, 1, tc(1.0), &env))
}

/// An id the same every time it's made, so two sequences built apart
/// differ only in what a test changed in them.
fn fixed<T: serde::de::DeserializeOwned>(n: u128) -> T {
    serde_json::from_value(Value::String(format!("{:032x}", n).chars().enumerate().flat_map(|(i, c)| if [8, 12, 16, 20].contains(&i) { vec!['-', c] } else { vec![c] }).collect())).unwrap()
}

/// A title over a media clip, both on screen at 1 s.
fn sequence(clip: Clip) -> Sequence {
    // Made once: a clip's default grade gives its nodes fresh ids.
    static TITLE: std::sync::OnceLock<Clip> = std::sync::OnceLock::new();
    let title = TITLE
        .get_or_init(|| {
            let mut t = Clip { kind: ClipKind::Title(TitleData::single_text(TextBox { text: "Over".into(), ..Default::default() })), ..Clip::gap(tc(0.0), tc(4.0)) };
            t.id = fixed(1);
            t
        })
        .clone();
    let mut seq = Sequence::new_default();
    seq.tracks = [("T1", TrackKind::Text, title), ("V1", TrackKind::Video, clip)]
        .map(|(name, kind, c)| {
            let mut t = Track::new(name, kind);
            t.clips = vec![c];
            t
        })
        .into();
    seq
}

/// The media clip every variant is made from: the same ids each time,
/// its grade's nodes' too, as it is made once.
fn media() -> Clip {
    static MEDIA: std::sync::OnceLock<Clip> = std::sync::OnceLock::new();
    MEDIA
        .get_or_init(|| {
            let mut c = Clip::media(fixed(2), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
            c.id = fixed(3);
            c
        })
        .clone()
}

/// Every leaf of `value` changed once, each with its path: a number moved,
/// a flag flipped, a text changed. Arrays and objects are walked.
fn changed_leaves(value: &Value) -> Vec<(String, Value)> {
    fn walk(at: &str, v: &Value, root: &Value, out: &mut Vec<(String, Value)>) {
        let set = |new: Value, out: &mut Vec<(String, Value)>| {
            let mut whole = root.clone();
            *whole.pointer_mut(at).unwrap() = new;
            out.push((at.to_string(), whole));
        };
        match v {
            Value::Bool(b) => set(Value::Bool(!b), out),
            Value::Number(n) => {
                let moved = if let Some(i) = n.as_u64() { Value::from(i + 1) } else if let Some(i) = n.as_i64() { Value::from(i + 1) } else { Value::from(n.as_f64().unwrap() * 0.5 + 0.25) };
                set(moved, out)
            }
            Value::String(s) => set(Value::String(format!("{s}x")), out),
            Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| walk(&format!("{at}/{i}"), x, root, out)),
            Value::Object(o) => o.iter().for_each(|(k, x)| walk(&format!("{at}/{}", k.replace('~', "~0").replace('/', "~1")), x, root, out)),
            Value::Null => {}
        }
    }
    let mut out = Vec::new();
    walk("", value, value, &mut out);
    out
}

/// The frame key of the sequence with `clip` as its media clip.
fn frame_of(clip: Clip) -> u64 {
    keys(&sequence(clip)).0
}

/// Part 2: every saved leaf of a media clip, changed, moves the frame key.
/// A leaf whose change no longer reads as a clip is counted, not tested;
/// there are far fewer of those than tested ones.
#[test]
fn every_saved_leaf_of_a_clip_moves_the_frame_key() {
    let mut clip = media();
    clip.set_effects(vec![klipp_effects::BuiltinEffect::FilmGrain { amount: 0.3, size: 1.5, color: 0, shadows: 0.6, seed: 0 }]);
    let base = frame_of(clip.clone());
    let saved = serde_json::to_value(&clip).unwrap();
    let (mut tested, mut unread, mut still) = (0, 0, Vec::new());
    for (at, whole) in changed_leaves(&saved) {
        let Ok(changed) = serde_json::from_value::<Clip>(whole) else {
            unread += 1;
            continue;
        };
        tested += 1;
        if frame_of(changed) == base {
            still.push(at);
        }
    }
    eprintln!("{tested} leaves tested, {unread} no longer read as a clip");
    assert!(tested >= 40 && tested > unread * 2, "the sweep must test the clip's fields: {tested} tested, {unread} unread");
    assert!(still.is_empty(), "changed, these left the frame key as it was: {still:?}");
}

/// Part 2: every effect in the registry, put on a clip, moves the frame
/// key, and so does each of its parameters, changed.
#[test]
fn every_effect_and_each_parameter_moves_the_frame_key() {
    let plain = frame_of(media());
    let (mut effects, mut params, mut still) = (0, 0, Vec::new());
    for ty in klipp_effects::registry::effect_types() {
        let sample = (ty.sample)();
        let with = |e: klipp_effects::BuiltinEffect| {
            let mut c = media();
            c.set_effects(vec![e]);
            frame_of(c)
        };
        let base = with(sample.clone());
        effects += 1;
        if base == plain {
            still.push(format!("{}: put on", ty.desc.id));
        }
        for (at, whole) in changed_leaves(&serde_json::to_value(&sample).unwrap()) {
            let Ok(changed) = serde_json::from_value::<klipp_effects::BuiltinEffect>(whole) else { continue };
            if changed == sample {
                continue;
            }
            params += 1;
            if with(changed) == base {
                still.push(format!("{} {at}", ty.desc.id));
            }
        }
    }
    eprintln!("{effects} effects, {params} parameters changed");
    assert!(effects >= 40 && params >= effects, "the registry swept: {effects} effects, {params} parameters");
    assert!(still.is_empty(), "these left the frame key as it was: {still:#?}");
}

/// A grade with a corrector, made once: a new graph's nodes take fresh
/// ids, and two grades made apart would key apart.
fn base_grade() -> (klipp_nodes::NodeGraph, klipp_nodes::NodeId) {
    static GRADE: std::sync::OnceLock<(klipp_nodes::NodeGraph, klipp_nodes::NodeId)> = std::sync::OnceLock::new();
    GRADE
        .get_or_init(|| {
            let mut g = klipp_nodes::NodeGraph::default();
            let corrector = g.ensure_corrector();
            (g, corrector)
        })
        .clone()
}

/// Part 2: every grade node kind, put in a clip's grade, moves the frame
/// key, and so does turning it off and changing each saved parameter. The
/// picture graph isn't saved with the clip (#3227): this is the key's own
/// hash of it.
#[test]
fn every_grade_node_and_its_parameters_move_the_frame_key() {
    let plain = {
        let mut c = media();
        let (g, corrector) = base_grade();
        c.set_grade(g);
        let group = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP).unwrap().id;
        let path = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, [group, corrector]);
        c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::SetMix { path, mix: Some(0.9) }]).unwrap();
        frame_of(c)
    };
    let (mut kinds, mut params, mut edits, mut still, mut unlifted, mut inert) = (0, 0, 0, Vec::new(), Vec::new(), Vec::new());
    for ty in klipp_nodes::registry::node_types() {
        // Its parameters as the registry gives them, each at its reset
        // value: unset, a parameter is not in the saved form to change.
        let mut given = serde_json::Map::new();
        for p in ty.desc.params.iter().filter(|p| p.index.is_none() && p.default.is_finite()) {
            let mut one = given.clone();
            one.insert(p.field.to_string(), Value::from(p.default as f64));
            if serde_json::from_value::<klipp_nodes::NodeParams>(Value::Object(one.clone())).is_ok() {
                given = one;
            } else {
                let mut one = given.clone();
                one.insert(p.field.to_string(), Value::Bool(p.default != 0.0));
                if serde_json::from_value::<klipp_nodes::NodeParams>(Value::Object(one.clone())).is_ok() {
                    given = one;
                }
            }
        }
        let node = klipp_nodes::Node {
            id: klipp_nodes::NodeId::new_v4(),
            kind: ty.kind,
            label: format!("{:?}", ty.kind),
            enabled: true,
            params: serde_json::from_value(Value::Object(given)).unwrap(),
            keyframes: Default::default(),
            output_link: None,
            input_links: vec![],
        };
        // A picture-truth clip, as one edited through picture ops is: its
        // picture, the grade in it, is not in its saved form, so only the
        // key's own hash of the picture can see an edit made there (#3227).
        let build = |n: &klipp_nodes::Node| -> Option<(Clip, klipp_graph_format::path::NodePath, klipp_graph_format::path::NodePath)> {
            let mut c = media();
            let (mut g, corrector) = base_grade();
            g.nodes.push(n.clone());
            c.set_grade(g);
            let group = c.picture_v2().ok()?.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP)?.id;
            let id = c.id.0;
            let at = move |node| klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: id }, [group, node]);
            // A kind no lone graph can hold (a second source) doesn't lift.
            c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::SetMix { path: at(corrector), mix: Some(0.9) }]).ok()?;
            assert!(c.is_picture_truth(), "the sweep's clip is picture-truth");
            let path = at(n.id);
            Some((c, path, at(corrector)))
        };
        let with = |n: &klipp_nodes::Node| build(n).map(|(c, ..)| frame_of(c));
        // What a clip holds: its saved form and its picture. An edit that
        // changes neither changes no pixel, and is not asked to move a key.
        let held = |c: &Clip| (serde_json::to_value(c).unwrap(), c.picture_v2().ok().map(|p| serde_json::to_value(&p.picture).unwrap()));
        let held_of = |n: &klipp_nodes::Node| build(n).map(|(c, ..)| held(&c));
        let Some(base) = with(&node) else {
            unlifted.push(ty.kind);
            continue;
        };
        kinds += 1;
        if base == plain {
            still.push(format!("{:?}: put in", ty.kind));
        }
        let base_held = held_of(&node);
        if with(&klipp_nodes::Node { enabled: false, ..node.clone() }) == Some(base) {
            still.push(format!("{:?}: turned off", ty.kind));
        }
        for (at, whole) in changed_leaves(&serde_json::to_value(&node.params).unwrap()) {
            let Ok(p) = serde_json::from_value::<klipp_nodes::NodeParams>(whole) else { continue };
            let changed = klipp_nodes::Node { params: p, ..node.clone() };
            if held_of(&changed) == base_held {
                inert.push(format!("{:?} {at}", ty.kind));
                continue;
            }
            params += 1;
            if with(&changed) == Some(base) {
                still.push(format!("{:?} {at}", ty.kind));
            }
        }
        // And edited where a picture-truth clip is edited, in its picture:
        // its mix, turned off, and each parameter the registry names.
        use klipp_graph_lift::ops::GraphOp;
        let (clip, path, corrector) = build(&node).expect("built above");
        let mut ops = vec![
            ("mix", GraphOp::SetMix { path: path.clone(), mix: Some(0.4) }),
            ("enabled", GraphOp::SetEnabled { path: path.clone(), enabled: false }),
            // A correction's mix and its gate's channel are held in the
            // picture alone (`NodeParams::correction_blend`, never saved):
            // the edit #3227 was.
            ("the corrector's mix", GraphOp::SetMix { path: corrector.clone(), mix: Some(0.4) }),
            ("the corrector's mask channel", GraphOp::SetMaskChannel { path: corrector.clone(), channel: Some("green".into()) }),
        ];
        for p in ty.desc.params.iter().filter(|p| p.default.is_finite()) {
            ops.push((p.key, GraphOp::SetParam { path: path.clone(), key: klipp_graph_format::ParamKey::from(p.key), value: klipp_graph_format::ParamValue::Float(p.default + 0.37) }));
        }
        for (what, op) in ops {
            let mut c = clip.clone();
            if c.apply_picture_ops(&[op]).is_err() {
                continue;
            }
            if held(&c) == held(&clip) {
                inert.push(format!("{:?}: {what} in its picture", ty.kind));
                continue;
            }
            edits += 1;
            if frame_of(c) == base {
                still.push(format!("{:?}: {what} in its picture", ty.kind));
            }
        }
    }
    eprintln!("{kinds} node kinds, {params} parameters changed, {edits} picture edits; not in a lone graph: {unlifted:?}; {} edits changed nothing the clip holds: {inert:?}", inert.len());
    assert!(kinds >= 20 && params >= kinds && edits >= kinds * 2 && unlifted.len() * 4 < kinds, "the registry swept: {kinds} kinds, {params} parameters, {edits} picture edits, {} not lifted", unlifted.len());
    assert!(still.is_empty(), "these left the frame key as it was: {still:#?}");
}

/// Part 2: a track's opacity and a clip's blend are laid over a frozen
/// layer live, so they move the whole frame's key and leave the layer's.
#[test]
fn what_is_laid_live_over_a_freeze_leaves_the_layers_key() {
    let seq = sequence(media());
    let (frame, layer) = keys(&seq);
    assert!(layer.is_some(), "the media track has a layer to freeze");
    let mut faded = seq.clone();
    faded.tracks[1].opacity = 0.5;
    let mut blended = seq.clone();
    blended.tracks[1].clips[0].blend_mode = klipp_core::BlendMode::Screen;
    for (what, s) in [("track opacity", faded), ("clip blend", blended)] {
        let (f, l) = keys(&s);
        assert_ne!(f, frame, "{what}: the whole frame keys anew");
        assert_eq!(l, layer, "{what}: the frozen layer's key stays");
    }
}

/// Part 3, as the keys take them: a media file's identity, a plug-in's,
/// and the environment (working space, proxies, how frames are drawn)
/// each move both keys; the engine's `freeze` tests say what moves each.
#[test]
fn a_files_a_plugins_and_the_environments_identity_move_the_keys() {
    let mut clip = media();
    clip.set_effects(vec![klipp_effects::BuiltinEffect::Frei0r { type_id: "frei0r:glow".into(), params: Default::default() }]);
    let seq = sequence(clip);
    let with = |m: u64, p: u64, e: u64| {
        let media = move |_: &str| Some(m);
        let plugin = move |_: &str| Some(p);
        let env = FreezeEnv { sequences: &[], media: &media, plugin: &plugin, environment: e };
        (frame_key(&seq, tc(1.0), &env), slot_key(&seq, 1, tc(1.0), &env))
    };
    let base = with(1, 1, 1);
    for (what, other) in [("the file", with(2, 1, 1)), ("the plug-in", with(1, 2, 1)), ("the environment", with(1, 1, 2))] {
        assert_ne!(other.0, base.0, "{what}: the frame key");
        assert_ne!(other.1, base.1, "{what}: the frozen layer's key");
    }
}

/// The sweeps compare keys of sequences built apart: built twice with
/// nothing changed, they key alike, so a key that differs is the change.
#[test]
fn a_sequence_built_twice_keys_alike() {
    assert_eq!(keys(&sequence(media())), keys(&sequence(media())));
}

