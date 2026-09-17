//! Analysis in the background: ask for an item, read where it stands.
//! At most two files at a time, since every analyser here is an ffmpeg
//! decode; a request for an item already done or running costs nothing.

use crate::{analysers, AnalysisCtx};
use klipp_core::MediaId;
use klipp_media::MediaItem;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const MAX_RUNNING: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Status {
    pub media_id: String,
    pub filename: String,
    pub state: State,
    /// Analysers that produced a result this run or were already stored.
    pub done: Vec<&'static str>,
    /// Analysers that failed, with why.
    pub failed: Vec<(String, String)>,
}

struct Service {
    statuses: HashMap<MediaId, Status>,
    queue: Vec<MediaItem>,
    running: usize,
    ctx: AnalysisCtx,
    /// Items whose analysis finished since the host last asked, so it
    /// can read the results once rather than poll every item.
    completed: Vec<MediaId>,
}

fn service() -> &'static Mutex<Service> {
    static S: OnceLock<Mutex<Service>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Service { statuses: HashMap::new(), queue: Vec::new(), running: 0, ctx: AnalysisCtx::default(), completed: Vec::new() }))
}

/// Ask for `item` to be analysed. Returns at once with its state;
/// `Done` if every analyser that wants it has a stored result already.
pub fn request(item: &MediaItem) -> Status {
    let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(st) = s.statuses.get(&item.id) {
        if matches!(st.state, State::Queued | State::Running) {
            return st.clone();
        }
    }
    // Everything stored already: done without a thread.
    if let Some(key) = crate::store::key_for(&item.path) {
        let wanted: Vec<_> = analysers().into_iter().filter(|a| a.wants(item)).collect();
        if !wanted.is_empty() && wanted.iter().all(|a| crate::store::exists(&key, a.id(), a.schema_version())) {
            let st = Status {
                media_id: item.id.0.to_string(),
                filename: item.filename.clone(),
                state: State::Done,
                done: wanted.iter().map(|a| a.id()).collect(),
                failed: Vec::new(),
            };
            s.statuses.insert(item.id, st.clone());
            s.completed.push(item.id);
            return st;
        }
    }
    let st = Status { media_id: item.id.0.to_string(), filename: item.filename.clone(), state: State::Queued, done: Vec::new(), failed: Vec::new() };
    s.statuses.insert(item.id, st.clone());
    s.queue.push(item.clone());
    pump(&mut s);
    st
}

/// Start queued work while there is room.
fn pump(s: &mut Service) {
    while s.running < MAX_RUNNING && !s.queue.is_empty() {
        let item = s.queue.remove(0);
        s.running += 1;
        if let Some(st) = s.statuses.get_mut(&item.id) {
            st.state = State::Running;
        }
        let ctx = s.ctx.clone();
        std::thread::Builder::new()
            .name(format!("klipp-analysis:{}", item.filename))
            .spawn(move || {
                let (done, failed) = run_all(&item, &ctx);
                let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
                s.running = s.running.saturating_sub(1);
                let any_done = !done.is_empty();
                if let Some(st) = s.statuses.get_mut(&item.id) {
                    st.state = if failed.is_empty() { State::Done } else { State::Failed };
                    st.done = done;
                    st.failed = failed;
                }
                if any_done {
                    s.completed.push(item.id);
                }
                pump(&mut s);
            })
            .ok();
    }
}

/// Every analyser that wants `item` and has no stored result, run in
/// order; each result saved as it is made.
fn run_all(item: &MediaItem, ctx: &AnalysisCtx) -> (Vec<&'static str>, Vec<(String, String)>) {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    let Some(key) = crate::store::key_for(&item.path) else {
        failed.push(("store".to_string(), format!("{} is not readable", item.path.display())));
        return (done, failed);
    };
    for a in analysers() {
        if !a.wants(item) {
            continue;
        }
        if crate::store::exists(&key, a.id(), a.schema_version()) {
            done.push(a.id());
            continue;
        }
        let t = std::time::Instant::now();
        match a.run(item, ctx) {
            Ok(v) => match crate::store::save(&key, a.id(), a.schema_version(), &v) {
                Ok(()) => {
                    tracing::info!("analysis {} of {} in {:.1} s", a.id(), item.filename, t.elapsed().as_secs_f64());
                    done.push(a.id());
                }
                Err(e) => failed.push((a.id().to_string(), e)),
            },
            Err(e) => {
                tracing::warn!("analysis {} of {} failed: {e}", a.id(), item.filename);
                failed.push((a.id().to_string(), e));
            }
        }
    }
    (done, failed)
}

/// The items whose analysis finished since the last call.
pub fn take_completed() -> Vec<MediaId> {
    std::mem::take(&mut service().lock().unwrap_or_else(|e| e.into_inner()).completed)
}

pub fn status(media_id: MediaId) -> Option<Status> {
    service().lock().unwrap_or_else(|e| e.into_inner()).statuses.get(&media_id).cloned()
}

pub fn all() -> Vec<Status> {
    let s = service().lock().unwrap_or_else(|e| e.into_inner());
    let mut v: Vec<Status> = s.statuses.values().cloned().collect();
    v.sort_by(|a, b| a.filename.cmp(&b.filename));
    v
}

/// Run every analyser for `item` on this thread and return the report;
/// for tests and tools, not the app.
pub fn run_now(item: &MediaItem, ctx: &AnalysisCtx) -> Result<crate::report::MediaAnalysis, String> {
    let (_, failed) = run_all(item, ctx);
    if let Some((id, why)) = failed.first() {
        return Err(format!("{id}: {why}"));
    }
    Ok(crate::report::MediaAnalysis::load(&item.path))
}
