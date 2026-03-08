use ringbuf::traits::{Observer, Producer, Split};
use ringbuf::HeapRb;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, info};

use crate::audio_decoder::{AudioConsumer, AudioProducer};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: usize = 2;

/// Commands for the PCM feeder thread.
enum PcmCommand {
    SeekTo(f64),
    Pause,
    Resume,
    Stop,
}

/// A PCM audio feeder that reads from a pre-conformed .pcm cache file.
/// Replaces AudioDecoder for clips that have been conformed to raw PCM.
/// Seeks are O(1) — just a byte offset calculation.
pub struct PcmAudioFeeder {
    thread: Option<JoinHandle<()>>,
    cmd_tx: crossbeam_channel::Sender<PcmCommand>,
    pub volume: Arc<AtomicU32>,
    pub muted: Arc<AtomicBool>,
    pub post_seek_pts_us: Arc<AtomicU64>,
    pub seek_generation: Arc<AtomicU64>,
}

impl PcmAudioFeeder {
    /// Open a PCM cache file and create a feeder + ring buffer consumer.
    /// The PCM file must be 48kHz stereo f32 interleaved (as produced by conform_audio).
    pub fn open(pcm_path: &Path, output_rate: u32) -> Result<(Self, AudioConsumer), PcmFeederError> {
        let pcm_data = std::fs::read(pcm_path)
            .map_err(|e| PcmFeederError::Open(format!("read {:?}: {e}", pcm_path)))?;

        if pcm_data.len() % 4 != 0 {
            return Err(PcmFeederError::Open("PCM file size not aligned to f32".into()));
        }

        let total_floats = pcm_data.len() / 4;
        let total_mono_samples = total_floats / CHANNELS;
        let duration_secs = total_mono_samples as f64 / SAMPLE_RATE as f64;

        info!(
            "PcmAudioFeeder: opened {:?}, {:.1}s, {} samples",
            pcm_path, duration_secs, total_mono_samples,
        );

        // Ring buffer: ~2s of stereo f32 at output_rate
        let buf_size = (output_rate as usize) * 2 * 2;
        let (producer, consumer) = HeapRb::<f32>::new(buf_size).split();

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let volume = Arc::new(AtomicU32::new(100));
        let muted = Arc::new(AtomicBool::new(false));
        let post_seek_pts_us = Arc::new(AtomicU64::new(0));
        let seek_generation = Arc::new(AtomicU64::new(0));

        let vol = volume.clone();
        let mute = muted.clone();
        let pts_clone = post_seek_pts_us.clone();
        let gen_clone = seek_generation.clone();

        let needs_resample = output_rate != SAMPLE_RATE;

        let thread = std::thread::Builder::new()
            .name("klipp-pcm-feed".into())
            .spawn(move || {
                pcm_feed_thread(
                    pcm_data,
                    output_rate,
                    needs_resample,
                    producer,
                    cmd_rx,
                    vol,
                    mute,
                    pts_clone,
                    gen_clone,
                );
            })
            .map_err(|e| PcmFeederError::Open(format!("spawn thread: {e}")))?;

        Ok((
            Self {
                thread: Some(thread),
                cmd_tx,
                volume,
                muted,
                post_seek_pts_us,
                seek_generation,
            },
            consumer,
        ))
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.cmd_tx.send(PcmCommand::SeekTo(secs));
    }

    pub fn resume(&self) {
        let _ = self.cmd_tx.send(PcmCommand::Resume);
    }

    pub fn pause(&self) {
        let _ = self.cmd_tx.send(PcmCommand::Pause);
    }

    pub fn stop(&mut self) {
        let _ = self.cmd_tx.send(PcmCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    pub fn stop_detached(&mut self) {
        let _ = self.cmd_tx.send(PcmCommand::Stop);
        self.thread.take();
    }
}

impl Drop for PcmAudioFeeder {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
pub enum PcmFeederError {
    Open(String),
}

impl std::fmt::Display for PcmFeederError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "pcm feeder open: {e}"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn pcm_feed_thread(
    pcm_data: Vec<u8>,
    output_rate: u32,
    needs_resample: bool,
    mut producer: AudioProducer,
    cmd_rx: crossbeam_channel::Receiver<PcmCommand>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    post_seek_pts_us: Arc<AtomicU64>,
    seek_generation: Arc<AtomicU64>,
) {
    // Interpret raw bytes as f32 samples
    let samples: &[f32] = bytemuck::cast_slice(&pcm_data);
    let total_floats = samples.len();

    let mut paused = true;
    let mut eof = false;
    // Current read position in the samples array (in floats, i.e., sample_index * CHANNELS)
    let mut read_pos: usize = 0;

    // For resampling: if output_rate != 48000, we need to adjust the stride.
    // Simple approach: nearest-neighbor resampling (no interpolation).
    // For most systems output_rate == 48000, so this is a rare path.
    let rate_ratio = SAMPLE_RATE as f64 / output_rate as f64;

    // How many output stereo frames to produce per iteration (target ~10ms chunks)
    let chunk_frames = (output_rate as usize / 100).max(64);
    let chunk_floats = chunk_frames * CHANNELS;

    loop {
        // Process commands
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                PcmCommand::SeekTo(secs) => {
                    let sample_index = (secs * SAMPLE_RATE as f64) as usize;
                    read_pos = (sample_index * CHANNELS).min(total_floats);
                    eof = false;

                    // Report seek position
                    let actual_secs = (read_pos / CHANNELS) as f64 / SAMPLE_RATE as f64;
                    post_seek_pts_us.store((actual_secs * 1_000_000.0) as u64, Ordering::Release);
                    seek_generation.fetch_add(1, Ordering::Release);

                    debug!("pcm_feed: seek to {secs:.3}s → pos={read_pos} ({actual_secs:.3}s)");
                }
                PcmCommand::Pause => paused = true,
                PcmCommand::Resume => {
                    paused = false;
                    eof = false;
                }
                PcmCommand::Stop => {
                    debug!("pcm_feed: stop");
                    return;
                }
            }
        }

        if paused || eof {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // Check ring buffer space
        let vacant = producer.vacant_len();
        if vacant < chunk_floats {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }

        let vol = volume.load(Ordering::Relaxed) as f32 / 100.0;
        let is_muted = muted.load(Ordering::Relaxed);

        if read_pos >= total_floats {
            eof = true;
            continue;
        }

        if !needs_resample {
            // Fast path: PCM is already at output_rate (48kHz == 48kHz)
            let remaining = total_floats - read_pos;
            let to_push = chunk_floats.min(remaining).min(vacant);

            if is_muted {
                let zeros = vec![0.0f32; to_push];
                producer.push_slice(&zeros);
            } else if (vol - 1.0).abs() < 0.01 {
                producer.push_slice(&samples[read_pos..read_pos + to_push]);
            } else {
                let scaled: Vec<f32> = samples[read_pos..read_pos + to_push]
                    .iter()
                    .map(|s| s * vol)
                    .collect();
                producer.push_slice(&scaled);
            }
            read_pos += to_push;
        } else {
            // Slow path: nearest-neighbor resample from 48kHz to output_rate
            let frames_to_produce = chunk_frames.min(vacant / CHANNELS);
            let mut buf = Vec::with_capacity(frames_to_produce * CHANNELS);

            for i in 0..frames_to_produce {
                let src_frame = ((read_pos / CHANNELS) as f64 + i as f64 * rate_ratio) as usize;
                let src_idx = src_frame * CHANNELS;
                if src_idx + CHANNELS > total_floats {
                    eof = true;
                    break;
                }
                for ch in 0..CHANNELS {
                    let s = samples[src_idx + ch];
                    if is_muted {
                        buf.push(0.0);
                    } else {
                        buf.push(s * vol);
                    }
                }
            }

            if !buf.is_empty() {
                producer.push_slice(&buf);
                // Advance read_pos by the source frames consumed
                let source_frames_consumed = (buf.len() / CHANNELS) as f64 * rate_ratio;
                read_pos += (source_frames_consumed as usize) * CHANNELS;
                read_pos = read_pos.min(total_floats);
            }
        }
    }
}
