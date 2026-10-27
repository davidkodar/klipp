use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::Arc;

use crate::audio_decoder::AudioDecoder;
use crate::pcm_reader::PcmAudioFeeder;

/// Settings for a source's running effect chain, waiting for its feed
/// thread to pick them up between blocks (see
/// [`klipp_audio::dsp::AudioEffectChain::apply_settings`]).
pub type EffectSettingsSlot = Arc<std::sync::Mutex<Option<Vec<klipp_core::AudioEffectInstance>>>>;

/// Unified audio source — either a real-time ffmpeg decoder or a PCM cache reader.
/// Provides the same interface for the engine regardless of backend.
pub enum AudioSource {
    Decoder(AudioDecoder),
    PcmFeeder(PcmAudioFeeder),
}

impl AudioSource {
    pub fn seek(&self, secs: f64) {
        match self {
            Self::Decoder(d) => d.seek(secs),
            Self::PcmFeeder(f) => f.seek(secs),
        }
    }

    /// Hand new effect settings to the running chain; applied in place
    /// before the next block, with no restart.
    pub fn update_effect_settings(&self, effects: Vec<klipp_core::AudioEffectInstance>) {
        let slot = match self {
            Self::Decoder(d) => &d.effect_settings,
            Self::PcmFeeder(f) => &f.effect_settings,
        };
        if let Ok(mut pending) = slot.lock() {
            *pending = Some(effects);
        }
    }

    pub fn resume(&self) {
        match self {
            Self::Decoder(d) => d.resume(),
            Self::PcmFeeder(f) => f.resume(),
        }
    }

    pub fn pause(&self) {
        match self {
            Self::Decoder(d) => d.pause(),
            Self::PcmFeeder(f) => f.pause(),
        }
    }

    pub fn stop(&mut self) {
        match self {
            Self::Decoder(d) => d.stop(),
            Self::PcmFeeder(f) => f.stop(),
        }
    }

    pub fn stop_detached(&mut self) {
        match self {
            Self::Decoder(d) => d.stop_detached(),
            Self::PcmFeeder(f) => f.stop_detached(),
        }
    }

    pub fn volume(&self) -> &Arc<AtomicU32> {
        match self {
            Self::Decoder(d) => &d.volume,
            Self::PcmFeeder(f) => &f.volume,
        }
    }

    pub fn muted(&self) -> &Arc<AtomicBool> {
        match self {
            Self::Decoder(d) => &d.muted,
            Self::PcmFeeder(f) => &f.muted,
        }
    }

    pub fn post_seek_pts_us(&self) -> &Arc<AtomicU64> {
        match self {
            Self::Decoder(d) => &d.post_seek_pts_us,
            Self::PcmFeeder(f) => &f.post_seek_pts_us,
        }
    }

    pub fn seek_generation(&self) -> &Arc<AtomicU64> {
        match self {
            Self::Decoder(d) => &d.seek_generation,
            Self::PcmFeeder(f) => &f.seek_generation,
        }
    }

    pub fn is_pcm(&self) -> bool {
        matches!(self, Self::PcmFeeder(_))
    }

    /// Play a short audio chirp for scrub/frame step. Only works with PCM feeder.
    pub fn chirp(&self, position_secs: f64, duration_secs: f64) {
        if let Self::PcmFeeder(f) = self {
            f.chirp(position_secs, duration_secs);
        }
    }

    /// Set fade parameters for per-clip fade in/out.
    /// Curve values: 0=Linear, 1=Exponential, 2=Logarithmic, 3=S-Curve
    pub fn set_fade(
        &self, source_in_secs: f64, clip_duration_secs: f64,
        fade_in: f64, fade_out: f64,
        fade_in_curve: u32, fade_out_curve: u32,
        gain: f32,
    ) {
        if let Self::PcmFeeder(f) = self {
            f.set_fade(source_in_secs, clip_duration_secs, fade_in, fade_out, fade_in_curve, fade_out_curve, gain);
        }
    }

    /// Read the clip's sound at `speed` (#642); the live decoder reads at
    /// 1x only, and the engine gives a retimed clip a PCM source.
    pub fn set_varispeed(&self, speed: f64, source_in: f64, source_out: f64, keep_pitch: bool) {
        if let Self::PcmFeeder(f) = self {
            f.set_varispeed(speed, source_in, source_out, keep_pitch);
        }
    }

    /// Run `frames` through the effects from where the feed was put and
    /// drop them before anything is heard (#2793): the PCM feeder's
    /// alone, which is the only one that rings a tail.
    pub fn discard(&self, frames: usize) {
        if let Self::PcmFeeder(f) = self {
            f.discard(frames);
        }
    }

    /// Where the clip's sound stops and its effects ring on (#2780): the
    /// PCM feeder's alone; the live decoder stops dead at the out point.
    pub fn ring(&self) -> Option<&std::sync::Arc<crate::pcm_reader::Ring>> {
        match self {
            Self::PcmFeeder(f) => Some(&f.ring),
            _ => None,
        }
    }

    /// Enable/disable anti-click fade at clip boundaries.
    pub fn set_anti_click(&self, enabled: bool) {
        if let Self::PcmFeeder(f) = self {
            f.set_anti_click(enabled);
        }
    }
}
