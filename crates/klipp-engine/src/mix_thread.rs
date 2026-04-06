use crate::audio_decoder::{AudioConsumer, AudioProducer};
use crossbeam_channel::{Receiver, Sender};
use klipp_audio::{AudioMixer, LufsMeter, LufsReadings};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
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
}

impl MixThread {
    /// Create the mix thread and the main output ring buffer.
    /// Returns `(MixThread, AudioConsumer)` — pass the consumer to `AudioOutput::new()`.
    pub fn new(sample_rate: u32, mixer: Arc<Mutex<AudioMixer>>) -> (Self, AudioConsumer) {
        // Main ring buffer: ~200ms stereo f32 at device sample rate
        // Small buffer = low latency for volume/pan/mute changes
        let buf_size = (sample_rate as usize) * 2 / 5;
        let (producer, consumer) = HeapRb::<f32>::new(buf_size).split();

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();

        let lufs_readings = Arc::new(Mutex::new(LufsReadings::default()));
        let lufs_reset = Arc::new(AtomicBool::new(false));
        let lufs_readings_writer = lufs_readings.clone();
        let lufs_reset_reader = lufs_reset.clone();
        let sr = sample_rate;

        let thread = std::thread::Builder::new()
            .name("klipp-mix".into())
            .spawn(move || mix_loop(producer, cmd_rx, mixer, sr, lufs_readings_writer, lufs_reset_reader))
            .expect("failed to spawn mix thread");

        (
            Self {
                thread: Some(thread),
                cmd_tx,
                lufs_readings,
                lufs_reset,
            },
            consumer,
        )
    }

    /// Get a clone of the shared LUFS readings for the UI/engine to read.
    pub fn lufs_readings(&self) -> Arc<Mutex<LufsReadings>> {
        self.lufs_readings.clone()
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
) {
    debug!("Mix thread started");

    struct StagingEntry {
        #[allow(dead_code)]
        track_index: usize,
        consumer: AudioConsumer,
    }

    let mut lufs_meter = LufsMeter::new(sample_rate as f32);
    let mut stagings: Vec<StagingEntry> = Vec::new();
    // Per-track temporary buffer for reading from staging
    let chunk_samples = 1024; // 512 stereo frames ≈ 10.7ms at 48kHz
    let mut drain_buf = vec![0.0f32; 8192];

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

        // Read from each staging consumer
        let mut track_buffers: Vec<Vec<f32>> = Vec::with_capacity(stagings.len());
        let mut any_data = false;

        for staging in &mut stagings {
            let mut buf = vec![0.0f32; read_size];
            let filled = staging.consumer.pop_slice(&mut buf);
            if filled > 0 {
                any_data = true;
                buf.truncate(filled);
            } else {
                buf.clear();
            }
            track_buffers.push(buf);
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
