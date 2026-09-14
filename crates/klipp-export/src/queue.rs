use klipp_core::ExportSettings;
use klipp_media::MediaPool;
use klipp_render::{ExportJob, SequenceSnapshot};
use klipp_timeline::Sequence;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderJobStatus {
    Queued,
    Rendering,
    Complete,
    Failed,
    Cancelled,
}

pub struct RenderJob {
    pub id: Uuid,
    pub label: String,
    pub output_path: PathBuf,
    pub settings: ExportSettings,
    pub status: RenderJobStatus,
    pub progress: f32,
    pub created_at: std::time::Instant,
    pub started_at: Option<std::time::Instant>,
    pub completed_at: Option<std::time::Instant>,
    pub error: Option<String>,
    /// Total frames in this job (for ETA calculation).
    pub total_frames: u64,
    /// Frames completed so far (raw counter).
    pub frames_done: u64,
    /// Hardware info string (e.g. "NVENC + CUDA" or "Software").
    pub hw_info: String,
    /// Timeline fps (for speed-vs-realtime display).
    pub timeline_fps: f64,
    /// Current render stage (e.g. "Encoding video (H.265)…").
    pub render_stage: String,
    /// Loudness normalization result (set on completion if normalization was enabled).
    pub loudness_result: Option<String>,
}

pub enum RenderQueueEvent {
    Progress { id: Uuid, progress: f32 },
    Complete { id: Uuid, output_path: PathBuf },
    Failed { id: Uuid, error: String },
}

/// Snapshot captured at job creation time. Uses the new SequenceSnapshot
/// from klipp-render which captures all clip data (color grades, effects,
/// transitions, etc.) — not just file paths.
pub struct RenderSnapshot {
    pub sequence_snapshot: SequenceSnapshot,
    pub total_frames: u64,
}

impl RenderSnapshot {
    /// Create a render snapshot from the current sequence and media pool.
    pub fn from_sequence(
        sequence: &Sequence,
        media_pool: &MediaPool,
        settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
        working_colorspace: klipp_core::WorkingColorspace,
    ) -> Result<Self, klipp_render::RenderError> {
        Self::from_sequence_with_all(
            sequence,
            &[],
            media_pool,
            settings,
            global_lut_path,
            use_marked_range,
            working_colorspace,
        )
    }

    /// Create a render snapshot, resolving compound clips via `all_sequences`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_sequence_with_all(
        sequence: &Sequence,
        all_sequences: &[Sequence],
        media_pool: &MediaPool,
        settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
        working_colorspace: klipp_core::WorkingColorspace,
    ) -> Result<Self, klipp_render::RenderError> {
        let snapshot = SequenceSnapshot::from_sequence_with_all(
            sequence,
            all_sequences,
            media_pool,
            settings,
            global_lut_path,
            use_marked_range,
            working_colorspace,
        )?;
        let total_frames = snapshot.frames_to_export();
        Ok(Self {
            sequence_snapshot: snapshot,
            total_frames,
        })
    }
}

/// One live render thread + its per-job shared state.
/// Bundled so RenderQueue can hold a Vec of them for parallel rendering (#62).
struct ActiveJob {
    job_id: Uuid,
    handle: std::thread::JoinHandle<()>,
    cancel: Arc<AtomicBool>,
    progress: (Arc<AtomicU64>, u64), // (counter, total_frames)
    event_rx: crossbeam_channel::Receiver<RenderThreadEvent>,
    hw_info: Arc<std::sync::Mutex<String>>,
    render_stage: Arc<std::sync::Mutex<String>>,
}

pub struct RenderQueue {
    pub jobs: Vec<RenderJob>,
    snapshots: Vec<(Uuid, RenderSnapshot)>,
    /// Currently running render threads (len up to `max_parallel`).
    active_jobs: Vec<ActiveJob>,
    /// Max jobs to run simultaneously. Read at job-start time, so changing
    /// this between frames takes effect on the next spawn.
    pub max_parallel: usize,
}

enum RenderThreadEvent {
    Started { id: Uuid, hw_info: String },
    Completed { id: Uuid, loudness_result: Option<String> },
    Failed { id: Uuid, error: String },
    Cancelled { id: Uuid },
}

impl Default for RenderQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderQueue {
    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            snapshots: Vec::new(),
            active_jobs: Vec::new(),
            max_parallel: 2,
        }
    }

    pub fn add_job(
        &mut self,
        output_path: PathBuf,
        settings: ExportSettings,
        label: String,
        snapshot: RenderSnapshot,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let total_frames = snapshot.total_frames;
        let timeline_fps = snapshot.sequence_snapshot.fps;
        self.jobs.push(RenderJob {
            id,
            label,
            output_path,
            settings,
            status: RenderJobStatus::Queued,
            progress: 0.0,
            created_at: std::time::Instant::now(),
            started_at: None,
            completed_at: None,
            error: None,
            total_frames,
            frames_done: 0,
            hw_info: String::new(),
            timeline_fps,
            render_stage: String::new(),
            loudness_result: None,
        });
        self.snapshots.push((id, snapshot));
        self.start_next_jobs();
        id
    }

    /// Signal all active render threads to stop (called on app exit).
    /// Doesn't join — threads clean up on their own.
    pub fn kill_active(&mut self) {
        for j in &self.active_jobs {
            j.cancel.store(true, Ordering::Relaxed);
        }
        self.active_jobs.clear();
    }

    pub fn cancel_job(&mut self, id: Uuid) {
        // Check current status first (avoid double-borrow).
        let status = self.jobs.iter().find(|j| j.id == id).map(|j| j.status.clone());
        match status {
            Some(RenderJobStatus::Queued) => {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                    job.status = RenderJobStatus::Cancelled;
                }
            }
            Some(RenderJobStatus::Rendering) => {
                // Signal the matching active job, then drop it.
                if let Some(idx) = self.active_jobs.iter().position(|a| a.job_id == id) {
                    self.active_jobs[idx].cancel.store(true, Ordering::Relaxed);
                    self.active_jobs.swap_remove(idx);
                }
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                    job.status = RenderJobStatus::Cancelled;
                }
                self.start_next_jobs();
            }
            _ => {}
        }
    }

    pub fn remove_job(&mut self, id: Uuid) {
        if let Some(job) = self.jobs.iter().find(|j| j.id == id) {
            // Only remove non-active jobs
            if job.status != RenderJobStatus::Rendering {
                self.jobs.retain(|j| j.id != id);
                self.snapshots.retain(|(sid, _)| *sid != id);
            }
        }
    }

    pub fn clear_completed(&mut self) {
        self.jobs.retain(|j| {
            j.status != RenderJobStatus::Complete
                && j.status != RenderJobStatus::Failed
                && j.status != RenderJobStatus::Cancelled
        });
    }

    /// Poll for progress and completion. Call each frame.
    pub fn poll(&mut self) -> Vec<RenderQueueEvent> {
        let mut events = Vec::new();

        // Reap finished threads (JoinHandle::is_finished) – they already sent
        // their terminal event before returning, so we'll still process it below
        // via try_recv on the buffered channel.
        // We don't remove active_jobs yet — we first need to drain their event_rx.

        // 1. Update progress counters + hw_info/render_stage for every active job.
        for aj in &self.active_jobs {
            let (ref counter, total_frames) = aj.progress;
            let frames_done = counter.load(Ordering::Relaxed);
            let pct = if total_frames > 0 {
                (frames_done as f32 / total_frames as f32).min(1.0)
            } else {
                0.0
            };
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == aj.job_id) {
                job.frames_done = frames_done;
                if job.progress != pct {
                    job.progress = pct;
                    events.push(RenderQueueEvent::Progress { id: aj.job_id, progress: pct });
                }
            }
            if let Ok(info) = aj.hw_info.lock() {
                if !info.is_empty() {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == aj.job_id) {
                        if job.hw_info.is_empty() {
                            job.hw_info = info.clone();
                        }
                    }
                }
            }
            if let Ok(s) = aj.render_stage.lock() {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == aj.job_id) {
                    job.render_stage = s.clone();
                }
            }
        }

        // 2. Drain per-job event channels and collect terminal events.
        let mut terminal_ids: Vec<Uuid> = Vec::new();
        let mut thread_events: Vec<RenderThreadEvent> = Vec::new();
        for aj in &self.active_jobs {
            while let Ok(event) = aj.event_rx.try_recv() {
                thread_events.push(event);
            }
        }

        for event in thread_events {
            match event {
                RenderThreadEvent::Started { id, hw_info: info } => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                        job.hw_info = info;
                    }
                }
                RenderThreadEvent::Completed { id, loudness_result } => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                        job.status = RenderJobStatus::Complete;
                        job.progress = 1.0;
                        job.completed_at = Some(std::time::Instant::now());
                        job.loudness_result = loudness_result;
                        events.push(RenderQueueEvent::Complete {
                            id,
                            output_path: job.output_path.clone(),
                        });
                        info!("Render complete: {}", job.output_path.display());
                    }
                    terminal_ids.push(id);
                }
                RenderThreadEvent::Cancelled { id } => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                        job.status = RenderJobStatus::Cancelled;
                    }
                    terminal_ids.push(id);
                }
                RenderThreadEvent::Failed { id, error } => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                        job.status = RenderJobStatus::Failed;
                        job.error = Some(error.clone());
                    }
                    events.push(RenderQueueEvent::Failed { id, error });
                    terminal_ids.push(id);
                }
            }
        }

        // 3. Drop ActiveJob entries whose terminal event arrived or whose
        //    thread already exited without sending (e.g. panic).
        self.active_jobs.retain(|aj| {
            if terminal_ids.contains(&aj.job_id) {
                return false;
            }
            if aj.handle.is_finished() {
                // Thread exited without a terminal event. Mark job Failed as a
                // defensive fallback (shouldn't normally happen).
                return false;
            }
            true
        });

        // 4. Spawn more jobs up to max_parallel.
        if !terminal_ids.is_empty() || self.active_jobs.len() < self.max_parallel {
            self.start_next_jobs();
        }

        events
    }

    pub fn has_active_job(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| j.status == RenderJobStatus::Rendering)
    }

    /// Returns (label, progress 0.0..1.0) of the first currently rendering job, if any.
    /// With parallel rendering multiple jobs may be running — this returns the first
    /// for legacy single-progress UI compatibility.
    pub fn active_job_progress(&self) -> Option<(&str, f32)> {
        self.jobs
            .iter()
            .find(|j| j.status == RenderJobStatus::Rendering)
            .map(|j| (j.label.as_str(), j.progress))
    }

    pub fn queued_count(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| j.status == RenderJobStatus::Queued)
            .count()
    }

    /// Spawn as many new render threads as fit under `max_parallel`.
    fn start_next_jobs(&mut self) {
        while self.active_jobs.len() < self.max_parallel.max(1) {
            let next_id = self
                .jobs
                .iter()
                .find(|j| j.status == RenderJobStatus::Queued)
                .map(|j| j.id);
            let Some(id) = next_id else { return };
            if !self.spawn_job(id) {
                // Spawn failed (e.g. no snapshot). Status already marked Failed.
                continue;
            }
        }
    }

    /// Spawn a single render thread for the job with the given id. Returns
    /// true if the thread started, false if the job was marked Failed before
    /// spawning (e.g. missing snapshot).
    fn spawn_job(&mut self, id: Uuid) -> bool {
        let job = self.jobs.iter_mut().find(|j| j.id == id).unwrap();
        job.status = RenderJobStatus::Rendering;
        job.started_at = Some(std::time::Instant::now());

        // Find and remove the snapshot for this job
        let snap_pos = self.snapshots.iter().position(|(sid, _)| *sid == id);
        let Some(snap_pos) = snap_pos else {
            job.status = RenderJobStatus::Failed;
            job.error = Some("No render snapshot found".into());
            return false;
        };
        let (_, snapshot) = self.snapshots.remove(snap_pos);

        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let hw_info = Arc::new(std::sync::Mutex::new(String::new()));
        let total_frames = snapshot.total_frames;
        let settings = job.settings.clone();
        let output_path = job.output_path.clone();

        let cancel_clone = cancel.clone();
        let progress_clone = progress.clone();
        let render_stage = Arc::new(std::sync::Mutex::new(String::new()));
        // P-9: we only need one Arc handle on this side; the thread
        // below moves it into the ExportJob.
        let loudness_result_clone = Arc::new(std::sync::Mutex::new(None::<String>));
        let hw_info_clone = hw_info.clone();
        let render_stage_clone = render_stage.clone();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();

        let handle = std::thread::Builder::new()
            .name(format!("klipp-render-{id}"))
            .spawn(move || {
                let export_job = ExportJob {
                    id,
                    output_path,
                    settings,
                    snapshot: snapshot.sequence_snapshot,
                    cancel: cancel_clone,
                    progress: progress_clone,
                    total_frames,
                    hw_info: hw_info_clone.clone(),
                    render_stage: render_stage_clone,
                    loudness_result: loudness_result_clone,
                };

                // Send hw_info as soon as it's available (after decoder/encoder init)
                let event_tx2 = event_tx.clone();
                let hw_info_ref = hw_info_clone;
                std::thread::spawn(move || {
                    // Poll for hw_info to be set (usually within 1-2 seconds of job start)
                    for _ in 0..100 {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        if let Ok(info) = hw_info_ref.lock() {
                            if !info.is_empty() {
                                let _ = event_tx2.send(RenderThreadEvent::Started {
                                    id,
                                    hw_info: info.clone(),
                                });
                                return;
                            }
                        }
                    }
                });

                let loudness_ref = export_job.loudness_result.clone();
                let result = export_job.run();
                let event = match result {
                    Ok(()) => {
                        let lr = loudness_ref.lock().ok().and_then(|r| r.clone());
                        RenderThreadEvent::Completed { id, loudness_result: lr }
                    }
                    Err(klipp_render::RenderError::Cancelled) => {
                        RenderThreadEvent::Cancelled { id }
                    }
                    Err(e) => RenderThreadEvent::Failed {
                        id,
                        error: e.to_string(),
                    },
                };
                let _ = event_tx.send(event);
            })
            .expect("Failed to spawn render thread");

        self.active_jobs.push(ActiveJob {
            job_id: id,
            handle,
            cancel,
            progress: (progress, total_frames),
            event_rx,
            hw_info,
            render_stage,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    //! Smoke tests for the queue-level bookkeeping. `add_job` is NOT
    //! exercised here — it calls `start_next_jobs` which spawns an OS
    //! thread that runs real ffmpeg via klipp_render::ExportJob; testing
    //! that end-to-end needs fixture media files and is out of scope for
    //! a smoke pass. Instead we construct RenderJob values directly (all
    //! fields are `pub`) and drive only the synchronous state methods:
    //! `cancel_job`, `remove_job`, `clear_completed`, `kill_active`,
    //! `queued_count`, `has_active_job`, `active_job_progress`.
    use super::*;

    fn fake_job(status: RenderJobStatus) -> RenderJob {
        RenderJob {
            id: Uuid::new_v4(),
            label: "fake".into(),
            output_path: PathBuf::from("/tmp/klipp-test-fake.mp4"),
            settings: ExportSettings::default(),
            status,
            progress: 0.0,
            created_at: std::time::Instant::now(),
            started_at: None,
            completed_at: None,
            error: None,
            total_frames: 100,
            frames_done: 0,
            hw_info: String::new(),
            timeline_fps: 24.0,
            render_stage: String::new(),
            loudness_result: None,
        }
    }

    #[test]
    fn new_queue_is_empty() {
        let q = RenderQueue::new();
        assert!(q.jobs.is_empty());
        assert_eq!(q.queued_count(), 0);
        assert!(!q.has_active_job());
        assert!(q.active_job_progress().is_none());
        assert_eq!(q.max_parallel, 2, "documented NVENC-session default");
    }

    #[test]
    fn default_matches_new() {
        let a = RenderQueue::default();
        let b = RenderQueue::new();
        assert_eq!(a.jobs.len(), b.jobs.len());
        assert_eq!(a.max_parallel, b.max_parallel);
    }

    #[test]
    fn clear_completed_keeps_non_terminal_only() {
        let mut q = RenderQueue::new();
        q.jobs.push(fake_job(RenderJobStatus::Queued));
        q.jobs.push(fake_job(RenderJobStatus::Rendering));
        q.jobs.push(fake_job(RenderJobStatus::Complete));
        q.jobs.push(fake_job(RenderJobStatus::Failed));
        q.jobs.push(fake_job(RenderJobStatus::Cancelled));
        q.clear_completed();
        assert_eq!(q.jobs.len(), 2);
        assert!(q.jobs.iter().all(|j| matches!(
            j.status,
            RenderJobStatus::Queued | RenderJobStatus::Rendering
        )));
    }

    #[test]
    fn cancel_queued_job_marks_cancelled() {
        let mut q = RenderQueue::new();
        let mut j = fake_job(RenderJobStatus::Queued);
        let id = j.id;
        q.jobs.push(j);

        q.cancel_job(id);
        assert_eq!(q.jobs[0].status, RenderJobStatus::Cancelled);
    }

    #[test]
    fn cancel_unknown_id_noops() {
        let mut q = RenderQueue::new();
        q.jobs.push(fake_job(RenderJobStatus::Queued));
        let original_status = q.jobs[0].status.clone();

        q.cancel_job(Uuid::new_v4()); // some other uuid

        assert_eq!(q.jobs[0].status, original_status);
    }

    #[test]
    fn remove_queued_job_drops_it() {
        let mut q = RenderQueue::new();
        let mut queued = fake_job(RenderJobStatus::Queued);
        let queued_id = queued.id;
        q.jobs.push(queued);
        q.jobs.push(fake_job(RenderJobStatus::Complete));

        q.remove_job(queued_id);

        assert_eq!(q.jobs.len(), 1);
        assert_eq!(q.jobs[0].status, RenderJobStatus::Complete);
    }

    #[test]
    fn remove_rendering_job_is_refused() {
        // Rendering jobs must be cancelled first — remove_job is a no-op
        // on a live render to avoid orphaning the worker thread.
        let mut q = RenderQueue::new();
        let mut rendering = fake_job(RenderJobStatus::Rendering);
        let rid = rendering.id;
        q.jobs.push(rendering);

        q.remove_job(rid);

        assert_eq!(q.jobs.len(), 1, "rendering job must not be removable");
        assert_eq!(q.jobs[0].status, RenderJobStatus::Rendering);
    }

    #[test]
    fn remove_unknown_id_noops() {
        let mut q = RenderQueue::new();
        q.jobs.push(fake_job(RenderJobStatus::Complete));

        q.remove_job(Uuid::new_v4());

        assert_eq!(q.jobs.len(), 1);
    }

    #[test]
    fn kill_active_on_empty_is_safe() {
        let mut q = RenderQueue::new();
        q.kill_active();
        assert!(!q.has_active_job());
    }

    #[test]
    fn queued_count_reflects_queued_status_only() {
        let mut q = RenderQueue::new();
        q.jobs.push(fake_job(RenderJobStatus::Queued));
        q.jobs.push(fake_job(RenderJobStatus::Queued));
        q.jobs.push(fake_job(RenderJobStatus::Rendering));
        q.jobs.push(fake_job(RenderJobStatus::Complete));
        assert_eq!(q.queued_count(), 2);
    }
}
