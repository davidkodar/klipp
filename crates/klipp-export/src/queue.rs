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
    ) -> Result<Self, klipp_render::RenderError> {
        Self::from_sequence_with_all(sequence, &[], media_pool, settings, global_lut_path, use_marked_range)
    }

    /// Create a render snapshot, resolving compound clips via `all_sequences`.
    pub fn from_sequence_with_all(
        sequence: &Sequence,
        all_sequences: &[Sequence],
        media_pool: &MediaPool,
        settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
    ) -> Result<Self, klipp_render::RenderError> {
        let snapshot =
            SequenceSnapshot::from_sequence_with_all(sequence, all_sequences, media_pool, settings, global_lut_path, use_marked_range)?;
        let total_frames = snapshot.frames_to_export();
        Ok(Self {
            sequence_snapshot: snapshot,
            total_frames,
        })
    }
}

pub struct RenderQueue {
    pub jobs: Vec<RenderJob>,
    snapshots: Vec<(Uuid, RenderSnapshot)>,
    /// Active render thread handle.
    active_thread: Option<std::thread::JoinHandle<()>>,
    /// Cancellation flag for the active job.
    active_cancel: Option<Arc<AtomicBool>>,
    /// Progress counter for the active job (frames completed).
    active_progress: Option<(Uuid, Arc<AtomicU64>, u64)>, // (id, counter, total_frames)
    /// Channel for completion/failure events from the render thread.
    event_rx: Option<crossbeam_channel::Receiver<RenderThreadEvent>>,
    /// Shared hw_info from the active export job.
    active_hw_info: Option<Arc<std::sync::Mutex<String>>>,
    /// Shared render_stage from the active export job.
    active_render_stage: Option<Arc<std::sync::Mutex<String>>>,
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
            active_thread: None,
            active_cancel: None,
            active_progress: None,
            event_rx: None,
            active_hw_info: None,
            active_render_stage: None,
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
            output_path: output_path.clone(),
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
        self.start_next_if_idle();
        id
    }

    /// Signal the active render thread to stop (called on app exit).
    pub fn kill_active(&mut self) {
        if let Some(cancel) = &self.active_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        // Don't join — let the thread clean up on its own
        self.active_thread = None;
        self.active_cancel = None;
        self.active_progress = None;
        self.event_rx = None;
        self.active_hw_info = None;
        self.active_render_stage = None;
    }

    pub fn cancel_job(&mut self, id: Uuid) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            match job.status {
                RenderJobStatus::Queued => {
                    job.status = RenderJobStatus::Cancelled;
                }
                RenderJobStatus::Rendering => {
                    if let Some(cancel) = &self.active_cancel {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    job.status = RenderJobStatus::Cancelled;
                    // Thread will detect cancellation and clean up
                    self.active_thread = None;
                    self.active_cancel = None;
                    self.active_progress = None;
                    self.event_rx = None;
                    self.active_hw_info = None;
                    self.active_render_stage = None;
                    self.start_next_if_idle();
                }
                _ => {}
            }
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

        // Update progress from AtomicU64
        if let Some((id, ref progress, total_frames)) = self.active_progress {
            let frames_done = progress.load(Ordering::Relaxed);
            let pct = if total_frames > 0 {
                (frames_done as f32 / total_frames as f32).min(1.0)
            } else {
                0.0
            };
            if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                job.frames_done = frames_done;
                if job.progress != pct {
                    job.progress = pct;
                    events.push(RenderQueueEvent::Progress { id, progress: pct });
                }
            }
        }

        // Read hw_info + render_stage from shared mutexes
        if let Some((id, _, _)) = self.active_progress {
            if let Some(ref hw) = self.active_hw_info {
                if let Ok(info) = hw.lock() {
                    if !info.is_empty() {
                        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                            if job.hw_info.is_empty() {
                                job.hw_info = info.clone();
                            }
                        }
                    }
                }
            }
            if let Some(ref stage) = self.active_render_stage {
                if let Ok(s) = stage.lock() {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                        job.render_stage = s.clone();
                    }
                }
            }
        }

        // Check for thread completion events
        let thread_events: Vec<RenderThreadEvent> = if let Some(ref rx) = self.event_rx {
            let mut evts = Vec::new();
            while let Ok(event) = rx.try_recv() {
                evts.push(event);
            }
            evts
        } else {
            Vec::new()
        };

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
                    self.active_thread = None;
                    self.active_cancel = None;
                    self.active_progress = None;
                    self.event_rx = None;
                    self.active_hw_info = None;
                    self.active_render_stage = None;
                    self.start_next_if_idle();
                }
                RenderThreadEvent::Cancelled { id } => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                        job.status = RenderJobStatus::Cancelled;
                    }
                    self.active_thread = None;
                    self.active_cancel = None;
                    self.active_progress = None;
                    self.event_rx = None;
                    self.active_hw_info = None;
                    self.active_render_stage = None;
                    self.start_next_if_idle();
                }
                RenderThreadEvent::Failed { id, error } => {
                    if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                        job.status = RenderJobStatus::Failed;
                        job.error = Some(error.clone());
                    }
                    events.push(RenderQueueEvent::Failed { id, error });
                    self.active_thread = None;
                    self.active_cancel = None;
                    self.active_progress = None;
                    self.event_rx = None;
                    self.active_hw_info = None;
                    self.active_render_stage = None;
                    self.start_next_if_idle();
                }
            }
        }

        events
    }

    pub fn has_active_job(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| j.status == RenderJobStatus::Rendering)
    }

    /// Returns (label, progress 0.0..1.0) of the currently rendering job, if any.
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

    fn start_next_if_idle(&mut self) {
        if self.active_thread.is_some() {
            return;
        }

        let next_id = self
            .jobs
            .iter()
            .find(|j| j.status == RenderJobStatus::Queued)
            .map(|j| j.id);

        let Some(id) = next_id else { return };

        let job = self.jobs.iter_mut().find(|j| j.id == id).unwrap();
        job.status = RenderJobStatus::Rendering;
        job.started_at = Some(std::time::Instant::now());

        // Find and remove the snapshot for this job
        let snap_pos = self.snapshots.iter().position(|(sid, _)| *sid == id);
        let Some(snap_pos) = snap_pos else {
            job.status = RenderJobStatus::Failed;
            job.error = Some("No render snapshot found".into());
            return;
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
        let loudness_result_shared = Arc::new(std::sync::Mutex::new(None::<String>));
        let hw_info_clone = hw_info.clone();
        let render_stage_clone = render_stage.clone();
        let loudness_result_clone = loudness_result_shared.clone();
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

        self.active_thread = Some(handle);
        self.active_cancel = Some(cancel);
        self.active_progress = Some((id, progress, total_frames));
        self.event_rx = Some(event_rx);
        self.active_hw_info = Some(hw_info);
        self.active_render_stage = Some(render_stage);
    }
}
