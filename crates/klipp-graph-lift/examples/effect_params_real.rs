//! Every effect entry stored in the projects given, read into its node's
//! map and written back, is the same bytes (M3 step 10, amendment M3-2's
//! real-data half): each `klipp.legacy.effect` node anywhere in a
//! project's `project.json` (a clip's picture, a track's effect list, a
//! group body), and each entry of an old-form effect list. The projects
//! are only read.
//!
//! cargo run --release -p klipp-graph-lift --example effect_params_real -- <project.klipp>...

use klipp_graph_lift::effect_params::{from_params, to_params};
use serde_json::Value;

/// Each stored effect entry in `v`, with where it is.
fn entries<'a>(v: &'a Value, at: &str, out: &mut Vec<(String, &'a Value)>) {
    match v {
        Value::Object(o) => {
            if o.get("type_id").and_then(Value::as_str) == Some("klipp.legacy.effect") {
                if let Some(p) = o.get("legacy").and_then(|l| l.get("payload")) {
                    out.push((format!("{at}/{}", o.get("id").and_then(Value::as_str).unwrap_or("?")), p));
                }
            }
            for (k, x) in o {
                entries(x, &format!("{at}/{k}"), out);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                entries(x, &format!("{at}[{i}]"), out);
            }
        }
        _ => {}
    }
}

fn main() {
    let (mut total, mut failed, mut typed, mut text) = (0usize, 0usize, 0usize, 0usize);
    let projects: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    for project in &projects {
        let file = project.join("project.json");
        let raw = match std::fs::read_to_string(&file) {
            Ok(r) => r,
            Err(e) => {
                failed += 1;
                println!("ERROR {}  {e}", file.display());
                continue;
            }
        };
        let v: Value = serde_json::from_str(&raw).expect("project.json is JSON");
        let mut found = Vec::new();
        entries(&v, "", &mut found);
        let mut bad = 0;
        for (at, entry) in &found {
            let back = to_params(entry).and_then(|(tag, map)| {
                typed += map.values().filter(|p| !matches!(p, klipp_graph_format::ParamValue::Text(_))).count();
                text += map.values().filter(|p| matches!(p, klipp_graph_format::ParamValue::Text(_))).count();
                from_params(&tag, &map)
            });
            let same = back.as_ref().is_ok_and(|b| serde_json::to_string(b).ok() == serde_json::to_string(entry).ok());
            if !same {
                bad += 1;
                if bad <= 3 {
                    println!("  DIFF {at}: {entry} -> {back:?}");
                }
            }
        }
        total += found.len();
        failed += usize::from(bad > 0);
        println!("{}  {} ({} effect entries, {bad} differ)", if bad == 0 { "SAME " } else { "DIFF " }, project.display(), found.len());
    }
    println!("{} of {} projects round-trip byte-identical; {total} effect entries, {typed} values typed, {text} held as JSON", projects.len() - failed, projects.len());
    std::process::exit(i32::from(failed > 0));
}
