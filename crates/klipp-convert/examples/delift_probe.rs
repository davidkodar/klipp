//! `lift(delift(p)) == p`, byte for byte, over every stored picture of the
//! projects a scan lists (#1089): each clip's, and each body standing
//! alone (timeline grade, group stages, track grade, grade versions).
//! Reads only.
//!
//!     cargo run --release -p klipp-convert --example delift_probe -- <scan>

use std::path::PathBuf;

use klipp_convert::delift;
use klipp_convert::ids::{grade_id, RootKind};
use klipp_convert::lift::{lift_clip, lift_grade_only, ClipKind, LiftCtx};
use klipp_convert::revision2;
use klipp_graph_format::lifted::ClipPicture;
use klipp_graph_format::Graph;
use serde_json::Value;
use uuid::Uuid;

fn project_dirs(scan: &str) -> Vec<PathBuf> {
    let report: Value = serde_json::from_str(&std::fs::read_to_string(scan).expect("scan")).expect("scan json");
    report["projects"].as_array().expect("projects").iter().filter_map(|p| p["path"].as_str().map(PathBuf::from)).collect()
}

fn same_body(v: &Value, migrate: bool, group: Uuid) -> Result<(), String> {
    let b: Graph = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    let again = lift_grade_only(&delift::grade_only(&b, group)?, migrate)?;
    if serde_json::to_string(&b).unwrap() != serde_json::to_string(&again).unwrap() {
        return Err("a body differs after lift(delift(b))".into());
    }
    Ok(())
}

fn main() {
    let scan = std::env::args().nth(1).expect("usage: delift_probe <scan>");
    let (mut pictures, mut bodies, mut failures) = (0, 0, Vec::new());
    for dir in project_dirs(&scan) {
        let Ok(text) = std::fs::read_to_string(dir.join("project.json")) else { continue };
        let Ok(raw) = serde_json::from_str::<Value>(&text) else { continue };
        let Ok((v2, _)) = revision2::project(&raw) else { continue };
        let seqs: Vec<&Value> = match v2.get("sequences") {
            Some(Value::Array(s)) => s.iter().collect(),
            _ => v2.get("sequence").into_iter().collect(),
        };
        let mut check = |what: &str, r: Result<(), String>| {
            if let Err(e) = r {
                failures.push(format!("{}: {what}: {e}", dir.display()));
            }
        };
        let uid = |v: &Value| Uuid::parse_str(v["id"].as_str().unwrap_or_default()).unwrap_or_default();
        for seq in seqs {
            if let Some(b) = seq.get("graph_v2") {
                bodies += 1;
                check("timeline grade", same_body(b, false, grade_id(RootKind::Sequence, uid(seq), "timeline/")));
            }
            for g in seq["grade_groups"].as_array().into_iter().flatten() {
                for k in ["pre", "post"] {
                    if let Some(b) = g.get("graph_v2").and_then(|s| s.get(k)) {
                        bodies += 1;
                        check("group stage", same_body(b, false, grade_id(RootKind::GradeGroup, uid(g), &format!("{k}/"))));
                    }
                }
            }
            for t in seq["tracks"].as_array().into_iter().flatten() {
                if let Some(b) = t.get("effects").and_then(|e| e.get("graph_v2")).and_then(|v| v.get("grade")) {
                    bodies += 1;
                    check("track grade", same_body(b, true, grade_id(RootKind::Track, uid(t), "")));
                }
                for c in t["clips"].as_array().into_iter().flatten() {
                    for (n, v) in c["grade_versions"].as_array().into_iter().flatten().enumerate() {
                        if let Some(b) = v.get("graph_v2") {
                            bodies += 1;
                            check("grade version", same_body(b, true, grade_id(RootKind::Clip, uid(c), &format!("version/{n}/"))));
                        }
                    }
                    let Some(p) = c.get("graph_v2") else { continue };
                    pictures += 1;
                    check("clip", (|| {
                        let picture: ClipPicture = serde_json::from_value(p.clone()).map_err(|e| e.to_string())?;
                        let ctx = LiftCtx { clip_id: Uuid::parse_str(c["id"].as_str().unwrap_or_default()).map_err(|e| e.to_string())?, kind: ClipKind::from_json(c.get("kind"))? };
                        let (again, _) = lift_clip(&delift::clip(&picture)?, &ctx)?;
                        if serde_json::to_string(&picture).unwrap() != serde_json::to_string(&again).unwrap() {
                            return Err("differs after lift(delift(p))".into());
                        }
                        Ok(())
                    })());
                }
            }
        }
    }
    println!("clip pictures {pictures}, bodies {bodies}, failures {}", failures.len());
    for f in failures.iter().take(20) {
        println!("  {f}");
    }
}
