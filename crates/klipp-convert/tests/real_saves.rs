//! Saving a converted project changes nothing it shouldn't (#1089, stage
//! A), over every real project in a list: a temp copy of each is converted,
//! then opened and saved twice by the app's own reader and writer.
//!
//! - **The first save** leaves every picture store exactly as the
//!   conversion wrote it.
//! - **The second save** changes nothing anywhere, save metadata aside.
//! - **Once-only normalisations:** fields outside the pictures that the first
//!   save rewrites (today's writer fills an old file's missing fields once)
//!   are reported by path, so the PR can name each one.
//! - **Save cost:** the revision-2 save is timed against today's v1 save of
//!   the same model, for the 5 % budget.
//!
//! Ignored: it needs `KLIPP_V1_PROJECT_LIST`, a file of real `project.json`
//! paths (the converter's scan writes one). Run it by hand on the machine
//! with the projects; only temp copies are touched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

/// Keys every save writes afresh: when and by what, and the pair's id.
const SAVE_METADATA: [&str; 3] = ["saved_at", "generation", "klipp_version"];

fn temp_copy(src: &Path, n: usize) -> PathBuf {
    let dst = std::env::temp_dir().join(format!("klipp-real-saves-{}-{n}", std::process::id())).join("project.klipp");
    let _ = std::fs::remove_dir_all(dst.parent().unwrap());
    std::fs::create_dir_all(dst.join("media")).unwrap();
    std::fs::copy(src.join("project.json"), dst.join("project.json")).unwrap();
    std::fs::copy(src.join("media/manifest.json"), dst.join("media/manifest.json")).unwrap();
    dst
}

fn read(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap()
}

fn strip_metadata(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        for k in SAVE_METADATA {
            o.remove(k);
        }
    }
    v
}

/// Every `graph_v2` in a project, by where it sits, with array indices kept.
fn pictures(v: &Value, at: &str, out: &mut BTreeMap<String, Value>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                let here = format!("{at}/{k}");
                if k == "graph_v2" {
                    out.insert(here, x.clone());
                } else {
                    pictures(x, &here, out);
                }
            }
        }
        Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| pictures(x, &format!("{at}/{i}"), out)),
        _ => {}
    }
}

/// The paths where `a` and `b` differ, outside any `graph_v2`, with array
/// indices folded to `[]` so one field changed on every clip is one entry.
fn changed_fields(a: &Value, b: &Value, at: &str, out: &mut std::collections::BTreeSet<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                if k == "graph_v2" {
                    continue;
                }
                let here = format!("{at}/{k}");
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => changed_fields(p, q, &here, out),
                    _ => {
                        out.insert(here);
                    }
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            x.iter().zip(y).for_each(|(p, q)| changed_fields(p, q, &format!("{at}/[]"), out));
        }
        _ if a != b => {
            out.insert(at.to_string());
        }
        _ => {}
    }
}

/// Save what `load_project` gave back, all of it, as the app does.
macro_rules! save_full {
    ($dir:expr, $r:expr) => {{
        let r = &$r;
        klipp_project::save_project_full($dir, &r.0, &r.1, &r.2, r.3.as_deref(), r.4.as_deref(), &r.5, 0, r.6, r.7, r.8, &r.9).unwrap()
    }};
}

#[test]
#[ignore = "needs KLIPP_V1_PROJECT_LIST naming real project.json paths"]
fn saving_a_converted_real_project_changes_nothing_it_shouldnt() {
    let list = std::env::var_os("KLIPP_V1_PROJECT_LIST").expect("set KLIPP_V1_PROJECT_LIST");
    let paths = std::fs::read_to_string(list).unwrap();
    let (mut projects, mut skipped) = (0, 0);
    let mut once_only: BTreeMap<String, usize> = BTreeMap::new();
    let mut picture_changes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut lut_rewrites = 0;
    let mut worst_conversion = (0.0_f64, String::new());
    let (mut t_v1, mut t_v2) = (Vec::new(), Vec::new());
    for (n, line) in paths.lines().filter(|l| !l.is_empty()).enumerate() {
        let src = Path::new(line).parent().unwrap();
        if !src.join("project.json").is_file() || !src.join("media/manifest.json").is_file() {
            skipped += 1;
            continue;
        }
        let dir = temp_copy(src, n);
        let v1 = read(&dir);
        let Ok(v1_model) = klipp_project::load_project_v1(&dir) else {
            skipped += 1; // today's reader refuses it (a broken pair, say): not a conversion question
            continue;
        };
        // Today's reader-and-writer rewrite of LUT paths, applied by the
        // conversion for this machine's library (#1089).
        let mut v1 = v1;
        let library = klipp_convert::v1::assets::bundled_luts_dir();
        let rule = |s: &str| klipp_convert::v1::assets::portable_lut_path(&klipp_convert::v1::assets::resolve_lut_path(s, library), library);
        lut_rewrites += klipp_graph_lift::load_rewrites::portable_lut_paths(&mut v1, &rule).len();
        let (converted, _notes) = klipp_graph_lift::revision2::project(&v1).unwrap_or_else(|e| panic!("{line}: {e}"));
        std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&converted).unwrap()).unwrap();

        // The first save: pictures exactly as converted.
        let first_model = klipp_project::load_project(&dir).unwrap_or_else(|e| panic!("{line}: the converted file doesn't load: {e}"));
        save_full!(&dir, first_model);
        let first = read(&dir);
        let (mut want, mut got) = (BTreeMap::new(), BTreeMap::new());
        pictures(&converted, "", &mut want);
        pictures(&first, "", &mut got);
        assert_eq!(want.keys().collect::<Vec<_>>(), got.keys().collect::<Vec<_>>(), "{line}: the first save moved picture stores");
        for (at, w) in &want {
            let mut inside = std::collections::BTreeSet::new();
            changed_fields_all(w, got.get(at).unwrap_or(&Value::Null), "", &mut inside);
            for f in inside {
                picture_changes.entry(f).or_default().push(line.to_string());
            }
        }
        let mut fields = std::collections::BTreeSet::new();
        changed_fields(&strip_metadata(converted.clone()), &strip_metadata(first.clone()), "", &mut fields);
        for f in fields {
            *once_only.entry(f).or_default() += 1;
        }

        // The second save: nothing at all.
        let second_model = klipp_project::load_project(&dir).unwrap_or_else(|e| panic!("{line}: the first save doesn't load: {e}"));
        let t = Instant::now();
        save_full!(&dir, second_model);
        t_v2.push(t.elapsed().as_secs_f64());
        assert_eq!(strip_metadata(read(&dir)), strip_metadata(first), "{line}: the second save changed the file");

        // The budget for stage A (#1089): what a save adds, the conversion
        // of the whole project, in memory, median of seven runs.
        let mut runs: Vec<f64> = (0..7)
            .map(|_| {
                let t = Instant::now();
                std::hint::black_box(klipp_graph_lift::revision2::project(std::hint::black_box(&v1)).unwrap());
                t.elapsed().as_secs_f64()
            })
            .collect();
        runs.sort_by(f64::total_cmp);
        let conversion = runs[runs.len() / 2];
        if conversion > worst_conversion.0 {
            worst_conversion = (conversion, line.to_string());
        }

        // Today's save of the same model, for the cost.
        let v1_dir = dir.with_file_name("v1.klipp");
        std::fs::create_dir_all(v1_dir.join("media")).unwrap();
        std::fs::copy(src.join("media/manifest.json"), v1_dir.join("media/manifest.json")).unwrap();
        let t = Instant::now();
        klipp_project::save_project_v1(&v1_dir, &v1_model.0, &v1_model.1).unwrap();
        t_v1.push(t.elapsed().as_secs_f64());

        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        projects += 1;
    }
    assert!(projects > 0, "the list held no readable project");
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let (max1, max2) = (t_v1.iter().cloned().fold(0.0, f64::max), t_v2.iter().cloned().fold(0.0, f64::max));
    let (m1, m2) = (median(&mut t_v1), median(&mut t_v2));
    let (s1, s2): (f64, f64) = (t_v1.iter().sum(), t_v2.iter().sum());
    println!("{projects} real projects saved twice after conversion; {skipped} skipped (missing or refused by today's reader)");
    println!("save, median per project: v1 {:.2} ms, revision 2 {:.2} ms; total v1 {:.0} ms, revision 2 {:.0} ms ({:+.1} %)", m1 * 1e3, m2 * 1e3, s1 * 1e3, s2 * 1e3, (s2 / s1 - 1.0) * 100.0);
    println!("save, slowest project: v1 {:.2} ms, revision 2 {:.2} ms", max1 * 1e3, max2 * 1e3);
    println!("LUT paths made portable by the conversion: {lut_rewrites}");
    println!("fields the first save changes INSIDE the pictures (must be none), by projects:");
    for (f, ps) in &picture_changes {
        println!("  {f}: {} (e.g. {})", ps.len(), ps[0]);
    }
    println!("fields the first save rewrites outside the pictures (once only), by projects:");
    for (f, n) in &once_only {
        println!("  {f}: {n}");
    }
    // 10 ms keeps the slowest save inside one 60 fps frame; stage B retires
    // the per-save conversion, and this budget with it. Held only in a
    // release build: a debug build's timing says nothing about it.
    println!("conversion on save, slowest project (median of 7): {:.2} ms ({})", worst_conversion.0 * 1e3, worst_conversion.1);
    if !cfg!(debug_assertions) {
        assert!(worst_conversion.0 < 0.010, "a save's conversion takes {:.2} ms on {}, over the 10 ms budget", worst_conversion.0 * 1e3, worst_conversion.1);
    }
    assert!(picture_changes.is_empty(), "the first save changed picture stores: {:?}", picture_changes.keys().collect::<Vec<_>>());
}

/// [`changed_fields`], inside a picture too, folding node and edge indices.
fn changed_fields_all(a: &Value, b: &Value, at: &str, out: &mut std::collections::BTreeSet<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                let here = format!("{at}/{k}");
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => changed_fields_all(p, q, &here, out),
                    _ => {
                        out.insert(here);
                    }
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            x.iter().zip(y).for_each(|(p, q)| changed_fields_all(p, q, &format!("{at}/[]"), out));
        }
        _ if a != b => {
            out.insert(at.to_string());
        }
        _ => {}
    }
}
