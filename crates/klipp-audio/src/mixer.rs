/// Per-track audio settings for the mixer.
#[derive(Debug, Clone)]
pub struct AudioTrackSettings {
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub peak_left: f32,
    pub peak_right: f32,
    /// RMS level (root mean square) for loudness display.
    pub rms_left: f32,
    pub rms_right: f32,
    /// Optional bus assignment (index into AudioMixer::buses). None = direct to master.
    pub bus: Option<usize>,
    /// The volume and pan the last block ended at (#2306): a change since
    /// is ramped across the next block, sample by sample, so an automated
    /// or moved control doesn't step (and click) at each block. `None`
    /// before the first block.
    pub ramp_from: Option<(f32, f32)>,
}

impl Default for AudioTrackSettings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            peak_left: 0.0,
            peak_right: 0.0,
            rms_left: 0.0,
            rms_right: 0.0,
            bus: None,
            ramp_from: None,
        }
    }
}

/// Audio submix bus with its own volume, pan, and mute controls.
#[derive(Debug, Clone)]
pub struct AudioBus {
    pub name: String,
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub peak_left: f32,
    pub peak_right: f32,
}

impl AudioBus {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            peak_left: 0.0,
            peak_right: 0.0,
        }
    }
}

/// A track's own effects (#2560): one chain run on the sum of the track's
/// clips, before its fader, so a track compressor or limiter acts on what
/// the track carries, once, as a mixer's inserts do. They were put on each
/// clip, ahead of its gain and fades and of the other clips on the track.
pub struct TrackInserts {
    /// Each effect's type and whether it is on: what a change of needs
    /// the chain made again rather than its settings changed in place.
    shape: Vec<(String, bool)>,
    chain: crate::dsp::AudioEffectChain,
}

/// Mixes multiple audio tracks into a single stereo output.
/// Supports optional submix buses: tracks can route to a bus, and buses route to master.
pub struct AudioMixer {
    pub tracks: Vec<AudioTrackSettings>,
    pub buses: Vec<AudioBus>,
    pub master_volume: f32,
    pub pan_law: klipp_core::PanLaw,
    /// Per-track total chain latency in stereo frames, advertised
    /// by the audio decoder when chains are built. The mixer uses
    /// these to size each track's [`crate::pdc::TrackDelayLine`] so
    /// fast tracks delay enough to align with the slowest. Length
    /// always tracks `tracks.len()`. v1: caller manages the values
    /// via [`Self::set_track_latency_samples`]; mix-thread wiring
    /// to feed real chain latencies is `PDC-NEXT-MIX-THREAD-WIRING`.
    pub track_latency_samples: Vec<u32>,
    /// Per-track delay lines applied by
    /// [`Self::apply_per_track_pdc_delay`] before the existing mix
    /// path runs. Sized lazily — a track without latency
    /// compensation never allocates a ring buffer.
    pub track_delay_lines: Vec<Option<crate::pdc::TrackDelayLine>>,
    /// Each track's own effects, run on its summed clips before its fader.
    pub track_inserts: Vec<Option<TrackInserts>>,
    /// The latency of each track's sources and of its inserts, in stereo
    /// frames: their sum is the track's in `track_latency_samples`.
    source_latency: Vec<u32>,
    insert_latency: Vec<u32>,
    /// The rate the inserts run at.
    pub sample_rate: u32,
}

impl AudioMixer {
    pub fn new(track_count: usize) -> Self {
        Self {
            tracks: vec![AudioTrackSettings::default(); track_count],
            buses: Vec::new(),
            master_volume: 1.0,
            pan_law: klipp_core::PanLaw::default(),
            track_latency_samples: vec![0; track_count],
            track_delay_lines: (0..track_count).map(|_| None).collect(),
            track_inserts: (0..track_count).map(|_| None).collect(),
            source_latency: vec![0; track_count],
            insert_latency: vec![0; track_count],
            sample_rate: 48_000,
        }
    }

    /// The rate the inserts run at; chains made at another are made again
    /// at their next [`Self::set_track_inserts`].
    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.track_inserts.iter_mut().for_each(|i| *i = None);
        }
    }

    /// Track `track_idx`'s own effects as `instances` say: the chain made
    /// when there is none or its effects or which are on changed, its
    /// settings changed in place otherwise, so a slider moved while
    /// playing is heard with no gap. Its latency is compensated with the
    /// sources'.
    pub fn set_track_inserts(&mut self, track_idx: usize, instances: &[klipp_core::AudioEffectInstance], max_block_samples: usize) {
        self.ensure_tracks(track_idx + 1);
        let shape: Vec<(String, bool)> = instances.iter().map(|i| (i.effect_type.clone(), i.enabled)).collect();
        let slot = &mut self.track_inserts[track_idx];
        if instances.is_empty() {
            *slot = None;
        } else if let Some(inserts) = slot.as_mut().filter(|i| i.shape == shape) {
            inserts.chain.apply_settings(instances);
        } else {
            let mut chain = crate::dsp::AudioEffectChain::from_instances(instances, self.sample_rate);
            chain.prepare(2);
            *slot = Some(TrackInserts { shape, chain });
        }
        let latency = self.track_inserts[track_idx].as_ref().map_or(0, |i| i.chain.total_latency_samples(self.sample_rate));
        if latency != self.insert_latency[track_idx] {
            self.insert_latency[track_idx] = latency;
            let source = self.source_latency[track_idx];
            self.set_track_latency_samples(track_idx, source, max_block_samples);
        }
    }

    /// Maximum latency (in stereo frames) reported across all tracks.
    /// Drives both the per-track delay-line sizing and the eventual
    /// `MasterClock::pdc_offset_us` (`PDC-NEXT-AV-OFFSET` follow-up).
    pub fn pdc_global_max_latency(&self) -> u32 {
        crate::pdc::max_latency_across_tracks(&self.track_latency_samples)
    }

    /// Per-track delay each track needs in order to align with the
    /// slowest. Length matches `tracks.len()`.
    pub fn pdc_compensation_per_track(&self) -> Vec<u32> {
        crate::pdc::compensation_per_track(&self.track_latency_samples)
    }

    /// Update one track's reported chain latency. Resizes the per-
    /// track delay-line whenever the global max latency changes —
    /// every track's required delay is `max - own`, so adding latency
    /// to one track grows every other track's delay line.
    ///
    /// Called from the audio decoder thread when a chain is rebuilt
    /// (effect added / removed / reordered, or initial chain
    /// construction). v1 assumes static-after-build latency; mid-
    /// session variable latency is `PDC-NEXT-VARIABLE-LATENCY`.
    pub fn set_track_latency_samples(
        &mut self,
        track_idx: usize,
        latency_samples: u32,
        max_block_samples: usize,
    ) {
        // Grow track-state Vecs if a track index appears that wasn't
        // there before (mirror `ensure_tracks` shape).
        while self.track_latency_samples.len() <= track_idx {
            self.track_latency_samples.push(0);
            self.track_delay_lines.push(None);
        }
        self.ensure_tracks(track_idx + 1);
        // The sources' latency, with the track's inserts' after it.
        self.source_latency[track_idx] = latency_samples;
        self.track_latency_samples[track_idx] = latency_samples + self.insert_latency[track_idx];

        // Recompute every track's required delay, resize lines
        // accordingly. A track with 0 required delay has its line
        // dropped (no per-block work); a track that needs a delay
        // gets its line either created or resized.
        let comp = self.pdc_compensation_per_track();
        for (i, &needed) in comp.iter().enumerate() {
            if needed == 0 {
                self.track_delay_lines[i] = None;
            } else {
                match &mut self.track_delay_lines[i] {
                    Some(line) => line.set_delay_samples(needed as usize, max_block_samples),
                    slot @ None => {
                        *slot = Some(crate::pdc::TrackDelayLine::new(
                            needed as usize,
                            max_block_samples,
                        ));
                    }
                }
            }
        }
    }

    /// Apply per-track PDC delay to one track's interleaved-stereo
    /// block in place. Called by the mix-thread integration
    /// (`PDC-NEXT-MIX-THREAD-WIRING`) for each track *before* the
    /// existing mix routing runs. No-op when the track has no delay
    /// line configured.
    pub fn apply_per_track_pdc_delay(&mut self, track_idx: usize, samples: &mut [f32]) {
        if let Some(line) = self
            .track_delay_lines
            .get_mut(track_idx)
            .and_then(|s| s.as_mut())
        {
            line.process_in_place(samples);
        }
    }

    /// Reset every PDC delay line to silence. Called on transport
    /// seek so stale samples from before the seek don't bleed
    /// across the boundary.
    pub fn reset_pdc_delay_lines(&mut self) {
        for slot in &mut self.track_delay_lines {
            if let Some(line) = slot.as_mut() {
                line.reset();
            }
        }
    }

    /// Calculate L/R gain factors from pan position and current pan law.
    /// Left and right gains for a pan position, by the configured law.
    ///
    /// Delegates to `PanLaw::gains`, which is where the laws live now.
    /// They used to be written out here, and the export mixdown had the
    /// constant-power trig written out separately — so a render came out
    /// 3 dB below what it sounded like.
    fn pan_gains_for(pan: f32, law: klipp_core::PanLaw) -> (f32, f32) {
        law.gains(pan)
    }

    /// Add a submix bus and return its index.
    pub fn add_bus(&mut self, name: &str) -> usize {
        let idx = self.buses.len();
        self.buses.push(AudioBus::new(name));
        idx
    }

    /// Ensure at least `count` track settings exist. Keeps the PDC
    /// per-track state in sync so a freshly-grown track gets its
    /// (zero-latency) entry instead of indexing past the end.
    pub fn ensure_tracks(&mut self, count: usize) {
        while self.tracks.len() < count {
            self.tracks.push(AudioTrackSettings::default());
        }
        while self.track_latency_samples.len() < count {
            self.track_latency_samples.push(0);
        }
        while self.track_delay_lines.len() < count {
            self.track_delay_lines.push(None);
        }
        while self.track_inserts.len() < count {
            self.track_inserts.push(None);
        }
        while self.source_latency.len() < count {
            self.source_latency.push(0);
            self.insert_latency.push(0);
        }
    }

    /// Mix one callback worth of samples from all tracks.
    /// `track_buffers`: `(sequence_track_index, samples)` pairs.
    /// Returns interleaved f32 stereo output.
    pub fn mix(&mut self, track_buffers: &[(usize, &[f32])]) -> Vec<f32> {
        let frame_len = track_buffers.iter().map(|(_, b)| b.len()).max().unwrap_or(0);
        let any_solo = self.tracks.iter().any(|t| t.solo);
        let has_buses = !self.buses.is_empty();

        // If buses exist, create per-bus intermediate buffers
        let mut bus_buffers: Vec<Vec<f32>> = if has_buses {
            vec![vec![0.0f32; frame_len]; self.buses.len()]
        } else {
            Vec::new()
        };
        let mut master_buf = vec![0.0f32; frame_len];

        let mut inserted: Vec<f32> = Vec::new();
        for &(track_idx, buf) in track_buffers.iter() {
            let Some(settings) = self.tracks.get(track_idx) else { continue };
            let heard = !(settings.muted || (any_solo && !settings.solo));
            // The track's own effects on its summed clips, before its
            // fader (#2560).
            let buf = match self.track_inserts.get_mut(track_idx).and_then(Option::as_mut) {
                Some(inserts) if heard => {
                    inserted.clear();
                    inserted.extend_from_slice(buf);
                    inserts.chain.process(&mut inserted, &crate::dsp::AudioProcessContext { sample_rate: self.sample_rate, channels: 2, clip_time_secs: 0.0 });
                    inserted.as_slice()
                }
                _ => buf,
            };
            let settings = match self.tracks.get_mut(track_idx) {
                Some(s) => s,
                None => continue,
            };
            if !heard {
                settings.peak_left = 0.0;
                settings.peak_right = 0.0;
                continue;
            }

            let (pan_l, pan_r) = Self::pan_gains_for(settings.pan, self.pan_law);
            // From where the last block ended to the settings now, when
            // they changed; held, the gains are the settings' as before.
            let (from_volume, from_pan) = settings.ramp_from.unwrap_or((settings.volume, settings.pan));
            let ramped = (from_volume, from_pan) != (settings.volume, settings.pan);
            let (from_l, from_r) = Self::pan_gains_for(from_pan, self.pan_law);
            let frames = (buf.len() / 2).max(1) as f32;
            settings.ramp_from = Some((settings.volume, settings.pan));

            let mut peak_l = 0.0f32;
            let mut peak_r = 0.0f32;
            let mut sum_sq_l = 0.0f32;
            let mut sum_sq_r = 0.0f32;
            let mut sample_count = 0u32;

            // Determine output target: bus buffer or master
            let target = if let Some(bus_idx) = settings.bus {
                if bus_idx < bus_buffers.len() {
                    &mut bus_buffers[bus_idx]
                } else {
                    &mut master_buf
                }
            } else {
                &mut master_buf
            };

            for (j, pair) in buf.chunks(2).enumerate() {
                let l = pair.first().copied().unwrap_or(0.0);
                let r = pair.get(1).copied().unwrap_or(0.0);
                let (out_l, out_r) = if ramped {
                    let t = (j + 1) as f32 / frames;
                    let volume = from_volume + (settings.volume - from_volume) * t;
                    (l * volume * (from_l + (pan_l - from_l) * t), r * volume * (from_r + (pan_r - from_r) * t))
                } else {
                    (l * settings.volume * pan_l, r * settings.volume * pan_r)
                };
                peak_l = peak_l.max(out_l.abs());
                peak_r = peak_r.max(out_r.abs());
                sum_sq_l += out_l * out_l;
                sum_sq_r += out_r * out_r;
                sample_count += 1;
                let base = j * 2;
                if base < target.len() {
                    target[base] += out_l;
                }
                if base + 1 < target.len() {
                    target[base + 1] += out_r;
                }
            }

            settings.peak_left = peak_l;
            settings.peak_right = peak_r;
            if sample_count > 0 {
                settings.rms_left = (sum_sq_l / sample_count as f32).sqrt();
                settings.rms_right = (sum_sq_r / sample_count as f32).sqrt();
            }
        }
        // Every track is at its settings after this block, the ones not
        // heard in it too (muted, soloed out, no sound): heard again, a
        // track starts from them, not ramping from where it was silenced.
        for t in &mut self.tracks {
            t.ramp_from = Some((t.volume, t.pan));
        }

        // Mix buses into master
        for (bus_idx, bus_buf) in bus_buffers.iter().enumerate() {
            if let Some(bus) = self.buses.get_mut(bus_idx) {
                if bus.muted { continue; }
                let (pan_l, pan_r) = Self::pan_gains_for(bus.pan, self.pan_law);
                let mut peak_l = 0.0f32;
                let mut peak_r = 0.0f32;
                for (j, pair) in bus_buf.chunks(2).enumerate() {
                    let l = pair.first().copied().unwrap_or(0.0);
                    let r = pair.get(1).copied().unwrap_or(0.0);
                    let out_l = l * bus.volume * pan_l;
                    let out_r = r * bus.volume * pan_r;
                    peak_l = peak_l.max(out_l.abs());
                    peak_r = peak_r.max(out_r.abs());
                    let base = j * 2;
                    if base < master_buf.len() {
                        master_buf[base] += out_l;
                    }
                    if base + 1 < master_buf.len() {
                        master_buf[base + 1] += out_r;
                    }
                }
                bus.peak_left = peak_l;
                bus.peak_right = peak_r;
            }
        }

        // Soft clip
        master_buf.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
        master_buf
    }
}

#[cfg(test)]
mod tests {
    /// #2560: a track's own effects run on what its clips sum to, before its
    /// fader: the track's output is the chain's on the sum, times the
    /// fader. A setting changed keeps the chain; an effect turned off makes
    /// it again without.
    #[test]
    fn a_tracks_inserts_run_on_its_sum_before_its_fader() {
        let insts = |gain: f32, on: bool| vec![klipp_core::AudioEffectInstance { effect_type: "compressor".into(), enabled: on, wet_dry: 1.0, params: vec![("threshold".into(), -24.0), ("ratio".into(), 4.0), ("makeup".into(), gain)] }];
        let buf: Vec<f32> = (0..2048).map(|i| 0.8 * ((i / 2) as f32 * 0.05).sin()).collect();
        let mut mixer = AudioMixer::new(1);
        mixer.set_sample_rate(48_000);
        mixer.set_track_inserts(0, &insts(0.0, true), 4096);
        mixer.tracks[0].volume = 0.5;
        mixer.tracks[0].ramp_from = Some((0.5, 0.0));
        let out = mixer.mix(&[(0, &buf)]);
        let mut chain = crate::dsp::AudioEffectChain::from_instances(&insts(0.0, true), 48_000);
        chain.prepare(2);
        let mut want = buf.clone();
        chain.process(&mut want, &crate::dsp::AudioProcessContext { sample_rate: 48_000, channels: 2, clip_time_secs: 0.0 });
        let (l, r) = AudioMixer::pan_gains_for(0.0, mixer.pan_law);
        for (i, (o, w)) in out.iter().zip(&want).enumerate() {
            let g = if i % 2 == 0 { l } else { r };
            assert!((o - (w * 0.5 * g).clamp(-1.0, 1.0)).abs() < 1e-6, "sample {i}: {o} vs {}", w * 0.5 * g);
        }
        mixer.set_track_inserts(0, &insts(3.0, true), 4096);
        assert!(mixer.track_inserts[0].is_some(), "changed in place");
        mixer.set_track_inserts(0, &[], 4096);
        assert!(mixer.track_inserts[0].is_none(), "none left");
    }

    use super::*;
    use klipp_core::PanLaw;

    const EPS: f32 = 1e-4;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    // ---------------------------------------------------------------
    // Basic mixer tests
    // ---------------------------------------------------------------

    /// #2306: held settings mix as they always did, bit for bit; a
    /// change is ramped across the next block from where the last ended,
    /// reaching the new value at its last sample, with no step.
    #[test]
    fn a_volume_change_ramps_across_the_next_block() {
        let mut mixer = AudioMixer::new(1);
        mixer.tracks[0].volume = 0.5;
        let buf = vec![1.0f32; 8];
        let held = mixer.mix(&[(0, &buf)]);
        let (pl, _) = AudioMixer::pan_gains_for(0.0, mixer.pan_law);
        assert!(held.iter().step_by(2).all(|v| v.to_bits() == (1.0f32 * 0.5 * pl).to_bits()), "{held:?}");
        assert_eq!(held, mixer.mix(&[(0, &buf)]), "held again: the same");
        mixer.tracks[0].volume = 1.0;
        let ramped = mixer.mix(&[(0, &buf)]);
        let left: Vec<f32> = ramped.iter().step_by(2).copied().collect();
        assert!(left.windows(2).all(|w| w[1] > w[0]), "rising, no step: {left:?}");
        assert!(approx(left[0], 0.625 * pl) && approx(left[3], 1.0 * pl), "{left:?}");
        let next = mixer.mix(&[(0, &buf)]);
        assert!(next.iter().step_by(2).all(|v| approx(*v, pl)), "then held at the new value");
    }

    /// A track whose volume changed while it was muted, or had no sound,
    /// is heard at the new value from its first sample back, not ramping
    /// from the old.
    #[test]
    fn a_change_while_silent_is_not_ramped_when_heard_again() {
        let buf = vec![1.0f32; 8];
        let (pl, _) = AudioMixer::pan_gains_for(0.0, klipp_core::PanLaw::default());
        for muted in [true, false] {
            let mut mixer = AudioMixer::new(1);
            mixer.tracks[0].volume = 0.5;
            mixer.mix(&[(0, &buf)]);
            mixer.tracks[0].volume = 1.0;
            mixer.tracks[0].muted = muted;
            let silent: Vec<(usize, &[f32])> = if muted { vec![(0, &buf)] } else { Vec::new() };
            mixer.mix(&silent);
            mixer.tracks[0].muted = false;
            let back = mixer.mix(&[(0, &buf)]);
            assert!(back.iter().step_by(2).all(|v| approx(*v, pl)), "muted {muted}: {back:?}");
        }
    }

    #[test]
    fn silence_in_silence_out() {
        let mut mixer = AudioMixer::new(1);
        let buf = vec![0.0f32; 8];
        let out = mixer.mix(&[(0, &buf)]);
        assert_eq!(out, vec![0.0; 8]);
    }

    #[test]
    fn passthrough_single_track() {
        let mut mixer = AudioMixer::new(1);
        let buf = vec![0.5, -0.3, 0.7, -0.1];
        let out = mixer.mix(&[(0, &buf)]);
        assert!((out[0] - 0.5).abs() < 0.001);
        assert!((out[1] - (-0.3)).abs() < 0.001);
    }

    #[test]
    fn two_tracks_sum() {
        let mut mixer = AudioMixer::new(2);
        let buf1 = vec![0.3, 0.3];
        let buf2 = vec![0.2, 0.2];
        let out = mixer.mix(&[(0, &buf1), (1, &buf2)]);
        assert!((out[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn muted_track_silent() {
        let mut mixer = AudioMixer::new(2);
        mixer.tracks[0].muted = true;
        let buf1 = vec![1.0, 1.0];
        let buf2 = vec![0.5, 0.5];
        let out = mixer.mix(&[(0, &buf1), (1, &buf2)]);
        assert!((out[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn solo_isolates_track() {
        let mut mixer = AudioMixer::new(2);
        mixer.tracks[1].solo = true;
        let buf1 = vec![1.0, 1.0];
        let buf2 = vec![0.3, 0.3];
        let out = mixer.mix(&[(0, &buf1), (1, &buf2)]);
        assert!((out[0] - 0.3).abs() < 0.001);
    }

    #[test]
    fn volume_scales() {
        let mut mixer = AudioMixer::new(1);
        mixer.tracks[0].volume = 0.5;
        let buf = vec![1.0, 1.0];
        let out = mixer.mix(&[(0, &buf)]);
        assert!((out[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn master_volume_not_applied_in_mixer() {
        let mut mixer = AudioMixer::new(1);
        mixer.master_volume = 0.5;
        let buf = vec![1.0, 1.0];
        let out = mixer.mix(&[(0, &buf)]);
        assert!((out[0] - 1.0).abs() < 0.001);
    }

    #[test]
    fn pan_hard_left() {
        let mut mixer = AudioMixer::new(1);
        mixer.tracks[0].pan = -1.0;
        let buf = vec![0.8, 0.8];
        let out = mixer.mix(&[(0, &buf)]);
        assert!((out[0] - 0.8).abs() < 0.001);
        assert!(out[1].abs() < 0.001);
    }

    #[test]
    fn pan_hard_right() {
        let mut mixer = AudioMixer::new(1);
        mixer.tracks[0].pan = 1.0;
        let buf = vec![0.8, 0.8];
        let out = mixer.mix(&[(0, &buf)]);
        assert!(out[0].abs() < 0.001);
        assert!((out[1] - 0.8).abs() < 0.001);
    }

    #[test]
    fn peak_levels_updated() {
        let mut mixer = AudioMixer::new(1);
        let buf = vec![0.5, -0.7, 0.3, 0.9];
        let _ = mixer.mix(&[(0, &buf)]);
        assert!(mixer.tracks[0].peak_left > 0.0);
        assert!(mixer.tracks[0].peak_right > 0.0);
    }

    #[test]
    fn muted_track_zero_peaks() {
        let mut mixer = AudioMixer::new(1);
        mixer.tracks[0].muted = true;
        let buf = vec![1.0, 1.0];
        let _ = mixer.mix(&[(0, &buf)]);
        assert_eq!(mixer.tracks[0].peak_left, 0.0);
        assert_eq!(mixer.tracks[0].peak_right, 0.0);
    }

    #[test]
    fn soft_clip_clamps() {
        let mut mixer = AudioMixer::new(2);
        let buf1 = vec![0.9, 0.9];
        let buf2 = vec![0.9, 0.9];
        let out = mixer.mix(&[(0, &buf1), (1, &buf2)]);
        // Sum would exceed 1.0, should be clamped
        assert!(out[0] <= 1.0);
        assert!(out[1] <= 1.0);
    }

    #[test]
    fn empty_input() {
        let mut mixer = AudioMixer::new(0);
        let out = mixer.mix(&[]);
        assert!(out.is_empty());
    }

    #[test]
    fn ensure_tracks_grows() {
        let mut mixer = AudioMixer::new(1);
        assert_eq!(mixer.tracks.len(), 1);
        mixer.ensure_tracks(3);
        assert_eq!(mixer.tracks.len(), 3);
    }

    // ---------------------------------------------------------------
    // Pan law: Linear
    // ---------------------------------------------------------------

    #[test]
    fn pan_linear_center_equal_gains() {
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::Linear);
        assert!(approx(l, 1.0), "Linear center L={l}, expected 1.0");
        assert!(approx(r, 1.0), "Linear center R={r}, expected 1.0");
    }

    #[test]
    fn pan_linear_hard_left() {
        let (l, r) = AudioMixer::pan_gains_for(-1.0, PanLaw::Linear);
        assert!(approx(l, 1.0), "Linear hard-left L={l}");
        assert!(approx(r, 0.0), "Linear hard-left R={r}");
    }

    #[test]
    fn pan_linear_hard_right() {
        let (l, r) = AudioMixer::pan_gains_for(1.0, PanLaw::Linear);
        assert!(approx(l, 0.0), "Linear hard-right L={l}");
        assert!(approx(r, 1.0), "Linear hard-right R={r}");
    }

    #[test]
    fn pan_linear_quarter_left() {
        let (l, r) = AudioMixer::pan_gains_for(-0.5, PanLaw::Linear);
        assert!(approx(l, 1.0), "Linear pan=-0.5 L={l}");
        assert!(approx(r, 0.5), "Linear pan=-0.5 R={r}");
    }

    #[test]
    fn pan_linear_quarter_right() {
        let (l, r) = AudioMixer::pan_gains_for(0.5, PanLaw::Linear);
        assert!(approx(l, 0.5), "Linear pan=0.5 L={l}");
        assert!(approx(r, 1.0), "Linear pan=0.5 R={r}");
    }

    // ---------------------------------------------------------------
    // Pan law: ConstantPower
    // ---------------------------------------------------------------

    #[test]
    fn pan_constant_power_center_equal_gains() {
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::ConstantPower);
        assert!(approx(l, r), "ConstantPower center L={l} != R={r}");
    }

    #[test]
    fn pan_constant_power_center_energy_preserved() {
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::ConstantPower);
        let energy = l * l + r * r;
        assert!(
            approx(energy, 1.0),
            "ConstantPower center energy={energy}, expected 1.0"
        );
    }

    #[test]
    fn pan_constant_power_hard_left() {
        let (l, r) = AudioMixer::pan_gains_for(-1.0, PanLaw::ConstantPower);
        assert!(approx(l, 1.0), "ConstantPower hard-left L={l}");
        assert!(r.abs() < EPS, "ConstantPower hard-left R={r}");
    }

    #[test]
    fn pan_constant_power_hard_right() {
        let (l, r) = AudioMixer::pan_gains_for(1.0, PanLaw::ConstantPower);
        assert!(l.abs() < EPS, "ConstantPower hard-right L={l}");
        assert!(approx(r, 1.0), "ConstantPower hard-right R={r}");
    }

    #[test]
    fn pan_constant_power_energy_preserved_across_range() {
        // Energy (L^2 + R^2) should be ~1.0 at every pan position
        for i in -10..=10 {
            let pan = i as f32 / 10.0;
            let (l, r) = AudioMixer::pan_gains_for(pan, PanLaw::ConstantPower);
            let energy = l * l + r * r;
            assert!(
                (energy - 1.0).abs() < 0.01,
                "ConstantPower energy={energy} at pan={pan}"
            );
        }
    }

    #[test]
    fn pan_constant_power_symmetry() {
        // pan_gains_for(-x) should mirror pan_gains_for(x): L and R swap
        for i in 1..=10 {
            let pan = i as f32 / 10.0;
            let (l_pos, r_pos) = AudioMixer::pan_gains_for(pan, PanLaw::ConstantPower);
            let (l_neg, r_neg) = AudioMixer::pan_gains_for(-pan, PanLaw::ConstantPower);
            assert!(
                approx(l_pos, r_neg),
                "Symmetry: L(+{pan})={l_pos} != R(-{pan})={r_neg}"
            );
            assert!(
                approx(r_pos, l_neg),
                "Symmetry: R(+{pan})={r_pos} != L(-{pan})={l_neg}"
            );
        }
    }

    // ---------------------------------------------------------------
    // Pan law: Minus3dB
    // ---------------------------------------------------------------

    #[test]
    fn pan_minus3db_center_equal_gains() {
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::Minus3dB);
        assert!(approx(l, r), "Minus3dB center L={l} != R={r}");
    }

    #[test]
    fn pan_minus3db_center_value() {
        // At center, t=0.5, linear_l = (0.5*2).min(1) = 1, sqrt(1) = 1
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::Minus3dB);
        assert!(approx(l, 1.0), "Minus3dB center L={l}");
        assert!(approx(r, 1.0), "Minus3dB center R={r}");
    }

    #[test]
    fn pan_minus3db_hard_left() {
        let (l, r) = AudioMixer::pan_gains_for(-1.0, PanLaw::Minus3dB);
        // t=0, l = sqrt(min(2,1)) = sqrt(1) = 1, r = sqrt(min(0,1)) = 0
        assert!(approx(l, (2.0f32).min(1.0).sqrt()), "Minus3dB hard-left L={l}");
        assert!(r.abs() < EPS, "Minus3dB hard-left R={r}");
    }

    #[test]
    fn pan_minus3db_hard_right() {
        let (l, r) = AudioMixer::pan_gains_for(1.0, PanLaw::Minus3dB);
        assert!(l.abs() < EPS, "Minus3dB hard-right L={l}");
        assert!(approx(r, 1.0), "Minus3dB hard-right R={r}");
    }

    #[test]
    fn pan_minus3db_monotonic() {
        // L gain should decrease monotonically as pan goes left-to-right
        let mut prev_l = f32::MAX;
        for i in -10..=10 {
            let pan = i as f32 / 10.0;
            let (l, _) = AudioMixer::pan_gains_for(pan, PanLaw::Minus3dB);
            assert!(l <= prev_l + EPS, "Minus3dB L not monotonically decreasing at pan={pan}");
            prev_l = l;
        }
    }

    // ---------------------------------------------------------------
    // Pan law: Minus4_5dB
    // ---------------------------------------------------------------

    #[test]
    fn pan_minus4_5db_center_equal_gains() {
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::Minus4_5dB);
        assert!(approx(l, r), "Minus4_5dB center L={l} != R={r}");
    }

    #[test]
    fn pan_minus4_5db_center_value() {
        // t=0.5, linear = min(1,1) = 1.0, 1.0^0.75 = 1.0
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::Minus4_5dB);
        assert!(approx(l, 1.0), "Minus4_5dB center L={l}");
        assert!(approx(r, 1.0), "Minus4_5dB center R={r}");
    }

    #[test]
    fn pan_minus4_5db_hard_left() {
        let (l, r) = AudioMixer::pan_gains_for(-1.0, PanLaw::Minus4_5dB);
        assert!(approx(l, 1.0), "Minus4_5dB hard-left L={l}");
        assert!(r.abs() < EPS, "Minus4_5dB hard-left R={r}");
    }

    #[test]
    fn pan_minus4_5db_hard_right() {
        let (l, r) = AudioMixer::pan_gains_for(1.0, PanLaw::Minus4_5dB);
        assert!(l.abs() < EPS, "Minus4_5dB hard-right L={l}");
        assert!(approx(r, 1.0), "Minus4_5dB hard-right R={r}");
    }

    #[test]
    fn pan_minus4_5db_quarter_pan() {
        // At pan=-0.5, t=0.25: l = (0.75*2).min(1) = 1.0, r = (0.25*2).min(1) = 0.5
        let (l, r) = AudioMixer::pan_gains_for(-0.5, PanLaw::Minus4_5dB);
        let expected_l = ((0.75 * 2.0f32).min(1.0)).powf(0.75);
        let expected_r = ((0.25 * 2.0f32).min(1.0)).powf(0.75);
        assert!(approx(l, expected_l), "Minus4_5dB pan=-0.5 L={l}, expected {expected_l}");
        assert!(approx(r, expected_r), "Minus4_5dB pan=-0.5 R={r}, expected {expected_r}");
    }

    // ---------------------------------------------------------------
    // Pan law: Minus6dB
    // ---------------------------------------------------------------

    #[test]
    fn pan_minus6db_center_equal_gains() {
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::Minus6dB);
        assert!(approx(l, r), "Minus6dB center L={l} != R={r}");
    }

    #[test]
    fn pan_minus6db_center_value() {
        // pan=0: l = 1 - max(0,0) = 1, r = 1 + min(0,0) = 1
        let (l, r) = AudioMixer::pan_gains_for(0.0, PanLaw::Minus6dB);
        assert!(approx(l, 1.0), "Minus6dB center L={l}");
        assert!(approx(r, 1.0), "Minus6dB center R={r}");
    }

    #[test]
    fn pan_minus6db_hard_left() {
        let (l, r) = AudioMixer::pan_gains_for(-1.0, PanLaw::Minus6dB);
        assert!(approx(l, 1.0), "Minus6dB hard-left L={l}");
        assert!(approx(r, 0.0), "Minus6dB hard-left R={r}");
    }

    #[test]
    fn pan_minus6db_hard_right() {
        let (l, r) = AudioMixer::pan_gains_for(1.0, PanLaw::Minus6dB);
        assert!(approx(l, 0.0), "Minus6dB hard-right L={l}");
        assert!(approx(r, 1.0), "Minus6dB hard-right R={r}");
    }

    #[test]
    fn pan_minus6db_half_pan() {
        // pan=0.5: l = 1 - 0.5 = 0.5, r = 1 + 0 = 1 (min(0.5,0) = 0)
        let (l, r) = AudioMixer::pan_gains_for(0.5, PanLaw::Minus6dB);
        assert!(approx(l, 0.5), "Minus6dB pan=0.5 L={l}");
        assert!(approx(r, 1.0), "Minus6dB pan=0.5 R={r}");
    }

    #[test]
    fn pan_minus6db_symmetry() {
        for i in 1..=10 {
            let pan = i as f32 / 10.0;
            let (l_pos, r_pos) = AudioMixer::pan_gains_for(pan, PanLaw::Minus6dB);
            let (l_neg, r_neg) = AudioMixer::pan_gains_for(-pan, PanLaw::Minus6dB);
            assert!(approx(l_pos, r_neg), "Minus6dB symmetry L(+{pan})={l_pos} != R(-{pan})={r_neg}");
            assert!(approx(r_pos, l_neg), "Minus6dB symmetry R(+{pan})={r_pos} != L(-{pan})={l_neg}");
        }
    }

    // ---------------------------------------------------------------
    // All pan laws: common invariants
    // ---------------------------------------------------------------

    #[test]
    fn all_laws_gains_in_0_to_1_range() {
        let laws = [
            PanLaw::Linear,
            PanLaw::ConstantPower,
            PanLaw::Minus3dB,
            PanLaw::Minus4_5dB,
            PanLaw::Minus6dB,
        ];
        for law in &laws {
            for i in -10..=10 {
                let pan = i as f32 / 10.0;
                let (l, r) = AudioMixer::pan_gains_for(pan, *law);
                assert!(
                    (-EPS..=1.0 + EPS).contains(&l),
                    "{law:?} L={l} out of range at pan={pan}"
                );
                assert!(
                    (-EPS..=1.0 + EPS).contains(&r),
                    "{law:?} R={r} out of range at pan={pan}"
                );
            }
        }
    }

    #[test]
    fn all_laws_center_l_equals_r() {
        let laws = [
            PanLaw::Linear,
            PanLaw::ConstantPower,
            PanLaw::Minus3dB,
            PanLaw::Minus4_5dB,
            PanLaw::Minus6dB,
        ];
        for law in &laws {
            let (l, r) = AudioMixer::pan_gains_for(0.0, *law);
            assert!(approx(l, r), "{law:?} center L={l} != R={r}");
        }
    }

    #[test]
    fn all_laws_hard_left_zero_right() {
        let laws = [
            PanLaw::Linear,
            PanLaw::ConstantPower,
            PanLaw::Minus3dB,
            PanLaw::Minus4_5dB,
            PanLaw::Minus6dB,
        ];
        for law in &laws {
            let (l, r) = AudioMixer::pan_gains_for(-1.0, *law);
            assert!(l > 0.5, "{law:?} hard-left L={l} should be high");
            assert!(r.abs() < EPS, "{law:?} hard-left R={r} should be ~0");
        }
    }

    #[test]
    fn all_laws_hard_right_zero_left() {
        let laws = [
            PanLaw::Linear,
            PanLaw::ConstantPower,
            PanLaw::Minus3dB,
            PanLaw::Minus4_5dB,
            PanLaw::Minus6dB,
        ];
        for law in &laws {
            let (l, r) = AudioMixer::pan_gains_for(1.0, *law);
            assert!(l.abs() < EPS, "{law:?} hard-right L={l} should be ~0");
            assert!(r > 0.5, "{law:?} hard-right R={r} should be high");
        }
    }

    // ---------------------------------------------------------------
    // Mixer: single track at unity passthrough
    // ---------------------------------------------------------------

    #[test]
    fn mixer_single_track_unity_passthrough() {
        let mut mixer = AudioMixer::new(1);
        // Default: volume=1.0, pan=0.0, Linear law -> L=1, R=1
        let input = vec![0.25, -0.5, 0.75, -0.125];
        let out = mixer.mix(&[(0, &input)]);
        assert_eq!(out.len(), input.len());
        for (i, (&o, &inp)) in out.iter().zip(input.iter()).enumerate() {
            assert!(approx(o, inp), "Sample {i}: out={o}, input={inp}");
        }
    }

    // ---------------------------------------------------------------
    // Mixer: two tracks summed before clipping
    // ---------------------------------------------------------------

    #[test]
    fn mixer_two_tracks_sum_before_clip() {
        let mut mixer = AudioMixer::new(2);
        let buf1 = vec![0.4, 0.3, 0.2, 0.1];
        let buf2 = vec![0.1, 0.2, 0.3, 0.4];
        let out = mixer.mix(&[(0, &buf1), (1, &buf2)]);
        assert!(approx(out[0], 0.5), "L0: {}", out[0]);
        assert!(approx(out[1], 0.5), "R0: {}", out[1]);
        assert!(approx(out[2], 0.5), "L1: {}", out[2]);
        assert!(approx(out[3], 0.5), "R1: {}", out[3]);
    }

    #[test]
    fn mixer_two_tracks_clipping() {
        let mut mixer = AudioMixer::new(2);
        let buf1 = vec![0.8, 0.8];
        let buf2 = vec![0.8, 0.8];
        let out = mixer.mix(&[(0, &buf1), (1, &buf2)]);
        // 0.8 + 0.8 = 1.6, should be clamped to 1.0
        assert!(approx(out[0], 1.0), "Clipped L: {}", out[0]);
        assert!(approx(out[1], 1.0), "Clipped R: {}", out[1]);
    }

    // ---------------------------------------------------------------
    // Mixer: mute flag
    // ---------------------------------------------------------------

    #[test]
    fn mixer_muted_track_produces_silence() {
        let mut mixer = AudioMixer::new(1);
        mixer.tracks[0].muted = true;
        let buf = vec![0.9, 0.8, 0.7, 0.6];
        let out = mixer.mix(&[(0, &buf)]);
        for (i, &s) in out.iter().enumerate() {
            assert!(s.abs() < EPS, "Muted output sample {i}={s} should be 0");
        }
    }

    #[test]
    fn mixer_muted_among_multiple() {
        let mut mixer = AudioMixer::new(3);
        mixer.tracks[1].muted = true;
        let buf0 = vec![0.1, 0.1];
        let buf1 = vec![0.5, 0.5]; // muted
        let buf2 = vec![0.2, 0.2];
        let out = mixer.mix(&[(0, &buf0), (1, &buf1), (2, &buf2)]);
        assert!(approx(out[0], 0.3), "L: {}", out[0]);
        assert!(approx(out[1], 0.3), "R: {}", out[1]);
    }

    // ---------------------------------------------------------------
    // Mixer: solo flag
    // ---------------------------------------------------------------

    #[test]
    fn mixer_solo_isolates_single_track() {
        let mut mixer = AudioMixer::new(3);
        mixer.tracks[1].solo = true;
        let buf0 = vec![0.5, 0.5];
        let buf1 = vec![0.2, 0.2];
        let buf2 = vec![0.3, 0.3];
        let out = mixer.mix(&[(0, &buf0), (1, &buf1), (2, &buf2)]);
        // Only track 1 should pass through
        assert!(approx(out[0], 0.2), "Solo L: {}", out[0]);
        assert!(approx(out[1], 0.2), "Solo R: {}", out[1]);
    }

    #[test]
    fn mixer_multiple_solos() {
        let mut mixer = AudioMixer::new(3);
        mixer.tracks[0].solo = true;
        mixer.tracks[2].solo = true;
        let buf0 = vec![0.1, 0.1];
        let buf1 = vec![0.5, 0.5]; // not solo'd
        let buf2 = vec![0.2, 0.2];
        let out = mixer.mix(&[(0, &buf0), (1, &buf1), (2, &buf2)]);
        assert!(approx(out[0], 0.3), "Multi-solo L: {}", out[0]);
        assert!(approx(out[1], 0.3), "Multi-solo R: {}", out[1]);
    }

    #[test]
    fn mixer_solo_zeroes_non_solo_peaks() {
        let mut mixer = AudioMixer::new(2);
        mixer.tracks[1].solo = true;
        let buf0 = vec![0.9, 0.9];
        let buf1 = vec![0.3, 0.3];
        let _ = mixer.mix(&[(0, &buf0), (1, &buf1)]);
        assert_eq!(mixer.tracks[0].peak_left, 0.0);
        assert_eq!(mixer.tracks[0].peak_right, 0.0);
        assert!(mixer.tracks[1].peak_left > 0.0);
    }

    // ---------------------------------------------------------------
    // Mixer: bus routing
    // ---------------------------------------------------------------

    #[test]
    fn mixer_bus_routing_to_master() {
        let mut mixer = AudioMixer::new(2);
        let bus_idx = mixer.add_bus("Submix A");

        // Route track 0 to bus, track 1 direct to master
        mixer.tracks[0].bus = Some(bus_idx);

        let buf0 = vec![0.3, 0.3];
        let buf1 = vec![0.2, 0.2];
        let out = mixer.mix(&[(0, &buf0), (1, &buf1)]);

        // Track 0 -> bus -> master, track 1 -> master
        // Both at unity volume/pan so result is 0.3 + 0.2 = 0.5
        assert!(approx(out[0], 0.5), "Bus+direct L: {}", out[0]);
        assert!(approx(out[1], 0.5), "Bus+direct R: {}", out[1]);
    }

    #[test]
    fn mixer_bus_volume_scales() {
        let mut mixer = AudioMixer::new(1);
        let bus_idx = mixer.add_bus("FX Bus");
        mixer.tracks[0].bus = Some(bus_idx);
        mixer.buses[bus_idx].volume = 0.5;

        let buf = vec![0.8, 0.8];
        let out = mixer.mix(&[(0, &buf)]);
        // Track at unity through bus at 0.5 volume: 0.8 * 0.5 = 0.4
        assert!(approx(out[0], 0.4), "Bus volume L: {}", out[0]);
        assert!(approx(out[1], 0.4), "Bus volume R: {}", out[1]);
    }

    #[test]
    fn mixer_bus_muted() {
        let mut mixer = AudioMixer::new(1);
        let bus_idx = mixer.add_bus("Muted Bus");
        mixer.tracks[0].bus = Some(bus_idx);
        mixer.buses[bus_idx].muted = true;

        let buf = vec![0.8, 0.8];
        let out = mixer.mix(&[(0, &buf)]);
        // Bus is muted, nothing reaches master
        assert!(out[0].abs() < EPS, "Muted bus L: {}", out[0]);
        assert!(out[1].abs() < EPS, "Muted bus R: {}", out[1]);
    }

    #[test]
    fn mixer_bus_pan() {
        let mut mixer = AudioMixer::new(1);
        let bus_idx = mixer.add_bus("Panned Bus");
        mixer.tracks[0].bus = Some(bus_idx);
        mixer.buses[bus_idx].pan = 1.0; // hard right

        let buf = vec![0.6, 0.6];
        let out = mixer.mix(&[(0, &buf)]);
        assert!(out[0].abs() < EPS, "Bus pan hard-right L: {}", out[0]);
        assert!(approx(out[1], 0.6), "Bus pan hard-right R: {}", out[1]);
    }

    #[test]
    fn mixer_multiple_tracks_same_bus() {
        let mut mixer = AudioMixer::new(2);
        let bus_idx = mixer.add_bus("Group");
        mixer.tracks[0].bus = Some(bus_idx);
        mixer.tracks[1].bus = Some(bus_idx);

        let buf0 = vec![0.2, 0.2];
        let buf1 = vec![0.3, 0.3];
        let out = mixer.mix(&[(0, &buf0), (1, &buf1)]);
        // Both tracks sum in bus, bus at unity -> 0.5
        assert!(approx(out[0], 0.5), "Multi-track bus L: {}", out[0]);
        assert!(approx(out[1], 0.5), "Multi-track bus R: {}", out[1]);
    }

    #[test]
    fn mixer_bus_peak_levels_updated() {
        let mut mixer = AudioMixer::new(1);
        let bus_idx = mixer.add_bus("Metered Bus");
        mixer.tracks[0].bus = Some(bus_idx);

        let buf = vec![0.7, 0.9];
        let _ = mixer.mix(&[(0, &buf)]);
        assert!(
            mixer.buses[bus_idx].peak_left > 0.0,
            "Bus peak L should be set"
        );
        assert!(
            mixer.buses[bus_idx].peak_right > 0.0,
            "Bus peak R should be set"
        );
    }

    // ---------------------------------------------------------------
    // Mixer: RMS metering
    // ---------------------------------------------------------------

    #[test]
    fn mixer_rms_levels_computed() {
        let mut mixer = AudioMixer::new(1);
        let buf = vec![0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
        let _ = mixer.mix(&[(0, &buf)]);
        // Constant 0.5 signal: RMS should be 0.5
        assert!(approx(mixer.tracks[0].rms_left, 0.5), "RMS L: {}", mixer.tracks[0].rms_left);
        assert!(approx(mixer.tracks[0].rms_right, 0.5), "RMS R: {}", mixer.tracks[0].rms_right);
    }

    // ---------------------------------------------------------------
    // Mixer: pan law integration (mix uses correct law)
    // ---------------------------------------------------------------

    #[test]
    fn mixer_uses_configured_pan_law() {
        // With ConstantPower law and center pan, gains are cos(pi/4) = sin(pi/4) ~ 0.7071
        let mut mixer = AudioMixer::new(1);
        mixer.pan_law = PanLaw::ConstantPower;
        let buf = vec![1.0, 1.0];
        let out = mixer.mix(&[(0, &buf)]);
        let expected = std::f32::consts::FRAC_PI_4.cos();
        assert!(
            approx(out[0], expected),
            "ConstantPower mix L={}, expected {}",
            out[0],
            expected,
        );
        assert!(
            approx(out[1], expected),
            "ConstantPower mix R={}, expected {}",
            out[1],
            expected,
        );
    }

    #[test]
    fn mixer_invalid_bus_index_falls_through_to_master() {
        let mut mixer = AudioMixer::new(1);
        // No buses added, but track points to bus 99
        mixer.tracks[0].bus = Some(99);
        let buf = vec![0.5, 0.5];
        let out = mixer.mix(&[(0, &buf)]);
        // Should fall through to master
        assert!(approx(out[0], 0.5), "Invalid bus fallthrough L: {}", out[0]);
    }

    #[test]
    fn mixer_out_of_range_track_index_ignored() {
        let mut mixer = AudioMixer::new(1);
        let buf = vec![0.5, 0.5];
        // Track index 5 has no settings, should be skipped
        let out = mixer.mix(&[(5, &buf)]);
        assert!(out[0].abs() < EPS, "Unknown track should not contribute");
    }

    // ─── PDC integration tests ─────────────────────────────────────────────

    #[test]
    fn pdc_global_max_zero_with_no_latencies() {
        let mixer = AudioMixer::new(3);
        assert_eq!(mixer.pdc_global_max_latency(), 0);
        assert_eq!(mixer.pdc_compensation_per_track(), vec![0, 0, 0]);
    }

    #[test]
    fn pdc_set_track_latency_grows_global_max() {
        let mut mixer = AudioMixer::new(3);
        mixer.set_track_latency_samples(0, 64, 1024);
        mixer.set_track_latency_samples(1, 256, 1024);
        mixer.set_track_latency_samples(2, 0, 1024);
        assert_eq!(mixer.pdc_global_max_latency(), 256);
        // Compensation = max - own.
        assert_eq!(mixer.pdc_compensation_per_track(), vec![192, 0, 256]);
    }

    #[test]
    fn pdc_zero_latency_track_has_no_delay_line() {
        let mut mixer = AudioMixer::new(2);
        mixer.set_track_latency_samples(0, 0, 1024);
        mixer.set_track_latency_samples(1, 0, 1024);
        assert!(mixer.track_delay_lines[0].is_none());
        assert!(mixer.track_delay_lines[1].is_none());
    }

    #[test]
    fn pdc_recomputes_delays_when_max_changes() {
        let mut mixer = AudioMixer::new(2);
        mixer.set_track_latency_samples(0, 0, 1024);
        mixer.set_track_latency_samples(1, 64, 1024);
        // Max is 64; track 0 needs 64 samples of compensation.
        assert!(mixer.track_delay_lines[0].is_some());
        assert_eq!(
            mixer.track_delay_lines[0].as_ref().unwrap().delay_samples(),
            64
        );
        // Now bump track 1's latency higher; track 0's compensation grows.
        mixer.set_track_latency_samples(1, 256, 1024);
        assert_eq!(
            mixer.track_delay_lines[0].as_ref().unwrap().delay_samples(),
            256
        );
    }

    #[test]
    fn pdc_drops_delay_line_when_track_becomes_slowest() {
        let mut mixer = AudioMixer::new(2);
        mixer.set_track_latency_samples(0, 0, 1024);
        mixer.set_track_latency_samples(1, 64, 1024);
        // Initially track 0 has a delay line.
        assert!(mixer.track_delay_lines[0].is_some());
        // Drop track 1's latency to 0 → no compensation needed → no delay line.
        mixer.set_track_latency_samples(1, 0, 1024);
        assert!(mixer.track_delay_lines[0].is_none());
        assert!(mixer.track_delay_lines[1].is_none());
    }

    #[test]
    fn apply_per_track_pdc_delay_aligns_streams() {
        // Three tracks, latencies [0, 8, 16] frames. Synthesise an
        // impulse at frame `latency[i]` per track (the "post-chain"
        // shape the chain would produce). Apply PDC. All impulses
        // must end up at frame `max = 16`.
        let mut mixer = AudioMixer::new(3);
        let latencies = [0u32, 8, 16];
        for (i, &l) in latencies.iter().enumerate() {
            mixer.set_track_latency_samples(i, l, 64);
        }

        let frames = 64;
        let mut signals: Vec<Vec<f32>> = (0..3)
            .map(|i| {
                let mut buf = vec![0.0_f32; frames * 2];
                let pos = latencies[i] as usize;
                buf[pos * 2] = 1.0;
                buf
            })
            .collect();
        for i in 0..3 {
            mixer.apply_per_track_pdc_delay(i, &mut signals[i]);
        }
        // Every track's L impulse should now be at frame 16.
        for (i, sig) in signals.iter().enumerate() {
            assert_eq!(sig[16 * 2], 1.0, "track {i} aligned to max latency");
        }
    }

    #[test]
    fn ensure_tracks_grows_pdc_state_in_sync() {
        // Regression pin — earlier shape forgot to grow the PDC vecs
        // alongside `tracks`, which would index past the end.
        let mut mixer = AudioMixer::new(2);
        mixer.ensure_tracks(5);
        assert_eq!(mixer.tracks.len(), 5);
        assert_eq!(mixer.track_latency_samples.len(), 5);
        assert_eq!(mixer.track_delay_lines.len(), 5);
    }
}
