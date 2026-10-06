//! The phase-1 dry run over every real project (#1066). Writes nothing
//! but its own report.
//!
//!     cargo run --release -p klipp-convert --example dry_run -- <scan> [--report <file>]
//!
//! `<scan>` is a `klipp-convert scan` report (JSON with `projects[].path`)
//! or a plain list of project directories, one per line. Standard output
//! carries counts only; the report file (default `dry-run-report.json` in
//! the current directory) names each project, its stops and its time, and
//! stays on this machine.
//!
//! This is an example, not part of the converter, because the parity check
//! compares the frozen reader with today's reader, which only the tests and
//! examples may link.

#[allow(dead_code, unused_imports)]
#[path = "../tests/parity.rs"]
mod parity;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use klipp_convert::dry_run::{check_colour_presets, check_project_with, ProjectReport};
use klipp_convert::splice::{stage_body, track_parts, SpliceInputs, StageInputs, TrackInputs};
use klipp_convert::{installed, to_v2, StoreKind};
use serde_json::{json, Value};

/// The parity test's comparison, which asserts, turned into a result: a
/// difference is a stop for that file, not the end of the run.
fn parity(raw: &Value, kind: StoreKind) -> Result<(), String> {
    let raw = raw.clone();
    std::panic::catch_unwind(move || {
        parity::compare(raw, kind);
    })
    .map_err(|e| {
        e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_else(|| "differs".into())
    })
}

/// Whether today's merge would add `stage` to `base` (`wrap`'s prepend
/// or append, and the track grade's append): not an identity, and the
/// merge neither refuses (a working space that doesn't match, no source)
/// nor adds nothing.
fn applies(base: &klipp_nodes::NodeGraph, stage: &klipp_nodes::NodeGraph, prepend: bool) -> bool {
    if stage.is_identity() {
        return false;
    }
    let mut g = base.clone();
    let r = if prepend { g.prepend_keeping_ids(stage) } else { g.append_keeping_ids(stage) };
    r.is_ok() && &g != base
}

fn graph(v: &Value) -> Result<klipp_nodes::NodeGraph, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}

/// A clip's splice inputs, decided with today's code: its grade group's
/// stages as `wrap` takes them, then its track's grade as the bake appends
/// it to the wrapped graph, and its track's effects when the track draws
/// pictures and `touches_picture`.
fn splice_inputs(clip: &Value, seq: &Value, track: &Value) -> Result<SpliceInputs, String> {
    let own = match clip.get("node_graph").filter(|g| !g.is_null()) {
        Some(g) => graph(g)?,
        None => klipp_nodes::NodeGraph::default(),
    };
    let mut wrapped = own.clone();
    let mut inputs = SpliceInputs::default();
    let group = clip.get("grade_group").and_then(Value::as_str).and_then(|id| {
        seq.get("grade_groups").and_then(Value::as_array).into_iter().flatten().find(|g| g.get("id").and_then(Value::as_str) == Some(id))
    });
    if let Some(group) = group {
        let typed: klipp_timeline::GradeGroup = serde_json::from_value(group.clone()).map_err(|e| e.to_string())?;
        let (pre, post) = to_v2::stages_to_v2(group.as_object().ok_or("group")?).map_err(|e| e.to_string())?;
        let take = |stage: &Option<klipp_nodes::NodeGraph>, conv: Option<klipp_graph_format::Graph>, prepend: bool| -> Result<Option<klipp_graph_format::Graph>, String> {
            match (stage, conv) {
                (Some(s), Some(c)) if applies(&own, s, prepend) => stage_body(&c).map(Some),
                _ => Ok(None),
            }
        };
        inputs.stages = Some(StageInputs { group: typed.id.0, pre: take(&typed.pre, pre, true)?, post: take(&typed.post, post, false)? });
        wrapped = typed.wrap(&own);
    }
    if let Some(fx) = track.get("effects").filter(|e| !e.is_null()) {
        let typed: klipp_timeline::TrackEffects = serde_json::from_value(fx.clone()).map_err(|e| e.to_string())?;
        let picture_track = matches!(track.get("kind").and_then(Value::as_str), Some("Video" | "Text"));
        if picture_track && typed.touches_picture() {
            let structural = to_v2::track_to_v2(track.as_object().ok_or("track")?).map_err(|e| e.to_string())?;
            let (effects, grade) = track_parts(&structural)?;
            let grade = grade.filter(|_| typed.grade.as_ref().is_some_and(|g| applies(&wrapped, g, false)));
            let id = uuid::Uuid::parse_str(track.get("id").and_then(Value::as_str).unwrap_or_default()).map_err(|e| e.to_string())?;
            inputs.track = Some(TrackInputs { track: id, effects, grade });
        }
    }
    Ok(inputs)
}

fn project_dirs(scan: &str) -> Result<Vec<PathBuf>, String> {
    let text = std::fs::read_to_string(scan).map_err(|e| format!("{scan}: {e}"))?;
    if let Ok(report) = serde_json::from_str::<Value>(&text) {
        let projects = report["projects"].as_array().ok_or("the scan report has no projects list")?;
        return Ok(projects.iter().filter_map(|p| p["path"].as_str().map(PathBuf::from)).collect());
    }
    Ok(text.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(scan) = args.first() else {
        eprintln!("usage: dry_run <scan report or list> [--report <file>]");
        std::process::exit(2);
    };
    let report_path = args.iter().position(|a| a == "--report").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| "dry-run-report.json".into());
    let dirs = project_dirs(scan).unwrap_or_else(|e| {
        eprintln!("dry_run: {e}");
        std::process::exit(2);
    });
    // Quiet the parity test's assertion messages: each becomes a stop.
    std::panic::set_hook(Box::new(|_| {}));

    let started = Instant::now();
    let reports: Vec<ProjectReport> = dirs.iter().map(|d| check_project_with(d, &parity, Some(&splice_inputs))).collect();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("klipp"));
    let (presets, preset_stops) = config.as_deref().map(|c| check_colour_presets(c, &parity)).unwrap_or_default();
    let total = started.elapsed();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let installed = installed::check(&repo);

    let mut stores: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &reports {
        for (k, n) in r.stores.iter().chain(&r.stores_extra) {
            *stores.entry(k).or_default() += n;
        }
    }
    let passed = reports.iter().filter(|r| r.passed()).count();
    let stopped = reports.len() - passed;
    let stop_count: usize = reports.iter().map(|r| r.stops.len()).sum::<usize>() + preset_stops.len();
    let no_clip = reports.iter().filter(|r| r.picture_without_clips).count();
    let slowest = reports.iter().map(|r| r.elapsed).max().unwrap_or_default();

    println!("projects: {} listed, {passed} passed, {stopped} stopped ({stop_count} stops)", reports.len());
    for (k, n) in &stores {
        println!("  {k}: {n}");
    }
    println!("  colour presets: {presets}");
    println!("picture on tracks or groups with no clip: {no_clip}");
    let mut notes: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &reports {
        for (k, n) in &r.lift_notes {
            *notes.entry(k).or_default() += n;
        }
    }
    println!("lift notes: {}", if notes.is_empty() { "none".to_owned() } else { notes.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ") });
    println!("time: {:.1} s in total, slowest project {:.2} s", total.as_secs_f64(), slowest.as_secs_f64());
    println!(
        "installed package: {}",
        match &installed {
            installed::Installed::None => "none".to_owned(),
            installed::Installed::Current { .. } => "has the revision check".to_owned(),
            installed::Installed::TooOld { .. } => "predates the revision check: rebuild before the real run".to_owned(),
            installed::Installed::Unknown { .. } => "unknown build: rebuild before the real run".to_owned(),
        }
    );

    let file = json!({
        "total_secs": total.as_secs_f64(),
        "projects": reports.iter().map(|r| json!({
            "dir": r.dir,
            "passed": r.passed(),
            "stores": r.stores,
            "stops": r.stops,
            "picture_without_clips": r.picture_without_clips,
            "lift_notes": r.lift_notes,
            "millis": r.elapsed.as_millis() as u64,
        })).collect::<Vec<_>>(),
        "colour_presets": {"checked": presets, "stops": preset_stops},
        "installed": format!("{installed:?}"),
    });
    match serde_json::to_string_pretty(&file).map_err(|e| e.to_string()).and_then(|s| std::fs::write(&report_path, s).map_err(|e| e.to_string())) {
        Ok(()) => println!("report: {report_path} (names and paths; keep it local)"),
        Err(e) => eprintln!("dry_run: the report couldn't be written: {e}"),
    }
    std::process::exit(if stop_count == 0 { 0 } else { 1 });
}
