//! The sequence camera's keys: set, dragged, moved, and their interpolation and
//! tangents (#890).

#[allow(unused_imports)]
use super::*;

/// One camera key set or cleared, on the list the param names.
///
/// Free of the app so it can be tested without one. The insert and the
/// remove are `klipp_core::keyframe`'s, so what counts as "the same
/// moment" stays one decision in one place.
pub(super) fn apply_camera_key(
    camera: &mut klipp_timeline::Camera,
    param: klipp_core::CameraParam,
    time_secs: f64,
    value: Option<f32>,
) {
    let keys = match param {
        klipp_core::CameraParam::X => &mut camera.x_keyframes,
        klipp_core::CameraParam::Y => &mut camera.y_keyframes,
        klipp_core::CameraParam::Zoom => &mut camera.zoom_keyframes,
        klipp_core::CameraParam::Roll => &mut camera.roll_keyframes,
    };
    match value {
        Some(v) => klipp_core::keyframe::upsert_keyframe(keys, time_secs, v),
        None => klipp_core::keyframe::remove_keyframe_at(keys, time_secs),
    }
}

/// The history key a drag on this sequence's camera keys is coalesced
/// under: every move and handle edit between the drag's begin and end
/// goes into one entry.
pub(super) fn camera_drag_key(sequence_idx: usize) -> String {
    format!("camera:{sequence_idx}")
}

/// Carry the camera key at `index` on one list to another moment and
/// value; the list is re-sorted, so the key may come out at another
/// index. False when there is no such key.
pub(super) fn move_camera_key(camera: &mut klipp_timeline::Camera, param: klipp_core::CameraParam, index: usize, new_time_secs: f64, new_value: f32) -> bool {
    klipp_core::keyframe::move_keyframe(crate::timeline::camera_lane::keys_mut(camera, param), index, new_time_secs, new_value)
}

/// Set how the curve leaves the key at `index`, and the key before it,
/// so the segments either side of the diamond take the shape — the
/// rule a clip's keyframes follow.
pub(super) fn set_camera_key_interpolation(camera: &mut klipp_timeline::Camera, param: klipp_core::CameraParam, index: usize, interpolation: klipp_core::keyframe::Interpolation) {
    let keys = crate::timeline::camera_lane::keys_mut(camera, param);
    if index < keys.len() {
        keys[index].interpolation = interpolation;
    }
    if index > 0 && index - 1 < keys.len() {
        keys[index - 1].interpolation = interpolation;
    }
}

/// Set either or both Bezier handles of the key at `index`.
pub(super) fn set_camera_key_tangent(camera: &mut klipp_timeline::Camera, param: klipp_core::CameraParam, index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)>) {
    if let Some(kf) = crate::timeline::camera_lane::keys_mut(camera, param).get_mut(index) {
        if let Some(ti) = tangent_in {
            kf.tangent_in = ti;
        }
        if let Some(to) = tangent_out {
            kf.tangent_out = to;
        }
    }
}

impl KlippApp {
    /// Set or clear one of the sequence camera's keys, as one undo step.
    ///
    /// The whole camera is snapshotted either side, which is what
    /// `SetSequenceShutter` already does, so this needs no history
    /// variant of its own and undo restores the keyframe lists along
    /// with everything else the camera carries. `value` of `None`
    /// removes the key at that moment.
    ///
    /// The dedup is `klipp_core::keyframe`'s, not a second copy: what
    /// counts as "the same moment" is one decision and it lives there.
    pub(super) fn key_sequence_camera(&mut self, sequence_idx: usize, param: klipp_core::CameraParam, time_secs: f64, value: Option<f32>) {
        self.edit_sequence_camera(sequence_idx, |camera| apply_camera_key(camera, param, time_secs, value));
    }

    /// Change the sequence camera and record the change as one camera
    /// snapshot — unless a drag on its keys is open, in which case the
    /// drag's end records the one entry for all of it. Nothing is
    /// recorded when the edit changed nothing.
    pub(super) fn edit_sequence_camera(&mut self, sequence_idx: usize, edit: impl FnOnce(&mut klipp_timeline::Camera)) {
        let Some(seq) = self.state.sequences.get_mut(sequence_idx) else { return };
        let old = seq.camera.clone();
        edit(&mut seq.camera);
        if seq.camera == old {
            return;
        }
        let new = seq.camera.clone();
        let id = seq.id;
        let history = self.state.history_for_mut(id);
        if !history.is_dragging(&camera_drag_key(sequence_idx)) {
            history.push(crate::history::HistoryEntry {
                command: crate::history::EditCommand::SetSequenceCamera { old: Box::new(old), new: Box::new(new) },
            });
        }
        self.sync_sequence_to_engine();
        self.state.dirty = true;
    }
}

#[cfg(test)]
mod sequence_camera_tests {
    use super::*;
    use klipp_core::CameraParam;

    /// A key goes on the list its param names and nowhere else.
    #[test]
    fn a_key_lands_on_one_axis() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::Zoom, 1.0, Some(1.4));
        assert_eq!(cam.zoom_keyframes.len(), 1);
        assert_eq!(cam.zoom_keyframes[0].value, 1.4);
        assert!(
            cam.x_keyframes.is_empty() && cam.y_keyframes.is_empty() && cam.roll_keyframes.is_empty(),
            "keying the zoom should leave the other three alone",
        );
    }

    /// Keying the same moment twice replaces rather than stacks, and
    /// clearing it takes it away — the diamond's two states.
    #[test]
    fn keying_twice_replaces_and_clearing_removes() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::X, 2.0, Some(0.4));
        apply_camera_key(&mut cam, CameraParam::X, 2.0, Some(0.6));
        assert_eq!(cam.x_keyframes.len(), 1, "one moment holds one key");
        assert_eq!(cam.x_keyframes[0].value, 0.6);
        apply_camera_key(&mut cam, CameraParam::X, 2.0, None);
        assert!(cam.x_keyframes.is_empty(), "clearing the key at that moment removes it");
    }

    /// Nothing keyed means the camera is still the camera: no keys, no
    /// lists, nothing written to the project.
    #[test]
    fn a_camera_with_no_keys_stays_empty() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::Y, 1.0, None);
        assert!(cam.y_keyframes.is_empty());
        assert_eq!(cam, klipp_timeline::Camera::default());
    }

    /// A key dragged past its neighbour comes out on the other side of
    /// it, in a list that is still in order; the drag names it by its
    /// moment, so the lane finds it again wherever it sorted to.
    #[test]
    fn a_moved_key_keeps_the_list_sorted() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::X, 1.0, Some(0.2));
        apply_camera_key(&mut cam, CameraParam::X, 2.0, Some(0.8));
        assert!(move_camera_key(&mut cam, CameraParam::X, 0, 3.0, 0.3));
        let times: Vec<f64> = cam.x_keyframes.iter().map(|k| k.time_secs).collect();
        assert_eq!(times, vec![2.0, 3.0]);
        assert_eq!(cam.x_keyframes[1].value, 0.3);
        assert_eq!(crate::timeline::camera_lane::index_at(&cam.x_keyframes, 3.0), Some(1));
        assert!(!move_camera_key(&mut cam, CameraParam::Y, 0, 1.0, 0.5), "no key, no move");
    }

    /// The curve out of a key and into it: the interpolation goes on
    /// the key and on the one before it, as a clip's keyframes do, so
    /// the diamond's two segments take the shape together.
    #[test]
    fn interpolation_shapes_both_segments_at_the_diamond() {
        use klipp_core::keyframe::Interpolation;
        let mut cam = klipp_timeline::Camera::default();
        for t in [1.0, 2.0, 3.0] {
            apply_camera_key(&mut cam, CameraParam::Zoom, t, Some(1.0));
        }
        set_camera_key_interpolation(&mut cam, CameraParam::Zoom, 1, Interpolation::Step);
        let shapes: Vec<_> = cam.zoom_keyframes.iter().map(|k| k.interpolation).collect();
        assert_eq!(shapes, vec![Interpolation::Step, Interpolation::Step, Interpolation::Linear]);
        set_camera_key_interpolation(&mut cam, CameraParam::Zoom, 0, Interpolation::Bezier);
        assert_eq!(cam.zoom_keyframes[0].interpolation, Interpolation::Bezier, "the first key has no key before it");
        set_camera_key_interpolation(&mut cam, CameraParam::Zoom, 9, Interpolation::Linear);
        assert_eq!(cam.zoom_keyframes[2].interpolation, Interpolation::Linear, "an index past the end changes nothing");
    }

    /// Either handle alone, or both.
    #[test]
    fn handles_are_set_one_at_a_time_or_together() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::Roll, 1.0, Some(0.0));
        set_camera_key_tangent(&mut cam, CameraParam::Roll, 0, None, Some((0.4, 2.0)));
        assert_eq!(cam.roll_keyframes[0].tangent_out, (0.4, 2.0));
        assert_eq!(cam.roll_keyframes[0].tangent_in, (0.0, 0.0));
        set_camera_key_tangent(&mut cam, CameraParam::Roll, 0, Some((-0.2, -1.0)), Some((0.1, 0.5)));
        assert_eq!(cam.roll_keyframes[0].tangent_in, (-0.2, -1.0));
        assert_eq!(cam.roll_keyframes[0].tangent_out, (0.1, 0.5));
    }
}
