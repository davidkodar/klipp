//! The animated parameter in focus (#2296): the one piece of state the
//! Inspector, the Curve Editor and the Dope Sheet share about which of a
//! clip's animated parameters the user is looking at, so a pick in one
//! shows it in the others.

use crate::layout_state::InspectorTab;
use crate::state::KeyframeParamKind;
use klipp_core::ClipId;
use klipp_timeline::Clip;

/// A parameter brought into focus, and when (the UI's time in seconds,
/// for a row's highlight to fade from).
#[derive(Debug, Clone, PartialEq)]
pub struct ParamFocus {
    pub clip_id: ClipId,
    pub kind: KeyframeParamKind,
    pub at: f64,
}

/// `clip`'s animated parameters, as the Curve Editor and the Dope Sheet
/// name them, each with the Inspector tab that holds its row: placement
/// and opacity on Video, sound and sound effects on Audio, effects on
/// Effects, the grade and its masks on Color, title layers on Title.
pub fn animated(clip: &Clip) -> Vec<(KeyframeParamKind, String, InspectorTab)> {
    let grade: Vec<_> = clip.animated_grade_keys().into_iter().map(|(path, key, _)| (path, key)).collect();
    crate::panels::dope_sheet::rows(clip)
        .into_iter()
        .map(|row| {
            let tab = match &row.kind {
                KeyframeParamKind::Video(_) => InspectorTab::Video,
                KeyframeParamKind::Audio(_) | KeyframeParamKind::AudioEffect { .. } => InspectorTab::Audio,
                KeyframeParamKind::Node { path, key } if grade.iter().any(|(p, k)| p == path && k == key) => InspectorTab::Color,
                KeyframeParamKind::Node { .. } => InspectorTab::Effects,
                KeyframeParamKind::Mask(_) => InspectorTab::Color,
                KeyframeParamKind::Title { .. } => InspectorTab::Title,
            };
            (row.kind, row.label, tab)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::test_clip::TestClip;

    /// A clip's keyed opacity and position are on the Video tab, named as
    /// the Curve Editor names them; a clip with no keys has none.
    #[test]
    fn a_clips_animated_parameters_and_their_tabs() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let plain = Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        assert!(animated(&plain).is_empty());
        let keyed = TestClip::of(plain).lane("opacity", 1.0, 0.5).lane("pos_x", 1.0, 10.0).lane("pos_x", 2.0, 20.0).build();
        let found = animated(&keyed);
        let names: Vec<(&str, InspectorTab)> = found.iter().map(|(_, l, tab)| (l.as_str(), *tab)).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(found.iter().all(|(_, _, tab)| *tab == InspectorTab::Video), "{names:?}");
        assert!(found.iter().any(|(k, ..)| *k == KeyframeParamKind::Video(klipp_core::VideoParam::PositionX)));
    }
}
