/// Per-track audio settings for the mixer.
#[derive(Debug, Clone)]
pub struct AudioTrackSettings {
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub peak_left: f32,
    pub peak_right: f32,
    /// Optional bus assignment (index into AudioMixer::buses). None = direct to master.
    pub bus: Option<usize>,
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
            bus: None,
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

/// Mixes multiple audio tracks into a single stereo output.
/// Supports optional submix buses: tracks can route to a bus, and buses route to master.
pub struct AudioMixer {
    pub tracks: Vec<AudioTrackSettings>,
    pub buses: Vec<AudioBus>,
    pub master_volume: f32,
}

impl AudioMixer {
    pub fn new(track_count: usize) -> Self {
        Self {
            tracks: vec![AudioTrackSettings::default(); track_count],
            buses: Vec::new(),
            master_volume: 1.0,
        }
    }

    /// Add a submix bus and return its index.
    pub fn add_bus(&mut self, name: &str) -> usize {
        let idx = self.buses.len();
        self.buses.push(AudioBus::new(name));
        idx
    }

    /// Ensure at least `count` track settings exist.
    pub fn ensure_tracks(&mut self, count: usize) {
        while self.tracks.len() < count {
            self.tracks.push(AudioTrackSettings::default());
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

        for &(track_idx, buf) in track_buffers.iter() {
            let settings = match self.tracks.get_mut(track_idx) {
                Some(s) => s,
                None => continue,
            };
            if settings.muted {
                settings.peak_left = 0.0;
                settings.peak_right = 0.0;
                continue;
            }
            if any_solo && !settings.solo {
                settings.peak_left = 0.0;
                settings.peak_right = 0.0;
                continue;
            }

            let pan_l = if settings.pan <= 0.0 { 1.0 } else { 1.0 - settings.pan };
            let pan_r = if settings.pan >= 0.0 { 1.0 } else { 1.0 + settings.pan };

            let mut peak_l = 0.0f32;
            let mut peak_r = 0.0f32;

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
                let out_l = l * settings.volume * pan_l;
                let out_r = r * settings.volume * pan_r;
                peak_l = peak_l.max(out_l.abs());
                peak_r = peak_r.max(out_r.abs());
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
        }

        // Mix buses into master
        for (bus_idx, bus_buf) in bus_buffers.iter().enumerate() {
            if let Some(bus) = self.buses.get_mut(bus_idx) {
                if bus.muted { continue; }
                let pan_l = if bus.pan <= 0.0 { 1.0 } else { 1.0 - bus.pan };
                let pan_r = if bus.pan >= 0.0 { 1.0 } else { 1.0 + bus.pan };
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
    use super::*;

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
}
