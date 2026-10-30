//! FPU denormal-flushing for audio DSP threads.
//!
//! Biquad EQ, compressors, reverb tails, and noise-reduction state can drop
//! into denormal float range when the input goes silent (e.g. between
//! clips, or during a fade-out). On x86, denormal arithmetic falls into
//! microcode and runs ~50–100× slower than normal — a single biquad in a
//! denormal state is enough to overrun the audio callback budget on a
//! moderately busy track, producing dropouts that look mysterious to the
//! user.
//!
//! The fix is a single SSE intrinsic that flips two MXCSR bits:
//!
//!   * **FZ** (Flush-to-Zero) — denormal RESULTS get clamped to 0.
//!   * **DAZ** (Denormals-Are-Zero) — denormal INPUTS read as 0.
//!
//! Both flags are per-thread on x86_64; setting them once at the top of
//! every DSP thread is enough. AArch64 has one bit for both, FPCR.FZ
//! (bit 24): with it set, denormal inputs and results are both flushed,
//! and it too is per-thread. Cost on subsequent samples is zero — the
//! flush happens transparently in the FPU.
//!
//! Call sites (audio threads only — UI / engine
//! main threads don't run DSP and don't need the flag):
//!   * `klipp-engine/src/audio_decoder.rs::audio_decode_thread`
//!   * `klipp-engine/src/pcm_reader.rs::pcm_feed_thread`
//!   * `klipp-engine/src/audio_output.rs` cpal output callback (defensive
//!     — even if the mixer ran upstream, the callback does its own
//!     volume/pan math that can produce denormals).

/// Configure the calling thread's FPU to flush denormal floats to zero.
///
/// On `x86_64`, sets MXCSR's FZ + DAZ bits via `_mm_setcsr`; on
/// `aarch64`, sets FPCR.FZ. On other architectures this is a no-op: the
/// slow path is harmless, just slower DSP on silent biquads.
///
/// Idempotent and cheap (one MXCSR write). Calling once at thread
/// start is sufficient; calling per callback is also fine.
#[inline]
pub fn flush_denormals_to_zero() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        // MXCSR layout (relevant bits):
        //   bit 6  = DAZ (Denormals-Are-Zero, treats denormal inputs as 0)
        //   bit 15 = FZ  (Flush-to-Zero, flushes denormal results to 0)
        // Other bits we leave at default (0x1F80: rounding mode = nearest,
        // exception masks all set so we don't trap on ordinary FP ops).
        //
        // `_mm_setcsr` was deprecated in newer Rust in favour of inline
        // asm; we follow the recommended pattern using `ldmxcsr`.
        const MXCSR_DEFAULT_PLUS_FTZ_DAZ: u32 = 0x1F80 | (1 << 15) | (1 << 6);
        let csr = MXCSR_DEFAULT_PLUS_FTZ_DAZ;
        core::arch::asm!(
            "ldmxcsr [{}]",
            in(reg) &csr,
            options(readonly, nostack, preserves_flags),
        );
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        // FPCR bit 24 = FZ. The other bits (rounding mode, trap enables)
        // are left as they are.
        const FPCR_FZ: u64 = 1 << 24;
        let mut fpcr: u64;
        core::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
        fpcr |= FPCR_FZ;
        core::arch::asm!("msr fpcr, {}", in(reg) fpcr, options(nomem, nostack, preserves_flags));
    }
    // Other arches: no-op. See module doc.
}

/// Read the calling thread's FPCR (aarch64 only), for the test below.
#[cfg(all(test, target_arch = "aarch64"))]
fn read_fpcr() -> u64 {
    let fpcr: u64;
    unsafe {
        core::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
    }
    fpcr
}

/// Read the calling thread's MXCSR (x86_64 only). Used by the
/// regression test below; not part of the public hot-path API.
#[cfg(all(test, target_arch = "x86_64"))]
fn read_mxcsr() -> u32 {
    let mut csr: u32 = 0;
    unsafe {
        core::arch::asm!(
            "stmxcsr [{}]",
            in(reg) &mut csr,
            options(nostack, preserves_flags),
        );
    }
    csr
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On x86_64, after `flush_denormals_to_zero()` the FZ and DAZ bits
    /// must both be set in MXCSR. Pinning the contract so a future
    /// "let me clean up that low-level intrinsic" surfaces in CI rather
    /// than as a mysterious dropout in someone's silent-track export.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn flush_denormals_sets_fz_and_daz_bits_on_x86_64() {
        flush_denormals_to_zero();
        let csr = read_mxcsr();
        assert!(csr & (1 << 6) != 0, "DAZ bit (6) must be set after flush_denormals_to_zero");
        assert!(csr & (1 << 15) != 0, "FZ bit (15) must be set after flush_denormals_to_zero");
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn flush_denormals_sets_fz_bit_on_aarch64() {
        flush_denormals_to_zero();
        assert!(read_fpcr() & (1 << 24) != 0, "FPCR.FZ (bit 24) must be set after flush_denormals_to_zero");
    }

    /// Behavioural pin: with FZ on, multiplying a denormal by a normal
    /// value gives 0 instead of a denormal result. This is the actual
    /// behaviour that saves the biquad state from sliding into slow
    /// land on silent input.
    ///
    /// `black_box` is required: without it, rustc evaluates the
    /// multiplication at compile time using its software-FP path,
    /// which DOESN'T consult MXCSR, and the test produces a denormal
    /// constant regardless of FZ. We need the multiply to happen at
    /// runtime on the actual FPU.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn flush_denormals_makes_arithmetic_round_denormals_to_zero() {
        use std::hint::black_box;
        flush_denormals_to_zero();
        // f32::MIN_POSITIVE = 2^-126, the smallest *normal* f32. Half
        // of that is the largest denormal. Multiplying it by 0.5
        // produces a smaller denormal — which FZ rounds to 0.
        let denormal: f32 = black_box(f32::MIN_POSITIVE) * black_box(0.5);
        let result: f32 = black_box(denormal) * black_box(0.5);
        assert_eq!(result, 0.0,
            "with FZ set, a denormal × 0.5 must flush to exactly 0.0 (got {result:e})");
    }

    /// Elsewhere the function is a no-op and the test just confirms
    /// it can be called without panicking.
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[test]
    fn flush_denormals_is_a_safe_noop_elsewhere() {
        flush_denormals_to_zero();
    }
}
