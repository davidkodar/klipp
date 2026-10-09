//! The Format family's cases (M4 step F, picture revision 4): for each
//! row, a revision-3 project planting its case, saved by today's writer,
//! converted by `klipp_convert::step4`, and held to what revision 4 must
//! store, the change it must declare, and a converted file that opens and
//! saves again unchanged (`Klipp-notes` step F design §5).
//!
//! Until a row lands, its cases fail (`step4::NOT_YET`, or the old form),
//! and the gate lists them open.

use std::path::{Path, PathBuf};

use klipp_convert::step4;
use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::{Clip, GradeAccess, Sequence};
use serde_json::Value;

use super::{Entry, Family, Key, Obligation, Report};

fn key(row: &str, obligation: Obligation) -> Key {
    Key { family: Family::Format, subject: row.into(), entry: Entry::Conversion, route: None, obligation }
}

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn clip() -> Clip {
    Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))
}

fn transform(x: f32, scale_x: f32, scale_y: f32) -> E {
    E::Transform { position_x: x, position_y: 0.0, scale_x, scale_y, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

/// A revision-3 project of one sequence built by `build`, saved by
/// today's writer into a scratch folder of its own: its folder and its
/// project file's text.
fn project(name: &str, build: impl FnOnce(&mut Sequence)) -> Result<(PathBuf, String), String> {
    let dir = std::env::temp_dir().join(format!("klipp-readiness-format-{name}-{}", std::process::id())).join("p.klipp");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut seq = Sequence::new_default();
    build(&mut seq);
    klipp_project::save_project(&dir, &[seq], &klipp_media::MediaPool::new())?;
    let text = std::fs::read_to_string(dir.join("project.json")).map_err(|e| e.to_string())?;
    Ok((dir, text))
}

fn json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// The first clip's stored picture in a project file.
fn picture(project: &Value) -> Result<&Value, String> {
    project["sequences"][0]["tracks"].as_array().and_then(|ts| ts.iter().flat_map(|t| t["clips"].as_array().into_iter().flatten()).next()).map(|c| &c["graph_v2"]).ok_or_else(|| "no clip".into())
}

/// The picture's nodes, its grade groups' bodies included.
fn nodes(picture: &Value) -> Vec<&Value> {
    fn walk<'a>(graph: &'a Value, out: &mut Vec<&'a Value>) {
        for n in graph["nodes"].as_array().into_iter().flatten() {
            out.push(n);
            if let Some(body) = n.get("group").and_then(|g| g.get("graph")) {
                walk(body, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&picture["graph"], &mut out);
    out
}

/// The file converted, held to revision 4: the conversion, its declared
/// changes, and its text.
fn convert(text: &str) -> Result<(Value, Vec<step4::Declared>, String), String> {
    let converted = step4::convert(text)?;
    let value = json(&converted.text)?;
    if value["format_revision"] != 4 {
        return Err(format!("the converted file is at revision {}", value["format_revision"]));
    }
    Ok((value, converted.declared, converted.text))
}

/// The converted file is the original, revision 4 aside: a row that
/// changes nothing in it.
fn unchanged_but_revision(original: &str, converted: &Value) -> Result<(), String> {
    let mut o = json(original)?;
    o["format_revision"] = Value::from(4);
    if &o != converted {
        return Err("the conversion changed more than the revision".into());
    }
    Ok(())
}

/// The converted text opens, strictly, and saves again to the same
/// document (its save stamps aside).
fn saved(dir: &Path, converted: &str) -> Result<(), String> {
    std::fs::write(dir.join("project.json"), converted).map_err(|e| e.to_string())?;
    let loaded = klipp_project::load_project_strict(dir)?;
    let again = dir.with_extension("again.klipp");
    let _ = std::fs::remove_dir_all(&again);
    klipp_project::save_project(&again, &loaded.0, &loaded.1)?;
    let stamp = |v: &mut Value| {
        if let Some(o) = v.as_object_mut() {
            for k in ["generation", "saved_at", "created_at", "active_sequence_id"] {
                o.remove(k);
            }
        }
    };
    let (mut a, mut b) = (json(converted)?, json(&std::fs::read_to_string(again.join("project.json")).map_err(|e| e.to_string())?)?);
    stamp(&mut a);
    stamp(&mut b);
    if a != b {
        return Err("the converted project saves again to another document".into());
    }
    Ok(())
}

/// Whether `declared` names `row`.
fn declares(declared: &[step4::Declared], row: &str) -> bool {
    declared.iter().any(|d| d.row == row)
}

/// One row's three cases: `build` plants its case; `expect` holds the
/// converted file to what revision 4 stores (from the original's text);
/// `declared` says whether the row declares a change for it.
fn row(report: &mut Report, row: &'static str, build: impl FnOnce(&mut Sequence), expect: impl FnOnce(&str, &Value) -> Result<(), String>, declared: bool) {
    let fixture = project(&row.replace('#', "row"), build);
    let mut declared_lines = Vec::new();
    let converted = fixture.as_ref().map_err(Clone::clone).and_then(|(_, text)| convert(text).map(|c| (text.clone(), c)));
    report.case(&format!("{row} converted"), [key(row, Obligation::Converted)], || {
        let (original, (value, _, _)) = converted.as_ref().map_err(Clone::clone)?;
        expect(original, value)
    });
    report.case(&format!("{row} declared"), [key(row, Obligation::DeclaredChange)], || {
        let (_, (_, d, _)) = converted.as_ref().map_err(Clone::clone)?;
        for change in d.iter().filter(|c| c.row == row) {
            declared_lines.push(change.what.clone());
        }
        match (declared, declares(d, row)) {
            (true, false) => Err(format!("{row}'s change isn't declared: {d:?}")),
            (false, true) => Err(format!("{row} declares a change on a project it leaves alone: {d:?}")),
            _ => Ok(()),
        }
    });
    for what in declared_lines {
        report.declare(row, what);
    }
    report.case(&format!("{row} saved"), [key(row, Obligation::Saved)], || {
        let (dir, _) = fixture.as_ref().map_err(Clone::clone)?;
        let (_, (_, _, text)) = converted.as_ref().map_err(Clone::clone)?;
        saved(dir, text)
    });
}

pub fn run(report: &mut Report) {
    // #1406: a Transform whose Scale Y differs from X keeps its aspect
    // (Y against X) in the placement, no Scale Y of its own, and the change
    // is declared.
    row(report, "#1406", |s| {
        let mut c = clip();
        c.set_effect_list(vec![transform(0.0, 1.0, 0.5)], vec![true]);
        s.tracks[0].clips.push(c);
    }, |_, v| {
        let p = &picture(v)?["placement"];
        if p["scale"] != 1.0 || p["aspect"] != 0.5 || p.get("scale_y").is_some() {
            return Err(format!("placement {p}: not scale 1 and aspect 0.5 with no scale_y"));
        }
        Ok(())
    }, true);

    // #1394: a moved, fixed placement stays fixed and moved: nothing to
    // convert (an automatic one with a move can't be stored at revision 3).
    row(report, "#1394", |s| {
        let mut c = clip();
        c.set_effect_list(vec![transform(100.0, 1.0, 1.0)], vec![true]);
        s.tracks[0].clips.push(c);
    }, unchanged_but_revision, false);

    // #1386: an enabled Opacity before the anchor, below 1, multiplies in:
    // its node is enabled in the chain, not kept off for the record, and
    // the change is declared.
    row(report, "#1386", |s| {
        let mut c = clip();
        c.set_effect_list(vec![E::Opacity { value: 0.5 }, E::Opacity { value: 1.0 }], vec![true, true]);
        s.tracks[0].clips.push(c);
    }, |_, v| {
        let opacities: Vec<_> = nodes(picture(v)?).into_iter().filter(|n| n["type_id"] == klipp_graph_format::types::LEGACY_OPACITY).collect();
        let below = opacities.iter().find(|n| n["legacy"]["payload"]["value"] == 0.5).ok_or("no Opacity whose value is 0.5 in the chain")?;
        if below["enabled"] != true {
            return Err(format!("the Opacity at 0.5 is kept off: {below}"));
        }
        Ok(())
    }, true);

    // #1409: a Speed Ramp keeps what changes the picture, its speed (it
    // retimes the clip) and its blend mode (frame blending); the fields
    // that never drew (its ramp curve, start and end, reverse, freeze-at)
    // and Freeze (a 0.1× slow motion, not a hold) are dropped. Nothing is
    // declared: none of them changed a frame, and the fixture doesn't
    // freeze (a frozen one becomes speed 1, declared, in the row's own
    // test).
    row(report, "#1409", |s| {
        let mut c = clip();
        c.set_effect_list(vec![E::SpeedRamp { speed: 1.0, blend_mode: 1 }], vec![true]);
        s.tracks[0].clips.push(c);
    }, |_, v| {
        let ramp = nodes(picture(v)?).into_iter().find(|n| n["type_id"] == klipp_graph_format::types::LEGACY_SPEED_RAMP).ok_or("no Speed Ramp in the chain")?;
        let payload = &ramp["legacy"]["payload"];
        for dead in ["ramp_curve", "ramp_start", "ramp_end", "reverse", "freeze_at", "freeze"] {
            if payload.get(dead).is_some() {
                return Err(format!("the Speed Ramp still stores {dead}: {payload}"));
            }
        }
        if payload["speed"] != 1.0 || payload["blend_mode"] != 1 {
            return Err(format!("the Speed Ramp doesn't keep its speed 1 and blend mode 1: {payload}"));
        }
        Ok(())
    }, false);

    // #1321: a sequence grade at revision 3 holds no correction mix (it
    // was refused), so nothing converts; keeping one is the row's own
    // test of the revision-4 store.
    row(report, "#1321", |s| {
        let mut g = klipp_nodes::NodeGraph::default();
        g.ensure_corrector();
        s.timeline_grade_slot().set(Some(g));
        s.tracks[0].clips.push(clip());
    }, unchanged_but_revision, false);

    // #1391: a grade compound, two deep, lifts as a group whose body is
    // its subgraph, a group within it for the inner one, and nothing is
    // declared (the same picture).
    row(report, "#1391", |s| {
        let mut c = clip();
        let inner = compound(corrector_graph());
        let outer = compound(with_node(corrector_graph(), inner));
        let g = corrector_graph();
        let anchor = g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).map(|n| n.id);
        c.set_grade(g);
        c.insert_grade_node(anchor, outer);
        s.tracks[0].clips.push(c);
    }, |_, v| {
        // The clip's grade group holds the outer compound as a group, whose
        // body holds the inner one as a group, whose body holds its
        // Corrector at exposure 0.4: nested, not side by side.
        let group = |n: &&Value| n["type_id"] == klipp_graph_format::lifted::types::GROUP;
        let body = |n: &Value| -> Vec<Value> { n["group"]["graph"]["nodes"].as_array().cloned().unwrap_or_default() };
        let top: Vec<&Value> = picture(v)?["graph"]["nodes"].as_array().map(|ns| ns.iter().filter(group).collect()).unwrap_or_default();
        let grade = top.first().ok_or("no grade group on the clip")?;
        let outer: Vec<Value> = body(grade).into_iter().filter(|n| group(&n)).collect();
        let [outer] = outer.as_slice() else { return Err(format!("{} groups in the grade's body, not the one compound", outer.len())) };
        let inner: Vec<Value> = body(outer).into_iter().filter(|n| group(&n)).collect();
        let [inner] = inner.as_slice() else { return Err(format!("{} groups in the compound's body, not the one inside it", inner.len())) };
        if !body(inner).iter().any(|n| n["legacy"]["payload"]["params"]["exposure"] == 0.4) {
            return Err("the innermost body doesn't keep its Corrector's exposure 0.4".into());
        }
        if picture(v)?.to_string().contains("compound_subgraph") {
            return Err("a compound's subgraph is still stored as its v1 payload".into());
        }
        Ok(())
    }, false);
}

/// A grade graph holding a Corrector.
fn corrector_graph() -> klipp_nodes::NodeGraph {
    let mut g = klipp_nodes::NodeGraph::default();
    let c = g.ensure_corrector();
    if let Some(n) = g.nodes.iter_mut().find(|n| n.id == c) {
        n.params.exposure = Some(0.4);
    }
    g
}

/// `graph` with `node` added after its Corrector.
fn with_node(mut graph: klipp_nodes::NodeGraph, node: klipp_nodes::Node) -> klipp_nodes::NodeGraph {
    let mut c = clip();
    c.set_grade(std::mem::take(&mut graph));
    let anchor = c.grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).map(|n| n.id);
    c.insert_grade_node(anchor, node);
    c.grade().clone()
}

/// A Compound node whose subgraph is `body`.
fn compound(body: klipp_nodes::NodeGraph) -> klipp_nodes::Node {
    let kind = klipp_nodes::NodeKind::Compound;
    let params = klipp_nodes::NodeParams { compound_subgraph: Some(body), ..Default::default() };
    klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind, label: kind.display_name().to_string(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}
