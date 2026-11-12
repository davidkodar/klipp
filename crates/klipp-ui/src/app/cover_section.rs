//! The Export page's Cover section (#3311): see [`crate::cover_section`].

use super::*;
use crate::cover_section::{edit, key, label, thumb_size, Ask, Proposed, LIST_W};

impl KlippApp {
    /// How the project draws its pictures, as its export does.
    fn cover_look(&self) -> klipp_render::cover::Look {
        klipp_render::cover::Look {
            lut: self.state.timeline_lut().map(|p| (p, self.state.global_lut_opacity)),
            working: self.state.project_working_colorspace,
            grade_encoding: klipp_core::grade_encoding(),
        }
    }

    /// `ask` made as one undo step, or why not in a toast.
    fn cover_edit(&mut self, ask: Ask) {
        match edit(self.state.sequence(), &self.state.media_pool, &ask) {
            Ok((_, step)) => self.apply_edits_as_one(&[step], "Cover"),
            Err(why) => self.state.toast.show(format!("Cover: {why}")),
        }
    }

    /// What finished on the section's threads: pictures drawn, frames
    /// proposed, files written.
    fn poll_covers(&mut self, ctx: &egui::Context) {
        while let Ok((id, k, drawn)) = self.covers_panel.drawn_rx.try_recv() {
            match drawn {
                Ok(still) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied([still.width as usize, still.height as usize], &still.rgba);
                    let texture = ctx.load_texture(format!("cover-{id}"), image, egui::TextureOptions::LINEAR);
                    self.covers_panel.thumbs.insert(id, (k, texture));
                }
                Err(why) => tracing::warn!("a cover could not be drawn: {why}"),
            }
        }
        if let Some(rx) = &self.covers_panel.proposing {
            if let Ok(done) = rx.try_recv() {
                self.covers_panel.proposing = None;
                match done {
                    Ok((found, unread)) => {
                        if !unread.is_empty() {
                            self.state.toast.show(format!("No faces proposed from {}: analyse its framing first", unread.join(", ")));
                        }
                        self.covers_panel.proposals = Some(found);
                    }
                    Err(why) => self.state.toast.show(format!("No frames proposed: {why}")),
                }
            }
        }
        while let Ok(note) = self.covers_panel.notes_rx.try_recv() {
            self.state.toast.show(note);
        }
    }

    /// `cover`'s picture drawn on a thread, unless it is drawn already or
    /// being drawn for what it is now.
    fn ask_cover_drawn(&mut self, cover: &klipp_timeline::cover::Cover, k: u64, ctx: &egui::Context) {
        let have = self.covers_panel.thumbs.get(&cover.id).map(|(k, _)| *k);
        if have == Some(k) || self.covers_panel.asked.get(&cover.id) == Some(&k) {
            return;
        }
        self.covers_panel.asked.insert(cover.id, k);
        let (seq, all, pool, look, cover, tx, ctx) = (self.state.sequence().clone(), self.state.sequences.clone(), self.state.media_pool.clone(), self.cover_look(), cover.clone(), self.covers_panel.drawn_tx.clone(), ctx.clone());
        let (w, h) = thumb_size(&cover);
        let _ = std::thread::Builder::new().name("cover-draw".into()).spawn(move || {
            let drawn = klipp_render::cover::draw(&seq, &all, &pool, &look, &cover, w, h);
            let _ = tx.send((cover.id, k, drawn));
            ctx.request_repaint();
        });
    }

    /// Frames for a cover proposed on a thread.
    fn propose_cover_frames(&mut self, ctx: &egui::Context) {
        let (seq, all, pool, look, ctx) = (self.state.sequence().clone(), self.state.sequences.clone(), self.state.media_pool.clone(), self.cover_look(), ctx.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.covers_panel.proposing = Some(rx);
        let _ = std::thread::Builder::new().name("cover-propose".into()).spawn(move || {
            let found = klipp_render::cover::proposals(&seq, &all, &pool, &look, &|p| klipp_analysis::report::MediaAnalysis::load(p))
                .map(|(found, unread)| (found.into_iter().map(|(p, _)| Proposed { at_secs: p.at_secs, why: p.why }).collect(), unread));
            let _ = tx.send(found);
            ctx.request_repaint();
        });
    }

    /// `cover` written where the person picks, at its size, on a thread.
    fn export_cover(&mut self, cover: &klipp_timeline::cover::Cover) {
        let Some(size) = klipp_core::cover_sizes::by_id(&cover.size) else { return };
        let stem = if cover.name.is_empty() { "cover".to_string() } else { cover.name.replace(['/', '\\'], "-") };
        let Some(path) = rfd::FileDialog::new().set_title("Export Cover").set_file_name(format!("{stem}.jpg")).add_filter("JPEG", &["jpg", "jpeg"]).add_filter("PNG", &["png"]).save_file() else { return };
        let Some(format) = klipp_render::still::StillFormat::of_path(&path) else {
            self.state.toast.show("A cover is written as .png, .jpg or .jpeg");
            return;
        };
        let (seq, all, pool, look, cover, tx, size) = (self.state.sequence().clone(), self.state.sequences.clone(), self.state.media_pool.clone(), self.cover_look(), cover.clone(), self.covers_panel.notes_tx.clone(), size.clone());
        let _ = std::thread::Builder::new().name("cover-export".into()).spawn(move || {
            let note = klipp_render::cover::draw(&seq, &all, &pool, &look, &cover, size.width, size.height)
                .and_then(|still| klipp_render::still::encode(&still, format, size.max_bytes))
                .and_then(|(bytes, quality)| std::fs::write(&path, &bytes).map(|_| (bytes.len(), quality)).map_err(|e| e.to_string()));
            let _ = tx.send(match note {
                Ok((n, Some(q))) => format!("Cover written: {} ({:.2} MB, JPEG quality {q})", path.display(), n as f64 / 1e6),
                Ok((n, None)) => format!("Cover written: {} ({:.2} MB)", path.display(), n as f64 / 1e6),
                Err(why) => format!("Cover not written: {why}"),
            });
        });
    }

    /// The section, for a sequence it shows for (#3311).
    pub(super) fn cover_section(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>) {
        if !self.prefs.export.cover_section.shows(self.state.sequence()) {
            return;
        }
        let ctx = ui.ctx().clone();
        self.poll_covers(&ctx);
        let covers = self.state.sequence().covers.clone();
        let revision = (self.state.active_history().undo_count(), self.state.active_history().redo_count());
        let header = if covers.is_empty() { "Cover".to_string() } else { format!("Cover ({})", covers.len()) };
        egui::CollapsingHeader::new(header).id_salt("export_cover").default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("The video's thumbnail: a frame with words over it, drawn as the export draws it.").small().weak());
                if ui.small_button("Hide").on_hover_text("Hide this section; the command palette's Show Cover in Export brings it back").clicked() {
                    cmds.push(AppCommand::ShowCoverSection { shown: false });
                }
            });
            ui.horizontal(|ui| {
                if ui.button("New Cover at Playhead").clicked() {
                    let at = self.state.playhead.to_secs_f64();
                    self.cover_edit(Ask::New { at_secs: at });
                }
                let busy = self.covers_panel.proposing.is_some();
                if ui.add_enabled(!busy, egui::Button::new(if busy { "Proposing\u{2026}" } else { "Propose Frames" }))
                    .on_hover_text("Markers first, then faces by size, how squarely they face the camera and sharpness, then still moments; eyes open and expression aren't judged")
                    .clicked()
                {
                    self.propose_cover_frames(&ctx);
                }
                if !covers.is_empty() {
                    ui.checkbox(&mut self.covers_panel.compare, "Compare").on_hover_text("Every cover side by side at the size a phone's search list shows it");
                }
            });
            if let Some(found) = self.covers_panel.proposals.clone() {
                if found.is_empty() {
                    ui.label(egui::RichText::new("No frames to propose: add markers, or analyse the footage's framing.").small().weak());
                }
                for p in found {
                    ui.horizontal(|ui| {
                        ui.label(format!("{:.2} s \u{00b7} {}", p.at_secs, p.why));
                        if ui.small_button("Show").on_hover_text("Move the playhead there").clicked() {
                            cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(p.at_secs, self.state.sequence().fps.rate())));
                        }
                        if ui.small_button("Make Cover").clicked() {
                            self.cover_edit(Ask::New { at_secs: p.at_secs });
                        }
                    });
                }
            }
            for c in &covers {
                self.ask_cover_drawn(c, key(c, revision), &ctx);
            }
            if self.covers_panel.compare {
                ui.horizontal_wrapped(|ui| {
                    for c in &covers {
                        if let Some((_, t)) = self.covers_panel.thumbs.get(&c.id) {
                            let size = t.size_vec2() * (LIST_W / t.size_vec2().x);
                            ui.add(egui::Image::new((t.id(), size))).on_hover_text(label(c));
                        }
                    }
                });
                return;
            }
            for c in &covers {
                ui.separator();
                let width = ui.available_width().min(crate::cover_section::FEED_W);
                match self.covers_panel.thumbs.get(&c.id) {
                    Some((_, t)) => {
                        let size = t.size_vec2() * (width / t.size_vec2().x);
                        ui.add(egui::Image::new((t.id(), size)));
                    }
                    None => {
                        let (w, h) = thumb_size(c);
                        ui.allocate_exact_size(egui::vec2(width, width * h as f32 / w as f32), egui::Sense::hover());
                    }
                }
                let selected = self.covers_panel.selected == Some(c.id);
                if ui.selectable_label(selected, label(c)).clicked() {
                    self.covers_panel.selected = Some(c.id);
                    self.covers_panel.words = c.text.as_ref().and_then(|t| t.primary_text()).map(|t| t.text.clone()).unwrap_or_default();
                }
                if selected {
                    ui.horizontal(|ui| {
                        ui.label("Words");
                        let r = ui.text_edit_singleline(&mut self.covers_panel.words);
                        if r.lost_focus() {
                            let now = c.text.as_ref().and_then(|t| t.primary_text()).map(|t| t.text.clone()).unwrap_or_default();
                            if now != self.covers_panel.words {
                                let words = self.covers_panel.words.clone();
                                self.cover_edit(Ask::Words { cover: c.id, text: &words });
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut size = c.size.clone();
                        egui::ComboBox::from_id_salt(("cover_size", c.id.0)).selected_text(klipp_core::cover_sizes::by_id(&size).map_or(size.clone(), |s| s.name.clone())).show_ui(ui, |ui| {
                            for s in klipp_core::cover_sizes::all() {
                                ui.selectable_value(&mut size, s.id.clone(), format!("{} {}×{}", s.name, s.width, s.height));
                            }
                        });
                        if size != c.size {
                            self.cover_edit(Ask::Size { cover: c.id, size: &size });
                        }
                        if ui.small_button("Export\u{2026}").clicked() {
                            self.export_cover(c);
                        }
                        if ui.small_button("Go to Frame").on_hover_text("Move the playhead to the cover's frame").clicked() {
                            cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(c.at_secs, self.state.sequence().fps.rate())));
                        }
                        if ui.small_button("Remove").clicked() {
                            self.cover_edit(Ask::Remove { cover: c.id });
                            self.covers_panel.selected = None;
                        }
                    });
                }
            }
        });
    }
}
