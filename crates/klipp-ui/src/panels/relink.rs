//! The Relink Offline Media dialog: search a folder, show what each
//! missing item would relink to and why, and relink only what the user
//! keeps ticked.
//!
//! The matching is `klipp_media::relink`. This file runs its scan on a
//! worker thread, so a folder on a slow share does not stop the UI, and
//! turns its proposals into rows. A clear match starts ticked; a weak one
//! (a bare name or duration) starts unticked; a tie between candidates
//! starts with nothing chosen, because guessing between two cards'
//! `C0001.MP4` is the failure this dialog exists to prevent.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use klipp_core::AppCommand;
use klipp_media::relink::{self, Candidate, Criteria, Progress, Proposal, ScanResult, Scored, Wanted};

use crate::state::AppState;
use crate::theme::KlippTheme;

#[derive(Default)]
pub struct RelinkPanel {
    criteria: Criteria,
    /// The project this panel's search and plan belong to.
    epoch: u64,
    /// A folder to search, asked for before the dialog next drew.
    requested: Option<PathBuf>,
    scan: Option<Running>,
    plan: Option<Plan>,
    /// A search or re-score that ended without an answer.
    failure: Option<String>,
}

struct Running {
    dir: PathBuf,
    wanted: Vec<Wanted>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    /// The criteria the scan runs with; a toggle while it runs is
    /// applied when it finishes.
    criteria: Criteria,
    rx: mpsc::Receiver<(ScanResult, Vec<Proposal>)>,
}

struct Plan {
    dir: PathBuf,
    wanted: Arc<Vec<Wanted>>,
    found: Arc<ScanResult>,
    /// The criteria the scan ran with: it read headers only if these
    /// needed them.
    scanned_with: Criteria,
    proposals: Vec<Proposal>,
    rows: Vec<Row>,
    /// Re-scoring for changed criteria, on a worker: the criteria, then
    /// the proposals.
    rescoring: Option<mpsc::Receiver<(Criteria, Vec<Proposal>)>>,
}

struct Row {
    apply: bool,
    /// The candidate chosen, an index into `found.candidates`.
    choice: Option<usize>,
}

impl RelinkPanel {
    /// Search `dir` for every offline item in the pool when the dialog
    /// next draws, replacing any search already running.
    pub fn request_scan(&mut self, dir: PathBuf) {
        self.requested = Some(dir);
    }

    fn start_scan(&mut self, dir: PathBuf, state: &AppState, ctx: &egui::Context) {
        if let Some(old) = self.scan.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let wanted: Vec<Wanted> = state.media_pool.all().filter(|m| m.offline).map(Wanted::from_item).collect();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new(Progress::default()));
        let (tx, rx) = mpsc::channel();
        let (dir2, wanted2, cancel2, progress2, criteria, ctx) =
            (dir.clone(), wanted.clone(), cancel.clone(), progress.clone(), self.criteria, ctx.clone());
        let spawned = std::thread::Builder::new().name("relink-scan".into()).spawn(move || {
            let found = relink::scan(&dir2, &wanted2, criteria, &cancel2, |p| {
                *progress2.lock().unwrap_or_else(|e| e.into_inner()) = p;
                ctx.request_repaint();
            });
            // Scored here too: with a large folder it is not a UI-frame job.
            let proposals = relink::propose(&wanted2, &found.candidates, criteria);
            let _ = tx.send((found, proposals));
            ctx.request_repaint();
        });
        if let Err(e) = spawned {
            tracing::error!("relink: could not start the scan: {e}");
            return;
        }
        self.plan = None;
        self.failure = None;
        self.scan = Some(Running { dir, wanted, cancel, progress, criteria, rx });
    }

    /// Draw the dialog's contents. Returns the relinks the user applied.
    pub fn show(&mut self, ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        // A search belongs to the project it was made for.
        if self.epoch != state.project_epoch {
            if let Some(old) = self.scan.take() {
                old.cancel.store(true, Ordering::Relaxed);
            }
            self.plan = None;
            self.epoch = state.project_epoch;
        }
        if let Some(dir) = self.requested.take() {
            self.start_scan(dir, state, ui.ctx());
        }
        self.collect_scan(ui.ctx());
        if let Some(why) = &self.failure {
            ui.label(egui::RichText::new(why).color(theme.marker_error));
        }

        let offline = state.media_pool.all().filter(|m| m.offline).count();
        if offline == 0 && self.plan.is_none() && self.scan.is_none() {
            ui.label("All media is online.");
            return cmds;
        }
        ui.label(format!("{offline} offline item(s)"));
        ui.add_space(4.0);

        // What counts as a match. Changing it re-scores what the last
        // search found; it does not search again.
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Match on").color(theme.text_secondary));
            let c = &mut self.criteria;
            let before = *c;
            ui.checkbox(&mut c.reel, "Reel").on_hover_text("The reel or tape name the camera recorded");
            ui.checkbox(&mut c.timecode, "Timecode").on_hover_text("The start timecode");
            ui.checkbox(&mut c.name, "Filename");
            ui.checkbox(&mut c.name_any_extension, "Name, any extension")
                .on_hover_text("A transcode: A001C001.MXF found as A001C001.mov");
            ui.checkbox(&mut c.duration, "Duration").on_hover_text("Within one frame");
            ui.label(egui::RichText::new("\u{24D8}").color(theme.text_dim)).on_hover_text(
                "The folders a file sits in, matched from the file upward, break ties between files of the same name — two cards' C0001.MP4 — but never make a match on their own.",
            );
            if *c != before {
                if let Some(plan) = &mut self.plan {
                    plan.repropose(*c, ui.ctx());
                }
            }
        });
        ui.label(
            egui::RichText::new("A copy with the same content always matches. A reel, timecode or duration that disagrees rules a file out.")
                .size(10.0)
                .color(theme.text_dim),
        );
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            let searching = self.scan.is_some();
            if ui.add_enabled(!searching, egui::Button::new("Search Folder\u{2026}")).clicked() {
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Select folder to search for media")
                        .pick_folder()
                        .map(|dir| vec![AppCommand::RelinkAllFromDirectory { directory: dir }])
                        .unwrap_or_default()
                });
            }
            if let Some(dir) = self.plan.as_ref().map(|p| p.dir.clone()) {
                if ui.add_enabled(!searching, egui::Button::new("Search Again")).on_hover_text(dir.display().to_string()).clicked() {
                    self.requested = Some(dir);
                }
            }
            if let Some(scan) = &self.scan {
                let p = *scan.progress.lock().unwrap_or_else(|e| e.into_inner());
                ui.spinner();
                ui.label(if p.to_read == 0 {
                    format!("Looking in {}\u{2026} {} media files", scan.dir.display(), p.files_seen)
                } else {
                    format!("Reading {} of {} files\u{2026}", p.read, p.to_read)
                });
                if ui.button("Stop").clicked() {
                    scan.cancel.store(true, Ordering::Relaxed);
                }
            }
        });

        ui.separator();
        if let Some(plan) = &mut self.plan {
            // The plan stays after relinking: rows that went online say
            // so, and the ones still to choose are still there.
            cmds = plan.show(ui, state, theme, self.criteria);
        } else {
            // Before a search: what is missing, each locatable by hand.
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for item in state.media_pool.all().filter(|m| m.offline) {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme.marker_error, "OFFLINE");
                        ui.label(&item.filename);
                        ui.label(egui::RichText::new(item.path.to_string_lossy()).size(10.0).color(theme.text_dim));
                        browse_button(ui, state, item.id);
                    });
                }
            });
        }
        cmds
    }

    fn collect_scan(&mut self, ctx: &egui::Context) {
        let Some(scan) = &self.scan else { return };
        let (found, proposals) = match scan.rx.try_recv() {
            Ok(done) => done,
            Err(mpsc::TryRecvError::Empty) => return,
            // The worker died (a panic): say so rather than spin forever.
            Err(mpsc::TryRecvError::Disconnected) => {
                self.scan = None;
                self.failure = Some("The search stopped unexpectedly; see the log, and try Search Folder again.".into());
                return;
            }
        };
        let scan = self.scan.take().expect("checked above");
        let rows = proposals.iter().map(Row::for_proposal).collect();
        let mut plan = Plan {
            dir: scan.dir,
            wanted: Arc::new(scan.wanted),
            found: Arc::new(found),
            scanned_with: scan.criteria,
            proposals,
            rows,
            rescoring: None,
        };
        // Criteria changed while the search ran: score for what is ticked now.
        if self.criteria != scan.criteria {
            plan.repropose(self.criteria, ctx);
        }
        self.plan = Some(plan);
    }
}

impl Row {
    fn for_proposal(p: &Proposal) -> Row {
        match p {
            Proposal::Found(s) => Row { apply: s.is_strong(), choice: Some(s.candidate) },
            Proposal::Ambiguous(_) | Proposal::NotFound { .. } => Row { apply: false, choice: None },
        }
    }
}

impl Plan {
    fn repropose(&mut self, criteria: Criteria, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (wanted, found, ctx) = (self.wanted.clone(), self.found.clone(), ctx.clone());
        let spawned = std::thread::Builder::new().name("relink-score".into()).spawn(move || {
            let _ = tx.send((criteria, relink::propose(&wanted, &found.candidates, criteria)));
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => self.rescoring = Some(rx),
            Err(e) => tracing::error!("relink: could not re-score: {e}"),
        }
    }

    fn collect_rescore(&mut self, current: Criteria) {
        let Some(rx) = &self.rescoring else { return };
        let (criteria, proposals) = match rx.try_recv() {
            Ok(done) => done,
            Err(mpsc::TryRecvError::Empty) => return,
            // The re-score died: keep the last proposals rather than spin.
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::error!("relink: re-scoring ended without an answer");
                self.rescoring = None;
                return;
            }
        };
        self.rescoring = None;
        // A later toggle has its own re-score coming.
        if criteria == current {
            self.rows = proposals.iter().map(Row::for_proposal).collect();
            self.proposals = proposals;
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme, criteria: Criteria) -> Vec<AppCommand> {
        self.collect_rescore(criteria);
        if self.rescoring.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Re-scoring\u{2026}");
            });
            return Vec::new();
        }
        let found = self.proposals.iter().filter(|p| matches!(p, Proposal::Found(_))).count();
        let choices = self.proposals.iter().filter(|p| matches!(p, Proposal::Ambiguous(_))).count();
        let missing = self.proposals.len() - found - choices;
        ui.label(format!(
            "In {}: {found} found, {choices} to choose between, {missing} not found",
            self.dir.display()
        ));
        let mut notes = Vec::new();
        if self.found.cancelled {
            notes.push("the search was stopped before it finished".to_string());
        }
        if self.found.hit_file_limit {
            notes.push(format!("the search stopped after {} media files", relink::MAX_FILES));
        }
        if relink::needs_headers(criteria) && !relink::needs_headers(self.scanned_with) {
            notes.push(
                "reel, timecode and duration were read only for files sharing a name or size with a missing item; Search Again to use them for the rest"
                    .to_string(),
            );
        }
        if self.found.skipped_dirs > 0 {
            notes.push(format!("{} folders could not be read or are links, and were skipped", self.found.skipped_dirs));
        }
        for n in notes {
            ui.label(egui::RichText::new(format!("Note: {n}.")).size(10.0).color(theme.marker_warning));
        }
        ui.add_space(4.0);

        let candidates = &self.found.candidates;
        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
            for (i, (w, proposal)) in self.wanted.iter().zip(&self.proposals).enumerate() {
                let row = &mut self.rows[i];
                let still_offline = state.media_pool.get(&w.media_id).is_some_and(|m| m.offline);
                ui.horizontal(|ui| {
                    ui.add_enabled(still_offline && row.choice.is_some(), egui::Checkbox::without_text(&mut row.apply));
                    ui.label(egui::RichText::new(&w.filename).strong());
                    if !still_offline {
                        ui.label(egui::RichText::new("online").color(theme.marker_success));
                        return;
                    }
                    match proposal {
                        Proposal::Found(s) => {
                            ui.label("\u{2192}");
                            scored_label(ui, theme, s, candidates);
                        }
                        Proposal::Ambiguous(options) => {
                            let text = row
                                .choice
                                .map(|c| candidates[c].path.display().to_string())
                                .unwrap_or_else(|| format!("{} candidates match equally \u{2014} choose", options.len()));
                            egui::ComboBox::from_id_salt(("relink_choice", i)).selected_text(text).width(320.0).show_ui(ui, |ui| {
                                for s in options {
                                    let label = format!("{}  ({})", candidates[s.candidate].path.display(), criteria_text(s));
                                    if ui.selectable_label(row.choice == Some(s.candidate), label).clicked() {
                                        row.choice = Some(s.candidate);
                                        row.apply = true;
                                    }
                                }
                            });
                        }
                        Proposal::NotFound { rejected } => {
                            let r = ui.label(egui::RichText::new("not found").color(theme.marker_error));
                            if !rejected.is_empty() {
                                r.on_hover_text(format!("Ruled out:\n{}", rejected.join("\n")));
                            }
                            browse_button(ui, state, w.media_id);
                        }
                    }
                });
            }
        });

        ui.add_space(6.0);
        let ready: Vec<(klipp_core::MediaId, PathBuf)> = self
            .wanted
            .iter()
            .zip(&self.rows)
            .filter(|(w, r)| r.apply && state.media_pool.get(&w.media_id).is_some_and(|m| m.offline))
            .filter_map(|(w, r)| r.choice.map(|c| (w.media_id, self.found.candidates[c].path.clone())))
            .collect();
        let mut cmds = Vec::new();
        ui.horizontal(|ui| {
            if ui.add_enabled(!ready.is_empty(), egui::Button::new(format!("Relink {} Ticked", ready.len()))).clicked() {
                // Every one relinked as one step.
                cmds.push(AppCommand::BeginHistoryBatch);
                cmds.extend(ready.into_iter().map(|(media_id, new_path)| AppCommand::RelinkMedia { media_id, new_path }));
                cmds.push(AppCommand::EndHistoryBatch);
                for row in &mut self.rows {
                    row.apply = false;
                }
            }
            if ui.button("Tick Clear Matches").on_hover_text("Tick every row whose match is strong").clicked() {
                for (row, p) in self.rows.iter_mut().zip(&self.proposals) {
                    if let Proposal::Found(s) = p {
                        row.apply = s.is_strong();
                    }
                }
            }
            if ui.button("Untick All").clicked() {
                for row in &mut self.rows {
                    row.apply = false;
                }
            }
        });
        cmds
    }
}

fn criteria_text(s: &Scored) -> String {
    s.matched.iter().map(|c| c.label()).collect::<Vec<_>>().join(", ")
}

fn scored_label(ui: &mut egui::Ui, theme: &KlippTheme, s: &Scored, candidates: &[Candidate]) {
    let path = &candidates[s.candidate].path;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    ui.label(name).on_hover_text(path.display().to_string());
    let (text, colour) = if s.is_strong() {
        (format!("matched {}", criteria_text(s)), theme.text_secondary)
    } else {
        (format!("only {} \u{2014} check before ticking", criteria_text(s)), theme.marker_warning)
    };
    ui.label(egui::RichText::new(text).size(10.0).color(colour));
    if !s.warnings.is_empty() {
        ui.label(egui::RichText::new(format!("\u{26A0} {}", s.warnings.join("; "))).size(10.0).color(theme.marker_warning))
            .on_hover_text("Relinking is still possible — a proxy is often smaller — but the file is not identical to the original");
    }
}

/// Locate one item by hand. The picker is filtered to the original
/// extension, which keeps a slip of the mouse from relinking a clip to
/// the wrong kind of file.
fn browse_button(ui: &mut egui::Ui, state: &AppState, media_id: klipp_core::MediaId) {
    if !ui.small_button("Browse\u{2026}").clicked() {
        return;
    }
    let Some(item) = state.media_pool.get(&media_id) else { return };
    let ext = item.path.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
    let title = format!("Locate {}", item.filename);
    crate::file_dialog::spawn(move || {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if !ext.is_empty() {
            dialog = dialog.add_filter(ext.to_uppercase(), &[ext.as_str()]);
        }
        dialog
            .pick_file()
            .map(|new_path| vec![AppCommand::RelinkMediaByHand { media_id, new_path }])
            .unwrap_or_default()
    });
}
