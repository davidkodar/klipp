//! Plugin Delay Compensation primitives.
//!
//! Forward-PDC building blocks for use by [`crate::AudioMixer`]:
//!
//! - [`TrackDelayLine`] — fixed-delay ring buffer applied to a track's
//!   interleaved-stereo audio before it reaches the mix bus.
//! - [`max_latency_across_tracks`] — pure helper that returns
//!   `max(track_latencies)`. Each track receives a delay line sized
//!   `max - own_track_latency` so all tracks reach the bus aligned.
//!
//! The mix thread does the wiring: it sizes each track's delay line
//! from the latencies the effect chains advertise, and feeds the
//! resulting offset to the master clock
//! (`MasterClock::set_pdc_offset_us`) so picture shifts with the
//! sound rather than drifting away from it.

/// Stereo per-track delay line backed by a power-of-two ring buffer.
/// Operates in-place on interleaved-stereo `&mut [f32]` blocks so it
/// slots directly into the existing mixer pipeline without extra
/// allocations on the audio thread.
///
/// ```ignore
/// let mut delay = TrackDelayLine::new(64, 1024);  // 64 samples, max 1024 per block
/// let mut block = vec![0.5f32; 256 * 2];          // 256 stereo frames
/// delay.process_in_place(&mut block);             // first 64 frames are silence
/// ```
#[derive(Debug)]
pub struct TrackDelayLine {
    /// Stereo-interleaved ring buffer. Length = 2 × `capacity`.
    buffer: Vec<f32>,
    /// Capacity in stereo frames (not samples).
    capacity: usize,
    /// Current write position (in stereo frames).
    write_pos: usize,
    /// Configured delay in stereo frames. 0 = passthrough (process is
    /// a no-op). May change over time via `set_delay_samples`; on
    /// resize the ring buffer is grown but never shrunk so existing
    /// in-flight samples stay valid.
    delay_samples: usize,
}

impl TrackDelayLine {
    /// Construct a delay line for the given delay (in stereo frames)
    /// with capacity for at least `max_block_samples` frames per
    /// process call. Capacity rounds up to the next power of two so
    /// the wrap-around math is a bit-mask, not a modulo.
    pub fn new(delay_samples: usize, max_block_samples: usize) -> Self {
        let capacity = (delay_samples + max_block_samples + 1)
            .next_power_of_two()
            .max(1024);
        Self {
            buffer: vec![0.0; capacity * 2],
            capacity,
            write_pos: 0,
            delay_samples,
        }
    }

    /// Currently-configured delay in stereo frames.
    pub fn delay_samples(&self) -> usize {
        self.delay_samples
    }

    /// Resize the configured delay. Grows the buffer if the new
    /// requirement exceeds capacity. Never shrinks — keeps old
    /// in-flight samples valid for as long as their delay arc fits.
    pub fn set_delay_samples(&mut self, new_delay: usize, max_block_samples: usize) {
        if new_delay + max_block_samples + 1 > self.capacity {
            // Need to grow. Allocate a fresh buffer (cheap one-shot
            // cost when the chain changes; not on the audio thread's
            // hot path).
            let new_capacity = (new_delay + max_block_samples + 1)
                .next_power_of_two()
                .max(1024);
            let mut fresh = vec![0.0; new_capacity * 2];
            // Copy existing samples into the new buffer in linear order.
            for f in 0..self.capacity {
                let src_idx = (self.write_pos + self.capacity - self.delay_samples + f)
                    % self.capacity;
                fresh[f * 2] = self.buffer[src_idx * 2];
                fresh[f * 2 + 1] = self.buffer[src_idx * 2 + 1];
            }
            self.buffer = fresh;
            self.capacity = new_capacity;
            self.write_pos = self.delay_samples; // align so the next read returns valid data
        }
        self.delay_samples = new_delay;
    }

    /// Apply the configured delay to an interleaved-stereo block in
    /// place. Buffer length must be a multiple of 2; odd-length tails
    /// are silently ignored (defensive — a malformed caller can't
    /// poison the ring).
    pub fn process_in_place(&mut self, samples: &mut [f32]) {
        if self.delay_samples == 0 || samples.len() < 2 {
            return;
        }
        let pairs = samples.len() / 2;
        let cap_mask = self.capacity - 1; // power-of-two by construction

        for i in 0..pairs {
            // Write the current input to the ring.
            let w = self.write_pos & cap_mask;
            self.buffer[w * 2] = samples[i * 2];
            self.buffer[w * 2 + 1] = samples[i * 2 + 1];
            // Read the delayed sample (delay_samples behind the write).
            let r = (self.write_pos + self.capacity - self.delay_samples) & cap_mask;
            samples[i * 2] = self.buffer[r * 2];
            samples[i * 2 + 1] = self.buffer[r * 2 + 1];
            self.write_pos = (self.write_pos + 1) & cap_mask;
        }
    }

    /// Reset the ring buffer to silence + write position to 0.
    /// Called when the chain is reset / rebuilt so stale samples
    /// don't bleed across the boundary.
    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.write_pos = 0;
    }
}

/// Returns the global max latency across the given per-track
/// reports. The mixer uses this to size each track's delay line
/// (max minus own).
pub fn max_latency_across_tracks(per_track_latency: &[u32]) -> u32 {
    per_track_latency.iter().copied().max().unwrap_or(0)
}

/// Compute the per-track delay each track needs in order to align
/// with the slowest track. Length matches the input.
pub fn compensation_per_track(per_track_latency: &[u32]) -> Vec<u32> {
    let global_max = max_latency_across_tracks(per_track_latency);
    per_track_latency
        .iter()
        .map(|&own| global_max.saturating_sub(own))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_delay_is_passthrough() {
        let mut line = TrackDelayLine::new(0, 256);
        let mut block = vec![1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0]; // 3 stereo frames
        line.process_in_place(&mut block);
        assert_eq!(block, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn delay_silences_first_n_frames() {
        // 4-frame delay: first 4 frames of output should be 0; samples
        // 5+ should be the original input shifted by 4.
        let mut line = TrackDelayLine::new(4, 256);
        let mut block: Vec<f32> = (0..16).map(|i| i as f32).collect(); // 8 frames L/R
        // Block layout: [0L, 0R, 1L, 1R, ..., 7L, 7R]
        line.process_in_place(&mut block);
        // First 4 stereo frames (8 samples) should be 0.
        for s in &block[..8] {
            assert_eq!(*s, 0.0);
        }
        // Samples 8.. should be the original first 4 frames.
        assert_eq!(&block[8..], &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn delay_steady_state_one_sample() {
        // After one delay's worth of samples, the line is at steady state.
        // Push 4 frames of 1.0, then 4 frames of 2.0; with delay=4, the
        // output for the second batch should be 1.0s.
        let mut line = TrackDelayLine::new(4, 256);
        let mut warmup = vec![1.0_f32; 8];
        line.process_in_place(&mut warmup);
        // After warmup, the ring holds 4 1.0-frames behind the write.
        let mut probe = vec![2.0_f32; 8];
        line.process_in_place(&mut probe);
        // First 4 output frames (8 samples) should be 1.0 (the warmup).
        assert_eq!(&probe[..8], &[1.0_f32; 8]);
    }

    #[test]
    fn reset_clears_ring() {
        let mut line = TrackDelayLine::new(4, 256);
        let mut block = vec![1.0_f32; 16];
        line.process_in_place(&mut block);
        line.reset();
        // After reset, the next 4-frame read should be silence.
        let mut probe = vec![5.0_f32; 8];
        line.process_in_place(&mut probe);
        assert_eq!(probe, vec![0.0_f32; 8]);
    }

    #[test]
    fn set_delay_samples_grows_capacity() {
        let mut line = TrackDelayLine::new(4, 256);
        let original_capacity = line.capacity;
        // Request a delay larger than initial capacity.
        line.set_delay_samples(2048, 1024);
        assert!(line.capacity > original_capacity);
        // After resize, behaviour with the new delay is correct.
        let mut probe = vec![1.0_f32; 8];
        line.process_in_place(&mut probe);
        // 4-frame block at delay=2048 → all silence.
        assert_eq!(probe, vec![0.0_f32; 8]);
    }

    #[test]
    fn delay_samples_getter_reflects_state() {
        let line = TrackDelayLine::new(64, 256);
        assert_eq!(line.delay_samples(), 64);
    }

    #[test]
    fn max_latency_across_zero_when_empty() {
        assert_eq!(max_latency_across_tracks(&[]), 0);
    }

    #[test]
    fn max_latency_across_picks_largest() {
        assert_eq!(max_latency_across_tracks(&[0, 64, 256, 128]), 256);
    }

    #[test]
    fn compensation_per_track_max_minus_own() {
        // Three tracks at [0, 64, 256] → max = 256 → compensation =
        // [256, 192, 0]. Slowest track gets 0 added delay; faster
        // tracks get the difference.
        assert_eq!(
            compensation_per_track(&[0, 64, 256]),
            vec![256, 192, 0]
        );
    }

    #[test]
    fn compensation_per_track_all_zero_when_no_latency() {
        // Bare project (no plugins anywhere) → every track gets 0
        // compensation → no delay lines fire.
        assert_eq!(
            compensation_per_track(&[0, 0, 0, 0]),
            vec![0, 0, 0, 0]
        );
    }

    #[test]
    fn compensation_per_track_handles_overflow_gracefully() {
        // saturating_sub guards against (own > max) which can't
        // happen in normal operation but should not panic if it ever
        // does (e.g. variable-latency mid-recompute race).
        let comp = compensation_per_track(&[u32::MAX, 0]);
        assert_eq!(comp, vec![0, u32::MAX]);
    }

    /// Pin the alignment guarantee: with three tracks at differing
    /// latency, applying the per-track compensation aligns their
    /// output streams sample-for-sample. Synthesised here as silence
    /// plus an impulse on each track at frame 0; the compensated streams
    /// must put their impulses at the same global frame offset
    /// (= max_latency).
    #[test]
    fn alignment_via_compensation_pins_the_invariant() {
        let latencies = [0u32, 8, 16];
        let comp = compensation_per_track(&latencies);
        // Each track has its own delay line sized to comp[i].
        let mut lines: Vec<TrackDelayLine> = comp
            .iter()
            .map(|&d| TrackDelayLine::new(d as usize, 64))
            .collect();
        // Per-track input: impulse at frame 0 of each track's PRE-chain
        // signal. After the chain, each track's effective signal sits
        // at frame `latency[i]` (the chain's intrinsic delay). After
        // PDC, all impulses should end up at frame `max = 16`.
        //
        // For the test we synthesise the post-chain signal directly:
        // an impulse at frame `latencies[i]` of a 64-frame block.
        let frames = 64;
        let mut signals: Vec<Vec<f32>> = (0..3)
            .map(|i| {
                let mut buf = vec![0.0f32; frames * 2];
                let pos = latencies[i] as usize;
                buf[pos * 2] = 1.0; // L channel only is fine for the test
                buf
            })
            .collect();
        // Apply PDC to each.
        for (i, line) in lines.iter_mut().enumerate() {
            line.process_in_place(&mut signals[i]);
        }
        // After PDC, every track's L impulse should be at frame
        // `max = 16`.
        for (i, sig) in signals.iter().enumerate() {
            assert_eq!(
                sig[16 * 2], 1.0,
                "track {i} impulse must land at the aligned frame"
            );
            // And no other 1.0 should appear in [0..32] (the impulse
            // should not be duplicated or shifted elsewhere).
            for f in 0..32 {
                if f == 16 {
                    continue;
                }
                assert_eq!(
                    sig[f * 2], 0.0,
                    "track {i} frame {f} should be zero, found {}",
                    sig[f * 2]
                );
            }
        }
    }
}
