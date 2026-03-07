use crate::texture::SharedFrameBuffer;
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{AppCommand, AppEvent, Fps, MediaId, RationalTime, Resolution, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Background V2 decoder state — runs ffmpeg in a thread to pipe raw RGBA frames
/// without blocking the main playback thread.
struct V2DecoderState {
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    clip_key: (usize, PathBuf), // (clip_index, path) to detect when clip changes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Stopped,
    Playing,
    Paused,
}

pub struct PlaybackEngine {
    cmd_rx: Receiver<AppCommand>,
    event_tx: Sender<AppEvent>,
    mpv: libmpv2::Mpv,
    frame_buffer: SharedFrameBuffer,
    state: PlaybackState,
    current_file: Option<PathBuf>,
    current_clip_index: usize,
    duration: Timecode,
    fps: Fps,
    playhead: Timecode,
    sequence: Arc<RwLock<Sequence>>,
    media_pool: Arc<RwLock<MediaPool>>,
    repaint_cb: Box<dyn Fn() + Send>,
    last_frame_time: std::time::Instant,
    dropped_frames: u64,
    // Video 2 compositing — background ffmpeg thread
    v2_shared_frame: Arc<std::sync::Mutex<Option<(Vec<u8>, u32, u32)>>>,
    v2_decoder_handle: Option<V2DecoderState>,
}

impl PlaybackEngine {
    pub fn new(
        cmd_rx: Receiver<AppCommand>,
        event_tx: Sender<AppEvent>,
        frame_buffer: SharedFrameBuffer,
        sequence: Arc<RwLock<Sequence>>,
        media_pool: Arc<RwLock<MediaPool>>,
        repaint_cb: Box<dyn Fn() + Send>,
    ) -> Result<Self, klipp_core::KlippError> {
        let mpv =
            libmpv2::Mpv::new().map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;

        mpv.set_property("vo", "null")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;
        mpv.set_property("hwdec", "no")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;
        // Audio enabled: don't set aid=no (Phase 1 disabled it)
        mpv.set_property("pause", "yes")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;
        mpv.set_property("keep-open", "no")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;

        warn!(
            "Using screenshot-based frame grab (render API requires OpenGL context). \
             Frames go through /dev/shm to minimize I/O latency."
        );

        Ok(Self {
            cmd_rx,
            event_tx,
            mpv,
            frame_buffer,
            state: PlaybackState::Idle,
            current_file: None,
            current_clip_index: 0,
            duration: Timecode::zero(),
            fps: Fps::film_24(),
            playhead: Timecode::zero(),
            sequence,
            media_pool,
            repaint_cb,
            last_frame_time: std::time::Instant::now(),
            dropped_frames: 0,
            v2_shared_frame: Arc::new(std::sync::Mutex::new(None)),
            v2_decoder_handle: None,
        })
    }

    pub fn run(mut self) {
        info!("Engine thread starting");

        loop {
            // Determine how long to wait for commands
            let timeout = if self.state == PlaybackState::Playing {
                let frame_duration =
                    Duration::from_secs_f64(self.fps.frame_duration_secs());
                let elapsed = self.last_frame_time.elapsed();
                if elapsed < frame_duration {
                    frame_duration - elapsed
                } else {
                    Duration::ZERO
                }
            } else {
                Duration::from_millis(50)
            };

            // Wait for a command or until next frame is due
            if !timeout.is_zero() {
                match self.cmd_rx.recv_timeout(timeout) {
                    Ok(cmd) => {
                        if self.handle_command(cmd) {
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }

            // Drain any additional pending commands
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                if self.handle_command(cmd) {
                    info!("Engine thread shutting down");
                    return;
                }
            }

            if self.state == PlaybackState::Playing {
                self.update_playback();
            }
        }

        info!("Engine thread shutting down");
    }

    fn handle_command(&mut self, cmd: AppCommand) -> bool {
        match cmd {
            AppCommand::Quit => return true,

            AppCommand::LoadMediaFile { path, media_id } => {
                self.load_file(path, media_id);
            }

            AppCommand::Play => {
                debug!("Play command received, current state: {:?}", self.state);
                if self.state == PlaybackState::Paused || self.state == PlaybackState::Stopped {
                    self.handle_play();
                }
            }

            AppCommand::Pause => {
                if self.state == PlaybackState::Playing {
                    self.mpv
                        .set_property("pause", "yes")
                        .unwrap_or_else(|e| error!("mpv set pause=yes: {e}"));
                    self.stop_v2_decoder();
                    self.state = PlaybackState::Paused;
                    let _ = self.event_tx.send(AppEvent::PlaybackPaused);
                    (self.repaint_cb)();
                }
            }

            AppCommand::Stop => {
                if self.state != PlaybackState::Idle {
                    self.mpv
                        .set_property("pause", "yes")
                        .unwrap_or_else(|e| error!("mpv set pause=yes: {e}"));
                    self.stop_v2_decoder();
                    self.seek_to_timecode(Timecode::zero());
                    self.state = PlaybackState::Stopped;
                    let _ = self.event_tx.send(AppEvent::PlaybackStopped);
                    (self.repaint_cb)();
                }
            }

            AppCommand::SeekTo(tc) => {
                if self.state != PlaybackState::Idle {
                    self.seek_in_sequence(tc);
                }
            }

            AppCommand::SetSpeed(speed) => {
                self.mpv
                    .set_property("speed", speed)
                    .unwrap_or_else(|e| error!("mpv set speed: {e}"));
            }

            AppCommand::SeekRelative(frames) => {
                if self.state != PlaybackState::Idle {
                    let seq_dur = self.sequence_duration();
                    let new_tc = RationalTime { value: self.playhead.value + frames, rate: self.playhead.rate };
                    let clamped = RationalTime { value: new_tc.value.max(0).min(seq_dur.value), rate: new_tc.rate };
                    self.seek_in_sequence(clamped);
                }
            }

            AppCommand::SetPlayhead(tc) => {
                if self.state != PlaybackState::Idle {
                    self.seek_in_sequence(tc);
                }
            }

            // UI-side commands — engine ignores them
            AppCommand::ImportMedia { .. }
            | AppCommand::AppendClip { .. }
            | AppCommand::AppendClipToTrack { .. }
            | AppCommand::RemoveClip { .. }
            | AppCommand::MoveClip { .. }
            | AppCommand::TrimClip { .. }
            | AppCommand::SelectClip(_)
            | AppCommand::Undo
            | AppCommand::Redo
            | AppCommand::SequenceUpdated
            | AppCommand::ShowExportDialog
            | AppCommand::ExportSequence { .. }
            | AppCommand::CancelExport
            | AppCommand::NewProject
            | AppCommand::SaveProject
            | AppCommand::SaveProjectAs { .. }
            | AppCommand::OpenProject { .. }
            | AppCommand::SetTrackOpacity { .. }
            | AppCommand::SetTrackVisibility { .. }
            | AppCommand::SetTrackVolume { .. }
            | AppCommand::SetTrackMute { .. }
            | AppCommand::SetTrackSolo { .. }
            | AppCommand::BladeClip { .. }
            | AppCommand::CutClip { .. }
            | AppCommand::CopyClip { .. }
            | AppCommand::PasteClip
            | AppCommand::LoadSourceMedia { .. }
            | AppCommand::SetSourceInPoint(_)
            | AppCommand::SetSourceOutPoint(_)
            | AppCommand::InsertSourceAtPlayhead
            | AppCommand::SetSequenceInPoint(_)
            | AppCommand::SetSequenceOutPoint(_)
            | AppCommand::ClearSequenceInPoint
            | AppCommand::ClearSequenceOutPoint
            | AppCommand::GenerateProxy { .. }
            | AppCommand::SetUseProxies(_)
            | AppCommand::RelinkMedia { .. }
            | AppCommand::SetSnapEnabled(_)
            | AppCommand::SetRippleEnabled(_)
            | AppCommand::SetViewerMode(_)
            | AppCommand::SetActiveTool(_)
            | AppCommand::ToggleFullscreen => {}
        }

        false
    }

    fn handle_play(&mut self) {
        let seq = self.sequence.read().unwrap();
        let clip_count = seq.video_track().map(|t| t.clips.len()).unwrap_or(0);

        // Find the clip at the current playhead position (resume from where we are)
        let clip_info = seq.video_track().and_then(|track| {
            // First try: clip at current playhead
            if let Some((idx, clip)) = track.clip_at_timecode(self.playhead) {
                let source_secs = clip.source_in.to_secs_f64()
                    + (self.playhead - clip.timeline_in).to_secs_f64();
                return Some((idx, clip.media_id, source_secs, self.playhead));
            }
            // Fallback: find the next clip after the playhead
            for (idx, clip) in track.clips.iter().enumerate() {
                if clip.timeline_in >= self.playhead {
                    return Some((idx, clip.media_id, clip.source_in.to_secs_f64(), clip.timeline_in));
                }
            }
            // Last resort: first clip
            track.clips.first().map(|clip| {
                (0, clip.media_id, clip.source_in.to_secs_f64(), clip.timeline_in)
            })
        });
        drop(seq);

        info!("handle_play: sequence has {clip_count} clips, playhead={:?}", self.playhead);

        if let Some((clip_idx, media_id, source_secs, timeline_pos)) = clip_info {
            if let Some(path) = self.path_for_media(media_id) {
                info!("handle_play: loading clip {} from {:?}, source_secs={:.3}", clip_idx, path, source_secs);
                self.load_clip_file(&path);
                self.current_clip_index = clip_idx;
                if source_secs > 0.01 {
                    self.seek_mpv(source_secs);
                }
                self.playhead = timeline_pos;
            } else {
                warn!("No path found for media {:?} — cannot play", media_id);
                return;
            }
        } else {
            debug!("No clips in sequence — nothing to play");
            return;
        }

        info!("handle_play: starting playback");
        self.mpv
            .set_property("pause", "no")
            .unwrap_or_else(|e| error!("mpv set pause=no: {e}"));
        self.state = PlaybackState::Playing;
        self.dropped_frames = 0;
        self.last_frame_time = std::time::Instant::now();

        // Start V2 background decoder if there's a clip at the playhead
        self.maybe_start_v2_decoder();

        let _ = self.event_tx.send(AppEvent::PlaybackStarted);
        (self.repaint_cb)();
    }

    /// Start the background ffmpeg V2 decoder if Video 2 has a clip at the playhead.
    fn maybe_start_v2_decoder(&mut self) {
        let v2_info = {
            let seq = self.sequence.read().unwrap();
            seq.video_track2().and_then(|track| {
                if track.muted || track.clips.is_empty() {
                    return None;
                }
                track.clip_at_timecode(self.playhead).map(|(idx, clip)| {
                    let source_secs = clip.source_in.to_secs_f64()
                        + (self.playhead - clip.timeline_in).to_secs_f64();
                    (idx, clip.media_id, source_secs)
                })
            })
        };

        if let Some((clip_idx, media_id, source_secs)) = v2_info {
            if let Some(path) = self.path_for_media(media_id) {
                let out_w: i64 = self.mpv.get_property("width").unwrap_or(1920);
                let out_h: i64 = self.mpv.get_property("height").unwrap_or(1080);
                self.start_v2_decoder(&path, source_secs, out_w as u32, out_h as u32, clip_idx);
            }
        }
    }

    /// Spawn a background ffmpeg thread to decode V2 frames as raw RGBA pipe output.
    fn start_v2_decoder(&mut self, path: &std::path::Path, start_secs: f64, out_w: u32, out_h: u32, clip_idx: usize) {
        let clip_key = (clip_idx, path.to_path_buf());

        // If already decoding the same clip, don't restart
        if let Some(ref handle) = self.v2_decoder_handle {
            if handle.clip_key == clip_key {
                return;
            }
        }

        self.stop_v2_decoder();

        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shared_frame = self.v2_shared_frame.clone();
        let stop = stop_flag.clone();
        let fps_rate = self.fps.rate();
        let path_owned = path.to_path_buf();

        let thread = std::thread::spawn(move || {
            let fps_val = fps_rate as f64;
            let mut child = match Command::new("ffmpeg")
                .args([
                    "-ss", &format!("{start_secs:.4}"),
                    "-i", &path_owned.to_string_lossy(),
                    "-f", "rawvideo",
                    "-pix_fmt", "rgba",
                    "-s", &format!("{out_w}x{out_h}"),
                    "-r", &format!("{fps_val}"),
                    "-v", "error",
                    "-",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    error!("Failed to spawn ffmpeg for V2: {e}");
                    return;
                }
            };

            let stdout = child.stdout.take().unwrap();
            let mut reader = std::io::BufReader::with_capacity(
                (out_w * out_h * 4) as usize * 2,
                stdout,
            );
            let frame_size = (out_w * out_h * 4) as usize;
            let frame_duration = Duration::from_secs_f64(1.0 / fps_val);

            debug!("V2 ffmpeg decoder started: {out_w}x{out_h} @ {fps_val} fps, seek={start_secs:.3}s");

            loop {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }

                let frame_start = std::time::Instant::now();
                let mut buf = vec![0u8; frame_size];

                use std::io::Read;
                match reader.read_exact(&mut buf) {
                    Ok(_) => {
                        if let Ok(mut lock) = shared_frame.lock() {
                            *lock = Some((buf, out_w, out_h));
                        }
                    }
                    Err(_) => break, // EOF or error
                }

                // Pace to real-time so we don't race ahead
                let elapsed = frame_start.elapsed();
                if elapsed < frame_duration {
                    std::thread::sleep(frame_duration - elapsed);
                }

                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
            }

            let _ = child.kill();
            let _ = child.wait();
            debug!("V2 ffmpeg decoder thread exiting");
        });

        self.v2_decoder_handle = Some(V2DecoderState {
            stop_flag,
            thread: Some(thread),
            clip_key,
        });
    }

    /// Stop the background V2 decoder thread.
    fn stop_v2_decoder(&mut self) {
        if let Some(mut handle) = self.v2_decoder_handle.take() {
            handle.stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = handle.thread.take() {
                let _ = thread.join();
            }
        }
        if let Ok(mut lock) = self.v2_shared_frame.lock() {
            *lock = None;
        }
    }

    fn sequence_duration(&self) -> Timecode {
        self.sequence
            .read()
            .unwrap()
            .total_duration()
    }

    fn seek_in_sequence(&mut self, tc: Timecode) {
        let seq = self.sequence.read().unwrap();
        if let Some(track) = seq.video_track() {
            if let Some((idx, clip)) = track.clip_at_timecode(tc) {
                let media_id = clip.media_id;
                let source_secs =
                    clip.source_in.to_secs_f64() + (tc - clip.timeline_in).to_secs_f64();
                let need_load = self.current_clip_index != idx
                    || self.current_file.is_none();
                drop(seq);

                if need_load {
                    if let Some(path) = self.path_for_media(media_id) {
                        self.load_clip_file(&path);
                        self.current_clip_index = idx;
                    }
                }
                self.seek_mpv(source_secs);
                self.playhead = tc;
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
                self.last_frame_time = std::time::Instant::now();
                self.grab_frame();
            } else {
                drop(seq);
                self.playhead = tc;
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
            }
        } else {
            drop(seq);
            self.seek_to_timecode(tc);
        }
    }

    /// Look up file path for a media_id from the shared media pool.
    fn path_for_media(&self, media_id: MediaId) -> Option<PathBuf> {
        self.media_pool
            .read()
            .unwrap()
            .get(&media_id)
            .map(|item| item.path.clone())
    }

    fn seek_to_timecode(&mut self, tc: Timecode) {
        let secs = tc.to_secs_f64();
        self.seek_mpv(secs);
        self.playhead = tc;
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
        self.last_frame_time = std::time::Instant::now();
        self.grab_frame();
    }

    fn seek_mpv(&mut self, secs: f64) {
        self.mpv
            .command("seek", &[&format!("{secs}"), "absolute"])
            .unwrap_or_else(|e| error!("mpv seek: {e}"));
        std::thread::sleep(Duration::from_millis(10));
    }

    fn wait_for_file_loaded(&mut self) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                warn!("wait_for_file_loaded: timed out after 5s");
                return false;
            }
            let timeout_secs = remaining.as_secs_f64().min(0.1);
            match self.mpv.wait_event(timeout_secs) {
                Some(Ok(libmpv2::events::Event::FileLoaded)) => {
                    return true;
                }
                Some(Ok(libmpv2::events::Event::EndFile(_))) => {
                    continue;
                }
                None => {
                    continue;
                }
                _ => {
                    continue;
                }
            }
        }
    }

    fn load_clip_file(&mut self, path: &std::path::Path) {
        let path_str = path.to_string_lossy().to_string();
        debug!("load_clip_file: loading {:?}", path);
        if let Err(e) = self.mpv.command("loadfile", &[&path_str, "replace"]) {
            error!("Failed to load clip file: {e}");
            return;
        }
        self.current_file = Some(path.to_path_buf());
        let loaded = self.wait_for_file_loaded();
        debug!("load_clip_file: wait_for_file_loaded returned {loaded}");
        // Give mpv time to decode the first frame after file is loaded
        std::thread::sleep(Duration::from_millis(50));
    }

    fn load_file(&mut self, path: PathBuf, media_id: MediaId) {
        info!("Loading media: {:?}", path);

        let path_str = path.to_string_lossy().to_string();
        if let Err(e) = self.mpv.command("loadfile", &[&path_str, "replace"]) {
            error!("Failed to load media: {e}");
            let _ = self.event_tx.send(AppEvent::MediaLoadFailed {
                media_id,
                error: e.to_string(),
            });
            (self.repaint_cb)();
            return;
        }

        if !self.wait_for_file_loaded() {
            error!("Failed to load media (timeout waiting for FileLoaded)");
            let _ = self.event_tx.send(AppEvent::MediaLoadFailed {
                media_id,
                error: "Timeout waiting for file to load".to_string(),
            });
            (self.repaint_cb)();
            return;
        }

        // Give mpv time to decode the first frame so properties and screenshots work
        std::thread::sleep(Duration::from_millis(50));

        let duration_secs: f64 = self.mpv.get_property("duration").unwrap_or(0.0);
        let video_w: i64 = self.mpv.get_property("width").unwrap_or(1920);
        let video_h: i64 = self.mpv.get_property("height").unwrap_or(1080);
        let container_fps: f64 = self.mpv.get_property("container-fps").unwrap_or(24.0);

        let video_codec: String = self
            .mpv
            .get_property("video-codec")
            .unwrap_or_else(|_| String::new());
        let audio_codec: String = self
            .mpv
            .get_property("audio-codec")
            .unwrap_or_else(|_| String::new());
        let has_audio = !audio_codec.is_empty();

        let file_size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        self.fps = fps_from_float(container_fps);
        self.duration = RationalTime::from_secs_f64(duration_secs, self.fps.rate());
        self.playhead = Timecode::zero();
        self.current_file = Some(path.clone());
        self.current_clip_index = 0;

        let resolution = Resolution {
            width: video_w as u32,
            height: video_h as u32,
        };

        self.state = PlaybackState::Stopped;

        let _ = self.event_tx.send(AppEvent::MediaLoaded {
            media_id,
            path,
            duration: self.duration,
            fps: self.fps,
            resolution,
            video_codec,
            audio_codec,
            has_audio,
            file_size_bytes,
        });

        self.grab_frame();

        info!(
            "Media loaded: {}x{} @ {:.3} fps, duration {:.2}s",
            video_w, video_h, container_fps, duration_secs
        );
    }

    fn update_playback(&mut self) {
        // Poll mpv events (non-blocking) to detect EndFile
        while let Some(ev) = self.mpv.wait_event(0.0) {
            match ev {
                Ok(libmpv2::events::Event::EndFile(_reason)) => {
                    debug!("EndFile event received — advancing to next clip");
                    self.advance_to_next_clip();
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    debug!("mpv event error: {e:?}");
                }
            }
        }

        // Update playhead from mpv time-pos
        let time_pos: f64 = self.mpv.get_property("time-pos").unwrap_or(0.0);

        let seq = self.sequence.read().unwrap();
        let mut past_clip_end = false;
        if let Some(track) = seq.video_track() {
            if let Some(clip) = track.clips.get(self.current_clip_index) {
                let source_in_secs = clip.source_in.to_secs_f64();
                let source_out_secs = clip.source_out.to_secs_f64();

                // Check if mpv has played past the clip's source_out boundary
                if time_pos >= source_out_secs - 0.01 {
                    // We've reached the end of this clip's trimmed region
                    self.playhead = clip.timeline_out;
                    past_clip_end = true;
                    debug!(
                        "Clip {} reached source_out ({:.3}s), time_pos={:.3}s — advancing",
                        self.current_clip_index, source_out_secs, time_pos
                    );
                } else {
                    let elapsed_secs = (time_pos - source_in_secs).max(0.0);
                    let elapsed_tc = RationalTime::from_secs_f64(elapsed_secs, self.fps.rate());
                    self.playhead = clip.timeline_in + elapsed_tc;
                }
            } else {
                self.playhead = RationalTime::from_secs_f64(time_pos, self.fps.rate());
            }
        } else {
            self.playhead = RationalTime::from_secs_f64(time_pos, self.fps.rate());
        }
        drop(seq);

        if past_clip_end {
            self.advance_to_next_clip();
            return;
        }

        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));

        // Only grab a frame when enough time has elapsed for the next frame
        let frame_duration = Duration::from_secs_f64(self.fps.frame_duration_secs());
        let elapsed = self.last_frame_time.elapsed();
        if elapsed >= frame_duration {
            // Detect dropped frames
            if elapsed >= frame_duration * 2 {
                self.dropped_frames += 1;
            }
            self.last_frame_time = std::time::Instant::now();
            self.grab_frame();
        }
    }

    /// Called when mpv fires EndFile — load the next clip in the sequence
    /// or stop if we've reached the end.
    fn advance_to_next_clip(&mut self) {
        let seq = self.sequence.read().unwrap();
        let next_idx = self.current_clip_index + 1;
        let next_clip_info = seq.video_track().and_then(|track| {
            track.clips.get(next_idx).map(|clip| {
                (
                    clip.media_id,
                    clip.source_in.to_secs_f64(),
                    clip.timeline_in,
                )
            })
        });
        drop(seq);

        if let Some((media_id, source_in_secs, timeline_in)) = next_clip_info {
            self.current_clip_index = next_idx;
            self.playhead = timeline_in;

            if let Some(path) = self.path_for_media(media_id) {
                info!(
                    "Advancing to clip {} (media {:?})",
                    next_idx, path
                );
                self.load_clip_file(&path);
                // Seek to source_in of the next clip
                if source_in_secs > 0.01 {
                    self.mpv
                        .command(
                            "seek",
                            &[&format!("{source_in_secs}"), "absolute"],
                        )
                        .unwrap_or_else(|e| error!("mpv seek on advance: {e}"));
                    std::thread::sleep(Duration::from_millis(10));
                }
                // Resume playback
                self.mpv
                    .set_property("pause", "no")
                    .unwrap_or_else(|e| error!("mpv resume on advance: {e}"));
                self.last_frame_time = std::time::Instant::now();
                let _ = self
                    .event_tx
                    .send(AppEvent::PlayheadMoved(self.playhead));

                // Restart V2 decoder for new position
                self.stop_v2_decoder();
                self.maybe_start_v2_decoder();

                self.grab_frame();
            } else {
                warn!("No path found for media {:?} — stopping", media_id);
                self.stop_at_end();
            }
        } else {
            // No more clips — stop playback
            self.stop_at_end();
        }
    }

    fn stop_at_end(&mut self) {
        self.state = PlaybackState::Paused;
        self.mpv
            .set_property("pause", "yes")
            .unwrap_or_else(|e| error!("mpv pause at end: {e}"));
        self.stop_v2_decoder();
        let _ = self.event_tx.send(AppEvent::PlaybackPaused);
        (self.repaint_cb)();
    }

    /// Grab the current video frame via screenshot-to-file.
    /// If Video 2 has a clip at the current playhead, composites both layers.
    fn grab_frame(&mut self) {
        let width: i64 = self.mpv.get_property("width").unwrap_or(0);
        let height: i64 = self.mpv.get_property("height").unwrap_or(0);
        if width <= 0 || height <= 0 {
            return;
        }

        let tmp_dir = if std::path::Path::new("/dev/shm").exists() {
            PathBuf::from("/dev/shm")
        } else {
            std::env::temp_dir()
        };

        // Grab Video 1 frame
        let v1_rgba = match grab_mpv_frame(&self.mpv, &tmp_dir, "klipp_frame_v1") {
            Some(frame) => frame,
            None => return,
        };

        // Check if Video 2 has a clip at the current playhead
        let v2_info = {
            let seq = self.sequence.read().unwrap();
            seq.video_track2().and_then(|track| {
                if track.muted || track.clips.is_empty() {
                    return None;
                }
                track.clip_at_timecode(self.playhead).map(|(idx, clip)| {
                    let source_secs = clip.source_in.to_secs_f64()
                        + (self.playhead - clip.timeline_in).to_secs_f64();
                    (idx, clip.media_id, source_secs, track.opacity)
                })
            })
        };

        if let Some((clip_idx, media_id, source_secs, opacity)) = v2_info {
            let out_w = v1_rgba.1;
            let out_h = v1_rgba.2;
            let is_playing = self.state == PlaybackState::Playing;

            let v2_frame = if is_playing {
                // During playback: read from background ffmpeg decoder (non-blocking)
                // Make sure decoder is running for this clip
                if let Some(path) = self.path_for_media(media_id) {
                    self.start_v2_decoder(&path, source_secs, out_w, out_h, clip_idx);
                }
                // Read latest frame from shared buffer
                self.v2_shared_frame.lock().ok().and_then(|lock| lock.clone())
            } else {
                // Paused/stepping: grab a single frame via ffmpeg (blocking, but acceptable)
                if let Some(path) = self.path_for_media(media_id) {
                    grab_v2_frame_single(&path, source_secs, out_w, out_h)
                } else {
                    None
                }
            };

            if let Some(v2_data) = v2_frame {
                let layers = vec![
                    crate::composite::CompositeLayer {
                        rgba: v1_rgba.0,
                        width: v1_rgba.1,
                        height: v1_rgba.2,
                        opacity: 1.0,
                        blend_mode: klipp_core::BlendMode::Normal,
                    },
                    crate::composite::CompositeLayer {
                        rgba: v2_data.0,
                        width: v2_data.1,
                        height: v2_data.2,
                        opacity,
                        blend_mode: klipp_core::BlendMode::Normal,
                    },
                ];
                let composited = crate::composite::composite_layers(&layers, out_w, out_h);
                let mut fb = self.frame_buffer.lock().unwrap();
                fb.write_frame(composited, out_w, out_h);
                let _ = self.event_tx.send(AppEvent::FrameReady);
                (self.repaint_cb)();
                return;
            }
        } else {
            // No V2 clip at playhead — stop decoder if running
            if self.v2_decoder_handle.is_some() {
                self.stop_v2_decoder();
            }
        }

        // No V2 clip or no V2 frame available — write V1 directly
        let mut fb = self.frame_buffer.lock().unwrap();
        fb.write_frame(v1_rgba.0, v1_rgba.1, v1_rgba.2);
        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
    }
}

/// Grab a single V2 frame via one-shot ffmpeg. Used when paused/stepping.
fn grab_v2_frame_single(
    path: &std::path::Path,
    seek_secs: f64,
    out_w: u32,
    out_h: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    let output = Command::new("ffmpeg")
        .args([
            "-ss",
            &format!("{seek_secs:.4}"),
            "-i",
            &path.to_string_lossy(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{out_w}x{out_h}"),
            "-frames:v",
            "1",
            "-v",
            "error",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    let frame_size = (out_w * out_h * 4) as usize;
    if output.stdout.len() == frame_size {
        Some((output.stdout, out_w, out_h))
    } else {
        debug!(
            "grab_v2_frame_single: expected {} bytes, got {}",
            frame_size,
            output.stdout.len()
        );
        None
    }
}

/// Grab a frame from an mpv instance via screenshot-to-file. Returns (rgba, width, height).
fn grab_mpv_frame(mpv: &libmpv2::Mpv, tmp_dir: &std::path::Path, name: &str) -> Option<(Vec<u8>, u32, u32)> {
    let _ = mpv.set_property("screenshot-format", "png");
    let tmp_path = tmp_dir.join(format!("{name}.png"));
    let tmp_str = tmp_path.to_string_lossy().to_string();

    match mpv.command("screenshot-to-file", &[&tmp_str, "video"]) {
        Ok(_) => {}
        Err(e) => {
            debug!("grab_mpv_frame: screenshot-to-file failed: {e}");
            return None;
        }
    }

    let result = match image::open(&tmp_path) {
        Ok(img) => {
            let rgba_img = img.into_rgba8();
            let (w, h) = (rgba_img.width(), rgba_img.height());
            Some((rgba_img.into_raw(), w, h))
        }
        Err(e) => {
            debug!("grab_mpv_frame: image::open failed: {e}");
            None
        }
    };

    let _ = std::fs::remove_file(&tmp_path);
    result
}

fn fps_from_float(fps: f64) -> Fps {
    if (fps - 23.976).abs() < 0.01 {
        Fps::ntsc_23976()
    } else if (fps - 24.0).abs() < 0.01 {
        Fps::film_24()
    } else if (fps - 25.0).abs() < 0.01 {
        Fps::pal_25()
    } else if (fps - 29.97).abs() < 0.01 {
        Fps::ntsc_29970()
    } else if (fps - 30.0).abs() < 0.01 {
        Fps::new(30, 1)
    } else if (fps - 50.0).abs() < 0.01 {
        Fps::new(50, 1)
    } else if (fps - 59.94).abs() < 0.01 {
        Fps::new(60000, 1001)
    } else if (fps - 60.0).abs() < 0.01 {
        Fps::new(60, 1)
    } else {
        Fps::new(fps.round() as u32, 1)
    }
}

/// Generate a thumbnail for a media file using a short-lived mpv instance.
pub fn generate_thumbnail(
    path: &std::path::Path,
    seek_secs: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mpv = libmpv2::Mpv::new().map_err(|e| format!("mpv init: {e}"))?;
    mpv.set_property("vo", "null")
        .map_err(|e| format!("set vo: {e}"))?;
    mpv.set_property("hwdec", "no")
        .map_err(|e| format!("set hwdec: {e}"))?;
    mpv.set_property("aid", "no")
        .map_err(|e| format!("set aid: {e}"))?;
    mpv.set_property("pause", "yes")
        .map_err(|e| format!("set pause: {e}"))?;
    mpv.set_property("keep-open", "yes")
        .map_err(|e| format!("set keep-open: {e}"))?;

    let path_str = path.to_string_lossy().to_string();
    mpv.command("loadfile", &[&path_str, "replace"])
        .map_err(|e| format!("loadfile: {e}"))?;

    std::thread::sleep(Duration::from_millis(500));

    if seek_secs > 0.5 {
        mpv.command("seek", &[&format!("{seek_secs}"), "absolute"])
            .map_err(|e| format!("seek: {e}"))?;
        std::thread::sleep(Duration::from_millis(100));
    }

    let _ = mpv.set_property("screenshot-format", "png");
    let tmp_dir = if std::path::Path::new("/dev/shm").exists() {
        PathBuf::from("/dev/shm")
    } else {
        std::env::temp_dir()
    };
    let tmp_path = tmp_dir.join(format!("klipp_thumb_{}.png", std::process::id()));
    let tmp_str = tmp_path.to_string_lossy().to_string();

    mpv.command("screenshot-to-file", &[&tmp_str, "video"])
        .map_err(|e| format!("screenshot: {e}"))?;

    let img = image::open(&tmp_path).map_err(|e| format!("image open: {e}"))?;
    let _ = std::fs::remove_file(&tmp_path);

    let rgba_img = img.into_rgba8();
    let (w, h) = (rgba_img.width(), rgba_img.height());

    let scale = (160.0 / w as f64).min(90.0 / h as f64).min(1.0);
    let new_w = (w as f64 * scale) as u32;
    let new_h = (h as f64 * scale) as u32;

    let resized = image::imageops::resize(
        &rgba_img,
        new_w,
        new_h,
        image::imageops::FilterType::Triangle,
    );
    let (rw, rh) = (resized.width(), resized.height());
    let rgba = resized.into_raw();

    Ok((rgba, rw, rh))
}
