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
    /// What the job is: "analysis" (every analyser for the file) or
    /// "subject_matte" (the matte pass over a range, #239).
    pub kind: &'static str,
    /// How far through it is, 0..1, when it can say.
    #[serde(default)]
    pub progress: Option<f32>,
    /// What it made: the matte sequence's folder.
    #[serde(default)]
    pub artifact: Option<String>,
    /// What each model the analysis opened runs on, a line each: "on
    /// CPU" says why, so a GPU left unused shows (#2250).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_providers: Vec<String>,
}

impl Status {
    fn new(item: &MediaItem, state: State, kind: &'static str) -> Self {
        Status { media_id: item.id.0.to_string(), filename: item.filename.clone(), state, done: Vec::new(), failed: Vec::new(), current: None, current_for_secs: None, current_since: None, kind, progress: None, artifact: None, execution_providers: Vec::new() }
    }
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
    /// The subject matte passes (#239), by media: their status and the
    /// flag that stops them.
    mattes: HashMap<MediaId, (Status, std::sync::Arc<std::sync::atomic::AtomicBool>)>,
}

fn service() -> &'static Mutex<Service> {
    static S: OnceLock<Mutex<Service>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Service { statuses: HashMap::new(), queue: Vec::new(), running: 0, running_video: 0, ctx: AnalysisCtx::default(), completed: Vec::new(), force_transcript: std::collections::HashSet::new(), mattes: HashMap::new() }))
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
            let st = Status { done: wanted.iter().map(|a| a.id()).collect(), ..Status::new(item, State::Done, "analysis") };
            s.statuses.insert(item.id, st.clone());
            s.completed.push(item.id);
            return st;
        }
    }
    let st = Status::new(item, State::Queued, "analysis");
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
    let st = Status::new(item, State::Queued, "analysis");
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
                // Nothing left to analyse: give the models back. A
                // loaded transcription model is half a gigabyte and a
                // share of the GPU, and the editor wants both for
                // playing video.
                let idle = s.running == 0 && s.queue.is_empty();
                drop(s);
                #[cfg(feature = "transcript")]
                if idle {
                    crate::transcript::release_models();
                }
                #[cfg(not(feature = "transcript"))]
                let _ = idle;
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
        // A file asked to be transcribed again (#531) is, however fresh.
        #[cfg(feature = "transcript")]
        let redo = a.id() == crate::transcript::ID && crate::transcript::wants_redo(&item.path);
        #[cfg(not(feature = "transcript"))]
        let redo = false;
        if !redo && crate::has_fresh(a.as_ref(), &key) {
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
        let run = crate::worker::run_analyser(a.as_ref(), item, ctx);
        let used = crate::providers::lines();
        if !used.is_empty() {
            if let Ok(mut s) = service().lock() {
                if let Some(st) = s.statuses.get_mut(&item.id) {
                    st.execution_providers = used;
                }
            }
        }
        match run {
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
    let mut v: Vec<Status> = s.statuses.values().chain(s.mattes.values().map(|m| &m.0)).cloned().map(timed).collect();
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

/// Cancel only one queued analysis. A running analyser owns decoder state
/// and cannot be interrupted safely; tell the caller instead of claiming it
/// stopped. No timeline edit is involved.
pub fn cancel_queued(media_id: MediaId) -> Result<(), String> {
    let mut service = service().lock().unwrap_or_else(|e| e.into_inner());
    cancel_queued_in(&mut service, media_id)
}

fn cancel_queued_in(service: &mut Service, media_id: MediaId) -> Result<(), String> {
    let state = service.statuses.get(&media_id).map(|status| &status.state)
        .ok_or_else(|| format!("no analysis for {}", media_id.0))?;
    if *state != State::Queued {
        return Err(format!("analysis {} is {state:?}; only queued analysis can be cancelled", media_id.0));
    }
    let Some(position) = service.queue.iter().position(|item| item.id == media_id) else {
        return Err(format!("analysis {} left the queue; read its current status", media_id.0));
    };
    service.queue.remove(position);
    service.force_transcript.remove(&media_id);
    if let Some(status) = service.statuses.get_mut(&media_id) { status.state = State::Cancelled; }
    Ok(())
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
    fn cancel_one_queued_analysis_without_touching_other_work() {
        let mut service = Service { statuses: HashMap::new(), queue: Vec::new(), running: 1,
            running_video: 0, ctx: AnalysisCtx::default(), completed: Vec::new(),
            force_transcript: std::collections::HashSet::new(), mattes: HashMap::new() };
        let first = MediaItem::new_synthetic_title(MediaId::new(), "first".into(),
            klipp_core::Timecode::from_frames(24, 24), klipp_core::Fps::new(24, 1));
        let second = MediaItem::new_synthetic_title(MediaId::new(), "second".into(),
            klipp_core::Timecode::from_frames(24, 24), klipp_core::Fps::new(24, 1));
        for item in [&first, &second] {
            service.statuses.insert(item.id, Status::new(item, State::Queued, "analysis"));
            service.queue.push(item.clone());
        }
        assert!(cancel_queued_in(&mut service, first.id).is_ok());
        assert_eq!(service.statuses[&first.id].state, State::Cancelled);
        assert_eq!(service.queue.len(), 1);
        assert_eq!(service.queue[0].id, second.id);
        service.statuses.get_mut(&second.id).unwrap().state = State::Running;
        assert!(cancel_queued_in(&mut service, second.id).unwrap_err().contains("only queued"));
        assert_eq!(service.statuses[&second.id].state, State::Running);
    }

    #[test]
    fn prioritize_and_cancel_preserve_running_work_and_reject_invalid_ids() {
        let mut s = Service { statuses: HashMap::new(), queue: Vec::new(), running: 1, running_video: 1, ctx: AnalysisCtx::default(), completed: Vec::new(), force_transcript: std::collections::HashSet::new(), mattes: HashMap::new() };
        for i in 0..22 {
            let item = MediaItem::new_synthetic_title(MediaId::new(), format!("take{i}"), klipp_core::Timecode::from_frames(24, 24), klipp_core::Fps::new(24, 1));
            s.statuses.insert(item.id, Status::new(&item, State::Queued, "analysis"));
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

/// Make the subject mattes (#239) of `item` from `from_secs` to `to_secs`
/// with `model` in the background: a job of its own, reported with the
/// analyses (its progress the share of frames made). The model must be
/// downloaded already; the pass never fetches it. Returns at once with
/// its state; one already running for the media is left to run.
#[cfg(feature = "subject_matte")]
pub fn request_subject_matte(item: &MediaItem, model: crate::subject_matte::MatteModel, label: Option<String>, from_secs: f64, to_secs: f64) -> Status {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((st, _)) = s.mattes.get(&item.id) {
        if matches!(st.state, State::Queued | State::Running) {
            return st.clone();
        }
    }
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let st = Status { current: Some(crate::subject_matte::ID), current_since: Some(std::time::Instant::now()), progress: Some(0.0), ..Status::new(item, State::Running, crate::subject_matte::ID) };
    s.mattes.insert(item.id, (st.clone(), cancel.clone()));
    drop(s);
    let id = item.id;
    let item = item.clone();
    let spawned = std::thread::Builder::new().name(format!("klipp-matte:{}", item.filename)).spawn(move || {
        klipp_jobs::lower_current_thread_priority(10);
        let mut progress = |done: usize, of: usize| {
            if let Ok(mut s) = service().lock() {
                if let Some((st, _)) = s.mattes.get_mut(&item.id) {
                    st.progress = Some(done as f32 / of.max(1) as f32);
                }
            }
            !cancel.load(Ordering::Relaxed)
        };
        let result = crate::subject_matte::run(&item, model, label.as_deref(), from_secs, to_secs, &mut progress);
        let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((st, _)) = s.mattes.get_mut(&item.id) {
            st.current = None;
            st.current_since = None;
            match result {
                Ok((dir, made)) => {
                    tracing::info!("subject matte of {}: {} frames made, the subject lost in {}", item.filename, made.frames.len(), made.lost);
                    st.artifact = Some(dir.display().to_string());
                    st.done = vec![crate::subject_matte::ID];
                    if made.cancelled {
                        st.state = State::Cancelled;
                    } else {
                        st.state = State::Done;
                        st.progress = Some(1.0);
                    }
                }
                Err(why) => {
                    tracing::warn!("subject matte of {} failed: {why}", item.filename);
                    st.state = State::Failed;
                    st.failed = vec![(crate::subject_matte::ID.to_string(), why)];
                }
            }
        }
    });
    if let Err(e) = spawned {
        let mut s = service().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((st, _)) = s.mattes.get_mut(&id) {
            st.state = State::Failed;
            st.failed = vec![(crate::subject_matte::ID.to_string(), e.to_string())];
        }
    }
    st
}

/// Stop the subject matte pass (#239) of a media: it finishes the frame
/// it is on, keeps every frame finished, and stops. Refused when none is
/// running.
pub fn cancel_subject_matte(media_id: MediaId) -> Result<(), String> {
    let s = service().lock().unwrap_or_else(|e| e.into_inner());
    match s.mattes.get(&media_id) {
        Some((st, cancel)) if matches!(st.state, State::Queued | State::Running) => {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        Some((st, _)) => Err(format!("the subject matte of {} is {:?}", st.filename, st.state)),
        None => Err(format!("no subject matte pass for {}", media_id.0)),
    }
}

/// The subject matte pass's status for a media (#239).
pub fn subject_matte_status(media_id: MediaId) -> Option<Status> {
    service().lock().unwrap_or_else(|e| e.into_inner()).mattes.get(&media_id).map(|m| timed(m.0.clone()))
}

#[cfg(all(test, feature = "subject_matte"))]
mod subject_matte_job_tests {
    use super::*;

    /// #239: a subject matte pass is a job of its own in the analyses'
    /// report, said running at once with its progress; one that can't run
    /// (here, a file that isn't there) ends failed with why, and a cancel
    /// of a pass not running is refused, not claimed.
    #[test]
    fn a_matte_pass_is_reported_with_the_analyses() {
        let mut item = MediaItem::synthetic_for_test(std::path::PathBuf::from("/nowhere/take.mov"), None);
        item.fps = klipp_core::Fps::new(25, 1);
        let st = request_subject_matte(&item, crate::subject_matte::MatteModel::Foreground, None, 0.0, 1.0);
        assert_eq!((st.kind, st.progress), (crate::subject_matte::ID, Some(0.0)));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let end = loop {
            let now = subject_matte_status(item.id).expect("reported");
            if !matches!(now.state, State::Queued | State::Running) || std::time::Instant::now() > deadline {
                break now;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(end.state, State::Failed, "{end:?}");
        assert!(!end.failed.is_empty() && end.failed[0].0 == crate::subject_matte::ID, "{end:?}");
        assert!(all().iter().any(|s| s.kind == crate::subject_matte::ID && s.media_id == item.id.0.to_string()), "listed with the analyses");
        assert!(cancel_subject_matte(item.id).unwrap_err().contains("Failed"));
        assert!(cancel_subject_matte(MediaId::new()).is_err());
    }
}
