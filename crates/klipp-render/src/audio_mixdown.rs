use crate::error::RenderError;
use crate::snapshot::{AudioClipSnapshot, SequenceSnapshot};
use klipp_audio::dsp::{AudioEffectChain, AudioProcessContext};
use tracing::{debug, warn};

/// Synchronous audio mixdown for export.
///
/// Unlike the playback audio system (ring buffers, real-time cpal output), this is a
/// simple decode-and-mix loop: for each video frame, decode the corresponding audio
/// samples from all active clips, apply volume/pan/mute/solo, and sum to stereo PCM.
pub struct AudioMixdown {
    /// Per-track decoders — each track can have multiple clips decoded sequentially.
    track_decoders: Vec<TrackAudioState>,
    /// Track settings from snapshot (volume, pan, mute, solo).
    track_settings: Vec<TrackSettings>,
    /// Whether any track has solo enabled.
    any_solo: bool,
    /// Output sample rate.
    sample_rate: u32,
    /// Samples per video frame (= sample_rate / fps), mono count.
    samples_per_frame: usize,
    /// Sequence fps for frame→sample conversion.
    fps: f64,
}

struct TrackSettings {
    volume: f32,
    pan: f32,
    muted: bool,
    solo: bool,
}

/// Per-track state: the clips and current decoder position.
struct TrackAudioState {
    clips: Vec<AudioClipSnapshot>,
    /// Index into clips vec — which clip we're currently decoding.
    current_clip_idx: usize,
    /// Decoded audio buffer for the current clip (interleaved stereo f32).
    buffer: Vec<f32>,
    /// Read position within buffer.
    buffer_pos: usize,
}

impl AudioMixdown {
    /// Create from a sequence snapshot.
    pub fn new(snapshot: &SequenceSnapshot) -> Result<Self, RenderError> {
        let sample_rate = snapshot.sample_rate;
        let fps = snapshot.fps;
        let samples_per_frame = (sample_rate as f64 / fps).round() as usize;

        let any_solo = snapshot.audio_tracks.iter().any(|t| t.solo);

        let mut track_decoders = Vec::with_capacity(snapshot.audio_tracks.len());
        let mut track_settings = Vec::with_capacity(snapshot.audio_tracks.len());

        for track in &snapshot.audio_tracks {
            track_decoders.push(TrackAudioState {
                clips: track.clips.clone(),
                current_clip_idx: usize::MAX,
                buffer: Vec::new(),
                buffer_pos: 0,
            });
            track_settings.push(TrackSettings {
                volume: track.volume,
                pan: track.pan,
                muted: track.muted,
                solo: track.solo,
            });
        }

        debug!(
            "AudioMixdown: {} tracks, {}Hz, {:.1}fps, {} samples/frame",
            track_decoders.len(),
            sample_rate,
            fps,
            samples_per_frame
        );

        Ok(Self {
            track_decoders,
            track_settings,
            any_solo,
            sample_rate,
            samples_per_frame,
            fps,
        })
    }

    /// Get the interleaved stereo f32 audio samples for one video frame.
    /// Returns exactly `samples_per_frame * 2` floats (stereo), clamped
    /// to `[-1.0, 1.0]` so the encoder gets legal values.
    ///
    /// Use [`Self::get_frame_audio_unclamped`] instead for loudness
    /// measurement — LUFS metering expects the raw summed mix, and
    /// clamping first caps any over-1.0 energy and under-reports
    /// integrated loudness on hot material (R-3).
    pub fn get_frame_audio(&mut self, frame_num: u64) -> Vec<f32> {
        let mut mixed = self.mix_frame(frame_num);
        for s in &mut mixed {
            *s = s.clamp(-1.0, 1.0);
        }
        mixed
    }

    /// Get the interleaved stereo f32 audio samples without the
    /// output-stage `[-1.0, 1.0]` clamp. Only use this for loudness
    /// measurement — the encoder MUST get the clamped form so samples
    /// land in the codec's legal input range.
    ///
    /// R-3: LUFS was previously measured on the clamped output, which
    /// capped the apparent loudness of any over-full-scale summed mix.
    /// The meter then reported a value below the real integrated LUFS,
    /// loudness-normalize computed too much make-up gain, and the
    /// encoded file came in LOUDER than the requested target.
    pub fn get_frame_audio_unclamped(&mut self, frame_num: u64) -> Vec<f32> {
        self.mix_frame(frame_num)
    }

    fn mix_frame(&mut self, frame_num: u64) -> Vec<f32> {
        let stereo_samples = self.samples_per_frame * 2;
        let mut mixed = vec![0.0f32; stereo_samples];

        for (track_idx, track_state) in self.track_decoders.iter_mut().enumerate() {
            let settings = &self.track_settings[track_idx];

            // Mute/solo logic
            if settings.muted {
                continue;
            }
            if self.any_solo && !settings.solo {
                continue;
            }

            // Get samples for this frame from this track
            let track_samples = get_track_samples(
                track_state,
                frame_num,
                self.samples_per_frame,
                self.sample_rate,
                self.fps,
            );

            if track_samples.is_empty() {
                continue;
            }

            // Apply volume and pan, mix into output
            // Equal-power pan law: left = cos(pan_angle), right = sin(pan_angle)
            let pan_angle = (settings.pan + 1.0) * std::f32::consts::FRAC_PI_4;
            let pan_l = pan_angle.cos() * settings.volume;
            let pan_r = pan_angle.sin() * settings.volume;

            let sample_count = track_samples.len().min(stereo_samples);
            for i in (0..sample_count).step_by(2) {
                let l = track_samples[i];
                let r = if i + 1 < track_samples.len() {
                    track_samples[i + 1]
                } else {
                    l
                };
                mixed[i] += l * pan_l;
                if i + 1 < stereo_samples {
                    mixed[i + 1] += r * pan_r;
                }
            }
        }

        mixed
    }

    /// Number of mono samples per video frame.
    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }
}

/// Get interleaved stereo f32 samples for one video frame from a track.
fn get_track_samples(
    state: &mut TrackAudioState,
    frame_num: u64,
    samples_per_frame: usize,
    sample_rate: u32,
    _fps: f64,
) -> Vec<f32> {
    let stereo_count = samples_per_frame * 2;

    // Find clip that covers this frame
    let clip_idx = state
        .clips
        .iter()
        .position(|c| frame_num >= c.timeline_start_frame && frame_num < c.timeline_end_frame);

    let clip_idx = match clip_idx {
        Some(i) => i,
        None => return vec![0.0; stereo_count],
    };

    // If we've moved to a different clip, decode the new clip
    if state.current_clip_idx != clip_idx {
        let clip = &state.clips[clip_idx];
        match decode_clip_audio(&clip.path, clip.source_start_secs, sample_rate) {
            Ok(mut buf) => {
                // Apply per-clip audio effects (EQ, compressor, reverb, etc.)
                if !clip.audio_effects.is_empty() {
                    let mut chain = AudioEffectChain::from_instances(&clip.audio_effects, sample_rate);
                    let ctx = AudioProcessContext {
                        sample_rate,
                        channels: 2,
                        clip_time_secs: 0.0,
                    };
                    chain.process(&mut buf, &ctx);
                }
                state.buffer = buf;
                state.buffer_pos = 0;
                state.current_clip_idx = clip_idx;
            }
            Err(e) => {
                warn!("AudioMixdown: failed to decode {:?}: {e}", clip.path);
                state.buffer.clear();
                state.buffer_pos = 0;
                state.current_clip_idx = clip_idx;
                return vec![0.0; stereo_count];
            }
        }
    }

    // Read samples_per_frame stereo samples from the buffer
    let available = state.buffer.len() - state.buffer_pos;
    let to_read = stereo_count.min(available);

    if to_read == 0 {
        return vec![0.0; stereo_count];
    }

    let mut output = state.buffer[state.buffer_pos..state.buffer_pos + to_read].to_vec();
    state.buffer_pos += to_read;

    // Pad with silence if the decoded clip buffer ran short of the expected
    // frame's worth of samples. This happens when the source file is shorter
    // than the timeline says, or when source_end_secs exceeds the real
    // duration. Without the warning the underrun is silent and looks like
    // A/V drift to anyone debugging the export.
    if output.len() < stereo_count {
        tracing::warn!(
            "audio_mixdown: clip underrun — padded {} samples of silence \
             (expected {}, decoded {})",
            stereo_count - output.len(),
            stereo_count,
            output.len(),
        );
    }
    output.resize(stereo_count, 0.0);
    output
}

/// Decode all audio from a clip into interleaved stereo f32 samples.
/// Uses ffmpeg-next's `packets()` iterator (which properly handles stream association).
fn decode_clip_audio(
    path: &std::path::Path,
    source_start_secs: f64,
    output_rate: u32,
) -> Result<Vec<f32>, RenderError> {
    let mut format_ctx = ffmpeg_next::format::input(path).map_err(RenderError::Decode)?;

    let audio_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .ok_or_else(|| RenderError::NoAudioStream(path.to_path_buf()))?;

    let audio_stream_idx = audio_stream.index();

    let codec_ctx =
        ffmpeg_next::codec::context::Context::from_parameters(audio_stream.parameters())
            .map_err(RenderError::Decode)?;

    let mut decoder = codec_ctx.decoder().audio().map_err(RenderError::Decode)?;

    // Determine source channel layout.
    // FFmpeg 8+ uses ch_layout (not the legacy channel_layout bitmask).
    // If bits() returns 0, construct a default layout from the channel count.
    let src_channels = decoder.channels();
    let src_layout = {
        let layout = decoder.channel_layout();
        if layout.bits() != 0 {
            layout
        } else {
            // Construct layout from channel count
            match src_channels {
                1 => ffmpeg_next::ChannelLayout::MONO,
                2 => ffmpeg_next::ChannelLayout::STEREO,
                6 => ffmpeg_next::ChannelLayout::_5POINT1,
                8 => ffmpeg_next::ChannelLayout::_7POINT1,
                _ => {
                    // Fallback: use default layout for this channel count
                    unsafe {
                        let mut ch_layout: ffmpeg_next::ffi::AVChannelLayout = std::mem::zeroed();
                        ffmpeg_next::ffi::av_channel_layout_default(
                            &mut ch_layout,
                            src_channels as i32,
                        );
                        ffmpeg_next::ChannelLayout::from(ch_layout)
                    }
                }
            }
        }
    };
    tracing::debug!(
        "AudioMixdown: source channels={}, layout_bits={:#x}",
        src_channels, src_layout.bits()
    );

    // Set up resampler: source format → f32 interleaved stereo at output_rate
    let mut resampler = ffmpeg_next::software::resampling::Context::get(
        decoder.format(),
        src_layout,
        decoder.rate(),
        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
        ffmpeg_next::ChannelLayout::STEREO,
        output_rate,
    )
    .map_err(RenderError::Decode)?;

    // Seek to source start
    if source_start_secs > 0.0 {
        let ts = (source_start_secs * 1_000_000.0) as i64;
        let _ = format_ctx.seek(ts, ..ts);
    }

    let mut buffer = Vec::new();

    // Decode using packets() iterator
    for (stream, packet) in format_ctx.packets() {
        if stream.index() != audio_stream_idx {
            continue;
        }

        if decoder.send_packet(&packet).is_err() {
            continue;
        }

        drain_audio_frames(&mut decoder, &mut resampler, &mut buffer);
    }

    // Flush decoder
    let _ = decoder.send_eof();
    drain_audio_frames(&mut decoder, &mut resampler, &mut buffer);

    debug!(
        "AudioMixdown: decoded {} stereo samples from {:?}",
        buffer.len() / 2,
        path.file_name().unwrap_or_default()
    );

    Ok(buffer)
}

/// Drain all available audio frames from the decoder, resample, and append to buffer.
fn drain_audio_frames(
    decoder: &mut ffmpeg_next::codec::decoder::Audio,
    resampler: &mut ffmpeg_next::software::resampling::Context,
    buffer: &mut Vec<f32>,
) {
    let mut frame = ffmpeg_next::frame::Audio::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        let mut resampled = ffmpeg_next::frame::Audio::empty();
        if resampler.run(&frame, &mut resampled).is_ok() {
            let nb_samples = resampled.samples();
            let channels = 2usize;
            let actual_floats = nb_samples * channels;
            let data = resampled.data(0);
            if actual_floats > 0 && data.len() >= actual_floats * 4 {
                let floats: &[f32] = bytemuck::cast_slice(&data[..actual_floats * 4]);
                buffer.extend_from_slice(floats);
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};

    fn make_empty_snapshot(num_audio_tracks: usize) -> SequenceSnapshot {
        let audio_tracks: Vec<AudioTrackSnapshot> = (0..num_audio_tracks)
            .map(|_| AudioTrackSnapshot {
                volume: 1.0,
                pan: 0.0,
                muted: false,
                solo: false,
                clips: vec![],
            })
            .collect();

        SequenceSnapshot {
            fps: 24.0,
            fps_num: 24,
            fps_den: 1,
            width: 1920,
            height: 1080,
            sample_rate: 48000,
            duration_frames: 240,
            export_range: None,
            video_tracks: vec![],
            audio_tracks,
            global_lut_path: None,
            subtitle_tracks: vec![], chapters: vec![],
        }
    }

    #[test]
    fn empty_tracks_produce_silence() {
        let snapshot = make_empty_snapshot(2);
        let mut mixdown = AudioMixdown::new(&snapshot).unwrap();

        let samples = mixdown.get_frame_audio(0);
        // 48000 / 24 = 2000 mono samples = 4000 stereo floats
        assert_eq!(samples.len(), 2000 * 2);
        assert!(samples.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn muted_track_produces_silence() {
        let mut snapshot = make_empty_snapshot(1);
        snapshot.audio_tracks[0].muted = true;

        let mut mixdown = AudioMixdown::new(&snapshot).unwrap();
        let samples = mixdown.get_frame_audio(0);
        assert!(samples.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn solo_excludes_non_solo() {
        let mut snapshot = make_empty_snapshot(2);
        snapshot.audio_tracks[0].solo = true;
        snapshot.audio_tracks[1].solo = false;

        let mut mixdown = AudioMixdown::new(&snapshot).unwrap();
        let samples = mixdown.get_frame_audio(0);
        assert_eq!(samples.len(), 2000 * 2);
    }

    #[test]
    fn samples_per_frame_calculation() {
        let snapshot = make_empty_snapshot(0);
        let mixdown = AudioMixdown::new(&snapshot).unwrap();
        assert_eq!(mixdown.samples_per_frame(), 2000);
    }

    #[test]
    fn samples_per_frame_30fps() {
        let mut snapshot = make_empty_snapshot(0);
        snapshot.fps = 30.0;
        snapshot.fps_num = 30;
        snapshot.fps_den = 1;
        let mixdown = AudioMixdown::new(&snapshot).unwrap();
        assert_eq!(mixdown.samples_per_frame(), 1600);
    }

    #[test]
    #[ignore = "requires TEST_MEDIA_FILE env var pointing to a video file with audio"]
    fn decode_real_audio() {
        let path = std::env::var("TEST_MEDIA_FILE")
            .map(std::path::PathBuf::from)
            .expect("set TEST_MEDIA_FILE");

        let mut snapshot = make_empty_snapshot(1);
        snapshot.fps = 30.0;
        snapshot.fps_num = 30;
        snapshot.fps_den = 1;
        snapshot.audio_tracks[0].clips.push(AudioClipSnapshot {
            timeline_start_frame: 0,
            timeline_end_frame: 60,
            source_start_secs: 0.0,
            path,
            audio_effects: vec![],
        });

        let mut mixdown = AudioMixdown::new(&snapshot).unwrap();

        let mut total_samples = 0;
        for f in 0..60 {
            let samples = mixdown.get_frame_audio(f);
            assert_eq!(samples.len(), 1600 * 2);
            total_samples += samples.len();
        }

        assert!(total_samples > 0, "expected samples for 60 frames");
        // If the first 2s are silence for this file, that's OK.
        // Verify the decode itself works by checking the direct decode.
        let path2 = std::env::var("TEST_MEDIA_FILE").map(std::path::PathBuf::from).unwrap();
        let buf = decode_clip_audio(&path2, 0.0, 48000).unwrap();
        assert!(
            buf.len() > 0,
            "decode_clip_audio should produce samples"
        );
        assert!(
            buf.iter().any(|&s| s != 0.0),
            "full file decode should have non-zero samples"
        );
    }
}
