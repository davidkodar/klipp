//! Recording: a take started and stopped (#890).

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

        // Build recording path under the proxy cache dir (or fall back to
        // the OS cache dir). Parent dir is created lazily.
        let cache_root = self
            .prefs
            .proxy
            .proxy_cache_dir
            .clone()
            .or_else(|| dirs::cache_dir().map(|d| d.join("klipp")))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/klipp"));
        let rec_dir = cache_root.join("recordings");
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
            let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                path: path.clone(),
                media_id,
            });
            self.state.toast.show("Recording saved. Drag from media pool onto a track, then use Audio Sync to align if needed.");
            tracing::info!("Recording saved: {}", path.display());
            // Auto-align bonus: deferred. Wiring requires teaching the
            // MediaLoaded event handler to (a) know which incoming imports
            // originated from a recording, (b) auto-append the resulting
            // clip to the armed track, and (c) fire AlignToAudioReference
            // once the clip exists on the timeline. That's tier-2b+ scope
            // (not in the 11-gap MVP). Leaving as manual for now.
        } else {
            self.state.toast.show("Recording stopped");
        }
    }
}
