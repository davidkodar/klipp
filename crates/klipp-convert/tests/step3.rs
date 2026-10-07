//! The step to revision 3 (`klipp_convert::step3`), in temporary folders
//! only: every project, backup and journal here is made by the test.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use klipp_convert::run::Settings;
use klipp_convert::step3::{self, Decision, Hooks, Point, RunError};
use klipp_convert::{revision2, revision3};
use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{types, Legacy, Node, NodeId, PortBinding};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::{Clip, GradeAccess};
use serde_json::{json, Value};

fn root(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-step3-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(d).unwrap()
}

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn graph() -> klipp_nodes::NodeGraph {
    let mut g = klipp_nodes::NodeGraph::default();
    let c = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == c).unwrap().params.exposure = Some(0.4);
    g
}

fn read(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// A revision-2 project as the build before revision 3 wrote one: saved
/// by the v1 writer and converted by stage A's conversion, so its effects
/// have their slot ids; with a grade version, a Look, a still and the
/// app's saved backup of the project file.
fn plain(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(format!("{name}.klipp"));
    let mut seq = klipp_timeline::Sequence::new_default();
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    clip.set_grade(graph());
    clip.edit_grade_versions(|v| v.push(klipp_timeline::GradeVersion { name: "B".into(), node_graph: klipp_timeline::Grade::new(graph()) }));
    clip.set_effect_list(vec![E::GaussianBlur { radius: 2.0 }, E::Sharpen { amount: 0.5 }], vec![true, false]);
    seq.tracks[0].clips.push(clip);
    // A track's effect list: its picture is the list as nodes.
    seq.tracks[0].effects.edit_video_effects(|list, enabled| {
        list.push(E::Sharpen { amount: 0.3 });
        enabled.push(true);
    });
    klipp_project::save_project_v1(&dir, &[seq], &klipp_media::MediaPool::new()).unwrap();
    klipp_project::save_looks_v1(&dir, &[klipp_timeline::Look { id: uuid::Uuid::new_v4(), name: "Warm".into(), node_graph: graph() }]).unwrap();
    std::fs::write(dir.join("media/stills.json"), json!([{ "name": "s", "filename": "s.png", "grade": graph() }]).to_string()).unwrap();
    let mut p = revision2::project(&read(&dir.join("project.json"))).unwrap().0;
    p["format_revision"] = json!(revision2::REVISION);
    std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&p).unwrap()).unwrap();
    std::fs::copy(dir.join("project.json"), dir.join("project.json.bak.1")).unwrap();
    let stills = dir.join("media/stills.json");
    std::fs::write(&stills, revision2::stills(&read(&stills)).unwrap().to_string()).unwrap();
    for look in std::fs::read_dir(dir.join("looks")).unwrap() {
        let look = look.unwrap().path();
        std::fs::write(&look, revision2::look(&read(&look)).unwrap().to_string()).unwrap();
    }
    std::fs::write(dir.join("media/footage.mov"), vec![7u8; 4096]).unwrap();
    dir
}

fn at(c: &Clip, ids: impl IntoIterator<Item = NodeId>) -> NodePath {
    NodePath::new(Root::Clip { clip: c.id.0 }, ids)
}

fn picture(c: &Clip) -> ClipPicture {
    c.picture_v2().unwrap().picture.clone()
}

fn effects(p: &ClipPicture) -> Vec<NodeId> {
    p.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect()
}

fn new_effect(id: u128, effect: &E) -> Box<Node> {
    Box::new(Node {
        id: uuid::Uuid::from_u128(id),
        type_id: types::LEGACY_EFFECT.into(),
        type_version: 1,
        label: String::new(),
        enabled: true,
        params: Default::default(),
        anim: Default::default(),
        mix: None,
        wireless: Default::default(),
        group: None,
        legacy: Some(Legacy { slot: None, payload: serde_json::to_value(effect).unwrap(), keyframes: None, modifiers: Default::default() }),
        legacy_flags: None,
        origin: None,
    })
}

/// A picture-truth clip after an add, a move and a remove: its blur masked
/// by a qualifier's matte the grade publishes and mixed at a half, so its
/// effect ids are its own, not slot ids. Saved, then marked revision 2 as
/// the build before revision 3 saved such a clip (its truth ids the same).
fn truth(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(format!("{name}.klipp"));
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::GaussianBlur { radius: 3.0 });
    c.push_effect(E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 });
    let mut g = klipp_nodes::NodeGraph::default();
    g.ensure_corrector();
    let source = g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).unwrap().id;
    let q = klipp_nodes::Node { id: NodeId::new_v4(), kind: klipp_nodes::NodeKind::Qualifier, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: q.id, to_port: 0 });
    let qualifier = q.id;
    g.nodes.push(q);
    c.set_grade(g);
    let p = picture(&c);
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
    let fx = effects(&p);
    c.apply_picture_ops(&[
        GraphOp::PublishOutput { group: at(&c, [group]), endpoint: PortBinding { node: qualifier, port: "out".into() }, key: "matte_selection".into(), label: "Selection".into() },
        GraphOp::Connect { from: at(&c, [group]), from_port: "matte_selection".into(), to: at(&c, [fx[0]]), to_port: "mask".into() },
        GraphOp::SetMix { path: at(&c, [fx[0]]), mix: Some(0.5) },
    ])
    .unwrap();
    assert!(c.is_picture_truth());
    c.apply_picture_ops(&[GraphOp::AddNode { after: at(&c, [fx[0]]), node: new_effect(0xa1, &E::Sharpen { amount: 0.5 }) }]).unwrap();
    c.apply_picture_ops(&[GraphOp::MoveAfter { path: at(&c, [fx[0]]), after: fx[1] }]).unwrap();
    c.apply_picture_ops(&[GraphOp::RemoveNode { path: at(&c, [uuid::Uuid::from_u128(0xa1)]) }]).unwrap();
    c.apply_picture_ops(&[GraphOp::AddNode { after: at(&c, [fx[1]]), node: new_effect(0xa2, &E::Sharpen { amount: 0.25 }) }]).unwrap();
    assert!(c.is_picture_truth());
    let mut seq = klipp_timeline::Sequence::new_default();
    seq.tracks[0].clips.push(c);
    klipp_project::save_project(&dir, &[seq], &klipp_media::MediaPool::new()).unwrap();
    let path = dir.join("project.json");
    let text = std::fs::read_to_string(&path).unwrap().replacen("\"format_revision\": 3", "\"format_revision\": 2", 1);
    std::fs::write(&path, text).unwrap();
    dir
}

/// A project already at revision 3, as the app saves it now.
fn current(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(format!("{name}.klipp"));
    let mut seq = klipp_timeline::Sequence::new_default();
    let mut c = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    c.push_effect(E::GaussianBlur { radius: 2.0 });
    seq.tracks[0].clips.push(c);
    klipp_project::save_project(&dir, &[seq], &klipp_media::MediaPool::new()).unwrap();
    dir
}

fn settings(r: &Path, projects: Vec<PathBuf>, excluded: Vec<PathBuf>, date: &str) -> Settings {
    Settings { projects, excluded: excluded.into_iter().collect::<BTreeSet<_>>(), config_dir: None, backup_root: r.join("backup/pre-v3"), date: date.into() }
}

fn reload(dir: &Path) -> Result<(), String> {
    klipp_project::load_project(dir).map(|_| ())
}

fn ok() -> Result<(), String> {
    Ok(())
}

fn hooks<'a>(crash: &'a dyn Fn(Point) -> bool, fail_write: &'a dyn Fn(usize) -> bool) -> Hooks<'a> {
    Hooks { crash, fail_write, reload: &reload }
}

const NO_CRASH: &dyn Fn(Point) -> bool = &|_| false;
const NO_FAIL: &dyn Fn(usize) -> bool = &|_| false;

/// Every file of a tree with its bytes.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut es: Vec<_> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().path()).collect();
        es.sort();
        for p in es {
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push((p.strip_prefix(base).unwrap().to_path_buf(), std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// The tree of every project of a test.
fn all(dirs: &[&PathBuf]) -> Vec<Vec<(PathBuf, Vec<u8>)>> {
    dirs.iter().map(|d| snapshot(d)).collect()
}

fn approved(s: &Settings) -> String {
    let m = step3::preflight(s);
    assert!(m.stops.is_empty(), "{:?}", m.stops);
    m.hash()
}

/// A run steps every revision-2 project, plain and picture-truth, to 3:
/// project.json differs from its backup by the revision number only, and
/// nothing else of any project changes; one at 3 and one excluded stay as
/// they are; the truth clip's own ids are kept.
#[test]
fn a_run_steps_revision_2_projects_to_3_and_nothing_else() {
    let r = root("full");
    let (a, b, c, d) = (plain(&r, "Plain"), truth(&r, "Truth"), current(&r, "Now"), plain(&r, "Left"));
    let before = all(&[&a, &b, &c, &d]);
    let s = settings(&r, vec![a.clone(), b.clone(), c.clone(), d.clone()], vec![d.clone()], "2026-10-07");
    let m = step3::preflight(&s);
    let decisions: Vec<_> = m.candidates.iter().map(|c| c.decision).collect();
    assert_eq!(decisions, [Decision::Excluded, Decision::AlreadyConverted, Decision::Convert, Decision::Convert], "by path: {m:?}");
    assert!(m.candidates[3].reason.contains("1 picture-truth"), "{}", m.candidates[3].reason);
    assert!(m.candidates[2].reason.contains("1 track effect lists"), "{}", m.candidates[2].reason);
    let report = step3::run(&s, &m.hash(), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
    assert_eq!(report.converted, vec![a.clone(), b.clone()]);
    for (dir, was) in [(&a, &before[0]), (&b, &before[1])] {
        let now = snapshot(dir);
        assert_eq!(now.len(), was.len());
        for ((p, x), (_, y)) in now.iter().zip(was.iter()) {
            if p == Path::new("project.json") {
                let back = String::from_utf8(y.clone()).unwrap().replacen("\"format_revision\": 2", "\"format_revision\": 3", 1);
                assert_eq!(String::from_utf8(x.clone()).unwrap(), back, "only the number changed");
                assert_eq!(std::fs::read(s.backup_root.join("2026-10-07").join(dir.join("project.json").strip_prefix("/").unwrap())).unwrap(), *y, "the backup is the original");
            } else {
                assert_eq!(x, y, "{} unchanged", p.display());
            }
        }
    }
    assert_eq!(snapshot(&c), before[2]);
    assert_eq!(snapshot(&d), before[3], "the excluded project, byte for byte");
    // The truth clip's ids are the ones it was saved with.
    let ids = |dir: &Path| read(&dir.join("project.json"))["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"].clone();
    assert!(ids(&b).to_string().contains(&uuid::Uuid::from_u128(0xa2).to_string()));
    let (loaded, ..) = klipp_project::load_project(&b).unwrap();
    let clip = loaded[0].all_clips().next().unwrap();
    assert!(clip.is_picture_truth());
    assert!(effects(&picture(clip)).contains(&uuid::Uuid::from_u128(0xa2)), "the added effect is reached by its own id after the conversion");
    assert_eq!(read(&step3::journal_path(&s))["state"], json!("Committed"));
    // A second run finds them all at 3.
    let s2 = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-08");
    let again = step3::run(&s2, &approved(&s2), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
    assert_eq!((again.converted.len(), again.already.len()), (0, 2));
    let _ = std::fs::remove_dir_all(&r);
}

/// The dry run reads only, writes under its output folder, says "render
/// NOT RUN" without a render check and isn't a go then; with one it is.
/// An output folder inside a project is refused.
#[test]
fn the_dry_run_reads_only_and_needs_a_render_for_the_go() {
    let r = root("dry");
    let (a, b) = (plain(&r, "Plain"), truth(&r, "Truth"));
    let before = all(&[&a, &b]);
    let s = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-07");
    let out = r.join("out");
    let report = step3::dry_run(&s, &out, "test", &reload, None).unwrap();
    assert!(report.text().contains("render NOT RUN"), "{}", report.text());
    assert!(!report.go(), "no go without a render");
    assert!(report.projects.iter().all(|p| p.proven.is_ok()), "{report:?}");
    assert_eq!(report.manifest_hash, approved(&s));
    assert!(out.join("report.txt").is_file() && out.join("manifest.json").is_file());
    // Its copies are never found as projects by a later scan.
    let found = klipp_convert::scan::discover(&r).projects;
    assert!(found.contains(&a) && found.contains(&b), "the projects are found: {found:?}");
    assert!(found.iter().all(|p| !p.starts_with(&out)), "the dry run's copies are not projects: {found:?}");
    let render = |_: &Path, _: &Path| Ok::<_, String>("3 frames match".to_owned());
    let rendered = step3::dry_run(&s, &r.join("out2"), "test", &reload, Some(&render)).unwrap();
    assert!(rendered.go(), "{}", rendered.text());
    let failing = |_: &Path, _: &Path| Err::<String, _>("a frame differs".to_owned());
    assert!(!step3::dry_run(&s, &r.join("out3"), "test", &reload, Some(&failing)).unwrap().go());
    assert!(step3::dry_run(&s, &a.join("report"), "test", &reload, None).is_err());
    assert!(step3::dry_run(&s, &r.join("backup/pre-v3/x"), "test", &reload, None).is_err());
    assert_eq!(all(&[&a, &b]), before, "the projects untouched");
    assert!(!s.backup_root.exists(), "no backup made");
    let _ = std::fs::remove_dir_all(&r);
}

/// A file changed after the dry run: the run refuses, and writes nothing.
#[test]
fn a_change_since_the_dry_run_is_refused() {
    let r = root("drift");
    let a = plain(&r, "Plain");
    let s = settings(&r, vec![a.clone()], vec![], "2026-10-07");
    let hash = approved(&s);
    std::fs::write(a.join("looks/extra.json"), json!({ "format_revision": 2, "id": uuid::Uuid::new_v4(), "name": "x", "graph_v2": {} }).to_string()).unwrap();
    let before = snapshot(&a);
    assert!(matches!(step3::run(&s, &hash, &ok, &hooks(NO_CRASH, NO_FAIL)), Err(RunError::Refused(why)) if why.contains("changed since the dry run")));
    assert_eq!(snapshot(&a), before);
    let _ = std::fs::remove_dir_all(&r);
}

/// A write failing at the second project rolls back the first and the
/// second: every project as it was, and the journal says rolled back.
#[test]
fn a_failed_write_rolls_back_every_project() {
    let r = root("fail");
    let (a, b, c) = (plain(&r, "A"), truth(&r, "B"), plain(&r, "C"));
    let before = all(&[&a, &b, &c]);
    let s = settings(&r, vec![a.clone(), b.clone(), c.clone()], vec![], "2026-10-07");
    let err = step3::run(&s, &approved(&s), &ok, &hooks(NO_CRASH, &|n| n == 1)).unwrap_err();
    assert!(matches!(&err, RunError::RolledBack(why) if why.contains("no space")), "{err:?}");
    assert_eq!(all(&[&a, &b, &c]), before);
    assert_eq!(read(&step3::journal_path(&s))["state"], json!("RolledBack"));
    let _ = std::fs::remove_dir_all(&r);
}

/// A Klipp started part way: the run stops at the next project and rolls
/// back the ones before it.
#[test]
fn a_klipp_started_part_way_rolls_back() {
    let r = root("klipp");
    let (a, b) = (plain(&r, "A"), plain(&r, "B"));
    let before = all(&[&a, &b]);
    let s = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-07");
    let calls = std::cell::Cell::new(0);
    let pre = || {
        calls.set(calls.get() + 1);
        if calls.get() > 2 { Err("a Klipp is running".to_owned()) } else { Ok(()) }
    };
    let err = step3::run(&s, &approved(&s), &pre, &hooks(NO_CRASH, NO_FAIL)).unwrap_err();
    assert!(matches!(&err, RunError::RolledBack(why) if why.contains("Klipp is running")), "{err:?}");
    assert_eq!(all(&[&a, &b]), before);
    let _ = std::fs::remove_dir_all(&r);
}

/// A crash at any point, a crash in the middle of the rollback too: the
/// next run rolls back, never forward, and every project is as it was.
#[test]
fn a_crash_anywhere_is_rolled_back_by_the_next_run() {
    let points = [Point::BackingUp(0), Point::BackingUp(5), Point::BackedUp, Point::BeforeWrite(0), Point::Written(0), Point::Verified(0), Point::BeforeWrite(1), Point::Written(1), Point::Verified(1), Point::BeforeCommit];
    for point in points {
        let r = root(&format!("crash-{point:?}").replace(['(', ')'], ""));
        let (a, b) = (plain(&r, "A"), truth(&r, "B"));
        let before = all(&[&a, &b]);
        let s = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-07");
        let crash = move |p: Point| p == point;
        assert_eq!(step3::run(&s, &approved(&s), &ok, &hooks(&crash, NO_FAIL)).unwrap_err(), RunError::Crashed(point));
        // The next run rolls back first, crashing once more in the middle.
        let again = |p: Point| p == Point::Restored(0);
        let second = step3::recover(&s, &hooks(&again, NO_FAIL));
        let s2 = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-08");
        let third = step3::run(&s2, "any", &ok, &hooks(NO_CRASH, NO_FAIL));
        match second {
            // Rolled back whole by the recovery.
            Ok(true) => assert!(matches!(&third, Err(RunError::Refused(_))), "{point:?}: {third:?}"),
            // Crashed again in the rollback: the next run finishes it.
            Err(RunError::Crashed(Point::Restored(0))) => assert!(matches!(&third, Err(RunError::RolledBack(why)) if why.contains("rolled back")), "{point:?}: {third:?}"),
            other => panic!("{point:?}: {other:?}"),
        }
        assert_eq!(all(&[&a, &b]), before, "{point:?}: as it was");
        assert_eq!(read(&step3::journal_path(&s))["state"], json!("RolledBack"), "{point:?}");
        // And a fresh run then converts them.
        let s3 = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-09");
        assert_eq!(step3::run(&s3, &approved(&s3), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap().converted.len(), 2, "{point:?}");
        let _ = std::fs::remove_dir_all(&r);
    }
}

/// A restore that fails is reported with the file, and the backups and
/// the journal are kept.
#[test]
fn a_failed_restore_is_reported_and_everything_kept() {
    let r = root("restore");
    let (a, b) = (plain(&r, "A"), plain(&r, "B"));
    let s = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-07");
    let crash = |p: Point| p == Point::Written(1);
    step3::run(&s, &approved(&s), &ok, &hooks(&crash, NO_FAIL)).unwrap_err();
    // A backup damaged before the rollback: it no longer has its hash.
    let damaged = s.backup_root.join("2026-10-07").join(a.join("project.json").strip_prefix("/").unwrap());
    std::fs::write(&damaged, b"{}").unwrap();
    match step3::recover(&s, &hooks(NO_CRASH, NO_FAIL)) {
        Err(RunError::RollbackIncomplete { unrestored, .. }) => assert_eq!(unrestored.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), vec![a.join("project.json")]),
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&step3::journal_path(&s))["state"], json!("RollingBack"), "kept, unfinished");
    assert!(s.backup_root.join("2026-10-07").join(b.join("project.json").strip_prefix("/").unwrap()).is_file());
    assert_eq!(read(&b.join("project.json"))["format_revision"], json!(2), "the other restored");
    let _ = std::fs::remove_dir_all(&r);
}

/// A track effect whose stored id isn't its slot id stops preflight too.
#[test]
fn a_track_effect_with_a_moved_id_stops_preflight() {
    let r = root("track-moved");
    let dir = plain(&r, "Track");
    let path = dir.join("project.json");
    let mut v = read(&path);
    v["sequences"][0]["tracks"][0]["effects"]["graph_v2"]["effects"][0]["id"] = json!(uuid::Uuid::new_v4().to_string());
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let stops = step3::preflight(&settings(&r, vec![dir], vec![], "d")).stops;
    assert!(stops.len() == 1 && stops[0].contains("track") && stops[0].contains("not its slot id"), "{stops:?}");
    let _ = std::fs::remove_dir_all(&r);
}

/// What preflight stops on: a project that doesn't parse, one from before
/// revision 2, a fields clip whose effect id isn't its slot id, a truth
/// clip with two nodes of one id, an exclusion naming no project, a side
/// store at another revision.
#[test]
fn preflight_stops_on_what_it_cannot_settle() {
    let r = root("stops");
    let stop = |dir: &Path, damage: &dyn Fn(&Path)| -> Vec<String> {
        damage(dir);
        step3::preflight(&settings(&r, vec![dir.to_path_buf()], vec![], "d")).stops
    };
    let set = |dir: &Path, f: &dyn Fn(&mut Value)| {
        let mut v = read(&dir.join("project.json"));
        f(&mut v);
        std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&v).unwrap()).unwrap();
    };
    let nodes = |v: &mut Value| -> Vec<Value> { v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"].as_array().unwrap().clone() };
    assert!(stop(&plain(&r, "Garbled"), &|d| std::fs::write(d.join("project.json"), b"{ not json").unwrap())[0].contains("doesn't parse"));
    assert!(stop(&plain(&r, "Old"), &|d| set(d, &|v| v["format_revision"] = json!(1)))[0].contains("stage-A"));
    let moved = stop(&plain(&r, "Moved"), &|d| {
        set(d, &|v| {
            let mut ns = nodes(v);
            let fx = ns.iter_mut().find(|n| n["legacy"]["slot"] == json!(0) && n["type_id"] == json!(types::LEGACY_EFFECT)).unwrap();
            fx["id"] = json!(uuid::Uuid::new_v4().to_string());
            v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"] = json!(ns);
        })
    });
    assert!(moved[0].contains("not its slot id"), "{moved:?}");
    let twice = stop(&truth(&r, "Twice"), &|d| {
        set(d, &|v| {
            let mut ns = nodes(v);
            let first = ns[0]["id"].clone();
            let last = ns.len() - 1;
            ns[last]["id"] = first;
            v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"] = json!(ns);
        })
    });
    assert!(twice[0].contains("two nodes have id") || twice[0].contains("isn't a node"), "{twice:?}");
    let look = stop(&plain(&r, "Look"), &|d| {
        let l = std::fs::read_dir(d.join("looks")).unwrap().next().unwrap().unwrap().path();
        std::fs::write(l, b"{\"format_revision\": 1}").unwrap();
    });
    assert!(look[0].contains("not 2"), "{look:?}");
    let a = plain(&r, "Fine");
    let unmatched = step3::preflight(&settings(&r, vec![a], vec![r.join("Gone.klipp")], "d")).stops;
    assert!(unmatched[0].contains("names no project"), "{unmatched:?}");
    let _ = std::fs::remove_dir_all(&r);
}

/// The oracle holds the written file to its original as raw JSON with only
/// the revision removed: each deliberate change fails it.
#[test]
fn the_oracle_fails_every_deliberate_change() {
    let r = root("oracle");
    let dir = truth(&r, "Truth");
    let mut v = read(&dir.join("project.json"));
    // A field no reader knows, which the oracle must keep too.
    v["sequences"][0]["tracks"][0]["clips"][0]["x_future"] = json!({ "kept": true });
    let original = serde_json::to_string_pretty(&v).unwrap();
    let written = revision3::project_text(&original).unwrap();
    revision3::verify(&original, &written).unwrap();
    let good: Value = serde_json::from_str(&written).unwrap();
    let edges = good["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["edges"].as_array().unwrap().clone();
    let fx_mask = edges.iter().position(|e| e["to"][1] == json!("mask")).expect("a mask edge");
    type Damage = Box<dyn Fn(&mut Value)>;
    let damages: Vec<(&str, Damage)> = vec![
        ("drop the mask edge", Box::new(move |v: &mut Value| {
            v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["edges"].as_array_mut().unwrap().remove(fx_mask);
        })),
        ("change a mix", Box::new(|v: &mut Value| {
            let ns = v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"].as_array_mut().unwrap();
            let n = ns.iter_mut().find(|n| n.get("mix").is_some()).expect("a mixed node");
            n["mix"] = json!(0.75);
        })),
        ("change an id", Box::new(|v: &mut Value| {
            v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"][1]["id"] = json!(uuid::Uuid::new_v4().to_string());
        })),
        ("drop a grade version", Box::new(|v: &mut Value| {
            v["sequences"][0]["tracks"][0]["clips"][0]["grade_versions"] = json!([]);
        })),
        ("drop an unknown field", Box::new(|v: &mut Value| {
            v["sequences"][0]["tracks"][0]["clips"][0].as_object_mut().unwrap().remove("x_future");
        })),
        ("change the placement", Box::new(|v: &mut Value| {
            v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["opacity"] = json!(0.5);
        })),
        ("leave the revision at 2", Box::new(|v: &mut Value| v["format_revision"] = json!(2))),
    ];
    // The truth fixture has no grade version: give the original one, so
    // dropping it is a change.
    let mut with_version: Value = serde_json::from_str(&original).unwrap();
    with_version["sequences"][0]["tracks"][0]["clips"][0]["grade_versions"] = json!([{ "name": "B", "graph_v2": { "kept": 1 } }]);
    let original_v = serde_json::to_string_pretty(&with_version).unwrap();
    // Written as the original was (pretty, keys in order), so each damage
    // below is the only difference from what the step writes.
    let good: Value = serde_json::from_str(&revision3::project_text(&original_v).unwrap()).unwrap();
    revision3::verify(&original_v, &serde_json::to_string_pretty(&good).unwrap()).unwrap();
    for (what, damage) in damages {
        let mut bad = good.clone();
        damage(&mut bad);
        assert!(revision3::verify(&original_v, &serde_json::to_string_pretty(&bad).unwrap()).is_err(), "{what} passed the oracle");
    }
    let _ = std::fs::remove_dir_all(&r);
}

/// A change no parse can see fails the oracle too: a number spelled another
/// way (`0.10` for `0.1`) and two keys in another order give the same JSON
/// value, but not the original's bytes with the revision alone changed.
#[test]
fn the_oracle_holds_the_bytes_not_only_the_value() {
    let original = "{\n  \"a\": 0.10,\n  \"b\": 1,\n  \"format_revision\": 2,\n  \"sequences\": []\n}";
    let written = revision3::project_text(original).unwrap();
    assert_eq!(written, original.replace("\"format_revision\": 2", "\"format_revision\": 3"), "the revision alone changes");
    revision3::verify(original, &written).unwrap();
    let respelled = written.replace("0.10", "0.1");
    assert_eq!(serde_json::from_str::<Value>(&respelled).unwrap(), serde_json::from_str::<Value>(&written).unwrap(), "the same value");
    assert!(revision3::verify(original, &respelled).is_err(), "a number spelled another way");
    let reordered = written.replace("\"a\": 0.10,\n  \"b\": 1,", "\"b\": 1,\n  \"a\": 0.10,");
    assert_ne!(reordered, written);
    assert!(revision3::verify(original, &reordered).is_err(), "keys in another order");
}

/// The step keeps a project file's permission bits.
#[test]
fn a_converted_file_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let r = root("mode");
    let a = plain(&r, "A");
    std::fs::set_permissions(a.join("project.json"), std::fs::Permissions::from_mode(0o640)).unwrap();
    let s = settings(&r, vec![a.clone()], vec![], "2026-10-08");
    step3::run(&s, &approved(&s), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
    assert_eq!(std::fs::metadata(a.join("project.json")).unwrap().permissions().mode() & 0o777, 0o640);
    let _ = std::fs::remove_dir_all(&r);
}
