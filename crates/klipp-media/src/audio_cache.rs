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
    /// The same peaks for each channel, left and right, for a track that
    /// shows its channels apart (#922). `None` for a cache made before
    /// they were written, until the media is conformed again.
    pub channel_peaks: Option<[Vec<(f32, f32)>; 2]>,
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
    let mut format_ctx = crate::open_input(media_path)
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
    let src_rate = decoder.rate();
    // Derive layout from channel count, not from `decoder.channel_layout()`
    // — see the long comment in `klipp_engine::audio_decoder` for why.
    // Short version: PCM WAVs frequently have unknown channel_layout in
    // the container, which makes the resampler reject every frame with
    // "Input changed".
    let src_channels = decoder.channels();
    let src_layout = ffmpeg_next::ChannelLayout::default(src_channels as i32);

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

            // Force the frame's channel_layout to match what the resampler
            // was built with. The resampler rejects frames whose layout
            // differs from the one it was configured for — including the
            // subtle 5.1-vs-5.1(side) case that AC-3 audio typically
            // decodes to while `ChannelLayout::default(6)` gives plain
            // 5.1. Previously this branch only patched *empty* layouts;
            // non-empty-but-mismatched layouts were silently rejected and
            // every frame dropped, producing a 0-byte PCM cache and a
            // silent clip. Overriding is safe: src_layout was derived from
            // the same channel count as the frame, so we're just
            // normalising the layout-order enum, not remapping channels.
            frame.set_channel_layout(src_layout);
            // Pre-allocate the output buffer at the rate-scaled worst-case
            // size. ffmpeg-next's `Context::run` would otherwise allocate
            // it at `input.samples()` count, which silently truncates the
            // output for any upsampling case. See the same comment in
            // `klipp_engine::audio_decoder` for the full story.
            let out_samples =
                frame.samples() * CONFORM_SAMPLE_RATE as usize / src_rate.max(1) as usize + 256;
            let mut resampled = ffmpeg_next::frame::Audio::new(
                ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
                out_samples,
                ffmpeg_next::ChannelLayout::STEREO,
            );
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

    // Write raw PCM to disk atomically (write to .tmp, then rename). This
    // prevents the ghost-file failure mode where the conform thread crashes
    // after `fs::File::create` but before the write finishes, leaving a
    // zero-byte file on disk that the next cache-hit check would happily
    // load as empty audio. If we crash mid-write, only the .tmp exists and
    // the next conform call starts from a clean slate.
    let pcm_bytes: &[u8] = bytemuck::cast_slice(&all_samples);
    let pcm_tmp = pcm_path.with_extension("pcm.tmp");
    std::fs::write(&pcm_tmp, pcm_bytes)?;
    std::fs::rename(&pcm_tmp, pcm_path)?;

    // Generate waveform peaks (min/max per PEAK_BIN_SIZE mono samples)
    let peaks = generate_peaks(&all_samples, CONFORM_CHANNELS as usize, PEAK_BIN_SIZE);

    // Serialize peaks as raw f32 pairs (atomic write — see PCM above).
    let peaks_flat: Vec<f32> = peaks.iter().flat_map(|&(min, max)| [min, max]).collect();
    let peaks_bytes: &[u8] = bytemuck::cast_slice(&peaks_flat);
    let peaks_tmp = peaks_path.with_extension("peaks.tmp");
    std::fs::write(&peaks_tmp, peaks_bytes)?;
    std::fs::rename(&peaks_tmp, peaks_path)?;

    // Each channel's peaks too, beside them (#922): left and right
    // interleaved per bin.
    let channel_peaks = generate_channel_peaks(&all_samples, CONFORM_CHANNELS as usize, PEAK_BIN_SIZE);
    let flat: Vec<f32> = channel_peaks[0].iter().zip(&channel_peaks[1]).flat_map(|(&(a, b), &(c, d))| [a, b, c, d]).collect();
    let channel_path = channel_peaks_path(peaks_path);
    let channel_tmp = channel_path.with_extension("peaks2.tmp");
    std::fs::write(&channel_tmp, bytemuck::cast_slice::<f32, u8>(&flat))?;
    std::fs::rename(&channel_tmp, &channel_path)?;

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
        channel_peaks: Some(channel_peaks),
    })
}

/// Where a peaks file's per-channel peaks are kept (#922).
pub fn channel_peaks_path(peaks_path: &Path) -> PathBuf {
    peaks_path.with_extension("peaks2")
}

/// Each channel's (min, max) per `bin_size` frames: the first channel and
/// the second, or the first twice for mono.
fn generate_channel_peaks(samples: &[f32], channels: usize, bin_size: usize) -> [Vec<(f32, f32)>; 2] {
    let channels = channels.max(1);
    let frames = samples.len() / channels;
    let bins = frames.div_ceil(bin_size.max(1));
    let peaks_of = |channel: usize| -> Vec<(f32, f32)> {
        (0..bins)
            .map(|bin| {
                let (start, end) = (bin * bin_size, ((bin + 1) * bin_size).min(frames));
                (start..end).map(|i| samples[i * channels + channel]).fold((0.0f32, 0.0f32), |(lo, hi), s| (lo.min(s), hi.max(s)))
            })
            .collect()
    };
    [peaks_of(0), peaks_of(1.min(channels - 1))]
}

/// Load per-channel peaks written by a conform (#922).
pub fn load_channel_peaks(path: &Path) -> Result<[Vec<(f32, f32)>; 2], std::io::Error> {
    let bytes = std::fs::read(path)?;
    if bytes.len() % 16 != 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "channel peaks file size not aligned to bins"));
    }
    let mut floats = vec![0f32; bytes.len() / 4];
    bytemuck::cast_slice_mut::<f32, u8>(&mut floats).copy_from_slice(&bytes);
    let left = floats.chunks_exact(4).map(|c| (c[0], c[1])).collect();
    let right = floats.chunks_exact(4).map(|c| (c[2], c[3])).collect();
    Ok([left, right])
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
    // `Vec<u8>` from `std::fs::read` is only 1-byte aligned; bytemuck panics
    // on `cast_slice` when the underlying pointer isn't aligned to the
    // target type's alignment (4 bytes for f32). Allocate a `Vec<f32>`
    // (guaranteed 4-byte aligned) and copy via the reverse cast.
    let total_floats = bytes.len() / 4;
    let mut floats = vec![0f32; total_floats];
    bytemuck::cast_slice_mut::<f32, u8>(&mut floats).copy_from_slice(&bytes);
    let peaks: Vec<(f32, f32)> = floats.chunks_exact(2).map(|c| (c[0], c[1])).collect();
    Ok(peaks)
}

#[cfg(test)]
mod channel_peaks_tests {
    use super::{generate_channel_peaks, load_channel_peaks};

    #[test]
    fn each_channel_has_its_own_peaks_and_they_round_trip() {
        // Left loud and positive, right quiet and negative, two bins of two.
        let samples = [0.9, -0.1, 0.5, -0.2, 0.3, -0.05, 0.8, -0.3];
        let peaks = generate_channel_peaks(&samples, 2, 2);
        assert_eq!(peaks[0], vec![(0.0, 0.9), (0.0, 0.8)]);
        assert_eq!(peaks[1], vec![(-0.2, 0.0), (-0.3, 0.0)]);
        let path = std::env::temp_dir().join(format!("klipp_channel_peaks_{}.peaks2", std::process::id()));
        let flat: Vec<f32> = peaks[0].iter().zip(&peaks[1]).flat_map(|(&(a, b), &(c, d))| [a, b, c, d]).collect();
        std::fs::write(&path, bytemuck::cast_slice::<f32, u8>(&flat)).unwrap();
        assert_eq!(load_channel_peaks(&path).unwrap(), peaks);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mono_shows_its_one_channel_on_both_lanes() {
        let peaks = generate_channel_peaks(&[0.5, -0.4], 1, 2);
        assert_eq!(peaks[0], peaks[1]);
    }
}

#[cfg(test)]
mod conform_regression_tests {
    //! Regression tests for two latent ffmpeg-next resampler bugs that
    //! used to silently corrupt PCM WAV imports:
    //!
    //! 1. PCM WAVs with `unknown` channel_layout in the container produced
    //!    decoded frames with empty `ch_layout`, which made
    //!    `swr_convert_frame` reject every frame ("Input changed"). The
    //!    cache file ended up empty.
    //!
    //! 2. Upsampling (e.g. 16 kHz → 48 kHz) was silently truncated because
    //!    ffmpeg-next's `Context::run` pre-allocates the output frame at
    //!    `input.samples()` count instead of the rate-scaled size.
    //!
    //! Both fixes live in `conform_audio_to_paths`. These tests construct
    //! synthetic mono PCM WAVs (which exhibit both bugs in the wild) and
    //! verify the conformed PCM has the right size and is non-empty.
    use super::*;

    fn write_test_wav(path: &Path, samples: &[i16], sample_rate: u32) {
        let n = samples.len();
        let byte_rate = sample_rate * 2; // mono, 16-bit = 2 bytes/sample
        let data_size = (n * 2) as u32;
        let mut bytes = Vec::with_capacity(44 + data_size as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
        bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn sine_wave_samples(sample_rate: u32, duration_secs: f64) -> Vec<i16> {
        let n = (sample_rate as f64 * duration_secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                let v = (t * 440.0 * std::f64::consts::TAU).sin();
                (v * 16000.0) as i16
            })
            .collect()
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "klipp_conform_test_{}_{}",
            label,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn conform_decodes_unknown_layout_pcm_wav() {
        ffmpeg_next::init().unwrap();
        let dir = temp_dir("unknown_layout");
        let wav = dir.join("input.wav");
        // 1 second of mono sine at 48 kHz — no resampling, isolates the
        // empty-channel-layout bug.
        write_test_wav(&wav, &sine_wave_samples(48_000, 1.0), 48_000);

        let pcm = dir.join("out.pcm");
        let peaks = dir.join("out.peaks");
        let mid = klipp_core::MediaId::new();

        let entry = conform_audio_to_paths(mid, &wav, &pcm, &peaks)
            .expect("conform should succeed for mono PCM WAV");

        // ~1s of stereo f32 at 48 kHz = 48000 stereo frames = 384000 bytes.
        let pcm_bytes = std::fs::metadata(&pcm).unwrap().len();
        assert!(
            pcm_bytes > 100_000,
            "conform produced unexpectedly small PCM ({} bytes) — \
             empty-channel-layout regression?",
            pcm_bytes
        );
        assert!(
            (entry.duration_secs - 1.0).abs() < 0.05,
            "expected ~1s, got {:.3}s",
            entry.duration_secs
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conform_upsamples_low_rate_pcm_wav() {
        ffmpeg_next::init().unwrap();
        let dir = temp_dir("upsample");
        let wav = dir.join("input.wav");
        // 1 second of mono sine at 16 kHz — exercises the upsampling
        // truncation bug. Output should be ~1s at 48 kHz stereo.
        write_test_wav(&wav, &sine_wave_samples(16_000, 1.0), 16_000);

        let pcm = dir.join("out.pcm");
        let peaks = dir.join("out.peaks");
        let mid = klipp_core::MediaId::new();

        let entry = conform_audio_to_paths(mid, &wav, &pcm, &peaks)
            .expect("conform should succeed for 16 kHz mono WAV");

        // 1s @ 48 kHz stereo f32 = 48000 * 2 * 4 = 384000 bytes.
        // Pre-fix this would have been ~128000 bytes (1/3 of expected).
        let pcm_bytes = std::fs::metadata(&pcm).unwrap().len();
        assert!(
            pcm_bytes > 350_000,
            "conform produced too few PCM bytes for upsampling case ({}) \
             — output-buffer-truncation regression?",
            pcm_bytes
        );
        assert!(
            (entry.duration_secs - 1.0).abs() < 0.05,
            "expected ~1s after upsampling, got {:.3}s",
            entry.duration_secs
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
