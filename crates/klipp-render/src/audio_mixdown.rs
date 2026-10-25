use crate::error::RenderError;
use crate::snapshot::{AudioClipSnapshot, AudioClipShape, SequenceSnapshot};
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
    /// The export frame rate as a ratio. Frame `n` starts at sample
    /// `n * sample_rate * fps_den / fps_num`, computed exactly — a
    /// rounded "samples per frame" would put NTSC audio 0.4 samples
    /// further behind the picture every frame, a quarter second by the
    /// end of a ten-minute render.
    fps_num: u64,
    fps_den: u64,
    /// How a pan position becomes left and right gains — the project's
    /// law, so the render mixes the way playback does.
    pan_law: klipp_core::PanLaw,
}

struct TrackSettings {
    volume: f32,
    pan: f32,
    muted: bool,
    solo: bool,
    /// The track's automation (#2306), evaluated as playback evaluates it.
    automation: klipp_timeline::TrackAutomation,
}

/// Per-track state: the clips and the decoded sound of those playing.
struct TrackAudioState {
    clips: Vec<AudioClipSnapshot>,
    /// Decoded audio (interleaved stereo f32) of the clips sounding now,
    /// by index into `clips`, each starting at the clip's source start.
    /// Usually one; two inside a crossfade, where the outgoing clip's
    /// tail and the incoming clip's head overlap. A frame reads from each
    /// at the offset its timeline position implies, never "wherever the
    /// last read stopped": a ranged export begins mid-clip, and the old
    /// read cursor started every clip from its first sample.
    buffers: Vec<(usize, Vec<f32>)>,
}

impl AudioMixdown {
    /// Create from a sequence snapshot.
    pub fn new(snapshot: &SequenceSnapshot) -> Result<Self, RenderError> {
        let sample_rate = snapshot.sample_rate;
        let fps = snapshot.fps;

        let any_solo = snapshot.audio_tracks.iter().any(|t| t.solo);

        let mut track_decoders = Vec::with_capacity(snapshot.audio_tracks.len());
        let mut track_settings = Vec::with_capacity(snapshot.audio_tracks.len());

        for track in &snapshot.audio_tracks {
            track_decoders.push(TrackAudioState {
                clips: track.clips.clone(),
                buffers: Vec::new(),
            });
            track_settings.push(TrackSettings {
                volume: track.volume,
                pan: track.pan,
                muted: track.muted,
                solo: track.solo,
                automation: track.automation.clone(),
            });
        }

        debug!(
            "AudioMixdown: {} tracks, {}Hz, {:.3}fps ({}/{})",
            track_decoders.len(),
            sample_rate,
            fps,
            snapshot.fps_num,
            snapshot.fps_den,
        );

        Ok(Self {
            track_decoders,
            track_settings,
            any_solo,
            sample_rate,
            fps_num: snapshot.fps_num.max(1) as u64,
            fps_den: snapshot.fps_den.max(1) as u64,
            pan_law: snapshot.pan_law,
        })
    }

    /// Get the interleaved stereo f32 audio samples for one video frame:
    /// the samples between the frame's start and the next frame's start
    /// (see [`Self::frame_sample_range`]), clamped to `[-1.0, 1.0]` so
    /// the encoder gets legal values. Frames may be called in any order;
    /// each is placed by its timeline position.
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
        let (first, end) = self.frame_sample_range(frame_num);
        let stereo_samples = (end - first) as usize * 2;
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
                first,
                end,
                self.sample_rate,
                self.fps_num,
                self.fps_den,
                self.fps_num as f64 / self.fps_den as f64,
            );

            if track_samples.is_empty() {
                continue;
            }

            // Volume and pan, by the project's pan law — the same
            // `PanLaw::gains` playback uses.
            //
            // This used to be the constant-power law, written out here.
            // That is 0.707 at centre, and the default law is Linear,
            // which is unity — so every centred stereo track came out
            // of a render 3 dB below what it sounded like in the viewer,
            // and the user's choice of law was ignored entirely. An
            // equal-power law is for panning a *mono* source; a stereo
            // track at centre has to pass through untouched.
            let (gain_l, gain_r) = self.pan_law.gains(settings.pan);
            let pan_l = gain_l * settings.volume;
            let pan_r = gain_r * settings.volume;
            // Automated (#2306): the track's volume and pan at the frame's
            // first sample and at the next frame's, by the function the
            // playback mixer is fed by, ramped across the frame's samples.
            let automated = settings.automation.plays(klipp_timeline::TrackParam::Volume) || settings.automation.plays(klipp_timeline::TrackParam::Pan);
            let ramp = automated.then(|| {
                let at = |sample: u64| automated_gains(settings, self.pan_law, sample as f64 / self.sample_rate as f64);
                (at(first), at(end))
            });

            let sample_count = track_samples.len().min(stereo_samples);
            let frames = (stereo_samples / 2).max(1) as f32;
            for i in (0..sample_count).step_by(2) {
                let l = track_samples[i];
                let r = if i + 1 < track_samples.len() {
                    track_samples[i + 1]
                } else {
                    l
                };
                let (pan_l, pan_r) = match ramp {
                    Some(((l0, r0), (l1, r1))) => {
                        let t = (i / 2) as f32 / frames;
                        (l0 + (l1 - l0) * t, r0 + (r1 - r0) * t)
                    }
                    None => (pan_l, pan_r),
                };
                mixed[i] += l * pan_l;
                if i + 1 < stereo_samples {
                    mixed[i + 1] += r * pan_r;
                }
            }
        }

        mixed
    }

    /// The mono sample index at which frame `frame_num` starts, and the
    /// one at which the next frame starts: `[first, end)`. Exact, so the
    /// counts of consecutive frames sum to the true sample position and
    /// audio cannot drift against the picture.
    pub fn frame_sample_range(&self, frame_num: u64) -> (u64, u64) {
        (
            first_sample_of_frame(frame_num, self.sample_rate, self.fps_num, self.fps_den),
            first_sample_of_frame(frame_num + 1, self.sample_rate, self.fps_num, self.fps_den),
        )
    }
}

/// A track's left and right gains at `secs` from its automation (#2306):
/// its automated volume times its pan law's gains at its automated pan,
/// as the playback mixer multiplies them.
fn automated_gains(settings: &TrackSettings, law: klipp_core::PanLaw, secs: f64) -> (f32, f32) {
    let volume = settings.automation.value_at(klipp_timeline::TrackParam::Volume, secs, settings.volume);
    let (l, r) = law.gains(settings.automation.value_at(klipp_timeline::TrackParam::Pan, secs, settings.pan));
    (l * volume, r * volume)
}

/// `floor(frame * sample_rate / fps)` in integer arithmetic.
fn first_sample_of_frame(frame: u64, sample_rate: u32, fps_num: u64, fps_den: u64) -> u64 {
    (frame as u128 * sample_rate as u128 * fps_den as u128 / fps_num as u128) as u64
}

/// The samples a clip sounds over on the timeline, first to past its
/// last: from its exact times when it has them, by the same rounding a
/// frame's first sample takes, so two clips meeting at a cut meet on one
/// sample; else from its frames.
fn clip_samples(clip: &AudioClipSnapshot, sample_rate: u32, fps_num: u64, fps_den: u64) -> (u64, u64) {
    match clip.exact_secs {
        Some((a, b)) => {
            let at = |s: f64| (s * sample_rate as f64 + 1e-6).floor().max(0.0) as u64;
            (at(a), at(b))
        }
        None => (
            first_sample_of_frame(clip.timeline_start_frame, sample_rate, fps_num, fps_den),
            first_sample_of_frame(clip.timeline_end_frame, sample_rate, fps_num, fps_den),
        ),
    }
}

/// Get interleaved stereo f32 samples for one video frame from a track.
/// `first..end` is the frame's mono sample range on the timeline.
#[allow(clippy::too_many_arguments)]
fn get_track_samples(
    state: &mut TrackAudioState,
    frame_num: u64,
    first: u64,
    end: u64,
    sample_rate: u32,
    fps_num: u64,
    fps_den: u64,
    fps: f64,
) -> Vec<f32> {
    let stereo_count = (end - first) as usize * 2;

    // The clips that cover this frame: one, or two where a crossfade
    // overlaps the outgoing clip's tail with the incoming clip's head.
    let covering: Vec<usize> = state
        .clips
        .iter()
        .enumerate()
        .filter(|(_, c)| frame_num >= c.timeline_start_frame && frame_num < c.timeline_end_frame)
        .map(|(i, _)| i)
        .collect();
    if covering.is_empty() {
        state.buffers.clear();
        return vec![0.0; stereo_count];
    }
    // Keep the sound of the clips still playing, decode the ones starting.
    state.buffers.retain(|(i, _)| covering.contains(i));
    for &clip_idx in &covering {
        if state.buffers.iter().any(|(i, _)| *i == clip_idx) {
            continue;
        }
        let clip = &state.clips[clip_idx];
        // The clip's effects, made first: how far they look ahead says how
        // far past the clip's end its sound must be read.
        let mut chain = (!clip.audio_effects.is_empty()).then(|| AudioEffectChain::from_instances_with_kfs(&clip.audio_effects, &clip.audio_effect_keyframes, sample_rate));
        let latency_secs = chain.as_ref().map_or(0.0, |c| c.total_latency_samples(sample_rate) as f64 / sample_rate as f64);
        let until = decode_until(clip, fps, latency_secs);
        let buf = match decode_clip_audio(&clip.path, clip.source_start_secs, until, sample_rate) {
            Ok(mut buf) => {
                // A retimed clip (#642): its sound read at its speed, from
                // where each moment of it on the timeline is in the source,
                // as playback's feeder reads it; then the buffer is the
                // clip's own time from its first sample, as at 1x.
                if let Some(v) = clip.varispeed {
                    let (start, end) = clip.span_secs(fps);
                    let want = ((end - start).max(0.0) * sample_rate as f64).ceil() as usize + 1;
                    let from = (v.source_at(0.0) - clip.source_start_secs) * sample_rate as f64;
                    buf = if v.keep_pitch && v.speed > 0.0 {
                        // On playback's grain grid: anchored at the clip's
                        // in, its sound starting `head_secs` before it.
                        let anchor = (v.source_in - clip.source_start_secs) * sample_rate as f64;
                        klipp_audio::dsp::stretch::Stretcher::new(anchor, v.speed).read(&buf, 2, (-v.head_secs * sample_rate as f64).round() as i64, want)
                    } else {
                        klipp_audio::dsp::varispeed::read(&buf, 2, from, v.speed, want)
                    };
                }
                // Apply per-clip audio effects (EQ, compressor, reverb, etc.).
                // `decode_clip_audio` is called with `source_start_secs`, so the
                // returned buffer's first sample IS clip-relative t=0. The chain
                // sub-chunks internally and walks `clip_time_secs` forward
                // through the buffer when keyframes are present, so a single
                // call covers the whole clip.
                if let Some(chain) = chain.as_mut() {
                    let ctx = AudioProcessContext {
                        sample_rate,
                        channels: 2,
                        clip_time_secs: 0.0,
                    };
                    // Effects that look ahead (STFT denoiser, pitch shift)
                    // delay their output. Export has the whole clip, so
                    // flush the delay with silence and drop the head to
                    // keep the sound on the picture.
                    let latency = chain.total_latency_samples(sample_rate) as usize * 2;
                    buf.resize(buf.len() + latency, 0.0);
                    chain.process(&mut buf, &ctx);
                    buf.drain(..latency);
                }
                buf
            }
            Err(e) => {
                warn!("AudioMixdown: failed to decode {:?}: {e}", clip.path);
                Vec::new()
            }
        };
        state.buffers.push((clip_idx, buf));
    }

    let mut mixed = vec![0.0f32; stereo_count];
    for (clip_idx, buffer) in &state.buffers {
        let clip = &state.clips[*clip_idx];
        // The frame's place in the clip: how far past the clip's start on
        // the timeline it begins, in samples. The buffer starts at the
        // clip's source start, which is what plays at that timeline start.
        // A clip placed by the sample starts, and ends, inside its first
        // and last frames: silent there before and after it (#1588).
        let (clip_first, clip_end) = clip_samples(clip, sample_rate, fps_num, fps_den);
        let from = first.max(clip_first);
        let until = end.min(clip_end);
        if until <= from {
            continue;
        }
        let lead = (from - first) as usize * 2;
        let want = (until - from) as usize * 2;
        let offset = (from - clip_first) as usize * 2;
        let available = buffer.len().saturating_sub(offset);
        let to_read = want.min(available);
        if to_read == 0 {
            continue;
        }
        let mut output = buffer[offset..offset + to_read].to_vec();

        // Pad with silence if the decoded clip buffer ran short of the expected
        // frame's worth of samples. This happens when the source file is shorter
        // than the timeline says, or when source_end_secs exceeds the real
        // duration. Without the warning the underrun is silent and looks like
        // A/V drift to anyone debugging the export.
        if output.len() < want {
            tracing::warn!(
                "audio_mixdown: clip underrun — padded {} samples of silence \
                 (expected {}, decoded {})",
                want - output.len(),
                want,
                output.len(),
            );
        }
        output.resize(want, 0.0);
        // The clip's own settings, as playback applies them: this frame's
        // volume, gain and pan from the keyframes, the fade envelope per
        // sample, then the channel layout.
        let clip_pos_secs = (from - clip_first) as f64 / sample_rate as f64;
        let timeline_secs = frame_num as f64 * fps_den as f64 / fps_num as f64;
        shape_samples(&mut output, &clip.shape, clip_pos_secs, timeline_secs, fps, sample_rate);
        for (m, o) in mixed[lead..].iter_mut().zip(&output) {
            *m += o;
        }
    }
    mixed
}

/// A clip's volume, gain and pan at `clip_pos` seconds into it.
fn levels_at(shape: &AudioClipShape, clip_pos: f64, timeline_secs: f64, fps: f64) -> (f32, f32) {
    let ctx = klipp_core::EvalContext {
        clip_time_secs: clip_pos,
        clip_duration_secs: shape.clip_duration_secs,
        timeline_time_secs: timeline_secs,
        fps,
        seed: shape.seed,
    };
    let eval = |kfs: &[klipp_core::Keyframe], p: klipp_core::ParamId, base: f32| {
        klipp_compose::eval::eval_with_modifiers(kfs, &shape.param_modifiers, &p, clip_pos, base, &ctx)
    };
    let volume = eval(&shape.volume_keyframes, klipp_core::ParamId::Volume, shape.volume);
    let gain = eval(&shape.gain_keyframes, klipp_core::ParamId::Gain, shape.gain);
    let pan = eval(&shape.pan_keyframes, klipp_core::ParamId::Pan, shape.pan);
    (volume * gain, pan)
}

/// A curve over a `0..=1` ramp, shared with the playback feeder
/// through [`klipp_core::FadeCurve`] rather than transcribed here.
fn fade_curve(t: f64, curve: u8) -> f64 {
    klipp_core::FadeCurve::from_index(curve as u32).gain(t)
}

/// The two-millisecond ramp playback puts at a clip's start so a cut
/// into sound does not click. Its end is left alone, as playback leaves
/// it: the next clip carries on.
const ANTI_CLICK_SECS: f64 = 0.002;

/// The fade envelope at `pos` seconds into a clip of `dur` seconds, as
/// the playback feeder computes it.
fn fade_envelope(pos: f64, dur: f64, fade_in: f64, fade_out: f64, in_curve: u8, out_curve: u8) -> f32 {
    let mut g = 1.0;
    let eff_in = fade_in.max(ANTI_CLICK_SECS);
    if pos < eff_in {
        let t = (pos / eff_in).clamp(0.0, 1.0);
        g = fade_curve(t, if fade_in > 0.001 { in_curve } else { 0 });
    }
    if fade_out > 0.001 && pos > dur - fade_out {
        let t = ((dur - pos) / fade_out).clamp(0.0, 1.0);
        g *= fade_curve(t, out_curve);
    }
    g as f32
}

/// The crossfade gain `pos` seconds into a clip extended to `ext_dur`
/// seconds by its crossfades' handles: constant power in over the first
/// `xfade_in` seconds and out over the last `xfade_out`, so the outgoing
/// and incoming gains' squares sum to one through the whole window.
pub(crate) fn crossfade_gain(pos: f64, ext_dur: f64, xfade_in: f64, xfade_out: f64) -> f32 {
    let cp = |t: f64| klipp_core::FadeCurve::ConstantPower.gain(t.clamp(0.0, 1.0));
    let mut g = 1.0;
    if xfade_in > 0.0 && pos < xfade_in {
        g *= cp(pos / xfade_in);
    }
    if xfade_out > 0.0 && pos > ext_dur - xfade_out {
        g *= cp((ext_dur - pos) / xfade_out);
    }
    g as f32
}

/// [`fade_envelope`] without the fade in or its anti-click ramp.
fn fade_out_only(pos: f64, dur: f64, fade_out: f64, out_curve: u8) -> f32 {
    if fade_out > 0.001 && pos > dur - fade_out {
        fade_curve(((dur - pos) / fade_out).clamp(0.0, 1.0), out_curve) as f32
    } else {
        1.0
    }
}

/// How long a multicam's sound takes to hand over from one angle to the
/// next at a cut: the outgoing gate falls as the incoming one rises, so a
/// cut is neither a click nor a dip.
const HANDOVER_SECS: f64 = 0.005;

/// A multicam angle's gate `pos` seconds into a clip of `dur` seconds,
/// heard over `spans` (see `AudioClipShape::heard`): 1 inside a span, 0
/// outside, a straight ramp across each cut. A span's edge at the clip's
/// own ends is left to the clip's fades.
fn heard_gain(spans: &[(f64, f64)], pos: f64, dur: f64) -> f32 {
    let half = HANDOVER_SECS / 2.0;
    let rise = |edge: f64| if edge <= 0.0 { 1.0 } else { ((pos - (edge - half)) / HANDOVER_SECS).clamp(0.0, 1.0) };
    let fall = |edge: f64| if edge >= dur { 1.0 } else { (((edge + half) - pos) / HANDOVER_SECS).clamp(0.0, 1.0) };
    spans.iter().map(|&(a, b)| rise(a).min(fall(b))).sum::<f64>().min(1.0) as f32
}

/// Apply `shape` to `samples`, interleaved stereo starting `clip_pos`
/// seconds into the clip at timeline time `timeline_secs`. `clip_pos`
/// counts from the clip's extended start when a crossfade gives it a
/// head: keyframes and the clip's own fades are placed by its in point.
fn shape_samples(samples: &mut [f32], shape: &AudioClipShape, clip_pos_ext: f64, timeline_secs: f64, fps: f64, sample_rate: u32) {
    let head = shape.head_secs;
    let clip_pos = clip_pos_ext - head;
    let crossfading = shape.crossfade_in_secs > 0.0 || shape.crossfade_out_secs > 0.0;
    let ext_dur = head + shape.clip_duration_secs + shape.tail_secs;
    // The levels at the frame's first sample and at the next frame's,
    // and a straight line between: keyframes move a level across a
    // frame, and holding it for the frame turned a duck into a staircase
    // of steps a frame long, each one a click.
    let frame_secs = (samples.len() / 2) as f64 / sample_rate as f64;
    let (vol, pan) = levels_at(shape, clip_pos, timeline_secs, fps);
    let animated = !shape.volume_keyframes.is_empty() || !shape.gain_keyframes.is_empty() || !shape.pan_keyframes.is_empty() || !shape.param_modifiers.is_empty();
    let (vol_end, pan_end) = if animated { levels_at(shape, clip_pos + frame_secs, timeline_secs + frame_secs, fps) } else { (vol, pan) };
    let moving = (vol_end - vol).abs() > 1e-6 || (pan_end - pan).abs() > 1e-6;
    let has_fade = crossfading || shape.heard.is_some() || shape.fade_in_secs > 0.001 || shape.fade_out_secs > 0.001 || clip_pos < ANTI_CLICK_SECS;
    if !has_fade && !moving && (vol - 1.0).abs() < 1e-6 && pan.abs() < 1e-6 && shape.channel_config == 0 {
        return;
    }
    // Balance, as the output applies a clip's pan: centre is unity and
    // a side attenuates the other.
    let balance = |pan: f32| (if pan <= 0.0 { 1.0 } else { 1.0 - pan }, if pan >= 0.0 { 1.0 } else { 1.0 + pan });
    let n = (samples.len() / 2).max(1) as f32;
    let dur = shape.clip_duration_secs;
    for (i, pair) in samples.chunks_mut(2).enumerate() {
        let fade = if has_fade {
            let pos = clip_pos + i as f64 / sample_rate as f64;
            let own = if shape.crossfade_in_secs > 0.0 {
                // Fading in from silence already: no click to ramp out,
                // and the anti-click ramp at the in point would dip the
                // middle of the crossfade.
                fade_out_only(pos, dur, shape.fade_out_secs as f64, shape.fade_out_curve)
            } else {
                fade_envelope(pos, dur, shape.fade_in_secs as f64, shape.fade_out_secs as f64, shape.fade_in_curve, shape.fade_out_curve)
            };
            let own = match &shape.heard {
                Some(spans) => own * heard_gain(spans, pos, dur),
                None => own,
            };
            if crossfading {
                own * crossfade_gain(pos + head, ext_dur, shape.crossfade_in_secs, shape.crossfade_out_secs)
            } else {
                own
            }
        } else {
            1.0
        };
        let t = if moving { i as f32 / n } else { 0.0 };
        let m = (vol + (vol_end - vol) * t) * fade;
        let (pan_l, pan_r) = balance(pan + (pan_end - pan) * t);
        if let [l, r] = pair {
            *l *= m * pan_l;
            *r *= m * pan_r;
            match shape.channel_config {
                1 => {
                    let mono = (*l + *r) * 0.5;
                    *l = mono;
                    *r = mono;
                }
                2 => std::mem::swap(l, r),
                // One side of a stereo clip split to mono (#922).
                5 => *r = *l,
                6 => *l = *r,
                _ => {}
            }
        } else if let [l] = pair {
            *l *= m;
        }
    }
}

/// Read past a clip's last source second by this much, at its speed: room
/// for a kept-pitch stretcher's grain and the resampler's filter, so the
/// clip's last sample sounds as it did when the whole file was read.
const DECODE_MARGIN_SECS: f64 = 0.5;

/// The source second a clip's decode can stop at (#1594): the end of what
/// it plays (its timeline range, crossfade handles included, read at its
/// speed), its effects' look-ahead, and [`DECODE_MARGIN_SECS`]. `None`, to
/// read to the end of the file, for a speed with no end to read to.
fn decode_until(clip: &AudioClipSnapshot, fps: f64, latency_secs: f64) -> Option<f64> {
    let (start, end) = clip.span_secs(fps);
    let secs = (end - start).max(0.0);
    let (end, speed) = match clip.varispeed {
        None => (clip.source_start_secs + secs, 1.0),
        Some(v) if v.speed.is_finite() && v.speed != 0.0 => (v.source_at(0.0).max(v.source_at(secs)), v.speed.abs()),
        Some(_) => return None,
    };
    (fps > 0.0 && end.is_finite()).then_some(end + (latency_secs + DECODE_MARGIN_SECS) * speed)
}

/// Decode a clip's audio from `source_start_secs` into interleaved stereo
/// f32 samples, stopping once `until` (a source second) is decoded, or at
/// the end of the file: a clip holds what it plays, not the rest of its
/// file (#1594: 120 MB for ten seconds of a five-minute track).
/// Uses ffmpeg-next's `packets()` iterator (which properly handles stream association).
pub(crate) fn decode_clip_audio(
    path: &std::path::Path,
    source_start_secs: f64,
    until: Option<f64>,
    output_rate: u32,
) -> Result<Vec<f32>, RenderError> {
    let mut format_ctx = ffmpeg_next::format::input(path).map_err(RenderError::Decode)?;

    let audio_stream = format_ctx
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .ok_or_else(|| RenderError::NoAudioStream(path.to_path_buf()))?;

    let audio_stream_idx = audio_stream.index();
    let stream_time_base = audio_stream.time_base();

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
                // Fallback: the default layout for this channel count.
                // ffmpeg-next's layout is a bit mask before ffmpeg 7, so
                // the constructor both forms have, not an AVChannelLayout.
                _ => ffmpeg_next::ChannelLayout::default(src_channels as i32),
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

    // Seek to source start. This lands on the last packet at or before
    // it — for AAC, FLAC or Opus that can be tens of milliseconds early
    // — so the first decoded frame's timestamp says where the buffer
    // really begins, and the lead-in is trimmed below.
    if source_start_secs > 0.0 {
        let ts = (source_start_secs * 1_000_000.0) as i64;
        let _ = format_ctx.seek(ts, ..ts);
    }

    let mut buffer = Vec::new();
    let mut first_pts: Option<i64> = None;

    // Decode using packets() iterator
    for (stream, packet) in format_ctx.packets() {
        if stream.index() != audio_stream_idx {
            continue;
        }

        if decoder.send_packet(&packet).is_err() {
            continue;
        }

        drain_audio_frames(&mut decoder, &mut resampler, src_layout, &mut buffer, &mut first_pts);
        // Read as far as the clip plays: the buffer starts at the first
        // decoded frame's time, which the seek put at or before the in
        // point.
        if let (Some(until), Some(pts)) = (until, first_pts) {
            let start = pts as f64 * f64::from(stream_time_base);
            if start + (buffer.len() / 2) as f64 / output_rate as f64 >= until {
                break;
            }
        }
    }

    // Flush decoder
    let _ = decoder.send_eof();
    drain_audio_frames(&mut decoder, &mut resampler, src_layout, &mut buffer, &mut first_pts);

    // Flush the resampler: its filter still holds the last few samples.
    loop {
        let mut flushed = stereo_frame(4096);
        if resampler.flush(&mut flushed).is_err() || !append_stereo(&flushed, &mut buffer) {
            break;
        }
    }

    // Line the buffer up so its first sample is `source_start_secs`:
    // drop what the seek delivered before the in point, or pad with
    // silence if it overshot. The viewer's decoder discards by PTS the
    // same way; without this the export played every trimmed clip from
    // up to a packet before its in point, and everything after that
    // came late by the same amount.
    if let Some(pts) = first_pts {
        let first_secs = pts as f64 * f64::from(stream_time_base);
        let lead = ((source_start_secs - first_secs) * output_rate as f64).round() as i64;
        let lead_floats = lead.unsigned_abs() as usize * 2;
        if lead > 0 {
            let n = lead_floats.min(buffer.len());
            buffer.drain(..n);
            debug!("AudioMixdown: trimmed {} samples of lead-in before {source_start_secs:.3}s", n / 2);
        } else if lead < 0 {
            buffer.splice(0..0, std::iter::repeat_n(0.0, lead_floats));
            debug!("AudioMixdown: seek overshot by {} samples, padded", lead_floats / 2);
        }
    }

    debug!(
        "AudioMixdown: decoded {} stereo samples from {:?}",
        buffer.len() / 2,
        path.file_name().unwrap_or_default()
    );

    Ok(buffer)
}

/// Drain all available audio frames from the decoder, resample, and
/// append to buffer. `first_pts` records the first decoded frame's
/// timestamp, in the stream's time base, so the caller knows where the
/// buffer starts.
fn drain_audio_frames(
    decoder: &mut ffmpeg_next::codec::decoder::Audio,
    resampler: &mut ffmpeg_next::software::resampling::Context,
    src_layout: ffmpeg_next::ChannelLayout,
    buffer: &mut Vec<f32>,
    first_pts: &mut Option<i64>,
) {
    let mut frame = ffmpeg_next::frame::Audio::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        if first_pts.is_none() {
            *first_pts = frame.pts().or_else(|| frame.timestamp());
        }
        // A PCM decoder's frames say how many channels they have and not
        // how they are laid out. The resampler was built with the layout
        // worked out above; a frame that names none reads to it as a
        // change of input, every frame is refused, and a WAV on the
        // timeline rendered as silence.
        if frame.channel_layout().bits() == 0 {
            frame.set_channel_layout(src_layout);
        }
        // Sized for what this frame resamples to. Given an empty frame,
        // ffmpeg-next allocates `input.samples()` output samples, which is
        // right only when the rates match: upsampled (24 kHz narration to
        // 48 kHz) each call gave back that many and the resampler kept the
        // rest, so the export played the first half of the clip (#1564).
        // The + 256 is FFmpeg's own padding when it allocates.
        let (in_rate, out_rate) = (resampler.input().rate.max(1) as usize, resampler.output().rate as usize);
        let mut resampled = stereo_frame(frame.samples() * out_rate / in_rate + 256);
        if resampler.run(&frame, &mut resampled).is_ok() {
            append_stereo(&resampled, buffer);
        }
    }
}

/// An interleaved f32 stereo frame with room for `samples` samples.
fn stereo_frame(samples: usize) -> ffmpeg_next::frame::Audio {
    ffmpeg_next::frame::Audio::new(
        ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
        samples,
        ffmpeg_next::ChannelLayout::STEREO,
    )
}

/// Append a resampled stereo frame's samples (its count, not its
/// buffer's padded length) to `buffer`; false when it held none.
fn append_stereo(frame: &ffmpeg_next::frame::Audio, buffer: &mut Vec<f32>) -> bool {
    let floats = frame.samples() * 2;
    let data = frame.data(0);
    if floats == 0 || data.len() < floats * 4 {
        return false;
    }
    buffer.extend_from_slice(bytemuck::cast_slice(&data[..floats * 4]));
    true
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot, VarispeedRead};

    /// #2306: export's gains for an automated track are playback's at the
    /// same time: the mixer, held at the volume and pan the engine sets
    /// from the automation there, gives the same samples.
    #[test]
    fn automated_gains_are_playbacks() {
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let mut automation = klipp_timeline::TrackAutomation::default();
        automation.lanes.insert(klipp_timeline::TrackParam::Volume, vec![key(0.0, 1.0), key(2.0, 0.1)]);
        automation.lanes.insert(klipp_timeline::TrackParam::Pan, vec![key(0.0, -0.5), key(2.0, 0.5)]);
        let settings = TrackSettings { volume: 0.7, pan: 0.0, muted: false, solo: false, automation: automation.clone() };
        let law = klipp_core::PanLaw::default();
        for secs in [0.0, 0.4, 1.0, 1.7, 3.0] {
            let mut mixer = klipp_audio::AudioMixer::new(1);
            mixer.pan_law = law;
            mixer.tracks[0].volume = automation.value_at(klipp_timeline::TrackParam::Volume, secs, 0.7);
            mixer.tracks[0].pan = automation.value_at(klipp_timeline::TrackParam::Pan, secs, 0.0);
            let one = vec![1.0f32; 2];
            let played = mixer.mix(&[(0, &one)]);
            let (l, r) = automated_gains(&settings, law, secs);
            assert_eq!((played[0].to_bits(), played[1].to_bits()), (l.to_bits(), r.to_bits()), "at {secs} s");
        }
    }

    fn make_empty_snapshot(num_audio_tracks: usize) -> SequenceSnapshot {
        let audio_tracks: Vec<AudioTrackSnapshot> = (0..num_audio_tracks)
            .map(|_| AudioTrackSnapshot {
                volume: 1.0,
                pan: 0.0,
                muted: false,
                solo: false,
                clips: vec![],
                automation: Default::default(),
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
            video: Default::default(),
            audio_tracks,
            pan_law: Default::default(),
            global_lut_path: None,
            subtitle_tracks: vec![], chapters: vec![],
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
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
        assert_eq!(mixdown.frame_sample_range(0), (0, 2000));
        assert_eq!(mixdown.frame_sample_range(7), (14000, 16000));
    }

    #[test]
    fn samples_per_frame_30fps() {
        let mut snapshot = make_empty_snapshot(0);
        snapshot.fps = 30.0;
        snapshot.fps_num = 30;
        snapshot.fps_den = 1;
        let mixdown = AudioMixdown::new(&snapshot).unwrap();
        assert_eq!(mixdown.frame_sample_range(0), (0, 1600));
    }

    /// 48000 / 29.97 is 1601.6 samples: a rounded 1602 per frame runs
    /// 0.4 samples ahead every frame. Over 100 s that is 25 ms of drift
    /// — a lip-sync error by the end of every NTSC export. The frames
    /// must tile the true sample count instead.
    #[test]
    fn ntsc_frames_tile_the_exact_sample_count() {
        let mut snapshot = make_empty_snapshot(1);
        snapshot.fps = 30000.0 / 1001.0;
        snapshot.fps_num = 30000;
        snapshot.fps_den = 1001;
        let mut mixdown = AudioMixdown::new(&snapshot).unwrap();
        let frames = 3000u64;
        let total: usize = (0..frames).map(|f| mixdown.get_frame_audio(f).len() / 2).sum();
        assert_eq!(total as u64, frames * 48000 * 1001 / 30000, "the frames' audio does not add up to the time they cover");
        let lengths: std::collections::BTreeSet<usize> = (0..frames).map(|f| mixdown.get_frame_audio(f).len() / 2).collect();
        assert_eq!(lengths, [1601, 1602].into_iter().collect());
    }

    /// A multicam's two angles hand over at a cut: silent outside their
    /// spans, whole inside, and across the cut the outgoing gate and the
    /// incoming one sum to one, so the cut neither clicks nor dips. Edges
    /// at the clip's own ends are not ramped.
    #[test]
    fn multicam_angles_hand_over_at_a_cut_without_a_dip() {
        let out = [(0.0, 2.0)];
        let inc = [(2.0, 10.0)];
        for pos in [1.9975, 1.999, 2.0, 2.001, 2.0025] {
            let sum = heard_gain(&out, pos, 10.0) + heard_gain(&inc, pos, 10.0);
            assert!((sum - 1.0).abs() < 1e-5, "{pos}: {sum}");
        }
        assert_eq!(heard_gain(&out, 1.0, 10.0), 1.0);
        assert_eq!(heard_gain(&out, 3.0, 10.0), 0.0);
        assert_eq!(heard_gain(&inc, 1.0, 10.0), 0.0);
        assert_eq!(heard_gain(&out, 0.0, 10.0), 1.0, "the clip's start is its own fade's");
        assert_eq!(heard_gain(&inc, 10.0, 10.0), 1.0, "and so is its end");
        // Through `shape_samples`: gated out between spans.
        let shape = AudioClipShape { clip_duration_secs: 10.0, heard: Some(vec![(0.0, 2.0), (5.0, 10.0)]), ..Default::default() };
        let mut s = vec![1.0f32; 8];
        shape_samples(&mut s, &shape, 3.0, 3.0, 25.0, 48000);
        assert!(s.iter().all(|v| *v == 0.0));
        let mut s = vec![1.0f32; 8];
        shape_samples(&mut s, &shape, 6.0, 6.0, 25.0, 48000);
        assert!(s.iter().all(|v| (*v - 1.0).abs() < 1e-6));
    }

    /// The clip's settings shape its samples as playback does: gain and
    /// volume multiply, pan is a balance, a fade-in ramps from the clip's
    /// start along its curve, a fade-out to its end, mono sums.
    #[test]
    fn a_clip_shapes_its_samples_as_playback_does() {
        let sr = 48000;
        let ones = |n: usize| vec![1.0f32; n * 2];

        // Volume and gain multiply; a full-right balance silences the left.
        let shape = AudioClipShape { volume: 0.5, gain: 2.0, pan: 1.0, clip_duration_secs: 10.0, ..Default::default() };
        let mut s = ones(4);
        shape_samples(&mut s, &shape, 1.0, 1.0, 25.0, sr);
        assert_eq!(&s[..4], &[0.0, 1.0, 0.0, 1.0]);

        // A one-second linear fade-in: half way through, half the level.
        let shape = AudioClipShape { fade_in_secs: 1.0, clip_duration_secs: 10.0, ..Default::default() };
        let mut s = ones(2);
        shape_samples(&mut s, &shape, 0.5, 0.5, 25.0, sr);
        assert!((s[0] - 0.5).abs() < 1e-3, "{}", s[0]);
        // An exponential fade-in is the square of the ramp.
        let shape = AudioClipShape { fade_in_secs: 1.0, fade_in_curve: 1, clip_duration_secs: 10.0, ..Default::default() };
        let mut s = ones(2);
        shape_samples(&mut s, &shape, 0.5, 0.5, 25.0, sr);
        assert!((s[0] - 0.25).abs() < 1e-3, "{}", s[0]);

        // A fade-out of two seconds on a ten-second clip: at nine seconds, half.
        let shape = AudioClipShape { fade_out_secs: 2.0, clip_duration_secs: 10.0, ..Default::default() };
        let mut s = ones(2);
        shape_samples(&mut s, &shape, 9.0, 9.0, 25.0, sr);
        assert!((s[0] - 0.5).abs() < 1e-3, "{}", s[0]);

        // At the very start, the anti-click ramp: the first sample is silent,
        // two milliseconds in it is full.
        let shape = AudioClipShape { clip_duration_secs: 10.0, ..Default::default() };
        let mut s = ones(200);
        shape_samples(&mut s, &shape, 0.0, 0.0, 25.0, sr);
        assert_eq!(s[0], 0.0);
        assert!((s[199 * 2] - 1.0).abs() < 1e-3, "{}", s[199 * 2]);

        // Mono downmix sums the sides after the balance.
        let shape = AudioClipShape { channel_config: 1, clip_duration_secs: 10.0, ..Default::default() };
        let mut s = vec![1.0f32, 0.0, 1.0, 0.0];
        shape_samples(&mut s, &shape, 1.0, 1.0, 25.0, sr);
        assert_eq!(&s[..], &[0.5, 0.5, 0.5, 0.5]);

        // Left only and right only, each side of a split clip (#922),
        // as playback mixes them.
        for (config, want) in [(5u8, [0.75f32, 0.75]), (6, [0.25, 0.25])] {
            let shape = AudioClipShape { channel_config: config, clip_duration_secs: 10.0, ..Default::default() };
            let mut s = vec![0.75f32, 0.25];
            shape_samples(&mut s, &shape, 1.0, 1.0, 25.0, sr);
            assert_eq!(&s[..], &want[..], "channel config {config}");
        }

        // Keyframed volume: from 1 at 0 s to 0 at 2 s, read at 1 s.
        let shape = AudioClipShape {
            volume_keyframes: {
                let mut kfs = Vec::new();
                klipp_core::keyframe::upsert_keyframe(&mut kfs, 0.0, 1.0);
                klipp_core::keyframe::upsert_keyframe(&mut kfs, 2.0, 0.0);
                kfs
            },
            clip_duration_secs: 10.0,
            ..Default::default()
        };
        let mut s = ones(2);
        shape_samples(&mut s, &shape, 1.0, 1.0, 25.0, sr);
        assert!((s[0] - 0.5).abs() < 1e-3, "{}", s[0]);
        // And it moves across the frame, not in a step at its start: a
        // tenth of a second on, the level is a twentieth lower, with
        // every sample between on the line.
        let mut s = ones(4800);
        shape_samples(&mut s, &shape, 1.0, 1.0, 25.0, sr);
        assert!((s[0] - 0.5).abs() < 1e-3 && (s[4799 * 2] - 0.45).abs() < 1e-3, "{} then {}", s[0], s[4799 * 2]);
        assert!((s[2400 * 2] - 0.475).abs() < 1e-3, "{}", s[2400 * 2]);
        assert!(s.chunks(2).collect::<Vec<_>>().windows(2).all(|w| w[1][0] <= w[0][0]), "never a step up on the way down");
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
            audio_effect_keyframes: std::collections::HashMap::new(),
            shape: AudioClipShape::default(),
            varispeed: None,
            exact_secs: None,
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
        let buf = decode_clip_audio(&path2, 0.0, None, 48000).unwrap();
        assert!(
            !buf.is_empty(),
            "decode_clip_audio should produce samples"
        );
        assert!(
            buf.iter().any(|&s| s != 0.0),
            "full file decode should have non-zero samples"
        );
    }

    /// #1594: a clip's decode stops where it stops playing, with room for
    /// its effects' look-ahead, instead of holding the rest of its file.
    #[test]
    fn a_clip_decodes_only_what_it_plays() {
        if !std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("klipp-decode-until-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone-9s.wav");
        let made = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:duration=9"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(made.success());
        let secs = |buf: &[f32]| buf.len() as f64 / 2.0 / 48000.0;
        let whole = decode_clip_audio(&path, 1.0, None, 48000).unwrap();
        assert!((secs(&whole) - 8.0).abs() < 0.05, "unbounded, to the end of the file: {}", secs(&whole));
        let bounded = decode_clip_audio(&path, 1.0, Some(4.0), 48000).unwrap();
        assert!(secs(&bounded) >= 3.0 && secs(&bounded) < 3.5, "from 1 s to 4 s and a packet: {}", secs(&bounded));
        assert_eq!(bounded[..2 * 48000], whole[..2 * 48000], "the same sound where both have it");
        let _ = std::fs::remove_dir_all(&dir);

        // Where a decode stops: a clip of 48 frames at 24 fps from source
        // 5 s plays to 7 s; effects that look ahead read further.
        let clip = |varispeed| AudioClipSnapshot {
            timeline_start_frame: 24,
            timeline_end_frame: 72,
            source_start_secs: 5.0,
            path: std::path::PathBuf::new(),
            audio_effects: vec![],
            audio_effect_keyframes: std::collections::HashMap::new(),
            shape: AudioClipShape::default(),
            varispeed,
            exact_secs: None,
        };
        assert_eq!(decode_until(&clip(None), 24.0, 0.0), Some(7.0 + DECODE_MARGIN_SECS));
        assert_eq!(decode_until(&clip(None), 24.0, 0.25), Some(7.25 + DECODE_MARGIN_SECS));
        // Played backward at 2x from its out point at 20 s: it reads 20 s
        // down to 16 s, so it stops after 20 s.
        let reversed = VarispeedRead { speed: -2.0, source_in: 16.0, source_out: 20.0, head_secs: 0.0, keep_pitch: false };
        assert_eq!(decode_until(&clip(Some(reversed)), 24.0, 0.0), Some(20.0 + 2.0 * DECODE_MARGIN_SECS));
        let stopped = VarispeedRead { speed: 0.0, ..reversed };
        assert_eq!(decode_until(&clip(Some(stopped)), 24.0, 0.0), None);
    }
}
