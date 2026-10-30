//! DSP effects, loudness metering, and the multi-track audio mixer.
//!
//! # Mixer + routing
//!
//! [`mixer::AudioMixer`] is the engine's per-playback mixer. It holds
//! one staging ring per active track, applies per-track [`AudioTrackSettings`]
//! (solo / mute / pan / gain), and produces an interleaved stereo sum
//! for the cpal output ring. The engine's `mix_thread` calls into the
//! mixer and also runs the [`LufsMeter`] + [`TruePeakDetector`] against
//! the summed mix before it leaves the host.
//!
//! # DSP effects ([`dsp`])
//!
//! All DSP primitives share a small sample-by-sample trait so they can
//! be chained freely. The biquad family (parametric EQ, de-esser BPF,
//! noise-reduction band-split) uses Direct Form II Transposed from the
//! RBJ cookbook — numerically stable but susceptible to FPU-denormal
//! stalls on silence. The mix thread sets FTZ + DAZ at startup to
//! avoid those stalls.
//!
//! # Metering
//!
//! * [`lufs`] — BS.1770-4 K-weighted loudness. 400 ms blocks / 100 ms
//!   hop, proper two-pass gating (absolute −70 LU, relative −10 LU),
//!   LRA via P10/P95 on EBU Tech 3342 short-term history.
//! * [`true_peak`] — 4× polyphase FIR oversampling with a 48-tap
//!   Hann-windowed sinc prototype. Self-documented as an
//!   "approximation of BS.1770-4 Annex 2"; wants a golden-file
//!   regression against a reference meter.
//! * [`spectrum`] — Hann-windowed FFT via `realfft`, peak-binned to log
//!   frequency for UI scope display.
//!
//! # LADSPA
//!
//! [`ladspa`] is the third-party-plugin bridge. `scan_ladspa_library`
//! dlopens a `.so`, walks descriptors, wraps them as `AudioEffect`
//! implementations. Discovery is `catch_unwind`-guarded so one broken
//! plugin can't abort a scan.

pub mod denormal;
pub mod dsp;
pub mod clap;
pub mod ladspa;
pub mod lv2;
pub mod lufs;
pub mod pdc;
pub mod routing;
pub mod mixer;
pub mod narration;
pub mod spectrum;
pub mod synth;
pub mod true_peak;

pub use denormal::flush_denormals_to_zero;
pub use lufs::{LufsMeter, LufsReadings};
pub use mixer::{AudioMixer, AudioTrackSettings, SendLane};
pub use synth::{synthesise_whoosh, write_whoosh_wav, WhooshVariant};
pub use true_peak::TruePeakDetector;
