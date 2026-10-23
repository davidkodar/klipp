//! A common read view over Klipp's live render and analysis services.
//! The jobs remain owned by those services, so reconnecting clients can
//! read their latest known state by ID without relying on MCP session data.

use std::collections::{HashMap, HashSet};
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

/// Renders that have finished, as last read, by job id, with when they
/// were first seen finished (#721). The UI's render queue is the user's
/// to clear; a job removed from it is still answered from here for an
/// hour, so an agent can read the outcome it is waiting for.
static FINISHED_RENDERS: OnceLock<Mutex<HashMap<String, (u128, Value)>>> = OnceLock::new();
const FINISHED_RETAIN_MS: u128 = 60 * 60 * 1_000;
const FINISHED_KEEP: usize = 256;

fn finished_renders() -> &'static Mutex<HashMap<String, (u128, Value)>> {
    FINISHED_RENDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone)]
enum Submitted {
    /// `media_id` once the pool has been seen holding it (#1902): from
    /// then on the job is its item, wherever the file moves. A save brings
    /// downloaded music into the project's folder and repoints the item,
    /// and a relink or a folder move does the same; found by the path it
    /// was imported from, the job read "probing" for the rest of the
    /// session.
    Import { path: std::path::PathBuf, analyze: bool, project_identity: u64, media_id: Option<klipp_core::MediaId> },
    Sync { media_ids: Vec<String>, existing_groups: Vec<String>, project_identity: u64 },
    /// Proxies asked for (#2478): done as each file's proxy is ready.
    Proxies { media_ids: Vec<klipp_core::MediaId> },
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
        Submitted::Import { path, analyze, project_identity: project_identity(ctx), media_id: None });
}

/// The media id an import made, once it is known (#1902).
pub(super) fn note_imported(operation_id: &str, id: klipp_core::MediaId) {
    if let Some(Submitted::Import { media_id, .. }) = submitted().lock().unwrap_or_else(|e| e.into_inner()).get_mut(operation_id) {
        media_id.get_or_insert(id);
    }
}

/// The pool item an import made: the one it was seen as, else the one
/// at the path it was imported from, else, the file having been copied
/// elsewhere since (a save adopting downloaded music), the one with the
/// same content.
fn imported_item(pool: &klipp_media::MediaPool, known: Option<klipp_core::MediaId>, path: &std::path::Path) -> Option<klipp_core::MediaId> {
    if let Some(id) = known {
        return pool.get(&id).map(|item| item.id);
    }
    pool.find_by_path(path)
        .or_else(|| klipp_media::relink::fingerprint(path).and_then(|fp| pool.find_by_fingerprint(fp)))
        .map(|item| item.id)
}

pub(super) fn existing_multicam_groups(ctx: &ControlContext) -> Vec<String> {
    let Ok(sequence) = ctx.sequence.read() else { return Vec::new(); };
    sequence.tracks.iter().flat_map(|track| track.clips.iter())
        .filter_map(|clip| clip.multicam_group.map(|group| format!("{group:?}")))
        .collect()
}

pub(super) fn register_proxies(job_id: &str, media_ids: Vec<klipp_core::MediaId>) {
    submitted().lock().unwrap_or_else(|e| e.into_inner()).insert(job_id.to_owned(), Submitted::Proxies { media_ids });
}

/// A proxies job as it stands: each file ready (in the pool), being made
/// (its share done) or failed, as the UI last said.
fn proxies_job(ctx: &ControlContext, job_id: &str, media_ids: &[klipp_core::MediaId]) -> Value {
    let status = ctx.shared.proxies.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (mut done, mut share, mut failed, mut gone) = (0usize, 0.0f32, Vec::new(), 0usize);
    if let Ok(pool) = ctx.media_pool.read() {
        for id in media_ids {
            let Some(item) = pool.get(id) else {
                gone += 1;
                continue;
            };
            if item.proxy_ready {
                done += 1;
                share += 1.0;
            } else if let Some((_, why)) = status.failed.iter().find(|(f, _)| f == id) {
                failed.push(format!("{}: {why}", item.filename));
            } else if let Some((_, p)) = status.making.iter().find(|(m, _)| m == id) {
                share += p.clamp(0.0, 1.0);
            }
        }
    }
    let n = media_ids.len().max(1);
    let settled = done + failed.len() + gone >= media_ids.len();
    let state = if !settled { "running" } else if failed.is_empty() && gone == 0 { "completed" } else { "failed" };
    json!({
        "job_id": job_id, "kind": "proxies", "state": state,
        "phase": if settled { state } else { "transcoding" },
        "progress": share / n as f32,
        "completed_units": done, "total_units": media_ids.len(), "unit": "files",
        "eta_secs": null,
        "error": (!failed.is_empty() || gone > 0).then(|| { let mut e = failed.join("; "); if gone > 0 { e.push_str(&format!("{}{gone} file(s) left the pool", if e.is_empty() { "" } else { "; " })); } e }),
        "artifact": null, "result": (state == "completed").then(|| json!({ "ready": done })),
    })
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

fn prune_submitted(jobs: &[Value]) {
    const KEEP_TERMINAL: usize = 256;
    const RETAIN_MS: u128 = 10 * 60 * 1_000;
    let now = now_ms();
    let mut terminal: Vec<_> = jobs.iter()
        .filter(|job| matches!(job["kind"].as_str(), Some("import" | "sync" | "proxies")) && is_terminal(job))
        .filter_map(|job| Some((job["job_id"].as_str()?.to_owned(), job["finished_at_ms"].as_u64()? as u128)))
        .collect();
    terminal.sort_by_key(|(_, finished)| *finished);
    let overflow = terminal.len().saturating_sub(KEEP_TERMINAL);
    let stale: HashSet<String> = terminal.iter().enumerate()
        .filter(|(index, (_, finished))| *index < overflow || now.saturating_sub(*finished) > RETAIN_MS)
        .map(|(_, (id, _))| id.clone()).collect();
    if stale.is_empty() { return; }
    submitted().lock().unwrap_or_else(|e| e.into_inner()).retain(|id, _| !stale.contains(id));
    OBSERVED.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner())
        .retain(|id, _| !stale.contains(id));
}

/// Keep the finished renders in `jobs`, and add back the ones the UI's
/// queue no longer lists, marked `in_queue: false`.
fn remember_finished_renders(jobs: &mut Vec<Value>) {
    let now = now_ms();
    let mut kept = finished_renders().lock().unwrap_or_else(|e| e.into_inner());
    for job in jobs.iter().filter(|job| is_terminal(job)) {
        let id = job["job_id"].as_str().unwrap_or_default().to_owned();
        let first = kept.get(&id).map(|(at, _)| *at).unwrap_or(now);
        kept.insert(id, (first, job.clone()));
    }
    kept.retain(|_, (at, _)| now.saturating_sub(*at) <= FINISHED_RETAIN_MS);
    if kept.len() > FINISHED_KEEP {
        let mut by_age: Vec<(String, u128)> = kept.iter().map(|(id, (at, _))| (id.clone(), *at)).collect();
        by_age.sort_by_key(|(_, at)| *at);
        for (id, _) in by_age.into_iter().take(kept.len() - FINISHED_KEEP) {
            kept.remove(&id);
        }
    }
    let listed: HashSet<String> = jobs.iter().filter_map(|job| job["job_id"].as_str().map(str::to_owned)).collect();
    for (id, (_, job)) in kept.iter() {
        if !listed.contains(id) {
            let mut job = job.clone();
            job["in_queue"] = json!(false);
            jobs.push(job);
        }
    }
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
            "unit": "frames", "eta_secs": render.eta_secs, "projected_bytes": render.projected_bytes, "error": render.error,
            "artifact": if state == "completed" { Some(render.path.to_string_lossy().to_string()) } else { None },
            "loudness": render.loudness,
        })
    }).collect();
    remember_finished_renders(&mut jobs);
    jobs.extend(klipp_analysis::service::all().into_iter().map(|analysis| {
        let state = match analysis.state {
            klipp_analysis::service::State::Queued => "queued",
            klipp_analysis::service::State::Running => "running",
            klipp_analysis::service::State::Done => "completed",
            klipp_analysis::service::State::Failed => "failed",
            klipp_analysis::service::State::Cancelled => "cancelled",
        };
        // A subject matte pass (#239) is a job of its own beside the
        // file's analysis: its id the media's with its kind, its progress
        // the share of frames made, its artifact the matte sequence.
        let job_id = if analysis.kind == "analysis" { analysis.media_id.clone() } else { format!("{}:{}", analysis.media_id, analysis.kind) };
        let (unit, units) = if analysis.kind == "analysis" { ("analysers", Some(analysis.done.len() + analysis.failed.len())) } else { ("frames", None) };
        json!({
            "job_id": job_id, "kind": analysis.kind, "state": state,
            "phase": analysis.current.unwrap_or(state), "progress": analysis.progress,
            "completed_units": units, "total_units": null,
            "unit": unit, "eta_secs": null,
            "error": analysis.failed.first().map(|(stage, why)| format!("{stage}: {why}")),
            "artifact": analysis.artifact, "result": if state == "completed" { Some(analysis.filename) } else { None },
        })
    }));
    // Caption translations (#2213), run by the app as its menu runs them.
    jobs.extend(klipp_ml_candle::translation::jobs::all().into_iter().map(|t| {
        use klipp_ml_candle::translation::jobs::State;
        let state = match t.state {
            State::Running => "running",
            State::Done => "completed",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        };
        json!({
            "job_id": t.id, "kind": "translation", "state": state,
            "phase": t.phase, "progress": t.progress,
            "completed_units": t.done, "total_units": t.total,
            "unit": "sentences", "eta_secs": null,
            "error": t.error, "artifact": null, "result": t.result,
        })
    }));
    let submitted: Vec<_> = submitted().lock().unwrap_or_else(|e| e.into_inner())
        .iter().map(|(id, work)| (id.clone(), work.clone())).collect();
    for (operation_id, work) in submitted {
        let operation = crate::operations::status(&operation_id).ok();
        let refused = operation.as_ref().is_some_and(|status| status["state"] == "refused");
        let error = operation.as_ref().and_then(|status| status["reason"].as_str()).map(str::to_owned);
        match work {
            Submitted::Proxies { media_ids } => jobs.push(proxies_job(ctx, &operation_id, &media_ids)),
            Submitted::Import { path, analyze, project_identity: expected_project, media_id: known } => {
                let project_changed = expected_project != project_identity(ctx);
                let found = ctx.media_pool.read().ok().and_then(|pool| imported_item(&pool, known, &path));
                if let (None, Some(id)) = (known, found) {
                    note_imported(&operation_id, id);
                }
                let media_id = found.map(|id| id.0.to_string());
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
    prune_submitted(&jobs);
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
    let reply = |jobs: Vec<Value>, changed: bool, timed_out: bool| json!({
        "text": summary(&jobs), "jobs": jobs, "changed": changed, "timed_out": timed_out,
    });
    let initial = pick(snapshot(ctx))?;
    if !wait || initial.is_empty() || initial.iter().all(is_terminal) {
        return Ok(reply(initial, false, false));
    }
    let seen: Vec<Value> = initial.iter().map(progress_signature).collect();
    loop {
        if started.elapsed() >= deadline {
            return Ok(reply(pick(snapshot(ctx))?, false, true));
        }
        std::thread::sleep(Duration::from_millis(200).min(deadline.saturating_sub(started.elapsed())));
        let latest = pick(snapshot(ctx))?;
        // A render's frame count and time left move with every frame;
        // waking on those made `wait` return within 200 ms with nothing
        // a caller follows changed (#721). Wake on what a caller reports.
        if latest.iter().map(progress_signature).collect::<Vec<_>>() != seen {
            return Ok(reply(latest, true, false));
        }
    }
}

/// What a caller following a job reports: its state, phase, whole
/// percent, error and result, and, for jobs with no percentage, how many
/// of its units are done. Not the per-frame counters or the time left.
pub(super) fn progress_signature(job: &Value) -> Value {
    let percent = job["progress"].as_f64().map(|p| (p * 100.0).floor() as i64);
    let units = if percent.is_none() { job["completed_units"].clone() } else { Value::Null };
    json!([job["job_id"], job["state"], job["phase"], percent, units, job["error"], job["artifact"], job["result"]])
}

/// The jobs in words, one line each, the state first: what a caller
/// reads before the fields.
pub(super) fn summary(jobs: &[Value]) -> String {
    if jobs.is_empty() {
        return "No jobs.".into();
    }
    jobs.iter().map(|job| {
        let kind = job["kind"].as_str().unwrap_or("job");
        let state = job["state"].as_str().unwrap_or("unknown");
        let mut line = format!("{kind} {state}");
        if let Some(p) = job["progress"].as_f64().filter(|_| state != "completed") {
            line.push_str(&format!(" {:.0}%", (p * 100.0).floor()));
        }
        if let Some(phase) = job["phase"].as_str().filter(|phase| *phase != state && !phase.is_empty() && !is_terminal(job)) {
            line.push_str(&format!(": {phase}"));
        }
        if let Some(artifact) = job["artifact"].as_str() {
            line.push_str(&format!(" — {artifact}"));
        }
        if let Some(loudness) = job["loudness"].as_str() {
            line.push_str(&format!(", {loudness}"));
        }
        if let Some(eta) = job["eta_secs"].as_u64().filter(|_| state == "running") {
            line.push_str(&format!(", about {}:{:02} left", eta / 60, eta % 60));
        }
        if let Some(bytes) = job["projected_bytes"].as_u64().filter(|_| state == "running") {
            line.push_str(&format!(", about {:.0} MB when done", bytes as f64 / 1e6));
        }
        if let Some(error) = job["error"].as_str() {
            line.push_str(&format!("; {error}"));
        }
        if job["in_queue"] == json!(false) {
            line.push_str(" (removed from the render queue)");
        }
        format!("{line}  [{}]", job["job_id"].as_str().unwrap_or("?"))
    }).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(id: &str, state: &str, progress: f64, frames: u64, eta: u64) -> Value {
        json!({ "job_id": id, "kind": "render", "state": state, "phase": "Encoding", "progress": progress,
                "completed_units": frames, "total_units": 3484, "eta_secs": eta, "error": null,
                "artifact": if state == "completed" { Some("/tmp/out.mp4") } else { None }, "loudness": null })
    }

    /// #721: a render's frames and time left move every frame; a waiter
    /// wakes on its state, phase or whole percent, not on those.
    #[test]
    fn a_waiter_wakes_on_progress_not_on_every_frame() {
        let a = progress_signature(&render("r", "running", 0.101, 350, 64));
        assert_eq!(a, progress_signature(&render("r", "running", 0.104, 362, 61)), "same whole percent");
        assert_ne!(a, progress_signature(&render("r", "running", 0.11, 383, 60)), "a percent further");
        assert_ne!(a, progress_signature(&render("r", "completed", 1.0, 3484, 0)), "ended");
    }

    /// #721: a finished render the person clears from the render queue is
    /// still answered, marked as gone from the queue; a running one is not
    /// kept.
    #[test]
    fn a_finished_render_outlives_the_queue() {
        let mut listed = vec![render("done-721", "completed", 1.0, 3484, 0), render("live-721", "running", 0.5, 10, 9)];
        remember_finished_renders(&mut listed);
        assert_eq!(listed.len(), 2);
        let mut cleared: Vec<Value> = Vec::new();
        remember_finished_renders(&mut cleared);
        let kept: Vec<&Value> = cleared.iter().filter(|j| j["job_id"] == "done-721").collect();
        assert_eq!(kept.len(), 1, "{cleared:?}");
        assert_eq!(kept[0]["in_queue"], false);
        assert!(!cleared.iter().any(|j| j["job_id"] == "live-721"));
        assert!(summary(&cleared).contains("render completed — /tmp/out.mp4"), "{}", summary(&cleared));
        assert!(summary(&cleared).contains("removed from the render queue"));
    }

    /// #1902: an import is its item once seen, wherever the file then
    /// goes. A save copies downloaded music into the project and repoints
    /// the item; found by the path it was imported from, the job read
    /// "probing" from then on. Repointed before it was ever seen, the copy
    /// is found by its content.
    #[test]
    fn an_import_follows_its_item_when_the_file_is_adopted() {
        let dir = std::env::temp_dir().join(format!("klipp-import-job-{}", uuid::Uuid::new_v4()));
        let download = dir.join("swansong.mp3");
        let adopted = dir.join("project").join("media").join("swansong.mp3");
        std::fs::create_dir_all(adopted.parent().unwrap()).unwrap();
        std::fs::write(&download, b"ID3 a downloaded track, its first bytes").unwrap();
        std::fs::copy(&download, &adopted).unwrap();
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test(download.clone(), None);
        item.fingerprint = klipp_media::relink::fingerprint(&download);
        let id = pool.add(item);

        assert_eq!(imported_item(&pool, None, &download), Some(id), "in the pool at its path");
        // The save adopts it, as `adopt_made_media` does.
        pool.get_mut(&id).unwrap().path = adopted.clone();
        assert!(pool.find_by_path(&download).is_none(), "the path alone no longer finds it: the report");
        assert_eq!(imported_item(&pool, Some(id), &download), Some(id), "seen once, followed");
        assert_eq!(imported_item(&pool, None, &download), Some(id), "never seen: found by its content");
        // Removed from the pool, the job no longer claims an item.
        pool.remove(&id);
        assert_eq!(imported_item(&pool, Some(id), &download), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1902: the id an import made is kept on its job once noted, and a
    /// later note doesn't replace it.
    #[test]
    fn an_imports_id_is_noted_once() {
        let (first, second) = (klipp_core::MediaId::new(), klipp_core::MediaId::new());
        submitted().lock().unwrap().insert("op-1902".into(), Submitted::Import { path: "/x.mp3".into(), analyze: true, project_identity: 0, media_id: None });
        note_imported("op-1902", first);
        note_imported("op-1902", second);
        let kept = match submitted().lock().unwrap().remove("op-1902") {
            Some(Submitted::Import { media_id, .. }) => media_id,
            _ => None,
        };
        assert_eq!(kept, Some(first));
    }

    /// The state comes first, in words, then where it is and what it made.
    #[test]
    fn the_summary_says_the_state_first() {
        let mut job = render("r1", "running", 0.457, 1600, 75);
        assert_eq!(summary(std::slice::from_ref(&job)), "render running 45%: Encoding, about 1:15 left  [r1]");
        job = render("r1", "completed", 1.0, 3484, 0);
        job["loudness"] = json!("-14.0 LUFS");
        assert_eq!(summary(&[job]), "render completed — /tmp/out.mp4, -14.0 LUFS  [r1]");
    }
}
