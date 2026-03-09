use klipp_core::{ExportSettings, Resolution};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;
use std::path::PathBuf;
use tracing::{error, info};
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
}

pub enum RenderQueueEvent {
    Progress { id: Uuid, progress: f32 },
    Complete { id: Uuid, output_path: PathBuf },
    Failed { id: Uuid, error: String },
}

/// Snapshot of sequence + media needed to build the ffmpeg command.
/// Captured at the time the job is added so editing can continue.
pub struct RenderSnapshot {
    pub clip_entries: Vec<(String, f64, f64)>, // (path, seek_secs, duration_secs)
    pub resolution: Resolution,
    pub total_duration_secs: f64,
}

impl RenderSnapshot {
    pub fn from_sequence(
        sequence: &Sequence,
        media_pool: &MediaPool,
        export_range: Option<(klipp_core::RationalTime, klipp_core::RationalTime)>,
    ) -> Self {
        let total_duration = if let Some((in_pt, out_pt)) = export_range {
            (out_pt - in_pt).to_secs_f64()
        } else {
            sequence.total_duration().to_secs_f64()
        };
        let res = sequence.resolution;

        let mut clip_entries = Vec::new();
        if let Some(track) = sequence.video_track() {
            for clip in &track.clips {
                if let Some((range_in, range_out)) = export_range {
                    if clip.timeline_out <= range_in || clip.timeline_in >= range_out {
                        continue;
                    }
                }
                if let Some(media) = clip.media_id().and_then(|mid| media_pool.get(&mid)) {
                    let path_str = media.path.to_string_lossy().to_string();
                    let (src_in, src_out) = if let Some((range_in, range_out)) = export_range {
                        let clip_start = clip.timeline_in.max(range_in);
                        let clip_end = clip.timeline_out.min(range_out);
                        let offset_start = clip_start - clip.timeline_in;
                        let offset_end = clip_end - clip.timeline_in;
                        (clip.source_in + offset_start, clip.source_in + offset_end)
                    } else {
                        (clip.source_in, clip.source_out)
                    };
                    let in_secs = src_in.to_secs_f64();
                    let dur_secs = src_out.to_secs_f64() - in_secs;
                    clip_entries.push((path_str, in_secs, dur_secs));
                }
            }
        }

        Self {
            clip_entries,
            resolution: res,
            total_duration_secs: total_duration,
        }
    }
}

pub struct RenderQueue {
    pub jobs: Vec<RenderJob>,
    snapshots: Vec<(Uuid, RenderSnapshot)>,
    active_process: Option<std::process::Child>,
    active_stdout_thread: Option<std::thread::JoinHandle<()>>,
    progress_rx: Option<crossbeam_channel::Receiver<(Uuid, f32)>>,
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
            active_process: None,
            active_stdout_thread: None,
            progress_rx: None,
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
        });
        self.snapshots.push((id, snapshot));
        self.start_next_if_idle();
        id
    }

    /// Kill any active ffmpeg child process (called on app exit).
    pub fn kill_active(&mut self) {
        if let Some(ref mut child) = self.active_process {
            let _ = child.kill();
        }
        self.active_process = None;
    }

    pub fn cancel_job(&mut self, id: Uuid) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            match job.status {
                RenderJobStatus::Queued => {
                    job.status = RenderJobStatus::Cancelled;
                }
                RenderJobStatus::Rendering => {
                    if let Some(child) = &mut self.active_process {
                        let _ = child.kill();
                    }
                    job.status = RenderJobStatus::Cancelled;
                    self.active_process = None;
                    self.active_stdout_thread = None;
                    self.progress_rx = None;
                    self.start_next_if_idle();
                }
                _ => {}
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

        // Drain progress updates
        if let Some(rx) = &self.progress_rx {
            while let Ok((id, progress)) = rx.try_recv() {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                    job.progress = progress;
                }
                events.push(RenderQueueEvent::Progress { id, progress });
            }
        }

        // Check if ffmpeg has exited
        let exited = if let Some(child) = &mut self.active_process {
            match child.try_wait() {
                Ok(Some(status)) => Some(status.success()),
                Ok(None) => None,
                Err(_) => Some(false),
            }
        } else {
            None
        };

        if let Some(success) = exited {
            let active_id = self
                .jobs
                .iter()
                .find(|j| j.status == RenderJobStatus::Rendering)
                .map(|j| j.id);

            if let Some(id) = active_id {
                let job = self.jobs.iter_mut().find(|j| j.id == id).unwrap();
                if success {
                    job.status = RenderJobStatus::Complete;
                    job.progress = 1.0;
                    job.completed_at = Some(std::time::Instant::now());
                    events.push(RenderQueueEvent::Complete {
                        id,
                        output_path: job.output_path.clone(),
                    });
                    info!("Render complete: {}", job.output_path.display());
                } else {
                    job.status = RenderJobStatus::Failed;
                    let err = job
                        .error
                        .clone()
                        .unwrap_or_else(|| "ffmpeg exited with error".into());
                    events.push(RenderQueueEvent::Failed {
                        id,
                        error: err,
                    });
                }
            }
            self.active_process = None;
            self.active_stdout_thread = None;
            self.progress_rx = None;
            self.start_next_if_idle();
        }

        events
    }

    pub fn has_active_job(&self) -> bool {
        self.jobs.iter().any(|j| j.status == RenderJobStatus::Rendering)
    }

    pub fn queued_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.status == RenderJobStatus::Queued).count()
    }

    fn start_next_if_idle(&mut self) {
        if self.active_process.is_some() {
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

        let snapshot = self.snapshots.iter().find(|(sid, _)| *sid == id).map(|(_, s)| s);
        let Some(snapshot) = snapshot else {
            job.status = RenderJobStatus::Failed;
            job.error = Some("No render snapshot found".into());
            return;
        };

        match Self::spawn_ffmpeg(&job.settings, &job.output_path, snapshot) {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                let (tx, rx) = crossbeam_channel::unbounded();
                let total_dur = snapshot.total_duration_secs;
                let job_id = id;

                let handle = std::thread::spawn(move || {
                    if let Some(stdout) = stdout {
                        use std::io::BufRead;
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if let Some(time_str) = line.strip_prefix("out_time_us=") {
                                if let Ok(us) = time_str.trim().parse::<i64>() {
                                    let secs = us as f64 / 1_000_000.0;
                                    let pct = if total_dur > 0.0 {
                                        (secs / total_dur).min(1.0) as f32
                                    } else {
                                        0.0
                                    };
                                    let _ = tx.send((job_id, pct));
                                }
                            }
                        }
                    }
                });

                self.active_process = Some(child);
                self.active_stdout_thread = Some(handle);
                self.progress_rx = Some(rx);
            }
            Err(e) => {
                error!("Failed to spawn ffmpeg for render job: {e}");
                let job = self.jobs.iter_mut().find(|j| j.id == id).unwrap();
                job.status = RenderJobStatus::Failed;
                job.error = Some(format!("Failed to start ffmpeg: {e}"));
                self.start_next_if_idle();
            }
        }
    }

    fn spawn_ffmpeg(
        settings: &ExportSettings,
        output_path: &std::path::Path,
        snapshot: &RenderSnapshot,
    ) -> Result<std::process::Child, std::io::Error> {
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.arg("-y");

        for (path, ss, t) in &snapshot.clip_entries {
            cmd.arg("-ss").arg(format!("{:.6}", ss));
            cmd.arg("-t").arg(format!("{:.6}", t));
            cmd.arg("-i").arg(path);
        }

        let n = snapshot.clip_entries.len();
        let w = settings.resolution.as_ref().map(|r| r.width).unwrap_or(snapshot.resolution.width);
        let h = settings.resolution.as_ref().map(|r| r.height).unwrap_or(snapshot.resolution.height);

        let mut filter = String::new();
        for i in 0..n {
            filter.push_str(&format!(
                "[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1[v{i}];",
            ));
        }
        for i in 0..n {
            filter.push_str(&format!("[v{i}]"));
        }
        filter.push_str(&format!("concat=n={n}:v=1:a=0[outv]"));

        cmd.arg("-filter_complex").arg(&filter);
        cmd.arg("-map").arg("[outv]");
        cmd.arg("-map").arg("0:a?");

        cmd.arg("-c:v")
            .arg(settings.video_codec.ffmpeg_codec())
            .arg("-crf")
            .arg(settings.crf.to_string())
            .arg("-preset")
            .arg("medium");

        for arg in settings.audio_codec.ffmpeg_args(settings.audio_bitrate_kbps) {
            cmd.arg(arg);
        }

        cmd.arg("-progress")
            .arg("pipe:1")
            .arg(output_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        info!("Render queue ffmpeg command: {:?}", cmd);
        cmd.spawn()
    }
}
