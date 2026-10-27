//! Real-time playback engine: decode, composite, mix, output.
//!
//! The engine owns the running timeline. One [`PlaybackEngine`] runs on
//! its own thread, receives [`klipp_core::AppCommand`]s from the UI, and
//! emits [`klipp_core::AppEvent`]s back. It drives video decode
//! ([`video_decoder`]), audio decode ([`audio_decoder`], [`pcm_reader`]),
//! multi-track audio mixing ([`mix_thread`]), audio output
//! ([`audio_output`]), compositing ([`klipp_compose`]), effects application,
//! and the render cache ([`render_cache`]).
//!
//! # Threading model
//!
//! * **Engine thread** — owns `PlaybackEngine`; drains commands, advances
//!   the playhead, pops decoded frames, composites, writes to
//!   [`texture::FrameBuffer`]. Never blocks on the UI.
//! * **Decode threads** — one per open video decoder, one per active
//!   audio source. Communicate via [`types::FrameQueue`] (bounded Mutex)
//!   and [`types::AudioRingBuf`] (lock-free ringbuf).
//! * **Mix thread** — drains per-track audio staging buffers, sums,
//!   applies per-clip gain / pan / fade / effects, writes to the cpal
//!   ring. FTZ/DAZ enabled to avoid denormal stalls on silence.
//! * **cpal audio callback** — on an OS-owned thread; pulls from the mix
//!   ring and drives the output device.
//!
//! # Synchronisation primitive
//!
//! [`sync::MasterClock`] is the single source of truth for "what time is
//! it". Audio and video both slave to it. See `sync.rs` for the
//! seek / pause / resume state machine and the generation counter used
//! by callers to detect stale state.
//!
//! # Decode-pipeline invariant
//!
//! Two-phase decode: **always drain `receive_frame` before feeding new
//! packets with `send_packet`.** Mixing the two order causes EAGAIN
//! packet loss on high-latency codecs (AV1, HEVC with B-frames, VP9).
//! On EAGAIN from `send_packet`, drain output frames then retry — the
//! helpers in this module (`video_decoder.rs`, `audio_decoder.rs`, and
//! every `engine.rs` helper that calls `send_packet` directly) follow
//! this rule; new code must too.
//!
//! # Colour pipeline
//!
//! Linear light throughout. The decoder emits [`types::LinearFrame`] —
//! f32 RGBA in the working space the grade runs in, where 0.18 is mid
//! grey and values above 1.0 keep their highlight headroom — and every
//! effect, grade and composite works on those floats.
//!
//! Eight-bit bytes survive at the two ends only. The hardware decoder's
//! conversion shader leaves the frame as sRGB u8 in a `wgpu::Buffer`
//! ([`frame::GpuFrame`]), which [`types::realise_linear_from_gpu`]
//! expands when something asks for CPU pixels; and the viewer's
//! [`texture::FrameBuffer`] holds what the display transform produced.
//!
//! That transform — `klipp_color::display`, dispatched per working
//! space by `PlaybackEngine::display_encode` — is the one place
//! out-of-range linear values are resolved. On the GPU path it encodes
//! straight into the `Rgba8Unorm` texture the UI samples, with no
//! readback.

#[cfg(target_os = "linux")]
pub mod alsa_quiet;
pub mod analyse;
pub use analyse::{analyse_into, analyse_media};
pub mod canonical_hash;
pub mod node_thumbnails;
pub mod audio_decoder;
pub mod audio_input;
pub mod audio_output;
pub mod audio_source;
pub mod commands;
pub mod decode_budget;
pub mod disk_budget;
pub mod engine;
pub mod fast_path;
pub mod scopes;
pub mod frame;
pub mod gpu_health;
pub mod ffmpeg_caps;
pub mod grade_resample;
pub mod mix_thread;
pub mod perf;
pub mod render_cache;
pub mod pcm_reader;
pub mod skim;
pub mod skim_audio;
pub mod sync;
pub mod texture;
pub mod thread_guard;
pub mod transition;
pub mod types;
pub mod video_decoder;

// Re-export composite types from klipp-effects so existing import
// paths (`klipp_engine::composite_layers`) keep working after the
// Phase 4G move. Down-stream crates can also import directly from
// `klipp_effects::composite::*`.
pub use klipp_effects::composite::{composite_layers, CompositeLayer};
pub use engine::{detect_rotation_from_path, engine_stage_name, generate_filmstrip_incremental, generate_filmstrip_range, FilmstripOutcome, generate_thumbnail, generate_thumbnail_at_size, generate_thumbnail_at_size_with_rotation, generate_thumbnail_non_black, generate_thumbnail_at_size_non_black, is_frame_black, seq_write_holder_name, seq_write_holder_set, ui_stage, EngineStage, PlaybackEngine, PlaybackState, SeqWriteHolder, SequentialFrameDecoder, ENGINE_STAGE, ENGINE_TICK, ENGINE_SHUTDOWN_COMPLETE, PREGRADE_DISPLAYS_MADE, SEQ_WRITE_HOLDER, UI_STAGE, UI_TICK, UI_WANTS_FRAMES};
pub use frame::{CpuFrame, CpuFrameRef, Frame, FrameRef, GpuFrame, GpuFrameRef};
pub use texture::{FrameBuffer, GpuOutputSnapshot, SharedFrameBuffer};
pub use types::{realise_linear_from_gpu, realise_srgb_u8_from_gpu, AudioRingBuf, DecodedFrame, FrameQueue, SharedFrameQueue, CPU_READBACK_COUNT, CPU_READBACK_US};

/// Initialize ffmpeg. Idempotent — safe to call multiple times. Required
/// before any decode operation. The app binary calls this once at startup;
/// tests in downstream crates that exercise decode helpers should call it
/// themselves to avoid depending on `ffmpeg_next` directly.
pub fn init_ffmpeg() -> Result<(), ffmpeg_next::Error> {
    ffmpeg_next::init()
}

/// `av_log_set_callback` target that routes ffmpeg's own log messages
/// into our `tracing` subscriber. Without this, ffmpeg writes to
/// stderr (invisible to GUI launches) — and we need its diagnostics
/// when triaging hwaccel / cuvid / decoder failures.
///
/// Levels are mapped:
///   * `AV_LOG_PANIC` / `_FATAL` / `_ERROR` → `tracing::error!`
///   * `AV_LOG_WARNING`                      → `tracing::warn!`
///   * `AV_LOG_INFO`                         → `tracing::info!`
///   * `AV_LOG_VERBOSE` / `_DEBUG` / lower   → `tracing::debug!`
///
/// `av_log_format_line` does the printf-style formatting for us so
/// we never touch a `va_list` directly from Rust.
///
/// # Safety
///
/// Called from arbitrary ffmpeg-internal threads. We use a
/// stack-allocated buffer (no heap from the callback) and make zero
/// assumptions about the `avcl` pointer's lifetime — only its name
/// is read for context, then we drop the reference.
#[no_mangle]
pub unsafe extern "C" fn ffmpeg_log_to_tracing(
    avcl: *mut std::ffi::c_void,
    level: std::os::raw::c_int,
    fmt: *const std::os::raw::c_char,
    vl: *mut ffmpeg_next::ffi::__va_list_tag,
) {
    use ffmpeg_next::ffi::*;
    // Skip if level is above the user's configured ffmpeg log level
    // — av_log_format_line still pays the printf cost otherwise.
    let max_level = av_log_get_level();
    if level > max_level {
        return;
    }
    let mut buf = [0u8; 1024];
    let mut print_prefix: std::os::raw::c_int = 1;
    let written = av_log_format_line2(
        avcl,
        level,
        fmt,
        vl,
        buf.as_mut_ptr() as *mut std::os::raw::c_char,
        buf.len() as std::os::raw::c_int,
        &mut print_prefix,
    );
    if written <= 0 {
        return;
    }
    // av_log_format_line2 writes a NUL-terminated, newline-suffixed
    // string. Trim the trailing newline so tracing doesn't render
    // double line breaks.
    let len = (written as usize).min(buf.len()).saturating_sub(1);
    let raw = &buf[..len];
    let trimmed = if raw.last() == Some(&b'\n') { &raw[..raw.len() - 1] } else { raw };
    let msg = String::from_utf8_lossy(trimmed);
    // AV_LOG_* constants from libavutil/log.h:
    // PANIC=0, FATAL=8, ERROR=16, WARNING=24, INFO=32, VERBOSE=40, DEBUG=48, TRACE=56
    //
    // No `target:` override — EnvFilter's crate whitelist treats
    // custom targets as opaque names, which silently drops the
    // event. Default target = module path = `klipp_engine`, which
    // is whitelisted in `klipp-app::main`.
    if level <= 16 {
        tracing::error!("[ffmpeg] {msg}");
    } else if level <= 24 {
        tracing::warn!("[ffmpeg] {msg}");
    } else if level <= 32 {
        tracing::info!("[ffmpeg] {msg}");
    } else {
        tracing::debug!("[ffmpeg] {msg}");
    }
}

/// Extract the largest embedded JPEG preview from a camera RAW file (ARW,
/// CR2, NEF, RAF, ORF, DNG, etc.) and decode it to RGBA.
///
/// Stock ffmpeg can't decode the proprietary compression in most RAW formats
/// (e.g. Sony's "Unknown compression method 32767" on ARW). Doing real
/// demosaicing would require pulling in libraw or a Rust equivalent like
/// rawler — heavy dependency for what's almost always a thumbnail use case.
///
/// Every RAW file produced by a real camera embeds 1-3 JPEG previews so the
/// camera's playback screen can render the image without doing a full RAW
/// conversion. The largest of these is typically a near-full-resolution copy
/// of the photo (e.g. 6192×3480 in a Sony A7 IV ARW). For thumbnail/preview
/// purposes that's strictly better than what we could produce by demosaicing
/// in 50ms anyway.
///
/// The algorithm:
/// 1. Read the file into memory.
/// 2. Linear scan for `FF D8 FF` JPEG SOI markers (followed by a valid
///    marker byte 0xC0..0xEF, which excludes `FF D9` EOI being mistaken
///    for a SOI continuation).
/// 3. For each candidate SOI, find the next `FF D9` EOI to determine the
///    JPEG's length.
/// 4. Pick the longest candidate — by camera convention this is always the
///    full-resolution preview, never an intermediate thumbnail.
/// 5. Decode the extracted bytes via the `image` crate (jpeg feature) and
///    optionally resize to `max_width` on the long edge.
///
/// Returns `(rgba, width, height)` matching the shape of the existing
/// thumbnail helpers, so callers in the file browser thumb pipeline can use
/// this as a drop-in replacement when they detect a RAW extension.
///
/// # Limitations
///
/// **This function returns the camera-baked JPEG preview, NOT demosaiced
/// data from the raw bayer sensor values.** That means:
///
/// - White balance, exposure, highlight recovery, and any other in-camera
///   processing are baked into the pixels we return — there's nothing to
///   "develop" or color-grade at the raw level.
/// - The bit depth is whatever the camera's preview JPEG is (typically 8-bit
///   per channel), not the 12-14 bits captured by the sensor.
/// - Any sharpening / noise reduction applied by the camera is also baked in.
///
/// For thumbnails, file browser previews, drag-drop into the timeline, and
/// "show me what's in this file" workflows, this is exactly right and matches
/// what the user expects (the same image their camera shows on its screen).
///
/// # Upgrading to real RAW decoding
///
/// If Klipp ever needs to support **actual raw image editing** — color grading
/// on linear bayer values, white balance adjustment, exposure compensation
/// applied to the sensor data, highlight recovery from clipped highlights —
/// then we need to add real RAW decoding alongside this preview path. The
/// preview-extraction code here should NOT be deleted: it's still the right
/// thing for fast thumbnails. The real-decode path is an addition, not a
/// replacement.
///
/// Two reasonable options for that future work:
///
/// 1. **`rawler` crate** ([crates.io/crates/rawler](https://crates.io/crates/rawler)):
///    pure Rust, no system dependencies, MIT/Apache. Maintained as part of
///    the dnglab project. Supports all the formats in `RAW_EXTENSIONS` plus
///    more. Returns demosaiced linear RGB. ~2 MB compiled. Recommended.
///
/// 2. **`libraw` C++ library via `libraw-rs` bindings**: the gold-standard
///    RAW decoder used by darktable, RawTherapee, etc. Most comprehensive
///    format support. Requires libraw as a system dependency, which is a
///    deployment headache for an app like Klipp that ships as a single
///    binary. Avoid unless rawler turns out to be insufficient.
///
/// The integration shape would be:
/// - Add a new helper alongside this one, e.g.
///   `decode_raw_demosaiced(path) -> Result<(Vec<f32>, u32, u32), String>`
///   returning linear RGB at 32-bit per channel.
/// - Use the demosaiced output for the source viewer, color grading, and
///   the timeline render path when the user explicitly opts in to "raw mode"
///   for a clip.
/// - Keep `extract_raw_preview` for everything thumbnail-related — it's
///   ~50ms vs ~500ms-1s for full demosaicing on a 24MP image, and the
///   visual difference is invisible at thumbnail sizes.
/// - Add a new IMAGE_EXTENSIONS / RAW_EXTENSIONS distinction in the media
///   pool item type so the engine knows which decode path to use per clip.
///
/// In other words: this function stays exactly as-is forever; the upgrade
/// path is purely additive.
pub fn extract_raw_preview(
    path: &std::path::Path,
    max_width: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("read raw file: {e}"))?;
    if bytes.len() < 8 {
        return Err("file too small to contain a preview".to_string());
    }

    // Find the longest contained JPEG. We use a small state machine that
    // alternates between LookingForSOI and LookingForEOI; this correctly
    // handles SEQUENTIAL JPEGs (the common case in RAW files where the
    // file is laid out as: thumbnail JPEG → medium preview JPEG → full
    // preview JPEG → raw bayer data).
    let (start, end) = find_largest_jpeg(&bytes)
        .ok_or_else(|| "no embedded JPEG found in RAW file".to_string())?;

    let jpeg_bytes = &bytes[start..end];

    // Decode the extracted JPEG via the `image` crate.
    let dynamic = image::load_from_memory_with_format(jpeg_bytes, image::ImageFormat::Jpeg)
        .map_err(|e| format!("decode embedded jpeg: {e}"))?;

    // Resize to fit `max_width` on the long edge (preserve aspect).
    let src_w = dynamic.width();
    let src_h = dynamic.height();
    let scale = if src_w > max_width || src_h > max_width {
        (max_width as f32) / (src_w.max(src_h) as f32)
    } else {
        1.0
    };
    let dst_w = ((src_w as f32) * scale).round().max(1.0) as u32;
    let dst_h = ((src_h as f32) * scale).round().max(1.0) as u32;

    let resized = if scale < 1.0 {
        dynamic.resize(dst_w, dst_h, image::imageops::FilterType::Triangle)
    } else {
        dynamic
    };

    let rgba = resized.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Ok((rgba.into_raw(), w, h))
}

/// Find the byte offsets `(start, end)` of the longest embedded JPEG in `bytes`.
/// Returns `None` if no JPEG is found.
///
/// `start` points at the JPEG's `FF D8` SOI marker; `end` is one past the
/// `FF D9` EOI marker, so `&bytes[start..end]` is the complete JPEG.
///
/// Pulled out as a freestanding function so the unit tests can drive it
/// with synthetic byte sequences without touching the filesystem.
fn find_largest_jpeg(bytes: &[u8]) -> Option<(usize, usize)> {
    enum State {
        LookingForSoi,
        InsideJpeg { start: usize },
    }

    // A "valid SOI" is `FF D8 FF Xn` where Xn is a known JPEG marker byte.
    // We require the 4th byte to be 0xC0..=0xEF or 0xDB so that random
    // raw bayer data containing `FF D8 FF` followed by garbage doesn't
    // get mistaken for a JPEG start. This covers JFIF (E0), Exif (E1),
    // app markers (E2..EF), DQT (DB), and start-of-frame (C0..CF).
    let is_valid_soi_continuation = |b: u8| (0xC0..=0xEF).contains(&b) || b == 0xDB;

    let mut best: Option<(usize, usize)> = None;
    let mut state = State::LookingForSoi;
    let mut i = 0;
    while i + 1 < bytes.len() {
        match state {
            State::LookingForSoi => {
                if i + 3 < bytes.len()
                    && bytes[i] == 0xFF
                    && bytes[i + 1] == 0xD8
                    && bytes[i + 2] == 0xFF
                    && is_valid_soi_continuation(bytes[i + 3])
                {
                    state = State::InsideJpeg { start: i };
                    i += 2; // skip past SOI; the FF Xn at i+2..i+4 will be re-scanned
                } else {
                    i += 1;
                }
            }
            State::InsideJpeg { start } => {
                if bytes[i] == 0xFF && bytes[i + 1] == 0xD9 {
                    let end = i + 2;
                    let len = end - start;
                    // Reject suspiciously tiny "JPEGs" — likely false positives
                    // from random bayer data. Real previews are at least a few KB.
                    if len >= 1024 {
                        match best {
                            None => best = Some((start, end)),
                            Some((bs, be)) if (be - bs) < len => best = Some((start, end)),
                            _ => {}
                        }
                    }
                    state = State::LookingForSoi;
                    i = end;
                } else {
                    i += 1;
                }
            }
        }
    }

    best
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

    let mut format_ctx = klipp_media::open_input(path)
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
    let format_ctx = klipp_media::open_input(path)
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

#[cfg(test)]
mod raw_preview_tests {
    use super::*;

    /// Helper: build a synthetic byte stream that contains N "fake JPEGs"
    /// of given lengths at deterministic offsets, separated by filler bytes
    /// (zeros, which never form valid JPEG markers). Returns the bytes plus
    /// the (start, length) of each embedded JPEG so tests can verify the
    /// scanner picked the right one.
    fn build_synthetic_raw(
        jpeg_lengths: &[usize],
    ) -> (Vec<u8>, Vec<(usize, usize)>) {
        let mut out = Vec::new();
        let mut markers = Vec::new();
        // Some leading TIFF-ish padding so the first JPEG isn't at offset 0.
        out.extend_from_slice(b"II*\0\x08\0\0\0FAKE_TIFF_HEADER_PADDING");
        for &len in jpeg_lengths {
            assert!(len >= 4, "JPEG must have room for SOI + EOI");
            let start = out.len();
            // SOI: FF D8 FF E1 (Exif marker)
            out.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
            // Filler bytes — use 0xAA so they don't accidentally form
            // valid markers. Number of filler bytes = len - 6 (4 SOI + 2 EOI).
            out.extend(std::iter::repeat_n(0xAA, len - 6));
            // EOI: FF D9
            out.extend_from_slice(&[0xFF, 0xD9]);
            markers.push((start, len));
            // Some inter-JPEG padding (raw bayer-ish data — random-looking
            // bytes that contain neither FF D8 FF nor FF D9).
            out.extend(std::iter::repeat_n(0x55, 64));
        }
        (out, markers)
    }

    #[test]
    fn find_largest_jpeg_picks_largest_of_three() {
        // Three sequential JPEGs of increasing size — typical RAW layout
        // (small thumb, medium preview, large preview).
        let (bytes, markers) = build_synthetic_raw(&[200, 1500, 8192]);
        let result = find_largest_jpeg(&bytes);
        let (start, end) = result.expect("should find a JPEG");
        let (expected_start, expected_len) = markers[2]; // the 8192-byte one
        assert_eq!(start, expected_start);
        assert_eq!(end - start, expected_len);
    }

    #[test]
    fn find_largest_jpeg_picks_when_first_is_largest() {
        let (bytes, markers) = build_synthetic_raw(&[8192, 1500, 200]);
        let result = find_largest_jpeg(&bytes);
        let (start, end) = result.expect("should find a JPEG");
        let (expected_start, expected_len) = markers[0];
        assert_eq!(start, expected_start);
        assert_eq!(end - start, expected_len);
    }

    #[test]
    fn find_largest_jpeg_returns_none_for_no_jpegs() {
        // Random bytes that contain no JPEG markers.
        let bytes: Vec<u8> = (0..1000).map(|i| (i % 200) as u8).collect();
        assert!(find_largest_jpeg(&bytes).is_none());
    }

    #[test]
    fn find_largest_jpeg_returns_none_for_empty_input() {
        assert!(find_largest_jpeg(&[]).is_none());
    }

    #[test]
    fn find_largest_jpeg_rejects_tiny_false_positives() {
        // A "JPEG" of only 100 bytes is below the 1024-byte sanity floor —
        // rejected as a likely false positive from random raw bayer data.
        let (bytes, _markers) = build_synthetic_raw(&[100]);
        assert!(find_largest_jpeg(&bytes).is_none());
    }

    #[test]
    fn find_largest_jpeg_handles_truncated_final_jpeg() {
        // First JPEG is complete, second SOI has no matching EOI (truncated).
        // Scanner should return the first one and not panic.
        let (mut bytes, markers) = build_synthetic_raw(&[2048]);
        // Append a truncated JPEG: SOI but no EOI.
        bytes.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE1]);
        bytes.extend(std::iter::repeat_n(0xAA, 500));
        // No EOI — file ends here.

        let result = find_largest_jpeg(&bytes);
        let (start, end) = result.expect("should still find the first complete JPEG");
        let (expected_start, expected_len) = markers[0];
        assert_eq!(start, expected_start);
        assert_eq!(end - start, expected_len);
    }

    #[test]
    fn find_largest_jpeg_ignores_random_ff_d8_in_data() {
        // Random bytes that include `FF D8` followed by something invalid
        // (not a valid SOI continuation).
        let mut bytes = vec![0u8; 5000];
        bytes[100] = 0xFF;
        bytes[101] = 0xD8;
        bytes[102] = 0xFF;
        bytes[103] = 0x55; // not a valid marker — must NOT be treated as SOI
        // Add a real JPEG further along.
        let real_start = 1000;
        bytes[real_start] = 0xFF;
        bytes[real_start + 1] = 0xD8;
        bytes[real_start + 2] = 0xFF;
        bytes[real_start + 3] = 0xE1;
        // Pad with safe bytes
        for i in (real_start + 4)..(real_start + 2000) {
            bytes[i] = 0xAA;
        }
        // EOI
        bytes[real_start + 2000] = 0xFF;
        bytes[real_start + 2001] = 0xD9;
        bytes.truncate(real_start + 2002);

        let result = find_largest_jpeg(&bytes);
        let (start, _end) = result.expect("should find the real JPEG");
        assert_eq!(start, real_start, "false-positive at offset 100 should have been rejected");
    }

    #[test]
    fn extract_raw_preview_errors_on_missing_file() {
        let result = extract_raw_preview(
            std::path::Path::new("/nonexistent/path/to/file.arw"),
            480,
        );
        assert!(result.is_err());
    }

    #[test]
    fn extract_raw_preview_errors_on_file_with_no_jpeg() {
        // Write a small non-JPEG file to disk and verify extract_raw_preview
        // returns Err rather than panicking.
        let dir = std::env::temp_dir().join(format!(
            "klipp_test_no_jpeg_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fake.arw");
        std::fs::write(&path, b"this is not a jpeg, no markers here at all").unwrap();
        let result = extract_raw_preview(&path, 480);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Opt-in regression test against a real RAW file. Set
    /// `KLIPP_TEST_RAW_FILE=/path/to/file.arw` to enable.
    /// Skipped silently otherwise.
    #[test]
    fn extract_raw_preview_against_real_arw_file() {
        let path = match std::env::var("KLIPP_TEST_RAW_FILE") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => {
                eprintln!("[skipped] set KLIPP_TEST_RAW_FILE to a real RAW file path to run this test");
                return;
            }
        };
        let result = extract_raw_preview(&path, 480);
        match result {
            Ok((rgba, w, h)) => {
                eprintln!("[OK] {} → {}x{} ({} bytes RGBA)", path.display(), w, h, rgba.len());
                assert!(w > 0 && h > 0);
                assert_eq!(rgba.len() as u32, w * h * 4);
                // Result should fit within max_width on the long edge.
                assert!(w <= 480 && h <= 480, "result must fit in 480px max bound");
            }
            Err(e) => {
                panic!("extract_raw_preview failed for {}: {}", path.display(), e);
            }
        }
    }
}
