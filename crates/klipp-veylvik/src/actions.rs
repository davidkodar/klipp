//! Whether a clip menu's Veylvik action does anything: decided here once,
//! for the app's handler and the control socket alike, so what one
//! refuses the other refuses too, in the same words.

use klipp_core::{CompositionClipAction as A, MediaId};
use klipp_timeline::{Clip, Sequence};

use crate::Compositions;

/// Why a clip can't be given a composition.
pub const NO_MEDIA: &str = "the clip has no media to compose with: a video or still clip has";

/// Why a clip's composition can't be changed or copied.
pub const NO_COMPOSITION: &str = "the clip has no Veylvik composition";

/// Why `action` can't be done at all, whichever clips it is on: what it
/// copies from isn't there. `copied` is whether a composition is copied;
/// `in_pool` whether the project's pool holds a media.
pub fn refusal(action: A, seq: &Sequence, compositions: &Compositions, copied: bool, in_pool: impl Fn(MediaId) -> bool) -> Option<String> {
    match action {
        A::Paste if !copied => Some("nothing is copied".into()),
        A::CopyFrom(from) => {
            let Some((t, c)) = seq.find_clip(from) else { return Some("the clip to copy from is no longer on the timeline".into()) };
            match seq.tracks[t].clips[c].composition {
                None => Some("the clip to copy from has no Veylvik composition".into()),
                Some(id) if !compositions.contains_key(&id) => Some("the project doesn't hold that clip's composition".into()),
                Some(_) => None,
            }
        }
        A::AddMedia(media) if !in_pool(media) => Some(format!("no media {} in the pool", media.0)),
        _ => None,
    }
}

/// Why `action` leaves `clip` as it is; `None` when it changes it.
/// Composition from Clips is the clips together, never one alone.
pub fn passes_over(action: A, clip: &Clip, compositions: &Compositions) -> Option<&'static str> {
    match action {
        A::FromClips => None,
        A::Copy => clip.composition.is_none().then_some(NO_COMPOSITION),
        _ if clip.media_id().is_none() => Some(NO_MEDIA),
        A::Reset | A::Remove => clip.composition.is_none().then_some(NO_COMPOSITION),
        A::OverBelow if clip.composition.and_then(|id| compositions.get(&id)).is_some_and(|b| crate::bytes_read_below(b)) => Some("the clip's composition reads the layers below it already"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{ClipId, RationalTime};
    use klipp_timeline::CompositionId;
    use std::sync::Arc;

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    fn composition(below: bool) -> Arc<[u8]> {
        let c = crate::Composition::from_host_footage(crate::CLIP_MEDIA_KEY, "Clip", None, [64, 36], crate::Time { ticks: 24, rate: 1 }, 24).unwrap();
        let bytes: Arc<[u8]> = serde_json::to_vec(&c).unwrap().into();
        if below { crate::below::over_below(&bytes).unwrap() } else { bytes }
    }

    /// Each action passes over a clip it can't change, saying why, and
    /// changes the others: no media to compose with, no composition to
    /// reset, remove or copy, the layers below drawn under it already.
    #[test]
    fn each_action_says_why_it_passes_a_clip_over() {
        let (plain_id, below_id) = (CompositionId::new(), CompositionId::new());
        let compositions: Compositions = [(plain_id, composition(false)), (below_id, composition(true))].into();
        let bare = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let composed = Clip { composition: Some(plain_id), ..bare.clone() };
        let over = Clip { composition: Some(below_id), ..bare.clone() };
        let gap = Clip::gap(t(0.0), t(2.0));

        for action in [A::Paste, A::CopyFrom(ClipId::new()), A::Reset, A::Remove, A::OverBelow, A::AddMedia(MediaId::new())] {
            assert_eq!(passes_over(action, &gap, &compositions), Some(NO_MEDIA), "{action:?} on a clip without media");
        }
        for action in [A::Reset, A::Remove, A::Copy] {
            assert_eq!(passes_over(action, &bare, &compositions), Some(NO_COMPOSITION), "{action:?} with no composition");
            assert_eq!(passes_over(action, &composed, &compositions), None, "{action:?} with one");
        }
        assert!(passes_over(A::OverBelow, &over, &compositions).is_some(), "over the layers below already");
        for clip in [&bare, &composed] {
            assert_eq!(passes_over(A::OverBelow, clip, &compositions), None);
            assert_eq!(passes_over(A::AddMedia(MediaId::new()), clip, &compositions), None);
            assert_eq!(passes_over(A::Paste, clip, &compositions), None);
        }
        assert_eq!(passes_over(A::FromClips, &gap, &compositions), None, "the clips together");
    }

    /// An action whose source is missing is refused before any clip: a
    /// paste with nothing copied, a copy from a clip that is gone or has
    /// no composition the project holds, media not in the pool.
    #[test]
    fn an_action_whose_source_is_missing_is_refused() {
        let held = CompositionId::new();
        let compositions: Compositions = [(held, composition(false))].into();
        let mut seq = Sequence::new_default();
        let bare = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let composed = Clip { composition: Some(held), ..Clip::media(MediaId::new(), t(2.0), t(4.0), t(0.0), t(2.0)) };
        let lost = Clip { composition: Some(CompositionId::new()), ..Clip::media(MediaId::new(), t(4.0), t(6.0), t(0.0), t(2.0)) };
        let (bare_id, composed_id, lost_id) = (bare.id, composed.id, lost.id);
        seq.tracks[0].clips = vec![bare, composed, lost];
        let media = MediaId::new();
        let refused = |action, copied| refusal(action, &seq, &compositions, copied, |m| m == media);

        assert!(refused(A::Paste, false).is_some());
        assert_eq!(refused(A::Paste, true), None);
        assert!(refused(A::CopyFrom(ClipId::new()), false).is_some(), "gone");
        assert!(refused(A::CopyFrom(bare_id), false).is_some(), "no composition");
        assert!(refused(A::CopyFrom(lost_id), false).is_some(), "not held");
        assert_eq!(refused(A::CopyFrom(composed_id), false), None);
        assert!(refused(A::AddMedia(MediaId::new()), false).is_some());
        assert_eq!(refused(A::AddMedia(media), false), None);
        for action in [A::Copy, A::Reset, A::Remove, A::OverBelow, A::FromClips] {
            assert_eq!(refused(action, false), None, "{action:?} needs no source");
        }
    }
}
