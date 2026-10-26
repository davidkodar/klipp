//! Translate Captions (#2213): a subtitle track's captions translated
//! as a job on a worker, its progress on the status bar and its cancel
//! in the track's menu, then a new subtitle track under the source as
//! one undo step. The menu and agents (`translate_captions`) start it the
//! same way.

use super::KlippApp;
use klipp_document::caption_translation as ct;
use klipp_ml_candle::translation as tr;

/// A translation running: what it was asked for and where its answer
/// comes.
pub(super) struct Pending {
    job_id: String,
    track_id: klipp_core::TrackId,
    language: String,
    /// The target language's tag, for the new track (#2223).
    code: String,
    sentences: Vec<ct::Sentence>,
    /// What each sentence's placeholders come back as (#2429).
    kept: Vec<Vec<String>>,
    rx: std::sync::mpsc::Receiver<Result<Vec<tr::Line>, String>>,
}

impl KlippApp {
    /// The captions on `track_id`, as whole sentences, translated from
    /// `source` into `target` as job `job_id`; the pair's model fetched
    /// first when it is not downloaded yet.
    pub(super) fn translate_captions(&mut self, track_id: klipp_core::TrackId, source: &str, target: &str, job_id: &str, glossary: &[(String, String)]) {
        let language = tr::language_name(target).to_string();
        // Refused before it starts, the job is still kept, failed with
        // why: an agent was given its id.
        let refuse = |this: &mut Self, why: &str| {
            tr::jobs::refused(job_id, &language, why);
            this.state.toast.show(format!("No translation: {why}"));
        };
        let seq = self.state.sequence();
        let Some(index) = seq.tracks.iter().position(|t| t.id == track_id) else {
            return refuse(self, "the caption track is gone");
        };
        let captions = ct::captions(seq, index);
        if captions.is_empty() {
            return refuse(self, "that track has no captions to translate");
        }
        let sentences = ct::sentences(&captions);
        // The brief's names and terms, and the glossary's, kept through
        // the model as placeholders it copies (#2429).
        let terms = ct::keep_terms(seq.brief.as_ref(), glossary);
        let (lines, kept): (Vec<String>, Vec<Vec<String>>) = sentences.iter().map(|s| ct::protect(&s.text, &terms)).unzip();
        match tr::start(job_id.to_string(), source, target, lines) {
            Ok(rx) => {
                self.state.toast.show(format!("Translating {} captions into {language}…", captions.len()));
                self.translation = Some(Pending { job_id: job_id.to_string(), track_id, language, code: target.to_string(), sentences, kept, rx });
            }
            Err(why) => refuse(self, &why),
        }
    }

    /// Each frame while a translation runs: its line on the status bar,
    /// and once it is back, the new track added and the job closed with
    /// what it made.
    pub(super) fn take_translation(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.translation else { return };
        let answer = match pending.rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.state.status_translation = tr::jobs::get(&pending.job_id).map(|j| status_line(&j));
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the translation stopped without an answer".into()),
        };
        let pending = self.translation.take().expect("checked above");
        self.state.status_translation = None;
        let lines = match answer {
            Ok(lines) => lines,
            Err(why) if why == tr::CANCELLED => {
                self.state.toast.show("Translation cancelled");
                return;
            }
            Err(why) => {
                // The worker ends its own job; one that stopped without an
                // answer (a panic) is ended here, so the next can start.
                tr::jobs::finish(&pending.job_id, Err(why.clone()));
                self.state.toast.show(format!("No translation: {why}"));
                return;
            }
        };
        // Cancelled while its translation was on the way back: not put in
        // place, as the cancel said.
        if tr::jobs::cancel_requested(&pending.job_id) {
            tr::jobs::cancelled(&pending.job_id);
            self.state.toast.show("Translation cancelled");
            return;
        }
        // The kept terms put back; a sentence that lost one is flagged.
        let (texts, cuts): (Vec<String>, Vec<Option<&'static str>>) = lines
            .iter()
            .enumerate()
            .map(|(k, l)| {
                let (text, lost) = ct::restore(&l.text, pending.kept.get(k).map(Vec::as_slice).unwrap_or(&[]));
                (text, l.cut.or(lost.then_some(ct::TERM_LOST)))
            })
            .unzip();
        let translations = ct::per_caption(&pending.sentences, &texts);
        let cut = ct::cut_captions(&pending.sentences, &cuts);
        let seq = self.state.sequence();
        let Some(index) = seq.tracks.iter().position(|t| t.id == pending.track_id) else {
            tr::jobs::finish(&pending.job_id, Err("the caption track went while it was translated".into()));
            self.state.toast.show("The caption track is gone");
            return;
        };
        match ct::translated_track(seq, index, &translations, &cut, &pending.language, &pending.code) {
            Ok(out) => {
                self.apply_edits_as_one(std::slice::from_ref(&out.edit), "Translate Captions");
                let tc = self.state.timecode_display();
                let flagged: Vec<String> = out.flagged.iter().map(|f| format!("{} {}", tc.position_secs(f.at_secs), f.why)).collect();
                let made = format!("{} captions in {} on track {}, hidden so the two languages don't overlap (set_track visible shows it); {} to check{}", translations.len(), pending.language, out.track_index, flagged.len(), if flagged.is_empty() { String::new() } else { format!(": {}", flagged.join("; ")) });
                tr::jobs::finish(&pending.job_id, Ok(made));
                if flagged.is_empty() {
                    self.state.toast.show(format!("Captions translated into {} on a hidden track; its eye shows it", pending.language));
                } else {
                    self.state.toast.show(format!("Captions translated into {} on a hidden track; {} to check: {}", pending.language, flagged.len(), flagged.join("; ")));
                }
            }
            Err(why) => {
                tr::jobs::finish(&pending.job_id, Err(why.clone()));
                self.state.toast.show(format!("No translation: {why}"));
            }
        }
    }
}

/// The status bar's line for job `j`.
fn status_line(j: &tr::jobs::Job) -> String {
    let pct = (j.progress * 100.0).round() as u32;
    match j.phase {
        "downloading" => format!("Downloading the {} translation model {pct}%", j.language),
        "translating" => format!("Translating into {} {}/{}", j.language, j.done, j.total),
        _ => format!("Translating into {}…", j.language),
    }
}
