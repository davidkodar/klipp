//! The sound of the clip open on the Veylvik page, for Veylvik's editor to
//! play with its footage: the sound linked to the clip, mixed as Edit
//! plays it (the clip's gain, fades, effects and channel layout; at unity
//! and centred, as a bounced track hears it), sample for sample at the
//! picture it plays with.
//!
//! Veylvik asks in the media's own time. At the clip's own speed a moment
//! of media is a moment of timeline, so the answer is the timeline's mix
//! over the same stretch, the picture's offset from its sound kept. A
//! retimed or reversed clip's sound plays at its speed on Edit and the
//! page plays media at its own; those give none until the page has the
//! clip's time.

use klipp_core::{ClipId, MediaId};
use klipp_media::MediaPool;
use klipp_render::{AudioMixdown, SequenceSnapshot};
use klipp_timeline::Sequence;
use veylvik_audio::Pcm;

/// One clip's sound, frozen when the page opens it (and again when the
/// timeline changes under it).
pub struct ClipSound {
    snapshot: SequenceSnapshot,
    /// The clip's first moment on the timeline, and the media's there.
    timeline_in: f64,
    source_in: f64,
    /// The footage's media; the sound is only of its key's.
    media: MediaId,
}

impl ClipSound {
    /// The sound linked to video clip `clip` of `seq`, `None` when it has
    /// none, is retimed, or can't be read.
    pub fn new(seq: &Sequence, all: &[Sequence], pool: &MediaPool, clip: ClipId) -> Option<Self> {
        let c = seq.tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == clip)?;
        if c.speed != 1.0 || !c.speed_keyframes.is_empty() || c.is_reversed() {
            return None;
        }
        let media = c.media_id()?;
        let snapshot = match klipp_render::stem::clip_sound_snapshot(seq, all, pool, clip) {
            Ok(s) => s?,
            Err(e) => {
                tracing::warn!("the Veylvik page's sound: {e}");
                return None;
            }
        };
        Some(Self { snapshot, timeline_in: c.timeline_in.to_secs_f64(), source_in: c.source_in.to_secs_f64(), media })
    }

    /// The media this is the sound of.
    pub fn media(&self) -> MediaId {
        self.media
    }

    /// The sound from `window[0]` to `window[1]` seconds of the media's
    /// time at `rate`, stereo and interleaved, as `Host::media_sound`
    /// answers: silence where the clip's sound is not.
    pub fn pcm(&self, window: [f64; 2], rate: u32) -> Option<Pcm> {
        let [from, to] = window;
        if !(from.is_finite() && to.is_finite()) || to <= from || rate == 0 {
            return None;
        }
        let frames = ((to - from) * f64::from(rate)).round() as usize;
        // A few minutes at most: one mix's playback range.
        if frames > rate as usize * 60 * 30 {
            return None;
        }
        let mut snap = self.snapshot.clone();
        snap.sample_rate = rate;
        let mut mix = AudioMixdown::new(&snap).ok()?;
        let start = self.timeline_in + (from - self.source_in);
        let first = (start * f64::from(rate)).round() as i64;
        let mut samples = vec![0.0f32; frames * 2];
        let fps = snap.fps.max(1.0);
        let frame_of = |secs: f64| (secs * fps).floor().max(0.0) as u64;
        let last = (start + (to - from)) * fps;
        for f in frame_of(start)..=(last.ceil().max(0.0) as u64) {
            let (s0, _) = mix.frame_sample_range(f);
            let audio = mix.get_frame_audio_unclamped(f);
            for (k, pair) in audio.as_chunks::<2>().0.iter().enumerate() {
                let at = s0 as i64 + k as i64 - first;
                if at >= 0 && (at as usize) < frames {
                    samples[at as usize * 2] = pair[0];
                    samples[at as usize * 2 + 1] = pair[1];
                }
            }
        }
        Some(Pcm { rate, start: from, samples })
    }
}
