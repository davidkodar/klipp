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
use serde_json::Value;

use super::{Entry, Family, Key, Obligation, Report};

fn key(row: &str, obligation: Obligation) -> Key {
    Key { family: Family::Format, subject: row.into(), entry: Entry::Conversion, route: None, obligation }
}

/// Where the revision-3 fixtures are: real projects saved by the app at
/// revision 3 before any row landed (see their README).
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/rev3");

fn fixture_name(row: &str) -> String {
    row.replace('#', "row")
}

/// The revision-3 fixture of `row`, copied into a scratch folder of its own
/// (the conversion's saved check writes there): its folder and its
/// project file's text. Each call its own folder, so tests reading one
/// fixture at once don't share one.
fn fixture(row: &str) -> Result<(PathBuf, String), String> {
    static CALL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let name = fixture_name(row);
    let from = Path::new(FIXTURES).join(&name);
    let call = CALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("klipp-readiness-format-{name}-{}-{call}", std::process::id())).join("p.klipp");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("media")).map_err(|e| e.to_string())?;
    for file in ["project.json", "media/manifest.json"] {
        std::fs::copy(from.join(file), dir.join(file)).map_err(|e| format!("{}: {e}", from.join(file).display()))?;
    }
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
        return Err(format!("the conversion changed more than the revision: {}", first_change(&o, converted, "")));
    }
    Ok(())
}

/// Where `a` and `b` first differ, as a path and the two values.
fn first_change(a: &Value, b: &Value, at: &str) -> String {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                if x.get(k) != y.get(k) {
                    return first_change(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &format!("{at}/{k}"));
                }
            }
            String::new()
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                if p != q {
                    return first_change(p, q, &format!("{at}/{i}"));
                }
            }
            String::new()
        }
        _ => format!("{at}: {a} vs {b}"),
    }
}

/// The converted text opens, strictly, and saves again to the same
/// document (its save stamps aside).
fn saved(dir: &Path, converted: &str) -> Result<(), String> {
    // Stepped on to today's revision (#2754), which the app opens and
    // saves.
    let converted = &klipp_convert::step5::convert(converted)?.text;
    std::fs::write(dir.join("project.json"), converted).map_err(|e| e.to_string())?;
    let loaded = klipp_project::load_project_strict(dir)?;
    let again = dir.with_extension("again.klipp");
    let _ = std::fs::remove_dir_all(&again);
    klipp_project::save_project(&again, &loaded.0, &loaded.1)?;
    let stamp = |v: &mut Value| {
        if let Some(o) = v.as_object_mut() {
            for k in ["generation", "saved_at", "klipp_version", "platform", "created_at", "active_sequence_id"] {
                o.remove(k);
            }
        }
    };
    let (mut a, mut b) = (json(converted)?, json(&std::fs::read_to_string(again.join("project.json")).map_err(|e| e.to_string())?)?);
    stamp(&mut a);
    stamp(&mut b);
    if a != b {
        return Err(format!("the converted project saves again to another document, first at {}", first_change(&a, &b, "")));
    }
    Ok(())
}

/// Whether `declared` names `row`.
fn declares(declared: &[step4::Declared], row: &str) -> bool {
    declared.iter().any(|d| d.row == row)
}

/// One row's three cases: its revision-3 fixture converted; `expect`
/// holds the converted file to what revision 4 stores (from the original's
/// text); `declared` says whether the row declares a change for it.
fn row(report: &mut Report, row: &'static str, expect: impl FnOnce(&str, &Value) -> Result<(), String>, declared: bool) {
    let fixture = fixture(row);
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
    row(report, "#1406", |_, v| {
        let p = &picture(v)?["placement"];
        if p["scale"] != 1.0 || p["aspect"] != 0.5 || p.get("scale_y").is_some() {
            return Err(format!("placement {p}: not scale 1 and aspect 0.5 with no scale_y"));
        }
        Ok(())
    }, true);

    // #1394: a moved, fixed placement stays fixed and moved: nothing to
    // convert (an automatic one with a move can't be stored at revision 3).
    row(report, "#1394", unchanged_but_revision, false);

    // #1386: an enabled Opacity before the anchor, below 1, multiplies in:
    // its node is enabled in the chain, not kept off for the record, and
    // the change is declared.
    row(report, "#1386", |_, v| {
        let opacities: Vec<_> = nodes(picture(v)?).into_iter().filter(|n| n["type_id"] == klipp_graph_format::types::LEGACY_OPACITY).collect();
        let below = opacities.iter().find(|n| n["legacy"]["payload"]["value"] == 0.5).ok_or("no Opacity whose value is 0.5 in the chain")?;
        if below.get("enabled") == Some(&Value::Bool(false)) {
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
    // test). The fixture, saved at revision 3, holds every one of them.
    row(report, "#1409", |_, v| {
        let ramp = nodes(picture(v)?).into_iter().find(|n| n["legacy"]["payload"]["type"] == "speed_ramp").ok_or("no Speed Ramp in the chain")?;
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
    // was refused), so nothing converts; keeping one is held by
    // klipp-project's `sequence_grade_mix` tests, through a real save.
    row(report, "#1321", unchanged_but_revision, false);

    // #1391: a grade compound, two deep, lifts as a group whose body is
    // its subgraph, a group within it for the inner one, and nothing is
    // declared (the same picture).
    row(report, "#1391", |_, v| {
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
        if !body(inner).iter().any(|n| n["legacy"]["payload"]["params"]["exposure"].as_f64().is_some_and(|e| (e - 0.4).abs() < 1e-6)) {
            return Err("the innermost body doesn't keep its Corrector's exposure 0.4".into());
        }
        if picture(v)?.to_string().contains("compound_subgraph") {
            return Err("a compound's subgraph is still stored as its v1 payload".into());
        }
        Ok(())
    }, false);

    // klipp.frei0r (d9's fields): a frei0r plug-in is stored as its own
    // node, its plug-in by name and each value by ABI index and type, a
    // double as f64 (0.1, which an f32 can't hold); the same picture, so
    // nothing is declared.
    row(report, "klipp.frei0r", |_, v| {
        let node = nodes(picture(v)?).into_iter().find(|n| n["type_id"] == klipp_graph_format::lifted::types::FREI0R).ok_or("no klipp.frei0r node")?;
        let p = &node["params"];
        if p["plugin"]["Text"] != "frei0r:/usr/lib/frei0r-1/glow.so:Glow" || p["p0"]["Float64"] != 0.1 || p["p1"]["Bool"] != true {
            return Err(format!("the plug-in and its values aren't kept by index and type: {p}"));
        }
        if node.to_string().contains("\"params\":[") {
            return Err("the values are still a positional list".into());
        }
        Ok(())
    }, false);

    // klipp.ofx (d9's fields): an OFX plug-in is stored as its own node,
    // its plug-in, context and each value by name in its kind at full
    // precision (a double 0.1, an RGB colour); the same picture, so nothing
    // is declared.
    row(report, "klipp.ofx", |_, v| {
        let node = nodes(picture(v)?).into_iter().find(|n| n["type_id"] == klipp_graph_format::lifted::types::OFX).ok_or("no klipp.ofx node")?;
        let p = &node["params"];
        if p["plugin"]["Text"] != "org.example.Glow" || p["p.amount"]["Float64"] != 0.1 {
            return Err(format!("the plug-in and its values aren't kept by name and kind: {p}"));
        }
        if p["p.key"]["ColorF64"]["alpha"] != false || p["p.key"]["ColorF64"]["rgba"][3] != 0.0 {
            return Err(format!("the RGB colour isn't kept with alpha off and 0: {p}"));
        }
        if node.to_string().contains("param_values") {
            return Err("the values are still the entry's map".into());
        }
        Ok(())
    }, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2461: every revision-3 fixture converts to revision 4, opens
    /// strictly, and saves again to the same document, in a default run
    /// (the readiness gate that checks this too is `#[ignore]`d).
    #[test]
    fn every_revision_3_fixture_converts_opens_and_saves_again_the_same() {
        let mut names: Vec<String> = std::fs::read_dir(FIXTURES)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("project.json").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert!(names.len() >= 8, "the fixtures: {names:?}");
        for name in names {
            let (dir, text) = fixture(&name).unwrap();
            let (_, _, converted) = convert(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            saved(&dir, &converted).unwrap_or_else(|e| panic!("{name}: {e}"));
            let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        }
    }

    /// The fixtures' rows.
    const FIXTURE_ROWS: [&str; 8] = ["#1406", "#1394", "#1386", "#1409", "#1321", "#1391", "klipp.frei0r", "klipp.ofx"];

    /// Each revision-3 fixture is as recorded: its bytes the ones its README
    /// pins, at revision 3, and read by the conversion's revision-3 reader.
    /// A fixture rewritten by accident (by a save at revision 4, say) fails.
    #[test]
    fn the_revision_3_fixtures_are_as_recorded() {
        let readme = std::fs::read_to_string(Path::new(FIXTURES).join("README.md")).unwrap();
        let pinned: Vec<(&str, &str)> = readme.lines().filter_map(|l| l.split_once("  ./")).collect();
        assert_eq!(pinned.len(), FIXTURE_ROWS.len() * 2, "every fixture's two files are pinned");
        for (hash, file) in pinned {
            let bytes = std::fs::read(Path::new(FIXTURES).join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(klipp_convert::step3::sha256(&bytes), hash, "{file} isn't as recorded");
        }
        for row in FIXTURE_ROWS {
            let (_, text) = fixture(row).unwrap();
            let v = json(&text).unwrap();
            assert_eq!(v["format_revision"], 3, "{row}");
            klipp_convert::unlift::project(&v).unwrap_or_else(|e| panic!("{row}: the revision-3 reader refuses it: {e}"));
        }
    }
}
