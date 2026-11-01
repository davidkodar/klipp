//! Preferences that have a runtime copy in the app state: each applied to both
//! at once (#890).

use super::*;

// ── Live-sync helpers for Editing prefs (T-PREFS-LIVE-SYNC) ───────────
//
// Each helper mirrors a `prefs.editing.{field}` change to the matching
// runtime copy on `AppState`. Called from the AppCommand handler arms
// (so dispatch is the single mutation path) and from tests (which
// construct `Preferences::default()` + `AppState::default()` and pin
// the contract directly without standing up a `KlippApp` instance).
//
// The shape is uniform on purpose so future preferences with a runtime
// state copy can opt in by following the same pattern. The handler
// arm is a one-liner that calls the helper plus `prefs_dirty = true`.

pub(super) fn apply_snap_threshold_px(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: f32) {
    prefs.editing.snap_threshold_px = val;
    state.snap_threshold_px = val;
}

pub(super) fn apply_zoom_step(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: f32) {
    prefs.editing.zoom_step = val;
    state.zoom_step = val;
}

pub(super) fn apply_magnetic_timeline(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: bool) {
    // Naming asymmetry: the pref is `magnetic_timeline`, the runtime
    // copy is `magnetic_timeline_enabled`. Bridged here so the rest of
    // the codebase doesn't have to know about it. (Renaming for
    // symmetry is filed separately as T-PREFS-NAMING-CLEANUP.)
    prefs.editing.magnetic_timeline = val;
    state.magnetic_timeline_enabled = val;
}

pub(super) fn apply_default_sequence_resolution_idx(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: usize) {
    // Read at "+ new sequence" time (timeline/sequence_tabs.rs:182),
    // not on a hot path — but mid-session changes still need to
    // propagate so that immediately creating a sequence picks up the
    // new default.
    prefs.editing.default_sequence_resolution_idx = val;
    state.default_sequence_resolution_idx = val;
}

pub(super) fn apply_default_transition_frames(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: u32) {
    prefs.editing.default_transition_frames = val;
    state.default_transition_frames = val;
}

pub(super) fn apply_shift_step_frames(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: u32) {
    prefs.editing.shift_step_frames = val;
    state.shift_step_frames = val;
}

pub(super) fn apply_show_reused_footage(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: bool) {
    prefs.editing.show_reused_footage = val;
    state.show_reused_footage = val;
}

pub(super) fn apply_multicam_show_angles(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: bool) {
    prefs.editing.multicam_show_angles = val;
    state.multicam_show_angles = val;
}

#[cfg(test)]
mod prefs_apply_tests {
    use super::*;
    use crate::prefs::Preferences;

    fn fresh() -> (Preferences, AppState) {
        (Preferences::default(), AppState::default())
    }

    #[test]
    fn apply_snap_threshold_px_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        let new_val = 22.0;
        assert!((prefs.editing.snap_threshold_px - new_val).abs() > 1e-6,
            "default must differ from new_val to make the test meaningful");
        apply_snap_threshold_px(&mut prefs, &mut state, new_val);
        assert!((prefs.editing.snap_threshold_px - new_val).abs() < 1e-6);
        assert!((state.snap_threshold_px - new_val).abs() < 1e-6);
    }

    #[test]
    fn apply_zoom_step_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        let new_val = 2.0;
        assert!((prefs.editing.zoom_step - new_val).abs() > 1e-6);
        apply_zoom_step(&mut prefs, &mut state, new_val);
        assert!((prefs.editing.zoom_step - new_val).abs() < 1e-6);
        assert!((state.zoom_step - new_val).abs() < 1e-6);
    }

    #[test]
    fn apply_magnetic_timeline_bridges_naming_asymmetry() {
        // The pref is `magnetic_timeline` but state holds the runtime
        // copy as `magnetic_timeline_enabled`. Helper writes both —
        // failing this means a future rename broke the bridge.
        let (mut prefs, mut state) = fresh();
        prefs.editing.magnetic_timeline = true;
        state.magnetic_timeline_enabled = true;
        apply_magnetic_timeline(&mut prefs, &mut state, false);
        assert!(!prefs.editing.magnetic_timeline);
        assert!(!state.magnetic_timeline_enabled);
        apply_magnetic_timeline(&mut prefs, &mut state, true);
        assert!(prefs.editing.magnetic_timeline);
        assert!(state.magnetic_timeline_enabled);
    }

    #[test]
    fn apply_default_sequence_resolution_idx_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        let new_val: usize = 4;
        assert_ne!(prefs.editing.default_sequence_resolution_idx, new_val);
        apply_default_sequence_resolution_idx(&mut prefs, &mut state, new_val);
        assert_eq!(prefs.editing.default_sequence_resolution_idx, new_val);
        assert_eq!(state.default_sequence_resolution_idx, new_val);
    }

    #[test]
    fn apply_default_transition_frames_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        let new_val: u32 = 60;
        assert_ne!(prefs.editing.default_transition_frames, new_val);
        apply_default_transition_frames(&mut prefs, &mut state, new_val);
        assert_eq!(prefs.editing.default_transition_frames, new_val);
        assert_eq!(state.default_transition_frames, new_val);
    }

    #[test]
    fn apply_shift_step_frames_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        apply_shift_step_frames(&mut prefs, &mut state, 10);
        assert_eq!(prefs.editing.shift_step_frames, 10);
        assert_eq!(state.shift_step_frames, 10);
    }

    #[test]
    fn apply_multicam_show_angles_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        prefs.editing.multicam_show_angles = true;
        state.multicam_show_angles = true;
        apply_multicam_show_angles(&mut prefs, &mut state, false);
        assert!(!prefs.editing.multicam_show_angles);
        assert!(!state.multicam_show_angles);
    }
}
