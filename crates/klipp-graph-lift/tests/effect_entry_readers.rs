//! No dual truth (M3 step 10, amendment M3-2): after the lift an effect
//! node's values are its `params` map, and its stored entry
//! (`legacy.payload`) holds only its tag until a writer stores it again.
//! So nothing reads an effect node's entry but `effect_params` (the tag,
//! the writer, and the readers built on them) and code that reads the
//! stored form before the lift or after the writer.
//!
//! This scan counts, in each crate's sources, the lines that read a
//! legacy entry, and holds every file to the count listed here with why
//! it reads one. A new reader fails it: move it onto `effect_params` (or,
//! if it reads a grade node, list it with that reason).

use std::collections::BTreeMap;
use std::path::Path;

/// Each file that reads a legacy entry, how many lines do, and why none of
/// them reads an effect node's values.
const ALLOWED: &[(&str, usize, &str)] = &[
    ("klipp-ui/src/inspector/grade_view.rs", 1, "a grade node's stored parameter fields, reached through its grade group"),
    ("klipp-ui/src/panels/node_graph_v2/picture.rs", 5, "grade kinds and parameters only; native rejects effect node types; a plug-in node's own name (an OFX entry, #1724); a new effect's defaults for Reset and Replace, built fresh (#1745)"),
    ("klipp-graph-lift/src/effect_params.rs", 12, "the tag, the writer (into_entry, into_stored, entry_of), into_memory, and the plug-ins' stored forms (frei0r, OFX) and their tests"),
    ("klipp-graph-lift/src/lift.rs", 2, "the lift, on the stored form, before into_memory; a grade compound's subgraph (#1391)"),
    ("klipp-graph-lift/src/delift.rs", 1, "a grade compound's params, to put its subgraph back (#1391)"),
    ("klipp-graph-lift/src/placed.rs", 3, "12b-0's bridge on the stored record (deleted in 12f): an entry's tag, a plug-in's flag and values"),
    ("klipp-timeline/src/clip_anim.rs", 3, "a grade node's kind, label and parameters, for its tracks in the curve editor (#891)"),
    ("klipp-timeline/src/composition_template.rs", 1, "a grade node's stored entry, saved whole into a recipe (#2757 step 6)"),
    ("klipp-timeline/src/picture_reads.rs", 1, "an OFX node's stored entry, which is the plug-in's (M4 step 12a)"),
    ("klipp-graph-lift/src/to_v1.rs", 6, "after the delift's into_stored: the stored form"),
    ("klipp-graph-lift/src/ops.rs", 10, "grade nodes, the stored form, and the in-memory test of an effect node"),
    ("klipp-graph-lift/src/stack_view.rs", 1, "an OFX node's enabled flag"),
    ("klipp-graph-lift/src/topology.rs", 1, "a grade node's label"),
    ("klipp-compose/src/resolve.rs", 3, "a fixed placement's Transform entry and a branch's Transform beside the chain (#2758), both a move's stored entry; and a grade producer's kind"),
    ("klipp-control/src/graph_masks.rs", 1, "grade mask nodes"),
    ("klipp-control/src/graph_model.rs", 2, "a grade node's kind and params"),
    ("klipp-control/src/graph_read.rs", 2, "a grade node's params and label"),
    ("klipp-convert/src/adapter.rs", 1, "`payload`, used for every node but an effect (`entry`)"),
    ("klipp-graph-format/src/lib.rs", 1, "a grade node's kind"),
    ("klipp-graph-format/src/lifted.rs", 1, "a grade node's kind"),
    ("klipp-graph-lift/src/registry_catalog.rs", 1, "a grade node's kind"),
    ("klipp-graph-lift/src/structure.rs", 1, "a grade node's kind, for Group and Ungroup on a grade body (#1476)"),
    ("klipp-graph-lift/src/revision4.rs", 3, "the conversion to revision 4, on the stored form: a grade compound's kind and a Speed Ramp's fields"),
];

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Whether `line` reads a legacy entry.
fn reads_an_entry(line: &str) -> bool {
    let code = line.trim_start();
    if code.starts_with("//") || !code.contains(".payload") {
        return false;
    }
    code.contains("legacy") || code.contains("|l|") || code.contains("l.payload")
}

#[test]
fn only_the_listed_code_reads_an_effect_nodes_entry() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut found: BTreeMap<String, usize> = BTreeMap::new();
    for krate in std::fs::read_dir(crates).unwrap().flatten() {
        let src = krate.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        sources(&src, &mut files);
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            if name == "tests.rs" || name.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&f).unwrap();
            // A file's own tests aren't readers.
            let code = text.split("#[cfg(test)]\nmod tests").next().unwrap_or("");
            let n = code.lines().filter(|l| reads_an_entry(l)).count();
            if n > 0 {
                found.insert(f.strip_prefix(crates).unwrap().to_string_lossy().replace('\\', "/"), n);
            }
        }
    }
    let allowed: BTreeMap<String, usize> = ALLOWED.iter().map(|(f, n, _)| (f.to_string(), *n)).collect();
    let mut wrong = Vec::new();
    for (f, n) in &found {
        match allowed.get(f) {
            Some(a) if a == n => {}
            Some(a) => wrong.push(format!("{f}: {n} lines read a legacy entry, {a} are listed")),
            None => wrong.push(format!("{f}: {n} lines read a legacy entry, and it isn't listed")),
        }
    }
    for f in allowed.keys().filter(|f| !found.contains_key(*f)) {
        wrong.push(format!("{f} is listed but reads no entry: take it off"));
    }
    assert!(wrong.is_empty(), "an effect node's values are its map (M3-2); read them through effect_params:\n{}", wrong.join("\n"));
}
