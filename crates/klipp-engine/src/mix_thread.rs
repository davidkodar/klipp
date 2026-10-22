use crate::audio_decoder::{AudioConsumer, AudioProducer};
use crossbeam_channel::{Receiver, Sender};
use klipp_audio::{AudioMixer, LufsMeter, LufsReadings};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use std::collections::VecDeque;
use crate::sync::MasterClock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, warn};

/// The samples mixed in one block, interleaved L+R: 512 stereo frames,
/// ≈ 10.7 ms at 48 kHz. The mixer ramps a changed volume or pan over one.
pub(crate) const MIX_CHUNK_SAMPLES: usize = 1024;

/// One source's clip settings, applied to its own samples before the
/// track sums them: volume times gain, a balance pan, and the channel
/// config (0 as is, 1 mono downmix, 2 swap left and right), in the
/// order the export applies them. Each source carries its own, so the
/// two clips of a crossfade and the clips on other tracks keep theirs;
/// set once on the whole output, the first clip's settings played every
/// track.
pub struct SourceMix {
    volume: AtomicU32,
    pan: AtomicU32,
    channel_config: AtomicU32,
    /// The mixed stereo frame the source's first sample is heard at, on
    /// the count [`MixThread::mixed_frames`] keeps: a sound opened ahead
    /// of its clip is silent until then, so it starts on its own sample
    /// as the export starts it (#1588). `u64::MAX` (the default): at
    /// once.
    starts_at: AtomicU64,
    /// The mixed frame the source falls silent at, on the same count: its
    /// clip's end, or the cut into the next clip on its track, so it ends
    /// on that sample rather than at the tick that retires it (#1632).
    /// `u64::MAX` (the default): it plays on until it is retired.
    ends_at: AtomicU64,
}

impl Default for SourceMix {
    fn default() -> Self {
        Self {
            volume: AtomicU32::new(1.0f32.to_bits()),
            pan: AtomicU32::new(0.0f32.to_bits()),
            channel_config: AtomicU32::new(0),
            starts_at: AtomicU64::new(u64::MAX),
            ends_at: AtomicU64::new(u64::MAX),
        }
    }
}

impl SourceMix {
    /// Start the source at mixed frame `frame` (see `starts_at`).
    pub fn start_at(&self, frame: u64) {
        self.starts_at.store(frame, Ordering::Relaxed);
    }

    /// End the source at mixed frame `frame` (see `ends_at`).
    pub fn end_at(&self, frame: u64) {
        self.ends_at.store(frame, Ordering::Relaxed);
    }

    /// The interleaved samples of a chunk mixed from frame `at` that this
    /// source is heard in up to its end: all of `len` before it.
    fn heard_until(&self, at: u64, len: usize) -> usize {
        match self.ends_at.load(Ordering::Relaxed) {
            u64::MAX => len,
            end => (end.saturating_sub(at).saturating_mul(2)).min(len as u64) as usize,
        }
    }

    /// The silence, in interleaved samples, a chunk of `len` mixed from
    /// frame `at` owes this source before its first sample.
    fn silence_before(&self, at: u64, len: usize) -> usize {
        match self.starts_at.load(Ordering::Relaxed) {
            u64::MAX => 0,
            start => (start.saturating_sub(at).saturating_mul(2)).min(len as u64) as usize,
        }
    }

    pub fn set(&self, volume: f32, pan: f32, channel_config: u32) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
        self.pan.store(pan.to_bits(), Ordering::Relaxed);
        self.channel_config.store(channel_config, Ordering::Relaxed);
    }

    /// Apply the settings to interleaved stereo samples.
    pub fn apply(&self, samples: &mut [f32]) {
        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let pan = f32::from_bits(self.pan.load(Ordering::Relaxed));
        let channel_config = self.channel_config.load(Ordering::Relaxed);
        if (volume - 1.0).abs() > 0.001 || pan.abs() > 0.001 {
            let pan_l = if pan <= 0.0 { 1.0 } else { 1.0 - pan };
            let pan_r = if pan >= 0.0 { 1.0 } else { 1.0 + pan };
            for pair in samples.chunks_exact_mut(2) {
                pair[0] *= volume * pan_l;
                pair[1] *= volume * pan_r;
            }
        }
        match channel_config {
            1 => {
                for pair in samples.chunks_exact_mut(2) {
                    let mono = (pair[0] + pair[1]) * 0.5;
                    pair[0] = mono;
                    pair[1] = mono;
                }
            }
            2 => {
                for pair in samples.chunks_exact_mut(2) {
                    pair.swap(0, 1);
                }
            }
            // One side of a stereo clip split to mono (#922).
            5 => {
                for pair in samples.chunks_exact_mut(2) {
                    pair[1] = pair[0];
                }
            }
            6 => {
                for pair in samples.chunks_exact_mut(2) {
                    pair[0] = pair[1];
                }
            }
            _ => {}
        }
    }
}

enum MixCommand {
    /// Add a staging consumer for a track. `latency_advertise` is
    /// the source decoder's [`AudioDecoder::latency_advertise`] —
    /// the mix loop polls it each block and pushes the value into
    /// AudioMixer's per-track PDC state for delay-line compensation.
    AddStaging {
        track_index: usize,
        consumer: AudioConsumer,
        latency_advertise: Arc<AtomicU32>,
        mix: Arc<SourceMix>,
    },
    /// Clear all staging consumers (called on stop/clip change).
    ClearAll,
    /// Drop the staging consumer of one track: its clip has ended.
    RemoveStaging { track_index: usize },
    /// Drop the staging fed through `mix` alone: one of two sources a
    /// track has across a cut (#1632).
    RemoveStagingOf { mix: Arc<SourceMix> },
    /// Drain all staging consumers and main producer (on seek).
    /// The mix thread sets the ack flag when done.
    Flush { ack: Arc<AtomicBool> },
    /// Stop the mix thread.
    Stop,
}

/// Reads from per-source staging ring buffers, mixes via AudioMixer,
/// and pushes the mixed output to a main ring buffer that AudioOutput reads.
pub struct MixThread {
    thread: Option<JoinHandle<()>>,
    cmd_tx: Sender<MixCommand>,
    lufs_readings: Arc<Mutex<LufsReadings>>,
    lufs_reset: Arc<AtomicBool>,
    /// Shared interleaved-stereo PCM ring feeding the UI spectrum analyser.
    /// Capped to ~2 seconds of audio; older samples drop off the front.
    spectrum_buf: Arc<Mutex<VecDeque<f32>>>,
    spectrum_sample_rate: u32,
    /// Number of samples currently buffered in the main ring (the one cpal
    /// reads from). Updated by the mix loop on every iteration. Used by the
    /// engine after seek to pre-fill audio before resuming the cpal stream
    /// without sleeping a fixed duration.
    main_buffered: Arc<AtomicUsize>,
    /// Stereo frames the mix loop has mixed since it started: the count
    /// a source's [`SourceMix::start_at`] is given on.
    mixed_frames: Arc<AtomicU64>,
}

impl MixThread {
    /// Create the mix thread and the main output ring buffer.
    /// Returns `(MixThread, AudioConsumer)` — pass the consumer to `AudioOutput::new()`.
    pub fn new(
        sample_rate: u32,
        mixer: Arc<Mutex<AudioMixer>>,
        master_clock: Arc<MasterClock>,
        tp_oversample: bool,
    ) -> (Self, AudioConsumer) {
        // Main ring buffer: ~200ms stereo f32 at device sample rate
        // Small buffer = low latency for volume/pan/mute changes
        let buf_size = (sample_rate as usize) * 2 / 5;
        let (producer, consumer) = HeapRb::<f32>::new(buf_size).split();

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();

        let lufs_readings = Arc::new(Mutex::new(LufsReadings::default()));
        let lufs_reset = Arc::new(AtomicBool::new(false));
        let spectrum_buf = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let main_buffered = Arc::new(AtomicUsize::new(0));
        let mixed_frames = Arc::new(AtomicU64::new(0));
        let mixed_frames_writer = mixed_frames.clone();
        let lufs_readings_writer = lufs_readings.clone();
        let lufs_reset_reader = lufs_reset.clone();
        let spectrum_writer = spectrum_buf.clone();
        let main_buffered_writer = main_buffered.clone();
        let sr = sample_rate;

        let thread = std::thread::Builder::new()
            .name("klipp-mix".into())
            .spawn(move || {
                crate::thread_guard::catch_panic("klipp-mix", || {
                    mix_loop(producer, cmd_rx, mixer, master_clock, sr, lufs_readings_writer, lufs_reset_reader, spectrum_writer, main_buffered_writer, mixed_frames_writer, tp_oversample);
                });
            })
            .expect("failed to spawn mix thread");

        (
            Self {
                thread: Some(thread),
                cmd_tx,
                lufs_readings,
                lufs_reset,
                spectrum_buf,
                spectrum_sample_rate: sample_rate,
                main_buffered,
                mixed_frames,
            },
            consumer,
        )
    }

    /// Number of samples currently sitting in the main ring buffer (the one
    /// cpal pulls from). The engine polls this after a seek to know when the
    /// pipeline has pre-filled enough to resume cpal without an underrun gap.
    pub fn buffered_samples(&self) -> usize {
        self.main_buffered.load(Ordering::Acquire)
    }

    /// Stereo frames mixed so far, the newest in the main ring: with
    /// [`Self::buffered_samples`] it says which mixed frame is heard when.
    pub fn mixed_frames(&self) -> u64 {
        self.mixed_frames.load(Ordering::Acquire)
    }

    /// Get a clone of the shared LUFS readings for the UI/engine to read.
    pub fn lufs_readings(&self) -> Arc<Mutex<LufsReadings>> {
        self.lufs_readings.clone()
    }

    /// Get a clone of the shared spectrum PCM ring for the UI FFT analyser.
    pub fn spectrum_buf(&self) -> Arc<Mutex<VecDeque<f32>>> {
        self.spectrum_buf.clone()
    }

    /// Sample rate the mix thread was started with — matches the data in
    /// `spectrum_buf()`.
    pub fn spectrum_sample_rate(&self) -> u32 {
        self.spectrum_sample_rate
    }

    /// Get a clone of the reset flag so the UI can trigger a meter reset.
    pub fn lufs_reset_flag(&self) -> Arc<AtomicBool> {
        self.lufs_reset.clone()
    }

    /// Register a staging consumer for a given track.
    /// `latency_advertise` is the producing decoder's
    /// [`crate::audio_decoder::AudioDecoder::latency_advertise`]
    /// atomic — polled per mix block to push real chain latency
    /// into AudioMixer's PDC state.
    pub fn add_staging(
        &self,
        track_index: usize,
        consumer: AudioConsumer,
        latency_advertise: Arc<AtomicU32>,
        mix: Arc<SourceMix>,
    ) {
        let _ = self.cmd_tx.send(MixCommand::AddStaging {
            track_index,
            consumer,
            latency_advertise,
            mix,
        });
    }

    /// Remove one track's staging consumer; the others play on.
    pub fn remove_staging(&self, track_index: usize) {
        let _ = self.cmd_tx.send(MixCommand::RemoveStaging { track_index });
    }

    /// Remove the one staging fed through `mix`, of a track that may
    /// have two across a cut.
    pub fn remove_staging_of(&self, mix: &Arc<SourceMix>) {
        let _ = self.cmd_tx.send(MixCommand::RemoveStagingOf { mix: mix.clone() });
    }

    /// Remove all staging consumers (e.g., on clip change or stop).
    pub fn clear_all(&self) {
        let _ = self.cmd_tx.send(MixCommand::ClearAll);
    }

    /// Flush all staging and main buffers. Blocks until the mix thread
    /// has drained everything, so no stale samples remain after a seek.
    pub fn flush(&self) {
        let ack = Arc::new(AtomicBool::new(false));
        let _ = self.cmd_tx.send(MixCommand::Flush { ack: ack.clone() });
        // Wait for the mix thread to process the flush (should be <1ms)
        let deadline = std::time::Instant::now() + Duration::from_millis(20);
        while !ack.load(Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                debug!("Mix thread flush timeout");
                break;
            }
            std::thread::sleep(Duration::from_micros(50));
        }
    }

    /// Stop the mix thread. Called when the engine shuts down.
    pub fn stop(&mut self) {
        let _ = self.cmd_tx.send(MixCommand::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for MixThread {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The mix thread loop — reads staging consumers, mixes, pushes to main.
#[allow(clippy::too_many_arguments)]
fn mix_loop(
    mut producer: AudioProducer,
    cmd_rx: Receiver<MixCommand>,
    mixer: Arc<Mutex<AudioMixer>>,
    master_clock: Arc<MasterClock>,
    sample_rate: u32,
    lufs_readings: Arc<Mutex<LufsReadings>>,
    lufs_reset: Arc<AtomicBool>,
    spectrum_buf: Arc<Mutex<VecDeque<f32>>>,
    main_buffered: Arc<AtomicUsize>,
    mixed_frames: Arc<AtomicU64>,
    tp_oversample: bool,
) {
    let mut mixed_so_far: u64 = 0;
    // 2 seconds of interleaved-stereo audio is plenty for any realistic FFT
    // window and leaves room for UI jitter. At 192 kHz that's ~1.5 MiB.
    let spectrum_max_samples = (sample_rate as usize) * 2 * 2;
    debug!("Mix thread started");

    // Flush-to-zero + denormals-are-zero on the SSE control register. At very
    // low sample magnitudes (silent input running through an EQ, reverb tail
    // decaying past audibility, …) biquad state can drift into IEEE 754
    // denormal range, where some x86 CPUs stall by ~100× per FLOP. FTZ+DAZ
    // forces those values to zero with no audible impact — the threshold is
    // ~10⁻³⁸, far below audio sample precision. This runs once per mix
    // thread start and affects only this thread's FPU control word.
    //
    // We set the bits directly in MXCSR via stmxcsr/ldmxcsr because the DAZ
    // intrinsic (`_MM_SET_DENORMALS_ZERO_MODE`) is not exposed in stable
    // `std::arch`, and the `_mm_getcsr` / `_mm_setcsr` wrappers are
    // deprecated in favour of inline asm. Bit 15 = FTZ, bit 6 = DAZ;
    // available since SSE3 on effectively every x86_64 CPU shipped in the
    // last ~20 years.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    unsafe {
        let mut mxcsr: u32 = 0;
        std::arch::asm!(
            "stmxcsr [{0}]",
            in(reg) &mut mxcsr,
            options(nostack, preserves_flags),
        );
        const FTZ_BIT: u32 = 1 << 15;
        const DAZ_BIT: u32 = 1 << 6;
        mxcsr |= FTZ_BIT | DAZ_BIT;
        std::arch::asm!(
            "ldmxcsr [{0}]",
            in(reg) &mxcsr,
            options(nostack, preserves_flags),
        );
    }

    struct StagingEntry {
        track_index: usize,
        consumer: AudioConsumer,
        /// Per-track chain latency polled each block; pushed into
        /// AudioMixer's PDC state when changed. Last-seen value is
        /// tracked separately so we only call into the locked
        /// mixer's set_track_latency_samples on actual change (the
        /// common no-plugins-on-this-track case is one atomic load
        /// + one == comparison per block).
        latency_advertise: Arc<AtomicU32>,
        latency_last_seen: u32,
        /// The clip settings of the source feeding this staging.
        mix: Arc<SourceMix>,
    }

    let mut lufs_meter = LufsMeter::new(sample_rate as f32, tp_oversample);
    let mut stagings: Vec<StagingEntry> = Vec::new();
    // Per-track temporary buffer for reading from staging
    let chunk_samples = MIX_CHUNK_SAMPLES;
    let mut drain_buf = vec![0.0f32; 8192];
    // Reused across loop iterations — the inner `Vec<f32>`s are resized to
    // `read_size` (never exceeds `chunk_samples`) before each pop_slice and
    // truncated to the actual filled length afterwards. Saves ~8 MB/s of
    // allocator traffic at 10 tracks × 200 Hz × 4 KB.
    let mut track_buffers: Vec<Vec<f32>> = Vec::new();

    loop {
        // Publish current main-ring fill so the engine's pre-fill polling
        // (replacing the old hardcoded 15 ms sleep on seek) sees the latest
        // state on every iteration regardless of which branch we take below.
        main_buffered.store(producer.occupied_len(), Ordering::Release);

        // Process commands
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                MixCommand::AddStaging {
                    track_index,
                    consumer,
                    latency_advertise,
                    mix,
                } => {
                    debug!("Mix thread: added staging for track {track_index}");
                    stagings.push(StagingEntry {
                        track_index,
                        consumer,
                        latency_advertise,
                        latency_last_seen: u32::MAX, // sentinel — first poll will always count as a "change"
                        mix,
                    });
                }
                MixCommand::ClearAll => {
                    debug!("Mix thread: cleared all stagings");
                    stagings.clear();
                }
                MixCommand::RemoveStaging { track_index } => {
                    debug!("Mix thread: removed staging for track {track_index}");
                    stagings.retain(|s| s.track_index != track_index);
                }
                MixCommand::RemoveStagingOf { mix } => {
                    stagings.retain(|s| !Arc::ptr_eq(&s.mix, &mix));
                }
                MixCommand::Flush { ack } => {
                    // Drain all staging consumers
                    for staging in &mut stagings {
                        loop {
                            let n = staging.consumer.pop_slice(&mut drain_buf);
                            if n == 0 {
                                break;
                            }
                        }
                    }
                    // Drain the main producer (discard what we already mixed)
                    // We can't drain a producer directly, but we can fill it with
                    // zeros that cpal will silently consume. Instead, just signal
                    // that we're done — AudioOutput::notify_seek() will drain main.
                    // Also reset PDC delay lines so stale samples from before the
                    // seek don't bleed across the boundary.
                    if let Ok(mut m) = mixer.lock() {
                        m.reset_pdc_delay_lines();
                    }
                    ack.store(true, Ordering::Release);
                    debug!("Mix thread: flushed all staging buffers");
                }
                MixCommand::Stop => {
                    debug!("Mix thread stopping");
                    return;
                }
            }
        }

        // If no stagings, sleep and retry
        if stagings.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        // Check how much room the main producer has
        let vacant = producer.vacant_len();
        if vacant < chunk_samples {
            // Main buffer is full, wait for cpal to consume
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }

        let read_size = chunk_samples.min(vacant);

        // Read from each staging consumer. Reuse `track_buffers` across
        // iterations — resize_with trims or extends to match the current
        // staging count (handles ClearAll shrinking stagings mid-stream).
        track_buffers.resize_with(stagings.len(), Vec::new);
        let mut any_data = false;

        for (i, staging) in stagings.iter_mut().enumerate() {
            let buf = &mut track_buffers[i];
            buf.resize(read_size, 0.0);
            // A source that starts later in this chunk, or after it, is
            // silent until its first sample's frame (#1588).
            let silence = staging.mix.silence_before(mixed_so_far, read_size);
            // And one that ends in it is heard up to its end, silent after.
            let heard = staging.mix.heard_until(mixed_so_far, read_size).max(silence);
            buf[..silence].fill(0.0);
            let filled = silence + staging.consumer.pop_slice(&mut buf[silence..heard]);
            if filled > 0 {
                any_data = true;
                staging.mix.apply(&mut buf[silence..filled]);
                if filled == heard && heard < read_size {
                    buf[heard..].fill(0.0);
                } else {
                    buf.truncate(filled);
                }
            } else {
                buf.clear();
            }
        }

        if !any_data {
            // No data from any staging — brief sleep
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // A track's second lane (the other clip of an audio crossfade) is
        // summed into its first before anything else: the mixer, the PDC
        // delay lines and the meters see one buffer per track, so the two
        // sides of a crossfade share the track's volume, pan, mute, solo
        // and latency compensation exactly.
        let mut lanes: Vec<(usize, usize)> = Vec::with_capacity(stagings.len());
        for (i, staging) in stagings.iter().enumerate() {
            let track = crate::engine::key_track(staging.track_index);
            match lanes.iter().find(|(t, _)| *t == track).map(|(_, first)| *first) {
                Some(first) => {
                    let (into, from) = if first < i {
                        let (a, b) = track_buffers.split_at_mut(i);
                        (&mut a[first], &b[0])
                    } else {
                        let (a, b) = track_buffers.split_at_mut(first);
                        (&mut b[0], &a[i])
                    };
                    if into.len() < from.len() {
                        into.resize(from.len(), 0.0);
                    }
                    for (o, x) in into.iter_mut().zip(from.iter()) {
                        *o += x;
                    }
                }
                None => lanes.push((track, i)),
            }
        }

        // Mix via AudioMixer. Single critical section folds PDC
        // updates + per-track delay application + the existing mix
        // routine. Holding the lock once across all three avoids
        // double-locking on the audio thread.
        let max_track_idx = lanes.iter().map(|(t, _)| *t).max().unwrap_or(0);
        let mixed = match mixer.lock() {
            Ok(mut m) => {
                m.ensure_tracks(max_track_idx + 1);

                // Push any newly-advertised per-track latencies into
                // AudioMixer's PDC state. set_track_latency_samples
                // only resizes delay lines when the global max
                // changes, so this is cheap when nothing changed.
                let mut any_change = false;
                for staging in stagings.iter_mut() {
                    let advertised = staging.latency_advertise.load(Ordering::Acquire);
                    if advertised != staging.latency_last_seen {
                        m.set_track_latency_samples(
                            crate::engine::key_track(staging.track_index),
                            advertised,
                            chunk_samples / 2, // chunk_samples is interleaved L+R
                        );
                        staging.latency_last_seen = advertised;
                        any_change = true;
                    }
                }

                // When per-track latencies change, the global max
                // may have changed too. Push it into MasterClock
                // so the engine subtracts it at video frame pick;
                // video stays aligned with audio. Cheap atomic
                // store; no contention with audio-thread reads.
                if any_change {
                    let global_max = m.pdc_global_max_latency();
                    let offset_us = MasterClock::pdc_offset_us_from_samples(
                        global_max,
                        sample_rate,
                    );
                    master_clock.set_pdc_offset_us(offset_us);
                }

                // Apply per-track PDC delay BEFORE mix. The delay
                // lines are a no-op when the track has zero
                // compensation needed (slot is None), so the common
                // no-plugins case has zero per-block work.
                for &(track, i) in &lanes {
                    m.apply_per_track_pdc_delay(track, &mut track_buffers[i]);
                }

                let refs: Vec<(usize, &[f32])> = lanes
                    .iter()
                    .map(|&(track, i)| (track, track_buffers[i].as_slice()))
                    .collect();
                m.mix(&refs)
            }
            Err(e) => {
                warn!("Mix thread: mutex poisoned: {e}");
                continue;
            }
        };

        // LUFS metering: process mixed output before pushing to ring buffer
        if !mixed.is_empty() {
            // Check reset flag
            if lufs_reset.swap(false, Ordering::Relaxed) {
                lufs_meter.reset();
            }
            lufs_meter.process(&mixed);
            // Publish readings (try_lock to avoid blocking the mix thread)
            if let Ok(mut r) = lufs_readings.try_lock() {
                *r = lufs_meter.readings();
            }
            // Feed the UI spectrum analyser. try_lock so the mix thread never
            // blocks on UI contention — a dropped chunk just means a slightly
            // older spectrum for one frame.
            if let Ok(mut ring) = spectrum_buf.try_lock() {
                ring.extend(mixed.iter().copied());
                while ring.len() > spectrum_max_samples {
                    ring.pop_front();
                }
            }
        }

        // Push mixed output to main ring buffer
        if !mixed.is_empty() {
            let pushed = producer.push_slice(&mixed);
            mixed_so_far += (mixed.len() / 2) as u64;
            mixed_frames.store(mixed_so_far, Ordering::Release);
            // With the count, so the two read together say which mixed
            // frame is heard when.
            main_buffered.store(producer.occupied_len(), Ordering::Release);
            if pushed < mixed.len() {
                debug!(
                    "Mix thread: main buffer overflow, dropped {} samples",
                    mixed.len() - pushed
                );
            }
        }
    }
}

#[cfg(test)]
mod source_mix_tests {
    use super::SourceMix;

    #[test]
    fn volume_gain_and_balance_then_channel_config() {
        let mix = SourceMix::default();
        let mut s = [0.5, 0.25, 0.5, 0.25];
        mix.apply(&mut s);
        assert_eq!(s, [0.5, 0.25, 0.5, 0.25], "the default leaves samples alone");

        mix.set(0.5, 0.5, 0);
        let mut s = [1.0, 1.0];
        mix.apply(&mut s);
        assert_eq!(s, [0.25, 0.5], "half volume, balance right halves the left");

        mix.set(1.0, -1.0, 0);
        let mut s = [1.0, 1.0];
        mix.apply(&mut s);
        assert_eq!(s, [1.0, 0.0], "full left silences the right");

        mix.set(1.0, 0.0, 1);
        let mut s = [1.0, 0.0];
        mix.apply(&mut s);
        assert_eq!(s, [0.5, 0.5], "mono downmix");

        // One side of a stereo clip split to mono (#922).
        mix.set(1.0, 0.0, 5);
        let mut s = [0.75, 0.25];
        mix.apply(&mut s);
        assert_eq!(s, [0.75, 0.75], "left only, on both sides");
        mix.set(1.0, 0.0, 6);
        let mut s = [0.75, 0.25];
        mix.apply(&mut s);
        assert_eq!(s, [0.25, 0.25], "right only, on both sides");

        mix.set(1.0, 0.0, 2);
        let mut s = [1.0, 0.0];
        mix.apply(&mut s);
        assert_eq!(s, [0.0, 1.0], "swap");
    }

    /// Through the mix thread: a clip at half volume on one track leaves
    /// the clip on another track at its own level, and the two lanes of
    /// a crossfade each keep their own. The whole output used to take the
    /// first clip's settings.
    #[test]
    fn each_source_keeps_its_own_clip_settings_in_the_mix() {
        use super::MixThread;
        use ringbuf::traits::{Consumer, Producer, Split};
        use std::sync::atomic::AtomicU32;
        use std::sync::{Arc, Mutex};

        let mixer = Arc::new(Mutex::new(klipp_audio::AudioMixer::new(2)));
        let (mt, mut main) = MixThread::new(48_000, mixer, Arc::new(crate::sync::MasterClock::new()), false);
        let n = 512;
        // Track 0: lane 0 at 0.5 and its crossfade lane at 0.25; track 1 at 1.0.
        let feeds = [(crate::engine::source_key(0, 0), 0.5f32), (crate::engine::source_key(0, 1), 0.25), (crate::engine::source_key(1, 0), 1.0)];
        for (key, volume) in feeds {
            let (mut producer, consumer) = ringbuf::HeapRb::<f32>::new(n * 4).split();
            assert_eq!(producer.push_slice(&vec![0.4f32; n]), n);
            let mix = Arc::new(super::SourceMix::default());
            mix.set(volume, 0.0, 0);
            mt.add_staging(key, consumer, Arc::new(AtomicU32::new(0)), mix);
        }
        // The sources may land in different blocks, so compare the total:
        // each is 0.4 over n samples at its own volume, times the track's
        // centre pan gain.
        let (centre, _) = klipp_audio::AudioMixer::new(1).pan_law.gains(0.0);
        let want = 0.4 * n as f32 * (0.5 + 0.25 + 1.0) * centre;
        let mut total = 0.0f32;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (total - want).abs() > 1e-2 && std::time::Instant::now() < deadline {
            let mut buf = vec![0.0f32; n];
            let got = main.pop_slice(&mut buf);
            total += buf[..got].iter().sum::<f32>();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!((total - want).abs() <= 1e-2, "mixed {total}, want {want}");
    }

    /// #1588: a sound opened ahead of its clip starts on its own sample
    /// in the mix, the sample the export starts it on. A clip placed at
    /// 4.15 s, opened while the mix is at 4.0 s, is silent for exactly
    /// the 0.15 s between, at 48 and at 44.1 kHz, as the export mixdown
    /// puts it 0.15 s of samples after 4.0 s.
    #[test]
    fn a_sound_opened_ahead_starts_on_its_sample() {
        use super::MixThread;
        use ringbuf::traits::{Consumer, Producer, Split};
        use std::sync::atomic::AtomicU32;
        use std::sync::{Arc, Mutex};

        for rate in [48_000u32, 44_100] {
            let mixer = Arc::new(Mutex::new(klipp_audio::AudioMixer::new(1)));
            let (mt, mut main) = MixThread::new(rate, mixer, Arc::new(crate::sync::MasterClock::new()), false);
            let start = crate::engine::start_frame_for(4.15, 4.0, mt.mixed_frames(), rate).expect("it starts later");
            // The export's sample for the same placement, counted from 4.0 s.
            let export_at = |secs: f64| (secs * rate as f64 + 1e-6).floor() as u64;
            assert_eq!(start, export_at(4.15) - export_at(4.0), "{rate}: the export's sample");
            let (mut producer, consumer) = ringbuf::HeapRb::<f32>::new(4096).split();
            assert_eq!(producer.push_slice(&[0.9f32; 512]), 512);
            let mix = Arc::new(super::SourceMix::default());
            mix.start_at(start);
            mt.add_staging(crate::engine::source_key(0, 0), consumer, Arc::new(AtomicU32::new(0)), mix);

            let mut heard: Vec<f32> = Vec::new();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while heard.len() < (start as usize + 256) * 2 && std::time::Instant::now() < deadline {
                let mut buf = vec![0.0f32; 4096];
                let got = main.pop_slice(&mut buf);
                heard.extend_from_slice(&buf[..got]);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let onset = heard.chunks_exact(2).position(|f| f[0] != 0.0).expect("the sound is heard");
            assert_eq!(onset as u64, start, "{rate}: heard from its own sample");
        }
    }

    /// #1632: at a cut on one track the outgoing sound ends on the cut's
    /// frame and the incoming starts on it: two sources on one track key,
    /// the one heard to the frame before, the other from it, neither
    /// both nor neither.
    #[test]
    fn a_cut_on_one_track_swaps_on_its_sample() {
        use super::MixThread;
        use ringbuf::traits::{Consumer, Producer, Split};
        use std::sync::atomic::AtomicU32;
        use std::sync::{Arc, Mutex};

        let cut = 3_001u64;
        let mixer = Arc::new(Mutex::new(klipp_audio::AudioMixer::new(1)));
        let (mt, mut main) = MixThread::new(48_000, mixer, Arc::new(crate::sync::MasterClock::new()), false);
        let key = crate::engine::source_key(0, 0);
        for (level, start, end) in [(0.25f32, None, Some(cut)), (0.75, Some(cut), None)] {
            let (mut producer, consumer) = ringbuf::HeapRb::<f32>::new(16_384).split();
            assert_eq!(producer.push_slice(&[level; 12_000]), 12_000);
            let mix = Arc::new(super::SourceMix::default());
            if let Some(f) = start {
                mix.start_at(f);
            }
            if let Some(f) = end {
                mix.end_at(f);
            }
            mt.add_staging(key, consumer, Arc::new(AtomicU32::new(0)), mix);
        }
        let mut heard: Vec<f32> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while heard.len() < (cut as usize + 1_000) * 2 && std::time::Instant::now() < deadline {
            let mut buf = vec![0.0f32; 4096];
            let got = main.pop_slice(&mut buf);
            heard.extend_from_slice(&buf[..got]);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let frames: Vec<f32> = heard.chunks_exact(2).map(|f| f[0]).collect();
        let (before, after) = (frames[cut as usize - 1], frames[cut as usize]);
        assert!(before > 0.0 && after > 0.0, "sound on both sides of the cut: {before} {after}");
        assert!((after / before - 3.0).abs() < 1e-3, "the outgoing to the frame before, the incoming from it: {before} then {after}");
        assert!(frames[..cut as usize].iter().all(|&f| (f - before).abs() < 1e-6), "the outgoing alone before the cut");
        assert!(frames[cut as usize..cut as usize + 1_000].iter().all(|&f| (f - after).abs() < 1e-6), "the incoming alone after it");
    }

    /// The mixed frame a clip is heard at, from where the mix is.
    #[test]
    fn start_frames_count_from_the_mix() {
        use crate::engine::start_frame_for;
        assert_eq!(start_frame_for(4.15, 4.0, 1000, 48_000), Some(1000 + 7200));
        assert_eq!(start_frame_for(4.0, 4.0, 1000, 48_000), None, "already started: opened under its clip");
        assert_eq!(start_frame_for(3.9, 4.0, 1000, 48_000), None);
    }
}
