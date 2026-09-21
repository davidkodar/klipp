//! Analysis in the background: ask for an item, read where it stands.
//! At most two files at a time and one of them with a picture, since
//! every video analyser decodes on the one hardware decoder the viewer
//! also needs; a request for an item already done or running costs
//! nothing. The threads run at low priority and every step waits while
//! the editor is busy (`hold`), so the person editing never feels it.

use crate::{analysers, AnalysisCtx};
use klipp_core::MediaId;
use klipp_media::MediaItem;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const MAX_RUNNING: usize = 2;
const MAX_VIDEO_RUNNING: usize = 1;

/// A file with a picture to decode, as against sound alone or a still.
fn has_video(item: &MediaItem) -> bool {
    !item.video_codec.is_empty() && !item.is_still
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
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
    /// The analyser at work now, and since when: a transcript of a long
    /// file is minutes, and "running" with nothing else said read as
    /// stuck.
    #[serde(default)]
    pub current: Option<&'static str>,
    #[serde(default)]
    pub current_for_secs: Option<u64>,
    #[serde(skip)]
    current_since: Option<std::time::Instant>,
}

struct Service {
    statuses: HashMap<MediaId, Status>,
    queue: Vec<MediaItem>,
    running: usize,
    running_video: usize,
    ctx: AnalysisCtx,
    /// Items whose analysis finished since the host last asked, so it
    /// can read the results once rather than poll every item.
    completed: Vec<MediaId>,
    /// Items an on-demand "Transcribe clip" queued: run the speech
    /// analyser for these even when automatic transcription is off.
    force_transcript: std::collections::HashSet<MediaId>,
}

fn service() -> &'static Mutex<Service> {
    static S: OnceLock<Mutex<Service>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Service { statuses: HashMap::new(), queue: Vec::new(), running: 0, running_video: 0, ctx: AnalysisCtx::default(), completed: Vec::new(), force_transcript: std::collections::HashSet::new() }))
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
        if !wanted.is_empty() && wanted.iter().all(|a| crate::has_fresh(a.as_ref(), &key)) {
            let st = Status {
                media_id: item.id.0.to_string(),
                filename: item.filename.clone(),
                state: State::Done,
                done: wanted.iter().map(|a| a.id()).collect(),
                failed: Vec::new(),
                current: None,
                current_for_secs: None,
                current_since: None,
            };
            s.statuses.insert(item.id, st.clone());
            s.completed.push(item.id);
            return st;
        }
    }
    let st = Status { media_id: item.id.0.to_string(), filename: item.filename.clone(), state: State::Queued, done: Vec::new(), failed: Vec::new(), current: None, current_for_secs: None, current_since: None };
    s.statuses.insert(item.id, st.clone());
    s.queue.push(item.clone());
    pump(&mut s);
    st
}

/// Transcribe one clip now — the on-demand "Transcribe clip" action. Runs
/// the speech analyser for this item even when automatic transcription is
/// switched off, without touching the other analysers or the global
/// preference. The item appears in `take_completed()` when it is done. A
/// no-op if a run for the item is already in flight, or if the clip has no
/// sound to transcribe.
pub fn request_transcription(item: &MediaItem) -> Status {
    let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(st) = s.statuses.get(&item.id) {
        if matches!(st.state, State::Queued | State::Running) {
            return st.clone();
        }
    }
    s.force_transcript.insert(item.id);
    let st = Status { media_id: item.id.0.to_string(), filename: item.filename.clone(), state: State::Queued, done: Vec::new(), failed: Vec::new(), current: None, current_for_secs: None, current_since: None };
    s.statuses.insert(item.id, st.clone());
    s.queue.push(item.clone());
    pump(&mut s);
    st
}

/// Start queued work while there is room.
fn pump(s: &mut Service) {
    while s.running < MAX_RUNNING {
        let video_full = s.running_video >= MAX_VIDEO_RUNNING;
        let Some(pos) = s.queue.iter().position(|i| !(video_full && has_video(i))) else { break };
        let item = s.queue.remove(pos);
        let video = has_video(&item);
        s.running += 1;
        if video {
            s.running_video += 1;
        }
        if let Some(st) = s.statuses.get_mut(&item.id) {
            st.state = State::Running;
        }
        let ctx = s.ctx.clone();
        let force_transcript = s.force_transcript.remove(&item.id);
        std::thread::Builder::new()
            .name(format!("klipp-analysis:{}", item.filename))
            .spawn(move || {
                // Below the editor's threads, and the decoder and model
                // threads started from here inherit it.
                klipp_jobs::lower_current_thread_priority(10);
                let (done, failed) = run_all(&item, &ctx, force_transcript);
                let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
                s.running = s.running.saturating_sub(1);
                if video {
                    s.running_video = s.running_video.saturating_sub(1);
                }
                let any_done = !done.is_empty();
                if let Some(st) = s.statuses.get_mut(&item.id) {
                    st.state = if failed.is_empty() { State::Done } else { State::Failed };
                    st.done = done;
                    st.failed = failed;
                    st.current = None;
                    st.current_since = None;
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
fn run_all(item: &MediaItem, ctx: &AnalysisCtx, force_transcript: bool) -> (Vec<&'static str>, Vec<(String, String)>) {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    let Some(key) = crate::store::key_for(&item.path) else {
        failed.push(("store".to_string(), format!("{} is not readable", item.path.display())));
        return (done, failed);
    };
    // The on-demand transcribe forces the speech analyser in even when
    // automatic transcription is off; otherwise the enabled set is used.
    let mut list = analysers();
    #[cfg(feature = "transcript")]
    if force_transcript && !list.iter().any(|a| a.id() == crate::transcript::ID) {
        list.push(Box::new(crate::transcript::TranscriptAnalyser));
    }
    let _ = force_transcript;
    for a in list {
        if !a.wants(item) {
            continue;
        }
        if crate::has_fresh(a.as_ref(), &key) {
            done.push(a.id());
            continue;
        }
        // Said as it goes: what is done so far, and what is running.
        if let Ok(mut s) = service().lock() {
            if let Some(st) = s.statuses.get_mut(&item.id) {
                st.done = done.clone();
                st.current = Some(a.id());
                st.current_since = Some(std::time::Instant::now());
            }
        }
        crate::hold::wait();
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

/// A status as handed out: how long the running analyser has been at it.
fn timed(mut st: Status) -> Status {
    st.current_for_secs = st.current_since.map(|t| t.elapsed().as_secs());
    st
}

pub fn status(media_id: MediaId) -> Option<Status> {
    service().lock().unwrap_or_else(|e| e.into_inner()).statuses.get(&media_id).cloned().map(timed)
}

pub fn all() -> Vec<Status> {
    let s = service().lock().unwrap_or_else(|e| e.into_inner());
    let mut v: Vec<Status> = s.statuses.values().cloned().map(timed).collect();
    v.sort_by(|a, b| a.filename.cmp(&b.filename));
    v
}

/// Run every analyser for `item` on this thread and return the report;
/// for tests and tools, not the app.
pub fn run_now(item: &MediaItem, ctx: &AnalysisCtx) -> Result<crate::report::MediaAnalysis, String> {
    let (_, failed) = run_all(item, ctx, false);
    if let Some((id, why)) = failed.first() {
        return Err(format!("{id}: {why}"));
    }
    Ok(crate::report::MediaAnalysis::load(&item.path))
}

/// Prioritize one queued item and optionally cancel every other pending
/// item. Running workers finish normally; the decoder concurrency limits
/// still apply. Invalid IDs are refused before the queue changes.
pub fn manage_queue(priority: Option<MediaId>, cancel_pending: bool) -> Result<Vec<String>, String> {
    let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
    let cancelled = edit_queue(&mut s, priority, cancel_pending)?;
    pump(&mut s);
    Ok(cancelled)
}

fn edit_queue(s: &mut Service, priority: Option<MediaId>, cancel_pending: bool) -> Result<Vec<String>, String> {
    if let Some(id) = priority {
        let st = s.statuses.get(&id).ok_or_else(|| format!("no analysis for {}; call analyze first", id.0))?;
        if !matches!(st.state, State::Queued | State::Running) {
            return Err(format!("analysis {} is already terminal; call analyze to request it again", id.0));
        }
    }
    let mut cancelled = Vec::new();
    if cancel_pending {
        s.queue.retain(|item| {
            if Some(item.id) == priority { return true; }
            if let Some(st) = s.statuses.get_mut(&item.id) {
                st.state = State::Cancelled;
            }
            s.force_transcript.remove(&item.id);
            cancelled.push(item.id.0.to_string());
            false
        });
    }
    if let Some(pos) = s.queue.iter().position(|item| Some(item.id) == priority) {
        let item = s.queue.remove(pos);
        s.queue.insert(0, item);
    }
    Ok(cancelled)
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    #[test]
    fn prioritize_and_cancel_preserve_running_work_and_reject_invalid_ids() {
        let mut s = Service { statuses: HashMap::new(), queue: Vec::new(), running: 1, running_video: 1, ctx: AnalysisCtx::default(), completed: Vec::new(), force_transcript: std::collections::HashSet::new() };
        for i in 0..22 {
            let item = MediaItem::new_synthetic_title(MediaId::new(), format!("take{i}"), klipp_core::Timecode::from_frames(24, 24), klipp_core::Fps::new(24, 1));
            s.statuses.insert(item.id, Status { media_id: item.id.0.to_string(), filename: item.filename.clone(), state: State::Queued, done: Vec::new(), failed: Vec::new(), current: None, current_for_secs: None, current_since: None });
            s.queue.push(item);
        }
        let running = s.queue.remove(0).id;
        s.statuses.get_mut(&running).unwrap().state = State::Running;
        let wanted = s.queue.last().unwrap().id;
        assert!(edit_queue(&mut s, Some(MediaId::new()), true).is_err());
        assert_eq!(s.queue.len(), 21);
        assert!(edit_queue(&mut s, Some(wanted), false).unwrap().is_empty());
        assert_eq!(s.queue[0].id, wanted);
        let cancelled = edit_queue(&mut s, Some(wanted), true).unwrap();
        assert_eq!(cancelled.len(), 20);
        assert_eq!(s.queue.len(), 1);
        assert_eq!(s.statuses[&running].state, State::Running);
        assert_eq!(s.statuses[&wanted].state, State::Queued);
        assert_eq!(s.statuses.values().filter(|s| s.state == State::Cancelled).count(), 20);
        assert_eq!(edit_queue(&mut s, None, true).unwrap().len(), 1);
        assert!(s.queue.is_empty());
    }
}
