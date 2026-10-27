//! The water under a reflection (#2795), measured from the picture below
//! it on a worker thread and stored on the effect, so every render draws
//! with the same value. A reflection set to take its tint or darkness from
//! the water is measured once, when it has never been; one whose
//! footprint (the footage under it, its range, its waterline) has changed
//! since is outdated, and measured again only when asked (Re-analyze).
//! A measurement that lands for a footprint that has moved on is dropped.

use super::*;

impl KlippApp {
    /// Every reflection on the active sequence set to take the water's
    /// colour or darkness and never measured, measured.
    pub(super) fn measure_unmeasured_water(&mut self) {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let unmeasured: Vec<klipp_core::ClipId> = seq
            .all_clips()
            .filter(|c| {
                matches!(klipp_render::water::reflection_of(c), Some((_, klipp_effects::BuiltinEffect::Reflection { tint_auto, darken_auto, water_key: 0, .. })) if tint_auto || darken_auto)
            })
            .map(|c| c.id)
            .collect();
        for clip_id in unmeasured {
            self.start_water_measure(clip_id);
        }
    }

    /// Measure the water under `clip_id`'s reflection on a thread, unless
    /// a measurement for its footprint now is already running.
    pub(super) fn start_water_measure(&mut self, clip_id: klipp_core::ClipId) {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let Some(key) = klipp_render::water::footprint_key(seq, clip_id) else { return };
        if !self.water_measuring.insert((clip_id, key)) {
            return;
        }
        let snapshot = klipp_render::SequenceSnapshot::from_sequence(
            seq,
            &self.state.media_pool,
            &klipp_core::ExportSettings::default(),
            self.state.global_lut_path.clone().filter(|_| self.state.global_lut_enabled),
            false,
            self.state.project_working_colorspace,
        );
        let Ok(snapshot) = snapshot else {
            self.water_measuring.remove(&(clip_id, key));
            return;
        };
        let tx = self.proxy_event_tx.clone();
        let spawned = std::thread::Builder::new().name("klipp-water-measure".into()).spawn(move || {
            let water = klipp_render::water::measure(&snapshot, clip_id).map(|w| w.srgb);
            let _ = tx.send(klipp_core::AppEvent::WaterMeasured { clip_id, key, water });
        });
        if spawned.is_err() {
            self.water_measuring.remove(&(clip_id, key));
        }
    }

    /// A measurement in (#2795): stored on the reflection, its tint and
    /// darkness following it where they are set to, one undo step; dropped
    /// when the footprint has changed since it started.
    pub(super) fn water_measured(&mut self, clip_id: klipp_core::ClipId, key: u32, water: Option<[u8; 3]>) {
        self.water_measuring.remove(&(clip_id, key));
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        if klipp_render::water::footprint_key(seq, clip_id) != Some(key) {
            tracing::info!("water under {clip_id:?} measured for footage that has since changed; dropped");
            return;
        }
        let Some(water) = water else {
            self.state.toast.show("No water could be measured under the reflection: set its tint by hand");
            return;
        };
        let Some(old) = seq.all_clips().find(|c| c.id == clip_id).cloned() else { return };
        let Some((node, effect)) = klipp_render::water::reflection_of(&old) else { return };
        let Some(index) = old.picture_effects().ok().and_then(|fx| fx.iter().position(|e| e.node == node)) else { return };
        let updated = with_water(effect, water, key);
        let mut new = old.clone();
        if !matches!(new.apply_effect_edits(&[klipp_timeline::EffectEdit::Update(index, &updated)]), Some(Ok(_))) {
            return;
        }
        let edit = EditCommand::ReplaceClip { description: "Measure Water", old: Box::new(old), new: Box::new(new) };
        self.apply_edits_as_one(std::slice::from_ref(&edit), "Measure Water");
    }
}

/// `effect` (a reflection) with the water measured as `water` for
/// footprint `key`: its tint the water's where it takes the tint from the
/// water, its darkness from the water's brightness where it takes that.
pub(super) fn with_water(effect: klipp_effects::BuiltinEffect, water: [u8; 3], key: u32) -> klipp_effects::BuiltinEffect {
    let mut e = effect;
    if let klipp_effects::BuiltinEffect::Reflection { tint, darken, tint_auto, darken_auto, water: w, water_key, .. } = &mut e {
        *w = water;
        *water_key = key;
        if *tint_auto {
            *tint = water;
        }
        if *darken_auto {
            *darken = klipp_effects::reflection::auto_darken(water);
        }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2795: a measurement sets the tint and darkness where they follow
    /// the water, and leaves them where they're the user's.
    #[test]
    fn a_measurement_sets_what_follows_the_water() {
        let calm = klipp_effects::reflection::surface("calm_water", 0.6).unwrap();
        let sunset = [205, 120, 60];
        let got = with_water(calm.clone(), sunset, 77);
        let klipp_effects::BuiltinEffect::Reflection { tint, darken, water, water_key, .. } = got else { unreachable!() };
        assert_eq!((tint, water, water_key), (sunset, sunset, 77));
        assert!((darken - klipp_effects::reflection::auto_darken(sunset)).abs() < 1e-6);
        let mut manual = calm;
        if let klipp_effects::BuiltinEffect::Reflection { tint_auto, darken_auto, .. } = &mut manual {
            (*tint_auto, *darken_auto) = (false, false);
        }
        let klipp_effects::BuiltinEffect::Reflection { tint, darken, water, .. } = with_water(manual, sunset, 77) else { unreachable!() };
        assert_eq!((tint, water), ([25, 85, 100], sunset), "the user's tint kept, the water still recorded");
        assert!((darken - 0.45).abs() < 1e-6, "the user's darkness kept");
        // Bounded: glinting white water or black water can't take it to an end.
        assert!(klipp_effects::reflection::auto_darken([255, 255, 255]) >= 0.15);
        assert!(klipp_effects::reflection::auto_darken([0, 0, 0]) <= 0.7);
    }
}
