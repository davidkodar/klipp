//! A sequence's version at another aspect (#2211): its files' framing
//! analysis asked for, and the version reframed once that is done.

use super::*;

/// The aspects a version can be made at, as width and height parts.
pub(crate) const ASPECTS: [(&str, u32, u32); 4] = [("9:16", 9, 16), ("1:1", 1, 1), ("4:5", 4, 5), ("16:9", 16, 9)];

/// A frame of `aspect` keeping `frame`'s short side as its own, both
/// sides even: 1920x1080 is 1080x1920 at 9:16, 1080x1080 at 1:1 and
/// 1080x1350 at 4:5.
pub(crate) fn aspect_size(frame: klipp_core::Resolution, aspect: (u32, u32)) -> (u32, u32) {
    let short = frame.width.min(frame.height).max(2) as f64;
    let (a, b) = (aspect.0.max(1) as f64, aspect.1.max(1) as f64);
    let even = |v: f64| ((v / 2.0).round() as u32).max(1) * 2;
    if a <= b { (even(short), even(short * b / a)) } else { (even(short * a / b), even(short)) }
}

/// Whether `frame` already has `aspect`, to a pixel.
pub(crate) fn has_aspect(frame: klipp_core::Resolution, aspect: (u32, u32)) -> bool {
    (frame.width as u64 * aspect.1 as u64).abs_diff(frame.height as u64 * aspect.0 as u64) <= aspect.0.max(aspect.1) as u64
}

/// A copy of sequence `source` at `size`, named `name`, added after the
/// others: its index. The pictures are reframed later, as one undo step.
pub(super) fn add_version(state: &mut crate::state::AppState, source: usize, size: (u32, u32), name: &str) -> Option<usize> {
    let master = state.sequences.get(source)?;
    let mut version = master.clone();
    // Whose version, at which cut (#2724): one made of a version is the
    // master's.
    version.version_of = Some(master.version_of.unwrap_or(klipp_timeline::VersionOf { master: master.id, cut: master.cut_stamp() }));
    version.id = klipp_core::SequenceId::new();
    version.name = name.to_string();
    let from = version.resolution;
    version.resolution = klipp_core::Resolution { width: size.0, height: size.1 };
    // Each reframe on its picture in the new frame, for the refit to keep.
    klipp_document::ops::keep_reframes(&mut version, &state.media_pool, from);
    version.awaiting_refit = true;
    state.sequences.push(version);
    state.dirty = true;
    Some(state.sequences.len() - 1)
}

/// Which of `pending` to reframe now, given the sequences there are, the
/// one open and whether a file is still being analysed: those whose files
/// are done, once open. The rest are kept, less the files that are done,
/// and those whose sequence is gone are dropped.
pub(super) fn due_refits(pending: &mut Vec<crate::state::PendingRefit>, exists: impl Fn(klipp_core::SequenceId) -> bool, open: Option<klipp_core::SequenceId>, busy: impl Fn(klipp_core::MediaId) -> bool) -> Vec<klipp_core::SequenceId> {
    let mut due = Vec::new();
    pending.retain_mut(|p| {
        if !exists(p.sequence) {
            return false;
        }
        p.waiting.retain(|id| busy(*id));
        if p.waiting.is_empty() && open == Some(p.sequence) {
            due.push(p.sequence);
            return false;
        }
        true
    });
    due
}

/// The waits of an opened project's aspect versions still to be reframed
/// (#2231): their files' framing asked for again.
pub(super) fn waiting_refits(sequences: &[klipp_timeline::Sequence], pool: &klipp_media::MediaPool) -> Vec<crate::state::PendingRefit> {
    sequences.iter().filter(|s| s.awaiting_refit).map(|s| crate::state::PendingRefit { sequence: s.id, waiting: request_framing(s, pool), keep: Vec::new() }).collect()
}

/// Ask for the framing analysis of every file `seq` shows a picture of,
/// with subject detection on: the files still being analysed.
pub(super) fn request_framing(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Vec<klipp_core::MediaId> {
    if !klipp_analysis::subject_detection_enabled() {
        return Vec::new();
    }
    framing_of_pictures(seq, pool, klipp_analysis::service::request)
}

/// Each file `seq` shows a picture of asked for with `ask`: the files
/// still being analysed.
fn framing_of_pictures(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, ask: fn(&klipp_media::MediaItem) -> klipp_analysis::service::Status) -> Vec<klipp_core::MediaId> {
    let mut waiting = Vec::new();
    let pictures = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video && !t.locked).flat_map(|t| &t.clips);
    for id in pictures.filter_map(|c| c.media_id()) {
        let Some(item) = pool.get(&id).filter(|m| m.resolution.width > 0 && !waiting.contains(&id)) else { continue };
        let status = ask(item);
        if matches!(status.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running) {
            waiting.push(id);
        }
    }
    waiting
}

/// The toast button a refit's report earns: "Find people now" when it
/// left a picture centred that was never looked through for people.
/// `ops` the request, `what` its steps' reports.
pub(super) fn find_people_offer(ops: &[klipp_document::ops::Op], what: &[&str], sequence: klipp_core::SequenceId) -> Option<crate::widgets::toast::ToastAction> {
    let refit = ops.iter().any(|o| matches!(o, klipp_document::ops::Op::Refit { .. }));
    (refit && what.iter().any(|w| w.contains(klipp_document::ops::NOT_ANALYSED_FOR_PEOPLE))).then_some(crate::widgets::toast::ToastAction::FindPeople(sequence))
}

/// What a "Find people now" waiting on the detection model does next.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum FindPeopleNext {
    /// The model is there: look through the files.
    Run,
    /// Its consent dialog or download is still going.
    Wait,
    /// Declined or failed: nothing to look with.
    Drop,
}

/// Given whether the model is there and where its download stands.
pub(super) fn find_people_next(model_ready: bool, download: &crate::model_download::ModelDownloadState) -> FindPeopleNext {
    use crate::model_download::ModelDownloadState as D;
    match download {
        _ if model_ready => FindPeopleNext::Run,
        D::Consenting { .. } | D::Downloading { .. } | D::Completed { .. } => FindPeopleNext::Wait,
        D::Idle | D::Failed { .. } => FindPeopleNext::Drop,
    }
}

/// Whether the detection model is downloaded.
fn people_model_ready() -> bool {
    klipp_analysis::framing::model_path().is_some()
}

impl KlippApp {
    /// "Find people now" from a refit's toast: the detection model asked
    /// for (its consent dialog) when it is not downloaded, then the
    /// sequence's files looked through for people, subject detection off
    /// or not, and the sequence refit when that is done, as one step.
    pub(super) fn find_people_now(&mut self, sequence: klipp_core::SequenceId) {
        self.state.find_people = Some(sequence);
        if !people_model_ready() {
            self.request_model_download(klipp_analysis::framing::MODEL_ID);
        }
        self.advance_find_people();
    }

    /// A "Find people now" taken on once its model is there.
    pub(super) fn advance_find_people(&mut self) {
        let Some(id) = self.state.find_people else { return };
        match find_people_next(people_model_ready(), &self.state.model_download) {
            FindPeopleNext::Wait => {}
            FindPeopleNext::Drop => {
                self.state.find_people = None;
                self.state.toast.show("People not looked for: the detection model is not downloaded");
            }
            FindPeopleNext::Run => {
                self.state.find_people = None;
                let Some(seq) = self.state.sequences.iter_mut().find(|s| s.id == id) else { return };
                let waiting = framing_of_pictures(seq, &self.state.media_pool, klipp_analysis::service::request_framing);
                seq.awaiting_refit = true;
                let name = seq.name.clone();
                self.state.pending_refits.retain(|p| p.sequence != id);
                self.state.toast.show(format!("{name}: finding the people in {} file{}; it is refit when that is done", waiting.len(), if waiting.len() == 1 { "" } else { "s" }));
                self.state.pending_refits.push(crate::state::PendingRefit { sequence: id, waiting, keep: Vec::new() });
            }
        }
    }

    /// Reframe each aspect version whose files are analysed, once it is
    /// the sequence open; one whose sequence was deleted is let go.
    pub(super) fn advance_refits(&mut self) {
        if self.state.pending_refits.is_empty() {
            return;
        }
        let open = self.state.sequences.get(self.state.active_sequence_idx).map(|s| s.id);
        let ids: Vec<_> = self.state.sequences.iter().map(|s| s.id).collect();
        let busy = |id| klipp_analysis::service::status(id).is_some_and(|s| matches!(s.state, klipp_analysis::service::State::Queued | klipp_analysis::service::State::Running));
        // The open sequence is the one an op resolves on, so one due at
        // most.
        let keeps: std::collections::HashMap<_, _> = self.state.pending_refits.iter().map(|p| (p.sequence, p.keep.clone())).collect();
        for id in due_refits(&mut self.state.pending_refits, |id| ids.contains(&id), open, busy) {
            if let Some(seq) = self.state.sequences.iter_mut().find(|s| s.id == id) {
                seq.awaiting_refit = false;
            }
            self.state.pending_doc_ops.push(crate::state::DocOpsRequest {
                label: "Refit to Frame".into(),
                ops: vec![klipp_document::ops::Op::Refit { markers: true, punch: None, keep: keeps.get(&id).cloned().unwrap_or_default() }],
                independent: false,
            });
        }
    }
}

/// Version `index` made again from its master's cut (#2724): the
/// master's tracks, each media clip still there keeping the picture it
/// has in the version (its framing there, and its look); those clips,
/// and the master's cut. `None` when it is no version or its master is
/// gone.
pub(crate) fn rebuilt_tracks(sequences: &[klipp_timeline::Sequence], index: usize) -> Option<(Vec<klipp_timeline::Track>, Vec<klipp_core::ClipId>, u64)> {
    let version = sequences.get(index)?;
    let of = version.version_of?.master;
    let master = sequences.iter().find(|s| s.id == of)?;
    let old: std::collections::HashMap<klipp_core::ClipId, &klipp_timeline::Clip> = version.all_clips().map(|c| (c.id, c)).collect();
    let mut tracks = master.tracks.clone();
    let mut kept = Vec::new();
    for clip in tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
        let Some(was) = old.get(&clip.id).filter(|_| matches!(clip.kind, klipp_timeline::ClipKind::Media { .. })) else { continue };
        clip.restore_picture(&was.picture);
        kept.push(clip.id);
    }
    Some((tracks, kept, master.cut_stamp()))
}

/// The ticked delivery formats as outputs (#2724): each written beside
/// `output_path`, its name with the format's suffix, with its preset.
pub(crate) fn delivery_outputs(ticked: &[bool], output_path: &str) -> Vec<klipp_core::DeliveryOutput> {
    let path = std::path::Path::new(output_path.trim());
    let dir = path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let stem = path.file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("delivery");
    let presets = klipp_core::builtin_presets();
    klipp_core::delivery_templates().iter().zip(ticked).filter(|(_, on)| **on).filter_map(|(t, _)| {
        let p = presets.iter().find(|p| p.id == t.preset)?;
        let file = dir.join(format!("{stem}_{}.{}", t.suffix, p.settings.format.extension()));
        Some(klipp_core::DeliveryOutput { aspect: t.aspect, settings: p.settings.settings_for(file) })
    }).collect()
}

/// The open sequence's master's versions at `aspect` older than its cut
/// (#2724), by index.
pub(crate) fn stale_version_at(sequences: &[klipp_timeline::Sequence], open: usize, aspect: (u32, u32)) -> Option<usize> {
    let master = sequences.get(open)?.version_of.map_or(sequences[open].id, |v| v.master);
    sequences.iter().position(|s| s.version_of.is_some_and(|v| v.master == master) && has_aspect(s.resolution, aspect) && s.version_is_stale(sequences) == Some(true))
}

/// The sequence each delivery output is exported from (#2724): the
/// master at its own aspect, else its version at the output's (one made
/// of a version is the master's), made when there is none. Each made one
/// is reframed when its files' framing is in. A version older than the
/// master's cut is said, never rebuilt unasked. Its outputs, with the
/// versions made and the stale ones by name.
pub(crate) fn delivery_targets(state: &mut crate::state::AppState, outputs: &[klipp_core::DeliveryOutput]) -> (Vec<crate::state::PendingDelivery>, Vec<usize>, Vec<String>) {
    let open = state.active_sequence_idx;
    let master_idx = state.sequences[open].version_of.and_then(|v| state.sequences.iter().position(|s| s.id == v.master)).unwrap_or(open);
    let master_id = state.sequences[master_idx].id;
    let (mut pending, mut made, mut stale) = (Vec::new(), Vec::new(), Vec::new());
    for o in outputs {
        let master = &state.sequences[master_idx];
        let target = if has_aspect(master.resolution, o.aspect) {
            master_id
        } else if let Some(v) = state.sequences.iter().find(|s| s.version_of.is_some_and(|v| v.master == master_id) && has_aspect(s.resolution, o.aspect)) {
            if v.version_is_stale(&state.sequences) == Some(true) {
                stale.push(v.name.clone());
            }
            v.id
        } else {
            let size = aspect_size(master.resolution, o.aspect);
            let name = format!("{} {}:{}", master.name, o.aspect.0, o.aspect.1);
            let Some(index) = add_version(state, master_idx, size, &name) else { continue };
            made.push(index);
            state.sequences[index].id
        };
        pending.push(crate::state::PendingDelivery { sequence: target, settings: o.settings.clone() });
    }
    (pending, made, stale)
}

/// The next delivery export to queue, given the sequences there are, the
/// open one, whether an export's confirmation is open and whether a
/// sequence awaits its reframe: open it first, wait for its reframe, then
/// export it. `Go::Done` when none is left.
#[derive(Debug, PartialEq)]
pub(crate) enum Go {
    Done,
    Open(usize),
    Wait,
    Export(usize),
    Gone,
}

pub(crate) fn next_delivery(pending: &[crate::state::PendingDelivery], sequences: &[klipp_timeline::Sequence], open: usize, confirming: bool) -> Go {
    let Some(first) = pending.first() else { return Go::Done };
    if confirming {
        return Go::Wait;
    }
    match sequences.iter().position(|s| s.id == first.sequence) {
        None => Go::Gone,
        Some(i) if i != open => Go::Open(i),
        Some(i) if sequences[i].awaiting_refit => Go::Wait,
        Some(i) => Go::Export(i),
    }
}

impl super::KlippApp {
    /// The Export page's delivery section (#2724): a box for each format,
    /// a stale version named with its rebuild, and Export All. Whether
    /// that was pressed.
    pub(super) fn deliver_section(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>) -> bool {
        let mut go = false;
        egui::CollapsingHeader::new("Deliver to\u{2026}").id_salt("export_deliver").default_open(false).show(ui, |ui| {
            ui.label(egui::RichText::new("Every format at once, each from this sequence's version in its shape, reframed to follow the subjects. Files go beside the output file, its name with the format's suffix.").small().weak());
            let open = self.state.active_sequence_idx;
            for (i, t) in klipp_core::delivery_templates().iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.export_dialog.deliver[i], t.name.as_str());
                    if let Some(v) = stale_version_at(&self.state.sequences, open, t.aspect) {
                        ui.label(egui::RichText::new("\u{26A0} older than the master's last edit").small().color(ui.visuals().warn_fg_color));
                        if ui.small_button("Rebuild from Master").on_hover_text("Copy the master's cut in again; clips still there keep their framing").clicked() {
                            cmds.push(AppCommand::RebuildVersion { sequence_idx: v });
                        }
                    }
                });
            }
            let outputs = delivery_outputs(&self.export_dialog.deliver, &self.export_dialog.output_path);
            let ready = !outputs.is_empty() && !self.export_dialog.output_path.trim().is_empty() && !self.state.sequence().has_nothing_to_export();
            if ui.add_enabled(ready, egui::Button::new(format!("Export All ({})", outputs.len()))).on_disabled_hover_text("Tick a format, and choose an output file to name them by").clicked() {
                cmds.push(AppCommand::ExportDelivery { outputs });
                go = true;
            }
        });
        go
    }

    /// Version `index` made again from its master's cut, as one undo step
    /// on it, opened first; the clips that are new to it reframed once
    /// their framing is in (#2724).
    pub(super) fn rebuild_version(&mut self, index: usize) {
        let Some((tracks, kept, cut)) = rebuilt_tracks(&self.state.sequences, index) else {
            self.state.toast.show("Not a version of a sequence there is: nothing to rebuild from");
            return;
        };
        if index != self.state.active_sequence_idx {
            self.send_commands(vec![AppCommand::SetActiveSequence { sequence_idx: index }]);
        }
        let old = self.state.sequences[index].tracks.clone();
        self.apply_edits_as_one(&[klipp_document::history::EditCommand::SetTracks { description: "Rebuild from Master", old, new: tracks }], "Rebuild from Master");
        let seq = &mut self.state.sequences[index];
        if let Some(v) = seq.version_of.as_mut() {
            v.cut = cut;
        }
        seq.awaiting_refit = true;
        let (id, name) = (seq.id, seq.name.clone());
        let waiting = request_framing(&self.state.sequences[index], &self.state.media_pool);
        self.state.pending_refits.push(crate::state::PendingRefit { sequence: id, waiting, keep: kept.clone() });
        self.state.toast.show(format!("{name} rebuilt from the master's cut: {} clip(s) kept their framing, the new ones are reframed", kept.len()));
    }

    /// A delivery's outputs found or made, and queued to export each in
    /// turn as its version is reframed (#2724).
    pub(super) fn start_delivery(&mut self, outputs: &[klipp_core::DeliveryOutput]) {
        let home = self.state.sequences[self.state.active_sequence_idx].id;
        let (pending, made, stale) = delivery_targets(&mut self.state, outputs);
        for index in made {
            let id = self.state.sequences[index].id;
            let waiting = request_framing(&self.state.sequences[index], &self.state.media_pool);
            self.state.pending_refits.push(crate::state::PendingRefit { sequence: id, waiting, keep: Vec::new() });
        }
        if !stale.is_empty() {
            self.state.toast.show(format!("{} older than the master's last edit: rebuild from the master to deliver the current cut", stale.join(", ")));
        }
        self.state.pending_deliveries.extend(pending);
        self.state.delivery_home.get_or_insert(home);
    }

    /// One step of a delivery each frame: open the next export's
    /// sequence, wait for its reframe, export it; back to where it began
    /// when all are queued.
    pub(super) fn advance_deliveries(&mut self) {
        let confirming = self.state.export_offline_warning.is_some() || self.state.export_source_overwrite_warning.is_some();
        match next_delivery(&self.state.pending_deliveries, &self.state.sequences, self.state.active_sequence_idx, confirming) {
            Go::Done => {
                if let Some(home) = self.state.delivery_home.take() {
                    if let Some(i) = self.state.sequences.iter().position(|s| s.id == home) {
                        self.send_commands(vec![AppCommand::SetActiveSequence { sequence_idx: i }]);
                    }
                }
            }
            Go::Wait => {}
            Go::Gone => {
                self.state.pending_deliveries.remove(0);
                self.state.toast.show("A delivery's sequence was deleted: that format was left out");
            }
            Go::Open(i) => self.send_commands(vec![AppCommand::SetActiveSequence { sequence_idx: i }]),
            Go::Export(_) => {
                let next = self.state.pending_deliveries.remove(0);
                self.export_or_ask(&next.settings, super::export::Confirmed::Nothing);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_keeps_the_short_side() {
        let hd = klipp_core::Resolution { width: 1920, height: 1080 };
        assert_eq!(aspect_size(hd, (9, 16)), (1080, 1920));
        assert_eq!(aspect_size(hd, (1, 1)), (1080, 1080));
        assert_eq!(aspect_size(hd, (4, 5)), (1080, 1350));
        let tall = klipp_core::Resolution { width: 1080, height: 1920 };
        assert_eq!(aspect_size(tall, (16, 9)), (1920, 1080));
        assert!(has_aspect(hd, (16, 9)) && !has_aspect(hd, (9, 16)));
        assert!(has_aspect(klipp_core::Resolution { width: 3840, height: 2160 }, (16, 9)));
    }

    /// A version is the sequence copied at its new size, added last; the
    /// one it was made from is left as it was.
    #[test]
    fn a_version_is_a_copy_at_the_new_size() {
        let mut state = crate::state::AppState::default();
        let source = state.sequences[0].clone();
        let index = add_version(&mut state, 0, (1080, 1920), "Cut 9:16").unwrap();
        assert_eq!(index, 1);
        let version = &state.sequences[1];
        assert_eq!((version.name.as_str(), version.resolution.width, version.resolution.height), ("Cut 9:16", 1080, 1920));
        assert_ne!(version.id, source.id);
        assert_eq!(version.tracks.len(), source.tracks.len());
        assert_eq!(state.sequences[0].resolution, source.resolution);
        assert!(add_version(&mut state, 9, (1, 1), "x").is_none());
        assert!(state.sequences[1].awaiting_refit && !state.sequences[0].awaiting_refit, "the version waits to be reframed");
    }

    /// An opened project's versions still to be reframed wait again
    /// (#2231); the rest do not.
    #[test]
    fn an_opened_version_still_to_be_reframed_waits_again() {
        let mut state = crate::state::AppState::default();
        add_version(&mut state, 0, (1080, 1920), "Cut 9:16").unwrap();
        let pending = waiting_refits(&state.sequences, &state.media_pool);
        assert_eq!(pending.iter().map(|p| p.sequence).collect::<Vec<_>>(), vec![state.sequences[1].id]);
        state.sequences[1].awaiting_refit = false;
        assert!(waiting_refits(&state.sequences, &state.media_pool).is_empty());
    }

    /// A version is reframed once its files are analysed and it is the
    /// sequence open, however the tabs were switched meanwhile; one whose
    /// sequence was deleted is let go.
    #[test]
    fn a_version_is_reframed_once_analysed_and_open() {
        use crate::state::PendingRefit;
        let (a, b) = (klipp_core::SequenceId::new(), klipp_core::SequenceId::new());
        let file = klipp_core::MediaId::new();
        let mut pending = vec![PendingRefit { sequence: a, waiting: vec![file], keep: Vec::new() }];
        let all = |_| true;
        assert!(due_refits(&mut pending, all, Some(a), |_| true).is_empty(), "still analysing");
        assert!(due_refits(&mut pending, all, Some(b), |_| false).is_empty(), "analysed, but another tab is open");
        assert!(pending[0].waiting.is_empty(), "the file done is crossed off");
        assert_eq!(due_refits(&mut pending, all, Some(a), |_| true), vec![a], "open again: reframed");
        assert!(pending.is_empty());
        let mut gone = vec![PendingRefit { sequence: b, waiting: vec![], keep: Vec::new() }];
        assert!(due_refits(&mut gone, |_| false, Some(b), |_| false).is_empty());
        assert!(gone.is_empty(), "its sequence was deleted");
    }

    fn output(aspect: (u32, u32), path: &str) -> klipp_core::DeliveryOutput {
        klipp_core::DeliveryOutput { aspect, settings: klipp_core::ExportSettings { output_path: path.into(), ..Default::default() } }
    }

    /// #2724: a delivery exports 16:9 from the master itself, 9:16 from
    /// the version there is, and 1:1 from one made now; a version made
    /// before the master's cut changed is named, and kept as it is.
    #[test]
    fn a_refit_that_left_pictures_unanalysed_offers_to_find_people() {
        use crate::widgets::toast::ToastAction;
        let id = klipp_core::SequenceId::new();
        let refit = [klipp_document::ops::Op::Refit { markers: true, punch: None, keep: Vec::new() }];
        let said = format!("Refit: talk.mov: {}, so it stays centred", klipp_document::ops::NOT_ANALYSED_FOR_PEOPLE);
        assert_eq!(find_people_offer(&refit, &[&said], id), Some(ToastAction::FindPeople(id)));
        // No one found in an analysed picture is no reason to look again.
        assert_eq!(find_people_offer(&refit, &["Refit: talk.mov: no one found in it"], id), None);
        // Nor is another op's report, whatever it says.
        let other: [klipp_document::ops::Op; 0] = [];
        assert_eq!(find_people_offer(&other, &[&said], id), None);
    }

    #[test]
    fn find_people_waits_for_the_model_and_lets_go_when_declined() {
        use crate::model_download::ModelDownloadState as D;
        assert_eq!(find_people_next(true, &D::Idle), FindPeopleNext::Run);
        assert_eq!(find_people_next(false, &D::Failed { entry_label: "RF-DETR".into(), error: "offline".into() }), FindPeopleNext::Drop);
        // The consent dialog closed without a download is a no.
        assert_eq!(find_people_next(false, &D::Idle), FindPeopleNext::Drop);
        assert_eq!(find_people_next(false, &D::Completed { entry_label: "RF-DETR".into(), path: "/m".into() }), FindPeopleNext::Wait);
    }

    #[test]
    fn a_delivery_uses_the_master_its_versions_or_new_ones() {
        let mut state = crate::state::AppState::default();
        let media = klipp_core::MediaId::new();
        let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 30);
        state.sequences[0].tracks[0].clips.push(klipp_timeline::Clip::media(media, at(0.0), at(4.0), at(0.0), at(4.0)));
        let tall = add_version(&mut state, 0, (1080, 1920), "Cut 9:16").unwrap();
        state.sequences[tall].awaiting_refit = false;
        let tall_id = state.sequences[tall].id;
        let (pending, made, stale) = delivery_targets(&mut state, &[output((16, 9), "/tmp/a.mp4"), output((9, 16), "/tmp/b.mp4"), output((1, 1), "/tmp/c.mp4")]);
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].sequence, state.sequences[0].id, "16:9 from the master");
        assert_eq!(pending[1].sequence, tall_id, "9:16 from its version");
        assert_eq!(made.len(), 1);
        let square = &state.sequences[made[0]];
        assert_eq!((square.resolution.width, square.resolution.height, pending[2].sequence), (1080, 1080, square.id), "1:1 made");
        assert!(square.awaiting_refit && square.version_of.is_some_and(|v| v.master == state.sequences[0].id));
        assert!(stale.is_empty(), "nothing cut since");
        // The master trimmed: its versions are older than its cut.
        state.sequences[0].tracks[0].clips[0].timeline_out = at(3.0);
        let (_, made, stale) = delivery_targets(&mut state, &[output((9, 16), "/tmp/b.mp4")]);
        assert!(made.is_empty() && stale == vec!["Cut 9:16".to_string()], "{stale:?}");
        assert_eq!(state.sequences[tall].tracks[0].clips[0].timeline_out, at(4.0), "never rebuilt unasked");
    }

    /// One step a frame: the next export's sequence opened, its reframe
    /// waited for, then exported; none waits while a confirmation is open.
    #[test]
    fn a_delivery_opens_waits_and_exports_in_turn() {
        let mut state = crate::state::AppState::default();
        let v = add_version(&mut state, 0, (1080, 1920), "Cut 9:16").unwrap();
        let pending = vec![crate::state::PendingDelivery { sequence: state.sequences[v].id, settings: Default::default() }];
        assert_eq!(next_delivery(&pending, &state.sequences, 0, false), Go::Open(v));
        assert_eq!(next_delivery(&pending, &state.sequences, v, false), Go::Wait, "awaiting its reframe");
        state.sequences[v].awaiting_refit = false;
        assert_eq!(next_delivery(&pending, &state.sequences, v, true), Go::Wait, "a confirmation open");
        assert_eq!(next_delivery(&pending, &state.sequences, v, false), Go::Export(v));
        assert_eq!(next_delivery(&[], &state.sequences, v, false), Go::Done);
        state.sequences.remove(v);
        assert_eq!(next_delivery(&pending, &state.sequences, 0, false), Go::Gone);
    }

    /// #2724: a version rebuilt from its trimmed master takes the new
    /// cut, a clip still there keeping the framing given it in the
    /// version, a new one left for the refit.
    #[test]
    fn a_rebuilt_version_takes_the_cut_and_keeps_its_framing() {
        let mut state = crate::state::AppState::default();
        let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 30);
        let media = klipp_core::MediaId::new();
        state.sequences[0].tracks[0].clips.push(klipp_timeline::Clip::media(media, at(0.0), at(4.0), at(0.0), at(4.0)));
        let v = add_version(&mut state, 0, (1080, 1920), "Cut 9:16").unwrap();
        // Reframed by hand in the version.
        let framed = klipp_timeline::test_clip::TestClip::of(state.sequences[v].tracks[0].clips[0].clone()).lane("scale", 0.0, 2.5).build();
        state.sequences[v].tracks[0].clips[0] = framed;
        // The master trimmed, and a shot added.
        state.sequences[0].tracks[0].clips[0].timeline_out = at(3.0);
        state.sequences[0].tracks[0].clips.push(klipp_timeline::Clip::media(media, at(3.0), at(6.0), at(5.0), at(8.0)));
        assert_eq!(state.sequences[v].version_is_stale(&state.sequences), Some(true));
        let (tracks, kept, cut) = rebuilt_tracks(&state.sequences, v).unwrap();
        let clips = &tracks[0].clips;
        assert_eq!((clips.len(), clips[0].timeline_out), (2, at(3.0)), "the new cut");
        assert_eq!(kept, vec![clips[0].id], "the clip still there kept");
        let scale = |c: &klipp_timeline::Clip| klipp_timeline::frame_glide::lane(c, "scale").iter().map(|k| k.value).collect::<Vec<_>>();
        assert_eq!(scale(&clips[0]), vec![2.5], "with its framing");
        assert!(scale(&clips[1]).is_empty(), "the new one left for the refit");
        assert_eq!(cut, state.sequences[0].cut_stamp());
        assert!(rebuilt_tracks(&state.sequences, 0).is_none(), "the master is no version");
    }

    /// #2724: the ticked formats are written beside the output file,
    /// each named with its suffix and with its preset's size.
    #[test]
    fn ticked_formats_are_named_beside_the_output() {
        let outputs = delivery_outputs(&[true, true, false, true], "/out/film.mov");
        let named: Vec<_> = outputs.iter().map(|o| (o.aspect, o.settings.output_path.to_string_lossy().to_string(), o.settings.resolution.map(|r| (r.width, r.height)))).collect();
        assert_eq!(named, vec![
            ((16, 9), "/out/film_16x9.mp4".to_string(), Some((1920, 1080))),
            ((9, 16), "/out/film_9x16.mp4".to_string(), Some((1080, 1920))),
            ((4, 5), "/out/film_4x5.mp4".to_string(), Some((1080, 1350))),
        ]);
        assert!(outputs.iter().all(|o| o.settings.target_lufs == Some(-14.0)));
        assert!(delivery_outputs(&[false; 4], "/out/film.mp4").is_empty());
    }
}
