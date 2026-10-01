//! A common read view over Klipp's live render and analysis services.
//! The jobs remain owned by those services, so reconnecting clients can
//! read their latest known state by ID without relying on MCP session data.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::ControlContext;

#[derive(Clone)]
struct Observed {
    first_seen_at_ms: u128,
    updated_at_ms: u128,
    finished_at_ms: Option<u128>,
    signature: Value,
}

static OBSERVED: OnceLock<Mutex<HashMap<String, Observed>>> = OnceLock::new();

#[derive(Clone)]
enum Submitted {
    Import { path: std::path::PathBuf, analyze: bool, project_identity: u64 },
    Sync { media_ids: Vec<String>, existing_groups: Vec<String>, project_identity: u64 },
}

static SUBMITTED: OnceLock<Mutex<HashMap<String, Submitted>>> = OnceLock::new();

fn submitted() -> &'static Mutex<HashMap<String, Submitted>> {
    SUBMITTED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn project_identity(ctx: &ControlContext) -> u64 {
    ctx.shared.project.lock().unwrap_or_else(|e| e.into_inner()).identity
}

pub(super) fn register_import(ctx: &ControlContext, operation_id: &str, path: std::path::PathBuf, analyze: bool) {
    submitted().lock().unwrap_or_else(|e| e.into_inner()).insert(operation_id.to_owned(),
        Submitted::Import { path, analyze, project_identity: project_identity(ctx) });
}

pub(super) fn existing_multicam_groups(ctx: &ControlContext) -> Vec<String> {
    let Ok(sequence) = ctx.sequence.read() else { return Vec::new(); };
    sequence.tracks.iter().flat_map(|track| track.clips.iter())
        .filter_map(|clip| clip.multicam_group.map(|group| format!("{group:?}")))
        .collect()
}

pub(super) fn register_sync(ctx: &ControlContext, operation_id: &str, media_ids: Vec<String>, existing_groups: Vec<String>) {
    submitted().lock().unwrap_or_else(|e| e.into_inner()).insert(operation_id.to_owned(),
        Submitted::Sync { media_ids, existing_groups, project_identity: project_identity(ctx) });
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

fn with_timestamps(jobs: &mut [Value]) {
    let store = OBSERVED.get_or_init(|| Mutex::new(HashMap::new()));
    let mut store = store.lock().unwrap_or_else(|e| e.into_inner());
    let now = now_ms();
    for job in jobs {
        let id = job["job_id"].as_str().unwrap_or_default().to_owned();
        let terminal = is_terminal(job);
        let entry = store.entry(id).or_insert_with(|| Observed {
            first_seen_at_ms: now, updated_at_ms: now,
            finished_at_ms: terminal.then_some(now), signature: job.clone(),
        });
        if entry.signature != *job {
            entry.signature = job.clone();
            entry.updated_at_ms = now;
        }
        if terminal && entry.finished_at_ms.is_none() { entry.finished_at_ms = Some(now); }
        job["first_seen_at_ms"] = json!(entry.first_seen_at_ms);
        job["updated_at_ms"] = json!(entry.updated_at_ms);
        job["finished_at_ms"] = json!(entry.finished_at_ms);
    }
}

fn is_terminal(job: &Value) -> bool {
    matches!(job["state"].as_str(), Some("completed" | "failed" | "cancelled" | "unavailable"))
}

pub(super) fn snapshot(ctx: &ControlContext) -> Vec<Value> {
    let exports = ctx.shared.exports.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut jobs: Vec<Value> = exports.into_iter().map(|render| {
        let state = match render.status.as_str() {
            "complete" => "completed", "failed" => "failed", "cancelled" => "cancelled",
            "rendering" => "running", _ => "queued",
        };
        json!({
            "job_id": render.job_id, "kind": "render", "state": state,
            "phase": render.stage, "progress": render.progress.clamp(0.0, 1.0),
            "completed_units": render.frames_done, "total_units": render.total_frames,
            "unit": "frames", "eta_secs": render.eta_secs, "error": render.error,
            "artifact": if state == "completed" { Some(render.path.to_string_lossy().to_string()) } else { None },
        })
    }).collect();
    jobs.extend(klipp_analysis::service::all().into_iter().map(|analysis| {
        let state = match analysis.state {
            klipp_analysis::service::State::Queued => "queued",
            klipp_analysis::service::State::Running => "running",
            klipp_analysis::service::State::Done => "completed",
            klipp_analysis::service::State::Failed => "failed",
            klipp_analysis::service::State::Cancelled => "cancelled",
        };
        json!({
            "job_id": analysis.media_id, "kind": "analysis", "state": state,
            "phase": analysis.current.unwrap_or(state), "progress": null,
            "completed_units": analysis.done.len() + analysis.failed.len(), "total_units": null,
            "unit": "analysers", "eta_secs": null,
            "error": analysis.failed.first().map(|(stage, why)| format!("{stage}: {why}")),
            "artifact": null, "result": if state == "completed" { Some(analysis.filename) } else { None },
        })
    }));
    let submitted: Vec<_> = submitted().lock().unwrap_or_else(|e| e.into_inner())
        .iter().map(|(id, work)| (id.clone(), work.clone())).collect();
    for (operation_id, work) in submitted {
        let operation = crate::operations::status(&operation_id).ok();
        let refused = operation.as_ref().is_some_and(|status| status["state"] == "refused");
        let error = operation.as_ref().and_then(|status| status["reason"].as_str()).map(str::to_owned);
        match work {
            Submitted::Import { path, analyze, project_identity: expected_project } => {
                let project_changed = expected_project != project_identity(ctx);
                let media_id = ctx.media_pool.read().ok().and_then(|pool| pool.find_by_path(&path).map(|item| item.id.0.to_string()));
                let state = if project_changed { "unavailable" } else if refused { "failed" } else if media_id.is_some() { "completed" }
                    else if operation.as_ref().is_some_and(|status| status["state"] == "queued") { "queued" } else { "running" };
                jobs.push(json!({
                    "job_id": operation_id, "kind":"import", "state":state,
                    "phase": if state == "completed" { "in_media_pool" } else if state == "unavailable" { "project_changed" }
                        else if state == "failed" { "ui_refused" }
                        else if state == "queued" { "waiting_for_ui" } else { "probing_media" },
                    "progress": if state == "completed" { Some(1.0) } else { None },
                    "completed_units": if state == "completed" { 1 } else { 0 }, "total_units":1, "unit":"file",
                    "eta_secs":null, "error":if project_changed { Some("the open project changed; this import outcome is no longer observable here".to_string()) } else { error }, "artifact":null,
                    "result":media_id.as_ref().filter(|_| state == "completed").map(|id| json!({ "media_id":id,
                        "analysis_job_id":if analyze { Some(id) } else { None } })),
                }));
            }
            Submitted::Sync { media_ids, existing_groups, project_identity: expected_project } => {
                let project_changed = expected_project != project_identity(ctx);
                let group = ctx.sequence.read().ok().and_then(|sequence| {
                    let mut groups = HashMap::<String, Vec<String>>::new();
                    for clip in sequence.tracks.iter().flat_map(|track| track.clips.iter()) {
                        if let (Some(group), Some(id)) = (clip.multicam_group, clip.media_id()) {
                            groups.entry(format!("{group:?}")).or_default().push(id.0.to_string());
                        }
                    }
                    groups.into_iter().find(|(group, members)| !existing_groups.contains(group)
                        && media_ids.iter().all(|id| members.contains(id))).map(|(group, _)| group)
                });
                let state = if project_changed { "unavailable" } else if refused { "failed" } else if group.is_some() { "completed" }
                    else if operation.as_ref().is_some_and(|status| status["state"] == "queued") { "queued" } else { "running" };
                jobs.push(json!({
                    "job_id":operation_id, "kind":"sync", "state":state,
                    "phase":if state == "completed" { "multicam_group_ready" } else if state == "unavailable" { "project_changed" }
                        else if state == "failed" { "ui_refused" }
                        else if state == "queued" { "waiting_for_ui" } else { "aligning_angles" },
                    "progress":if state == "completed" { Some(1.0) } else { None },
                    "completed_units":if state == "completed" { 1 } else { 0 }, "total_units":1, "unit":"group",
                    "eta_secs":null, "error":if project_changed { Some("the open project changed; this sync outcome is no longer observable here".to_string()) } else { error }, "artifact":null,
                    "result":group.filter(|_| state == "completed").map(|group| json!({ "group_id":group })),
                }));
            }
        }
    }
    jobs.sort_by_key(is_terminal);
    with_timestamps(&mut jobs);
    jobs
}

pub(super) fn active(ctx: &ControlContext) -> Vec<Value> {
    snapshot(ctx).into_iter().filter(|job| !is_terminal(job)).take(20).collect()
}

pub(super) fn status(ctx: &ControlContext, job_id: Option<&str>, wait: bool, timeout_ms: Option<u64>) -> Result<Value, String> {
    let pick = |jobs: Vec<Value>| -> Result<Vec<Value>, String> {
        match job_id {
            Some(id) => {
                let job = jobs.into_iter().find(|job| job["job_id"] == id)
                    .ok_or_else(|| format!("no known job {id}; read klipp_status for active jobs"))?;
                Ok(vec![job])
            }
            None => Ok(jobs.into_iter().take(100).collect()),
        }
    };
    let started = Instant::now();
    let deadline = Duration::from_millis(timeout_ms.unwrap_or(10_000).min(10_000));
    let initial = pick(snapshot(ctx))?;
    if !wait || initial.is_empty() || initial.iter().all(is_terminal) {
        return Ok(json!({ "jobs": initial, "changed": false, "timed_out": false }));
    }
    loop {
        if started.elapsed() >= deadline {
            return Ok(json!({ "jobs": pick(snapshot(ctx))?, "changed": false, "timed_out": true }));
        }
        std::thread::sleep(Duration::from_millis(200).min(deadline.saturating_sub(started.elapsed())));
        let latest = pick(snapshot(ctx))?;
        if latest != initial {
            return Ok(json!({ "jobs": latest, "changed": true, "timed_out": false }));
        }
    }
}
