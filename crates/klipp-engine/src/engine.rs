use crate::texture::SharedFrameTexture;
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{AppCommand, AppEvent, Fps, MediaId, Resolution, Timecode};
use std::path::PathBuf;
use std::sync::Arc;
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
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    frame_texture: SharedFrameTexture,
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
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        frame_texture: SharedFrameTexture,
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
            device,
            queue,
            frame_texture,
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
                if self.state == PlaybackState::Paused || self.state == PlaybackState::Stopped {
                    self.mpv
                        .set_property("pause", "no")
                        .unwrap_or_else(|e| error!("mpv set pause=no: {e}"));
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
        // Give mpv a moment to seek, then grab the frame
        std::thread::sleep(Duration::from_millis(10));
        self.grab_and_upload_frame();
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

        // Query duration and fps from mpv
        let duration_secs: f64 = self
            .mpv
            .get_property("duration")
            .unwrap_or(0.0);
        let video_w: i64 = self.mpv.get_property("width").unwrap_or(1920);
        let video_h: i64 = self.mpv.get_property("height").unwrap_or(1080);
        let container_fps: f64 = self.mpv.get_property("container-fps").unwrap_or(24.0);

        // Determine rational fps
        self.fps = fps_from_float(container_fps);
        self.duration = Timecode::from_secs(duration_secs, &self.fps);
        self.playhead = Timecode::zero();
        self.current_file = Some(path);

        let resolution = Resolution {
            width: video_w as u32,
            height: video_h as u32,
        };

        // Resize texture if needed
        {
            let mut ft = self.frame_texture.lock().unwrap();
            ft.resize(&self.device, resolution.width, resolution.height);
        }

        self.state = PlaybackState::Stopped;

        let _ = self.event_tx.send(AppEvent::MediaLoaded {
            media_id,
            duration: self.duration,
            fps: self.fps,
            resolution,
        });

        // Grab first frame
        self.grab_and_upload_frame();

        info!(
            "Media loaded: {}x{} @ {:.3} fps, duration {:.2}s",
            video_w, video_h, container_fps, duration_secs
        );
    }

    fn update_playback(&mut self) {
        // Update playhead from mpv's current position
        let time_pos: f64 = self.mpv.get_property("time-pos").unwrap_or(0.0);
        self.playhead = Timecode::from_secs(time_pos, &self.fps);
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));

        // Check if playback has reached the end
        let eof: bool = self
            .mpv
            .get_property("eof-reached")
            .unwrap_or(false);
        if eof {
            self.state = PlaybackState::Paused;
            let _ = self.event_tx.send(AppEvent::PlaybackPaused);
        }

        self.grab_and_upload_frame();
    }

    fn grab_and_upload_frame(&mut self) {
        // Use mpv screenshot-raw to grab the current frame as RGBA
        // libmpv2 doesn't expose a direct frame callback easily,
        // so we use the screenshot command to capture to a byte buffer.
        // For Phase 1, this is acceptable — Phase 2 will use render API.

        let width: i64 = self.mpv.get_property("width").unwrap_or(0);
        let height: i64 = self.mpv.get_property("height").unwrap_or(0);
        if width <= 0 || height <= 0 {
            return;
        }

        // Use mpv's screenshot-to-file with a tempfile, then read it back.
        // This is the simplest approach that works with libmpv2's API.
        let tmp_path = std::env::temp_dir().join("klipp_frame.bmp");
        let tmp_str = tmp_path.to_string_lossy().to_string();

        if self
            .mpv
            .command("screenshot-to-file", &[&tmp_str, "video"])
            .is_err()
        {
            return;
        }

        // Read the BMP and convert to RGBA
        if let Ok(data) = std::fs::read(&tmp_path) {
            if let Some(rgba) = decode_bmp_to_rgba(&data, width as u32, height as u32) {
                let mut ft = self.frame_texture.lock().unwrap();
                ft.resize(&self.device, width as u32, height as u32);
                ft.upload_frame(&self.queue, &rgba);
                let _ = self.event_tx.send(AppEvent::FrameReady);
                (self.repaint_cb)();
                debug!("Frame uploaded to GPU, playhead={:?}", self.playhead);
            }
        }

        let _ = std::fs::remove_file(&tmp_path);
    }
}

fn fps_from_float(fps: f64) -> Fps {
    // Match common frame rates to their rational representations
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

fn decode_bmp_to_rgba(data: &[u8], expected_w: u32, expected_h: u32) -> Option<Vec<u8>> {
    // Minimal BMP parser — BMP header is 14 bytes, DIB header follows
    if data.len() < 54 {
        return None;
    }
    if &data[0..2] != b"BM" {
        return None;
    }

    let pixel_offset = u32::from_le_bytes(data[10..14].try_into().ok()?) as usize;
    let width = i32::from_le_bytes(data[18..22].try_into().ok()?) as u32;
    let height_raw = i32::from_le_bytes(data[22..26].try_into().ok()?);
    let bpp = u16::from_le_bytes(data[28..30].try_into().ok()?);

    if width != expected_w {
        return None;
    }
    let height = height_raw.unsigned_abs();
    if height != expected_h {
        return None;
    }
    let bottom_up = height_raw > 0;

    let bytes_per_pixel = (bpp / 8) as usize;
    if bytes_per_pixel < 3 {
        return None;
    }

    // BMP rows are padded to 4-byte boundaries
    let row_size = ((bytes_per_pixel as u32 * width + 3) & !3) as usize;
    let pixel_data = &data[pixel_offset..];

    let mut rgba = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        let src_y = if bottom_up { height - 1 - y } else { y };
        let src_row_start = src_y as usize * row_size;
        let dst_row_start = y as usize * width as usize * 4;

        for x in 0..width as usize {
            let src_offset = src_row_start + x * bytes_per_pixel;
            let dst_offset = dst_row_start + x * 4;

            if src_offset + 2 >= pixel_data.len() {
                break;
            }

            // BMP stores BGR(A)
            rgba[dst_offset] = pixel_data[src_offset + 2]; // R
            rgba[dst_offset + 1] = pixel_data[src_offset + 1]; // G
            rgba[dst_offset + 2] = pixel_data[src_offset]; // B
            rgba[dst_offset + 3] = if bytes_per_pixel >= 4 {
                pixel_data[src_offset + 3]
            } else {
                255
            };
        }
    }

    Some(rgba)
}
