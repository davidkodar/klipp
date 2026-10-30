//! Audio decoder thread: ffmpeg demux + decode + resample →
//! interleaved f32 stereo samples in a lock-free ring buffer that
//! cpal's output callback reads from.
//!
//! # Two-phase decode rule
//!
//! Same `send_packet` / `receive_frame` asymmetry as the video
//! decoder — drain receive_frame to `EAGAIN` before every
//! send_packet, and if send_packet returns `EAGAIN` stash the
//! packet and retry after the next drain. See the
//! [`crate::video_decoder`] module header for the full rationale.
//! Silent packet loss here shows up as a clipped tail at the end
//! of every audio region and progressive A/V drift.
//!
//! # Resampled-frame `data(0).len()` is NOT the usable size
//!
//! FFmpeg aligns resampler output to a buffer-internal linesize
//! that may be padded beyond the actual sample count. Reading all
//! of `resampled.data(0)` includes this padding and produces loud
//! clicks or clipping in the output. Use
//! `resampled.samples() * channels * size_of::<f32>()` as the byte
//! count instead.
//!
//! # Resampler lifetime
//!
//! The [`ffmpeg_next::software::resampling::Context`] is rebuilt
//! after every seek. Its internal state is time-continuous, so
//! reusing it across a discontinuity in the input stream produces
//! smearing between the two sides of the jump.
//!
//! # Channel layout
//!
//! Incoming frames may declare a non-default channel layout
//! (5.1(side), quad, etc.). The resampler must be configured with
//! the frame's actual layout, not the default for its channel
//! count — otherwise it rejects every frame and the output is
//! silent. See the conform_audio / audio_decoder / skim_audio
//! paths for the "override on every setup" pattern.

use crossbeam_channel::{Receiver, Sender};
use klipp_audio::dsp::{AudioEffectChain, AudioProcessContext};
use klipp_core::AudioEffectInstance;
use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::HeapRb;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, warn};

use crate::video_decoder::DecodeCommand;

/// Decoded audio samples are pushed to a lock-free ring buffer as f32 interleaved stereo.
/// The cpal output callback reads from the consumer half.
pub type AudioProducer = ringbuf::HeapProd<f32>;
pub type AudioConsumer = ringbuf::HeapCons<f32>;

pub struct AudioDecoder {
    decode_thread: Option<JoinHandle<()>>,
    decode_cmd_tx: Sender<DecodeCommand>,
    pub volume: Arc<AtomicU32>,
    pub muted: Arc<AtomicBool>,
    /// PTS (microseconds) of the first audio frame produced after a seek.
    /// Engine reads this to precisely sync the audio clock.
    pub post_seek_pts_us: Arc<AtomicU64>,
    /// Incremented by the decode thread after each seek completes (first frame produced).
    /// Engine compares against its own counter to know when audio is ready.
    pub seek_generation: Arc<AtomicU64>,
    /// Total chain latency in stereo frames advertised by the decode
    /// thread after constructing the AudioEffectChain. Mix-thread
    /// reads this each block via the StagingEntry's matching Arc and
    /// pushes the value into AudioMixer's per-track PDC state.
    /// Initial value 0 — for chains with no plugins, no compensation
    /// is needed and the mixer's delay-line stays None. v1 sets the
    /// value once at thread start (chain shape is static for the
    /// thread's lifetime); variable latency is filed as
    /// PDC-NEXT-VARIABLE-LATENCY.
    pub latency_advertise: Arc<AtomicU32>,
    /// New effect settings for the running chain; see
    /// [`crate::audio_source::AudioSource::update_effect_settings`].
    pub effect_settings: crate::audio_source::EffectSettingsSlot,
}

impl AudioDecoder {
    /// Open a file's audio stream for decoding.
    /// Returns the decoder and a consumer for reading samples.
    /// `output_rate` is the cpal device's sample rate.
    /// `audio_effects` are the per-clip DSP effects to apply after decode/resample.
    /// `audio_effect_keyframes` are the per-clip param/bypass/wet-dry keyframe
    /// tracks (`clip.audio_effect_keyframes`); when non-empty the chain runs
    /// in sub-chunked evaluation mode.
    /// `clip_source_in_secs` is the clip's source-in time — used to derive
    /// clip-relative `clip_time_secs` for keyframe evaluation in the chain.
    pub fn open(
        path: &Path,
        output_rate: u32,
        audio_effects: Vec<AudioEffectInstance>,
        audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
        clip_source_in_secs: f64,
    ) -> Result<(Self, AudioConsumer), AudioDecoderError> {
        // Validate that file has an audio stream
        {
            let ctx = klipp_media::open_input(path)
                .map_err(|e| AudioDecoderError::Open(e.to_string()))?;
            ctx.streams()
                .best(ffmpeg_next::media::Type::Audio)
                .ok_or(AudioDecoderError::NoAudioStream)?;
        }

        // Ring buffer: ~2s of stereo f32 audio at output_rate
        // Larger buffer prevents underruns from bursty decode timing.
        let buf_size = (output_rate as usize) * 2 * 2; // stereo * 2 seconds
        let (producer, consumer) = HeapRb::<f32>::new(buf_size).split();

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let volume = Arc::new(AtomicU32::new(100)); // 100 = 1.0
        let muted = Arc::new(AtomicBool::new(false));
        let post_seek_pts_us = Arc::new(AtomicU64::new(0));
        let seek_generation = Arc::new(AtomicU64::new(0));
        let latency_advertise = Arc::new(AtomicU32::new(0));

        let vol = volume.clone();
        let mute = muted.clone();
        let pts_clone = post_seek_pts_us.clone();
        let gen_clone = seek_generation.clone();
        let latency_clone = latency_advertise.clone();
        let path_owned = path.to_path_buf();
        let effect_settings: crate::audio_source::EffectSettingsSlot = Default::default();
        let settings_rx = effect_settings.clone();

        let thread = std::thread::Builder::new()
            .name("klipp-adec".into())
            .spawn(move || {
                crate::thread_guard::catch_panic("klipp-adec", || {
                    audio_decode_thread(path_owned, output_rate, producer, cmd_rx, vol, mute, pts_clone, gen_clone, latency_clone, audio_effects, audio_effect_keyframes, clip_source_in_secs, settings_rx);
                });
            })
            .map_err(|e| AudioDecoderError::Open(format!("spawn thread: {e}")))?;

        debug!("AudioDecoder opened: {:?}, output_rate={output_rate}", path);

        Ok((
            Self {
                decode_thread: Some(thread),
                decode_cmd_tx: cmd_tx,
                volume,
                muted,
                post_seek_pts_us,
                seek_generation,
                latency_advertise,
                effect_settings,
            },
            consumer,
        ))
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::SeekTo(secs, 0));
    }

    pub fn resume(&self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Resume);
    }

    pub fn pause(&self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Pause);
    }

    pub fn stop(&mut self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Stop);
        if let Some(thread) = self.decode_thread.take() {
            let _ = thread.join();
        }
    }

    pub fn stop_detached(&mut self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Stop);
        self.decode_thread.take();
    }
}

impl Drop for AudioDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
pub enum AudioDecoderError {
    Open(String),
    NoAudioStream,
}

impl std::fmt::Display for AudioDecoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "audio open: {e}"),
            Self::NoAudioStream => write!(f, "no audio stream"),
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::drop_non_drop)]
fn audio_decode_thread(
    path: PathBuf,
    output_rate: u32,
    mut producer: AudioProducer,
    cmd_rx: Receiver<DecodeCommand>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    post_seek_pts_us: Arc<AtomicU64>,
    seek_generation: Arc<AtomicU64>,
    latency_advertise: Arc<AtomicU32>,
    audio_effects: Vec<AudioEffectInstance>,
    audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    clip_source_in_secs: f64,
    effect_settings: crate::audio_source::EffectSettingsSlot,
) {
    // Flush denormals on this DSP thread so a silent
    // input (between clips, post-fade-out) can't drop the biquad/EQ
    // state into denormal range and stall the FPU ~50–100×.
    klipp_audio::flush_denormals_to_zero();

    let mut format_ctx = match klipp_media::open_input(&path) {
        Ok(ctx) => ctx,
        Err(e) => {
            warn!("audio_decode_thread: failed to open {:?}: {e}", path);
            return;
        }
    };

    let audio_stream = match format_ctx.streams().best(ffmpeg_next::media::Type::Audio) {
        Some(s) => s,
        None => {
            warn!("audio_decode_thread: no audio stream in {:?}", path);
            return;
        }
    };

    let stream_index = audio_stream.index();
    let time_base = audio_stream.time_base();
    let params = audio_stream.parameters();

    let codec_ctx = match ffmpeg_next::codec::context::Context::from_parameters(params) {
        Ok(ctx) => ctx,
        Err(e) => {
            warn!("audio_decode_thread: codec context error: {e}");
            return;
        }
    };

    let mut decoder = match codec_ctx.decoder().audio() {
        Ok(d) => d,
        Err(e) => {
            warn!("audio_decode_thread: audio decoder error: {e}");
            return;
        }
    };

    // Set up resampler: source format → f32 stereo at output_rate
    let src_format = decoder.format();
    let src_rate = decoder.rate();
    // ALWAYS derive the layout from the channel count rather than from
    // `decoder.channel_layout()`. PCM WAV streams (the most common
    // import format) often have an `unknown` channel_layout in their
    // container header — `decoder.channel_layout()` then returns an
    // empty `ChannelLayout`, which makes the resampler refuse every
    // frame with "Input changed". `ChannelLayout::default(channels)`
    // is reliable for mono/stereo content (which is essentially all
    // music and dialog audio in practice).
    let src_channels = decoder.channels();
    let src_layout = ffmpeg_next::ChannelLayout::default(src_channels as i32);

    let mut resampler = match ffmpeg_next::software::resampling::Context::get(
        src_format,
        src_layout,
        src_rate,
        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
        ffmpeg_next::ChannelLayout::STEREO,
        output_rate,
    ) {
        Ok(r) => r,
        Err(e) => {
            warn!("audio_decode_thread: resampler error: {e}");
            return;
        }
    };

    let tb_num = time_base.numerator() as f64;
    let tb_den = time_base.denominator() as f64;
    let tb_f64 = if tb_den != 0.0 { tb_num / tb_den } else { 1.0 / 48000.0 };

    // Build audio effect chain from per-clip effects + keyframe tracks.
    let mut effect_chain = if !audio_effects.is_empty() {
        let mut chain = AudioEffectChain::from_instances_with_kfs(
            &audio_effects,
            &audio_effect_keyframes,
            output_rate,
        );
        // Start costly effects now, not on the first block after Play.
        chain.prepare(2);
        debug!(
            "audio_decode_thread: {} audio effects active ({} kf tracks)",
            chain.slot_count(),
            audio_effect_keyframes.len(),
        );
        // The chain trims its own latency (`process_aligned`), so this
        // track already lines up with the picture; advertising it too would
        // make the mixer delay every other track by the same amount.
        latency_advertise.store(0, Ordering::Release);
        Some(chain)
    } else {
        // No plugins → no latency. (The atomic was initialised to 0
        // at AudioDecoder::open; this store is defensive in case a
        // future reuse path ever leaves stale state.)
        latency_advertise.store(0, Ordering::Release);
        None
    };

    debug!(
        "audio_decode_thread: started, stream={stream_index}, src={src_rate}Hz {:?} → {output_rate}Hz stereo f32",
        src_format
    );

    let mut paused = true;
    let mut eof = false;
    let mut seek_target: Option<f64> = None;
    let mut packets = format_ctx.packets();
    let mut eof_from_demuxer = false;
    let mut pending_packet: Option<ffmpeg_next::Packet> = None;
    let mut frame = ffmpeg_next::frame::Audio::empty();

    // Temporary buffer for samples that couldn't fit in the ring buffer
    let mut overflow_buf: Vec<f32> = Vec::new();

    // Accumulation buffer for batching small decoded frames before effect processing.
    // 2048 stereo frames ≈ 42ms at 48kHz — matches pcm_reader chunk size.
    let fx_batch_target = if effect_chain.is_some() { 2048 * 2 } else { 0 };
    let mut fx_accum: Vec<f32> = Vec::new();
    // Source-time of the FIRST frame currently in fx_accum. Captured when
    // the accumulator is empty and we're about to extend; cleared on seek
    // and after each chain.process. Used to derive clip_time_secs for
    // keyframe evaluation in the audio effect chain.
    let mut fx_accum_start_pts_secs: Option<f64> = None;

    loop {
        // New settings for the running chain (a slider moved), in place.
        if let Some(ref mut chain) = effect_chain {
            if let Some(settings) = effect_settings.try_lock().ok().and_then(|mut p| p.take()) {
                chain.apply_settings(&settings);
            }
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DecodeCommand::SeekTo(secs, _) | DecodeCommand::SeekOn(secs, _) | DecodeCommand::SeekFromKeyframe(secs, _) => {
                    let ts = (secs * 1_000_000.0) as i64;
                    debug!("audio_decode_thread: seek to {secs:.3}s");
                    drop(packets);
                    let _ = format_ctx.seek(ts, ..ts);
                    packets = format_ctx.packets();
                    decoder.flush();
                    // Recreate resampler to flush its internal buffers
                    match ffmpeg_next::software::resampling::Context::get(
                        src_format,
                        src_layout,
                        src_rate,
                        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
                        ffmpeg_next::ChannelLayout::STEREO,
                        output_rate,
                    ) {
                        Ok(r) => resampler = r,
                        Err(e) => {
                            warn!("audio_decode_thread: resampler recreate failed: {e}");
                        }
                    }
                    seek_target = Some(secs);
                    eof = false;
                    eof_from_demuxer = false;
                    pending_packet = None;
                    overflow_buf.clear();
                    fx_accum.clear();
                    fx_accum_start_pts_secs = None;
                    // Reset effect chain state to avoid stale audio bleeding across seeks
                    if let Some(ref mut chain) = effect_chain {
                        chain.reset();
                    }
                }
                DecodeCommand::Pause => paused = true,
                DecodeCommand::Resume => {
                    paused = false;
                    eof = false;
                }
                DecodeCommand::Stop => {
                    debug!("audio_decode_thread: stop");
                    return;
                }
            }
        }

        if paused || eof {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // ── Flush overflow buffer first ──
        // If we had samples that didn't fit last time, push them now.
        if !overflow_buf.is_empty() {
            let vacant = producer.vacant_len();
            if vacant == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            let to_push = overflow_buf.len().min(vacant);
            producer.push_slice(&overflow_buf[..to_push]);
            overflow_buf.drain(..to_push);
            if !overflow_buf.is_empty() {
                continue; // still have overflow, wait for more space
            }
        }
        // With effects, render only a little ahead, so a setting changed
        // while playing is heard soon (see EFFECT_READ_AHEAD_SECS_X1000).
        if effect_chain.is_some()
            && producer.occupied_len()
                >= crate::pcm_reader::EFFECT_READ_AHEAD_SECS_X1000 as usize * output_rate as usize * 2 / 1000
        {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // ── Phase 1: Drain decoded audio frames from the decoder ──
        let mut drained_any = false;
        loop {
            if decoder.receive_frame(&mut frame).is_err() {
                break; // need more packets
            }
            drained_any = true;

            // Skip pre-seek frames based on PTS
            if let Some(target) = seek_target {
                let pts_secs = frame
                    .pts()
                    .map(|pts| pts as f64 * tb_f64)
                    .unwrap_or(0.0);
                if pts_secs < target - 0.05 {
                    continue; // discard audio before seek target
                }
                debug!("audio_decode_thread: first post-seek frame at {pts_secs:.3}s (target={target:.3}s)");
                post_seek_pts_us.store((pts_secs * 1_000_000.0) as u64, Ordering::Release);
                seek_generation.fetch_add(1, Ordering::Release);
                seek_target = None;
            }

            // Force the frame's channel_layout to match the resampler's
            // configured input layout. The resampler rejects frames whose
            // layout differs — covering both the PCM WAV case (empty
            // layout) *and* the AC-3 / DTS case where the decoder gives
            // back 5.1(side) while `ChannelLayout::default(6)` is plain
            // 5.1. The old `if is_empty()` gate silently dropped every
            // AC-3 frame and produced a silent clip; overriding
            // unconditionally is safe because src_layout is derived from
            // the same channel count.
            frame.set_channel_layout(src_layout);

            // Pre-allocate the output frame ourselves with the rate-scaled
            // worst-case sample count. ffmpeg-next's `Context::run` would
            // otherwise call `output.alloc(format, input.samples(), layout)`
            // when the output frame is empty — sizing the buffer for
            // input.samples() STEREO frames, which is correct only when
            // src_rate == dst_rate. For upsampling (e.g. 44.1 kHz → 48 kHz)
            // that's too small and `swr_convert_frame` silently truncates
            // every call, leaving samples buffered in the resampler that
            // never come out. The `+ 256` matches FFmpeg's own internal
            // padding in its auto-allocation path.
            let out_samples =
                frame.samples() * output_rate as usize / src_rate.max(1) as usize + 256;
            let mut resampled = ffmpeg_next::frame::Audio::new(
                ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
                out_samples,
                ffmpeg_next::ChannelLayout::STEREO,
            );
            if resampler.run(&frame, &mut resampled).is_err() {
                continue;
            }

            // Use actual sample count, NOT data(0).len() which may include
            // linesize padding — reading padding bytes causes audio artifacts.
            let nb_samples = resampled.samples();
            let channels = 2usize; // we requested stereo from resampler
            let actual_floats = nb_samples * channels;
            let data = resampled.data(0);
            if actual_floats == 0 || data.len() < actual_floats * 4 {
                continue;
            }

            let floats: &[f32] = bytemuck::cast_slice(&data[..actual_floats * 4]);

            // When effects are active, accumulate small decoded frames into
            // larger batches (≈42ms) so DSP algorithms with internal state
            // (pitch shift, reverb, delay) get enough samples to work correctly.
            let batching_chain = if fx_batch_target > 0 { effect_chain.as_mut() } else { None };
            if let Some(chain) = batching_chain {
                // Capture the source-time of the first frame in this batch
                // so chain.process can derive clip_time_secs.
                if fx_accum.is_empty() {
                    fx_accum_start_pts_secs = frame.pts().map(|pts| pts as f64 * tb_f64);
                }
                fx_accum.extend_from_slice(floats);
                if fx_accum.len() < fx_batch_target {
                    continue; // keep accumulating
                }
                // Process the accumulated batch
                let clip_time_secs = fx_accum_start_pts_secs
                    .map(|secs| (secs - clip_source_in_secs).max(0.0))
                    .unwrap_or(0.0);
                let ctx = AudioProcessContext {
                    sample_rate: output_rate,
                    channels: 2,
                    clip_time_secs,
                };
                chain.process_aligned(&mut fx_accum, &ctx);
                fx_accum_start_pts_secs = None;

                let vol = volume.load(Ordering::Relaxed) as f32 / 100.0;
                let is_muted = muted.load(Ordering::Relaxed);
                let samples: Vec<f32> = if is_muted {
                    vec![0.0f32; fx_accum.len()]
                } else if (vol - 1.0).abs() < 0.01 {
                    std::mem::take(&mut fx_accum)
                } else {
                    fx_accum.iter().map(|s| s * vol).collect()
                };
                if !samples.is_empty() {
                    fx_accum.clear();
                    let vacant = producer.vacant_len();
                    if samples.len() <= vacant {
                        producer.push_slice(&samples);
                    } else {
                        if vacant > 0 {
                            producer.push_slice(&samples[..vacant]);
                        }
                        overflow_buf.extend_from_slice(&samples[vacant..]);
                        break;
                    }
                }
                continue;
            }

            // No effects — process each frame directly (no batching needed)
            let vol = volume.load(Ordering::Relaxed) as f32 / 100.0;
            let is_muted = muted.load(Ordering::Relaxed);

            // Prepare samples with volume applied
            let samples: Vec<f32> = if is_muted {
                vec![0.0f32; floats.len()]
            } else if (vol - 1.0).abs() < 0.01 {
                floats.to_vec()
            } else {
                floats.iter().map(|s| s * vol).collect()
            };

            // Push to ring buffer, saving overflow if it doesn't all fit
            let vacant = producer.vacant_len();
            if samples.len() <= vacant {
                producer.push_slice(&samples);
            } else {
                // Push what fits, buffer the rest
                if vacant > 0 {
                    producer.push_slice(&samples[..vacant]);
                }
                overflow_buf.extend_from_slice(&samples[vacant..]);
                break; // ring buffer full, stop draining
            }
        }

        // Flush any remaining accumulated effect samples when no more frames drained
        if !fx_accum.is_empty() && !drained_any {
            if let Some(ref mut chain) = effect_chain {
                let clip_time_secs = fx_accum_start_pts_secs
                    .map(|secs| (secs - clip_source_in_secs).max(0.0))
                    .unwrap_or(0.0);
                let ctx = AudioProcessContext {
                    sample_rate: output_rate,
                    channels: 2,
                    clip_time_secs,
                };
                chain.process_aligned(&mut fx_accum, &ctx);
                fx_accum_start_pts_secs = None;
                let vol = volume.load(Ordering::Relaxed) as f32 / 100.0;
                let is_muted = muted.load(Ordering::Relaxed);
                let samples: Vec<f32> = if is_muted {
                    vec![0.0f32; fx_accum.len()]
                } else if (vol - 1.0).abs() < 0.01 {
                    std::mem::take(&mut fx_accum)
                } else {
                    fx_accum.iter().map(|s| s * vol).collect()
                };
                fx_accum.clear();
                let vacant = producer.vacant_len();
                let to_push = samples.len().min(vacant);
                if to_push > 0 {
                    producer.push_slice(&samples[..to_push]);
                }
                if to_push < samples.len() {
                    overflow_buf.extend_from_slice(&samples[to_push..]);
                }
            }
        }

        // ── Phase 2: Feed next packet to the decoder ──
        if let Some(pkt) = pending_packet.take() {
            match decoder.send_packet(&pkt) {
                Ok(()) => {}
                Err(_) => {
                    pending_packet = Some(pkt);
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
            }
        } else if !eof_from_demuxer {
            match packets.next() {
                Some((stream, packet)) => {
                    if stream.index() == stream_index {
                        match decoder.send_packet(&packet) {
                            Ok(()) => {}
                            Err(_) => {
                                pending_packet = Some(packet);
                            }
                        }
                    }
                    // Non-audio packets: just skip
                }
                None => {
                    let _ = decoder.send_eof();
                    eof_from_demuxer = true;
                }
            }
        } else if !drained_any {
            eof = true;
        }
    }
}
