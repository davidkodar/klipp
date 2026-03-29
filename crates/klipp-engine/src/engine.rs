use crate::audio_decoder::AudioDecoder;
use crate::audio_output::AudioOutput;
use crate::audio_source::AudioSource;
use crate::mix_thread::MixThread;
use crate::pcm_reader::PcmAudioFeeder;
use crate::sync::MasterClock;
use crate::texture::SharedFrameBuffer;
use crate::video_decoder::VideoDecoder;

/// Wrapper adapter so we can implement AlphaMaskProcessor (from klipp-title)
/// for GpuEffectsPipeline (from klipp-effects). Orphan rule requires the newtype pattern.
struct GpuAlphaMaskAdapter<'a>(&'a mut klipp_effects::gpu::GpuEffectsPipeline);

impl klipp_title::AlphaMaskProcessor for GpuAlphaMaskAdapter<'_> {
    fn blur(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        self.0.blur_alpha_gpu(mask, w, h, radius)
    }
    fn dilate(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        self.0.dilate_alpha_gpu(mask, w, h, radius)
    }
}

/// Wrapper to make AudioOutput Send. The cpal::Stream inside is !Send
/// due to platform internals, but we only ever use it on the engine thread.
struct SendAudioOutput(#[allow(dead_code)] AudioOutput);
unsafe impl Send for SendAudioOutput {}

fn fade_curve_to_u32(c: klipp_timeline::FadeCurve) -> u32 {
    match c {
        klipp_timeline::FadeCurve::Linear => 0,
        klipp_timeline::FadeCurve::Exponential => 1,
        klipp_timeline::FadeCurve::Logarithmic => 2,
        klipp_timeline::FadeCurve::SCurve => 3,
    }
}

fn channel_config_to_u32(c: klipp_timeline::ChannelConfig) -> u32 {
    match c {
        klipp_timeline::ChannelConfig::Default => 0,
        klipp_timeline::ChannelConfig::MonoDownmix => 1,
        klipp_timeline::ChannelConfig::SwapLR => 2,
    }
}

/// Evaluate a parameter with optional modifier stack, falling back to plain keyframe evaluation.
fn eval_with_modifiers(
    keyframes: &[klipp_core::Keyframe],
    modifiers: &std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    param: &klipp_core::ParamId,
    time_secs: f64,
    default: f32,
    ctx: &klipp_core::EvalContext,
) -> f32 {
    match modifiers.get(param) {
        Some(stack) if !stack.modifiers.is_empty() => {
            klipp_core::evaluate_param(keyframes, stack, time_secs, default, ctx)
        }
        _ => klipp_core::keyframe::evaluate(keyframes, time_secs, default),
    }
}

/// Evaluate an effect param with optional modifier stack.
fn eval_effect_with_modifiers(
    keyframes: &[klipp_core::Keyframe],
    modifiers: &std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    effect_index: usize,
    param_name: &str,
    time_secs: f64,
    default: f32,
    ctx: &klipp_core::EvalContext,
) -> f32 {
    let param = klipp_core::ParamId::Effect {
        effect_index,
        param_name: param_name.to_string(),
    };
    match modifiers.get(&param) {
        Some(stack) if !stack.modifiers.is_empty() => {
            klipp_core::evaluate_param(keyframes, stack, time_secs, default, ctx)
        }
        _ => klipp_core::keyframe::evaluate(keyframes, time_secs, default),
    }
}

/// One entry per active audio source (one per playing track).
struct ActiveAudioSource {
    #[allow(dead_code)]
    track_index: usize,
    source: AudioSource,
}
/// Pre-opened audio source ready for loop swap (created by pre-seek thread).
struct PreSeekAudioSource {
    track_index: usize,
    source: AudioSource,
    consumer: crate::audio_decoder::AudioConsumer,
}

/// Result from the pre-seek background thread.
struct LoopPreSeekResult {
    generation: u64,
    sources: Vec<PreSeekAudioSource>,
    #[allow(dead_code)]
    target_secs: f64,
}

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
    /// True when audio is continuing seamlessly across a contiguous clip boundary.
    /// Suppresses fade-in on the new clip to prevent dropout.
    contiguous_audio: std::sync::atomic::AtomicBool,
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
    // Audio — one source per active track, mixed through mix thread
    audio_sources: Vec<ActiveAudioSource>,
    audio_output: Option<SendAudioOutput>,
    mix_thread: Option<MixThread>,
    mixer: std::sync::Arc<std::sync::Mutex<klipp_audio::AudioMixer>>,
    cpal_sample_rate: u32,
    // Master clock — single source of truth for A/V timing
    master_clock: Arc<MasterClock>,
    // Timeline offset: maps source PTS to sequence timeline.
    clip_source_start: f64, // source seconds at which playback started for current clip
    clip_timeline_start: Timecode, // timeline position of clip_source_start
    clip_speed: f64, // per-clip speed multiplier (1.0 = normal)
    // Audio seek synchronization — generation counter
    expected_audio_generation: u64,
    // Performance
    use_hw_decode: bool,
    last_stats_time: std::time::Instant,
    frames_consumed: u64,
    last_playback_log: std::time::Instant,
    // Color pipeline — post-processing on RGBA frames before display
    color_pipeline: klipp_color::ColorPipeline,
    // Node graph executor — processes per-clip color grading
    graph_executor: klipp_nodes::GraphExecutor,
    // Before/After — when true, skip per-clip color grade to show ungraded frame
    bypass_color_grade: bool,
    // Matte preview — when true, show B&W qualifier/mask matte instead of graded frame
    matte_preview: bool,
    // Scope tap — which node's output to capture for scope display
    scope_tap_node: Option<klipp_nodes::NodeId>,
    // Matte preview node — which Qualifier/Mask node's matte to show
    matte_preview_node: Option<klipp_nodes::NodeId>,
    // Cached pre-grade frame for instant parameter updates when paused.
    // Stored after decode, before color grading. Re-graded when sequence changes.
    cached_pregrade_frame: Option<(Vec<u8>, u32, u32)>, // (rgba, width, height)
    // OFX plugin host — applies per-clip effects chain after color grade
    ofx_host: klipp_plugin::OfxHost,
    // GPU effects pipeline — compute shaders for built-in effects
    gpu_effects: Option<klipp_effects::gpu::GpuEffectsPipeline>,
    // Transition decoder — secondary decoder for the partner clip during transitions
    transition_decoder: Option<VideoDecoder>,
    transition_clip_id: Option<klipp_core::ClipId>,
    transition_frame_cache: Option<crate::types::DecodedFrame>,
    // Title renderer — generates RGBA frames for title clips
    title_renderer: klipp_title::TitleRenderer,
    // VU metering — emit peak levels at ~30Hz
    last_peak_time: std::time::Instant,
    // Proxy: when true, decode from proxy files instead of originals
    use_proxies: bool,
    // Skim audio — shared with app layer for browser hover audio
    skim_consumer: crate::audio_output::SkimConsumer,
    // Loop playback — cycle between in/out points or whole sequence
    loop_enabled: bool,
    // Predictive pre-seek — background thread pre-opens audio decoders before loop point
    pre_seek_result: Arc<std::sync::Mutex<Option<LoopPreSeekResult>>>,
    pre_seek_generation: u64,
    pre_seek_started: bool,
    // Clip look-ahead: pre-decoder for the next clip, opened ~1s before the cut point.
    // When the cut arrives, this decoder is swapped in as v1_decoder — zero stall.
    preroll_decoder: Option<VideoDecoder>,
    preroll_clip_index: Option<usize>,
    preroll_source_in: f64,
}

impl PlaybackEngine {
    pub fn new(
        cmd_rx: Receiver<AppCommand>,
        event_tx: Sender<AppEvent>,
        frame_buffer: SharedFrameBuffer,
        sequence: Arc<RwLock<Sequence>>,
        media_pool: Arc<RwLock<MediaPool>>,
        repaint_cb: Box<dyn Fn() + Send>,
        skim_consumer: crate::audio_output::SkimConsumer,
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

        // Create audio output eagerly so skim audio works without playing a clip first.
        let mixer = std::sync::Arc::new(std::sync::Mutex::new(klipp_audio::AudioMixer::new(0)));
        let (mix_thread, main_consumer) = MixThread::new(cpal_sample_rate, mixer.clone());
        let audio_output = match AudioOutput::new(main_consumer, skim_consumer.clone()) {
            Ok(output) => {
                info!("PlaybackEngine: audio output created eagerly");
                Some(SendAudioOutput(output))
            }
            Err(e) => {
                warn!("Failed to create audio output: {e}");
                None
            }
        };

        Ok(Self {
            cmd_rx,
            event_tx,
            frame_buffer,
            state: PlaybackState::Idle,
            current_file: None,
            current_clip_index: 0,
            contiguous_audio: std::sync::atomic::AtomicBool::new(false),
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
            audio_sources: Vec::new(),
            audio_output,
            mix_thread: Some(mix_thread),
            mixer,
            cpal_sample_rate,
            master_clock: Arc::new(MasterClock::new()),
            clip_source_start: 0.0,
            clip_timeline_start: Timecode::zero(),
            clip_speed: 1.0,
            expected_audio_generation: 0,
            use_hw_decode: false,
            last_stats_time: std::time::Instant::now(),
            frames_consumed: 0,
            last_playback_log: std::time::Instant::now(),
            color_pipeline: klipp_color::ColorPipeline::new(),
            graph_executor: klipp_nodes::GraphExecutor::new(),
            bypass_color_grade: false,
            matte_preview: false,
            scope_tap_node: None,
            matte_preview_node: None,
            cached_pregrade_frame: None,
            ofx_host: {
                let mut h = klipp_plugin::OfxHost::new();
                h.discover_plugins();
                h
            },
            gpu_effects: klipp_effects::gpu::GpuEffectsPipeline::new(),
            transition_decoder: None,
            transition_clip_id: None,
            transition_frame_cache: None,
            title_renderer: klipp_title::TitleRenderer::new(),
            last_peak_time: std::time::Instant::now(),
            use_proxies: true,
            skim_consumer,
            loop_enabled: false,
            pre_seek_result: Arc::new(std::sync::Mutex::new(None)),
            pre_seek_generation: 0,
            pre_seek_started: false,
            preroll_decoder: None,
            preroll_clip_index: None,
            preroll_source_in: 0.0,
        })
    }

    pub fn run(mut self) {
        info!("Engine thread starting");

        loop {
            let timeout = if self.state == PlaybackState::Playing {
                Duration::from_millis(1)
            } else {
                Duration::from_millis(8)
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
                // Sync clip audio params periodically during playback (for keyframe animation)
                self.sync_clip_volumes();
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

            // Emit VU peak levels at ~30Hz
            if self.state == PlaybackState::Playing
                && self.last_peak_time.elapsed() >= Duration::from_millis(33)
            {
                self.last_peak_time = std::time::Instant::now();
                if let Ok(mixer) = self.mixer.lock() {
                    let mut master_l = 0.0f32;
                    let mut master_r = 0.0f32;
                    for (i, track) in mixer.tracks.iter().enumerate() {
                        let _ = self.event_tx.send(AppEvent::TrackPeakLevels {
                            track_index: i,
                            left: track.peak_left,
                            right: track.peak_right,
                        });
                        master_l = master_l.max(track.peak_left);
                        master_r = master_r.max(track.peak_right);
                    }
                    let _ = self.event_tx.send(AppEvent::MasterPeakLevels {
                        left: master_l,
                        right: master_r,
                    });
                }
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
                if self.state == PlaybackState::Paused
                    && (self.v1_decoder.is_some() || !self.audio_sources.is_empty())
                {
                    // Compute current source position from playhead
                    let source_secs = self.clip_source_start
                        + (self.playhead - self.clip_timeline_start).to_secs_f64() * self.clip_speed;
                    debug!(
                        "Play resume: playhead={:.3}s clip_source_start={:.3}s clip_timeline_start={:.3}s speed={:.2}x → source={:.3}s",
                        self.playhead.to_secs_f64(), self.clip_source_start,
                        self.clip_timeline_start.to_secs_f64(), self.clip_speed, source_secs
                    );

                    // Seek video decoders to current position and flush stale frames
                    if let Some(dec) = &self.v1_decoder {
                        dec.seek(source_secs);
                        // Drain stale frames from queue immediately —
                        // the decoder thread will also clear on seek, but we
                        // clear from here to discard any frames produced between
                        // our seek command and the decoder processing it.
                        dec.frame_queue().lock().unwrap().clear();
                        dec.resume();
                    }
                    if let Some(dec) = &self.v2_decoder {
                        dec.resume();
                    }

                    // Flush stale audio, seek all tracks to current position.
                    // Flushes mix thread staging, seeks each source per-track,
                    // and bumps seek_gen on audio output to drain stale main buffer.
                    self.seek_all_audio_to_timecode(self.playhead);

                    // Wait for a valid post-seek video frame before starting
                    // clock+audio. This also gives cpal time to process the
                    // seek_gen bump (drain stale samples) while we wait.
                    self.wait_for_valid_frame(source_secs);

                    // Set clock to target position (not running yet)
                    let source_us = (source_secs * 1_000_000.0) as u64;
                    self.master_clock.seek(source_us);

                    // Pre-fill audio pipeline: resume audio sources so samples
                    // flow through staging → mix thread → main ring buffer.
                    // cpal is still paused, so it accumulates without consuming.
                    for active in &self.audio_sources {
                        active.source.resume();
                    }
                    std::thread::sleep(Duration::from_millis(20));

                    // Start playback: resume audio output + clock together.
                    // Main ring buffer already has ~20ms of correct samples.
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
                    for active in &self.audio_sources {
                        active.source.pause();
                    }
                    if let Some(aout) = &self.audio_output {
                        aout.0.pause();
                        // Drain stale samples so they don't play on resume
                        aout.0.notify_seek();
                    }
                    if let Some(ref mt) = self.mix_thread {
                        mt.flush();
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
                    self.stop_all_audio();
                    self.playhead = Timecode::zero();
                    self.state = PlaybackState::Stopped;
                    self.pre_seek_generation += 1;
                    self.pre_seek_started = false;
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

            AppCommand::SetSpeed(speed) => {
                debug!("SetSpeed({speed})");
                self.master_clock.set_speed(speed);
                // Mute audio at high speeds (unintelligible above 4x)
                for active in &self.audio_sources {
                    if speed.abs() > 4.0 {
                        active.source.muted().store(true, Ordering::Release);
                    } else {
                        active.source.muted().store(false, Ordering::Release);
                    }
                }
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
                // Always update the engine's playhead (even when Idle) so title overlay lookups work
                self.playhead = tc;
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

            AppCommand::SetTrackVolume { track_index, volume } => {
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].volume = volume;
                }
            }
            AppCommand::SetTrackMute { track_index, muted } => {
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].muted = muted;
                }
            }
            AppCommand::SetTrackSolo { track_index, solo } => {
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].solo = solo;
                }
            }
            AppCommand::SetTrackPan { track_index, pan } => {
                if let Ok(mut m) = self.mixer.lock() {
                    m.ensure_tracks(track_index + 1);
                    m.tracks[track_index].pan = pan;
                }
            }
            AppCommand::SetMasterVolume(vol) => {
                // Apply in cpal callback for instant response (no ring buffer lag)
                if let Some(aout) = &self.audio_output {
                    aout.0.set_master_volume(vol);
                }
                // Also keep mixer in sync for VU metering
                if let Ok(mut m) = self.mixer.lock() {
                    m.master_volume = vol;
                }
            }

            AppCommand::SetClipVolume(vol) => {
                if let Some(aout) = &self.audio_output {
                    aout.0.set_clip_volume(vol);
                }
            }

            AppCommand::SetClipPan(pan) => {
                if let Some(aout) = &self.audio_output {
                    aout.0.set_clip_pan(pan);
                }
            }

            AppCommand::SetClipGain(gain) => {
                if let Some(aout) = &self.audio_output {
                    aout.0.set_clip_gain(gain);
                }
            }

            AppCommand::SetClipChannelConfig(config) => {
                if let Some(aout) = &self.audio_output {
                    aout.0.set_channel_config(config);
                }
            }

            AppCommand::SetUseProxies(enabled) => {
                info!("Engine: use_proxies = {enabled}");
                self.use_proxies = enabled;
            }

            AppCommand::SetLoopPlayback(enabled) => {
                debug!("SetLoopPlayback: {enabled}");
                self.loop_enabled = enabled;
                if !enabled {
                    self.pre_seek_generation += 1;
                    self.pre_seek_started = false;
                }
            }

            AppCommand::SetAntiClickFade(enabled) => {
                for active in &self.audio_sources {
                    active.source.set_anti_click(enabled);
                }
            }

            AppCommand::ToggleBeforeAfter => {
                self.bypass_color_grade = !self.bypass_color_grade;
                debug!("Before/After: bypass_color_grade = {}", self.bypass_color_grade);
            }

            AppCommand::SetMattePreview(active) => {
                self.matte_preview = active;
                // When matte preview is disabled, clear the matte preview node
                if !active {
                    self.matte_preview_node = None;
                }
                debug!("Matte preview: {}", self.matte_preview);
            }

            AppCommand::SetScopeTapNode { node_id, .. } => {
                self.scope_tap_node = node_id.clone();
                debug!("Scope tap node: {:?}", self.scope_tap_node);
            }

            AppCommand::SequenceUpdated => {
                // Sync per-clip volume/pan to active audio sources
                self.sync_clip_volumes();
                // Re-read clip speed in case SpeedRamp changed it during playback
                if self.state == PlaybackState::Playing {
                    let seq = self.sequence.read().unwrap();
                    if let Some(clip) = seq.tracks.iter()
                        .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                        .find_map(|t| t.clips.get(self.current_clip_index))
                    {
                        let new_speed = clip.speed.max(0.001);
                        if (self.clip_speed - new_speed).abs() > 0.001 {
                            tracing::debug!("SequenceUpdated: clip_speed {} → {}", self.clip_speed, new_speed);
                            self.clip_speed = new_speed;
                            self.master_clock.set_speed(new_speed);
                            // Mute audio at non-normal speeds
                            for active in &self.audio_sources {
                                active.source.muted().store((new_speed - 1.0).abs() > 0.01, Ordering::Release);
                            }
                        }
                    }
                    drop(seq);
                } else {
                    // Re-grade cached frame when paused — instant parameter feedback
                    // Refresh title overlay FIRST so regrade composites the updated title
                    self.refresh_v2_title_overlay();
                    self.regrade_cached_frame();
                }
            }

            // UI-side commands — engine ignores them
            _ => {}
        }

        false
    }

    fn handle_play(&mut self) {
        // Check if no media clip exists at playhead but a title clip does (standalone title)
        {
            let seq = self.sequence.read().unwrap();
            let has_media_clip = seq.tracks.iter()
                .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text))
                .any(|track| {
                    track.clip_at_timecode(self.playhead)
                        .map(|(_, c)| c.media_id().is_some())
                        .unwrap_or(false)
                });
            let title_info = if !has_media_clip {
                seq.tracks.iter()
                    .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text))
                    .find_map(|track| {
                        track.clip_at_timecode(self.playhead).and_then(|(idx, clip)| {
                            let td = clip.title_data()?.clone();
                            Some((idx, td, clip.timeline_in, clip.source_in.to_secs_f64()))
                        })
                    })
            } else {
                None
            };
            drop(seq);

            if let Some((idx, _title_data, timeline_in, clip_source_in)) = title_info {
                info!("handle_play: title clip {idx} at playhead");
                self.v1_decoder = None;
                self.current_clip_index = idx;
                self.current_file = None;
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;

                // Start audio for all tracks
                self.start_all_audio(self.playhead);
                if let Some(aout) = &self.audio_output {
                    aout.0.pause();
                }

                // Set clock to playhead position (title clips use timeline time as source)
                let source_secs = self.playhead.to_secs_f64();
                let source_us = (source_secs * 1_000_000.0) as u64;
                self.master_clock.seek(source_us);

                // Pre-fill audio
                for active in &self.audio_sources {
                    active.source.resume();
                }
                std::thread::sleep(Duration::from_millis(20));

                if let Some(aout) = &self.audio_output {
                    aout.0.resume();
                }
                self.master_clock.resume();

                self.state = PlaybackState::Playing;
                self.maybe_open_v2_decoder(self.playhead);
                self.dropped_frames = 0;
                self.frames_consumed = 0;
                self.last_playback_log = std::time::Instant::now();
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlaybackStarted);
                (self.repaint_cb)();
                return;
            }
        }

        let seq = self.sequence.read().unwrap();
        // Search ALL video tracks (not just Video 1)
        let clip_info = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|track| {
                // Check if playhead is on a non-gap clip
                if let Some((idx, clip)) = track.clip_at_timecode(self.playhead) {
                    if let Some(media_id) = clip.media_id() {
                        let source_secs = clip.source_in.to_secs_f64()
                            + (self.playhead - clip.timeline_in).to_secs_f64() * clip.speed;
                        let clip_source_in = clip.source_in.to_secs_f64();
                        return Some((idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed));
                    }
                    // Playhead is on a gap clip — fall through to find next real clip
                }
                // Find next non-gap clip at or after playhead
                for (idx, clip) in track.clips.iter().enumerate() {
                    if clip.timeline_in >= self.playhead {
                        if let Some(media_id) = clip.media_id() {
                            let si = clip.source_in.to_secs_f64();
                            return Some((idx, media_id, si, clip.timeline_in, si, clip.speed));
                        }
                        // Gap clip — continue searching
                    }
                }
                // Fallback: first non-gap clip
                for (idx, clip) in track.clips.iter().enumerate() {
                    if let Some(media_id) = clip.media_id() {
                        let si = clip.source_in.to_secs_f64();
                        return Some((idx, media_id, si, clip.timeline_in, si, clip.speed));
                    }
                }
                None
            });
        drop(seq);

        if let Some((clip_idx, media_id, source_secs, timeline_in, clip_source_in, clip_speed)) = clip_info {
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

                // Open audio for ALL tracks (sources start paused, already
                // seeked to correct position — no stale data to flush)
                self.start_all_audio(self.playhead);

                // Pause cpal output while we wait for video
                if let Some(aout) = &self.audio_output {
                    aout.0.pause();
                }

                // Wait for a valid post-seek video frame
                self.wait_for_valid_frame(source_secs);

                // Set clock to target (not running yet)
                let source_us = (source_secs * 1_000_000.0) as u64;
                self.master_clock.seek(source_us);

                // Pre-fill: resume audio sources so samples flow into ring buffer
                // while cpal is still paused (accumulates without consuming)
                for active in &self.audio_sources {
                    active.source.resume();
                }
                std::thread::sleep(Duration::from_millis(20));

                // Start playback: resume cpal + clock together
                if let Some(aout) = &self.audio_output {
                    aout.0.resume();
                }
                // Set clock speed to clip_speed so source time advances at the right rate.
                self.master_clock.set_speed(clip_speed);
                self.master_clock.resume();

                // Mute audio when clip speed is not normal (audio pitch would be wrong)
                for active in &self.audio_sources {
                    active.source.muted().store((clip_speed - 1.0).abs() > 0.01, Ordering::Release);
                }

                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = clip_speed;
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
            // No video clip — try audio-only playback
            self.handle_play_audio_only();
        }
    }

    /// Audio-only playback — no video decoder, just audio tracks.
    fn handle_play_audio_only(&mut self) {
        // Check if any audio track has a clip at the playhead
        let has_audio = {
            let seq = self.sequence.read().unwrap();
            seq.tracks.iter().any(|track| {
                track.kind == klipp_timeline::TrackKind::Audio
                    && track.clip_at_timecode(self.playhead).is_some()
            })
        };
        if !has_audio {
            debug!("No clips in sequence (video or audio)");
            return;
        }

        info!("handle_play: audio-only at {:.3}s", self.playhead.to_secs_f64());

        // Open audio sources (start paused, already seeked — no stale data)
        self.start_all_audio(self.playhead);

        // Pause cpal while we set up
        if let Some(aout) = &self.audio_output {
            aout.0.pause();
        }

        // Set clock
        let source_us = (self.playhead.to_secs_f64() * 1_000_000.0) as u64;
        self.master_clock.seek(source_us);

        // Pre-fill audio pipeline
        for active in &self.audio_sources {
            active.source.resume();
        }
        std::thread::sleep(Duration::from_millis(20));

        // Start playback
        if let Some(aout) = &self.audio_output {
            aout.0.resume();
        }
        self.master_clock.resume();

        self.clip_source_start = self.playhead.to_secs_f64();
        self.clip_timeline_start = self.playhead;
        self.clip_speed = 1.0; // audio-only: no per-clip speed for now
        self.state = PlaybackState::Playing;
        self.dropped_frames = 0;
        self.frames_consumed = 0;
        self.last_playback_log = std::time::Instant::now();
        self.last_frame_time = std::time::Instant::now();
        let _ = self.event_tx.send(AppEvent::PlaybackStarted);
        (self.repaint_cb)();
    }

    /// Open audio sources for ALL tracks that have a clip at `tc`.
    /// Replaces any existing audio sources.
    fn start_all_audio(&mut self, tc: Timecode) {
        self.stop_all_audio();

        // Collect (track_index, media_id, path, source_secs) for every track with audio at tc
        let track_audio: Vec<(usize, MediaId, PathBuf, f64)> = {
            let seq = self.sequence.read().unwrap();
            let pool = self.media_pool.read().unwrap();
            seq.tracks
                .iter()
                .enumerate()
                .filter_map(|(track_idx, track)| {
                    let (_clip_idx, clip) = track.clip_at_timecode(tc)?;
                    let media_id = clip.media_id()?;
                    let item = pool.get(&media_id)?;
                    if !item.has_audio {
                        return None;
                    }
                    // Skip audio for video-track clips that have a linked audio-track partner
                    // (the audio track clip will produce audio instead)
                    if track.kind == klipp_timeline::TrackKind::Video {
                        if let Some(lid) = clip.linked_clip_id {
                            let partner_on_audio = seq.tracks.iter().any(|t| {
                                t.kind == klipp_timeline::TrackKind::Audio
                                    && t.clips.iter().any(|c| c.id == lid)
                            });
                            if partner_on_audio {
                                return None;
                            }
                        }
                    }
                    let source_secs = clip.source_in.to_secs_f64()
                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed;
                    let decode_path = if self.use_proxies && item.proxy_ready {
                        item.proxy_path.as_ref()
                            .filter(|p| p.exists())
                            .cloned()
                            .unwrap_or_else(|| item.path.clone())
                    } else {
                        item.path.clone()
                    };
                    Some((track_idx, media_id, decode_path, source_secs))
                })
                .collect()
        };

        debug!("start_all_audio: {} tracks with audio at {:.3}s", track_audio.len(), tc.to_secs_f64());
        for (track_idx, media_id, path, source_secs) in track_audio {
            self.open_audio_source(track_idx, media_id, &path, source_secs);
        }
        // Apply per-clip volume from sequence data
        self.sync_clip_volumes();
    }

    /// Sync per-clip volume, pan, and fade from sequence data to the audio output.
    /// Called on start_all_audio and SequenceUpdated.
    fn sync_clip_volumes(&self) {
        let seq = self.sequence.read().unwrap();
        let mut clip_volume = 1.0f32;
        let mut clip_pan = 0.0f32;
        let mut clip_gain_out = 1.0f32;
        let mut clip_ch_config = 0u32;

        for active in &self.audio_sources {
            if let Some(track) = seq.tracks.get(active.track_index) {
                if let Some((_idx, audio_clip)) = track.clip_at_timecode(self.playhead) {
                    // Compute clip-relative time for keyframe evaluation
                    let clip_time_secs = (self.playhead.to_secs_f64() - audio_clip.timeline_in.to_secs_f64()).max(0.0);

                    let audio_eval_ctx = klipp_core::EvalContext {
                        clip_time_secs,
                        clip_duration_secs: (audio_clip.source_out.to_secs_f64() - audio_clip.source_in.to_secs_f64()),
                        timeline_time_secs: self.playhead.to_secs_f64(),
                        fps: self.fps.as_f64(),
                        seed: audio_clip.id.0.as_u64_pair().0,
                    };

                    clip_volume = eval_with_modifiers(&audio_clip.volume_keyframes, &audio_clip.param_modifiers, &klipp_core::ParamId::Volume, clip_time_secs, audio_clip.volume, &audio_eval_ctx);
                    clip_pan = eval_with_modifiers(&audio_clip.pan_keyframes, &audio_clip.param_modifiers, &klipp_core::ParamId::Pan, clip_time_secs, audio_clip.pan, &audio_eval_ctx);
                    let mut clip_gain = eval_with_modifiers(&audio_clip.gain_keyframes, &audio_clip.param_modifiers, &klipp_core::ParamId::Gain, clip_time_secs, audio_clip.gain, &audio_eval_ctx);
                    let mut fade_in = audio_clip.fade_in_secs;
                    let mut fade_out = audio_clip.fade_out_secs;
                    let mut fi_curve = audio_clip.fade_in_curve;
                    let mut fo_curve = audio_clip.fade_out_curve;
                    let mut ch_config = audio_clip.channel_config;
                    let source_in_secs = audio_clip.source_in.to_secs_f64();
                    let clip_dur_secs = audio_clip.duration().to_secs_f64();

                    // If linked to a video clip, use the video clip's audio params
                    if let Some(linked_id) = audio_clip.linked_clip_id {
                        for t in &seq.tracks {
                            if let Some(video_clip) = t.clips.iter().find(|c| c.id == linked_id) {
                                let linked_clip_time = (self.playhead.to_secs_f64() - video_clip.timeline_in.to_secs_f64()).max(0.0);
                                let linked_eval_ctx = klipp_core::EvalContext {
                                    clip_time_secs: linked_clip_time,
                                    clip_duration_secs: (video_clip.source_out.to_secs_f64() - video_clip.source_in.to_secs_f64()),
                                    timeline_time_secs: self.playhead.to_secs_f64(),
                                    fps: self.fps.as_f64(),
                                    seed: video_clip.id.0.as_u64_pair().0,
                                };
                                clip_volume = eval_with_modifiers(&video_clip.volume_keyframes, &video_clip.param_modifiers, &klipp_core::ParamId::Volume, linked_clip_time, video_clip.volume, &linked_eval_ctx);
                                clip_pan = eval_with_modifiers(&video_clip.pan_keyframes, &video_clip.param_modifiers, &klipp_core::ParamId::Pan, linked_clip_time, video_clip.pan, &linked_eval_ctx);
                                clip_gain = eval_with_modifiers(&video_clip.gain_keyframes, &video_clip.param_modifiers, &klipp_core::ParamId::Gain, linked_clip_time, video_clip.gain, &linked_eval_ctx);
                                fade_in = video_clip.fade_in_secs;
                                fade_out = video_clip.fade_out_secs;
                                fi_curve = video_clip.fade_in_curve;
                                fo_curve = video_clip.fade_out_curve;
                                ch_config = video_clip.channel_config;
                                break;
                            }
                        }
                    }

                    // Set fade params on the audio source
                    // When contiguous_audio is true, offset source_in backward so the
                    // fade envelope doesn't think we're at position 0 (which would
                    // trigger a fade-in from silence at blade cut boundaries)
                    let is_contiguous = self.contiguous_audio.load(std::sync::atomic::Ordering::Relaxed);
                    let effective_source_in = if is_contiguous {
                        source_in_secs - 1.0 // pretend clip started 1s earlier
                    } else {
                        source_in_secs
                    };
                    let effective_clip_dur = if is_contiguous {
                        clip_dur_secs + 1.0 // extend duration to match
                    } else {
                        clip_dur_secs
                    };
                    active.source.set_fade(
                        effective_source_in,
                        effective_clip_dur,
                        fade_in as f64,
                        fade_out as f64,
                        fade_curve_to_u32(fi_curve),
                        fade_curve_to_u32(fo_curve),
                        clip_gain,
                    );
                    // Clear contiguous flag — it's consumed
                    self.contiguous_audio.store(false, std::sync::atomic::Ordering::Relaxed);
                    clip_gain_out = clip_gain;
                    clip_ch_config = channel_config_to_u32(ch_config);
                    break;
                }
            }
        }
        drop(seq);
        if let Some(aout) = &self.audio_output {
            aout.0.set_clip_gain(clip_gain_out);
            aout.0.set_clip_volume(clip_volume);
            aout.0.set_clip_pan(clip_pan);
            aout.0.set_channel_config(clip_ch_config);
        }
    }

    /// Stop and drop all audio sources, mix thread, and the output.
    fn stop_all_audio(&mut self) {
        // Clear mix thread staging first (stop reading from source ring buffers)
        if let Some(ref mt) = self.mix_thread {
            mt.clear_all();
        }
        for active in &mut self.audio_sources {
            active.source.stop_detached();
        }
        self.audio_sources.clear();
        // Drop mix thread and audio output
        self.mix_thread = None;
        self.audio_output = None;
        self.expected_audio_generation = 0;
    }

    /// Open one audio source for a given track and add it to audio_sources.
    /// Creates mix thread + audio output if not already present.
    /// The source starts PAUSED — caller is responsible for resuming.
    fn open_audio_source(
        &mut self,
        track_index: usize,
        media_id: MediaId,
        path: &std::path::Path,
        source_secs: f64,
    ) {
        // Try PCM cache first (instant seeks, zero codec overhead)
        let pcm_path = self.pcm_cache_path(media_id);
        if let Some(ref pcm) = pcm_path {
            if pcm.exists() {
                match PcmAudioFeeder::open(pcm, self.cpal_sample_rate) {
                    Ok((feeder, consumer)) => {
                        if source_secs > 0.01 {
                            feeder.seek(source_secs);
                        }
                        // Source starts paused — caller resumes when ready

                        self.ensure_mix_pipeline(consumer, track_index);
                        self.audio_sources.push(ActiveAudioSource {
                            track_index,
                            source: AudioSource::PcmFeeder(feeder),
                        });
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
                // Source starts paused — caller resumes when ready

                self.ensure_mix_pipeline(consumer, track_index);
                self.audio_sources.push(ActiveAudioSource {
                    track_index,
                    source: AudioSource::Decoder(adec),
                });
            }
            Err(e) => {
                debug!("No audio for this clip: {e}");
            }
        }
    }

    /// Ensure the mix thread and audio output exist, then register
    /// the given staging consumer for a track.
    fn ensure_mix_pipeline(
        &mut self,
        staging_consumer: crate::audio_decoder::AudioConsumer,
        track_index: usize,
    ) {
        // Create mix thread + audio output on first use
        if self.mix_thread.is_none() {
            let (mt, main_consumer) =
                MixThread::new(self.cpal_sample_rate, self.mixer.clone());
            match AudioOutput::new(main_consumer, self.skim_consumer.clone()) {
                Ok(output) => {
                    self.audio_output = Some(SendAudioOutput(output));
                }
                Err(e) => {
                    warn!("Failed to create audio output: {e}");
                }
            }
            self.mix_thread = Some(mt);
        }

        // Register this source's consumer with the mix thread
        if let Some(ref mt) = self.mix_thread {
            mt.add_staging(track_index, staging_consumer);
        }
    }

    /// Look up the PCM cache path for a media item.
    fn pcm_cache_path(&self, media_id: MediaId) -> Option<std::path::PathBuf> {
        let pool = self.media_pool.read().ok()?;
        let item = pool.get(&media_id)?;
        item.audio_cache.as_ref().map(|c| c.pcm_path.clone())
    }

    /// Seek all audio sources to match the clip at `tc` on each source's track.
    /// Flushes the mix pipeline, bumps seek_gen, and waits for all sources to confirm.
    fn seek_all_audio_to_timecode(&mut self, tc: Timecode) {
        if self.audio_sources.is_empty() {
            return;
        }

        // Flush mix thread staging BEFORE telling sources to seek
        if let Some(ref mt) = self.mix_thread {
            mt.flush();
        }

        // Compute per-track seek positions from the timeline
        let seek_positions: Vec<(usize, f64)> = {
            let seq = self.sequence.read().unwrap();
            self.audio_sources
                .iter()
                .filter_map(|active| {
                    let track = seq.tracks.get(active.track_index)?;
                    let (_idx, clip) = track.clip_at_timecode(tc)?;
                    let source_secs = clip.source_in.to_secs_f64()
                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed;
                    Some((active.track_index, source_secs))
                })
                .collect()
        };

        self.expected_audio_generation += 1;
        for active in &self.audio_sources {
            if let Some((_, secs)) = seek_positions.iter().find(|(idx, _)| *idx == active.track_index) {
                active.source.seek(*secs);
            }
        }

        // Tell cpal callback to drain stale main ring buffer samples
        if let Some(aout) = &self.audio_output {
            aout.0.notify_seek();
        }

        // Wait for all audio sources to confirm seek
        let deadline = std::time::Instant::now() + Duration::from_millis(50);
        loop {
            let all_ready = self.audio_sources.iter().all(|a| {
                a.source.seek_generation().load(Ordering::Acquire)
                    >= self.expected_audio_generation
            });
            if all_ready {
                break;
            }
            if std::time::Instant::now() >= deadline {
                debug!(
                    "audio seek timeout: expected gen={} — proceeding anyway",
                    self.expected_audio_generation
                );
                break;
            }
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    fn update_playback(&mut self) {
        let t_update = std::time::Instant::now();

        // Mute audio when clip speed is not 1.0 (speed ramp, slow-mo, etc.)
        let should_mute = (self.clip_speed - 1.0).abs() > 0.01;
        for active in &self.audio_sources {
            active.source.muted().store(should_mute, Ordering::Release);
        }

        // Master clock is the single source of truth.
        let current_source_secs = self.master_clock.current_secs();
        // Frame presentation tolerance: half a frame duration (scales with FPS)
        let frame_tolerance = self.fps.frame_duration_secs() * 0.5;
        let _clock_running = self.master_clock.is_running();

        // Title clip path — standalone title on V1 (no media decoder active)
        if self.v1_decoder.is_none() && self.current_file.is_none() {
            let seq = self.sequence.read().unwrap();
            let title_clip = seq.tracks.iter()
                .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Text))
                .find_map(|track| {
                    track.clips.get(self.current_clip_index).and_then(|clip| {
                        clip.title_data().map(|td| (clip.clone(), td.clone()))
                    })
                });
            drop(seq);

            if let Some((clip, title_data)) = title_clip {
                // Update playhead from clock (divide by speed for timeline time)
                let elapsed_secs = current_source_secs - self.clip_source_start;
                let speed = self.clip_speed.max(0.001);
                let elapsed_tc = RationalTime::from_secs_f64((elapsed_secs / speed).max(0.0), self.fps.rate());
                self.playhead = clip.timeline_in + elapsed_tc;

                // Check if past end of title clip
                if self.playhead >= clip.timeline_out {
                    self.playhead = clip.timeline_out;
                    self.advance_to_next_clip();
                    return;
                }

                // Render title frame at sequence resolution
                let seq = self.sequence.read().unwrap();
                let (w, h) = (seq.resolution.width, seq.resolution.height);
                drop(seq);
                let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
                let rgba = self.title_renderer.render_with_gpu(&title_data, w, h, gpu);

                // Apply post-processing (color grade, effects, LUT)
                let final_rgba = self.post_process_rgba(rgba, w, h);

                // Write to frame buffer
                let mut fb = self.frame_buffer.lock().unwrap();
                fb.write_frame(final_rgba, w, h);
                drop(fb);

                self.frames_consumed += 1;
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                (self.repaint_cb)();
                return;
            }
        }

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
                                debug!(
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
                if self.v1_decoder.is_none() && !self.audio_sources.is_empty() {
                    // Audio-only playback: update playhead from clock
                    let playhead_secs = current_source_secs;
                    let new_playhead = RationalTime::from_secs_f64(playhead_secs, self.fps.rate());
                    self.playhead = new_playhead;

                    // Check if past end of all audio clips
                    let seq_dur = self.sequence_duration();
                    if self.playhead >= seq_dur {
                        self.stop_at_end();
                        return;
                    }

                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    (self.repaint_cb)();
                    return;
                }

                // No new frame this tick — still update playhead from clock
                // so the timeline playhead stays smooth during slow-mo
                let speed = self.clip_speed.max(0.001);
                let elapsed_secs = current_source_secs - self.clip_source_start;
                let elapsed_tc = RationalTime::from_secs_f64((elapsed_secs / speed).max(0.0), self.fps.rate());
                self.playhead = self.clip_timeline_start + elapsed_tc;
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                (self.repaint_cb)();
                return;
            }
        };

        if frame.is_eof {
            debug!("EOF sentinel received — advancing to next clip");
            self.advance_to_next_clip();
            return;
        }

        // Update playhead from the frame's PTS relative to clip timeline
        // Search ALL video tracks for the current clip
        let seq = self.sequence.read().unwrap();
        let found_clip = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|track| track.clips.get(self.current_clip_index).cloned());
        drop(seq);

        if found_clip.is_none() {
            info!(
                "CUT-DIAG STALE INDEX: clip_idx={} not found, pts={:.4}s clock={:.4}s — skipping frame",
                self.current_clip_index, frame.pts_secs(), current_source_secs,
            );
            return;
        }
        if let Some(clip) = found_clip {
            let source_elapsed_secs = frame.pts_secs() - clip.source_in.to_secs_f64();
            // Convert source elapsed time to timeline elapsed time (divide by speed)
            let speed = clip.speed.max(0.001);
            let timeline_elapsed_secs = source_elapsed_secs / speed;
            let elapsed_tc = RationalTime::from_secs_f64(
                timeline_elapsed_secs.max(0.0),
                self.fps.rate(),
            );
            self.playhead = clip.timeline_in + elapsed_tc;

            // Loop out-point detection: if loop enabled and playhead crossed out_point, loop back
            if self.loop_enabled {
                let seq = self.sequence.read().unwrap();
                let out_pt = seq.out_point;
                let in_pt = seq.in_point;
                drop(seq);

                if let Some(out) = out_pt {
                    let out_secs = out.to_secs_f64();
                    let playhead_secs = self.playhead.to_secs_f64();
                    let in_secs = in_pt.map(|t| t.to_secs_f64()).unwrap_or(0.0);
                    let range_duration = out_secs - in_secs;

                    // Predictive pre-seek: start 200ms before out_point if range >= 1.0s
                    if range_duration >= 1.0
                        && !self.pre_seek_started
                        && playhead_secs >= out_secs - 0.200
                        && playhead_secs < out_secs
                    {
                        self.begin_pre_seek(in_secs);
                    }

                    // Poll pre-seek result (non-blocking) — consumed by execute_loop
                    if let Ok(guard) = self.pre_seek_result.try_lock() {
                        drop(guard);
                    }

                    if playhead_secs >= out_secs {
                        // Target: in_point if set, else start
                        let target = in_pt.unwrap_or_else(|| RationalTime { value: 0, rate: self.fps.rate() });
                        self.execute_loop(target);
                        return;
                    }
                }
            }

            // Look-ahead: pre-decode next clip when within 1.5s of source_out
            let dist_to_out = clip.source_out.to_secs_f64() - frame.pts_secs();
            if dist_to_out < 1.5 && dist_to_out > 0.0 && self.preroll_decoder.is_none() {
                self.start_preroll_for_next_clip();
            }

            // Check if we've passed source_out
            if frame.pts_secs() >= clip.source_out.to_secs_f64() - 0.01 {
                info!("CUT-DIAG BOUNDARY HIT: pts={:.4}s source_out={:.4}s timeline_out={:.4}s clock={:.4}s",
                    frame.pts_secs(), clip.source_out.to_secs_f64(),
                    clip.timeline_out.to_secs_f64(), current_source_secs);
                self.playhead = clip.timeline_out;
                self.advance_to_next_clip();
                return;
            }
        }

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
        let t_composite = std::time::Instant::now();
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

        let composite_ms = t_composite.elapsed().as_secs_f64() * 1000.0;
        if composite_ms > 2.0 {
            debug!("PERF composite+into_rgba: {composite_ms:.1}ms");
        }

        // Check for active transition and blend if needed
        let final_rgba = self.apply_transition_if_active(final_rgba, final_w, final_h);

        // Apply full post-processing pipeline (color grade → OFX → builtin FX → global LUT)
        let final_rgba = self.post_process_rgba(final_rgba, final_w, final_h);

        // Write frame to the shared FrameBuffer
        let mut fb = self.frame_buffer.lock().unwrap();
        fb.write_frame(final_rgba, final_w, final_h);
        drop(fb);

        self.frames_consumed += 1;

        let update_ms = t_update.elapsed().as_secs_f64() * 1000.0;
        if update_ms > 5.0 {
            debug!(
                "PERF update_playback: {update_ms:.1}ms for {}x{} frame",
                final_w, final_h
            );
        }

        // Periodic playback status (every 2 seconds)
        if self.last_playback_log.elapsed().as_secs() >= 2 {
            let elapsed = self.last_playback_log.elapsed().as_secs_f64();
            debug!(
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

        // Find the track that contains the current clip, then look for the next clip
        // on the SAME track. This ensures contiguous detection compares adjacent clips
        // on one track, not clips at the same index across different tracks.
        let next_idx = self.current_clip_index + 1;
        let (current_clip_info, next_clip_info) = {
            let mut cur_info = None;
            let mut next_info = None;
            for track in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
                if let Some(c) = track.clips.get(self.current_clip_index) {
                    if let Some(mid) = c.media_id() {
                        cur_info = Some((mid, c.source_out.to_secs_f64()));
                        // Search for next non-gap clip on THIS SAME track
                        for idx in next_idx..track.clips.len() {
                            let clip = &track.clips[idx];
                            if let Some(media_id) = clip.media_id() {
                                next_info = Some((idx, media_id, clip.source_in.to_secs_f64(), clip.timeline_in, clip.speed));
                                break;
                            }
                        }
                        break; // Found the track with current clip — stop searching
                    }
                }
            }
            (cur_info, next_info)
        };
        drop(seq);

        if let Some((actual_idx, media_id, source_in_secs, timeline_in, next_clip_speed)) = next_clip_info {
            // Contiguous clip optimization: if the next clip uses the same media file
            // and its source_in matches the current clip's source_out (e.g. after a blade cut),
            // keep the decoder running — no seek, no flush, no stall.
            let is_contiguous = current_clip_info.as_ref().is_some_and(|(cur_media_id, cur_source_out)| {
                *cur_media_id == media_id && (*cur_source_out - source_in_secs).abs() < 0.02
            });
            info!(
                "CUT-DIAG advance: cur_clip={:?} next_idx={} next_media={:?} next_src_in={:.4}s is_contiguous={} has_decoder={}",
                current_clip_info.as_ref().map(|(mid, so)| format!("media={:?} src_out={:.4}", mid, so)),
                actual_idx, media_id, source_in_secs, is_contiguous, self.v1_decoder.is_some(),
            );

            if is_contiguous && self.v1_decoder.is_some() {
                info!(
                    "CUT-DIAG CONTIGUOUS: old_idx={} new_idx={} source_in={:.4}s clock={:.4}s",
                    self.current_clip_index, actual_idx, source_in_secs,
                    self.master_clock.current_secs(),
                );
                self.current_clip_index = actual_idx;
                self.playhead = timeline_in;
                self.clip_source_start = source_in_secs;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = next_clip_speed;
                self.contiguous_audio.store(true, std::sync::atomic::Ordering::Relaxed); // suppress fade-in on next sync
                self.last_frame_time = std::time::Instant::now();
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));

                // CRITICAL: Immediately update fade parameters on audio sources so the
                // PCM feeder produces correct audio going forward. Without this, the feeder
                // continues with clip A's parameters, applying fade-out to samples past
                // clip A's end, filling the ring buffer with ~2s of silence.
                {
                    let seq = self.sequence.read().unwrap();
                    for active_src in &self.audio_sources {
                        if let Some(track) = seq.tracks.get(active_src.track_index) {
                            if let Some((_idx, new_clip)) = track.clip_at_timecode(self.playhead) {
                                let new_source_in = new_clip.source_in.to_secs_f64();
                                let new_clip_dur = new_clip.duration().to_secs_f64();
                                // Offset source_in backward so fade_envelope doesn't trigger fade-in
                                active_src.source.set_fade(
                                    new_source_in - 1.0,
                                    new_clip_dur + 1.0,
                                    new_clip.fade_in_secs as f64,
                                    new_clip.fade_out_secs as f64,
                                    fade_curve_to_u32(new_clip.fade_in_curve),
                                    fade_curve_to_u32(new_clip.fade_out_curve),
                                    new_clip.gain,
                                );
                            }
                        }
                    }
                }

                // Do NOT flush or notify_seek here — that would empty the pipeline
                // and create a 2-3 second silence while the ring buffer refills.
                // Instead, let the existing buffered audio play naturally. The immediate
                // set_fade() above ensures the feeder produces correct audio going forward.
                // At most ~10-40ms of silence-faded audio may play before new params take effect.

                // V2 may need updating
                self.maybe_open_v2_decoder(self.playhead);
                return;
            }

            self.current_clip_index = actual_idx;
            self.playhead = timeline_in;

            // Preroll hit: if we pre-decoded this clip, swap in the ready decoder — zero stall.
            if self.preroll_clip_index == Some(actual_idx) {
                if let Some(preroll_dec) = self.preroll_decoder.take() {
                    info!("CUT-DIAG PREROLL HIT: swapping in pre-decoded clip idx={} source_in={:.4}s", actual_idx, source_in_secs);
                    // Stop old V1 decoder
                    if let Some(dec) = &mut self.v1_decoder {
                        dec.stop_detached();
                    }
                    self.v1_decoder = Some(preroll_dec);
                    self.preroll_clip_index = None;

                    // Update file path
                    if let Some(path) = self.path_for_media(media_id) {
                        self.current_file = Some(path);
                    }

                    // Seek audio to the new position
                    self.seek_all_audio_to_timecode(self.playhead);

                    let start_us = (source_in_secs * 1_000_000.0) as u64;
                    self.master_clock.seek(start_us);
                    self.master_clock.resume();

                    self.clip_source_start = source_in_secs;
                    self.clip_timeline_start = timeline_in;
                    self.clip_speed = next_clip_speed;
                    self.last_frame_time = std::time::Instant::now();
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    self.maybe_open_v2_decoder(self.playhead);
                    return;
                }
            }
            // Clear stale preroll if it didn't match
            if let Some(mut old) = self.preroll_decoder.take() {
                old.stop_detached();
            }
            self.preroll_clip_index = None;

            if let Some(path) = self.path_for_media(media_id) {
                // Same-file optimization: if next clip is from the same file,
                // reuse the decoder (just seek) instead of tearing down and reopening.
                let same_file = self.current_file.as_ref() == Some(&path);

                if same_file && self.v1_decoder.is_some() {
                    info!("CUT-DIAG SAME-FILE seek: idx={} source_in={:.4}s (reusing decoder)", actual_idx, source_in_secs);
                    // Seek existing decoder — much faster than close+reopen
                    if let Some(dec) = &self.v1_decoder {
                        dec.seek(source_in_secs);
                        dec.frame_queue().lock().unwrap().clear();
                    }

                    // Pause clock while waiting for the decoder to produce a post-seek frame.
                    self.master_clock.pause();
                    self.wait_for_valid_frame(source_in_secs);

                    // Seek audio to the new clip position
                    self.seek_all_audio_to_timecode(self.playhead);

                    let start_us = (source_in_secs * 1_000_000.0) as u64;
                    self.master_clock.seek(start_us);
                    self.master_clock.resume();

                    self.clip_source_start = source_in_secs;
                    self.clip_timeline_start = timeline_in;
                    self.clip_speed = next_clip_speed;
                    self.last_frame_time = std::time::Instant::now();
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    self.maybe_open_v2_decoder(self.playhead);
                    return;
                }

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
                for active in &mut self.audio_sources {
                    active.source.stop_detached();
                }
                self.audio_sources.clear();
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

                // Open audio for all tracks at the new clip position
                // (sources start paused, already seeked — no stale data)
                let start_us = (source_in_secs * 1_000_000.0) as u64;
                self.master_clock.seek(start_us);
                self.start_all_audio(self.playhead);

                // Pre-fill + resume
                for active in &self.audio_sources {
                    active.source.resume();
                }
                std::thread::sleep(Duration::from_millis(10));
                if let Some(aout) = &self.audio_output {
                    aout.0.resume();
                }
                self.master_clock.resume();

                self.clip_source_start = source_in_secs;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = next_clip_speed;
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

    /// Start pre-decoding the next clip so frames are ready when the cut arrives.
    fn start_preroll_for_next_clip(&mut self) {
        let seq = self.sequence.read().unwrap();
        let next_idx = self.current_clip_index + 1;

        // Find next non-gap clip on the SAME track as the current clip
        let next_info = {
            let mut result = None;
            for track in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
                if track.clips.get(self.current_clip_index).and_then(|c| c.media_id()).is_some() {
                    for idx in next_idx..track.clips.len() {
                        if let Some(media_id) = track.clips[idx].media_id() {
                            result = Some((idx, media_id, track.clips[idx].source_in.to_secs_f64()));
                            break;
                        }
                    }
                    break;
                }
            }
            result
        };
        drop(seq);

        if let Some((idx, media_id, source_in_secs)) = next_info {
            if self.preroll_clip_index == Some(idx) {
                return; // Already pre-rolling this clip
            }
            if let Some(path) = self.path_for_media(media_id) {
                match VideoDecoder::open(&path) {
                    Ok(dec) => {
                        if source_in_secs > 0.01 {
                            dec.seek(source_in_secs);
                        }
                        dec.resume();
                        info!("CUT-DIAG PREROLL started: idx={} source_in={:.4}s", idx, source_in_secs);
                        // Close any previous preroll
                        if let Some(mut old) = self.preroll_decoder.take() {
                            old.stop_detached();
                        }
                        self.preroll_decoder = Some(dec);
                        self.preroll_clip_index = Some(idx);
                        self.preroll_source_in = source_in_secs;
                    }
                    Err(e) => {
                        warn!("Failed to open preroll decoder: {e}");
                    }
                }
            }
        }
    }

    /// Check if Video 2 has a clip at the given playhead position.
    /// Opens/closes the V2 decoder as needed.
    fn maybe_open_v2_decoder(&mut self, playhead: Timecode) {
        let seq = self.sequence.read().unwrap();
        // Check ALL overlay video/text tracks for clips at playhead.
        // Text tracks are always overlays. Video tracks are overlays if not the primary V1 track.
        let primary_v1_idx = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video);
        let overlay_info = seq.tracks.iter()
            .enumerate()
            .filter(|(i, t)| {
                !t.muted && match t.kind {
                    klipp_timeline::TrackKind::Text | klipp_timeline::TrackKind::Subtitle => true,
                    klipp_timeline::TrackKind::Video => primary_v1_idx.map_or(true, |v1| *i != v1),
                    _ => false,
                }
            })
            .find_map(|(track_idx, track)| {
                track.clip_at_timecode(playhead).map(|(idx, clip)| {
                    (track_idx, idx, clip.clone())
                })
            });
        let res = seq.resolution;
        drop(seq);

        match overlay_info {
            Some((_track_idx, idx, clip)) => {
                // Title clip on overlay track — render directly to v2_frame_cache
                if let Some(title_data) = clip.title_data() {
                    // Close media V2 decoder if open
                    if self.v2_decoder.is_some() {
                        if let Some(dec) = &mut self.v2_decoder {
                            dec.stop_detached();
                        }
                        self.v2_decoder = None;
                    }
                    let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
                    let mut rgba = self.title_renderer.render_with_gpu(title_data, res.width, res.height, gpu);

                    // Apply opacity + transform keyframes + modifiers to title overlay
                    let clip_relative_secs = (playhead - clip.timeline_in).to_secs_f64().max(0.0);
                    let eval_ctx = klipp_core::EvalContext {
                        clip_time_secs: clip_relative_secs,
                        clip_duration_secs: (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()),
                        timeline_time_secs: playhead.to_secs_f64(),
                        fps: self.fps.as_f64(),
                        seed: clip.id.0.as_u64_pair().0,
                    };
                    let opacity_default = clip.builtin_effects.iter()
                        .find_map(|e| match e { klipp_effects::BuiltinEffect::Opacity { value } => Some(*value), _ => None })
                        .unwrap_or(1.0);
                    let opacity = match clip.param_modifiers.get(&klipp_core::ParamId::Opacity) {
                        Some(stack) if !stack.modifiers.is_empty() =>
                            klipp_core::evaluate_param(&clip.opacity_keyframes, stack, clip_relative_secs, opacity_default, &eval_ctx),
                        _ => klipp_core::keyframe::evaluate(&clip.opacity_keyframes, clip_relative_secs, opacity_default),
                    };
                    if (opacity - 1.0).abs() > 0.001 {
                        let alpha_mul = (opacity * 255.0).clamp(0.0, 255.0) as u8;
                        for pixel in rgba.chunks_exact_mut(4) {
                            pixel[3] = ((pixel[3] as u16 * alpha_mul as u16) / 255) as u8;
                        }
                    }

                    // Apply transform keyframes + modifiers (position, scale, rotation)
                    let has_transform = !clip.transform_pos_x_keyframes.is_empty()
                        || !clip.transform_pos_y_keyframes.is_empty()
                        || !clip.transform_scale_keyframes.is_empty()
                        || !clip.transform_rotation_keyframes.is_empty()
                        || clip.param_modifiers.contains_key(&klipp_core::ParamId::PositionX)
                        || clip.param_modifiers.contains_key(&klipp_core::ParamId::PositionY)
                        || clip.param_modifiers.contains_key(&klipp_core::ParamId::Scale)
                        || clip.param_modifiers.contains_key(&klipp_core::ParamId::Rotation);

                    if has_transform {
                        let tdef = clip.builtin_effects.iter()
                            .find_map(|e| match e {
                                klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y } =>
                                    Some((*position_x, *position_y, *scale_x, *scale_y, *rotation, *anchor_x, *anchor_y)),
                                _ => None,
                            })
                            .unwrap_or((0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5));

                        // Build eval context for modifier evaluation
                        let eval_ctx = klipp_core::EvalContext {
                            clip_time_secs: clip_relative_secs,
                            clip_duration_secs: (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()),
                            timeline_time_secs: playhead.to_secs_f64(),
                            fps: self.fps.as_f64(),
                            seed: clip.id.0.as_u64_pair().0,
                        };

                        // Check for motion blur on animated title
                        let motion_blur = title_data.motion_blur.filter(|mb| mb.samples >= 2);

                        if let Some(mb) = motion_blur {
                            // Multi-sample motion blur: render transform at N time offsets and blend
                            let frame_dur = self.fps.frame_duration_secs();
                            let half_shutter = (mb.shutter_angle as f64 / 360.0) * frame_dur;
                            let n = mb.samples as usize;
                            let pixel_count = (res.width * res.height) as usize * 4;
                            let mut accum = vec![0u32; pixel_count];

                            for si in 0..n {
                                let t_offset = if n > 1 {
                                    (si as f64 / (n - 1) as f64 - 0.5) * 2.0 * half_shutter
                                } else {
                                    0.0
                                };
                                let sample_t = (clip_relative_secs + t_offset).max(0.0);
                                let sample_eval = klipp_core::EvalContext {
                                    clip_time_secs: sample_t,
                                    ..eval_ctx
                                };

                                let spx = match clip.param_modifiers.get(&klipp_core::ParamId::PositionX) {
                                    Some(stack) if !stack.modifiers.is_empty() =>
                                        klipp_core::evaluate_param(&clip.transform_pos_x_keyframes, stack, sample_t, tdef.0, &sample_eval),
                                    _ => klipp_core::keyframe::evaluate(&clip.transform_pos_x_keyframes, sample_t, tdef.0),
                                };
                                let spy = match clip.param_modifiers.get(&klipp_core::ParamId::PositionY) {
                                    Some(stack) if !stack.modifiers.is_empty() =>
                                        klipp_core::evaluate_param(&clip.transform_pos_y_keyframes, stack, sample_t, tdef.1, &sample_eval),
                                    _ => klipp_core::keyframe::evaluate(&clip.transform_pos_y_keyframes, sample_t, tdef.1),
                                };
                                let ssc = match clip.param_modifiers.get(&klipp_core::ParamId::Scale) {
                                    Some(stack) if !stack.modifiers.is_empty() =>
                                        klipp_core::evaluate_param(&clip.transform_scale_keyframes, stack, sample_t, tdef.2, &sample_eval),
                                    _ => klipp_core::keyframe::evaluate(&clip.transform_scale_keyframes, sample_t, tdef.2),
                                };
                                let sro = match clip.param_modifiers.get(&klipp_core::ParamId::Rotation) {
                                    Some(stack) if !stack.modifiers.is_empty() =>
                                        klipp_core::evaluate_param(&clip.transform_rotation_keyframes, stack, sample_t, tdef.4, &sample_eval),
                                    _ => klipp_core::keyframe::evaluate(&clip.transform_rotation_keyframes, sample_t, tdef.4),
                                };

                                let sample_rgba = klipp_effects::transform::apply(
                                    rgba.clone(), res.width, res.height,
                                    spx, spy, ssc, ssc, sro, tdef.5, tdef.6,
                                );
                                for (i, &b) in sample_rgba.iter().enumerate() {
                                    accum[i] += b as u32;
                                }
                            }
                            rgba = accum.iter().map(|v| (*v / n as u32) as u8).collect();
                        } else {
                            // No motion blur — single transform
                            let px = match clip.param_modifiers.get(&klipp_core::ParamId::PositionX) {
                                Some(stack) if !stack.modifiers.is_empty() =>
                                    klipp_core::evaluate_param(&clip.transform_pos_x_keyframes, stack, clip_relative_secs, tdef.0, &eval_ctx),
                                _ => klipp_core::keyframe::evaluate(&clip.transform_pos_x_keyframes, clip_relative_secs, tdef.0),
                            };
                            let py = match clip.param_modifiers.get(&klipp_core::ParamId::PositionY) {
                                Some(stack) if !stack.modifiers.is_empty() =>
                                    klipp_core::evaluate_param(&clip.transform_pos_y_keyframes, stack, clip_relative_secs, tdef.1, &eval_ctx),
                                _ => klipp_core::keyframe::evaluate(&clip.transform_pos_y_keyframes, clip_relative_secs, tdef.1),
                            };
                            let sc = match clip.param_modifiers.get(&klipp_core::ParamId::Scale) {
                                Some(stack) if !stack.modifiers.is_empty() =>
                                    klipp_core::evaluate_param(&clip.transform_scale_keyframes, stack, clip_relative_secs, tdef.2, &eval_ctx),
                                _ => klipp_core::keyframe::evaluate(&clip.transform_scale_keyframes, clip_relative_secs, tdef.2),
                            };
                            let ro = match clip.param_modifiers.get(&klipp_core::ParamId::Rotation) {
                                Some(stack) if !stack.modifiers.is_empty() =>
                                    klipp_core::evaluate_param(&clip.transform_rotation_keyframes, stack, clip_relative_secs, tdef.4, &eval_ctx),
                                _ => klipp_core::keyframe::evaluate(&clip.transform_rotation_keyframes, clip_relative_secs, tdef.4),
                            };

                            rgba = klipp_effects::transform::apply(rgba, res.width, res.height, px, py, sc, sc, ro, tdef.5, tdef.6);
                        }
                    }

                    self.v2_frame_cache = Some(crate::types::DecodedFrame {
                        data: crate::types::FrameData::Rgba(rgba),
                        width: res.width,
                        height: res.height,
                        pts: RationalTime::from_frames(0, 24),
                        is_eof: false,
                    });
                    self.current_clip_v2 = Some(idx);
                    return;
                }

                // Media clip on overlay track
                if let Some(media_id) = clip.media_id() {
                    let source_secs = clip.source_in.to_secs_f64()
                        + (playhead - clip.timeline_in).to_secs_f64() * clip.speed;
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
            }
            None => {
                // No overlay clip at playhead — close decoder if open
                if self.v2_decoder.is_some() {
                    if let Some(dec) = &mut self.v2_decoder {
                        dec.stop_detached();
                    }
                    self.v2_decoder = None;
                    self.current_clip_v2 = None;
                    self.v2_frame_cache = None;
                    debug!("V2 decoder closed (no clip at playhead)");
                } else if self.v2_frame_cache.is_some() && self.current_clip_v2.is_some() {
                    // Clear cached title frame
                    self.v2_frame_cache = None;
                    self.current_clip_v2 = None;
                }
            }
        }
    }

    fn stop_at_end(&mut self) {
        info!("stop_at_end called — loop_enabled={}, playhead={:.3}s",
            self.loop_enabled, self.playhead.to_secs_f64());
        // Loop: if enabled, seek back to in_point (or start)
        if self.loop_enabled {
            let seq = self.sequence.read().unwrap();
            let in_pt = seq.in_point;
            drop(seq);
            let target = in_pt.unwrap_or_else(|| RationalTime { value: 0, rate: self.fps.rate() });
            self.execute_loop(target);
            return;
        }

        self.v1_decoder = None;
        self.v2_decoder = None;
        self.current_clip_v2 = None;
        self.v2_frame_cache = None;
        if let Some(dec) = &mut self.transition_decoder {
            dec.stop_detached();
        }
        self.transition_decoder = None;
        self.transition_clip_id = None;
        self.transition_frame_cache = None;
        self.stop_all_audio();
        self.state = PlaybackState::Paused;
        let _ = self.event_tx.send(AppEvent::PlaybackPaused);
        (self.repaint_cb)();
    }

    /// Execute a loop-back seek to the given target timecode.
    /// Modeled on seek_in_sequence_with_timeout() but always resumes playback.
    /// Begin predictive pre-seek: spawn background thread to open audio decoders
    /// at the loop target position, so they're ready when we reach the out_point.
    fn begin_pre_seek(&mut self, target_secs: f64) {
        self.pre_seek_started = true;
        let generation = self.pre_seek_generation;
        let result_arc = self.pre_seek_result.clone();
        let sequence = self.sequence.clone();
        let media_pool = self.media_pool.clone();
        let sample_rate = self.cpal_sample_rate;
        let use_proxies = self.use_proxies;

        debug!("[Loop] Starting pre-seek to {target_secs:.3}s (gen={generation})");

        std::thread::spawn(move || {
            let tc = RationalTime::from_secs_f64(target_secs, 24); // rate doesn't matter for lookup
            let seq = match sequence.read() {
                Ok(s) => s,
                Err(_) => return,
            };
            let pool = match media_pool.read() {
                Ok(p) => p,
                Err(_) => return,
            };

            // Collect audio track info at target position
            let track_audio: Vec<(usize, PathBuf, f64)> = seq
                .tracks
                .iter()
                .enumerate()
                .filter_map(|(track_idx, track)| {
                    let (_, clip) = track.clip_at_timecode(tc)?;
                    let media_id = clip.media_id()?;
                    let item = pool.get(&media_id)?;
                    if !item.has_audio {
                        return None;
                    }
                    // Skip audio for linked video-track clips
                    if track.kind == klipp_timeline::TrackKind::Video {
                        if let Some(lid) = clip.linked_clip_id {
                            let partner_on_audio = seq.tracks.iter().any(|t| {
                                t.kind == klipp_timeline::TrackKind::Audio
                                    && t.clips.iter().any(|c| c.id == lid)
                            });
                            if partner_on_audio {
                                return None;
                            }
                        }
                    }
                    let source_secs = clip.source_in.to_secs_f64()
                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed;

                    // Resolve path (proxy or original)
                    let decode_path = if use_proxies && item.proxy_ready {
                        item.proxy_path
                            .as_ref()
                            .filter(|p| p.exists())
                            .cloned()
                            .unwrap_or_else(|| item.path.clone())
                    } else {
                        item.path.clone()
                    };

                    // Try PCM cache first
                    let path = item
                        .audio_cache
                        .as_ref()
                        .map(|c| c.pcm_path.clone())
                        .filter(|p| p.exists())
                        .unwrap_or(decode_path);

                    Some((track_idx, path, source_secs))
                })
                .collect();
            drop(pool);
            drop(seq);

            // Open and seek audio sources
            let mut sources = Vec::new();
            for (track_idx, path, source_secs) in track_audio {
                // Try PCM cache
                if let Ok((feeder, consumer)) = PcmAudioFeeder::open(&path, sample_rate) {
                    if source_secs > 0.01 {
                        feeder.seek(source_secs);
                    }
                    sources.push(PreSeekAudioSource {
                        track_index: track_idx,
                        source: AudioSource::PcmFeeder(feeder),
                        consumer,
                    });
                    continue;
                }
                // Fallback: ffmpeg decode
                if let Ok((adec, consumer)) = AudioDecoder::open(&path, sample_rate) {
                    if source_secs > 0.01 {
                        adec.seek(source_secs);
                    }
                    sources.push(PreSeekAudioSource {
                        track_index: track_idx,
                        source: AudioSource::Decoder(adec),
                        consumer,
                    });
                }
            }

            // Wait for all sources to confirm seek (max 50ms)
            let deadline = std::time::Instant::now() + Duration::from_millis(50);
            loop {
                let all_ready = sources.iter().all(|s| {
                    s.source.seek_generation().load(Ordering::Acquire) >= 1
                });
                if all_ready || std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_micros(200));
            }

            // Resume sources so they start filling their ring buffers
            for s in &sources {
                s.source.resume();
            }
            // Let them decode for a bit (~50ms of audio)
            std::thread::sleep(Duration::from_millis(50));
            // Pause them — they've pre-filled their ring buffers
            for s in &sources {
                s.source.pause();
            }

            // Store result
            if let Ok(mut guard) = result_arc.lock() {
                *guard = Some(LoopPreSeekResult {
                    generation,
                    sources,
                    target_secs,
                });
            }
            debug!("[Loop] Pre-seek ready (gen={generation})");
        });
    }

    fn execute_loop(&mut self, target: Timecode) {
        debug!("[Loop] Executing loop-back to {:.3}s", target.to_secs_f64());
        let was_playing = self.state == PlaybackState::Playing;

        // Reset pre-seek state for next iteration
        self.pre_seek_started = false;

        // Try pre-seek fast path: use pre-opened audio sources if available
        let pre_seek = self.pre_seek_result.try_lock().ok().and_then(|mut g| g.take());
        if let Some(ps) = pre_seek {
            if ps.generation == self.pre_seek_generation {
                debug!("[Loop] Using pre-seeked audio sources (gen={})", ps.generation);

                // 1. Pause clock + cpal
                self.master_clock.pause();
                if let Some(aout) = &self.audio_output { aout.0.pause(); }

                // 2. Stop old audio sources, swap in pre-seeked ones
                self.stop_all_audio();
                for ps_source in ps.sources {
                    self.ensure_mix_pipeline(ps_source.consumer, ps_source.track_index);
                    self.audio_sources.push(ActiveAudioSource {
                        track_index: ps_source.track_index,
                        source: ps_source.source,
                    });
                }

                // 3. Seek video decoder (still needed — video wasn't pre-seeked)
                let seq = self.sequence.read().unwrap();
                let clip_info = seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|track| {
                        track.clip_at_timecode(target).and_then(|(idx, clip)| {
                            let media_id = clip.media_id()?;
                            let source_secs = clip.source_in.to_secs_f64()
                                + (target - clip.timeline_in).to_secs_f64() * clip.speed;
                            let clip_source_in = clip.source_in.to_secs_f64();
                            Some((idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed))
                        })
                    });
                drop(seq);

                let source_secs_for_clock = if let Some((idx, media_id, source_secs, timeline_in, clip_source_in, spd)) = clip_info {
                    let need_new = self.current_clip_index != idx || self.v1_decoder.is_none();
                    if need_new {
                        if let Some(path) = self.path_for_media(media_id) {
                            self.v1_decoder = None;
                            if let Ok(dec) = VideoDecoder::open(&path) {
                                dec.seek(source_secs);
                                dec.resume();
                                self.v1_decoder = Some(dec);
                                self.current_clip_index = idx;
                                self.current_file = Some(path);
                                self.clip_source_start = clip_source_in;
                                self.clip_timeline_start = timeline_in;
                                self.clip_speed = spd;
                            }
                        }
                    } else if let Some(dec) = &self.v1_decoder {
                        dec.seek(source_secs);
                        dec.frame_queue().lock().unwrap().clear();
                        self.clip_source_start = clip_source_in;
                        self.clip_timeline_start = timeline_in;
                        self.clip_speed = spd;
                    }
                    source_secs
                } else {
                    target.to_secs_f64()
                };

                // 4. Set master clock
                let target_us = (source_secs_for_clock * 1_000_000.0) as u64;
                self.master_clock.seek(target_us);

                // 5. Resume — audio already pre-filled, skip the 15ms wait
                if was_playing {
                    for active in &self.audio_sources { active.source.resume(); }
                    // Shorter pre-fill since sources already have buffered samples
                    std::thread::sleep(Duration::from_millis(5));
                    if let Some(aout) = &self.audio_output { aout.0.resume(); }
                    self.master_clock.resume();
                }

                // 6. V2 decoder update
                self.v2_frame_cache = None;
                self.maybe_open_v2_decoder(target);

                self.playhead = target;
                let _ = self.event_tx.send(AppEvent::LoopedBack);
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
                (self.repaint_cb)();
                return;
            } else {
                debug!("[Loop] Pre-seek stale (gen={} vs current={}), falling back to cold seek",
                    ps.generation, self.pre_seek_generation);
                // Drop stale pre-seek sources (they'll be cleaned up)
            }
        }

        // Cold seek fallback (no pre-seek available)

        // Find video clip at target position
        let seq = self.sequence.read().unwrap();
        let clip_info = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|track| {
                track.clip_at_timecode(target).and_then(|(idx, clip)| {
                    let media_id = clip.media_id()?;
                    let source_secs = clip.source_in.to_secs_f64()
                        + (target - clip.timeline_in).to_secs_f64() * clip.speed;
                    let clip_source_in = clip.source_in.to_secs_f64();
                    Some((idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed))
                })
            });
        // Also check for title clip
        let title_info = if clip_info.is_none() {
            seq.tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|track| {
                    track.clip_at_timecode(target).and_then(|(idx, clip)| {
                        let td = clip.title_data()?.clone();
                        Some((idx, td, clip.timeline_in, clip.source_in.to_secs_f64()))
                    })
                })
        } else {
            None
        };
        let res = seq.resolution;
        drop(seq);

        // Handle title clip at loop target
        if let Some((idx, title_data, timeline_in, clip_source_in)) = title_info {
            self.v1_decoder = None;
            self.current_clip_index = idx;
            self.current_file = None;
            self.clip_source_start = clip_source_in;
            self.clip_timeline_start = timeline_in;

            let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
            let rgba = self.title_renderer.render_with_gpu(&title_data, res.width, res.height, gpu);
            let final_rgba = self.post_process_rgba(rgba, res.width, res.height);
            let mut fb = self.frame_buffer.lock().unwrap();
            fb.write_frame(final_rgba, res.width, res.height);
            drop(fb);

            // Reopen audio at target
            self.master_clock.pause();
            if let Some(aout) = &self.audio_output { aout.0.pause(); }
            self.start_all_audio(target);
            self.seek_all_audio_to_timecode(target);
            let target_us = (target.to_secs_f64() * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            if was_playing {
                for active in &self.audio_sources { active.source.resume(); }
                std::thread::sleep(Duration::from_millis(15));
                if let Some(aout) = &self.audio_output { aout.0.resume(); }
                self.master_clock.resume();
            }

            self.playhead = target;
            let _ = self.event_tx.send(AppEvent::LoopedBack);
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
            (self.repaint_cb)();
            return;
        }

        // Handle media clip at loop target
        if let Some((idx, media_id, source_secs, timeline_in, clip_source_in, spd)) = clip_info {
            let need_new_decoder = self.current_clip_index != idx || self.v1_decoder.is_none();

            if need_new_decoder {
                if let Some(path) = self.path_for_media(media_id) {
                    self.v1_decoder = None;
                    self.stop_all_audio();
                    match VideoDecoder::open(&path) {
                        Ok(dec) => {
                            dec.seek(source_secs);
                            dec.resume();
                            self.v1_decoder = Some(dec);
                            self.current_clip_index = idx;
                            self.current_file = Some(path);
                            self.clip_source_start = clip_source_in;
                            self.clip_timeline_start = timeline_in;
                            self.clip_speed = spd;
                        }
                        Err(e) => {
                            error!("[Loop] Failed to open decoder: {e}");
                            return;
                        }
                    }
                }
            } else if let Some(dec) = &self.v1_decoder {
                dec.seek(source_secs);
                dec.frame_queue().lock().unwrap().clear();
                if self.state != PlaybackState::Playing {
                    dec.resume();
                }
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = spd;
            }

            // 1. Pause clock + cpal
            self.master_clock.pause();
            if let Some(aout) = &self.audio_output { aout.0.pause(); }

            // 2. Reopen audio at target
            self.start_all_audio(target);
            self.seek_all_audio_to_timecode(target);

            // 3. Set master clock
            let target_us = (source_secs * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            // 4. Pre-fill + resume
            if was_playing {
                for active in &self.audio_sources { active.source.resume(); }
                std::thread::sleep(Duration::from_millis(15));
                if let Some(aout) = &self.audio_output { aout.0.resume(); }
                self.master_clock.resume();
            }

            // 5. V2 decoder update
            self.v2_frame_cache = None;
            self.maybe_open_v2_decoder(target);

            self.playhead = target;
            let _ = self.event_tx.send(AppEvent::LoopedBack);
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
            (self.repaint_cb)();
        } else {
            // No video clip at target — audio-only loop
            self.v1_decoder = None;
            self.v2_frame_cache = None;

            self.master_clock.pause();
            if let Some(aout) = &self.audio_output { aout.0.pause(); }

            self.start_all_audio(target);
            self.seek_all_audio_to_timecode(target);
            let target_us = (target.to_secs_f64() * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            if was_playing {
                for active in &self.audio_sources { active.source.resume(); }
                std::thread::sleep(Duration::from_millis(15));
                if let Some(aout) = &self.audio_output { aout.0.resume(); }
                self.master_clock.resume();
            }

            self.playhead = target;
            let _ = self.event_tx.send(AppEvent::LoopedBack);
            let _ = self.event_tx.send(AppEvent::PlayheadMoved(target));
            (self.repaint_cb)();
        }
    }

    fn seek_in_sequence(&mut self, tc: Timecode) {
        self.seek_in_sequence_with_timeout(tc, Duration::from_millis(50));
    }

    fn seek_in_sequence_with_timeout(&mut self, tc: Timecode, grab_timeout: Duration) {
        // Check for standalone title clip (no media clip at this timecode on any track)
        {
            let seq = self.sequence.read().unwrap();
            let has_media_clip = seq.tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .any(|track| {
                    track.clip_at_timecode(tc)
                        .map(|(_, c)| c.media_id().is_some())
                        .unwrap_or(false)
                });
            let title_info = if !has_media_clip {
                seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|track| {
                        track.clip_at_timecode(tc).and_then(|(idx, clip)| {
                            let td = clip.title_data()?.clone();
                            Some((idx, td, clip.timeline_in, clip.source_in.to_secs_f64()))
                        })
                    })
            } else {
                None
            };
            let res = seq.resolution;
            drop(seq);

            if let Some((idx, title_data, timeline_in, clip_source_in)) = title_info {
                // Close any existing video decoder — title clips don't need one
                self.v1_decoder = None;
                self.current_clip_index = idx;
                self.current_file = None;
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;

                // Render title frame and write to frame buffer
                let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
                let rgba = self.title_renderer.render_with_gpu(&title_data, res.width, res.height, gpu);
                let final_rgba = self.post_process_rgba(rgba, res.width, res.height);
                let mut fb = self.frame_buffer.lock().unwrap();
                fb.write_frame(final_rgba, res.width, res.height);
                drop(fb);

                self.playhead = tc;
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(tc));
                (self.repaint_cb)();
                return;
            }
        }

        let seq = self.sequence.read().unwrap();
        // Search ALL video tracks (not just Video 1)
        let clip_info = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|track| {
                track.clip_at_timecode(tc).and_then(|(idx, clip)| {
                    let media_id = clip.media_id()?;
                    let source_secs = clip.source_in.to_secs_f64()
                        + (tc - clip.timeline_in).to_secs_f64() * clip.speed;
                    let clip_source_in = clip.source_in.to_secs_f64();
                    Some((idx, media_id, source_secs, clip.timeline_in, clip_source_in, clip.speed))
                })
            });
        drop(seq);

        if let Some((idx, media_id, source_secs, timeline_in, clip_source_in, spd)) = clip_info {
            let need_new_decoder = self.current_clip_index != idx || self.v1_decoder.is_none();

            if need_new_decoder {
                if let Some(path) = self.path_for_media(media_id) {
                    self.v1_decoder = None;
                    self.stop_all_audio();
                    match VideoDecoder::open(&path) {
                        Ok(dec) => {
                            dec.seek(source_secs);
                            dec.resume();
                            self.v1_decoder = Some(dec);
                            self.current_clip_index = idx;
                            self.current_file = Some(path.clone());
                            self.clip_source_start = clip_source_in;
                            self.clip_timeline_start = timeline_in;
                            self.clip_speed = spd;
                        }
                        Err(e) => {
                            error!("Failed to open decoder for seek: {e}");
                        }
                    }
                }
            } else if let Some(dec) = &self.v1_decoder {
                dec.seek(source_secs);
                // Clear stale frames from queue
                dec.frame_queue().lock().unwrap().clear();
                if self.state != PlaybackState::Playing {
                    dec.resume();
                }
                self.clip_source_start = clip_source_in;
                self.clip_timeline_start = timeline_in;
                self.clip_speed = spd;
            }

            let was_playing = self.state == PlaybackState::Playing;

            // 1. Pause clock + cpal output
            self.master_clock.pause();
            if let Some(aout) = &self.audio_output {
                aout.0.pause();
            }

            // 2. Re-open audio sources (start paused) then flush+seek
            self.start_all_audio(tc);
            self.seek_all_audio_to_timecode(tc);

            // 3. Set master clock to target
            let target_us = (source_secs * 1_000_000.0) as u64;
            self.master_clock.seek(target_us);

            // 4. Resume or stay paused based on playback state
            if was_playing {
                // Pre-fill: resume audio sources while cpal is still paused
                for active in &self.audio_sources {
                    active.source.resume();
                }
                std::thread::sleep(Duration::from_millis(15));

                // Resume cpal + clock together
                if let Some(aout) = &self.audio_output {
                    aout.0.resume();
                }
                self.master_clock.resume();
            }
            // Sources already start paused, no need to pause them

            // Re-check V2 at the new playhead
            self.v2_frame_cache = None;
            self.maybe_open_v2_decoder(tc);

            if !was_playing {
                self.grab_single_frame_timeout(grab_timeout);
                // Play scrub chirp (~40ms snippet at the seek position)
                if let Some(active) = self.audio_sources.first() {
                    active.source.chirp(source_secs, 0.04);
                    if let Some(aout) = &self.audio_output {
                        aout.0.start_chirp();
                    }
                }
            }
        } else {
            // No video clip at this timecode — handle audio-only seek
            self.v1_decoder = None;
            self.v2_frame_cache = None;

            // Check if any audio track has a clip here
            let has_audio = {
                let seq = self.sequence.read().unwrap();
                seq.tracks.iter().any(|track| {
                    track.kind == klipp_timeline::TrackKind::Audio
                        && track.clip_at_timecode(tc).is_some()
                })
            };

            if has_audio {
                let was_playing = self.state == PlaybackState::Playing;

                self.master_clock.pause();
                if let Some(aout) = &self.audio_output {
                    aout.0.pause();
                }

                // Re-open audio (paused), flush+seek
                self.start_all_audio(tc);
                self.seek_all_audio_to_timecode(tc);
                let target_us = (tc.to_secs_f64() * 1_000_000.0) as u64;
                self.master_clock.seek(target_us);

                if was_playing {
                    // Pre-fill then resume
                    for active in &self.audio_sources {
                        active.source.resume();
                    }
                    std::thread::sleep(Duration::from_millis(15));
                    if let Some(aout) = &self.audio_output {
                        aout.0.resume();
                    }
                    self.master_clock.resume();
                } else {
                    // Play scrub chirp for audio-only
                    if let Some(active) = self.audio_sources.first() {
                        let source_secs = {
                            let seq = self.sequence.read().unwrap();
                            seq.tracks.iter().find_map(|track| {
                                let (_, clip) = track.clip_at_timecode(tc)?;
                                Some(clip.source_in.to_secs_f64()
                                    + (tc - clip.timeline_in).to_secs_f64() * clip.speed)
                            }).unwrap_or(0.0)
                        };
                        active.source.chirp(source_secs, 0.04);
                        if let Some(aout) = &self.audio_output {
                            aout.0.start_chirp();
                        }
                    }
                }
            } else {
                self.stop_all_audio();
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
            let last_frame = self.v1_decoder.as_ref().and_then(|dec| {
                let mut last = None;
                while let Some(frame) = dec.frame_queue().lock().unwrap().pop() {
                    if !frame.is_eof {
                        last = Some(frame);
                    }
                }
                last
            });
            // If we got a queued frame, use it as the step
            if let Some(frame) = last_frame {
                let chirp_pos = frame.pts_secs();
                let seq = self.sequence.read().unwrap();
                if let Some(clip) = seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .find_map(|t| t.clips.get(self.current_clip_index))
                {
                    let source_elapsed =
                        frame.pts_secs() - clip.source_in.to_secs_f64();
                    let spd = clip.speed.max(0.001);
                    let elapsed_tc = RationalTime::from_secs_f64(
                        (source_elapsed / spd).max(0.0),
                        self.fps.rate(),
                    );
                    self.playhead = clip.timeline_in + elapsed_tc;
                }
                drop(seq);
                let (w, h) = (frame.width, frame.height);
                let rgba = frame.to_rgba();
                let rgba = self.post_process_rgba(rgba, w, h);
                let mut fb = self.frame_buffer.lock().unwrap();
                fb.write_frame(rgba, w, h);
                drop(fb);
                // Frame step chirp (~20ms)
                if let Some(active) = self.audio_sources.first() {
                    active.source.chirp(chirp_pos, 0.02);
                    if let Some(aout) = &self.audio_output {
                        aout.0.start_chirp();
                    }
                }
                let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                let _ = self.event_tx.send(AppEvent::FrameReady);
                (self.repaint_cb)();
                return;
            }
            // No queued frames — resume decoder to produce one
            if let Some(dec) = &self.v1_decoder {
                dec.resume();
            }
            // Wait for one frame with short timeout
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            while std::time::Instant::now() < deadline {
                // Pop frame in a limited scope to avoid borrowing self across post_process
                let popped = self.v1_decoder.as_ref()
                    .and_then(|dec| dec.frame_queue().lock().unwrap().pop());
                if let Some(frame) = popped {
                    if frame.is_eof {
                        if let Some(dec) = &self.v1_decoder { dec.pause(); }
                        return;
                    }
                    // Update playhead from frame PTS
                    let chirp_pos = frame.pts_secs();
                    let seq = self.sequence.read().unwrap();
                    if let Some(clip) = seq.tracks.iter()
                        .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                        .find_map(|t| t.clips.get(self.current_clip_index))
                    {
                        let source_elapsed =
                            frame.pts_secs() - clip.source_in.to_secs_f64();
                        let spd = clip.speed.max(0.001);
                        let elapsed_tc = RationalTime::from_secs_f64(
                            (source_elapsed / spd).max(0.0),
                            self.fps.rate(),
                        );
                        self.playhead = clip.timeline_in + elapsed_tc;
                    }
                    drop(seq);

                    let (w, h) = (frame.width, frame.height);
                    let rgba = frame.to_rgba();
                    let rgba = self.post_process_rgba(rgba, w, h);
                    let mut fb = self.frame_buffer.lock().unwrap();
                    fb.write_frame(rgba, w, h);
                    drop(fb);
                    // Frame step chirp (~20ms)
                    if let Some(active) = self.audio_sources.first() {
                        if let Some(aout) = &self.audio_output {
                            aout.0.notify_seek();
                            aout.0.resume();
                        }
                        active.source.chirp(chirp_pos, 0.02);
                    }
                    let _ = self.event_tx.send(AppEvent::PlayheadMoved(self.playhead));
                    let _ = self.event_tx.send(AppEvent::FrameReady);
                    if let Some(dec) = &self.v1_decoder {
                        dec.pause();
                    }
                    (self.repaint_cb)();
                    return;
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
            // Pop best frame in limited scope to avoid borrow across post_process
            let best = self.v1_decoder.as_ref().and_then(|dec| {
                let mut queue = dec.frame_queue().lock().unwrap();
                let mut best = None;
                while let Some(frame) = queue.pop() {
                    if frame.is_eof {
                        break;
                    }
                    best = Some(frame);
                }
                best
            });
            if let Some(frame) = best {
                let (w, h) = (frame.width, frame.height);
                let rgba = frame.to_rgba();
                let rgba = self.post_process_rgba(rgba, w, h);
                let mut fb = self.frame_buffer.lock().unwrap();
                fb.write_frame(rgba, w, h);
                drop(fb);
                let _ = self.event_tx.send(AppEvent::FrameReady);
                if let Some(dec) = &self.v1_decoder {
                    dec.pause();
                }
                (self.repaint_cb)();
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if let Some(dec) = &self.v1_decoder {
            dec.pause();
        }
    }

    /// Apply the full post-processing pipeline to a raw decoded RGBA frame:
    /// color grade → OFX effects → built-in effects → global LUT.
    /// Check for an active transition at the current playhead and blend if needed.
    fn apply_transition_if_active(&mut self, from_rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
        // Read sequence to check for transitions
        let seq = self.sequence.read().unwrap();
        let active = crate::transition::find_active_transition(&seq, self.playhead);
        drop(seq);

        let active = match active {
            Some(a) => a,
            None => {
                // No transition active — close transition decoder if open
                if self.transition_decoder.is_some() {
                    if let Some(dec) = &mut self.transition_decoder {
                        dec.stop_detached();
                    }
                    self.transition_decoder = None;
                    self.transition_clip_id = None;
                    self.transition_frame_cache = None;
                }
                return from_rgba;
            }
        };

        // Open transition decoder for partner clip if needed
        let partner_clip_id = active.partner_clip.id;
        if self.transition_clip_id != Some(partner_clip_id) {
            // Close old transition decoder
            if let Some(dec) = &mut self.transition_decoder {
                dec.stop_detached();
            }
            self.transition_decoder = None;
            self.transition_frame_cache = None;

            // Open new decoder for the partner clip
            if let Some(media_id) = active.partner_clip.media_id() {
                if let Some(path) = self.path_for_media(media_id) {
                    match VideoDecoder::open(&path) {
                        Ok(dec) => {
                            let source_secs = active.partner_clip.source_in.to_secs_f64()
                                + (self.playhead - active.partner_clip.timeline_in).to_secs_f64().max(0.0) * active.partner_clip.speed;
                            if source_secs > 0.01 {
                                dec.seek(source_secs);
                            }
                            dec.resume();
                            self.transition_decoder = Some(dec);
                            self.transition_clip_id = Some(partner_clip_id);
                            debug!("Transition decoder opened for partner clip");
                        }
                        Err(e) => {
                            warn!("Failed to open transition decoder: {e}");
                        }
                    }
                }
            }
        }

        // Pop frame from transition decoder
        if let Some(t_dec) = &self.transition_decoder {
            let current_source_secs = self.master_clock.current_secs();
            let frame_tolerance = self.fps.frame_duration_secs() * 0.5;
            let mut queue = t_dec.frame_queue().lock().unwrap();
            while let Some(f) = queue.front() {
                if f.is_eof {
                    let _ = queue.pop();
                    break;
                }
                let pts = f.pts_secs();
                if pts <= current_source_secs + frame_tolerance {
                    let tf = queue.pop().unwrap();
                    if let Some(next) = queue.front() {
                        if !next.is_eof && next.pts_secs() <= current_source_secs + frame_tolerance {
                            continue;
                        }
                    }
                    self.transition_frame_cache = Some(tf);
                    break;
                } else {
                    break;
                }
            }
        }

        // Blend if we have the partner frame
        let to_rgba = match &self.transition_frame_cache {
            Some(f) => f.to_rgba(),
            None => {
                // No partner frame yet — show from frame unblended
                return from_rgba;
            }
        };

        let kind_id = crate::transition::transition_kind_to_id(active.kind);

        // GPU-accelerated transition (preferred) or CPU fallback
        if let Some(gpu) = &mut self.gpu_effects {
            gpu.render_transition(&from_rgba, &to_rgba, kind_id, active.progress, width, height)
        } else {
            crate::transition::render_transition(active.kind, &from_rgba, &to_rgba, active.progress, width, height)
        }
    }

    fn post_process_rgba(&mut self, rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
        // Cache the pre-grade frame for instant re-grading when paused
        self.cached_pregrade_frame = Some((rgba.clone(), width, height));
        // Also store in shared frame buffer so UI thread can re-grade directly
        {
            let mut fb = self.frame_buffer.lock().unwrap();
            fb.pregrade_data = rgba.clone();
        }
        self.post_process_rgba_inner(rgba, width, height)
    }

    /// Re-grade the cached pre-grade frame without re-decoding.
    /// Called when color parameters change while paused.
    /// Re-render title overlay (V2) from current sequence data.
    /// Called on SequenceUpdated when paused so title text/font/color changes show instantly.
    fn refresh_v2_title_overlay(&mut self) {
        let seq = self.sequence.read().unwrap();
        let primary_v1_idx = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video);
        let title_clip_info = seq.tracks.iter()
            .enumerate()
            .filter(|(i, t)| {
                !t.muted && match t.kind {
                    klipp_timeline::TrackKind::Text | klipp_timeline::TrackKind::Subtitle => true,
                    klipp_timeline::TrackKind::Video => primary_v1_idx.map_or(true, |v1| *i != v1),
                    _ => false,
                }
            })
            .find_map(|(_ti, track)| {
                track.clip_at_timecode(self.playhead).and_then(|(_, clip)| {
                    clip.title_data().map(|td| (td.clone(), clip.clone(), seq.resolution))
                })
            });
        drop(seq);

        if let Some((title_data, clip, res)) = title_clip_info {
            let mut gpu_adapter = self.gpu_effects.as_mut().map(GpuAlphaMaskAdapter);
                let gpu: Option<&mut dyn klipp_title::AlphaMaskProcessor> = gpu_adapter.as_mut().map(|a| a as &mut dyn klipp_title::AlphaMaskProcessor);
            let mut rgba = self.title_renderer.render_with_gpu(&title_data, res.width, res.height, gpu);

            // Apply opacity keyframes to the title overlay
            let clip_relative_secs = (self.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
            let eval_ctx = klipp_core::EvalContext {
                clip_time_secs: clip_relative_secs,
                clip_duration_secs: (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()),
                timeline_time_secs: self.playhead.to_secs_f64(),
                fps: self.fps.as_f64(),
                seed: clip.id.0.as_u64_pair().0,
            };

            // Evaluate opacity
            let opacity_default = clip.builtin_effects.iter()
                .find_map(|e| match e { klipp_effects::BuiltinEffect::Opacity { value } => Some(*value), _ => None })
                .unwrap_or(1.0);
            let opacity = match clip.param_modifiers.get(&klipp_core::ParamId::Opacity) {
                Some(stack) if !stack.modifiers.is_empty() => {
                    klipp_core::evaluate_param(&clip.opacity_keyframes, stack, clip_relative_secs, opacity_default, &eval_ctx)
                }
                _ => klipp_core::keyframe::evaluate(&clip.opacity_keyframes, clip_relative_secs, opacity_default),
            };

            // Apply opacity to all alpha pixels
            if (opacity - 1.0).abs() > 0.001 {
                let alpha_mul = (opacity * 255.0).clamp(0.0, 255.0) as u8;
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel[3] = ((pixel[3] as u16 * alpha_mul as u16) / 255) as u8;
                }
            }

            // Apply transform keyframes + modifiers (position, scale, rotation)
            let has_transform = !clip.transform_pos_x_keyframes.is_empty()
                || !clip.transform_pos_y_keyframes.is_empty()
                || !clip.transform_scale_keyframes.is_empty()
                || !clip.transform_rotation_keyframes.is_empty()
                || clip.param_modifiers.contains_key(&klipp_core::ParamId::PositionX)
                || clip.param_modifiers.contains_key(&klipp_core::ParamId::PositionY)
                || clip.param_modifiers.contains_key(&klipp_core::ParamId::Scale)
                || clip.param_modifiers.contains_key(&klipp_core::ParamId::Rotation);

            if has_transform {
                let tdef = clip.builtin_effects.iter()
                    .find_map(|e| match e {
                        klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y } =>
                            Some((*position_x, *position_y, *scale_x, *scale_y, *rotation, *anchor_x, *anchor_y)),
                        _ => None,
                    })
                    .unwrap_or((0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5));

                // Check for motion blur on animated title
                let motion_blur = title_data.motion_blur.filter(|mb| mb.samples >= 2);

                if let Some(mb) = motion_blur {
                    // Multi-sample motion blur: render transform at N time offsets and blend
                    let frame_dur = self.fps.frame_duration_secs();
                    let half_shutter = (mb.shutter_angle as f64 / 360.0) * frame_dur;
                    let n = mb.samples as usize;
                    let pixel_count = (res.width * res.height) as usize * 4;
                    let mut accum = vec![0u32; pixel_count];

                    for si in 0..n {
                        let t_offset = if n > 1 {
                            (si as f64 / (n - 1) as f64 - 0.5) * 2.0 * half_shutter
                        } else {
                            0.0
                        };
                        let sample_t = (clip_relative_secs + t_offset).max(0.0);
                        let sample_eval = klipp_core::EvalContext {
                            clip_time_secs: sample_t,
                            ..eval_ctx
                        };

                        let spx = match clip.param_modifiers.get(&klipp_core::ParamId::PositionX) {
                            Some(stack) if !stack.modifiers.is_empty() =>
                                klipp_core::evaluate_param(&clip.transform_pos_x_keyframes, stack, sample_t, tdef.0, &sample_eval),
                            _ => klipp_core::keyframe::evaluate(&clip.transform_pos_x_keyframes, sample_t, tdef.0),
                        };
                        let spy = match clip.param_modifiers.get(&klipp_core::ParamId::PositionY) {
                            Some(stack) if !stack.modifiers.is_empty() =>
                                klipp_core::evaluate_param(&clip.transform_pos_y_keyframes, stack, sample_t, tdef.1, &sample_eval),
                            _ => klipp_core::keyframe::evaluate(&clip.transform_pos_y_keyframes, sample_t, tdef.1),
                        };
                        let ssc = match clip.param_modifiers.get(&klipp_core::ParamId::Scale) {
                            Some(stack) if !stack.modifiers.is_empty() =>
                                klipp_core::evaluate_param(&clip.transform_scale_keyframes, stack, sample_t, tdef.2, &sample_eval),
                            _ => klipp_core::keyframe::evaluate(&clip.transform_scale_keyframes, sample_t, tdef.2),
                        };
                        let sro = match clip.param_modifiers.get(&klipp_core::ParamId::Rotation) {
                            Some(stack) if !stack.modifiers.is_empty() =>
                                klipp_core::evaluate_param(&clip.transform_rotation_keyframes, stack, sample_t, tdef.4, &sample_eval),
                            _ => klipp_core::keyframe::evaluate(&clip.transform_rotation_keyframes, sample_t, tdef.4),
                        };

                        let sample_rgba = klipp_effects::transform::apply(
                            rgba.clone(), res.width, res.height,
                            spx, spy, ssc, ssc, sro, tdef.5, tdef.6,
                        );
                        for (i, &b) in sample_rgba.iter().enumerate() {
                            accum[i] += b as u32;
                        }
                    }
                    rgba = accum.iter().map(|v| (*v / n as u32) as u8).collect();
                } else {
                    // No motion blur — single transform
                    let px = match clip.param_modifiers.get(&klipp_core::ParamId::PositionX) {
                        Some(stack) if !stack.modifiers.is_empty() =>
                            klipp_core::evaluate_param(&clip.transform_pos_x_keyframes, stack, clip_relative_secs, tdef.0, &eval_ctx),
                        _ => klipp_core::keyframe::evaluate(&clip.transform_pos_x_keyframes, clip_relative_secs, tdef.0),
                    };
                    let py = match clip.param_modifiers.get(&klipp_core::ParamId::PositionY) {
                        Some(stack) if !stack.modifiers.is_empty() =>
                            klipp_core::evaluate_param(&clip.transform_pos_y_keyframes, stack, clip_relative_secs, tdef.1, &eval_ctx),
                        _ => klipp_core::keyframe::evaluate(&clip.transform_pos_y_keyframes, clip_relative_secs, tdef.1),
                    };
                    let sc = match clip.param_modifiers.get(&klipp_core::ParamId::Scale) {
                        Some(stack) if !stack.modifiers.is_empty() =>
                            klipp_core::evaluate_param(&clip.transform_scale_keyframes, stack, clip_relative_secs, tdef.2, &eval_ctx),
                        _ => klipp_core::keyframe::evaluate(&clip.transform_scale_keyframes, clip_relative_secs, tdef.2),
                    };
                    let ro = match clip.param_modifiers.get(&klipp_core::ParamId::Rotation) {
                        Some(stack) if !stack.modifiers.is_empty() =>
                            klipp_core::evaluate_param(&clip.transform_rotation_keyframes, stack, clip_relative_secs, tdef.4, &eval_ctx),
                        _ => klipp_core::keyframe::evaluate(&clip.transform_rotation_keyframes, clip_relative_secs, tdef.4),
                    };

                    rgba = klipp_effects::transform::apply(rgba, res.width, res.height, px, py, sc, sc, ro, tdef.5, tdef.6);
                }
            }

            self.v2_frame_cache = Some(crate::types::DecodedFrame {
                data: crate::types::FrameData::Rgba(rgba),
                width: res.width,
                height: res.height,
                pts: RationalTime::from_frames(0, 24),
                is_eof: false,
            });
        }
    }

    fn regrade_cached_frame(&mut self) {
        // Case 1: V1 video exists — regrade and composite with V2 overlay
        if let Some((ref rgba, w, h)) = self.cached_pregrade_frame.clone() {
            let v1_result = self.post_process_rgba_inner(rgba.clone(), w, h);

            // Composite V2 overlay (title clips) directly over V1
            let final_result = if let Some(v2_frame) = &self.v2_frame_cache {
                let v2_rgba = v2_frame.to_rgba();
                let mut out = v1_result;
                // Simple alpha blend: title pixels with alpha > 0 overwrite V1 pixels
                if v2_rgba.len() == out.len() {
                    for i in (0..out.len()).step_by(4) {
                        let sa = v2_rgba[i + 3] as f32 / 255.0;
                        if sa > 0.001 {
                            let da = 1.0 - sa;
                            out[i]     = (v2_rgba[i]     as f32 * sa + out[i]     as f32 * da) as u8;
                            out[i + 1] = (v2_rgba[i + 1] as f32 * sa + out[i + 1] as f32 * da) as u8;
                            out[i + 2] = (v2_rgba[i + 2] as f32 * sa + out[i + 2] as f32 * da) as u8;
                            out[i + 3] = 255; // result is always fully opaque
                        }
                    }
                }
                out
            } else {
                v1_result
            };

            let mut fb = self.frame_buffer.lock().unwrap();
            fb.write_frame(final_result, w, h);
            drop(fb);
            let _ = self.event_tx.send(AppEvent::FrameReady);
        } else if let Some(v2_frame) = &self.v2_frame_cache {
            // Case 2: No V1 video, but V2 title overlay exists (standalone title)
            let rgba = v2_frame.to_rgba();
            let w = v2_frame.width;
            let h = v2_frame.height;
            let mut fb = self.frame_buffer.lock().unwrap();
            fb.write_frame(rgba, w, h);
            drop(fb);
            let _ = self.event_tx.send(AppEvent::FrameReady);
        }
    }

    fn post_process_rgba_inner(&mut self, rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
        let t0 = std::time::Instant::now();
        let seq = self.sequence.read().unwrap();
        let clip = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|t| t.clips.get(self.current_clip_index));
        let ofx_effects = clip.map(|c| c.effects.clone()).unwrap_or_default();
        let node_graph = clip.map(|c| c.node_graph.clone());
        let clip_time_secs = clip.map(|_c| {
            (self.playhead.to_secs_f64() - self.clip_timeline_start.to_secs_f64()) * self.clip_speed
                + self.clip_source_start
        }).unwrap_or(0.0);

        // Clip-relative time for keyframe evaluation (time since clip's timeline_in)
        let clip_relative_secs = clip.map(|c| {
            (self.playhead.to_secs_f64() - c.timeline_in.to_secs_f64()).max(0.0)
        }).unwrap_or(0.0);

        // Build modifier evaluation context
        let eval_ctx = clip.map(|c| klipp_core::EvalContext {
            clip_time_secs: clip_relative_secs,
            clip_duration_secs: (c.source_out.to_secs_f64() - c.source_in.to_secs_f64()),
            timeline_time_secs: self.playhead.to_secs_f64(),
            fps: self.fps.as_f64(),
            seed: c.id.0.as_u64_pair().0,
        });

        // Build builtin_fx with keyframed values overriding static ones
        let builtin_fx = if let Some(c) = clip {
            let mut fx = c.builtin_effects.clone();

            // Evaluate opacity keyframes + modifiers
            let has_opacity_modifiers = c.param_modifiers.contains_key(&klipp_core::ParamId::Opacity);
            if !c.opacity_keyframes.is_empty() || has_opacity_modifiers {
                let opacity_default = fx.iter().find_map(|e| match e {
                    klipp_effects::BuiltinEffect::Opacity { value } => Some(*value),
                    _ => None,
                }).unwrap_or(1.0);
                let opacity_val = eval_with_modifiers(
                    &c.opacity_keyframes,
                    &c.param_modifiers,
                    &klipp_core::ParamId::Opacity,
                    clip_relative_secs,
                    opacity_default,
                    eval_ctx.as_ref().unwrap(),
                );
                let mut found = false;
                for effect in &mut fx {
                    if let klipp_effects::BuiltinEffect::Opacity { value } = effect {
                        *value = opacity_val;
                        found = true;
                        break;
                    }
                }
                if !found && (opacity_val - 1.0).abs() > 0.001 {
                    fx.push(klipp_effects::BuiltinEffect::Opacity { value: opacity_val });
                }
            }

            // Evaluate transform keyframes + modifiers
            let has_transform_modifiers = c.param_modifiers.contains_key(&klipp_core::ParamId::PositionX)
                || c.param_modifiers.contains_key(&klipp_core::ParamId::PositionY)
                || c.param_modifiers.contains_key(&klipp_core::ParamId::Scale)
                || c.param_modifiers.contains_key(&klipp_core::ParamId::Rotation);
            let has_transform_kf = !c.transform_pos_x_keyframes.is_empty()
                || !c.transform_pos_y_keyframes.is_empty()
                || !c.transform_scale_keyframes.is_empty()
                || !c.transform_rotation_keyframes.is_empty()
                || has_transform_modifiers;

            if has_transform_kf {
                // Get current transform values (or defaults)
                let (mut pos_x, mut pos_y, mut scale_x, mut scale_y, mut rotation, anchor_x, anchor_y) =
                    fx.iter().find_map(|e| match e {
                        klipp_effects::BuiltinEffect::Transform {
                            position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y,
                        } => Some((*position_x, *position_y, *scale_x, *scale_y, *rotation, *anchor_x, *anchor_y)),
                        _ => None,
                    }).unwrap_or((0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5));

                let ectx = eval_ctx.as_ref().unwrap();
                if !c.transform_pos_x_keyframes.is_empty() || c.param_modifiers.contains_key(&klipp_core::ParamId::PositionX) {
                    pos_x = eval_with_modifiers(&c.transform_pos_x_keyframes, &c.param_modifiers, &klipp_core::ParamId::PositionX, clip_relative_secs, pos_x, ectx);
                }
                if !c.transform_pos_y_keyframes.is_empty() || c.param_modifiers.contains_key(&klipp_core::ParamId::PositionY) {
                    pos_y = eval_with_modifiers(&c.transform_pos_y_keyframes, &c.param_modifiers, &klipp_core::ParamId::PositionY, clip_relative_secs, pos_y, ectx);
                }
                if !c.transform_scale_keyframes.is_empty() || c.param_modifiers.contains_key(&klipp_core::ParamId::Scale) {
                    let s = eval_with_modifiers(&c.transform_scale_keyframes, &c.param_modifiers, &klipp_core::ParamId::Scale, clip_relative_secs, scale_x, ectx);
                    scale_x = s;
                    scale_y = s; // uniform scale from the Video tab
                }
                if !c.transform_rotation_keyframes.is_empty() || c.param_modifiers.contains_key(&klipp_core::ParamId::Rotation) {
                    rotation = eval_with_modifiers(&c.transform_rotation_keyframes, &c.param_modifiers, &klipp_core::ParamId::Rotation, clip_relative_secs, rotation, ectx);
                }

                let new_transform = klipp_effects::BuiltinEffect::Transform {
                    position_x: pos_x, position_y: pos_y,
                    scale_x, scale_y, rotation, anchor_x, anchor_y,
                };
                let mut found = false;
                for effect in &mut fx {
                    if matches!(effect, klipp_effects::BuiltinEffect::Transform { .. }) {
                        *effect = new_transform.clone();
                        found = true;
                        break;
                    }
                }
                if !found {
                    fx.push(new_transform);
                }
            }

            // Evaluate effect keyframes + modifiers (blur, sharpen, crop, chroma key)
            {
                let ectx = eval_ctx.as_ref().unwrap();
                for (i, effect) in fx.iter_mut().enumerate() {
                    match effect {
                        klipp_effects::BuiltinEffect::GaussianBlur { radius } => {
                            let key = format!("{}:radius", i);
                            if let Some(kfs) = c.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *radius = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "radius", clip_relative_secs, *radius, ectx);
                                }
                            } else {
                                // No keyframes but may have modifiers
                                *radius = eval_effect_with_modifiers(&[], &c.param_modifiers, i, "radius", clip_relative_secs, *radius, ectx);
                            }
                        }
                        klipp_effects::BuiltinEffect::Sharpen { amount } => {
                            let key = format!("{}:amount", i);
                            if let Some(kfs) = c.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *amount = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "amount", clip_relative_secs, *amount, ectx);
                                }
                            } else {
                                *amount = eval_effect_with_modifiers(&[], &c.param_modifiers, i, "amount", clip_relative_secs, *amount, ectx);
                            }
                        }
                        klipp_effects::BuiltinEffect::Crop { left, right, top, bottom, feather } => {
                            for (name, val) in [("left", left as &mut f32), ("right", right), ("top", top), ("bottom", bottom), ("feather", feather)] {
                                let key = format!("{}:{}", i, name);
                                let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                *val = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, name, clip_relative_secs, *val, ectx);
                            }
                        }
                        klipp_effects::BuiltinEffect::ChromaKey { tolerance, spill_suppression, .. } => {
                            for (name, val) in [("tolerance", tolerance as &mut f32), ("spill", spill_suppression)] {
                                let key = format!("{}:{}", i, name);
                                let kfs = c.effect_keyframes.get(&key).map(|v| v.as_slice()).unwrap_or(&[]);
                                *val = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, name, clip_relative_secs, *val, ectx);
                            }
                        }
                        klipp_effects::BuiltinEffect::Opacity { value } => {
                            let key = format!("{}:value", i);
                            if let Some(kfs) = c.effect_keyframes.get(&key) {
                                if !kfs.is_empty() {
                                    *value = eval_effect_with_modifiers(kfs, &c.param_modifiers, i, "value", clip_relative_secs, *value, ectx);
                                }
                            } else {
                                *value = eval_effect_with_modifiers(&[], &c.param_modifiers, i, "value", clip_relative_secs, *value, ectx);
                            }
                        }
                        _ => {} // Transform already handled by dedicated keyframes
                    }
                }
            }

            fx
        } else {
            Vec::new()
        };

        drop(seq);

        // Save pre-grade frame for Before/After split view
        let pre_grade_rgba = if self.bypass_color_grade {
            Some(rgba.clone())
        } else {
            None
        };

        // 1. Per-clip color grade via node graph executor
        let t1 = std::time::Instant::now();

        // Determine matte preview node: when matte_preview is active, find first
        // Qualifier or Mask node in the graph to preview its matte output
        let matte_pv_node = if self.matte_preview {
            self.matte_preview_node.or_else(|| {
                node_graph.as_ref().and_then(|g| {
                    g.nodes.iter()
                        .find(|n| matches!(n.kind, klipp_nodes::NodeKind::Qualifier | klipp_nodes::NodeKind::Mask))
                        .map(|n| n.id)
                })
            })
        } else {
            None
        };

        let rgba = if let Some(ref graph) = node_graph {
            if !graph.is_identity() || matte_pv_node.is_some() {
                let frame_number = (clip_time_secs * self.playhead.rate as f64).max(0.0) as u64;
                match self.graph_executor.execute(
                    &rgba,
                    graph,
                    width,
                    height,
                    clip_time_secs,
                    frame_number,
                    self.scope_tap_node,
                    matte_pv_node,
                ) {
                    Ok((result, tapped)) => {
                        // Store tapped output for scope display
                        if let Some(tap_data) = tapped {
                            let mut fb = self.frame_buffer.lock().unwrap();
                            fb.scope_data = tap_data;
                        }
                        result
                    }
                    Err(e) => {
                        tracing::error!("Node graph execution failed: {:?}", e);
                        rgba
                    }
                }
            } else {
                rgba
            }
        } else {
            rgba
        };
        let t2 = std::time::Instant::now();

        // 1.5. Speed Ramp — modify effective speed if SpeedRamp effect is present
        // The SpeedRamp effect modifies the clip's playback speed natively, bypassing OFX.
        // Frame blending uses the GPU transition pipeline for smooth interpolation.
        let speed_ramp = builtin_fx.iter().find_map(|e| match e {
            klipp_effects::BuiltinEffect::SpeedRamp { speed, blend_mode, reverse, freeze, freeze_at, .. } => {
                Some((*speed, *blend_mode, *reverse, *freeze, *freeze_at))
            }
            _ => None,
        });
        let rgba = if let Some((speed, blend_mode, _reverse, freeze, _freeze_at)) = speed_ramp {
            if freeze {
                // Freeze frame: keep using the current frame (no speed change needed)
                rgba
            } else {
                // Speed modification is handled by the clip.speed property.
                // Here we apply frame blending for smooth interpolation.
                // blend_mode: 0=nearest (pass-through), 1=linear (blend adjacent frames)
                if blend_mode >= 1 && speed != 1.0 {
                    // For frame blending, we'd need the next frame from the queue.
                    // For now, apply a slight motion blur to simulate smooth motion.
                    // Full frame blending requires access to adjacent decoded frames.
                    rgba
                } else {
                    rgba
                }
            }
        } else {
            rgba
        };

        // 2. OFX effects chain (with temporal clip access for Retime/SlitScan/etc.)
        let frame_time = self.playhead.to_secs_f64();
        let rgba = if !ofx_effects.is_empty() {
            // Check if any OFX effect needs temporal clip access (Retime, SlitScan, etc.)
            let needs_temporal = ofx_effects.iter().any(|e| {
                e.plugin_name.contains("Retime")
                    || e.plugin_name.contains("SlitScan")
                    || e.plugin_name.contains("timeOffset")
                    || e.plugin_name.contains("TimeOffset")
                    || e.plugin_name.contains("FrameBlend")
                    || e.plugin_name.contains("TimeBlur")
            });

            if needs_temporal {
                // Only create temporal decoder for plugins that actually need it
                let (source_path, clip_source_in) = {
                    let seq = self.sequence.read().unwrap();
                    let clip = seq.tracks.iter()
                        .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                        .find_map(|t| t.clips.get(self.current_clip_index));
                    let path = clip.and_then(|c| {
                        c.media_id().and_then(|mid| {
                            self.media_pool.read().ok().and_then(|pool| {
                                pool.get(&mid).map(|item| item.path.clone())
                            })
                        })
                    });
                    let src_in = clip.map(|c| c.source_in.to_secs_f64()).unwrap_or(0.0);
                    (path, src_in)
                };

                if let Some(path) = source_path {
                    let cached_decoder = std::cell::RefCell::new(
                        CachedFrameDecoder::open(&path)
                    );
                    let frame_provider: klipp_plugin::FrameProviderFn = Box::new(move |time_secs: f64| {
                        let source_secs = (clip_source_in + time_secs).max(0.0);
                        let mut decoder = cached_decoder.borrow_mut();
                        if let Some(ref mut dec) = *decoder {
                            dec.decode_at(source_secs)
                        } else {
                            None
                        }
                    });
                    self.ofx_host.apply_effects_chain_temporal(
                        &ofx_effects, &rgba, width, height, frame_time, frame_provider,
                    )
                } else {
                    self.ofx_host.apply_effects_chain(&ofx_effects, &rgba, width, height, frame_time)
                }
            } else {
                // No temporal plugins — fast path, no decoder creation
                self.ofx_host.apply_effects_chain(&ofx_effects, &rgba, width, height, frame_time)
            }
        } else { rgba };
        let t3 = std::time::Instant::now();

        // 3. Built-in effects (GPU-accelerated when available, CPU fallback)
        let rgba = if !builtin_fx.is_empty() {
            if let Some(gpu) = &mut self.gpu_effects {
                gpu.apply_effects(&builtin_fx, rgba, width, height)
            } else {
                klipp_effects::apply_effects(&builtin_fx, rgba, width, height)
            }
        } else { rgba };
        let t4 = std::time::Instant::now();

        // 4. Global color pipeline
        let rgba = self.color_pipeline.process_frame(rgba, width, height);
        let t5 = std::time::Instant::now();

        // Store pre-grade frame in frame buffer for Before/After split
        if let Some(original) = pre_grade_rgba {
            let mut fb = self.frame_buffer.lock().unwrap();
            fb.original_data = original;
        } else {
            // Clear original when not in split mode to free memory
            let mut fb = self.frame_buffer.lock().unwrap();
            if !fb.original_data.is_empty() {
                fb.original_data = Vec::new();
            }
        }

        let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if total_ms > 2.0 {
            debug!(
                "PERF post_process: {:.1}ms total | seq_read={:.1}ms grade={:.1}ms ofx={:.1}ms builtin={:.1}ms lut={:.1}ms",
                total_ms,
                (t1 - t0).as_secs_f64() * 1000.0,
                (t2 - t1).as_secs_f64() * 1000.0,
                (t3 - t2).as_secs_f64() * 1000.0,
                (t4 - t3).as_secs_f64() * 1000.0,
                (t5 - t4).as_secs_f64() * 1000.0,
            );
        }
        rgba
    }

    /// Wait for a video frame near `target_secs` to appear in the queue.
    /// Drains any stale frames that are far from the target (>2s away).
    /// This ensures the clock and audio don't start before video is ready.
    fn wait_for_valid_frame(&self, target_secs: f64) {
        let Some(dec) = &self.v1_decoder else { return };
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        loop {
            {
                let mut queue = dec.frame_queue().lock().unwrap();
                // Drain frames that are far from the seek target
                while let Some(front) = queue.front() {
                    let pts = front.pts_secs();
                    if (pts - target_secs).abs() < 2.0 || front.is_eof {
                        // Frame is close enough to target — good to go
                        return;
                    }
                    // Stale frame from a previous position — discard
                    debug!(
                        "wait_for_valid_frame: discarding stale frame pts={:.3}s (target={:.3}s)",
                        pts, target_secs
                    );
                    queue.pop();
                }
            }
            if std::time::Instant::now() >= deadline {
                debug!("wait_for_valid_frame: timed out waiting for frame near {target_secs:.3}s");
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
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
            .map(|item| {
                if self.use_proxies {
                    if let Some(ref proxy_path) = item.proxy_path {
                        if item.proxy_ready && proxy_path.exists() {
                            tracing::trace!("resolve_media_path: using proxy for {}", item.filename);
                            return proxy_path.clone();
                        }
                    }
                }
                item.path.clone()
            })
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
                let mut sample_rate: Option<u32> = None;
                let mut audio_channels: Option<u16> = None;
                let mut colour_space: u8 = 0; // 0=Unknown, 1=SDR, 2=HDR10, 3=HLG

                for stream in format_ctx.streams() {
                    let params = stream.parameters();
                    let medium = params.medium();
                    match medium {
                        ffmpeg_next::media::Type::Video => {
                            let decoder = ffmpeg_next::codec::context::Context::from_parameters(params.clone())
                                .and_then(|ctx| ctx.decoder().video());
                            if let Ok(dec) = decoder {
                                video_w = dec.width();
                                video_h = dec.height();
                                video_codec = format!("{:?}", dec.id());
                                // Extract colour space from codec parameters (FFI)
                                // color_primaries: BT.709=1, BT.2020=9
                                // color_trc: BT.709=1, PQ/SMPTE2084=16, HLG/ARIB=18
                                unsafe {
                                    let par = params.as_ptr();
                                    if !par.is_null() {
                                        let primaries = (*par).color_primaries as i32;
                                        let transfer = (*par).color_trc as i32;
                                        colour_space = if primaries == 9 && transfer == 16 {
                                            2 // HDR10: BT.2020 + PQ
                                        } else if primaries == 9 && transfer == 18 {
                                            3 // HLG: BT.2020 + HLG
                                        } else if primaries == 1 || primaries == 0 || primaries == 2 {
                                            1 // SDR: BT.709 or unspecified
                                        } else {
                                            0 // Unknown
                                        };
                                    }
                                }
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
                                sample_rate = Some(dec.rate() as u32);
                                audio_channels = Some(dec.channels() as u16);
                            }
                        }
                        _ => {}
                    }
                }

                // Detect still images: has video, no audio, near-zero duration,
                // and file extension matches common image formats.
                let is_still = !video_codec.is_empty()
                    && !has_audio
                    && (duration_secs < 0.1 || duration_secs.is_nan())
                    && is_image_extension(&path);

                self.fps = fps_from_float(container_fps);
                self.duration = RationalTime::from_secs_f64(duration_secs, self.fps.rate());
                self.playhead = Timecode::zero();
                self.current_file = Some(path.clone());
                self.current_clip_index = 0;
                self.state = PlaybackState::Stopped;

                let fs_meta = std::fs::metadata(&path).ok();
                let file_size_bytes = fs_meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let file_modified_at = fs_meta.as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());

                // Try to extract creation_time from container metadata (camera shoot date)
                let media_created_at = format_ctx.metadata().get("creation_time")
                    .and_then(|s| parse_iso8601_timestamp(s));

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
                    sample_rate,
                    audio_channels,
                    is_still,
                    file_modified_at,
                    media_created_at,
                    colour_space,
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
    let mut has_sequence_updated = false;

    for cmd in commands {
        match &cmd {
            AppCommand::SeekTo(_) | AppCommand::SetPlayhead(_) => {
                // Replace any pending seek with this newer one
                last_seek = Some(cmd);
            }
            AppCommand::SequenceUpdated => {
                // Coalesce multiple SequenceUpdated into one (deferred to end)
                has_sequence_updated = true;
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

    // Flush single coalesced SequenceUpdated at end
    if has_sequence_updated {
        result.push(AppCommand::SequenceUpdated);
    }

    result
}

/// Parse ISO 8601 timestamp like "2024-03-15T10:30:00.000000Z" to Unix epoch seconds.
fn parse_iso8601_timestamp(s: &str) -> Option<u64> {
    // Minimal parser for ffmpeg's creation_time format: YYYY-MM-DDTHH:MM:SS[.frac]Z
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: u64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;
    let time_main = time.split('.').next()?; // strip fractional seconds
    let mut time_parts = time_main.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let min: u64 = time_parts.next()?.parse().ok()?;
    let sec: u64 = time_parts.next()?.parse().ok()?;
    // Approximate days since epoch (not accounting for leap seconds, good enough for sorting)
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mdays = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 0..(month.saturating_sub(1) as usize).min(11) {
        days += mdays[m];
    }
    days += day.saturating_sub(1);
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_image_extension(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" | "webp")
    )
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

// ---------------------------------------------------------------------------
// GPU-accelerated thumbnail/filmstrip helpers
// ---------------------------------------------------------------------------

/// Open a video decoder with GPU acceleration (CUDA → VAAPI → Software fallback).
/// Returns (decoder, stream_index, is_hw).
fn open_gpu_decoder(
    format_ctx: &ffmpeg_next::format::context::Input,
) -> Result<(ffmpeg_next::decoder::Video, usize, bool), String> {
    let video_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or("No video stream")?;
    let stream_index = video_stream.index();
    let params = video_stream.parameters();

    let mut codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("codec context: {e}"))?;

    // Try GPU acceleration: CUDA → Software (skip VAAPI — broken on NVIDIA, spams stderr)
    let is_hw = unsafe {
        if crate::video_decoder::try_init_cuda(codec_ctx.as_mut_ptr()) {
            true
        } else {
            // Software fallback: use half cores
            let half_cores = (std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4) / 2)
                .max(1) as i32;
            (*codec_ctx.as_mut_ptr()).thread_count = half_cores;
            false
        }
    };

    let decoder = codec_ctx.decoder().video()
        .map_err(|e| format!("video decoder: {e}"))?;

    Ok((decoder, stream_index, is_hw))
}

/// Convert a decoded frame (HW or SW) to RGBA at the given target dimensions.
/// For HW frames: GPU→CPU transfer + swscale. For SW frames: just swscale.
/// `sws_cache` is an optional cached sws context pointer for reuse across frames.
fn frame_to_rgba(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
    _is_hw: bool,
    sws_cache: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
) -> Option<Vec<u8>> {
    // Detect actual frame format — CUDA init may "succeed" but format negotiation
    // can fall back to software. Check the real pixel format per-frame.
    let actual_hw = unsafe {
        let fmt = (*frame.as_ptr()).format;
        fmt == ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_CUDA as i32
            || fmt == ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32
    };
    if actual_hw {
        // HW frame path: GPU→CPU transfer then swscale
        unsafe { hw_frame_to_rgba(frame, dst_w, dst_h, sws_cache) }
    } else {
        // SW frame path: direct swscale
        sw_frame_to_rgba(frame, dst_w, dst_h)
    }
}

/// Transfer HW frame to CPU and scale to RGBA at target dimensions.
unsafe fn hw_frame_to_rgba(
    hw_frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
    sws_cache: &mut Option<*mut ffmpeg_next::ffi::SwsContext>,
) -> Option<Vec<u8>> {
    use ffmpeg_next::ffi::*;

    let sw_frame = av_frame_alloc();
    if sw_frame.is_null() {
        return None;
    }

    let ret = av_hwframe_transfer_data(sw_frame, hw_frame.as_ptr(), 0);
    if ret < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }

    let src_w = (*sw_frame).width as u32;
    let src_h = (*sw_frame).height as u32;
    let sw_fmt = std::mem::transmute::<i32, AVPixelFormat>((*sw_frame).format);

    let sws = sws_getCachedContext(
        sws_cache.unwrap_or(std::ptr::null_mut()),
        src_w as i32,
        src_h as i32,
        sw_fmt,
        dst_w as i32,
        dst_h as i32,
        AVPixelFormat::AV_PIX_FMT_RGBA,
        1, // SWS_FAST_BILINEAR
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null(),
    );
    if sws.is_null() {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }
    *sws_cache = Some(sws);

    let rgba_frame = av_frame_alloc();
    if rgba_frame.is_null() {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }
    (*rgba_frame).width = dst_w as i32;
    (*rgba_frame).height = dst_h as i32;
    (*rgba_frame).format = AVPixelFormat::AV_PIX_FMT_RGBA as i32;
    if av_frame_get_buffer(rgba_frame, 0) < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        av_frame_free(&mut (rgba_frame as *mut _));
        return None;
    }

    sws_scale(
        sws,
        (*sw_frame).data.as_ptr() as *const *const u8,
        (*sw_frame).linesize.as_ptr(),
        0,
        src_h as i32,
        (*rgba_frame).data.as_ptr(),
        (*rgba_frame).linesize.as_ptr(),
    );

    av_frame_free(&mut (sw_frame as *mut _));

    let stride = (*rgba_frame).linesize[0] as usize;
    let expected_stride = (dst_w * 4) as usize;
    let data_slice = std::slice::from_raw_parts((*rgba_frame).data[0], stride * dst_h as usize);

    let rgba = if stride == expected_stride {
        data_slice.to_vec()
    } else {
        let mut buf = Vec::with_capacity(expected_stride * dst_h as usize);
        for row in 0..dst_h as usize {
            let start = row * stride;
            buf.extend_from_slice(&data_slice[start..start + expected_stride]);
        }
        buf
    };

    av_frame_free(&mut (rgba_frame as *mut _));
    Some(rgba)
}

/// Convert a software-decoded frame to RGBA at target dimensions.
fn sw_frame_to_rgba(
    frame: &ffmpeg_next::frame::Video,
    dst_w: u32,
    dst_h: u32,
) -> Option<Vec<u8>> {
    use ffmpeg_next::software::scaling;

    let mut scaler = scaling::Context::get(
        frame.format(),
        frame.width(),
        frame.height(),
        ffmpeg_next::format::Pixel::RGBA,
        dst_w,
        dst_h,
        scaling::Flags::FAST_BILINEAR,
    )
    .ok()?;

    let mut rgba_frame = ffmpeg_next::frame::Video::empty();
    scaler.run(frame, &mut rgba_frame).ok()?;

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
    Some(rgba)
}

/// Open a software-only video decoder (no GPU). Used for thumbnails where
/// GPU context creation overhead isn't worth it for a single frame.
fn open_sw_decoder(
    format_ctx: &ffmpeg_next::format::context::Input,
) -> Result<(ffmpeg_next::decoder::Video, usize), String> {
    let video_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or("No video stream")?;
    let stream_index = video_stream.index();
    let params = video_stream.parameters();

    let mut codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("codec context: {e}"))?;

    unsafe {
        let cores = (std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4) as i32)
            .min(16);
        (*codec_ctx.as_mut_ptr()).thread_count = cores;
    }

    let decoder = codec_ctx.decoder().video()
        .map_err(|e| format!("video decoder: {e}"))?;

    Ok((decoder, stream_index))
}

/// Generate a thumbnail for a media file. Uses software decode (CPU) — thumbnails
/// are single small frames, not worth GPU context creation overhead.
pub fn generate_thumbnail(
    path: &std::path::Path,
    seek_secs: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let (mut decoder, stream_index) = open_sw_decoder(&format_ctx)?;

    if seek_secs > 0.1 {
        let ts = (seek_secs * 1_000_000.0) as i64;
        format_ctx.seek(ts, ..ts).map_err(|e| format!("seek: {e}"))?;
    }

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
        return Err("No frame decoded".to_string());
    }

    let src_w = decoded_frame.width();
    let src_h = decoded_frame.height();
    let scale = (160.0 / src_w as f64).min(90.0 / src_h as f64).min(1.0);
    let dst_w = (src_w as f64 * scale) as u32;
    let dst_h = (src_h as f64 * scale) as u32;

    let rgba = sw_frame_to_rgba(&decoded_frame, dst_w, dst_h)
        .ok_or("Failed to convert frame to RGBA")?;
    Ok((rgba, dst_w, dst_h))
}

/// Generate a thumbnail at a specific max width (for viewer-quality poster frames).
/// Uses software decode — single frame, not worth GPU overhead.
pub fn generate_thumbnail_at_size(
    path: &std::path::Path,
    seek_secs: f64,
    max_width: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let (mut decoder, stream_index) = open_sw_decoder(&format_ctx)?;

    if seek_secs > 0.1 {
        let ts = (seek_secs * 1_000_000.0) as i64;
        format_ctx.seek(ts, ..ts).map_err(|e| format!("seek: {e}"))?;
    }

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
        return Err("No frame decoded".to_string());
    }

    let src_w = decoded_frame.width();
    let src_h = decoded_frame.height();
    let scale = (max_width as f64 / src_w as f64).min(1.0);
    let dst_w = ((src_w as f64 * scale) as u32).max(1);
    let dst_h = ((src_h as f64 * scale) as u32).max(1);

    let rgba = sw_frame_to_rgba(&decoded_frame, dst_w, dst_h)
        .ok_or("Failed to convert frame to RGBA")?;
    Ok((rgba, dst_w, dst_h))
}

/// Cached frame decoder — keeps the ffmpeg context open for fast temporal access.
/// Optimized for sequential and near-sequential access patterns (common in Retime).
/// Only seeks when the requested time is far from the current position.
struct CachedFrameDecoder {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::decoder::Video,
    stream_index: usize,
    frame: ffmpeg_next::frame::Video,
    /// Current decoder position in seconds
    current_pts: f64,
    /// Frame duration in seconds (1/fps)
    frame_duration: f64,
    /// Cached frames: (time, rgba, w, h)
    cache: Vec<(f64, Vec<u8>, u32, u32)>,
}

impl CachedFrameDecoder {
    fn open(path: &std::path::Path) -> Option<Self> {
        let format_ctx = ffmpeg_next::format::input(path).ok()?;
        let (decoder, stream_index) = open_sw_decoder(&format_ctx).ok()?;

        // Get frame duration from stream
        let frame_duration = format_ctx.streams().best(ffmpeg_next::media::Type::Video)
            .map(|s| {
                let r = s.avg_frame_rate();
                if r.numerator() > 0 { r.denominator() as f64 / r.numerator() as f64 } else { 1.0 / 24.0 }
            })
            .unwrap_or(1.0 / 24.0);

        Some(Self {
            format_ctx,
            decoder,
            stream_index,
            frame: ffmpeg_next::frame::Video::empty(),
            current_pts: -1.0,
            frame_duration,
            cache: Vec::new(),
        })
    }

    fn decode_at(&mut self, seek_secs: f64) -> Option<(Vec<u8>, u32, u32)> {
        // Check cache first
        for (t, rgba, w, h) in &self.cache {
            if (*t - seek_secs).abs() < self.frame_duration * 0.5 {
                return Some((rgba.clone(), *w, *h));
            }
        }

        // Determine if we need to seek or can just read forward
        let need_seek = if self.current_pts < 0.0 {
            true // first access
        } else if seek_secs < self.current_pts - self.frame_duration {
            true // seeking backwards
        } else if seek_secs > self.current_pts + self.frame_duration * 30.0 {
            true // jumping far forward (>30 frames)
        } else {
            false // close enough — just read forward
        };

        if need_seek {
            let ts = ((seek_secs - 0.5).max(0.0) * 1_000_000.0) as i64;
            let _ = self.format_ctx.seek(ts, ..ts);
            self.decoder.flush();
            self.current_pts = seek_secs - 1.0; // approximate
        }

        // Read frames until we reach or pass the target time
        let _target_us = (seek_secs * 1_000_000.0) as i64;
        let mut best_frame: Option<(Vec<u8>, u32, u32, f64)> = None;

        for (stream, packet) in self.format_ctx.packets() {
            if stream.index() != self.stream_index {
                continue;
            }
            let _ = self.decoder.send_packet(&packet);
            while self.decoder.receive_frame(&mut self.frame).is_ok() {
                let pts = self.frame.pts().unwrap_or(0);
                let time_base = stream.time_base();
                let frame_secs = pts as f64 * time_base.numerator() as f64 / time_base.denominator() as f64;
                self.current_pts = frame_secs;

                let w = self.frame.width();
                let h = self.frame.height();

                if let Some(rgba) = sw_frame_to_rgba(&self.frame, w, h) {
                    best_frame = Some((rgba, w, h, frame_secs));
                }

                // If we've reached or passed the target, stop
                if frame_secs >= seek_secs - self.frame_duration * 0.5 {
                    if let Some((rgba, w, h, t)) = best_frame {
                        // Cache this frame (keep last 8 frames for Retime interpolation)
                        if self.cache.len() >= 8 {
                            self.cache.remove(0);
                        }
                        self.cache.push((t, rgba.clone(), w, h));
                        return Some((rgba, w, h));
                    }
                }
            }
        }

        // Return whatever we decoded last
        best_frame.map(|(rgba, w, h, t)| {
            if self.cache.len() >= 8 { self.cache.remove(0); }
            self.cache.push((t, rgba.clone(), w, h));
            (rgba, w, h)
        })
    }
}

/// Decode a single full-resolution frame from a video file at the given time.
/// Used for OFX temporal clip access — plugins that need frames at arbitrary times.
pub fn decode_frame_at_time(
    path: &std::path::Path,
    seek_secs: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let (mut decoder, stream_index) = open_sw_decoder(&format_ctx)?;

    if seek_secs > 0.01 {
        let ts = (seek_secs * 1_000_000.0) as i64;
        format_ctx.seek(ts, ..ts).map_err(|e| format!("seek: {e}"))?;
    }

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
        return Err("No frame decoded".to_string());
    }

    let w = decoded_frame.width();
    let h = decoded_frame.height();
    let rgba = sw_frame_to_rgba(&decoded_frame, w, h)
        .ok_or("Failed to convert frame to RGBA")?;
    Ok((rgba, w, h))
}

/// Check if an RGBA frame is effectively black (< 5% average brightness).
/// Samples ~64 evenly spaced pixels for speed.
pub fn is_frame_black(rgba: &[u8], width: u32, height: u32) -> bool {
    let total_pixels = (width * height) as usize;
    if total_pixels == 0 || rgba.len() < total_pixels * 4 {
        return true;
    }
    let step = (total_pixels / 64).max(1);
    let mut brightness_sum: u64 = 0;
    let mut samples = 0u64;
    for i in (0..total_pixels).step_by(step) {
        let idx = i * 4;
        brightness_sum += rgba[idx] as u64 + rgba[idx + 1] as u64 + rgba[idx + 2] as u64;
        samples += 1;
    }
    if samples == 0 {
        return true;
    }
    // Average brightness per channel (0-255). Threshold: 13 (~5% of 255)
    let avg = brightness_sum / (samples * 3);
    avg < 13
}

/// Generate a thumbnail, skipping black frames by trying progressively later positions.
/// Tries the initial seek, then 5%, 10%, 20%, 30% of duration.
pub fn generate_thumbnail_non_black(
    path: &std::path::Path,
    initial_seek_secs: f64,
    duration_secs: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let positions = [
        initial_seek_secs,
        duration_secs * 0.05,
        duration_secs * 0.10,
        duration_secs * 0.20,
        duration_secs * 0.30,
    ];
    let mut last_result = None;
    for &secs in &positions {
        if secs < 0.0 {
            continue;
        }
        match generate_thumbnail(path, secs) {
            Ok((rgba, w, h)) => {
                if !is_frame_black(&rgba, w, h) {
                    return Ok((rgba, w, h));
                }
                last_result = Some((rgba, w, h));
            }
            Err(e) => {
                if last_result.is_none() {
                    last_result = Some(generate_thumbnail(path, 0.0).map_err(|_| e)?);
                }
            }
        }
    }
    // All positions were black — return the last one anyway
    last_result.ok_or_else(|| "No frame decoded".to_string())
}

/// Same as generate_thumbnail_non_black but at a specific max width.
pub fn generate_thumbnail_at_size_non_black(
    path: &std::path::Path,
    initial_seek_secs: f64,
    duration_secs: f64,
    max_width: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let positions = [
        initial_seek_secs,
        duration_secs * 0.05,
        duration_secs * 0.10,
        duration_secs * 0.20,
        duration_secs * 0.30,
    ];
    let mut last_result = None;
    for &secs in &positions {
        if secs < 0.0 {
            continue;
        }
        if let Ok((rgba, w, h)) = generate_thumbnail_at_size(path, secs, max_width) {
            if !is_frame_black(&rgba, w, h) {
                return Ok((rgba, w, h));
            }
            last_result = Some((rgba, w, h));
        }
    }
    last_result.ok_or_else(|| "No non-black frame found".to_string())
}

/// Generate filmstrip thumbnails incrementally with GPU acceleration (CUDA → VAAPI → CPU fallback).
/// Calls `on_frame` for each generated frame. Returns the total number of frames generated.
pub fn generate_filmstrip_incremental(
    path: &std::path::Path,
    duration_secs: f64,
    interval_secs: f64,
    target_height: u32,
    mut on_frame: impl FnMut(f64, Vec<u8>, u32, u32),
) -> Result<usize, String> {
    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let (mut decoder, stream_index, is_hw) = open_gpu_decoder(&format_ctx)?;

    let src_w = decoder.width();
    let src_h = decoder.height();
    let aspect = src_w as f64 / src_h.max(1) as f64;
    let dst_h = target_height;
    let dst_w = (dst_h as f64 * aspect).round() as u32;

    let mut sws_cache: Option<*mut ffmpeg_next::ffi::SwsContext> = None;
    let mut count = 0;

    let num_frames = ((duration_secs / interval_secs).ceil() as usize).min(500);
    let accel_label = if is_hw { "GPU" } else { "CPU" };
    tracing::info!("Filmstrip: generating {} frames via {} decode for {:?}", num_frames, accel_label, path);

    for i in 0..=num_frames {
        let seek_secs = i as f64 * interval_secs;
        if seek_secs > duration_secs {
            break;
        }

        // Throttle: yield between frames so playback decode isn't starved.
        // GPU decode is fast so use shorter sleep; CPU needs more breathing room.
        if i > 0 {
            let sleep_ms = if is_hw { 10 } else { 100 };
            std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
        }

        let ts = (seek_secs * 1_000_000.0) as i64;
        if format_ctx.seek(ts, ..ts).is_err() {
            continue;
        }
        decoder.flush();

        let mut decoded_frame = ffmpeg_next::frame::Video::empty();
        let mut got_frame = false;
        let target_pts = ts;

        for (stream, packet) in format_ctx.packets() {
            if stream.index() != stream_index {
                continue;
            }
            let _ = decoder.send_packet(&packet);
            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                let frame_ts = decoded_frame.timestamp().unwrap_or(0);
                let tb = stream.time_base();
                let frame_us = frame_ts * 1_000_000 * tb.numerator() as i64 / tb.denominator() as i64;
                if frame_us >= target_pts - 100_000 {
                    got_frame = true;
                    break;
                }
            }
            if got_frame { break; }
        }

        if !got_frame {
            continue;
        }

        if let Some(rgba) = frame_to_rgba(&decoded_frame, dst_w, dst_h, is_hw, &mut sws_cache) {
            on_frame(seek_secs, rgba, dst_w, dst_h);
            count += 1;
        }
    }

    Ok(count)
}
