//! Playback heard at the loudness the export will deliver (#1565).
//!
//! An export with a loudness target measures the whole mix and gains it to
//! the target; playback had no such gain, so a quiet mix delivered at
//! -14 LUFS played several decibels under the file it exports to. Here the
//! export's own measurement ([`klipp_render::measure_delivery`]) runs when
//! playback starts on a sequence it hasn't measured, off the engine thread,
//! and its gain is applied in the output callback, after the master
//! volume. Until the measurement lands, playback keeps the gain it had.
//! Playback has no limiter, so the gain is held under the true-peak
//! ceiling: a delivery the export limits plays a little under it.

use super::PlaybackEngine;
use klipp_core::AppEvent;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// What a measurement is of: the sequence's revision and the target
/// and ceiling it was measured for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DeliveryKey {
    revision: klipp_timeline::SequenceRevision,
    target_lufs: f32,
    ceiling_dbtp: f32,
}

/// The delivery state of playback.
#[derive(Default)]
pub(super) struct Delivery {
    /// The target and ceiling an export would use; `None` plays at unity.
    target: Option<(f32, f32)>,
    /// The last measurement and the gain it gives playback.
    measured: Option<(DeliveryKey, DeliveryGain)>,
    /// A measurement under way: its key, its cancel flag, its answer.
    running: Option<(DeliveryKey, Arc<AtomicBool>, std::sync::mpsc::Receiver<Option<DeliveryGain>>)>,
}

/// The gain one measurement gives playback.
#[derive(Debug, Clone, Copy)]
pub(super) struct DeliveryGain {
    /// What playback adds, in dB, after the master volume.
    gain_db: f32,
    /// The export's gain where the ceiling holds playback's under it.
    export_gain_db: f32,
}

impl Drop for Delivery {
    fn drop(&mut self) {
        if let Some((_, cancel, _)) = &self.running {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl PlaybackEngine {
    /// Set the loudness an export would deliver at (`None`: no target,
    /// playback at unity) and, while playing, measure for it.
    pub(super) fn set_delivery_target(&mut self, target: Option<(f32, f32)>) {
        self.delivery.target = target;
        if target.is_none() {
            self.delivery.measured = None;
            self.cancel_delivery_measure();
            self.apply_delivery_gain(None);
        } else if self.state == super::PlaybackState::Playing {
            self.refresh_delivery_gain();
        } else {
            // A cached measurement for this target and revision applies at
            // once; anything else waits for Play.
            let key = self.delivery_key();
            if let (Some(key), Some((at, gain))) = (key, self.delivery.measured) {
                if key == at {
                    self.apply_delivery_gain(Some(gain));
                }
            }
        }
    }

    /// On Play: apply the measurement for the sequence as it is, or start
    /// one; meanwhile playback keeps the gain it has.
    pub(super) fn refresh_delivery_gain(&mut self) {
        let Some(key) = self.delivery_key() else { return };
        if let Some((at, gain)) = self.delivery.measured {
            if at == key {
                self.apply_delivery_gain(Some(gain));
                return;
            }
        }
        if self.delivery.running.as_ref().is_some_and(|(at, _, _)| *at == key) {
            return;
        }
        self.cancel_delivery_measure();
        let (seq, revision) = self.sequence.snapshot_with_revision();
        if revision != key.revision {
            return;
        }
        let pan_law = crate::mix_thread::lock_mixer(&self.mixer).pan_law;
        let settings = klipp_core::ExportSettings { target_lufs: Some(key.target_lufs), true_peak_dbtp: Some(key.ceiling_dbtp), pan_law, ..Default::default() };
        let snapshot = {
            let pool = self.media_pool.read().unwrap_or_else(|e| e.into_inner());
            klipp_render::SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear)
        };
        let snapshot = match snapshot {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("delivery loudness: no snapshot to measure: {e}");
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let flag = cancel.clone();
        let spawned = std::thread::Builder::new().name("klipp-delivery-loudness".into()).spawn(move || {
            let gain = klipp_render::measure_delivery(&snapshot, key.target_lufs, key.ceiling_dbtp, &flag, |_, _| {})
                .ok()
                .map(|a| DeliveryGain { gain_db: a.playback_gain_db(key.ceiling_dbtp), export_gain_db: a.applied_gain_db });
            let _ = tx.send(gain);
        });
        if spawned.is_ok() {
            self.delivery.running = Some((key, cancel, rx));
            let _ = self.event_tx.send(AppEvent::DeliveryLoudness { target_lufs: Some(key.target_lufs), gain_db: None, export_gain_db: None, measuring: true });
        }
    }

    /// Take a finished measurement: apply it if it is still of the
    /// sequence and target playback has.
    pub(super) fn poll_delivery_measure(&mut self) {
        let Some((key, _, rx)) = &self.delivery.running else { return };
        let gain = match rx.try_recv() {
            Ok(gain) => gain,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        let key = *key;
        self.delivery.running = None;
        match gain {
            Some(gain) => {
                self.delivery.measured = Some((key, gain));
                if self.delivery_key() == Some(key) {
                    self.apply_delivery_gain(Some(gain));
                }
            }
            None => {
                let _ = self.event_tx.send(AppEvent::DeliveryLoudness { target_lufs: Some(key.target_lufs), gain_db: None, export_gain_db: None, measuring: false });
            }
        }
    }

    fn cancel_delivery_measure(&mut self) {
        if let Some((_, cancel, _)) = self.delivery.running.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn delivery_key(&self) -> Option<DeliveryKey> {
        let (target_lufs, ceiling_dbtp) = self.delivery.target?;
        Some(DeliveryKey { revision: self.sequence.revision(), target_lufs, ceiling_dbtp })
    }

    fn apply_delivery_gain(&mut self, gain: Option<DeliveryGain>) {
        let db = gain.map_or(0.0, |g| g.gain_db);
        match (self.delivery.target, gain) {
            (Some((target, _)), Some(g)) => tracing::info!("delivery loudness: playback {:+.1} dB to {target:.1} LUFS (export {:+.1} dB)", g.gain_db, g.export_gain_db),
            _ => tracing::info!("delivery loudness: playback at unity"),
        }
        if let Some(aout) = &self.audio_output {
            aout.0.set_delivery_gain(10f32.powf(db / 20.0));
        }
        let _ = self.event_tx.send(AppEvent::DeliveryLoudness {
            target_lufs: self.delivery.target.map(|(t, _)| t),
            gain_db: gain.map(|g| g.gain_db),
            export_gain_db: gain.map(|g| g.export_gain_db),
            measuring: false,
        });
    }
}
