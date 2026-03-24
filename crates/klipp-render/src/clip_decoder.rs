use crate::error::RenderError;
use std::path::Path;
use tracing::debug;

/// Synchronous, frame-accurate decoder for a single clip during export.
///
/// Unlike the playback decoder (async, tolerates ±50ms imprecision), this decoder
/// is sequential and exact: it seeks to the keyframe at-or-before `source_start_secs`,
/// skips frames until the exact start PTS, then decodes forward frame-by-frame.
///
/// Uses Lanczos scaling for maximum quality (export, not real-time).
/// Always decodes in software — HW decode adds complexity with minimal benefit
/// for non-real-time export where quality > speed.
pub struct RenderDecoder {
    format_ctx: ffmpeg_next::format::context::Input,
    decoder: ffmpeg_next::codec::decoder::Video,
    video_stream_idx: usize,
    scaler: ffmpeg_next::software::scaling::Context,
    /// Stream time_base as f64 ratio (num/den) for PTS→seconds conversion.
    time_base_secs: f64,
    /// Source time (seconds) below which frames are discarded (keyframe→target skip).
    skip_until_secs: f64,
    /// Source time (seconds) at which this clip ends — frames at/beyond this are not returned.
    source_end_secs: f64,
    /// fps of the sequence — used for half-frame tolerance in skip logic.
    fps: f64,
    /// True when the demuxer has reached EOF.
    demuxer_eof: bool,
    /// True when the decoder has been flushed and returned EOF.
    decoder_eof: bool,
    /// True once we've sent EOF to the decoder (to avoid sending it twice).
    eof_sent: bool,
    /// Total frames decoded (for diagnostics).
    frames_decoded: u64,
    /// Total frames skipped during seek-to-exact (for diagnostics).
    frames_skipped: u64,
}

impl RenderDecoder {
    /// Open a decoder for a media file, seeking to `source_start_secs`.
    ///
    /// The decoder will skip frames before the exact start point (Gap 5 fix)
    /// and stop returning frames when `source_end_secs` is reached.
    pub fn open(
        path: &Path,
        source_start_secs: f64,
        source_end_secs: f64,
        target_width: u32,
        target_height: u32,
        fps: f64,
    ) -> Result<Self, RenderError> {
        let mut format_ctx = ffmpeg_next::format::input(path)
            .map_err(RenderError::Decode)?;

        let video_stream = format_ctx
            .streams()
            .best(ffmpeg_next::media::Type::Video)
            .ok_or_else(|| RenderError::NoVideoStream(path.to_path_buf()))?;

        let video_stream_idx = video_stream.index();
        let time_base = video_stream.time_base();
        let time_base_secs = time_base.numerator() as f64 / time_base.denominator() as f64;

        // Set up codec context — software decode with multi-threading
        let mut codec_ctx =
            ffmpeg_next::codec::context::Context::from_parameters(video_stream.parameters())
                .map_err(RenderError::Decode)?;

        // Multi-threaded software decode (thread_count=0 = auto)
        unsafe {
            (*codec_ctx.as_mut_ptr()).thread_count = 0;
        }

        let decoder = codec_ctx
            .decoder()
            .video()
            .map_err(RenderError::Decode)?;

        // Create scaler: source format → RGBA at target resolution.
        // Use Lanczos for export quality (NOT FAST_BILINEAR like playback).
        let scaler = ffmpeg_next::software::scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg_next::format::Pixel::RGBA,
            target_width,
            target_height,
            ffmpeg_next::software::scaling::Flags::LANCZOS,
        )
        .map_err(RenderError::Decode)?;

        // Seek to keyframe at-or-before source_start_secs.
        // The seek lands on a keyframe, so we'll need to decode and discard
        // frames until we reach the exact target PTS (Gap 5).
        if source_start_secs > 0.0 {
            let ts = (source_start_secs * 1_000_000.0) as i64; // AV_TIME_BASE = 1_000_000
            let _ = format_ctx.seek(ts, ..ts);
        }

        debug!(
            "RenderDecoder::open {:?} [{:.3}s..{:.3}s] → {}×{} Lanczos",
            path, source_start_secs, source_end_secs, target_width, target_height
        );

        Ok(Self {
            format_ctx,
            decoder,
            video_stream_idx,
            scaler,
            time_base_secs,
            skip_until_secs: source_start_secs,
            source_end_secs,
            fps,
            demuxer_eof: false,
            decoder_eof: false,
            eof_sent: false,
            frames_decoded: 0,
            frames_skipped: 0,
        })
    }

    /// Decode the next frame. Returns `Ok(None)` when the clip is exhausted.
    ///
    /// Implements the correct two-phase decode pattern (Gap 4):
    /// 1. Drain all available frames from the decoder
    /// 2. Only then feed the next packet
    ///
    /// Also implements exact PTS skip (Gap 5): frames before `skip_until_secs`
    /// are decoded but not returned.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, RenderError> {
        if self.decoder_eof {
            return Ok(None);
        }

        loop {
            // Phase 1: Try to drain a frame from the decoder
            let mut decoded = ffmpeg_next::util::frame::Video::empty();
            match self.decoder.receive_frame(&mut decoded) {
                Ok(()) => {
                    // Check PTS against source_end — stop if we've gone past
                    let frame_pts_secs = decoded
                        .pts()
                        .map(|pts| pts as f64 * self.time_base_secs)
                        .unwrap_or(0.0);

                    // Gap 5: Skip frames before exact start point.
                    // Half-frame tolerance: a frame is "before" if its PTS is more than
                    // half a frame duration before the target.
                    let half_frame = 0.5 / self.fps;
                    if self.skip_until_secs > 0.0 && frame_pts_secs < self.skip_until_secs - half_frame {
                        self.frames_skipped += 1;
                        continue; // Discard and try next frame
                    }
                    // Done skipping after first accepted frame
                    if self.skip_until_secs > 0.0 {
                        debug!(
                            "RenderDecoder: skipped {} frames to reach {:.3}s (landed at {:.3}s)",
                            self.frames_skipped, self.skip_until_secs, frame_pts_secs
                        );
                        self.skip_until_secs = 0.0;
                    }

                    // Check if we've reached the end of this clip's source range
                    if frame_pts_secs >= self.source_end_secs + half_frame {
                        self.decoder_eof = true;
                        return Ok(None);
                    }

                    // Scale to RGBA at target resolution
                    let rgba = self.scale_frame(&decoded)?;
                    self.frames_decoded += 1;
                    return Ok(Some(rgba));
                }
                Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
                    // Decoder needs more input — fall through to Phase 2
                }
                Err(ffmpeg_next::Error::Eof) => {
                    self.decoder_eof = true;
                    return Ok(None);
                }
                Err(e) => return Err(RenderError::Decode(e)),
            }

            // Phase 2: Feed the next video packet to the decoder
            if self.demuxer_eof {
                // Already hit demuxer EOF — send EOF to decoder if not already done
                if !self.eof_sent {
                    self.eof_sent = true;
                    let _ = self.decoder.send_eof(); // Ignore "already EOF" errors
                }
                continue;
            }

            // Read packets until we find a video packet or hit EOF.
            // Use raw av_read_frame since packets() borrows format_ctx.
            loop {
                let read_result = read_next_packet(&mut self.format_ctx);
                match read_result {
                    Ok(packet) => {
                        if packet.stream() == self.video_stream_idx {
                            self.decoder
                                .send_packet(&packet)
                                .map_err(RenderError::Decode)?;
                            break; // Go back to Phase 1 to drain
                        }
                        // Non-video packet: keep reading (don't break to Phase 1)
                    }
                    Err(ffmpeg_next::Error::Eof) => {
                        // Demuxer EOF
                        self.demuxer_eof = true;
                        if !self.eof_sent {
                            self.eof_sent = true;
                            let _ = self.decoder.send_eof();
                        }
                        break; // Go back to Phase 1 to drain remaining frames
                    }
                    Err(e) => {
                        return Err(RenderError::Decode(e));
                    }
                }
            }
        }
    }

    /// Scale a decoded video frame to RGBA at the target resolution.
    fn scale_frame(&mut self, decoded: &ffmpeg_next::util::frame::Video) -> Result<Vec<u8>, RenderError> {
        let mut rgba_frame = ffmpeg_next::util::frame::Video::empty();
        self.scaler
            .run(decoded, &mut rgba_frame)
            .map_err(RenderError::Decode)?;

        // Extract RGBA data. The linesize may include padding, so we need
        // to copy row-by-row if linesize != width * 4.
        let width = rgba_frame.width() as usize;
        let height = rgba_frame.height() as usize;
        let linesize = rgba_frame.stride(0);
        let expected_stride = width * 4;

        if linesize == expected_stride {
            // No padding — fast path
            Ok(rgba_frame.data(0)[..expected_stride * height].to_vec())
        } else {
            // Has padding — copy row by row
            let mut out = vec![0u8; expected_stride * height];
            let src = rgba_frame.data(0);
            for y in 0..height {
                let src_offset = y * linesize;
                let dst_offset = y * expected_stride;
                out[dst_offset..dst_offset + expected_stride]
                    .copy_from_slice(&src[src_offset..src_offset + expected_stride]);
            }
            Ok(out)
        }
    }

    /// Number of frames decoded and returned so far.
    pub fn frames_decoded(&self) -> u64 {
        self.frames_decoded
    }

    /// Number of frames skipped during seek-to-exact.
    pub fn frames_skipped(&self) -> u64 {
        self.frames_skipped
    }
}

impl Drop for RenderDecoder {
    fn drop(&mut self) {
        if self.frames_decoded > 0 || self.frames_skipped > 0 {
            debug!(
                "RenderDecoder: {} decoded, {} skipped",
                self.frames_decoded, self.frames_skipped
            );
        }
    }
}

/// Read the next packet from the demuxer using raw FFI (av_read_frame).
/// This avoids the lifetime issue with `format_ctx.packets()`.
/// Read the next packet from the demuxer using raw FFI (av_read_frame).
/// This avoids the lifetime issue with `format_ctx.packets()`.
fn read_next_packet(
    format_ctx: &mut ffmpeg_next::format::context::Input,
) -> Result<ffmpeg_next::Packet, ffmpeg_next::Error> {
    use ffmpeg_next::codec::packet::Mut as PacketMut;
    unsafe {
        let mut pkt = ffmpeg_next::Packet::empty();
        match ffmpeg_next::ffi::av_read_frame(format_ctx.as_mut_ptr(), pkt.as_mut_ptr()) {
            0 => Ok(pkt),
            e => Err(ffmpeg_next::Error::from(e)),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests — require real media files, so they are integration tests
// marked #[ignore] by default. Run with:
//   cargo test -p klipp-render -- --ignored
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Path to a test media file. Set TEST_MEDIA_FILE env var to override.
    fn test_media_path() -> Option<PathBuf> {
        std::env::var("TEST_MEDIA_FILE").ok().map(PathBuf::from)
    }

    #[test]
    #[ignore = "requires TEST_MEDIA_FILE env var pointing to a video file"]
    fn decoder_produces_frames() {
        let path = test_media_path().expect("set TEST_MEDIA_FILE env var");
        // Use 30fps as a reasonable default; the test is fps-agnostic.
        let fps = 30.0;
        let duration_secs = 2.0;
        let mut dec = RenderDecoder::open(&path, 0.0, duration_secs, 1920, 1080, fps).unwrap();

        let mut count = 0u64;
        while let Ok(Some(frame)) = dec.next_frame() {
            assert_eq!(frame.len(), 1920 * 1080 * 4);
            count += 1;
        }

        assert!(count > 0, "expected at least one frame");
        // Allow wide tolerance since source fps may vary
        let expected = (duration_secs * fps) as u64;
        assert!(
            count >= expected / 2 && count <= expected * 2,
            "expected ~{expected} frames for {duration_secs}s, got {count}"
        );
        println!("decoded {count} frames, skipped {}", dec.frames_skipped());
    }

    #[test]
    #[ignore = "requires TEST_MEDIA_FILE env var pointing to a video file"]
    fn decoder_seeks_correctly() {
        let path = test_media_path().expect("set TEST_MEDIA_FILE env var");
        let fps = 30.0;
        let duration_secs = 2.0;

        // Decode from 1.0s — should skip frames between keyframe and 1.0s
        let mut dec = RenderDecoder::open(&path, 1.0, 1.0 + duration_secs, 1280, 720, fps).unwrap();

        let mut count = 0u64;
        while let Ok(Some(_)) = dec.next_frame() {
            count += 1;
        }

        assert!(count > 0, "expected frames after seek");
        let expected = (duration_secs * fps) as u64;
        assert!(
            count >= expected / 2 && count <= expected * 2,
            "expected ~{expected} frames for {duration_secs}s from seek, got {count}"
        );
        println!(
            "decoded {count} frames, skipped {} during seek",
            dec.frames_skipped()
        );
    }

    #[test]
    #[ignore = "requires TEST_MEDIA_FILE env var pointing to a video file"]
    fn decoder_respects_source_end() {
        let path = test_media_path().expect("set TEST_MEDIA_FILE env var");
        let fps = 30.0;
        let duration_secs = 0.5;

        let mut dec = RenderDecoder::open(&path, 0.0, duration_secs, 640, 480, fps).unwrap();

        let mut count = 0u64;
        while let Ok(Some(_)) = dec.next_frame() {
            count += 1;
        }

        let expected = (duration_secs * fps) as u64;
        assert!(
            count >= expected / 2 && count <= expected * 2,
            "expected ~{expected} frames for {duration_secs}s, got {count}"
        );
    }
}
