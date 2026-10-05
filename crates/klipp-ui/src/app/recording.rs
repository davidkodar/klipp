//! Recording: a take started and stopped (#890), punched in to picture
//! and placed when it stops (#114).

use super::*;

impl KlippApp {
    /// Begin capturing audio from the configured input device into a WAV file.
    /// Called from `AppCommand::StartRecording` handler; broken out so the
    /// error/early-return flow stays readable.
    pub(super) fn start_recording(&mut self) {
        if self.state.is_recording {
            self.state.toast.show("Already recording");
            return;
        }

        // Find the armed track (first in the active sequence — MVP is single-track).
        let Some(seq) = self.state.sequences.get(self.state.active_sequence_idx) else {
            return;
        };
        let armed_track = seq.tracks.iter().find(|t| t.record_armed);
        let Some(track) = armed_track else {
            self.state.toast.show("Arm an audio track first (click the red dot on a track header)");
            return;
        };
        if track.kind != klipp_timeline::TrackKind::Audio {
            self.state.toast.show("Recording requires an audio track");
            return;
        }
        let track_id = track.id;
        let punch = crate::punch_in::plan(seq, track_id, self.state.playhead, f64::from(self.prefs.playback.pre_roll_duration));

        // A take is the project's own media: it goes in the project's
        // folder, not a cache that may be cleared (#114). An unsaved
        // project's go under the cache until it is saved.
        let rec_dir = match &self.state.project_path {
            Some(dir) => dir.join("Recordings"),
            None => self
                .prefs
                .proxy
                .proxy_cache_dir
                .clone()
                .or_else(|| dirs::cache_dir().map(|d| d.join("klipp")))
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp/klipp"))
                .join("recordings"),
        };
        if let Err(e) = std::fs::create_dir_all(&rec_dir) {
            self.state.toast.show(format!("Recording dir error: {e}"));
            return;
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = rec_dir.join(format!("rec_{ts}.wav"));

        let device = if self.prefs.audio_hardware.input_device.is_empty() {
            None
        } else {
            Some(self.prefs.audio_hardware.input_device.as_str())
        };
        let sample_rate = self.state.audio_sample_rate;

        // The meter's stream lets go of the device for the take's.
        self.state.input_meter = None;
        let (input, consumer) = match klipp_engine::audio_input::AudioInput::new(device, sample_rate) {
            Ok(pair) => pair,
            Err(e) => {
                self.state.toast.show(format!("Record: {e}"));
                return;
            }
        };
        if let Err(e) = input.start() {
            self.state.toast.show(format!("Record start: {e}"));
            return;
        }
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let _handle = klipp_engine::audio_input::start_recording_thread(
            consumer,
            path.clone(),
            input.sample_rate,
            input.channels,
            stop_rx,
        );

        self.state.recording_audio_input = Some(input);
        self.state.recording_stop_tx = Some(stop_tx);
        self.state.recording_path = Some(path);
        self.state.recording_track_id = Some(track_id);
        self.state.is_recording = true;
        self.state.recording_punch = Some(punch);
        // Roll from the pre-roll, as the capture starts (#114).
        self.state.playhead = punch.roll_from;
        self.send_commands(vec![AppCommand::SetSpeed(1.0), AppCommand::SeekTo(punch.roll_from), AppCommand::Play]);
        self.state.toast.show("Recording\u{2026}");
    }

    /// Stop an active recording, finalize the WAV file, and auto-import it.
    pub(super) fn stop_recording(&mut self) {
        if !self.state.is_recording {
            return;
        }

        // 1. Tell the writer thread to drain + patch + exit.
        if let Some(tx) = self.state.recording_stop_tx.take() {
            let _ = tx.send(());
        }
        // 2. Stop and drop the cpal input stream.
        if let Some(mut input) = self.state.recording_audio_input.take() {
            input.stop();
            drop(input);
        }
        self.state.is_recording = false;
        let punch = self.state.recording_punch.take();
        self.send_commands(vec![AppCommand::Pause]);

        // 3. Hand the WAV off to the import pipeline. The writer thread may
        //    still be finalizing the header — that's OK, the import side
        //    opens the file on demand after the user drags it onto the
        //    timeline or the auto-import hits the media loader.
        //    In practice the writer thread finishes within a few ms.
        let recorded_path = self.state.recording_path.take();
        let _track_id = self.state.recording_track_id.take();
        if let Some(path) = recorded_path {
            // Small settle sleep so the WAV header patch is on disk before
            // the importer opens the file. Worst case the import reads a
            // slightly smaller RIFF size, which every player tolerates — but
            // we want a valid-on-disk file by the time LoadMediaFile runs.
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Route through the media-loading pipeline the same way the
            // file-picker import does (see handle_media_loading): mint a
            // MediaId and ask the engine to decode the header + add it to
            // the pool. `pending_media_path = None` prevents auto-append.
            let media_id = klipp_core::MediaId::new();
            self.state.pending_media_path = None;
            self.state.pending_take = punch.map(|p| (media_id, p));
            let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                path: path.clone(),
                media_id,
            });
            tracing::info!("Recording saved: {}", path.display());
        } else {
            self.state.toast.show("Recording stopped");
        }
    }

    /// A stopped take, loaded: placed where it was punched in (#114), on
    /// the armed track or a take track below it, as one undo step.
    pub(super) fn place_take(&mut self, media_id: klipp_core::MediaId) {
        let Some((id, punch)) = self.state.pending_take else { return };
        if id != media_id {
            return;
        }
        self.state.pending_take = None;
        let Some(secs) = self.state.media_pool.get(&media_id).map(|m| m.duration.to_secs_f64()) else { return };
        let Some((track, clip)) = crate::punch_in::place(self.state.sequence(), &punch, media_id, secs) else {
            self.state.toast.show("The take stopped before the In; it is in the media pool");
            return;
        };
        let edits = match track {
            crate::punch_in::TakeTrack::Armed(track_index) => vec![EditCommand::LegacyAppendClip { clip, track_index }],
            crate::punch_in::TakeTrack::New { index, track } => {
                let name = track.name.clone();
                self.state.toast.show(format!("Take on a new track, {name}"));
                vec![EditCommand::AddTrack { index, track: *track }, EditCommand::LegacyAppendClip { clip, track_index: index }]
            }
        };
        self.apply_edits_as_one(&edits, "Record Take");
    }

    /// Each frame: the input meter, open while a track is armed and
    /// nothing records, and the punch out (#114).
    pub(super) fn tick_recording(&mut self) {
        if self.state.is_recording {
            if let Some(out) = self.state.recording_punch.and_then(|p| p.punch_out) {
                if self.state.playhead >= out {
                    self.stop_recording();
                }
            }
        }
        let armed = self.state.sequence().tracks.iter().any(|t| t.record_armed && t.kind == klipp_timeline::TrackKind::Audio);
        if !armed {
            self.state.input_meter = None;
            self.state.input_meter_failed = false;
        } else if !self.state.is_recording && self.state.input_meter.is_none() && !self.state.input_meter_failed {
            let device = (!self.prefs.audio_hardware.input_device.is_empty()).then_some(self.prefs.audio_hardware.input_device.as_str());
            match klipp_engine::audio_input::AudioInput::new(device, self.state.audio_sample_rate) {
                Ok((input, _samples)) if input.start().is_ok() => self.state.input_meter = Some(input),
                _ => self.state.input_meter_failed = true,
            }
        }
        let live = self.state.recording_audio_input.as_ref().or(self.state.input_meter.as_ref());
        let peak = live.map(|i| i.take_peak()).unwrap_or(0.0);
        // Up at once, down gently, as a peak meter falls back.
        self.state.input_level = peak.max(self.state.input_level * 0.92);
    }
}
