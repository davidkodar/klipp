use crate::audio_decoder::AudioDecoder;
use crate::audio_output::AudioOutput;
use crate::audio_source::AudioSource;
use crate::pcm_reader::PcmAudioFeeder;
use crate::sync::MasterClock;
use crate::texture::SharedFrameBuffer;
use crate::video_decoder::VideoDecoder;

/// Wrapper to make AudioOutput Send. The cpal::Stream inside is !Send
/// due to platform internals, but we only ever use it on the engine thread.
struct SendAudioOutput(#[allow(dead_code)] AudioOutput);
unsafe impl Send for SendAudioOutput {}
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{AppCommand, AppEvent, Fps, MediaId, RationalTime, Resolution, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
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
    // Video decoders
    v1_decoder: Option<VideoDecoder>,
    v2_decoder: Option<VideoDecoder>,
    current_clip_v2: Option<usize>,
    v2_frame_cache: Option<crate::types::DecodedFrame>,
    // Audio — AudioSource wraps either AudioDecoder (ffmpeg) or PcmAudioFeeder (cache)
    audio_source: Option<AudioSource>,
    audio_output: Option<SendAudioOutput>,
    cpal_sample_rate: u32,
    // Master clock — single source of truth for A/V timing
    master_clock: Arc<MasterClock>,
    // Timeline offset: maps source PTS to sequence timeline.
    clip_source_start: f64, // source seconds at which playback started for current clip
    clip_timeline_start: Timecode, // timeline position of clip_source_start
    // Audio seek synchronization — generation counter
    expected_audio_generation: u64,
    // Performance
    use_hw_decode: bool,
    last_stats_time: std::time::Instant,
    frames_consumed: u64,
    last_playback_log: std::time::Instant,
    // Color pipeline — post-processing on RGBA frames before display
    color_pipeline: klipp_color::ColorPipeline,
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
        info!("PlaybackEngine: initializing (ffmpeg-next backend)");

        // Probe cpal sample rate (don't start the stream yet — we start it per-clip)
        let cpal_sample_rate = {
            use cpal::traits::{DeviceTrait, HostTrait};
            let host = cpal::default_host();
            host.default_output_device()
                .and_then(|d| d.default_output_config().ok())
                .map(|c| c.sample_rate().0)
                .unwrap_or(48000)
        };
        info!("PlaybackEngine: cpal sample rate = {cpal_sample_rate}");

        // Probe VAAPI availability once
        let use_hw_decode = unsafe {
            use ffmpeg_next::ffi::*;
            let device_type = av_hwdevice_find_type_by_name(c"vaapi".as_ptr());
            device_type != AVHWDeviceType::AV_HWDEVICE_TYPE_NONE
        };
        info!("PlaybackEngine: VAAPI available = {use_hw_decode}");

        Ok(Self {
            cmd_rx,
            event_tx,
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
            v1_decoder: None,
            v2_decoder: None,
            current_clip_v2: None,
            v2_frame_cache: None,
            audio_source: None,
            audio_output: None,
            cpal_sample_rate,
            master_clock: Arc::new(MasterClock::new()),
            clip_source_start: 0.0,
            clip_timeline_start: Timecode::zero(),
            expected_audio_generation: 0,
            use_hw_decode: false,
            last_stats_time: std::time::Instant::now(),
            frames_consumed: 0,
            last_playback_log: std::time::Instant::now(),
            color_pipeline: klipp_color::ColorPipeline::new(),
        })
    }

    pub fn run(mut self) {
        info!("Engine thread starting");

        loop {
            let timeout = if self.state == PlaybackState::Playing {
                Duration::from_millis(1)
            } else {
                Duration::from_millis(50)
            };

            // Collect all pending commands, coalescing seeks
            let mut commands = Vec::new();
            match self.cmd_rx.recv_timeout(timeout) {
                Ok(cmd) => commands.push(cmd),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                commands.push(cmd);
            }

            // Coalesce: keep only the last seek/setplayhead, drop earlier ones
            let commands = coalesce_commands(commands);

            for cmd in commands {
                if self.handle_command(cmd) {
                    info!("Engine thread shutting down");
                    return;
                }
            }

            if self.state == PlaybackState::Playing {
                self.update_playback();
            }

            // Emit perf stats periodically
            if self.last_stats_time.elapsed() >= Duration::from_millis(500) {
                self.last_stats_time = std::time::Instant::now();
                // Check actual HW decode status from decoder
                let hw = self.v1_decoder.as_ref()
                    .map(|d| d.hw_active.load(Ordering::Relaxed))
                    .unwrap_or(self.use_hw_decode);
                let _ = self.event_tx.send(AppEvent::EngineStats {
                    dropped_frames: self.dropped_frames,
                    hw_decode: hw,
                });
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
                debug!("Play command received, state={:?}", self.state);
                if self.state == PlaybackState::Paused && self.v1_decoder.is_some() {
                    // Resume existing decoders — no need to reopen
                    if let Some(dec) = &self.v1_decoder {
                        dec.resume();
                    }
                    if let Some(dec) = &self.v2_decoder {
                        dec.resume();
                    }
                    // Flush stale audio, seek to current position
                    let source_secs = self.clip_source_start
                        + (self.playhead - self.clip_timeline_start).to_secs_f64();
                    if let Some(aout) = &self.audio_output {
                        aout.0.start_flush();
                    }
                    self.seek_audio_and_wait(source_secs);
                    if let Some(asrc) = &self.audio_source {
                        asrc.resume();
                    }
                    // Set master clock to current position and resume
                    let source_us = (source_secs * 1_000_000.0) as u64;
                    self.master_clock.seek(source_us);
                    if let Some(aout) = &self.audio_output {
                        aout.0.resume();
                    }
                    self.master_clock.resume();
                    self.state = PlaybackState::Playing;
                    self.last_frame_time = std::time::Instant::now();
                    let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                    (self.repaint_cb)();
                } else if self.state == PlaybackState::Paused
                    || self.state == PlaybackState::Stopped
                {
                    self.handle_play();
                }
            }

            AppCommand::Pause => {
                if self.state == PlaybackState::Playing {
                    if let Some(dec) = &self.v1_decoder {
                        dec.pause();
                    }
                    if let Some(dec) = &self.v2_decoder {
                        dec.pause();
                    }
                    self.master_clock.pause();
                    if let Some(adec) = &self.audio_source {
                        adec.pause();
                    }
                    if let Some(aout) = &self.audio_output {
                        aout.0.pause();
                    }
                    self.state = PlaybackState::Paused;
                    let _ = self.event_tx.send(AppEvent::PlaybackPaused);
                    (self.repaint_cb)();
                }
            }

            AppCommand::Stop => {
                if self.state != PlaybackState::Idle {
                    self.master_clock.pause();
                    self.v1_decoder = None;
                    self.v2_decoder = None;
                    self.current_clip_v2 = None;
                    self.v2_frame_cache = None;
                    self.audio_source = None;
                    self.audio_output = None;
                    self.playhead = Timecode::zero();
                    self.state = PlaybackState::Stopped;
                    let _ = self.event_tx.send(AppEvent::PlaybackStopped);
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    (self.repaint_cb)();
                }
            }

            AppCommand::SeekTo(tc) => {
                if self.state != PlaybackState::Idle {
                    self.seek_in_sequence(tc);
                }
            }

            AppCommand::SetSpeed(_speed) => {
                debug!("SetSpeed({_speed}) — not yet implemented");
            }

            AppCommand::SeekRelative(frames) => {
                if self.state != PlaybackState::Idle {
                    if self.state != PlaybackState::Playing
                        && frames.abs() <= 1
                        && self.v1_decoder.is_some()
                    {
                        // Single frame step: just grab next frame without seeking
                        self.step_frame(frames);
                    } else {
                        let seq_dur = self.sequence_duration();
                        let new_tc = RationalTime {
                            value: self.playhead.value + frames,
                            rate: self.playhead.rate,
                        };
                        let clamped = RationalTime {
                            value: new_tc.value.max(0).min(seq_dur.value),
                            rate: new_tc.rate,
                        };
                        self.seek_in_sequence(clamped);
                    }
                }
            }

            AppCommand::SetPlayhead(tc) => {
                if self.state != PlaybackState::Idle {
                    self.seek_in_sequence(tc);
                }
            }

            AppCommand::LoadGlobalLut { path } => {
                match self.color_pipeline.load_global_lut(&path) {
                    Ok(()) => info!("Loaded global LUT: {}", path.display()),
                    Err(e) => error!("Failed to load LUT {}: {}", path.display(), e),
                }
            }
            AppCommand::ClearGlobalLut => {
                self.color_pipeline.clear_global_lut();
                info!("Cleared global LUT");
            }
            AppCommand::SetGlobalLutOpacity(opacity) => {
                self.color_pipeline.global_lut_opacity = opacity;
            }

            // UI-side commands — engine ignores them
            _ => {}
        }

        false
    }

    fn handle_play(&mut self) {
        let seq = self.sequence.read().unwrap();
        let clip_info = seq.video_track().and_then(|track| {
            if let Some((idx, clip)) = track.clip_at_timecode(self.playhead) {
                let source_secs = clip.source_in.to_secs_f64()
                    + (self.playhead - clip.timeline_in).to_secs_f64();
                return Some((idx, clip.media_id, source_secs, clip.timeline_in));
            }
            for (idx, clip) in track.clips.iter().enumerate() {
                if clip.timeline_in >= self.playhead {
                    return Some((idx, clip.media_id, clip.source_in.to_secs_f64(), clip.timeline_in));
                }
            }
            track.clips.first().map(|clip| {
                (0, clip.media_id, clip.source_in.to_secs_f64(), clip.timeline_in)
            })
        });
        drop(seq);

        if let Some((clip_idx, media_id, source_secs, timeline_in)) = clip_info {
            if let Some(path) = self.path_for_media(media_id) {
                info!("handle_play: clip {clip_idx} from {:?}, source={source_secs:.3}s", path);

                // Open video decoder
                match VideoDecoder::open(&path) {
                    Ok(dec) => {
                        if source_secs > 0.01 {
                            dec.seek(source_secs);
                        }
                        dec.resume();
                        self.v1_decoder = Some(dec);
                    }
                    Err(e) => {
                        error!("Failed to open video decoder: {e}");
                        return;
                    }
                }

                // Open audio decoder + output
                self.start_audio(media_id, &path, source_secs);

                // Set master clock to source position and start it
                let source_us = (source_secs * 1_000_000.0) as u64;
                self.master_clock.seek(source_us);
                self.master_clock.resume();

                self.clip_source_start = source_secs;
                self.clip_timeline_start = timeline_in;
                self.current_clip_index = clip_idx;
                self.current_file = Some(path);
                self.state = PlaybackState::Playing;

                // Open V2 decoder if there's a clip on Video 2 at the playhead
                self.maybe_open_v2_decoder(self.playhead);
                self.dropped_frames = 0;
                self.frames_consumed = 0;
                self.last_playback_log = std::time::Instant::now();
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                (self.repaint_cb)();
            } else {
                warn!("No path for media {:?}", media_id);
            }
        } else {
            debug!("No clips in sequence");
        }
    }

    fn start_audio(&mut self, media_id: MediaId, path: &std::path::Path, source_secs: f64) {
        // Drop previous audio
        self.audio_source = None;
        self.audio_output = None;

        // Try PCM cache first (instant seeks, zero codec overhead)
        let pcm_path = self.pcm_cache_path(media_id);
        if let Some(ref pcm) = pcm_path {
            if pcm.exists() {
                match PcmAudioFeeder::open(pcm, self.cpal_sample_rate) {
                    Ok((feeder, consumer)) => {
                        if source_secs > 0.01 {
                            feeder.seek(source_secs);
                        }
                        feeder.resume();

                        match AudioOutput::new(consumer) {
                            Ok(output) => {
                                self.audio_output = Some(SendAudioOutput(output));
                            }
                            Err(e) => {
                                warn!("Failed to create audio output: {e}");
                            }
                        }
                        self.audio_source = Some(AudioSource::PcmFeeder(feeder));
                        return;
                    }
                    Err(e) => {
                        warn!("PCM feeder failed, falling back to decoder: {e}");
                    }
                }
            }
        }

        // Fallback: real-time ffmpeg decode
        match AudioDecoder::open(path, self.cpal_sample_rate) {
            Ok((adec, consumer)) => {
                if source_secs > 0.01 {
                    adec.seek(source_secs);
                }
                adec.resume();

                match AudioOutput::new(consumer) {
                    Ok(output) => {
                        self.audio_output = Some(SendAudioOutput(output));
                    }
                    Err(e) => {
                        warn!("Failed to create audio output: {e}");
                    }
                }
                self.audio_source = Some(AudioSource::Decoder(adec));
            }
            Err(e) => {
                debug!("No audio for this clip: {e}");
            }
        }
    }

    /// Look up the PCM cache path for a media item.
    fn pcm_cache_path(&self, media_id: MediaId) -> Option<std::path::PathBuf> {
        let pool = self.media_pool.read().ok()?;
        let item = pool.get(&media_id)?;
        item.audio_cache.as_ref().map(|c| c.pcm_path.clone())
    }

    /// Seek audio and wait for readiness using generation counter.
    /// With PCM feeder this is sub-millisecond; the timeout is a safety net
    /// for the AudioDecoder fallback path.
    fn seek_audio_and_wait(&mut self, source_secs: f64) {
        if let Some(asrc) = &self.audio_source {
            self.expected_audio_generation += 1;
            asrc.seek(source_secs);

            // Wait for audio to confirm seek (generation counter incremented)
            let deadline = std::time::Instant::now() + Duration::from_millis(50);
            loop {
                let gen = asrc.seek_generation().load(Ordering::Acquire);
                if gen >= self.expected_audio_generation {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    debug!(
                        "audio seek timeout: expected gen={}, got gen={} — proceeding anyway",
                        self.expected_audio_generation, gen
                    );
                    break;
                }
                std::thread::sleep(Duration::from_micros(100));
            }
        }
    }

    fn update_playback(&mut self) {
        let t_update = std::time::Instant::now();
        // Master clock is the single source of truth.
        let current_source_secs = self.master_clock.current_secs();
        // Frame presentation tolerance: half a frame duration (scales with FPS)
        let frame_tolerance = self.fps.frame_duration_secs() * 0.5;
        let _clock_running = self.master_clock.is_running();

        // Pop frames from the queue, presenting the one closest to current time
        let mut display_frame = None;
        let mut q_len = 0usize;

        if let Some(dec) = &self.v1_decoder {
            let mut queue = dec.frame_queue().lock().unwrap();
            q_len = queue.len();

            loop {
                match queue.front() {
                    None => break,
                    Some(f) if f.is_eof => {
                        let frame = queue.pop().unwrap();
                        display_frame = Some(frame);
                        break;
                    }
                    Some(f) => {
                        let pts = f.pts_secs();
                        if pts <= current_source_secs + frame_tolerance {
                            // Frame is due or overdue — take it
                            if self.frames_consumed == 0 {
                                info!(
                                    "FIRST FRAME: pts={:.3}s clock={:.3}s delta={:.3}s q_len={}",
                                    pts, current_source_secs, pts - current_source_secs, q_len
                                );
                            }
                            let frame = queue.pop().unwrap();
                            // Check if the next frame is also due (we're behind — drop this one)
                            if let Some(next) = queue.front() {
                                if !next.is_eof && next.pts_secs() <= current_source_secs + frame_tolerance {
                                    // Only count as a drop if we're behind by more than 1 frame
                                    // (normal drift of <1 frame interval is not a real drop)
                                    let gap = next.pts_secs() - pts;
                                    let frame_dur = self.fps.frame_duration_secs();
                                    if gap > frame_dur * 1.5 {
                                        self.dropped_frames += 1;
                                        debug!(
                                            "DROP: frame pts={:.3}, next pts={:.3}, clock={:.3}, gap={:.3}",
                                            pts, next.pts_secs(), current_source_secs, gap
                                        );
                                    }
                                    continue; // skip to the more recent frame
                                }
                            }
                            display_frame = Some(frame);
                            break;
                        } else {
                            // Frame is in the future — wait
                            break;
                        }
                    }
                }
            }
        }

        let frame = match display_frame {
            Some(f) => f,
            None => {
                // Log actual stalls (delta > 0.5s means decode can't keep up)
                if q_len > 0 && self.last_playback_log.elapsed().as_millis() > 2000 {
                    if let Some(dec) = &self.v1_decoder {
                        let queue = dec.frame_queue().lock().unwrap();
                        if let Some(front) = queue.front() {
                            let delta = front.pts_secs() - current_source_secs;
                            if delta > 0.5 {
                                warn!(
                                    "STALL: clock={:.3}s front_pts={:.3}s delta={:.3}s q_len={}",
                                    current_source_secs, front.pts_secs(), delta, q_len
                                );
                                self.last_playback_log = std::time::Instant::now();
                            }
                        }
                    }
                }
                return;
            }
        };

        if frame.is_eof {
            debug!("EOF sentinel received — advancing to next clip");
            self.advance_to_next_clip();
            return;
        }

        // Update playhead from the frame's PTS relative to clip timeline
        let seq = self.sequence.read().unwrap();
        if let Some(track) = seq.video_track() {
            if let Some(clip) = track.clips.get(self.current_clip_index) {
                let source_elapsed_secs = frame.pts_secs() - clip.source_in.to_secs_f64();
                let elapsed_tc = RationalTime::from_secs_f64(
                    source_elapsed_secs.max(0.0),
                    self.fps.rate(),
                );
                self.playhead = clip.timeline_in + elapsed_tc;

                // Check if we've passed source_out
                if frame.pts_secs() >= clip.source_out.to_secs_f64() - 0.01 {
                    self.playhead = clip.timeline_out;
                    drop(seq);
                    self.advance_to_next_clip();
                    return;
                }
            }
        }
        drop(seq);

        // Check V2 decoder lifecycle — open/close as playhead moves through V2 clips
        self.maybe_open_v2_decoder(self.playhead);

        // Pop V2 frame if available
        if let Some(v2_dec) = &self.v2_decoder {
            let mut queue = v2_dec.frame_queue().lock().unwrap();
            while let Some(f) = queue.front() {
                if f.is_eof {
                    let _ = queue.pop();
                    break;
                }
                let pts = f.pts_secs();
                if pts <= current_source_secs + frame_tolerance {
                    let v2f = queue.pop().unwrap();
                    // Check if next is also due (drop current)
                    if let Some(next) = queue.front() {
                        if !next.is_eof && next.pts_secs() <= current_source_secs + frame_tolerance {
                            continue;
                        }
                    }
                    self.v2_frame_cache = Some(v2f);
                    break;
                } else {
                    break;
                }
            }
        }

        // Composite V1 + V2 if V2 frame is available, or pass V1 directly
        let (final_w, final_h) = (frame.width, frame.height);
        let final_rgba = if let Some(v2_frame) = &self.v2_frame_cache {
            let v2_opacity = self.sequence.read().unwrap()
                .video_track2()
                .map(|t| t.opacity)
                .unwrap_or(1.0);

            if v2_opacity > 0.001 {
                use crate::composite::{composite_layers, CompositeLayer};
                let layers = vec![
                    CompositeLayer {
                        rgba: frame.into_rgba(),
                        width: final_w,
                        height: final_h,
                        opacity: 1.0,
                        blend_mode: klipp_core::BlendMode::Normal,
                    },
                    CompositeLayer {
                        rgba: v2_frame.to_rgba(),
                        width: v2_frame.width,
                        height: v2_frame.height,
                        opacity: v2_opacity,
                        blend_mode: klipp_core::BlendMode::Normal,
                    },
                ];
                composite_layers(&layers, final_w, final_h)
            } else {
                frame.into_rgba()
            }
        } else {
            frame.into_rgba()
        };

        // Apply per-clip color grade
        let final_rgba = {
            let seq = self.sequence.read().unwrap();
            let grade = seq.video_track()
                .and_then(|t| t.clips.get(self.current_clip_index))
                .map(|c| c.color_grade);
            drop(seq);
            if let Some(g) = grade {
                if g.enabled {
                    self.color_pipeline.apply_clip_grade(
                        final_rgba, g.exposure, g.contrast, g.saturation, g.temperature, g.tint,
                    )
                } else {
                    final_rgba
                }
            } else {
                final_rgba
            }
        };

        // Apply global color pipeline (passthrough when disabled)
        let final_rgba = self.color_pipeline.process_frame(final_rgba, final_w, final_h);

        // Write frame to the shared FrameBuffer
        let mut fb = self.frame_buffer.lock().unwrap();
        fb.write_frame(final_rgba, final_w, final_h);
        drop(fb);

        self.frames_consumed += 1;

        let update_ms = t_update.elapsed().as_secs_f64() * 1000.0;
        if update_ms > 5.0 {
            info!(
                "PERF update_playback: {update_ms:.1}ms for {}x{} frame",
                final_w, final_h
            );
        }

        // Periodic playback status (every 2 seconds)
        if self.last_playback_log.elapsed().as_secs() >= 2 {
            let elapsed = self.last_playback_log.elapsed().as_secs_f64();
            info!(
                "PLAYBACK: clock={:.3}s consumed={} ({:.1}fps) drops={} q_len={}",
                current_source_secs, self.frames_consumed,
                self.frames_consumed as f64 / elapsed.max(0.001),
                self.dropped_frames, q_len
            );
            self.frames_consumed = 0;
            self.last_playback_log = std::time::Instant::now();
        }

        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
        let _ = self.event_tx.send(AppEvent::FrameReady);
        (self.repaint_cb)();
    }

    fn advance_to_next_clip(&mut self) {
        let seq = self.sequence.read().unwrap();
        let next_idx = self.current_clip_index + 1;
        let next_clip_info = seq.video_track().and_then(|track| {
            track.clips.get(next_idx).map(|clip| {
                (clip.media_id, clip.source_in.to_secs_f64(), clip.timeline_in)
            })
        });
        drop(seq);

        if let Some((media_id, source_in_secs, timeline_in)) = next_clip_info {
            self.current_clip_index = next_idx;
            self.playhead = timeline_in;

            if let Some(path) = self.path_for_media(media_id) {
                info!("Advancing to clip {next_idx} ({:?})", path);

                // Stop old decoders (detached)
                if let Some(dec) = &mut self.v1_decoder {
                    dec.stop_detached();
                }
                self.v1_decoder = None;
                if let Some(dec) = &mut self.v2_decoder {
                    dec.stop_detached();
                }
                self.v2_decoder = None;
                self.current_clip_v2 = None;
                self.v2_frame_cache = None;
                if let Some(adec) = &mut self.audio_source {
                    adec.stop_detached();
                }
                self.audio_source = None;
                self.audio_output = None;

                // Open new video decoder
                match VideoDecoder::open(&path) {
                    Ok(dec) => {
                        if source_in_secs > 0.01 {
                            dec.seek(source_in_secs);
                        }
                        dec.resume();
                        self.v1_decoder = Some(dec);
                        self.current_file = Some(path.clone());
                    }
                    Err(e) => {
                        error!("Failed to open decoder for next clip: {e}");
                        self.stop_at_end();
                        return;
                    }
                }

                // Open new audio decoder + output, set master clock
                let start_us = (source_in_secs * 1_000_000.0) as u64;
                self.master_clock.seek(start_us);
                self.master_clock.resume();
                self.start_audio(media_id, &path, source_in_secs);

                self.clip_source_start = source_in_secs;
                self.clip_timeline_start = timeline_in;
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));

                // Re-check V2 at the new playhead
                self.maybe_open_v2_decoder(self.playhead);
            } else {
                warn!("No path for media {:?}", media_id);
                self.stop_at_end();
            }
        } else {
            self.stop_at_end();
        }
    }

    /// Check if Video 2 has a clip at the given playhead position.
    /// Opens/closes the V2 decoder as needed.
    fn maybe_open_v2_decoder(&mut self, playhead: Timecode) {
        let seq = self.sequence.read().unwrap();
        let v2_info = seq.video_track2().and_then(|track| {
            if track.muted {
                return None;
            }
            track.clip_at_timecode(playhead).map(|(idx, clip)| {
                let source_secs = clip.source_in.to_secs_f64()
                    + (playhead - clip.timeline_in).to_secs_f64();
                (idx, clip.media_id, source_secs)
            })
        });
        drop(seq);

        match v2_info {
            Some((idx, media_id, source_secs)) => {
                // Already have the right decoder open?
                if self.current_clip_v2 == Some(idx) {
                    return;
                }
                // Close old V2 decoder
                if let Some(dec) = &mut self.v2_decoder {
                    dec.stop_detached();
                }
                self.v2_decoder = None;
                self.v2_frame_cache = None;

                if let Some(path) = self.path_for_media(media_id) {
                    match VideoDecoder::open(&path) {
                        Ok(dec) => {
                            if source_secs > 0.01 {
                                dec.seek(source_secs);
                            }
                            dec.resume();
                            self.v2_decoder = Some(dec);
                            self.current_clip_v2 = Some(idx);
                            debug!("V2 decoder opened for clip {idx}");
                        }
                        Err(e) => {
                            warn!("Failed to open V2 decoder: {e}");
                            self.current_clip_v2 = None;
                        }
                    }
                }
            }
            None => {
                // No V2 clip at playhead — close decoder if open
                if self.v2_decoder.is_some() {
                    if let Some(dec) = &mut self.v2_decoder {
                        dec.stop_detached();
                    }
                    self.v2_decoder = None;
                    self.current_clip_v2 = None;
                    self.v2_frame_cache = None;
                    debug!("V2 decoder closed (no clip at playhead)");
                }
            }
        }
    }

    fn stop_at_end(&mut self) {
        self.v1_decoder = None;
        self.v2_decoder = None;
        self.current_clip_v2 = None;
        self.v2_frame_cache = None;
        self.audio_source = None;
        self.audio_output = None;
        self.state = PlaybackState::Paused;
        let _ = self.event_tx.send(AppEvent::PlaybackPaused);
        (self.repaint_cb)();
    }

    fn seek_in_sequence(&mut self, tc: Timecode) {
        self.seek_in_sequence_with_timeout(tc, Duration::from_millis(50));
    }

    fn seek_in_sequence_with_timeout(&mut self, tc: Timecode, grab_timeout: Duration) {
        let seq = self.sequence.read().unwrap();
        let clip_info = seq.video_track().and_then(|track| {
            track.clip_at_timecode(tc).map(|(idx, clip)| {
                let source_secs = clip.source_in.to_secs_f64()
                    + (tc - clip.timeline_in).to_secs_f64();
                (idx, clip.media_id, source_secs, clip.timeline_in)
            })
        });
        drop(seq);

        if let Some((idx, media_id, source_secs, timeline_in)) = clip_info {
            let need_new_decoder = self.current_clip_index != idx || self.v1_decoder.is_none();

            if need_new_decoder {
                if let Some(path) = self.path_for_media(media_id) {
                    self.v1_decoder = None;
                    self.audio_source = None;
                    self.audio_output = None;
                    match VideoDecoder::open(&path) {
                        Ok(dec) => {
                            dec.seek(source_secs);
                            dec.resume();
                            self.v1_decoder = Some(dec);
                            self.current_clip_index = idx;
                            self.current_file = Some(path.clone());
                            self.clip_source_start = source_secs;
                            self.clip_timeline_start = timeline_in;
                        }
                        Err(e) => {
                            error!("Failed to open decoder for seek: {e}");
                        }
                    }
                    // Re-open audio for the new clip
                    self.start_audio(media_id, &path, source_secs);
                }
            } else if let Some(dec) = &self.v1_decoder {
                dec.seek(source_secs);
                if self.state != PlaybackState::Playing {
                    dec.resume();
                }
                self.clip_source_start = source_secs;
                self.clip_timeline_start = timeline_in;
            }

            // Master clock seek procedure:
            // 1. Pause clock
            self.master_clock.pause();

            // 2. Flush audio output
            if let Some(aout) = &self.audio_output {
                aout.0.start_flush();
            }

            // 3. Seek audio with generation counter (waits for readiness)
            self.seek_audio_and_wait(source_secs);

            // 4. Set master clock to target
            let target_us = (source_secs * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            // 5. Resume or pause audio based on playback state
            if self.state == PlaybackState::Playing {
                if let Some(asrc) = &self.audio_source {
                    asrc.resume();
                }
                if let Some(aout) = &self.audio_output {
                    aout.0.resume();
                }
            } else if let Some(asrc) = &self.audio_source {
                asrc.pause();
            }

            // 6. Resume master clock if playing
            if self.state == PlaybackState::Playing {
                self.master_clock.resume();
            }

            // Re-check V2 at the new playhead
            self.v2_frame_cache = None;
            self.maybe_open_v2_decoder(tc);

            if self.state != PlaybackState::Playing {
                self.grab_single_frame_timeout(grab_timeout);
            }
        }

        self.playhead = tc;
        let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
        (self.repaint_cb)();
    }

    /// Step forward or backward by one frame without a full seek.
    fn step_frame(&mut self, direction: i64) {
        if direction >= 0 {
            // Forward: drain any stale frames, then resume decoder to get the next one
            if let Some(dec) = &self.v1_decoder {
                // Drain existing queued frames (stale from before pause)
                let mut last_frame = None;
                while let Some(frame) = dec.frame_queue().lock().unwrap().pop() {
                    if !frame.is_eof {
                        last_frame = Some(frame);
                    }
                }
                // If we got a queued frame, use it as the step
                if let Some(frame) = last_frame {
                    let seq = self.sequence.read().unwrap();
                    if let Some(track) = seq.video_track() {
                        if let Some(clip) = track.clips.get(self.current_clip_index) {
                            let source_elapsed =
                                frame.pts_secs() - clip.source_in.to_secs_f64();
                            let elapsed_tc = RationalTime::from_secs_f64(
                                source_elapsed.max(0.0),
                                self.fps.rate(),
                            );
                            self.playhead = clip.timeline_in + elapsed_tc;
                        }
                    }
                    drop(seq);
                    let rgba = frame.to_rgba();
                    let mut fb = self.frame_buffer.lock().unwrap();
                    fb.write_frame(rgba, frame.width, frame.height);
                    drop(fb);
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    let _ = self.event_tx.send(AppEvent::FrameReady);
                    (self.repaint_cb)();
                    return;
                }
                // No queued frames — resume decoder to produce one
                dec.resume();
            }
            // Wait for one frame with short timeout
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            while std::time::Instant::now() < deadline {
                if let Some(dec) = &self.v1_decoder {
                    if let Some(frame) = dec.frame_queue().lock().unwrap().pop() {
                        if frame.is_eof {
                            dec.pause();
                            return;
                        }
                        // Update playhead from frame PTS
                        let seq = self.sequence.read().unwrap();
                        if let Some(track) = seq.video_track() {
                            if let Some(clip) = track.clips.get(self.current_clip_index) {
                                let source_elapsed =
                                    frame.pts_secs() - clip.source_in.to_secs_f64();
                                let elapsed_tc = RationalTime::from_secs_f64(
                                    source_elapsed.max(0.0),
                                    self.fps.rate(),
                                );
                                self.playhead = clip.timeline_in + elapsed_tc;
                            }
                        }
                        drop(seq);

                        let rgba = frame.to_rgba();
                        let mut fb = self.frame_buffer.lock().unwrap();
                        fb.write_frame(rgba, frame.width, frame.height);
                        drop(fb);
                        let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                        let _ = self.event_tx.send(AppEvent::FrameReady);
                        if let Some(dec) = &self.v1_decoder {
                            dec.pause();
                        }
                        (self.repaint_cb)();
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            if let Some(dec) = &self.v1_decoder {
                dec.pause();
            }
        } else {
            // Backward: need to seek since decoders only go forward
            let new_tc = RationalTime {
                value: (self.playhead.value - 1).max(0),
                rate: self.playhead.rate,
            };
            self.seek_in_sequence_with_timeout(new_tc, Duration::from_millis(300));
        }
    }

    fn grab_single_frame_timeout(&mut self, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Some(dec) = &self.v1_decoder {
                let mut queue = dec.frame_queue().lock().unwrap();
                // Drain to the latest non-EOF frame
                let mut best = None;
                while let Some(frame) = queue.pop() {
                    if frame.is_eof {
                        break;
                    }
                    best = Some(frame);
                }
                drop(queue);
                if let Some(frame) = best {
                    let rgba = frame.to_rgba();
                    let mut fb = self.frame_buffer.lock().unwrap();
                    fb.write_frame(rgba, frame.width, frame.height);
                    drop(fb);
                    let _ = self.event_tx.send(AppEvent::FrameReady);
                    dec.pause();
                    (self.repaint_cb)();
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(dec) = &self.v1_decoder {
            dec.pause();
        }
    }

    fn sequence_duration(&self) -> Timecode {
        self.sequence.read().unwrap().total_duration()
    }

    fn path_for_media(&self, media_id: MediaId) -> Option<PathBuf> {
        self.media_pool
            .read()
            .unwrap()
            .get(&media_id)
            .map(|item| item.path.clone())
    }

    fn load_file(&mut self, path: PathBuf, media_id: MediaId) {
        info!("Loading media: {:?}", path);

        match ffmpeg_next::format::input(&path) {
            Ok(format_ctx) => {
                let duration_secs = format_ctx.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE);

                let mut video_w = 1920u32;
                let mut video_h = 1080u32;
                let mut container_fps = 24.0f64;
                let mut video_codec = String::new();
                let mut audio_codec = String::new();
                let mut has_audio = false;

                for stream in format_ctx.streams() {
                    let params = stream.parameters();
                    let medium = params.medium();
                    match medium {
                        ffmpeg_next::media::Type::Video => {
                            let decoder = ffmpeg_next::codec::context::Context::from_parameters(params)
                                .and_then(|ctx| ctx.decoder().video());
                            if let Ok(dec) = decoder {
                                video_w = dec.width();
                                video_h = dec.height();
                                video_codec = format!("{:?}", dec.id());
                            }
                            let rate = stream.avg_frame_rate();
                            if rate.1 != 0 {
                                container_fps = rate.0 as f64 / rate.1 as f64;
                            }
                        }
                        ffmpeg_next::media::Type::Audio => {
                            has_audio = true;
                            let decoder = ffmpeg_next::codec::context::Context::from_parameters(params)
                                .and_then(|ctx| ctx.decoder().audio());
                            if let Ok(dec) = decoder {
                                audio_codec = format!("{:?}", dec.id());
                            }
                        }
                        _ => {}
                    }
                }

                self.fps = fps_from_float(container_fps);
                self.duration = RationalTime::from_secs_f64(duration_secs, self.fps.rate());
                self.playhead = Timecode::zero();
                self.current_file = Some(path.clone());
                self.current_clip_index = 0;
                self.state = PlaybackState::Stopped;

                let file_size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

                let _ = self.event_tx.send(AppEvent::MediaLoaded {
                    media_id,
                    path,
                    duration: self.duration,
                    fps: self.fps,
                    resolution: Resolution {
                        width: video_w,
                        height: video_h,
                    },
                    video_codec,
                    audio_codec,
                    has_audio,
                    file_size_bytes,
                });

                self.grab_first_frame();
                (self.repaint_cb)();
            }
            Err(e) => {
                error!("Failed to open media file: {e}");
                let _ = self.event_tx.send(AppEvent::MediaLoadFailed {
                    media_id,
                    error: e.to_string(),
                });
                (self.repaint_cb)();
            }
        }
    }

    fn grab_first_frame(&mut self) {
        if let Some(path) = &self.current_file {
            match VideoDecoder::open(path) {
                Ok(dec) => {
                    dec.resume();
                    self.v1_decoder = Some(dec);
                    // Longer timeout: VAAPI init can take ~150ms alone
                    self.grab_single_frame_timeout(Duration::from_millis(500));
                }
                Err(e) => {
                    warn!("Could not open decoder for first frame: {e}");
                }
            }
        }
    }
}

/// Coalesce a batch of commands: collapse consecutive seeks into just the last one.
fn coalesce_commands(commands: Vec<AppCommand>) -> Vec<AppCommand> {
    if commands.len() <= 1 {
        return commands;
    }

    let mut result = Vec::with_capacity(commands.len());
    let mut last_seek: Option<AppCommand> = None;

    for cmd in commands {
        match &cmd {
            AppCommand::SeekTo(_) | AppCommand::SetPlayhead(_) => {
                // Replace any pending seek with this newer one
                last_seek = Some(cmd);
            }
            _ => {
                // Flush pending seek before non-seek command
                if let Some(seek) = last_seek.take() {
                    result.push(seek);
                }
                result.push(cmd);
            }
        }
    }

    // Flush final pending seek
    if let Some(seek) = last_seek.take() {
        result.push(seek);
    }

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

/// Generate a thumbnail for a media file using ffmpeg-next.
pub fn generate_thumbnail(
    path: &std::path::Path,
    seek_secs: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    use ffmpeg_next::software::scaling;

    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let video_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or("No video stream")?;
    let stream_index = video_stream.index();
    let params = video_stream.parameters();

    let codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("codec context: {e}"))?;
    let mut decoder = codec_ctx.decoder().video()
        .map_err(|e| format!("video decoder: {e}"))?;

    if seek_secs > 0.1 {
        // seek with stream=-1 expects AV_TIME_BASE (microseconds)
        let ts = (seek_secs * 1_000_000.0) as i64;
        format_ctx.seek(ts, ..ts).map_err(|e| format!("seek: {e}"))?;
    }

    let mut decoded_frame = ffmpeg_next::frame::Video::empty();
    let mut got_frame = false;

    for (stream, packet) in format_ctx.packets() {
        if stream.index() != stream_index {
            continue;
        }
        decoder.send_packet(&packet).map_err(|e| format!("send_packet: {e}"))?;
        if decoder.receive_frame(&mut decoded_frame).is_ok() {
            got_frame = true;
            break;
        }
    }

    if !got_frame {
        return Err("No frame decoded".to_string());
    }

    let src_w = decoded_frame.width();
    let src_h = decoded_frame.height();
    let scale = (160.0 / src_w as f64).min(90.0 / src_h as f64).min(1.0);
    let dst_w = (src_w as f64 * scale) as u32;
    let dst_h = (src_h as f64 * scale) as u32;

    let mut scaler = scaling::Context::get(
        decoded_frame.format(),
        src_w,
        src_h,
        ffmpeg_next::format::Pixel::RGBA,
        dst_w,
        dst_h,
        scaling::Flags::BILINEAR,
    )
    .map_err(|e| format!("scaler: {e}"))?;

    let mut rgba_frame = ffmpeg_next::frame::Video::empty();
    scaler
        .run(&decoded_frame, &mut rgba_frame)
        .map_err(|e| format!("scale: {e}"))?;

    let stride = rgba_frame.stride(0);
    let expected = dst_w as usize * 4;
    let rgba = if stride == expected {
        rgba_frame.data(0)[..expected * dst_h as usize].to_vec()
    } else {
        let mut buf = Vec::with_capacity(expected * dst_h as usize);
        for row in 0..dst_h as usize {
            let start = row * stride;
            buf.extend_from_slice(&rgba_frame.data(0)[start..start + expected]);
        }
        buf
    };
    Ok((rgba, dst_w, dst_h))
}

/// Generate filmstrip thumbnails incrementally with throttling.
/// Calls `on_frame` for each generated frame, sleeping between frames to avoid
/// starving the playback decode thread. Returns the total number of frames generated.
pub fn generate_filmstrip_incremental(
    path: &std::path::Path,
    duration_secs: f64,
    interval_secs: f64,
    target_height: u32,
    mut on_frame: impl FnMut(f64, Vec<u8>, u32, u32),
) -> Result<usize, String> {
    use ffmpeg_next::software::scaling;

    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let video_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or("No video stream")?;
    let stream_index = video_stream.index();
    let params = video_stream.parameters();

    let mut codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("codec context: {e}"))?;

    // Enable multi-threaded decode but limit to half the cores so playback isn't starved
    unsafe {
        let half_cores = (std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4) / 2)
            .max(1) as i32;
        (*codec_ctx.as_mut_ptr()).thread_count = half_cores;
    }

    let mut decoder = codec_ctx.decoder().video()
        .map_err(|e| format!("video decoder: {e}"))?;

    let src_w = decoder.width();
    let src_h = decoder.height();
    let aspect = src_w as f64 / src_h.max(1) as f64;
    let dst_h = target_height;
    let dst_w = (dst_h as f64 * aspect).round() as u32;

    let mut scaler: Option<scaling::Context> = None;
    let mut count = 0;

    let num_frames = ((duration_secs / interval_secs).ceil() as usize).min(500);

    for i in 0..=num_frames {
        let seek_secs = i as f64 * interval_secs;
        if seek_secs > duration_secs {
            break;
        }

        // Throttle: yield CPU between frames so playback decode isn't starved.
        // 100ms sleep keeps filmstrip generation gentle in the background.
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let ts = (seek_secs * 1_000_000.0) as i64;
        if format_ctx.seek(ts, ..ts).is_err() {
            continue;
        }
        decoder.flush();

        let mut decoded_frame = ffmpeg_next::frame::Video::empty();
        let mut got_frame = false;

        for (stream, packet) in format_ctx.packets() {
            if stream.index() != stream_index {
                continue;
            }
            let _ = decoder.send_packet(&packet);
            if decoder.receive_frame(&mut decoded_frame).is_ok() {
                got_frame = true;
                break;
            }
        }

        if !got_frame {
            continue;
        }

        if scaler.is_none() {
            scaler = scaling::Context::get(
                decoded_frame.format(),
                decoded_frame.width(),
                decoded_frame.height(),
                ffmpeg_next::format::Pixel::RGBA,
                dst_w,
                dst_h,
                scaling::Flags::FAST_BILINEAR,
            )
            .ok();
        }

        if let Some(ref mut s) = scaler {
            let mut rgba_frame = ffmpeg_next::frame::Video::empty();
            if s.run(&decoded_frame, &mut rgba_frame).is_ok() {
                let stride = rgba_frame.stride(0);
                let expected = dst_w as usize * 4;
                let rgba = if stride == expected {
                    rgba_frame.data(0)[..expected * dst_h as usize].to_vec()
                } else {
                    let mut buf = Vec::with_capacity(expected * dst_h as usize);
                    for row in 0..dst_h as usize {
                        let start = row * stride;
                        buf.extend_from_slice(&rgba_frame.data(0)[start..start + expected]);
                    }
                    buf
                };
                on_frame(seek_secs, rgba, dst_w, dst_h);
                count += 1;
            }
        }
    }

    Ok(count)
}
