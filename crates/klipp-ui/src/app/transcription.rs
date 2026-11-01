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

/// How a transcript the app asked for stands (#2942), from the analysis
/// service's status.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TranscribeProgress {
    /// Not started: queued, or other passes first.
    Waiting,
    /// Transcribing, how far when it can say.
    Running(Option<f32>),
    /// The run ended: its transcript is in, or it failed, with why.
    Done,
    Failed(String),
}

/// A transcript of `media_id` asked for (#2942): with no sound, said;
/// with no speech model installed, said with the model named and its
/// download offered there, the clip kept to transcribe once it is in,
/// and nothing started; else followed, and `true` to start it.
pub(crate) fn begin_transcription(state: &mut AppState, media_id: klipp_core::MediaId, model_installed: bool, speech_model: Option<klipp_ml::ModelEntry>) -> bool {
    let Some(item) = state.media_pool.get(&media_id) else { return false };
    let filename = item.filename.clone();
    if !item.has_audio {
        state.toast.show(format!("{filename} has no sound to transcribe"));
        return false;
    }
    if !model_installed {
        match speech_model {
            Some(entry) => {
                let size = crate::model_download::fmt_bytes(crate::model_download::download_size(&entry));
                state.toast.show(format!("Transcription needs the speech model, {} ({size}). Download it to transcribe {filename}.", entry.display_name));
                if !state.transcribe_after_download.contains(&media_id) {
                    state.transcribe_after_download.push(media_id);
                }
                if matches!(state.model_download, crate::model_download::ModelDownloadState::Idle) {
                    state.model_download = crate::model_download::ModelDownloadState::Consenting { entry };
                }
            }
            None => state.toast.show("Transcription needs the speech model, and the model list names none"),
        }
        return false;
    }
    state.toast.show(format!("Transcribing {filename}\u{2026}"));
    if !state.transcribing.contains(&media_id) {
        state.transcribing.push(media_id);
    }
    state.status_transcription = Some(format!("Transcribing {filename}\u{2026}"));
    true
}

/// Each frame (#2942): the transcripts asked for, by `progress`: their
/// progress in the status bar while they run; once one is in, the
/// Transcript panel open on it and said; a failure said with why.
pub(crate) fn follow_transcriptions(state: &mut AppState, progress: &dyn Fn(klipp_core::MediaId) -> TranscribeProgress) {
    let mut lines = Vec::new();
    let mut still = Vec::new();
    for media_id in std::mem::take(&mut state.transcribing) {
        let Some(name) = state.media_pool.get(&media_id).map(|m| m.filename.clone()) else { continue };
        match progress(media_id) {
            TranscribeProgress::Waiting => {
                lines.push(format!("{name}: waiting to transcribe"));
                still.push(media_id);
            }
            TranscribeProgress::Running(at) => {
                lines.push(match at {
                    Some(p) => format!("Transcribing {name}\u{2026} {:.0}%", p.clamp(0.0, 1.0) * 100.0),
                    None => format!("Transcribing {name}\u{2026}"),
                });
                still.push(media_id);
            }
            TranscribeProgress::Done => {
                state.show_transcript_panel = true;
                state.toast.show(format!("The transcript of {name} is ready"));
            }
            TranscribeProgress::Failed(why) => state.toast.show(format!("{name} could not be transcribed: {why}")),
        }
    }
    state.transcribing = still;
    state.status_transcription = (!lines.is_empty()).then(|| lines.join(" \u{00b7} "));
}

impl KlippApp {
    /// A transcript of `media_id` asked for: started, or its model offered
    /// first (`begin_transcription`).
    pub(super) fn transcribe(&mut self, media_id: klipp_core::MediaId) {
        let installed = klipp_analysis::transcript::installed_model().is_some();
        let entry = klipp_ml::load_effective().ok().and_then(|m| m.get(klipp_analysis::transcript::MODEL_ID).cloned());
        if begin_transcription(&mut self.state, media_id, installed, entry) {
            self.apply_brief_to_transcription();
            if let Some(item) = self.state.media_pool.get(&media_id) {
                klipp_analysis::service::request_transcription(item);
            }
        }
    }

    /// Each frame: the transcripts asked for followed (`follow_transcriptions`).
    pub(super) fn follow_transcriptions(&mut self) {
        if self.state.transcribing.is_empty() {
            self.state.status_transcription = None;
            return;
        }
        let pool = self.state.media_pool.clone();
        follow_transcriptions(&mut self.state, &|media_id| {
            use klipp_analysis::service::State as S;
            let Some(status) = klipp_analysis::service::status(media_id) else { return TranscribeProgress::Waiting };
            if let Some((_, why)) = status.failed.iter().find(|(id, _)| id == klipp_analysis::transcript::ID) {
                return TranscribeProgress::Failed(why.clone());
            }
            match status.state {
                S::Queued => TranscribeProgress::Waiting,
                S::Running if status.current == Some(klipp_analysis::transcript::ID) => TranscribeProgress::Running(status.progress),
                S::Running => TranscribeProgress::Waiting,
                S::Failed | S::Cancelled => TranscribeProgress::Failed(status.failed.first().map(|f| f.1.clone()).unwrap_or_else(|| "the analysis stopped".into())),
                S::Done => match pool.get(&media_id) {
                    Some(item) if klipp_analysis::report::MediaAnalysis::load(&item.path).transcript.is_some() => TranscribeProgress::Done,
                    _ => TranscribeProgress::Failed("no speech was found".into()),
                },
            }
        });
    }

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
            AppCommand::SpellingIgnore { word } => {
                let word = word.trim();
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let old = seq.brief.clone();
                let Some(brief) = crate::spelling::ignored(old.as_ref(), word) else { return true };
                seq.brief = Some(brief.clone());
                self.state.push_history(crate::history::EditCommand::SetBrief { old: old.map(Box::new), new: Some(Box::new(brief)) });
                self.state.dirty = true;
                self.apply_brief_to_transcription();
                self.state.toast.show(format!("\u{201c}{word}\u{201d} is taken as right in this project"));
                true
            }
            AppCommand::SpellingLearn { word } => {
                let learnt = klipp_spelling::personal::default_path().ok_or_else(|| "there is no config folder to keep the word list in".to_string()).and_then(|p| klipp_spelling::personal::learn(&p, word));
                match learnt {
                    Ok(()) => {
                        self.state.spelling.personal_changed();
                        self.state.toast.show(format!("\u{201c}{}\u{201d} is added to your dictionary", word.trim()));
                    }
                    Err(e) => self.state.toast.show(e),
                }
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

#[cfg(test)]
mod transcribe_tests {
    use super::{begin_transcription, follow_transcriptions, TranscribeProgress};
    use crate::model_download::ModelDownloadState;
    use crate::state::AppState;

    /// A project with one clip's file, with sound.
    fn with_media() -> (AppState, klipp_core::MediaId) {
        let mut state = AppState::default();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/klipp-transcribe-test.wav".into(), None);
        item.has_audio = true;
        let id = state.media_pool.add(item);
        (state, id)
    }

    fn speech_model() -> klipp_ml::ModelEntry {
        klipp_ml::load_effective().expect("the model list").get(klipp_analysis::transcript::MODEL_ID).cloned().expect("the speech model is listed")
    }

    /// #2942: with no speech model the action says so, naming the model
    /// and its size, offers its download there, and starts nothing; the
    /// clip waits to be transcribed once it is in.
    #[test]
    fn without_the_speech_model_it_offers_the_download_and_starts_nothing() {
        let (mut state, id) = with_media();
        let entry = speech_model();
        assert!(!begin_transcription(&mut state, id, false, Some(entry.clone())), "nothing starts");
        let said = state.toast.message_for_test().unwrap_or_default().to_string();
        assert!(said.contains("Transcription needs the speech model") && said.contains(&entry.display_name) && said.contains(&crate::model_download::fmt_bytes(crate::model_download::download_size(&entry))), "{said}");
        assert!(matches!(&state.model_download, ModelDownloadState::Consenting { entry: offered } if offered.id == entry.id), "its download offered: {:?}", state.model_download);
        assert_eq!(state.transcribe_after_download, [id], "kept to transcribe once it is in");
        assert!(state.transcribing.is_empty() && state.status_transcription.is_none(), "nothing followed as running");
    }

    /// #2942: with the model, it starts and is followed: its progress in
    /// the status bar while it runs, then the Transcript panel opens on it
    /// and says so; a failure says why.
    #[test]
    fn with_the_model_progress_then_the_transcript_panel_opens() {
        let (mut state, id) = with_media();
        assert!(begin_transcription(&mut state, id, true, None), "it starts");
        assert_eq!(state.transcribing, [id]);
        follow_transcriptions(&mut state, &|_| TranscribeProgress::Running(Some(0.4)));
        assert_eq!(state.status_transcription.as_deref(), Some("Transcribing klipp-transcribe-test.wav\u{2026} 40%"));
        assert!(!state.show_transcript_panel);
        follow_transcriptions(&mut state, &|_| TranscribeProgress::Done);
        assert!(state.show_transcript_panel, "the Transcript panel opens");
        assert_eq!(state.toast.message_for_test(), Some("The transcript of klipp-transcribe-test.wav is ready"));
        assert!(state.transcribing.is_empty() && state.status_transcription.is_none());
        // A failure says why, and is no longer followed.
        let (mut state, id) = with_media();
        begin_transcription(&mut state, id, true, None);
        follow_transcriptions(&mut state, &|_| TranscribeProgress::Failed("ffmpeg could not decode the sound".into()));
        assert_eq!(state.toast.message_for_test(), Some("klipp-transcribe-test.wav could not be transcribed: ffmpeg could not decode the sound"));
        assert!(state.transcribing.is_empty());
    }

    /// #2942: each download prompt says what its own model is for: the
    /// speech model's speaks of a transcript, not of object masks, and a
    /// mask model's of masks.
    #[test]
    fn each_download_prompt_names_its_own_model() {
        let manifest = klipp_ml::load_effective().expect("the model list");
        let speech = crate::model_download::consent_text(&speech_model());
        assert!(speech.contains("transcript") && !speech.to_lowercase().contains("mask"), "{speech}");
        let mut kinds = std::collections::HashSet::new();
        for entry in &manifest.models {
            let text = crate::model_download::consent_text(entry);
            assert!(text.contains(crate::model_download::model_purpose(entry.kind)), "{}: {text}", entry.id);
            if entry.kind == klipp_ml::ModelKind::MaskGenerator {
                assert!(text.contains("mask"), "{}: {text}", entry.id);
            }
            kinds.insert(crate::model_download::model_purpose(entry.kind));
        }
        assert!(kinds.len() > 3, "the models' prompts differ: {kinds:?}");
    }
}
