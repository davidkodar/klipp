use klipp_core::MediaId;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Metadata for a conformed audio cache file (raw PCM).
#[derive(Debug, Clone)]
pub struct AudioCacheEntry {
    pub media_id: MediaId,
    /// Path to the raw PCM file (48kHz, stereo, f32 interleaved).
    pub pcm_path: PathBuf,
    /// Path to the waveform peaks file.
    pub peaks_path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_secs: f64,
    /// Waveform peaks: (min, max) per ~256 samples for timeline display.
    pub peaks: Vec<(f32, f32)>,
}

/// Errors from audio conforming.
#[derive(Debug)]
pub enum ConformError {
    NoAudioStream,
    FfmpegOpen(String),
    CodecError(String),
    ResamplerError(String),
    IoError(std::io::Error),
}

impl std::fmt::Display for ConformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAudioStream => write!(f, "no audio stream"),
            Self::FfmpegOpen(e) => write!(f, "ffmpeg open: {e}"),
            Self::CodecError(e) => write!(f, "codec: {e}"),
            Self::ResamplerError(e) => write!(f, "resampler: {e}"),
            Self::IoError(e) => write!(f, "io: {e}"),
        }
    }
}

impl From<std::io::Error> for ConformError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e)
    }
}

const CONFORM_SAMPLE_RATE: u32 = 48000;
const CONFORM_CHANNELS: u16 = 2;
/// Number of mono samples per peak bin for waveform display.
const PEAK_BIN_SIZE: usize = 256;

/// Ensure the audio cache directory exists and return it.
pub fn cache_dir_for_project(project_dir: &Path) -> PathBuf {
    let dir = project_dir.join(".klipp-cache").join("audio");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!("Failed to create audio cache dir {:?}: {e}", dir);
    }
    dir
}

/// Check if a valid PCM cache already exists for this media.
pub fn cache_exists(cache_dir: &Path, media_id: MediaId) -> bool {
    let pcm_path = cache_dir.join(format!("{}.pcm", media_id));
    let peaks_path = cache_dir.join(format!("{}.peaks", media_id));
    pcm_path.exists() && peaks_path.exists()
}

/// Decode the audio from `media_path` into a raw PCM file (48kHz stereo f32)
/// and generate waveform peak data. This is CPU-intensive and should be run
/// on a background thread.
///
/// The PCM file is written to `<cache_dir>/<media_id>.pcm`.
/// The peaks file is written to `<cache_dir>/<media_id>.peaks`.
pub fn conform_audio(
    media_id: MediaId,
    media_path: &Path,
    cache_dir: &Path,
) -> Result<AudioCacheEntry, ConformError> {
    let pcm_path = cache_dir.join(format!("{}.pcm", media_id));
    let peaks_path = cache_dir.join(format!("{}.peaks", media_id));
    conform_audio_to_paths(media_id, media_path, &pcm_path, &peaks_path)
}

/// Like `conform_audio` but writes to explicit pcm/peaks paths.
/// Used by content-based caching to place files at content-keyed paths.
pub fn conform_audio_to_paths(
    media_id: MediaId,
    media_path: &Path,
    pcm_path: &Path,
    peaks_path: &Path,
) -> Result<AudioCacheEntry, ConformError> {
    let start = std::time::Instant::now();

    // Open input
    let mut format_ctx = ffmpeg_next::format::input(media_path)
        .map_err(|e| ConformError::FfmpegOpen(e.to_string()))?;

    let audio_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .ok_or(ConformError::NoAudioStream)?;

    let stream_index = audio_stream.index();
    let params = audio_stream.parameters();

    let codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| ConformError::CodecError(e.to_string()))?;

    let mut decoder = codec_ctx
        .decoder()
        .audio()
        .map_err(|e| ConformError::CodecError(e.to_string()))?;

    let src_format = decoder.format();
    let src_layout = decoder.channel_layout();
    let src_rate = decoder.rate();

    debug!(
        "conform_audio: {:?} — src: {}Hz {:?}, dst: {}Hz stereo f32",
        media_path.file_name().unwrap_or_default(),
        src_rate,
        src_format,
        CONFORM_SAMPLE_RATE,
    );

    let mut resampler = ffmpeg_next::software::resampling::Context::get(
        src_format,
        src_layout,
        src_rate,
        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
        ffmpeg_next::ChannelLayout::STEREO,
        CONFORM_SAMPLE_RATE,
    )
    .map_err(|e| ConformError::ResamplerError(e.to_string()))?;

    // Accumulate all PCM samples in memory, then write to disk.
    // For a typical 10-min clip at 48kHz stereo f32: ~220 MB — acceptable for
    // a background conform task. For very long media we could stream to disk
    // but that adds complexity for minimal gain.
    let mut all_samples: Vec<f32> = Vec::new();
    let mut frame = ffmpeg_next::frame::Audio::empty();

    // Two-phase decode loop (same pattern as audio_decoder.rs)
    let mut packets = format_ctx.packets();
    let mut eof_from_demuxer = false;
    let mut pending_packet: Option<ffmpeg_next::Packet> = None;

    loop {
        // Phase 1: Drain all decoded frames
        let mut drained_any = false;
        loop {
            if decoder.receive_frame(&mut frame).is_err() {
                break;
            }
            drained_any = true;

            let mut resampled = ffmpeg_next::frame::Audio::empty();
            if resampler.run(&frame, &mut resampled).is_err() {
                continue;
            }

            let nb_samples = resampled.samples();
            let channels = CONFORM_CHANNELS as usize;
            let actual_floats = nb_samples * channels;
            let data = resampled.data(0);
            if actual_floats == 0 || data.len() < actual_floats * 4 {
                continue;
            }

            let floats: &[f32] = bytemuck::cast_slice(&data[..actual_floats * 4]);
            all_samples.extend_from_slice(floats);
        }

        // Phase 2: Feed next packet
        if let Some(pkt) = pending_packet.take() {
            match decoder.send_packet(&pkt) {
                Ok(()) => {}
                Err(_) => {
                    pending_packet = Some(pkt);
                    // Decoder full — drain again next iteration
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
                }
                None => {
                    let _ = decoder.send_eof();
                    eof_from_demuxer = true;
                }
            }
        } else if !drained_any {
            break; // EOF and nothing left to drain
        }
    }

    let total_mono_samples = all_samples.len() / CONFORM_CHANNELS as usize;
    let duration_secs =
        total_mono_samples as f64 / CONFORM_SAMPLE_RATE as f64;

    // Write raw PCM to disk
    let pcm_bytes: &[u8] = bytemuck::cast_slice(&all_samples);
    std::fs::write(pcm_path, pcm_bytes)?;

    // Generate waveform peaks (min/max per PEAK_BIN_SIZE mono samples)
    let peaks = generate_peaks(&all_samples, CONFORM_CHANNELS as usize, PEAK_BIN_SIZE);

    // Serialize peaks as raw f32 pairs
    let peaks_flat: Vec<f32> = peaks.iter().flat_map(|&(min, max)| [min, max]).collect();
    let peaks_bytes: &[u8] = bytemuck::cast_slice(&peaks_flat);
    std::fs::write(peaks_path, peaks_bytes)?;

    let elapsed = start.elapsed();
    let pcm_size_mb = pcm_bytes.len() as f64 / (1024.0 * 1024.0);
    info!(
        "conform_audio: {:?} — {:.1}s duration, {:.1} MB PCM, {} peak bins, took {:.1}s",
        media_path.file_name().unwrap_or_default(),
        duration_secs,
        pcm_size_mb,
        peaks.len(),
        elapsed.as_secs_f64(),
    );

    Ok(AudioCacheEntry {
        media_id,
        pcm_path: pcm_path.to_path_buf(),
        peaks_path: peaks_path.to_path_buf(),
        sample_rate: CONFORM_SAMPLE_RATE,
        channels: CONFORM_CHANNELS,
        duration_secs,
        peaks,
    })
}

/// Generate waveform peaks: (min, max) per `bin_size` mono samples.
fn generate_peaks(
    samples: &[f32],
    channels: usize,
    bin_size: usize,
) -> Vec<(f32, f32)> {
    let mono_len = samples.len() / channels.max(1);
    let num_bins = mono_len.div_ceil(bin_size);
    let mut peaks = Vec::with_capacity(num_bins);

    for bin in 0..num_bins {
        let start = bin * bin_size;
        let end = ((bin + 1) * bin_size).min(mono_len);
        let mut min = 0.0f32;
        let mut max = 0.0f32;
        for i in start..end {
            let s = samples[i * channels]; // left channel
            min = min.min(s);
            max = max.max(s);
        }
        peaks.push((min, max));
    }

    peaks
}

/// Load peaks from a previously written .peaks file.
pub fn load_peaks(peaks_path: &Path) -> Result<Vec<(f32, f32)>, std::io::Error> {
    let bytes = std::fs::read(peaks_path)?;
    if bytes.len() % 8 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "peaks file size not aligned to f32 pairs",
        ));
    }
    let floats: &[f32] = bytemuck::cast_slice(&bytes);
    let peaks: Vec<(f32, f32)> = floats.chunks_exact(2).map(|c| (c[0], c[1])).collect();
    Ok(peaks)
}
