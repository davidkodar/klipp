//! The run of a revision step (`klipp_convert::step3`'s machinery), as the
//! step to revision 4 (`klipp_convert::step4`) runs it, in temporary
//! folders only: every project, backup and journal here is made by the
//! test. A revision-3 project is one the app saves now, marked 3 (its
//! forms here are the same at both revisions).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use klipp_convert::run::Settings;
use klipp_convert::step3::{self, Decision, Hooks, Point, RunError};
use klipp_convert::{step4, step5};
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
    let d = std::env::temp_dir().join(format!("klipp-step-run-{name}-{}", std::process::id()));
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

/// A revision-3 project: saved by the v1 writer and converted by stage A's
/// conversion, marked 3; with a grade version, a Look, a still and the
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
    p["format_revision"] = json!(step4::FROM);
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
/// effect ids are its own, not slot ids. Saved, then marked revision 3.
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
    let mut v = read(&path);
    v["format_revision"] = json!(3);
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    dir
}

/// A project already at revision 4, as the app saves it now.
fn current(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(format!("{name}.klipp"));
    let mut seq = klipp_timeline::Sequence::new_default();
    let mut c = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    c.push_effect(E::GaussianBlur { radius: 2.0 });
    seq.tracks[0].clips.push(c);
    klipp_project::save_project(&dir, &[seq], &klipp_media::MediaPool::new()).unwrap();
    // As revision 4 saved it: the same, but for the surfaces it has none of.
    let path = dir.join("project.json");
    let mut v = read(&path);
    v["format_revision"] = json!(4);
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    dir
}

fn settings(r: &Path, projects: Vec<PathBuf>, excluded: Vec<PathBuf>, date: &str) -> Settings {
    Settings { projects, excluded: excluded.into_iter().collect::<BTreeSet<_>>(), config_dir: None, backup_root: r.join("backup/pre-v4"), date: date.into() }
}

/// A project step 4 wrote, stepped on to revision 5 (#2754) in place:
/// what today's loader reads.
fn to_five(dir: &Path) {
    let path = dir.join("project.json");
    let text = std::fs::read_to_string(&path).unwrap();
    if read(&path)["format_revision"] == json!(4) {
        std::fs::write(&path, klipp_graph_lift::revision5::project_text(&text).unwrap()).unwrap();
    }
}

/// The run's reload: the project as written, stepped to today's revision
/// on a copy beside it, which the strict loader reads.
fn reload(dir: &Path) -> Result<(), String> {
    let copy = dir.with_extension("reload");
    let _ = std::fs::remove_dir_all(&copy);
    copy_dir(dir, &copy);
    to_five(&copy);
    let loaded = klipp_project::load_project_strict(&copy).map(|_| ());
    let _ = std::fs::remove_dir_all(&copy);
    loaded
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
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
    let m = step4::preflight(s);
    assert!(m.stops.is_empty(), "{:?}", m.stops);
    m.hash()
}

/// A run steps every revision-3 project, plain and picture-truth, to 4:
/// project.json is its original's conversion, and nothing else of any
/// project changes; one at 4 and one excluded stay as they are; the truth
/// clip's own ids are kept.
#[test]
fn a_run_steps_revision_3_projects_to_4_and_nothing_else() {
    let r = root("full");
    let (a, b, c, d) = (plain(&r, "Plain"), truth(&r, "Truth"), current(&r, "Now"), plain(&r, "Left"));
    let before = all(&[&a, &b, &c, &d]);
    let s = settings(&r, vec![a.clone(), b.clone(), c.clone(), d.clone()], vec![d.clone()], "2026-10-07");
    let m = step4::preflight(&s);
    let decisions: Vec<_> = m.candidates.iter().map(|c| c.decision).collect();
    assert_eq!(decisions, [Decision::Excluded, Decision::AlreadyConverted, Decision::Convert, Decision::Convert], "by path: {m:?}");
    assert!(m.candidates[3].reason.contains("declares nothing"), "{}", m.candidates[3].reason);
    assert!(m.candidates[2].reason.contains("declares nothing"), "{}", m.candidates[2].reason);
    let report = step4::run(&s, &m.hash(), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
    assert_eq!(report.converted, vec![a.clone(), b.clone()]);
    for (dir, was) in [(&a, &before[0]), (&b, &before[1])] {
        let now = snapshot(dir);
        assert_eq!(now.len(), was.len());
        for ((p, x), (_, y)) in now.iter().zip(was.iter()) {
            if p == Path::new("project.json") {
                let back = step4::convert(&String::from_utf8(y.clone()).unwrap()).unwrap().text;
                assert_eq!(String::from_utf8(x.clone()).unwrap(), back, "the original's conversion");
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
    // Read by today's loader on a copy stepped on to revision 5.
    let five = b.with_extension("five");
    copy_dir(&b, &five);
    to_five(&five);
    let (loaded, ..) = klipp_project::load_project(&five).unwrap();
    let clip = loaded[0].all_clips().next().unwrap();
    assert!(clip.is_picture_truth());
    assert!(effects(&picture(clip)).contains(&uuid::Uuid::from_u128(0xa2)), "the added effect is reached by its own id after the conversion");
    assert_eq!(read(&step3::journal_path_with(&step4::STEP, &s))["state"], json!("Committed"));
    // A second run finds them all at 4.
    let s2 = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-08");
    let again = step4::run(&s2, &approved(&s2), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
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
    let report = step4::dry_run(&s, &out, "test", &reload, None).unwrap();
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
    let rendered = step4::dry_run(&s, &r.join("out2"), "test", &reload, Some(&render)).unwrap();
    assert!(rendered.go(), "{}", rendered.text());
    let failing = |_: &Path, _: &Path| Err::<String, _>("a frame differs".to_owned());
    assert!(!step4::dry_run(&s, &r.join("out3"), "test", &reload, Some(&failing)).unwrap().go());
    assert!(step4::dry_run(&s, &a.join("report"), "test", &reload, None).is_err());
    assert!(step4::dry_run(&s, &r.join("backup/pre-v4/x"), "test", &reload, None).is_err());
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
    assert!(matches!(step4::run(&s, &hash, &ok, &hooks(NO_CRASH, NO_FAIL)), Err(RunError::Refused(why)) if why.contains("changed since the dry run")));
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
    let err = step4::run(&s, &approved(&s), &ok, &hooks(NO_CRASH, &|n| n == 1)).unwrap_err();
    assert!(matches!(&err, RunError::RolledBack(why) if why.contains("no space")), "{err:?}");
    assert_eq!(all(&[&a, &b, &c]), before);
    assert_eq!(read(&step3::journal_path_with(&step4::STEP, &s))["state"], json!("RolledBack"));
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
    let err = step4::run(&s, &approved(&s), &pre, &hooks(NO_CRASH, NO_FAIL)).unwrap_err();
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
        assert_eq!(step4::run(&s, &approved(&s), &ok, &hooks(&crash, NO_FAIL)).unwrap_err(), RunError::Crashed(point));
        // The next run rolls back first, crashing once more in the middle.
        let again = |p: Point| p == Point::Restored(0);
        let second = step3::recover_with(&step4::STEP, &s, &hooks(&again, NO_FAIL));
        let s2 = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-08");
        let third = step4::run(&s2, "any", &ok, &hooks(NO_CRASH, NO_FAIL));
        match second {
            // Rolled back whole by the recovery.
            Ok(true) => assert!(matches!(&third, Err(RunError::Refused(_))), "{point:?}: {third:?}"),
            // Crashed again in the rollback: the next run finishes it.
            Err(RunError::Crashed(Point::Restored(0))) => assert!(matches!(&third, Err(RunError::RolledBack(why)) if why.contains("rolled back")), "{point:?}: {third:?}"),
            other => panic!("{point:?}: {other:?}"),
        }
        assert_eq!(all(&[&a, &b]), before, "{point:?}: as it was");
        assert_eq!(read(&step3::journal_path_with(&step4::STEP, &s))["state"], json!("RolledBack"), "{point:?}");
        // And a fresh run then converts them.
        let s3 = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-09");
        assert_eq!(step4::run(&s3, &approved(&s3), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap().converted.len(), 2, "{point:?}");
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
    step4::run(&s, &approved(&s), &ok, &hooks(&crash, NO_FAIL)).unwrap_err();
    // A backup damaged before the rollback: it no longer has its hash.
    let damaged = s.backup_root.join("2026-10-07").join(a.join("project.json").strip_prefix("/").unwrap());
    std::fs::write(&damaged, b"{}").unwrap();
    match step3::recover_with(&step4::STEP, &s, &hooks(NO_CRASH, NO_FAIL)) {
        Err(RunError::RollbackIncomplete { unrestored, .. }) => assert_eq!(unrestored.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), vec![a.join("project.json")]),
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&step3::journal_path_with(&step4::STEP, &s))["state"], json!("RollingBack"), "kept, unfinished");
    assert!(s.backup_root.join("2026-10-07").join(b.join("project.json").strip_prefix("/").unwrap()).is_file());
    assert_eq!(read(&b.join("project.json"))["format_revision"], json!(3), "the other restored");
    let _ = std::fs::remove_dir_all(&r);
}

/// What preflight stops on: a project that doesn't parse, one at another
/// revision than 3, a truth clip with two nodes of one id, an exclusion
/// naming no project, a side store at another revision.
#[test]
fn preflight_stops_on_what_it_cannot_settle() {
    let r = root("stops");
    let stop = |dir: &Path, damage: &dyn Fn(&Path)| -> Vec<String> {
        damage(dir);
        step4::preflight(&settings(&r, vec![dir.to_path_buf()], vec![], "d")).stops
    };
    let set = |dir: &Path, f: &dyn Fn(&mut Value)| {
        let mut v = read(&dir.join("project.json"));
        f(&mut v);
        std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&v).unwrap()).unwrap();
    };
    let nodes = |v: &mut Value| -> Vec<Value> { v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"].as_array().unwrap().clone() };
    assert!(stop(&plain(&r, "Garbled"), &|d| std::fs::write(d.join("project.json"), b"{ not json").unwrap())[0].contains("doesn't parse"));
    assert!(stop(&plain(&r, "Old"), &|d| set(d, &|v| v["format_revision"] = json!(1)))[0].contains("at revision 3 only"));
    let twice = stop(&truth(&r, "Twice"), &|d| {
        set(d, &|v| {
            let mut ns = nodes(v);
            let first = ns[0]["id"].clone();
            let last = ns.len() - 1;
            ns[last]["id"] = first;
            v["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"] = json!(ns);
        })
    });
    assert!(twice[0].contains("isn't one the lift makes"), "{twice:?}");
    let look = stop(&plain(&r, "Look"), &|d| {
        let l = std::fs::read_dir(d.join("looks")).unwrap().next().unwrap().unwrap().path();
        std::fs::write(l, b"{\"format_revision\": 1}").unwrap();
    });
    assert!(look[0].contains("not 2"), "{look:?}");
    let a = plain(&r, "Fine");
    let unmatched = step4::preflight(&settings(&r, vec![a], vec![r.join("Gone.klipp")], "d")).stops;
    assert!(unmatched[0].contains("names no project"), "{unmatched:?}");
    let _ = std::fs::remove_dir_all(&r);
}

/// Step 5 (#2754): a revision-4 project with a tracked surface is stepped
/// to 5 by the same run, each key's corners as the map to them, and it
/// loads with the surface where it was.
#[test]
fn a_run_steps_a_tracked_surface_to_revision_5() {
    let r = root("five");
    let a = current(&r, "Tracked");
    let corners = json!([[0.3, 0.3], [0.6, 0.32], [0.62, 0.6], [0.28, 0.58]]);
    let moved = json!([[0.31, 0.29], [0.62, 0.33], [0.6, 0.61], [0.27, 0.57]]);
    let mut v = read(&a.join("project.json"));
    v["sequences"][0]["tracks"][0]["clips"][0]["surfaces"] = json!([{
        "id": uuid::Uuid::new_v4(), "name": "Screen", "corners": corners, "reference_secs": 0.5,
        "keys": [{ "source_secs": 0.5, "corners": corners }, { "source_secs": 0.54, "corners": moved }],
        "source_size": [1920, 1080], "media_fps": 25.0
    }]);
    std::fs::write(a.join("project.json"), serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let s = settings(&r, vec![a.clone()], vec![], "2026-10-26");
    let m = step5::preflight(&s);
    assert!(m.stops.is_empty(), "{:?}", m.stops);
    assert_eq!(m.candidates.iter().map(|c| c.decision).collect::<Vec<_>>(), [Decision::Convert]);
    assert!(m.candidates[0].reason.contains("1 surfaces, 2 keys"), "{}", m.candidates[0].reason);
    let report = step5::run(&s, &m.hash(), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
    assert_eq!(report.converted, vec![a.clone()]);
    let (loaded, ..) = klipp_project::load_project_strict(&a).unwrap();
    let surface = &loaded[0].all_clips().next().unwrap().surfaces[0];
    let at = surface.corners_shown_at(0.54);
    let want: Vec<[f32; 2]> = serde_json::from_value(moved).unwrap();
    for (got, want) in at.iter().zip(&want) {
        assert!((got[0] - want[0]).abs() < 1e-5 && (got[1] - want[1]).abs() < 1e-5, "{at:?}");
    }
    assert_eq!(surface.keys[1].confidence, None, "the old tracker gave no confidence");
    let _ = std::fs::remove_dir_all(&r);
}

/// The proof holds the written file to its original's conversion: each
/// deliberate change fails it.
#[test]
fn the_oracle_fails_every_deliberate_change() {
    let r = root("oracle");
    let dir = truth(&r, "Truth");
    let mut v = read(&dir.join("project.json"));
    // A field no reader knows, which the oracle must keep too.
    v["sequences"][0]["tracks"][0]["clips"][0]["x_future"] = json!({ "kept": true });
    let original = serde_json::to_string_pretty(&v).unwrap();
    let written = step4::convert(&original).unwrap().text;
    step4::verify(&original, &written).unwrap();
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
        ("leave the revision at 3", Box::new(|v: &mut Value| v["format_revision"] = json!(3))),
    ];
    // The truth fixture has no grade version: give the original one, so
    // dropping it is a change.
    let mut with_version: Value = serde_json::from_str(&original).unwrap();
    let body = with_version["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["type_id"] == json!(lt::GROUP)).expect("the grade group")["group"]["graph"].clone();
    with_version["sequences"][0]["tracks"][0]["clips"][0]["grade_versions"] = json!([{ "name": "B", "node_graph": null, "graph_v2": body }]);
    let original_v = serde_json::to_string_pretty(&with_version).unwrap();
    // Written as the original was (pretty, keys in order), so each damage
    // below is the only difference from what the step writes.
    let good: Value = serde_json::from_str(&step4::convert(&original_v).unwrap().text).unwrap();
    step4::verify(&original_v, &serde_json::to_string_pretty(&good).unwrap()).unwrap();
    for (what, damage) in damages {
        let mut bad = good.clone();
        damage(&mut bad);
        assert!(step4::verify(&original_v, &serde_json::to_string_pretty(&bad).unwrap()).is_err(), "{what} passed the proof");
    }
    let _ = std::fs::remove_dir_all(&r);
}

/// Revision 3's own step (`revision3`, kept for readers that step a copy
/// 2 -> 3 -> 4): a change no parse can see fails its oracle too: a number
/// spelled another way (`0.10` for `0.1`) and two keys in another order
/// give the same JSON value, but not the original's bytes with the
/// revision alone changed.
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
    step4::run(&s, &approved(&s), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
    assert_eq!(std::fs::metadata(a.join("project.json")).unwrap().permissions().mode() & 0o777, 0o640);
    let _ = std::fs::remove_dir_all(&r);
}

/// Someone else changes the second project after the journal names it and
/// before the run writes it: the run stops, rolls the first back, and the
/// second keeps the bytes it was given and is named, the journal left
/// unfinished. A rollback never overwrites what the run didn't write.
#[test]
fn a_rollback_never_overwrites_bytes_the_run_did_not_write() {
    let r = root("theirs");
    let (a, b) = (plain(&r, "A"), plain(&r, "B"));
    let before_a = snapshot(&a);
    let s = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-07");
    let theirs = b"{\"someone\": \"else\"}".to_vec();
    let edit = |p: Point| {
        if p == Point::BeforeWrite(1) {
            std::fs::write(b.join("project.json"), &theirs).unwrap();
        }
        false
    };
    let err = step4::run(&s, &approved(&s), &ok, &hooks(&edit, NO_FAIL)).unwrap_err();
    assert!(matches!(&err, RunError::RollbackIncomplete { unrestored, .. } if unrestored.len() == 1 && unrestored[0].0 == b.join("project.json") && unrestored[0].1.contains("someone else")), "{err:?}");
    assert_eq!(snapshot(&a), before_a, "the first is rolled back");
    assert_eq!(std::fs::read(b.join("project.json")).unwrap(), theirs, "the second keeps their bytes");
    assert_eq!(read(&step3::journal_path_with(&step4::STEP, &s))["state"], json!("RollingBack"), "left unfinished for a person");
    let _ = std::fs::remove_dir_all(&r);
}

/// A file the run wrote, changed by someone else before the next run's
/// recovery: it's left as it is and named, the backups kept and the
/// journal unfinished. The crash is after the write and before the journal
/// marks it written, where nothing tells the edit from bytes that were
/// there before the write.
#[test]
fn a_file_changed_after_the_run_wrote_it_is_left_and_named() {
    let r = root("after");
    let a = plain(&r, "A");
    let s = settings(&r, vec![a.clone()], vec![], "2026-10-07");
    let crash = |p: Point| p == Point::Written(0);
    assert_eq!(step4::run(&s, &approved(&s), &ok, &hooks(&crash, NO_FAIL)).unwrap_err(), RunError::Crashed(Point::Written(0)));
    let theirs = b"{\"saved\": \"after\"}".to_vec();
    std::fs::write(a.join("project.json"), &theirs).unwrap();
    let err = step3::recover_with(&step4::STEP, &s, &hooks(NO_CRASH, NO_FAIL)).unwrap_err();
    assert!(matches!(&err, RunError::RollbackIncomplete { unrestored, .. } if unrestored.len() == 1 && unrestored[0].0 == a.join("project.json") && unrestored[0].1.contains("someone else")), "{err:?}");
    assert_eq!(std::fs::read(a.join("project.json")).unwrap(), theirs);
    assert!(s.backup_root.join("2026-10-07").exists(), "the backups kept");
    assert_eq!(read(&step3::journal_path_with(&step4::STEP, &s))["state"], json!("RollingBack"), "left unfinished for a person");
    assert_eq!(read(&step3::journal_path_with(&step4::STEP, &s))["touched"][0]["written"], json!(false), "the write wasn't marked");
    let _ = std::fs::remove_dir_all(&r);
}

/// A backup that no longer has its hash is never converted, nor proved
/// against: the run stops before writing.
#[test]
fn a_corrupt_backup_is_never_converted() {
    let r = root("corrupt");
    let a = plain(&r, "A");
    let before = snapshot(&a);
    let s = settings(&r, vec![a.clone()], vec![], "2026-10-07");
    let backup = s.backup_root.join("2026-10-07").join(a.join("project.json").strip_prefix("/").unwrap());
    let spoil = |p: Point| {
        if p == Point::BackedUp {
            std::fs::write(&backup, b"{\"format_revision\": 2}").unwrap();
        }
        false
    };
    let err = step4::run(&s, &approved(&s), &ok, &hooks(&spoil, NO_FAIL)).unwrap_err();
    assert!(matches!(&err, RunError::RolledBack(why) if why.contains("no longer has its hash")), "{err:?}");
    assert_eq!(snapshot(&a), before);
    let _ = std::fs::remove_dir_all(&r);
}

/// The dry run's output must be a fresh, empty folder reached through no
/// link and outside every project and backup by its real path; every
/// refusal leaves each original byte as it was and makes no backup.
#[test]
fn the_dry_run_output_is_fresh_plain_and_outside() {
    let r = root("dryout");
    let a = plain(&r, "Plain");
    let before = snapshot(&a);
    let s = settings(&r, vec![a.clone()], vec![], "2026-10-07");
    let refused = |out: &Path, says: &str| {
        let e = step4::dry_run(&s, out, "test", &reload, None).unwrap_err();
        assert!(e.contains(says), "{}: {e}", out.display());
    };
    // A link into the project, as the output or on its way.
    std::os::unix::fs::symlink(&a, r.join("into")).unwrap();
    refused(&r.join("into"), "link");
    refused(&r.join("into/out"), "link");
    std::os::unix::fs::symlink(a.join("media"), r.join("media-link")).unwrap();
    refused(&r.join("media-link/out"), "link");
    // A .. that would land inside it.
    refused(&r.join("elsewhere/../Plain.klipp/out"), "..");
    // Inside the project or the backup, or holding one.
    refused(&a.join("out"), "inside");
    refused(&s.backup_root.join("x"), "inside");
    refused(&r, "inside");
    // An earlier dry run's folder.
    let used = r.join("used");
    std::fs::create_dir(&used).unwrap();
    std::fs::write(used.join("report.txt"), b"old").unwrap();
    refused(&used, "isn't empty");
    assert_eq!(std::fs::read(used.join("report.txt")).unwrap(), b"old");
    assert_eq!(snapshot(&a), before, "the project untouched");
    assert!(!s.backup_root.exists(), "no backup made");
    // A fresh one, and an empty one, are taken.
    assert!(step4::dry_run(&s, &r.join("fresh/out"), "test", &reload, None).is_ok());
    std::fs::create_dir(r.join("empty")).unwrap();
    assert!(step4::dry_run(&s, &r.join("empty"), "test", &reload, None).is_ok());
    assert_eq!(snapshot(&a), before, "the project untouched");
    let _ = std::fs::remove_dir_all(&r);
}

/// A linked media, looks or .recovery folder stops preflight: the files
/// under it would be outside the project.
#[test]
fn a_linked_side_folder_stops_preflight() {
    for rel in ["media", "looks", ".recovery"] {
        let r = root(&format!("link-{}", rel.trim_start_matches('.')));
        let a = plain(&r, "A");
        let real = r.join("real");
        let at = a.join(rel);
        if at.exists() {
            std::fs::rename(&at, &real).unwrap();
        } else {
            std::fs::create_dir(&real).unwrap();
        }
        std::os::unix::fs::symlink(&real, &at).unwrap();
        let m = step4::preflight(&settings(&r, vec![a.clone()], vec![], "2026-10-07"));
        assert!(m.stops.iter().any(|s| s.contains("isn't a plain folder")), "{rel}: {:?}", m.stops);
        let _ = std::fs::remove_dir_all(&r);
    }
}

/// Give each effect node of each clip's stored picture a slot of the
/// clip's list it isn't at: the picture still reads as written now (the
/// load's upgrade, #3318, reads each entry as its effect and writes it
/// so, which a key no effect has, the spoil this was, no longer
/// survives), but the clip's fields give the node its own slot again,
/// so the picture disagrees with them (as a picture the fields read back
/// otherwise would, #1270's LUT path).
fn spoil_stored_picture(dir: &Path) {
    fn walk(v: &mut Value) -> usize {
        match v {
            Value::Object(m) if m.get("type_id") == Some(&json!("klipp.legacy.effect")) => {
                m.insert("label".into(), json!("spoiled"));
                1
            }
            Value::Object(m) => m.values_mut().map(walk).sum(),
            Value::Array(a) => a.iter_mut().map(walk).sum(),
            _ => 0,
        }
    }
    let path = dir.join("project.json");
    let mut v = read(&path);
    let mut n = 0;
    for seq in v["sequences"].as_array_mut().unwrap() {
        for track in seq["tracks"].as_array_mut().unwrap() {
            for clip in track["clips"].as_array_mut().unwrap() {
                n += walk(&mut clip["graph_v2"]);
            }
        }
    }
    assert!(n > 0, "an effect to spoil");
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

/// A clip whose stored picture its fields don't give again stops the
/// conversion before it is written: the dry run names the part of the
/// picture the conversion would drop, and the run refuses with both
/// projects as they were. (The strict loader, which the run's proof
/// reads with, refuses such a picture too; the next test.)
#[test]
fn a_stored_picture_that_disagrees_stops_the_conversion() {
    let r = root("disagree");
    let (a, b) = (plain(&r, "A"), plain(&r, "B"));
    spoil_stored_picture(&b);
    let before = all(&[&a, &b]);
    let s = settings(&r, vec![a.clone(), b.clone()], vec![], "2026-10-07");
    let dry = step4::dry_run(&s, &r.join("out"), "test", &reload, None).unwrap();
    let text = dry.text();
    assert!(text.contains(&format!("STOP {}", b.display())) && text.contains("unlift loses part of its picture") && text.contains("NOT ready"), "{text}");
    assert!(dry.projects.iter().all(|p| p.dir != b), "{text}");
    // Its preflight stops on it, so no run can be approved; one asked
    // with that preflight's hash is refused.
    let m = step4::preflight(&s);
    assert!(m.stops.iter().any(|x| x.contains("B.klipp") && x.contains("unlift loses part")), "{:?}", m.stops);
    let err = step4::run(&s, &m.hash(), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap_err();
    assert!(matches!(&err, RunError::Refused(_)), "{err:?}");
    assert_eq!(all(&[&a, &b]), before, "neither written");
    let _ = std::fs::remove_dir_all(&r);
}

/// `load_project_strict` on its own: a revision-3 project whose stored
/// picture disagrees is refused, naming the clip; the same project whole
/// loads.
#[test]
fn the_strict_loader_refuses_a_stored_picture_that_disagrees() {
    let r = root("strict");
    let a = plain(&r, "A");
    let s = settings(&r, vec![a.clone()], vec![], "2026-10-07");
    step4::run(&s, &approved(&s), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
    to_five(&a);
    assert!(klipp_project::load_project_strict(&a).is_ok());
    spoil_stored_picture(&a);
    let e = klipp_project::load_project_strict(&a).map(|_| ()).unwrap_err();
    assert!(e.contains("clip ") && e.contains("differs"), "{e}");
    let _ = std::fs::remove_dir_all(&r);
}

/// An excluded project's linked media, looks or .recovery folder, or a
/// linked file among those the manifest covers, isn't a stop, and isn't
/// followed: the run never writes an excluded project.
/// Its other files are still in the manifest, and a run holds them to
/// their hashes while it converts the rest.
#[test]
fn an_excluded_projects_links_are_not_followed_or_a_stop() {
    for rel in ["media", "looks", ".recovery"] {
        let r = root(&format!("exlink-{}", rel.trim_start_matches('.')));
        let (a, b) = (plain(&r, "A"), plain(&r, "B"));
        let real = r.join("real");
        let at = b.join(rel);
        if at.exists() {
            std::fs::rename(&at, &real).unwrap();
        } else {
            std::fs::create_dir(&real).unwrap();
        }
        std::fs::write(real.join("behind-the-link.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(&real, &at).unwrap();
        // And linked files among those the manifest covers, as a harness's
        // copy links them.
        let linked_bak = b.join("project.json.bak.linked");
        std::os::unix::fs::symlink(a.join("project.json"), &linked_bak).unwrap();
        let s = settings(&r, vec![a.clone(), b.clone()], vec![b.clone()], "2026-10-07");
        let m = step4::preflight(&s);
        assert!(m.stops.is_empty(), "{rel}: {:?}", m.stops);
        let excluded = m.candidates.iter().find(|c| c.dir == b).unwrap();
        assert_eq!(excluded.decision, Decision::Excluded);
        assert!(excluded.items.iter().all(|i| !i.path.starts_with(&at) && i.path != linked_bak), "{rel}: nothing through a link: {:?}", excluded.items);
        assert!(excluded.items.iter().any(|i| i.path == b.join("project.json")), "its own files are held");
        let before = std::fs::read(b.join("project.json")).unwrap();
        let report = step4::run(&s, &approved(&s), &ok, &hooks(NO_CRASH, NO_FAIL)).unwrap();
        assert_eq!((report.converted, report.excluded), (vec![a.clone()], vec![b.clone()]));
        assert_eq!(std::fs::read(b.join("project.json")).unwrap(), before);
        // Not excluded, the same links stop it.
        let m = step4::preflight(&settings(&r, vec![b.clone()], vec![], "2026-10-08"));
        assert!(m.stops.iter().any(|s| s.contains("isn't a plain folder")), "{rel}: {:?}", m.stops);
        std::fs::remove_file(&at).unwrap();
        let m = step4::preflight(&settings(&r, vec![b.clone()], vec![], "2026-10-08"));
        assert!(m.stops.iter().any(|s| s.contains("isn't a plain file")), "{rel}: {:?}", m.stops);
        let _ = std::fs::remove_dir_all(&r);
    }
}
