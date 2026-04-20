use crate::audio_decoder::{AudioConsumer, AudioProducer};
use crossbeam_channel::{Receiver, Sender};
use klipp_audio::{AudioMixer, LufsMeter, LufsReadings};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, warn};

enum MixCommand {
    /// Add a staging consumer for a track.
    AddStaging {
        track_index: usize,
        consumer: AudioConsumer,
    },
    /// Clear all staging consumers (called on stop/clip change).
    ClearAll,
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
}

impl MixThread {
    /// Create the mix thread and the main output ring buffer.
    /// Returns `(MixThread, AudioConsumer)` — pass the consumer to `AudioOutput::new()`.
    pub fn new(
        sample_rate: u32,
        mixer: Arc<Mutex<AudioMixer>>,
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
        let lufs_readings_writer = lufs_readings.clone();
        let lufs_reset_reader = lufs_reset.clone();
        let spectrum_writer = spectrum_buf.clone();
        let sr = sample_rate;

        let thread = std::thread::Builder::new()
            .name("klipp-mix".into())
            .spawn(move || {
                crate::thread_guard::catch_panic("klipp-mix", || {
                    mix_loop(producer, cmd_rx, mixer, sr, lufs_readings_writer, lufs_reset_reader, spectrum_writer, tp_oversample);
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
            },
            consumer,
        )
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
    pub fn add_staging(&self, track_index: usize, consumer: AudioConsumer) {
        let _ = self.cmd_tx.send(MixCommand::AddStaging {
            track_index,
            consumer,
        });
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
fn mix_loop(
    mut producer: AudioProducer,
    cmd_rx: Receiver<MixCommand>,
    mixer: Arc<Mutex<AudioMixer>>,
    sample_rate: u32,
    lufs_readings: Arc<Mutex<LufsReadings>>,
    lufs_reset: Arc<AtomicBool>,
    spectrum_buf: Arc<Mutex<VecDeque<f32>>>,
    tp_oversample: bool,
) {
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
        #[allow(dead_code)]
        track_index: usize,
        consumer: AudioConsumer,
    }

    let mut lufs_meter = LufsMeter::new(sample_rate as f32, tp_oversample);
    let mut stagings: Vec<StagingEntry> = Vec::new();
    // Per-track temporary buffer for reading from staging
    let chunk_samples = 1024; // 512 stereo frames ≈ 10.7ms at 48kHz
    let mut drain_buf = vec![0.0f32; 8192];
    // Reused across loop iterations — the inner `Vec<f32>`s are resized to
    // `read_size` (never exceeds `chunk_samples`) before each pop_slice and
    // truncated to the actual filled length afterwards. Saves ~8 MB/s of
    // allocator traffic at 10 tracks × 200 Hz × 4 KB.
    let mut track_buffers: Vec<Vec<f32>> = Vec::new();

    loop {
        // Process commands
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                MixCommand::AddStaging {
                    track_index,
                    consumer,
                } => {
                    debug!("Mix thread: added staging for track {track_index}");
                    stagings.push(StagingEntry {
                        track_index,
                        consumer,
                    });
                }
                MixCommand::ClearAll => {
                    debug!("Mix thread: cleared all stagings");
                    stagings.clear();
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
            let filled = staging.consumer.pop_slice(buf);
            if filled > 0 {
                any_data = true;
                buf.truncate(filled);
            } else {
                buf.clear();
            }
        }

        if !any_data {
            // No data from any staging — brief sleep
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // Mix via AudioMixer — pass (sequence_track_index, samples) pairs
        let refs: Vec<(usize, &[f32])> = stagings.iter().zip(track_buffers.iter())
            .map(|(s, b)| (s.track_index, b.as_slice()))
            .collect();
        let max_track_idx = stagings.iter().map(|s| s.track_index).max().unwrap_or(0);
        let mixed = match mixer.lock() {
            Ok(mut m) => {
                m.ensure_tracks(max_track_idx + 1);
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
            if pushed < mixed.len() {
                debug!(
                    "Mix thread: main buffer overflow, dropped {} samples",
                    mixed.len() - pushed
                );
            }
        }
    }
}
