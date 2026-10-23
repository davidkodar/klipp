//! Transcripts from the app (#531): a word corrected by hand, a clip
//! transcribed again with another model or language, and the brief's
//! spellings and language handed to the transcriber before it runs.

use super::*;

/// The brief's spellings and language to the transcriber: what it is told
/// before it runs, from the app as from the socket. Without a brief the
/// language is detected.
pub(crate) fn brief_to_transcription(brief: Option<&klipp_timeline::Brief>) {
    klipp_analysis::transcript::set_vocabulary(brief.map(|b| b.spellings()).unwrap_or_default());
    klipp_analysis::transcript::set_language(brief.and_then(|b| b.language.clone()));
}

/// The spellings and language last handed to the transcriber.
static GIVEN: std::sync::Mutex<Option<(Vec<String>, Option<String>)>> = std::sync::Mutex::new(None);

impl KlippApp {
    /// [`brief_to_transcription`] with the active sequence's brief.
    pub(super) fn apply_brief_to_transcription(&self) {
        brief_to_transcription(self.state.sequences.get(self.state.active_sequence_idx).and_then(|s| s.brief.as_ref()));
    }

    /// Each frame: the active brief's spellings and language to the
    /// transcriber when they changed, however they changed — set over
    /// the socket or here, undone, another project opened (#2489). A
    /// transcript made, or read, after the brief was set has them; set
    /// only when analysis was asked for, one made later had none.
    pub(super) fn keep_brief_with_transcription(&self) {
        let brief = self.state.sequences.get(self.state.active_sequence_idx).and_then(|s| s.brief.as_ref());
        let now = brief.map(|b| (b.spellings(), b.language.clone())).unwrap_or_default();
        let mut given = GIVEN.lock().unwrap_or_else(|e| e.into_inner());
        if given.as_ref() != Some(&now) {
            brief_to_transcription(brief);
            *given = Some(now);
        }
    }

    /// The transcript commands: `false` for any other.
    pub(super) fn handle_transcription_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::CorrectTranscriptWord { media_id, source_secs, text } => {
                let Some(path) = self.state.media_pool.get(media_id).map(|m| m.path.clone()) else { return true };
                match klipp_analysis::transcript::correct_word(&path, *source_secs, text) {
                    Ok(()) => {
                        // Read again everywhere: the panel, captions, search.
                        self.state.analysis_generation += 1;
                        self.state.toast.show(format!("Corrected to \u{201C}{}\u{201D}", text.trim()));
                    }
                    Err(e) => self.state.toast.show(format!("Transcript unchanged: {e}")),
                }
                true
            }
            AppCommand::RetranscribeClip { media_id, model, language } => {
                let Some(item) = self.state.media_pool.get(media_id).cloned() else { return true };
                self.apply_brief_to_transcription();
                klipp_analysis::transcript::redo(&item.path, klipp_analysis::transcript::Redo { model: model.clone(), language: language.clone() });
                klipp_analysis::service::request_transcription(&item);
                let with = model.as_deref().map(klipp_analysis::transcript::model_label).unwrap_or("the best model");
                self.state.toast.show(format!("Transcribing {} again with {with}\u{2026}", item.filename));
                true
            }
            AppCommand::SetTranscriptLanguage { language } => {
                let language = language.as_deref().map(str::trim).filter(|l| !l.is_empty()).map(str::to_lowercase);
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let old = seq.brief.clone();
                let mut brief = old.clone().unwrap_or_default();
                if brief.language == language {
                    return true;
                }
                brief.language = language;
                seq.brief = Some(brief.clone());
                self.state.push_history(crate::history::EditCommand::SetBrief { old: old.map(Box::new), new: Some(Box::new(brief)) });
                self.state.dirty = true;
                self.apply_brief_to_transcription();
                // Transcripts whisper detected in another language are stale
                // now; asking for analysis makes those again, and only those.
                for item in self.state.media_pool.all().filter(|m| m.has_audio && m.kind == klipp_media::MediaItemKind::File && !m.offline) {
                    klipp_analysis::service::request(item);
                }
                true
            }
            _ => false,
        }
    }
}
