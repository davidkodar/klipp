use crossbeam_channel::{Receiver, Sender};
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
}

impl AudioDecoder {
    /// Open a file's audio stream for decoding.
    /// Returns the decoder and a consumer for reading samples.
    /// `output_rate` is the cpal device's sample rate.
    pub fn open(path: &Path, output_rate: u32) -> Result<(Self, AudioConsumer), AudioDecoderError> {
        // Validate that file has an audio stream
        {
            let ctx = ffmpeg_next::format::input(path)
                .map_err(|e| AudioDecoderError::Open(e.to_string()))?;
            ctx.streams()
                .best(ffmpeg_next::media::Type::Audio)
                .ok_or(AudioDecoderError::NoAudioStream)?;
        }

        // Ring buffer: ~0.5s of stereo f32 audio at output_rate
        let buf_size = (output_rate as usize) * 2; // stereo = 2 channels
        let (producer, consumer) = HeapRb::<f32>::new(buf_size).split();

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let volume = Arc::new(AtomicU32::new(100)); // 100 = 1.0
        let muted = Arc::new(AtomicBool::new(false));
        let post_seek_pts_us = Arc::new(AtomicU64::new(0));
        let seek_generation = Arc::new(AtomicU64::new(0));

        let vol = volume.clone();
        let mute = muted.clone();
        let pts_clone = post_seek_pts_us.clone();
        let gen_clone = seek_generation.clone();
        let path_owned = path.to_path_buf();

        let thread = std::thread::Builder::new()
            .name("klipp-adec".into())
            .spawn(move || {
                audio_decode_thread(path_owned, output_rate, producer, cmd_rx, vol, mute, pts_clone, gen_clone);
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
            },
            consumer,
        ))
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::SeekTo(secs));
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

fn audio_decode_thread(
    path: PathBuf,
    output_rate: u32,
    mut producer: AudioProducer,
    cmd_rx: Receiver<DecodeCommand>,
    volume: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
    post_seek_pts_us: Arc<AtomicU64>,
    seek_generation: Arc<AtomicU64>,
) {
    let mut format_ctx = match ffmpeg_next::format::input(&path) {
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
    let src_layout = decoder.channel_layout();
    let src_rate = decoder.rate();

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

    debug!(
        "audio_decode_thread: started, stream={stream_index}, src={src_rate}Hz {:?} → {output_rate}Hz stereo f32",
        src_format
    );

    let mut paused = true;
    let mut eof = false;
    let mut seek_target: Option<f64> = None;
    let mut packets = format_ctx.packets();
    let mut eof_from_demuxer = false;

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DecodeCommand::SeekTo(secs) => {
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

        // Throttle if ring buffer is mostly full
        if producer.vacant_len() < 4096 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // Read next packet (persistent iterator)
        if !eof_from_demuxer {
            match packets.next() {
                Some((stream, packet)) => {
                    if stream.index() != stream_index {
                        continue;
                    }
                    let _ = decoder.send_packet(&packet);
                }
                None => {
                    let _ = decoder.send_eof();
                    eof_from_demuxer = true;
                }
            }
        } else {
            eof = true;
            continue;
        }

        // Drain decoded frames
        let mut frame = ffmpeg_next::frame::Audio::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
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
                // Report actual PTS to engine for precise clock sync
                post_seek_pts_us.store((pts_secs * 1_000_000.0) as u64, Ordering::Release);
                seek_generation.fetch_add(1, Ordering::Release);
                seek_target = None;
            }

            let mut resampled = ffmpeg_next::frame::Audio::empty();
            if resampler.run(&frame, &mut resampled).is_err() {
                continue;
            }

            let data = resampled.data(0);
            if data.is_empty() {
                continue;
            }

            let floats: &[f32] = bytemuck::cast_slice(data);

            let vol = volume.load(Ordering::Relaxed) as f32 / 100.0;
            let is_muted = muted.load(Ordering::Relaxed);

            if is_muted {
                // Push silence (advance the position without audio)
                let silence = vec![0.0f32; floats.len()];
                producer.push_slice(&silence);
            } else if (vol - 1.0).abs() < 0.01 {
                // Unity gain — push directly
                producer.push_slice(floats);
            } else {
                // Apply volume scaling
                let scaled: Vec<f32> = floats.iter().map(|s| s * vol).collect();
                producer.push_slice(&scaled);
            }
        }
    }
}
