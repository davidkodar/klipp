use crate::texture::SharedFrameBuffer;
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{AppCommand, AppEvent, Fps, MediaId, RationalTime, Resolution, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tracing::{debug, error, info, warn};

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
            | AppCommand::OpenProject { .. } => {}
        }

        false
    }

    fn handle_play(&mut self) {
        let seq = self.sequence.read().unwrap();
        let clip_count = seq.video_track().map(|t| t.clips.len()).unwrap_or(0);
        let clip_info = seq.video_track().and_then(|track| {
            track.clips.first().map(|clip| {
                (clip.media_id, clip.source_in.to_secs_f64(), clip.timeline_in)
            })
        });
        drop(seq);

        info!("handle_play: sequence has {clip_count} clips");

        if let Some((media_id, source_in_secs, timeline_in)) = clip_info {
            if let Some(path) = self.path_for_media(media_id) {
                info!("handle_play: loading clip 0 from {:?}", path);
                self.load_clip_file(&path);
                self.current_clip_index = 0;
                if source_in_secs > 0.01 {
                    self.seek_mpv(source_in_secs);
                }
                self.playhead = timeline_in;
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
        let _ = self.event_tx.send(AppEvent::PlaybackStarted);
        (self.repaint_cb)();
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
                    // Ignore — this is the previous clip ending, not the new one failing.
                    // Keep looping and wait for FileLoaded.
                    continue;
                }
                None => {
                    // Timeout tick — keep waiting
                    continue;
                }
                _ => {
                    // Any other event — keep waiting
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
        if let Some(track) = seq.video_track() {
            if let Some(clip) = track.clips.get(self.current_clip_index) {
                let source_in_secs = clip.source_in.to_secs_f64();
                let elapsed_secs = (time_pos - source_in_secs).max(0.0);
                let elapsed_tc = RationalTime::from_secs_f64(elapsed_secs, self.fps.rate());
                self.playhead = clip.timeline_in + elapsed_tc;
            } else {
                self.playhead = RationalTime::from_secs_f64(time_pos, self.fps.rate());
            }
        } else {
            self.playhead = RationalTime::from_secs_f64(time_pos, self.fps.rate());
        }
        drop(seq);

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
        let _ = self.event_tx.send(AppEvent::PlaybackPaused);
        (self.repaint_cb)();
    }

    /// Grab the current video frame via screenshot-to-file.
    ///
    /// NOTE: screenshot-raw (which returns pixel data directly without disk I/O)
    /// is not available because libmpv2 v5 does not wrap mpv_command_node.
    /// The sys crate also doesn't expose it, so even raw FFI isn't possible.
    /// Using /dev/shm minimizes the I/O cost of the file-based approach.
    fn grab_frame(&mut self) {
        let width: i64 = self.mpv.get_property("width").unwrap_or(0);
        let height: i64 = self.mpv.get_property("height").unwrap_or(0);
        if width <= 0 || height <= 0 {
            return;
        }

        let _ = self.mpv.set_property("screenshot-format", "png");

        let tmp_dir = if std::path::Path::new("/dev/shm").exists() {
            PathBuf::from("/dev/shm")
        } else {
            std::env::temp_dir()
        };
        let tmp_path = tmp_dir.join("klipp_frame.png");
        let tmp_str = tmp_path.to_string_lossy().to_string();

        match self
            .mpv
            .command("screenshot-to-file", &[&tmp_str, "video"])
        {
            Ok(_) => {}
            Err(e) => {
                debug!("grab_frame: screenshot-to-file failed: {e}");
                return;
            }
        }

        match image::open(&tmp_path) {
            Ok(img) => {
                let rgba_img = img.into_rgba8();
                let (img_w, img_h) = (rgba_img.width(), rgba_img.height());
                let rgba = rgba_img.into_raw();
                let mut fb = self.frame_buffer.lock().unwrap();
                fb.write_frame(rgba, img_w, img_h);
                let _ = self.event_tx.send(AppEvent::FrameReady);
                (self.repaint_cb)();
            }
            Err(e) => {
                debug!("grab_frame: image::open failed: {e}");
            }
        }

        let _ = std::fs::remove_file(&tmp_path);
    }
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
