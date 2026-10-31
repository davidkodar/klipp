//! The note that suggests a fix while playback keeps dropping frames
//! (#3234): shown while playing, once per project unless another fix fits
//! or things got clearly worse, never acting by itself.

use super::*;

/// How long a note stays up unanswered.
const NOTE_SECS: u64 = 20;

impl KlippApp {
    /// Show the note when trouble starts, and put it away when playback
    /// stops or it has been up long enough. Called every frame; looks at
    /// the diagnosis at most once a second.
    pub(super) fn keep_playback_note(&mut self) {
        let playing = self.state.playback == PlaybackState::Playing;
        if let Some((_, since)) = &self.state.playback_note {
            if !playing || since.elapsed() >= std::time::Duration::from_secs(NOTE_SECS) {
                self.state.playback_note = None;
            }
            return;
        }
        if !playing || !self.prefs.playback.suggest_playback_fixes || self.state.is_scrubbing {
            return;
        }
        if self.playback_note_looked.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1)) {
            return;
        }
        self.playback_note_looked = Some(std::time::Instant::now());
        let seq = self.sequence.snapshot();
        let diagnosis = {
            let ring = self.perf_ring.lock().unwrap_or_else(|e| e.into_inner());
            let samples = (seq.fps.as_f64() * klipp_engine::playback_health::WINDOW as f64).ceil() as usize;
            klipp_engine::playback_health::diagnose_now(&seq, &self.state.media_pool, &ring, samples)
        };
        let Some(d) = diagnosis else { return };
        let project = self.state.project_path.as_ref().map_or_else(|| "untitled".to_string(), |p| p.display().to_string());
        let last = self.prefs.playback.playback_notes_shown.get(&project).map(|n| (n.fix.as_str(), n.dropping));
        if !klipp_engine::playback_health::should_show(last, &d) {
            return;
        }
        let shown = crate::prefs::PlaybackNoteShown { fix: d.fix.as_ref().map_or("", |f| f.kind()).to_string(), dropping: d.health.dropping };
        let notes = &mut self.prefs.playback.playback_notes_shown;
        notes.insert(project, shown);
        while notes.len() > crate::prefs::PLAYBACK_NOTES_KEPT {
            let Some(first) = notes.keys().next().cloned() else { break };
            notes.remove(&first);
        }
        self.prefs_dirty = true;
        self.state.playback_note = Some((d, std::time::Instant::now()));
    }
}
