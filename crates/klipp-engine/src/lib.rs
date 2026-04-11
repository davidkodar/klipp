pub mod audio_decoder;
pub mod audio_output;
pub mod audio_source;
pub mod composite;
pub mod engine;
pub mod mix_thread;
pub mod render_cache;
pub mod pcm_reader;
pub mod skim_audio;
pub mod sync;
pub mod texture;
pub mod transition;
pub mod types;
pub mod video_decoder;

pub use composite::{composite_layers, CompositeLayer};
pub use engine::{detect_rotation_from_path, generate_filmstrip_incremental, generate_thumbnail, generate_thumbnail_at_size, generate_thumbnail_non_black, generate_thumbnail_at_size_non_black, is_frame_black, PlaybackEngine, PlaybackState, SequentialFrameDecoder};
pub use texture::{FrameBuffer, SharedFrameBuffer};
pub use types::{AudioRingBuf, DecodedFrame, FrameData, FrameQueue, SharedFrameQueue};

/// Initialize ffmpeg. Idempotent — safe to call multiple times. Required
/// before any decode operation. The app binary calls this once at startup;
/// tests in downstream crates that exercise decode helpers should call it
/// themselves to avoid depending on `ffmpeg_next` directly.
pub fn init_ffmpeg() -> Result<(), ffmpeg_next::Error> {
    ffmpeg_next::init()
}

/// Generate a downsampled waveform peaks summary for an audio file.
///
/// Decodes the file via ffmpeg, resamples to 4 kHz MONO f32, accumulates
/// the samples, then computes `num_buckets` (min, max) pairs across the
/// entire stream. Memory bounded to ~57 MB for a 1-hour file.
///
/// Used by the file browser to show tiny waveform thumbnails for audio
/// files. Much lighter than `klipp_media::audio_cache::conform_audio`
/// (which writes a full PCM cache) — this just produces the visual
/// summary, no PCM persisted.
pub fn generate_audio_peaks(
    path: &std::path::Path,
    num_buckets: usize,
) -> Result<Vec<(f32, f32)>, String> {
    if num_buckets == 0 {
        return Ok(Vec::new());
    }

    let mut format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let audio_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .ok_or_else(|| "no audio stream".to_string())?;
    let stream_index = audio_stream.index();
    let params = audio_stream.parameters();

    let codec_ctx = ffmpeg_next::codec::context::Context::from_parameters(params)
        .map_err(|e| format!("codec context: {e}"))?;
    let mut decoder = codec_ctx
        .decoder()
        .audio()
        .map_err(|e| format!("audio decoder: {e}"))?;

    // Resample to 4 kHz MONO f32 packed. The low rate keeps memory bounded
    // to ~57 MB for a 1-hour file while still providing enough samples per
    // bucket (>100 even for 1-hour files at 144 buckets) for accurate
    // (min, max) peak detection.
    const TARGET_RATE: u32 = 4000;
    let mut resampler = ffmpeg_next::software::resampling::Context::get(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
        ffmpeg_next::ChannelLayout::MONO,
        TARGET_RATE,
    )
    .map_err(|e| format!("resampler: {e}"))?;

    let mut all_samples: Vec<f32> = Vec::new();
    let mut frame = ffmpeg_next::frame::Audio::empty();
    let mut resampled = ffmpeg_next::frame::Audio::empty();

    // Two-phase decode loop (drain before send) — same pattern as the
    // video decode helpers and audio_cache.rs.
    let mut packets = format_ctx.packets();
    let mut pending_packet: Option<ffmpeg_next::Packet> = None;
    let mut eof = false;

    loop {
        // Phase 1: drain all decoded frames
        loop {
            if decoder.receive_frame(&mut frame).is_err() {
                break;
            }
            if resampler.run(&frame, &mut resampled).is_err() {
                continue;
            }
            // Mono f32 packed → samples are sequential floats. We use the
            // actual sample count from the resampled frame, not the buffer
            // length, because the buffer can include padding.
            let n_samples = resampled.samples();
            let bytes_needed = n_samples * std::mem::size_of::<f32>();
            let raw = resampled.data(0);
            let usable = &raw[..bytes_needed.min(raw.len())];
            // Reinterpret bytes as f32 — safe because the resampler outputs
            // packed f32 directly.
            let chunk_count = usable.len() / 4;
            all_samples.reserve(chunk_count);
            for i in 0..chunk_count {
                let bytes = [usable[i * 4], usable[i * 4 + 1], usable[i * 4 + 2], usable[i * 4 + 3]];
                all_samples.push(f32::from_le_bytes(bytes));
            }
        }

        // Phase 2: feed next packet
        let packet = if let Some(p) = pending_packet.take() {
            Some(p)
        } else if eof {
            None
        } else {
            match packets.next() {
                Some((stream, p)) if stream.index() == stream_index => Some(p),
                Some(_) => continue,
                None => {
                    eof = true;
                    None
                }
            }
        };

        match packet {
            Some(p) => match decoder.send_packet(&p) {
                Ok(()) => {}
                Err(ffmpeg_next::Error::Other { errno: 11 }) => {
                    // EAGAIN — decoder full, drain in next iteration.
                    pending_packet = Some(p);
                }
                Err(_) => {} // bad packet — skip
            },
            None => {
                // Stream exhausted — flush.
                let _ = decoder.send_eof();
                // Drain any remaining frames after EOF.
                while decoder.receive_frame(&mut frame).is_ok() {
                    if resampler.run(&frame, &mut resampled).is_err() {
                        continue;
                    }
                    let n_samples = resampled.samples();
                    let bytes_needed = n_samples * std::mem::size_of::<f32>();
                    let raw = resampled.data(0);
                    let usable = &raw[..bytes_needed.min(raw.len())];
                    let chunk_count = usable.len() / 4;
                    all_samples.reserve(chunk_count);
                    for i in 0..chunk_count {
                        let bytes = [usable[i * 4], usable[i * 4 + 1], usable[i * 4 + 2], usable[i * 4 + 3]];
                        all_samples.push(f32::from_le_bytes(bytes));
                    }
                }
                break;
            }
        }
    }

    if all_samples.is_empty() {
        return Err("no audio samples decoded".to_string());
    }

    // Use the existing waveform helper to bucket the samples.
    let peaks = klipp_media::waveform::generate_waveform_from_samples(
        &all_samples,
        1, // mono
        num_buckets,
    );
    Ok(peaks)
}

/// Lightweight media metadata read from a file's container header. No frame
/// decode, no seek — just opens the input and reads stream parameters.
/// Typically <50 ms for any container.
///
/// Returned as a plain struct so downstream crates can display it without
/// depending on `ffmpeg_next` directly.
#[derive(Debug, Clone, Default)]
pub struct MediaHeader {
    pub codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub duration_secs: Option<f64>,
}

/// Read just the format header via ffmpeg. Errors are returned as strings
/// so the caller can show them in the UI.
pub fn probe_media_header(path: &std::path::Path) -> Result<MediaHeader, String> {
    let format_ctx = ffmpeg_next::format::input(path)
        .map_err(|e| format!("ffmpeg open: {e}"))?;

    let mut info = MediaHeader::default();

    if let Some(stream) = format_ctx.streams().best(ffmpeg_next::media::Type::Video) {
        let params = stream.parameters();
        info.codec = Some(format!("{:?}", params.id()));

        if let Ok(decoder_ctx) = ffmpeg_next::codec::context::Context::from_parameters(params) {
            if let Ok(video) = decoder_ctx.decoder().video() {
                info.width = Some(video.width());
                info.height = Some(video.height());
            }
        }

        let fr = stream.avg_frame_rate();
        if fr.numerator() > 0 && fr.denominator() > 0 {
            info.fps = Some(fr.numerator() as f64 / fr.denominator() as f64);
        }
    }

    let dur = format_ctx.duration();
    if dur > 0 {
        info.duration_secs = Some(dur as f64 / 1_000_000.0);
    }

    Ok(info)
}
