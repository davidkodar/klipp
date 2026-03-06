use crate::texture::SharedFrameBuffer;
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{AppCommand, AppEvent, Fps, MediaId, Resolution, Timecode};
use std::path::PathBuf;
use std::time::Duration;
use tracing::{debug, error, info};

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
    duration: Timecode,
    fps: Fps,
    playhead: Timecode,
    repaint_cb: Box<dyn Fn() + Send>,
}

impl PlaybackEngine {
    pub fn new(
        cmd_rx: Receiver<AppCommand>,
        event_tx: Sender<AppEvent>,
        frame_buffer: SharedFrameBuffer,
        repaint_cb: Box<dyn Fn() + Send>,
    ) -> Result<Self, klipp_core::KlippError> {
        let mpv =
            libmpv2::Mpv::new().map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;

        mpv.set_property("vo", "null")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;
        mpv.set_property("hwdec", "no")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;
        mpv.set_property("aid", "no")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;
        mpv.set_property("pause", "yes")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;
        mpv.set_property("keep-open", "yes")
            .map_err(|e| klipp_core::KlippError::Decode(e.to_string()))?;

        Ok(Self {
            cmd_rx,
            event_tx,
            mpv,
            frame_buffer,
            state: PlaybackState::Idle,
            current_file: None,
            duration: Timecode::zero(),
            fps: Fps::film_24(),
            playhead: Timecode::zero(),
            repaint_cb,
        })
    }

    pub fn run(mut self) {
        info!("Engine thread starting");

        loop {
            let timeout = if self.state == PlaybackState::Playing {
                Duration::from_millis((self.fps.frame_duration_secs() * 1000.0) as u64)
            } else {
                Duration::from_millis(50)
            };

            match self.cmd_rx.recv_timeout(timeout) {
                Ok(cmd) => {
                    if self.handle_command(cmd) {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }

            // Drain any additional pending commands
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                if self.handle_command(cmd) {
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
                    let result = self.mpv.set_property("pause", "no");
                    debug!("mpv set pause=no result: {:?}", result);
                    result.unwrap_or_else(|e| error!("mpv set pause=no: {e}"));
                    self.state = PlaybackState::Playing;
                    let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                    (self.repaint_cb)();
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
                    self.seek_to_timecode(tc);
                }
            }

            AppCommand::SeekRelative(frames) => {
                if self.state != PlaybackState::Idle {
                    let new_tc = Timecode(self.playhead.0 + frames);
                    let clamped = Timecode(new_tc.0.max(0).min(self.duration.0));
                    self.seek_to_timecode(clamped);
                }
            }

            AppCommand::SetPlayhead(tc) => {
                if self.state != PlaybackState::Idle {
                    self.seek_to_timecode(tc);
                }
            }
        }

        false
    }

    fn seek_to_timecode(&mut self, tc: Timecode) {
        let secs = tc.to_secs(&self.fps);
        self.mpv
            .command("seek", &[&format!("{secs}"), "absolute"])
            .unwrap_or_else(|e| error!("mpv seek: {e}"));
        self.playhead = tc;
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
        std::thread::sleep(Duration::from_millis(10));
        self.grab_frame();
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

        // Wait for the file to load
        std::thread::sleep(Duration::from_millis(500));

        let duration_secs: f64 = self.mpv.get_property("duration").unwrap_or(0.0);
        let video_w: i64 = self.mpv.get_property("width").unwrap_or(1920);
        let video_h: i64 = self.mpv.get_property("height").unwrap_or(1080);
        let container_fps: f64 = self.mpv.get_property("container-fps").unwrap_or(24.0);

        self.fps = fps_from_float(container_fps);
        self.duration = Timecode::from_secs(duration_secs, &self.fps);
        self.playhead = Timecode::zero();
        self.current_file = Some(path);

        let resolution = Resolution {
            width: video_w as u32,
            height: video_h as u32,
        };

        self.state = PlaybackState::Stopped;

        let _ = self.event_tx.send(AppEvent::MediaLoaded {
            media_id,
            duration: self.duration,
            fps: self.fps,
            resolution,
        });

        self.grab_frame();

        info!(
            "Media loaded: {}x{} @ {:.3} fps, duration {:.2}s",
            video_w, video_h, container_fps, duration_secs
        );
    }

    fn update_playback(&mut self) {
        let time_pos_result: Result<f64, _> = self.mpv.get_property("time-pos");
        debug!("update_playback: time-pos={:?}", time_pos_result);
        let time_pos = time_pos_result.unwrap_or(0.0);
        self.playhead = Timecode::from_secs(time_pos, &self.fps);
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));

        let eof: bool = self.mpv.get_property("eof-reached").unwrap_or(false);
        if eof {
            debug!("update_playback: EOF reached");
            self.state = PlaybackState::Paused;
            let _ = self.event_tx.send(AppEvent::PlaybackPaused);
        }

        self.grab_frame();
    }

    fn grab_frame(&mut self) {
        let width: i64 = self.mpv.get_property("width").unwrap_or(0);
        let height: i64 = self.mpv.get_property("height").unwrap_or(0);
        if width <= 0 || height <= 0 {
            debug!("grab_frame: invalid dimensions {}x{}", width, height);
            return;
        }

        let _ = self.mpv.set_property("screenshot-format", "png");
        let tmp_path = std::env::temp_dir().join("klipp_frame.png");
        let tmp_str = tmp_path.to_string_lossy().to_string();

        match self
            .mpv
            .command("screenshot-to-file", &[&tmp_str, "video"])
        {
            Ok(_) => {
                debug!("grab_frame: screenshot-to-file succeeded");
            }
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
                debug!("grab_frame: decoded {}x{}, {} bytes", img_w, img_h, rgba.len());
                let mut fb = self.frame_buffer.lock().unwrap();
                fb.write_frame(rgba, img_w, img_h);
                let _ = self.event_tx.send(AppEvent::FrameReady);
                (self.repaint_cb)();
                debug!("Frame written to buffer, playhead={:?}", self.playhead);
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
