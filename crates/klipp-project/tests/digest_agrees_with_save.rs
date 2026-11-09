//! The project's digest says "changed" exactly when a save would write
//! something else. The digest is hashed without building the files, so
//! nothing but this ties the two together: saved text is the reference, and
//! a change the digest missed would let a real edit read as saved.

use klipp_core::{GroupId, Keyframe, MediaId, MulticamSound, RationalTime};
use klipp_media::MediaPool;
use klipp_nodes::params::EffectBlend;
use klipp_nodes::{NodeGraph, NodeKind};
use klipp_project::{load_project_strict, project_digest, save_project_full, ColorSettings};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, GradeAccess, GradeGroup, GradeStage, GradeVersion, Look, Sequence, TitleAsset, Track, TrackKind};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    serde_json::from_value(json!({ "time_secs": time_secs, "value": value, "interpolation": "Linear" })).unwrap()
}

fn caption(text: &str, from: f64, to: f64) -> Clip {
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: text.into(), font_size: 40.0, ..Default::default() });
    Clip { kind: ClipKind::Title(td), source_in: t(0.0), source_out: t(to - from), ..Clip::gap(t(from), t(to)) }
}

fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
    let mut tr = Track::new(name, kind);
    tr.clips = clips;
    tr
}

/// A grade with a Corrector, its run setting `mix` if any.
fn grade(exposure: f32, mix: Option<f32>) -> NodeGraph {
    let mut g = NodeGraph::default();
    g.ensure_corrector();
    let n = g.nodes.iter_mut().find(|n| n.kind == NodeKind::Corrector).unwrap();
    n.params.exposure = Some(exposure);
    n.params.correction_blend = mix.map(|mix| EffectBlend { mix, channel: klipp_effects::mask_mix::MaskChannel::Red });
    g
}

/// `g` with only its Corrector's exposure changed: every node id kept, so
/// nothing else about the grade tells it apart.
fn exposed(g: &NodeGraph, exposure: f32) -> NodeGraph {
    let mut g = g.clone();
    g.nodes.iter_mut().find(|n| n.kind == NodeKind::Corrector).unwrap().params.exposure = Some(exposure);
    g
}

/// `g` with only its Corrector's mix changed.
fn mixed(g: &NodeGraph, mix: f32) -> NodeGraph {
    let mut g = g.clone();
    g.nodes.iter_mut().find(|n| n.kind == NodeKind::Corrector).unwrap().params.correction_blend = Some(EffectBlend { mix, channel: klipp_effects::mask_mix::MaskChannel::Red });
    g
}

/// All a save holds, and something of each kind to change.
#[derive(Clone)]
struct Project {
    sequences: Vec<Sequence>,
    pool: MediaPool,
    color: ColorSettings,
    notes: String,
    assets: HashMap<MediaId, TitleAsset>,
    looks: Vec<Look>,
}

fn project() -> Project {
    let (a, b) = (MediaId::new(), MediaId::new());
    let mut pool = MediaPool::new();
    for (id, name) in [(a, "a.mov"), (b, "b.mov")] {
        let mut item = klipp_media::MediaItem::synthetic_for_test(PathBuf::from(format!("/nowhere/{name}")), None);
        (item.id, item.filename) = (id, name.into());
        item.duration = t(60.0);
        pool.add(item);
    }
    pool.find_or_create_bin("Selects");

    // A multicam pair with its sound linked and keyed.
    let group = GroupId::new();
    let mut primary = Clip::media(a, t(0.0), t(10.0), t(2.0), t(12.0));
    (primary.multicam_group, primary.multicam_angle, primary.multicam_cuts, primary.multicam_sound) = (Some(group), 0, vec![(3.0, 1), (7.5, 0)], MulticamSound::Angle(1));
    primary.clip_markers = vec![serde_json::from_value(json!({ "time_secs": 1.25, "name": "Take", "color": "Green", "comment": "good" })).unwrap()];
    let mut angle = Clip::media(b, t(0.0), t(10.0), t(1.5), t(11.5));
    (angle.multicam_group, angle.multicam_angle) = (Some(group), 1);
    let mut sound = Clip::media(a, t(0.0), t(10.0), t(2.0), t(12.0));
    (primary.linked_clip_id, sound.linked_clip_id) = (Some(sound.id), Some(primary.id));
    sound.volume_keyframes = vec![key(0.0, 1.0), key(4.0, 0.25)];
    sound.volume = 0.8;

    // A clip graded, a clip with keyed effects, one whose picture is its
    // truth, and grade versions, one with a mix.
    let mut graded = Clip::media(a, t(10.0), t(14.0), t(0.0), t(4.0));
    graded.set_grade(grade(0.5, None));
    graded.edit_grade_versions(|vs| vs.extend([GradeVersion::new(String::from("A"), grade(0.1, None)), GradeVersion::new(String::from("B"), grade(0.2, Some(0.5)))]));
    let keyed = TestClip::of(Clip::media(b, t(14.0), t(18.0), t(0.0), t(4.0)))
        .effects([klipp_effects::BuiltinEffect::GaussianBlur { radius: 4.0 }, klipp_effects::BuiltinEffect::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }])
        .key(0, "radius", 0.0, 1.0)
        .key(0, "radius", 3.0, 8.0)
        .lane("opacity", 2.0, 0.5)
        .build();
    let truth = {
        use klipp_graph_format::path::{NodePath, Root};
        let mut c = TestClip::of(Clip::media(a, t(18.0), t(20.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 4.0 }).build();
        let blur = c.picture_effects().unwrap()[0].node;
        c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::SetMix { path: NodePath::new(Root::Clip { clip: c.id.0 }, [blur]), mix: Some(0.5) }]).unwrap();
        assert!(c.is_picture_truth());
        c
    };

    let mut subs = track("Subtitle 1", TrackKind::Subtitle, vec![caption("Hello.", 0.5, 2.0), caption("And welcome.", 2.2, 4.8)]);
    subs.language = Some("en".into());
    subs.caption_style = Some(klipp_timeline::CaptionStyle { size: 52.0, y: 0.8, shadow: true, ..Default::default() });

    let mut seq = Sequence::new_default();
    seq.name = "Rich".into();
    seq.tracks = vec![
        track("T1", TrackKind::Text, vec![caption("A title", 1.0, 5.0)]),
        track("V2", TrackKind::Video, vec![angle]),
        track("V1", TrackKind::Video, vec![primary, graded, keyed, truth]),
        track("A1", TrackKind::Audio, vec![sound]),
        subs,
    ];
    (seq.in_point, seq.out_point) = (Some(t(1.0)), Some(t(12.5)));
    seq.markers = vec![serde_json::from_value(json!({ "id": klipp_core::MarkerId::new(), "position": t(3.0), "label": "Cut", "color": "Blue", "duration": t(1.5), "comment": "" })).unwrap()];
    seq.timeline_grade_slot().set(Some(grade(0.3, Some(0.75))));
    let mut stage = GradeGroup::new("Night");
    stage.stage_slot(GradeStage::Post).set(Some(grade(0.4, Some(0.25))));
    seq.grade_groups.push(stage);

    let mut second = Sequence::new_default();
    second.name = "Short".into();
    second.tracks[0].clips = vec![Clip::media(b, t(0.0), t(3.0), t(5.0), t(8.0))];

    let mut assets = HashMap::new();
    let ClipKind::Title(data) = caption("Asset", 0.0, 1.0).kind else { unreachable!() };
    assets.insert(MediaId::new(), TitleAsset { display_name: "Lower third".into(), data, animation: None });
    let looks = vec![Look { id: MediaId::new().0, name: "Warm".into(), node_graph: grade(0.6, None), node_labels: Vec::new() }];
    Project { sequences: vec![seq, second], pool, color: ColorSettings::default(), notes: "notes".into(), assets, looks }
}

fn digest(p: &Project, dir: &Path) -> u64 {
    project_digest(dir, &p.sequences, &p.pool, &p.color, None, &p.notes, &p.assets, &p.looks).unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_digest_agrees_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The lines a save stamps afresh every time.
const STAMPS: [&str; 4] = ["  \"generation\":", "  \"saved_at\":", "  \"klipp_version\":", "  \"platform\":"];

fn file_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap().lines().filter(|l| !STAMPS.iter().any(|s| l.starts_with(s))).collect::<Vec<_>>().join("\n")
}

/// What a save writes, stamps aside: the reference.
fn saved(p: &Project, dir: &Path) -> String {
    save_project_full(dir, &p.sequences, &p.pool, &p.color, None, None, &p.notes, 0, None, None, None, &p.assets).unwrap();
    format!("{}\n{}\n{}", file_text(&dir.join("project.json")), file_text(&dir.join("media/manifest.json")), serde_json::to_string(&p.looks).unwrap())
}

fn main_clips(p: &mut Project) -> &mut Vec<Clip> {
    &mut p.sequences[0].tracks[2].clips
}

/// One change each to what a project holds: each changes what a save
/// writes, and each changes the digest.
#[test]
fn every_edit_a_save_would_write_changes_the_digest() {
    type Edit = (&'static str, fn(&mut Project));
    let edits: Vec<Edit> = vec![
        ("sequence name", |p| p.sequences[0].name.push('!')),
        ("in point", |p| p.sequences[0].in_point = Some(t(2.0))),
        ("sequence marker", |p| p.sequences[0].markers.clear()),
        ("track name", |p| p.sequences[0].tracks[2].name.push('!')),
        ("track visible", |p| p.sequences[0].tracks[2].visible = false),
        ("track muted", |p| p.sequences[0].tracks[3].muted = true),
        ("track level", |p| p.sequences[0].tracks[3].volume = 0.5),
        ("a trim", |p| main_clips(p)[0].timeline_out = t(9.0)),
        ("a source move", |p| main_clips(p)[0].source_in = t(3.0)),
        ("a speed", |p| main_clips(p)[0].speed = 2.0),
        ("a clip off", |p| main_clips(p)[0].enabled = false),
        ("a clip removed", |p| { main_clips(p).remove(1); }),
        ("a clip marker", |p| main_clips(p)[0].clip_markers.clear()),
        ("a multicam cut", |p| main_clips(p)[0].multicam_cuts.push((9.0, 1))),
        ("the multicam sound", |p| main_clips(p)[0].multicam_sound = MulticamSound::Angle(0)),
        ("a link broken", |p| main_clips(p)[0].linked_clip_id = None),
        ("a clip's level", |p| p.sequences[0].tracks[3].clips[0].volume = 0.5),
        ("a sound key", |p| p.sequences[0].tracks[3].clips[0].volume_keyframes.pop().map(drop).unwrap()),
        ("a clip's grade", |p| { let g = exposed(main_clips(p)[1].grade(), 0.9); main_clips(p)[1].set_grade(g); }),
        ("a grade version", |p| main_clips(p)[1].edit_grade_versions(|vs| { let g = exposed(vs[0].grade(), 0.7); vs[0].set_grade(g); })),
        ("a grade version's mix", |p| main_clips(p)[1].edit_grade_versions(|vs| { let g = mixed(vs[1].grade(), 0.9); vs[1].set_grade(g); })),
        ("a grade version added", |p| main_clips(p)[1].edit_grade_versions(|vs| vs.push(GradeVersion::new(String::from("C"), grade(0.0, None))))),
        ("an effect's value", |p| { let c = main_clips(p)[2].clone(); main_clips(p)[2] = TestClip::of(Clip::media(c.media_id().unwrap(), c.timeline_in, c.timeline_out, c.source_in, c.source_out)).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 9.0 }).build(); }),
        ("the truth clip's mix", |p| {
            use klipp_graph_format::path::{NodePath, Root};
            let c = &mut main_clips(p)[3];
            let blur = c.picture_effects().unwrap()[0].node;
            let path = NodePath::new(Root::Clip { clip: c.id.0 }, [blur]);
            c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::SetMix { path, mix: Some(0.25) }]).unwrap();
        }),
        ("a title's text", |p| p.sequences[0].tracks[0].clips[0] = caption("Another", 1.0, 5.0)),
        ("a caption's text", |p| p.sequences[0].tracks[4].clips[0] = caption("Goodbye.", 0.5, 2.0)),
        ("a caption style", |p| p.sequences[0].tracks[4].caption_style.as_mut().unwrap().size = 60.0),
        ("the timeline grade", |p| { let g = exposed(p.sequences[0].timeline_grade().unwrap(), 0.8); p.sequences[0].timeline_grade_slot().set(Some(g)); }),
        ("the timeline grade's mix", |p| { let g = mixed(p.sequences[0].timeline_grade().unwrap(), 0.5); p.sequences[0].timeline_grade_slot().set(Some(g)); }),
        ("a group's grade", |p| { let g = exposed(p.sequences[0].grade_groups[0].stage(GradeStage::Post).unwrap(), 0.8); p.sequences[0].grade_groups[0].stage_slot(GradeStage::Post).set(Some(g)); }),
        ("a group's grade mix", |p| { let g = mixed(p.sequences[0].grade_groups[0].stage(GradeStage::Post).unwrap(), 0.5); p.sequences[0].grade_groups[0].stage_slot(GradeStage::Post).set(Some(g)); }),
        ("a group's name", |p| p.sequences[0].grade_groups[0].name.push('!')),
        ("a second sequence", |p| p.sequences[1].name.push('!')),
        ("a sequence added", |p| p.sequences.push(Sequence::new_default())),
        ("a title asset", |p| p.assets.values_mut().next().unwrap().display_name.push('!')),
        ("a look's name", |p| p.looks[0].name.push('!')),
        ("a look's grade", |p| p.looks[0].node_graph = exposed(&p.looks[0].node_graph, 0.1)),
        ("the notes", |p| p.notes.push('!')),
        ("a media name", |p| { let id = p.pool.all().next().unwrap().id; p.pool.get_mut(&id).unwrap().filename.push('!'); }),
        ("a media rating", |p| { let id = p.pool.all().next().unwrap().id; p.pool.get_mut(&id).unwrap().rating = 3; }),
        ("a bin", |p| { p.pool.find_or_create_bin("Rejects"); }),
        ("the working colour", |p| p.color.scopes_after_viewing_lut = !p.color.scopes_after_viewing_lut),
    ];
    let dir = scratch("edits");
    let base = project();
    let (saved_before, digest_before) = (saved(&base, &dir), digest(&base, &dir));
    for (name, edit) in edits {
        let mut p = base.clone();
        edit(&mut p);
        assert_ne!(saved(&p, &dir), saved_before, "{name}: a save writes the same, so this edit tests nothing");
        assert_ne!(digest(&p, &dir), digest_before, "{name}: a save writes something else, and the digest says nothing changed");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a save doesn't change, the digest doesn't either: a copy, how the
/// timeline is viewed, a save and open, and maps built in another order.
#[test]
fn what_a_save_would_write_alike_digests_alike() {
    let dir = scratch("same");
    let base = project();
    let before = digest(&base, &dir);
    assert_eq!(digest(&base, &dir), before, "again");
    assert_eq!(digest(&base.clone(), &dir), before, "a copy");
    let mut viewed = base.clone();
    viewed.color.track_header_width += 80.0;
    assert_eq!(digest(&viewed, &dir), before, "the header width is how it is viewed");

    let text = saved(&base, &dir);
    let (sequences, pool, color, _active, _created, notes, _zoom, _scroll, _scale, assets) = load_project_strict(&dir).unwrap();
    let opened = Project { sequences, pool, color, notes, assets, looks: base.looks.clone() };
    assert_eq!(digest(&opened, &dir), before, "opened, it is what was saved");
    let again = scratch("same_again");
    assert_eq!(saved(&opened, &again), text, "saved again, the same");
    let _ = std::fs::remove_dir_all(&again);

    // A hash map in the saved data, built forwards and backwards.
    let positions: Vec<_> = (0..64).map(|i| (MediaId::new().0, klipp_nodes::NodePos { x: i as f32, y: 0.0 })).collect();
    let shared = grade(0.5, None);
    let with = |order: &[(klipp_nodes::NodeId, klipp_nodes::NodePos)]| {
        let mut p = base.clone();
        let mut g = shared.clone();
        g.node_positions = order.iter().cloned().collect();
        main_clips(&mut p)[1].set_grade(g);
        p
    };
    let backwards: Vec<_> = positions.iter().rev().cloned().collect();
    let (forward, reverse) = (with(&positions), with(&backwards));
    let keys = |p: &Project| main_clips(&mut p.clone())[1].grade().node_positions.keys().copied().collect::<Vec<_>>();
    assert_ne!(keys(&forward), keys(&reverse), "the two iterate differently, or this tests nothing");
    assert_eq!(saved(&forward, &dir), saved(&reverse, &dir), "a save writes them alike");
    assert_eq!(digest(&forward, &dir), digest(&reverse, &dir), "so they digest alike");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every value in project.json, changed in the file and opened: where the
/// project read back saves differently, it digests differently. A new field
/// in the file is covered by this with no edit written for it, if this
/// test's project holds it.
#[test]
fn every_value_in_the_saved_file_that_reads_back_changes_the_digest() {
    fn leaves(v: &Value, path: &mut Vec<Value>, out: &mut Vec<Vec<Value>>) {
        match v {
            Value::Object(o) => for (k, c) in o {
                path.push(json!(k));
                leaves(c, path, out);
                path.pop();
            },
            Value::Array(a) => for (i, c) in a.iter().enumerate() {
                path.push(json!(i));
                leaves(c, path, out);
                path.pop();
            },
            _ => out.push(path.clone()),
        }
    }
    fn at<'a>(v: &'a mut Value, path: &[Value]) -> &'a mut Value {
        path.iter().fold(v, |v, step| match step {
            Value::String(k) => &mut v[k.as_str()],
            n => &mut v[n.as_u64().unwrap() as usize],
        })
    }
    let dir = scratch("leaves");
    let base = project();
    let read = |dir: &Path| load_project_strict(dir).ok().map(|(sequences, pool, color, _active, _created, notes, _zoom, _scroll, _scale, assets)| Project { sequences, pool, color, notes, assets, looks: base.looks.clone() });
    let out = scratch("leaves_out");
    let saved_text = saved(&base, &dir);
    let opened = read(&dir).unwrap();
    let (resaved_before, digest_before) = (saved(&opened, &out), digest(&opened, &dir));
    assert_eq!(resaved_before, saved_text);
    let file: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    let mut paths = Vec::new();
    leaves(&file, &mut Vec::new(), &mut paths);
    let (mut read_back, mut refused, mut same, mut missed) = (0, 0, 0, Vec::new());
    for path in &paths {
        let name = path.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("/");
        if ["generation", "saved_at", "klipp_version", "platform", "format_revision"].iter().any(|s| name == format!("\"{s}\"")) {
            continue;
        }
        // How the timeline is viewed: saved, but not the project's content.
        if name.ends_with("\"track_header_width\"") {
            continue;
        }
        let mut changed = file.clone();
        let leaf = at(&mut changed, path);
        *leaf = match leaf {
            Value::Bool(b) => json!(!*b),
            Value::Number(n) if n.is_f64() => json!(n.as_f64().unwrap() * 0.5 + 0.125),
            Value::Number(n) => json!(n.as_i64().unwrap_or(0) + 1),
            Value::String(s) => json!(format!("{s}x")),
            _ => json!(1),
        };
        std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&changed).unwrap()).unwrap();
        let Some(p) = read(&dir) else { refused += 1; continue };
        read_back += 1;
        let Ok(resaved) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| saved(&p, &out))) else { refused += 1; continue };
        if resaved == resaved_before {
            same += 1;
            continue;
        }
        if digest(&p, &dir) == digest_before {
            missed.push(name);
        }
    }
    println!("{} values: {read_back} read back ({same} of them save the same), {refused} refused", paths.len());
    assert!(read_back > 100, "too few read back for this to test anything: {read_back}");
    assert!(missed.is_empty(), "saved differently, digested the same: {missed:#?}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&out);
}
