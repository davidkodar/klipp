//! Dedicated subtitle editor panel.
//!
//! A list-based editing surface for all subtitle and text clips in the active
//! sequence, sorted by timeline_in. Each row shows start/end timecodes and
//! the text with inline multiline editing. The viewer's double-click text
//! editor remains unchanged — this panel is an additional editing surface.
//!
//! Mirrors the structural pattern of `panels/markers.rs`: ScrollArea,
//! per-row rendering, AppState flag for visibility, edits flow back as
//! AppCommands through the existing handlers.

use crate::state::AppState;
use crate::widgets::timecode_edit::timecode_edit;
use klipp_core::{AppCommand, ClipId, Fps, Timecode, TrackId};

/// Row data flattened from all subtitle/text tracks, sorted by timeline_in.
#[derive(Debug, Clone)]
struct SubRow {
    clip_id: ClipId,
    track_index: usize,
    track_id: TrackId,
    timeline_in: Timecode,
    timeline_out: Timecode,
    source_in: Timecode,
    source_out: Timecode,
    text: String,
    /// All TitleData fields that must be preserved when emitting UpdateTitleData.
    font_size: f32,
    font_name: String,
    font_style_u8: u8,
    color: [u8; 4],
}

/// Per-track descriptor used by the track-grouped editor.
#[derive(Debug, Clone)]
struct SubTrack {
    track_id: TrackId,
    name: String,
    source_file: Option<std::path::PathBuf>,
    clip_count: usize,
}

/// Collect all subtitle/text clips across tracks, sorted by timeline_in.
fn collect_subtitle_clips(seq: &klipp_timeline::Sequence) -> Vec<SubRow> {
    let mut rows = Vec::new();
    for (track_idx, track) in seq.tracks.iter().enumerate() {
        if !matches!(
            track.kind,
            klipp_timeline::TrackKind::Subtitle | klipp_timeline::TrackKind::Text
        ) {
            continue;
        }
        for clip in &track.clips {
            if let klipp_timeline::ClipKind::Title(td) = &clip.kind {
                rows.push(SubRow {
                    clip_id: clip.id,
                    track_index: track_idx,
                    track_id: track.id,
                    timeline_in: clip.timeline_in,
                    timeline_out: clip.timeline_out,
                    source_in: clip.source_in,
                    source_out: clip.source_out,
                    text: td.text().clone(),
                    font_size: td.font_size(),
                    font_name: td.font_name().clone(),
                    font_style_u8: font_style_to_u8(td.font_style()),
                    color: td.color(),
                });
            }
        }
    }
    rows.sort_by_key(|r| r.timeline_in);
    rows
}

/// Collect descriptors for all subtitle/text tracks in display order.
fn collect_subtitle_tracks(seq: &klipp_timeline::Sequence) -> Vec<SubTrack> {
    seq.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            matches!(
                t.kind,
                klipp_timeline::TrackKind::Subtitle | klipp_timeline::TrackKind::Text
            )
        })
        .map(|(_idx, t)| SubTrack {
            track_id: t.id,
            name: t.name.clone(),
            source_file: t.source_file.clone(),
            clip_count: t.clips.len(),
        })
        .collect()
}

/// Find the index of the subtitle whose range contains the playhead.
/// Returns None if no subtitle covers the playhead.
fn active_subtitle_at(rows: &[SubRow], playhead: Timecode) -> Option<usize> {
    rows.iter().position(|r| {
        r.timeline_in <= playhead && playhead < r.timeline_out
    })
}

/// Compute reading-speed warning color. <=3.5 w/s: none. 3.5-5.0: amber. >5.0: red.
fn reading_speed_color(words_per_sec: f64) -> Option<egui::Color32> {
    if words_per_sec > 5.0 {
        Some(egui::Color32::from_rgb(0xff, 0x60, 0x60))
    } else if words_per_sec > 3.5 {
        Some(egui::Color32::from_rgb(0xe0, 0xa0, 0x30))
    } else {
        None
    }
}

fn font_style_to_u8(style: klipp_timeline::FontStyle) -> u8 {
    match style {
        klipp_timeline::FontStyle::Regular => 0,
        klipp_timeline::FontStyle::Bold => 1,
        klipp_timeline::FontStyle::Italic => 2,
        klipp_timeline::FontStyle::BoldItalic => 3,
    }
}

/// The "Captions from Transcript" form: `Op::Captions`'s settings, run as
/// one undo step through the same resolver the assistant uses. Captions go
/// on the subtitle track selected in this editor, or the first one (made
/// if there is none).
fn captions_form(ui: &mut egui::Ui, state: &mut AppState) {
    use klipp_document::ops::{Anchor, Op, TrackRef};
    let has_range = state.sequence().in_point.is_some() && state.sequence().out_point.is_some();
    let Some(form) = state.subtitle_editor.captions_form.as_mut() else { return };
    // With no in and out there is no "in to out": the whole sequence,
    // said so, rather than a ticked box that means nothing.
    if !has_range {
        form.in_out_only = false;
    }
    let mut make = false;
    ui.group(|ui| {
        // Plain captions, or a few words at a time with the spoken word
        // shown (#2710).
        ui.horizontal_wrapped(|ui| {
            ui.label("Style");
            egui::ComboBox::from_id_salt("captions_style")
                .selected_text(form.animated.map_or("Plain", |m| m.label()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut form.animated, None, "Plain");
                    for m in klipp_timeline::CaptionMotion::ALL {
                        ui.selectable_value(&mut form.animated, Some(m), m.label());
                    }
                });
            if form.animated.is_some() {
                ui.label("Words on screen");
                ui.add(egui::DragValue::new(&mut form.words_per_screen).range(1..=5));
                ui.label("Spoken word");
                let [r, g, b, a] = form.active_color;
                let mut c = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    form.active_color = c.to_array();
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            // An animated caption is one line of a few words.
            ui.add_enabled_ui(form.animated.is_none(), |ui| {
                ui.label("Characters per line");
                ui.add(egui::DragValue::new(&mut form.max_chars).range(8..=80));
                ui.label("Lines");
                ui.add(egui::DragValue::new(&mut form.max_lines).range(1..=3));
            });
            ui.add_enabled(has_range, egui::Checkbox::new(&mut form.in_out_only, "In to out only"))
                .on_disabled_hover_text("Set in and out points on the timeline first");
            ui.checkbox(&mut form.replace, "Replace captions already there")
                .on_hover_text("After a re-cut or a better transcript: take the old captions in that range away first");
            make = ui.button("Make Captions").clicked();
        });
    });
    if !make {
        return;
    }
    let form = form.clone();
    let seq = state.sequence();
    let track = state
        .subtitle_editor
        .selected_subtitle_track
        .and_then(|id| seq.tracks.iter().position(|t| t.id == id))
        .map(TrackRef::Index);
    let (from, to) = if form.in_out_only {
        (seq.in_point.map(|t| Anchor::Secs(t.to_secs_f64())), seq.out_point.map(|t| Anchor::Secs(t.to_secs_f64())))
    } else {
        (None, None)
    };
    let animate = form.animated.map(|m| klipp_document::ops::AnimateSpec {
        style: Some(m.name().to_string()),
        words_per_screen: Some(form.words_per_screen),
        active_color: Some(format!("#{:02x}{:02x}{:02x}", form.active_color[0], form.active_color[1], form.active_color[2])),
        ..Default::default()
    });
    let op = Op::Captions { track, from, to, max_chars: form.max_chars, max_lines: form.max_lines, replace: form.replace, animate };
    state.pending_doc_ops.push(crate::state::DocOpsRequest { label: "Captions from transcript".to_string(), ops: vec![op], independent: false });
    state.subtitle_editor.captions_form = None;
}

pub fn show(ctx: &egui::Context, state: &mut AppState) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let mut open = state.show_subtitle_editor;

    let fps = state.sequence().fps;
    let playhead = state.playhead;
    let selected_clip = state.selected_clip;

    let rows = collect_subtitle_clips(state.sequence());
    let active_idx = active_subtitle_at(&rows, playhead);

    egui::Window::new("Subtitle Editor")
        .open(&mut open)
        .default_width(520.0)
        .default_height(420.0)
        .resizable(true)
        .show(ctx, |ui| {
            // ── Top toolbar ──────────────────────────────────────
            ui.horizontal(|ui| {
                if ui.button("+ Add at playhead").clicked() {
                    // Flush any pending text edit first
                    flush_text_edit(&mut state.subtitle_editor, &rows, &mut cmds);
                    let dur_frames = (fps.as_f64() * 3.0).round() as u32;
                    cmds.push(AppCommand::AddSubtitleAtPlayhead {
                        timeline_in: playhead,
                        duration_frames: dur_frames,
                    });
                }

                // Split — enabled only if playhead is inside the selected subtitle
                let can_split = selected_clip
                    .and_then(|cid| rows.iter().find(|r| r.clip_id == cid))
                    .map(|r| r.timeline_in < playhead && playhead < r.timeline_out)
                    .unwrap_or(false);
                if ui.add_enabled(can_split, egui::Button::new("\u{2702} Split")).clicked() {
                    if let Some(cid) = selected_clip {
                        if let Some(row) = rows.iter().find(|r| r.clip_id == cid) {
                            cmds.push(AppCommand::BladeClip {
                                track_index: row.track_index,
                                clip_id: cid,
                                cut_at: playhead,
                            });
                        }
                    }
                }

                // Delete — enabled only if a subtitle is selected
                let can_delete = selected_clip
                    .is_some_and(|cid| rows.iter().any(|r| r.clip_id == cid));
                if ui.add_enabled(can_delete, egui::Button::new("\u{1F5D1} Delete")).clicked() {
                    if let Some(cid) = selected_clip {
                        cmds.push(AppCommand::RemoveClip { clip_id: cid });
                    }
                }

                ui.separator();

                // Ripple toggle
                let ripple = state.subtitle_editor.ripple;
                let ripple_btn = if ripple {
                    egui::Button::new("\u{27F3} Ripple").fill(egui::Color32::from_rgb(0x60, 0x80, 0x40))
                } else {
                    egui::Button::new("\u{27F3} Ripple")
                };
                if ui.add(ripple_btn).on_hover_text(
                    "When enabled, adjusting a subtitle's timing shifts all subsequent subtitles.",
                ).clicked() {
                    state.subtitle_editor.ripple = !ripple;
                }

                // Import one or more subtitle files (SRT/VTT/ASS/SSA/TTML/SCC/STL).
                // Each file becomes its own subtitle track named after the file stem.
                if ui.button("\u{2191} Import Files\u{2026}").clicked() {
                    crate::file_dialog::spawn(|| {
                        rfd::FileDialog::new()
                            .add_filter("Subtitles", &["srt", "vtt", "ass", "ssa", "ttml", "dfxp", "xml", "itt", "scc", "stl"])
                            .pick_files()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|path| AppCommand::ImportSubtitles { path })
                            .collect()
                    });
                }

                // Export (format selected by file extension)
                if ui.button("\u{2193} Export\u{2026}").clicked() {
                    crate::file_dialog::spawn(move || {
                        rfd::FileDialog::new()
                            .set_title("Export Subtitles")
                            .add_filter("SRT", &["srt"])
                            .add_filter("WebVTT", &["vtt"])
                            .add_filter("ASS", &["ass"])
                            .add_filter("SSA", &["ssa"])
                            .add_filter("TTML (IMSC)", &["ttml"])
                            .add_filter("SCC (CEA-608)", &["scc"])
                            .add_filter("EBU-STL", &["stl"])
                            .set_file_name("subtitles.srt")
                            .save_file()
                            .map(|path| vec![AppCommand::ExportSubtitles { path }])
                            .unwrap_or_default()
                    });
                }

                ui.separator();
                let open = state.subtitle_editor.captions_form.is_some();
                if ui
                    .selectable_label(open, "Captions from Transcript\u{2026}")
                    .on_hover_text("Make captions from what is said on the timeline, from the clips' transcripts")
                    .clicked()
                {
                    state.subtitle_editor.captions_form = if open { None } else { Some(Default::default()) };
                }
            });

            captions_form(ui, state);

            ui.separator();

            // ── Track-grouped list + empty state ──────────────────
            let sub_tracks = collect_subtitle_tracks(state.sequence());
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if sub_tracks.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label(
                            egui::RichText::new("No subtitle tracks yet.")
                                .size(13.0)
                                .color(egui::Color32::from_rgb(0xa0, 0xa0, 0xb0)),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Click [+ Add Subtitle Track] below or [\u{2191} Import Files\u{2026}] above to get started.")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(0x70, 0x70, 0x80)),
                        );
                        ui.add_space(8.0);
                    });
                } else {
                    let mut row_counter: usize = 0;
                    for st in &sub_tracks {
                        render_track_header(ui, st, playhead, fps, state, &mut cmds);
                        render_style_form(ui, st, state, &mut cmds);

                        let collapsed = state.subtitle_editor.subtitle_track_collapsed.contains(&st.track_id);
                        if !collapsed {
                            // Render only rows for this track (the flat `rows` list is sorted
                            // across all tracks; per-track ordering is consistent by timeline_in).
                            for (i, row) in rows.iter().enumerate() {
                                if row.track_id != st.track_id { continue; }
                                let is_active = active_idx == Some(i);
                                let is_selected = selected_clip == Some(row.clip_id);
                                let row_frame = egui::Frame::none()
                                    .fill(if is_active {
                                        egui::Color32::from_rgba_unmultiplied(0xff, 0xdc, 0x32, 30)
                                    } else if is_selected {
                                        egui::Color32::from_rgba_unmultiplied(0x40, 0x80, 0xff, 20)
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    })
                                    .inner_margin(egui::Margin::same(4.0))
                                    .stroke(egui::Stroke::new(
                                        1.0_f32,
                                        egui::Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 10),
                                    ));
                                row_frame.show(ui, |ui| {
                                    render_row(ui, row_counter, row, fps, state, &mut cmds);
                                });
                                ui.add_space(2.0);
                                row_counter += 1;
                            }
                            if st.clip_count == 0 {
                                ui.label(
                                    egui::RichText::new("  (empty track)")
                                        .size(10.0)
                                        .italics()
                                        .color(egui::Color32::from_rgb(0x70, 0x70, 0x80)),
                                );
                            }
                        }
                        ui.add_space(4.0);
                    }
                }

                // ── Footer: add-track / import-files ──────────────
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("+ Add Subtitle Track").clicked() {
                        let seq = state.sequence();
                        let name = seq.next_track_name(klipp_timeline::TrackKind::Subtitle);
                        cmds.push(AppCommand::AddSubtitleTrack { name });
                    }
                    if ui.button("\u{2191} Import Files\u{2026}").clicked() {
                        crate::file_dialog::spawn(|| {
                            rfd::FileDialog::new()
                                .add_filter("Subtitles", &["srt", "vtt", "ass", "ssa", "ttml", "dfxp", "xml", "itt", "scc", "stl"])
                                .pick_files()
                                .unwrap_or_default()
                                .into_iter()
                                .map(|path| AppCommand::ImportSubtitles { path })
                                .collect()
                        });
                    }
                });
            });
        });

    // Flush any pending edit if editing_text clip is no longer in the list
    // (defensive: e.g. if clip was deleted while being edited)
    if let Some(cid) = state.subtitle_editor.editing_text {
        if !rows.iter().any(|r| r.clip_id == cid) {
            state.subtitle_editor.editing_text = None;
            state.subtitle_editor.text_buf.clear();
        }
    }

    state.show_subtitle_editor = open;
    cmds
}

/// Render a collapsible track header row — chevron, name (editable on double-click),
/// source file label, add-clip button, delete-track button.
fn render_track_header(
    ui: &mut egui::Ui,
    st: &SubTrack,
    playhead: Timecode,
    fps: Fps,
    state: &mut AppState,
    cmds: &mut Vec<AppCommand>,
) {
    let collapsed = state.subtitle_editor.subtitle_track_collapsed.contains(&st.track_id);
    let hdr_frame = egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 6))
        .inner_margin(egui::Margin::symmetric(4.0, 3.0))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 16),
        ));
    hdr_frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            // Collapse chevron
            let arrow = if collapsed { "\u{25B6}" } else { "\u{25BC}" };
            if ui.small_button(arrow).clicked() {
                if collapsed {
                    state.subtitle_editor.subtitle_track_collapsed.remove(&st.track_id);
                } else {
                    state.subtitle_editor.subtitle_track_collapsed.insert(st.track_id);
                }
            }

            // Editable name (double-click to edit, Enter/blur to commit)
            let is_editing_name = state.subtitle_editor.editing_track_name == Some(st.track_id);
            if is_editing_name {
                let buf = &mut state.subtitle_editor.track_name_buf;
                let resp = ui.add(
                    egui::TextEdit::singleline(buf)
                        .desired_width(160.0)
                        .font(egui::TextStyle::Body),
                );
                if resp.lost_focus()
                    || ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    let new_name = buf.trim().to_string();
                    if !new_name.is_empty() && new_name != st.name {
                        cmds.push(AppCommand::RenameTrack {
                            track_id: st.track_id,
                            name: new_name,
                        });
                    }
                    state.subtitle_editor.editing_track_name = None;
                    state.subtitle_editor.track_name_buf.clear();
                }
            } else {
                let label = egui::Label::new(
                    egui::RichText::new(&st.name).strong().size(12.0),
                )
                .sense(egui::Sense::click());
                let resp = ui.add(label);
                if resp.double_clicked() {
                    state.subtitle_editor.editing_track_name = Some(st.track_id);
                    state.subtitle_editor.track_name_buf = st.name.clone();
                }
            }

            if let Some(src) = &st.source_file {
                let filename = src
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if !filename.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("  Source: {filename}"))
                            .size(10.0)
                            .italics()
                            .color(egui::Color32::from_rgb(0x80, 0x90, 0xa0)),
                    );
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Delete track — fires RemoveTrack (app shows its own confirm if non-empty).
                if ui
                    .small_button("\u{1F5D1}")
                    .on_hover_text("Delete this track")
                    .clicked()
                {
                    cmds.push(AppCommand::RemoveTrack { track_id: st.track_id });
                }
                // The track's caption style (#70): opens the form under
                // the header.
                let styling = state.subtitle_editor.style_draft.as_ref().is_some_and(|(id, _)| *id == st.track_id);
                if ui
                    .small_button(if styling { "Style \u{25B4}" } else { "Style\u{2026}" })
                    .on_hover_text("The look of the captions on this track: new ones take it, and it can be put on all of them")
                    .clicked()
                {
                    state.subtitle_editor.style_draft = if styling {
                        None
                    } else {
                        let current = state.sequence().tracks.iter().find(|t| t.id == st.track_id).and_then(|t| t.caption_style.clone());
                        Some((st.track_id, current.unwrap_or_default()))
                    };
                }
                // Add row at playhead, on THIS track.
                if ui
                    .small_button("+ Row")
                    .on_hover_text("Add a subtitle clip at the playhead on this track")
                    .clicked()
                {
                    let dur_frames = (fps.as_f64() * 3.0).round() as u32;
                    cmds.push(AppCommand::AddSubtitleAtPlayhead {
                        timeline_in: playhead,
                        duration_frames: dur_frames,
                    });
                }
                ui.label(
                    egui::RichText::new(format!("{} clip(s)", st.clip_count))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(0x70, 0x70, 0x80)),
                );
            });
        });
    });
}

/// The caption style form under a track's header (#70): the draft is
/// the editor's own until it is saved.
fn render_style_form(ui: &mut egui::Ui, st: &SubTrack, state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    let has_style = state.sequence().tracks.iter().find(|t| t.id == st.track_id).is_some_and(|t| t.caption_style.is_some());
    let Some((id, draft)) = state.subtitle_editor.style_draft.as_mut() else { return };
    if *id != st.track_id {
        return;
    }
    let track_id = *id;
    let mut close = false;
    ui.group(|ui| {
        egui::Grid::new(("caption_style", track_id)).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Size");
            ui.add(egui::DragValue::new(&mut draft.size).range(12.0..=160.0).suffix(" px"));
            ui.end_row();
            ui.label("Font");
            ui.add(egui::TextEdit::singleline(&mut draft.font).hint_text("default").desired_width(140.0));
            ui.end_row();
            ui.label("Colour");
            ui.horizontal(|ui| {
                ui.color_edit_button_srgba_unmultiplied(&mut draft.color);
                ui.checkbox(&mut draft.bold, "Bold");
            });
            ui.end_row();
            ui.label("Outline");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut draft.outline).range(0.0..=12.0).speed(0.1).suffix(" px"));
                ui.color_edit_button_srgba_unmultiplied(&mut draft.outline_color);
            });
            ui.end_row();
            ui.label("Backing");
            ui.add(egui::Slider::new(&mut draft.backing, 0.0..=1.0).custom_formatter(|v, _| if v <= 0.0 { "none".into() } else { format!("{:.0}%", v * 100.0) }));
            ui.end_row();
            // One box per line, as social-video captions are (#3237).
            if draft.backing > 0.0 {
                ui.label("");
                ui.horizontal(|ui| {
                    let mut per_line = draft.per_line.is_some();
                    if ui.checkbox(&mut per_line, "Box per line").on_hover_text("A box fitted to each line, joined into one shape").changed() {
                        draft.per_line = per_line.then(klipp_timeline::LineBoxes::default);
                    }
                    if let Some(lines) = draft.per_line.as_mut() {
                        ui.add(egui::DragValue::new(&mut lines.radius).range(0.0..=60.0).speed(0.2).prefix("radius ").suffix(" px")).on_hover_text("The corners' radius, inside and out; 0 is square");
                        ui.add(egui::DragValue::new(&mut lines.gap).range(0.0..=60.0).speed(0.2).prefix("gap ").suffix(" px")).on_hover_text("0 joins the boxes into one shape; more parts them");
                    }
                });
                ui.end_row();
            }
            ui.label("Shadow");
            ui.checkbox(&mut draft.shadow, "Soft drop shadow");
            ui.end_row();
            ui.label("Height");
            ui.add(egui::Slider::new(&mut draft.y, 0.05..=0.95).custom_formatter(|v, _| format!("{:.0}% down", v * 100.0)));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Save").on_hover_text("New captions on this track take this look").clicked() {
                cmds.push(AppCommand::SaveCaptionStyle { track_id, apply_to_all: false });
            }
            if ui.button("Save and Apply to All").on_hover_text("Also restyle every caption already on this track, as one undo step").clicked() {
                cmds.push(AppCommand::SaveCaptionStyle { track_id, apply_to_all: true });
            }
            if has_style && ui.button("Default Look").on_hover_text("New captions take the default look again; the captions there keep theirs").clicked() {
                cmds.push(AppCommand::ClearCaptionStyle { track_id });
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
    });
    if close {
        state.subtitle_editor.style_draft = None;
    }
}

fn render_row(
    ui: &mut egui::Ui,
    index: usize,
    row: &SubRow,
    fps: Fps,
    state: &mut AppState,
    cmds: &mut Vec<AppCommand>,
) {
    // A subtitle's in and out are positions on the sequence's clock.
    let tc = state.timecode_display();
    ui.horizontal(|ui| {
        // Row number
        ui.add_sized(
            [28.0, 20.0],
            egui::Label::new(
                egui::RichText::new(format!("{}", index + 1))
                    .monospace()
                    .size(11.0)
                    .color(egui::Color32::from_rgb(0x80, 0x80, 0x90)),
            ),
        );

        // Start timecode — click to seek, separate click on timecode text to edit
        let start_id = egui::Id::new(("subtitle_start_tc", row.clip_id));
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("Start")
                    .size(10.0)
                    .color(egui::Color32::from_rgb(0x70, 0x70, 0x80)),
            );
            let editing_timecode = &mut state.subtitle_editor.editing_timecode;
            let timecode_buf = &mut state.subtitle_editor.timecode_buf;
            if let Some(new_time) = timecode_edit(
                ui, start_id, row.timeline_in, tc, editing_timecode, timecode_buf,
            ) {
                // Clamp: start must be < end, and >= 0
                let min = Timecode::from_frames(0, fps.rate());
                let max_val = row.timeline_out.value.saturating_sub(1);
                let max = Timecode {
                    value: max_val,
                    rate: fps.rate(),
                };
                let clamped = if new_time < min { min }
                    else if new_time > max { max }
                    else { new_time };
                emit_trim_start(row, clamped, state.subtitle_editor.ripple, cmds);
            }
        });

        // End timecode
        let end_id = egui::Id::new(("subtitle_end_tc", row.clip_id));
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("End")
                    .size(10.0)
                    .color(egui::Color32::from_rgb(0x70, 0x70, 0x80)),
            );
            let editing_timecode = &mut state.subtitle_editor.editing_timecode;
            let timecode_buf = &mut state.subtitle_editor.timecode_buf;
            if let Some(new_time) = timecode_edit(
                ui, end_id, row.timeline_out, tc, editing_timecode, timecode_buf,
            ) {
                let min_val = row.timeline_in.value.saturating_add(1);
                let min = Timecode {
                    value: min_val,
                    rate: fps.rate(),
                };
                let clamped = if new_time < min { min } else { new_time };
                emit_trim_end(row, clamped, state.subtitle_editor.ripple, cmds);
            }
        });

        // Text column fills remaining width
        ui.vertical(|ui| {
            // If this row became active, load its text into buffer
            if state.subtitle_editor.editing_text != Some(row.clip_id) {
                // Not currently editing this row — show text and seed buffer on interaction below
            }

            let was_editing = state.subtitle_editor.editing_text == Some(row.clip_id);
            let mut text_to_show = if was_editing {
                state.subtitle_editor.text_buf.clone()
            } else {
                row.text.clone()
            };

            let avail = ui.available_width();
            // Its misspelt words underlined in the track's language, a
            // right-click on one offering what it might have been (#3236).
            let language = crate::spelling::language_of(state.sequence(), row.clip_id, None);
            let (out, respelt) = crate::spelling::show(
                ui,
                &state.spelling,
                &mut text_to_show,
                |e| e.desired_rows(2).desired_width(avail),
                &language,
                egui::TextStyle::Body.resolve(ui.style()),
                ui.visuals().text_color(),
                ui.visuals().error_fg_color,
                cmds,
            );
            let resp = out.response;
            if let Some(respelt) = respelt {
                // In the row being typed in, as typing is; in another, its
                // text set straight away.
                if was_editing {
                    text_to_show = respelt;
                } else {
                    cmds.push(AppCommand::UpdateTitleData {
                        clip_id: row.clip_id,
                        text: respelt,
                        font_size: row.font_size,
                        font_name: row.font_name.clone(),
                        font_style: row.font_style_u8,
                        color: row.color,
                    });
                }
            }

            // Row click = select + seek
            if resp.gained_focus() {
                // Flush any previous row's edit
                if state.subtitle_editor.editing_text.is_some()
                    && state.subtitle_editor.editing_text != Some(row.clip_id)
                {
                    let rows_snapshot: Vec<SubRow> = collect_subtitle_clips(state.sequence());
                    flush_text_edit(&mut state.subtitle_editor, &rows_snapshot, cmds);
                }
                // Start editing this row
                state.subtitle_editor.editing_text = Some(row.clip_id);
                state.subtitle_editor.text_buf = row.text.clone();
                cmds.push(AppCommand::SelectClip(Some(row.clip_id)));
                cmds.push(AppCommand::SeekTo(row.timeline_in));
            }

            if was_editing {
                // Commit text changes from the widget back to our buffer
                if text_to_show != state.subtitle_editor.text_buf {
                    state.subtitle_editor.text_buf = text_to_show.clone();
                    // Emit UpdateTitleData on every keystroke — undo coalescing
                    // handles the noise, matching viewer.rs behavior.
                    cmds.push(AppCommand::UpdateTitleData {
                        clip_id: row.clip_id,
                        text: text_to_show.clone(),
                        font_size: row.font_size,
                        font_name: row.font_name.clone(),
                        font_style: row.font_style_u8,
                        color: row.color,
                    });
                }
                if resp.lost_focus() {
                    state.subtitle_editor.editing_text = None;
                    state.subtitle_editor.text_buf.clear();
                }
            }

            // Character count + reading speed indicator
            let duration_secs = (row.timeline_out - row.timeline_in).to_secs_f64().max(0.001);
            let char_count = row.text.chars().count();
            let word_count = row.text.split_whitespace().count();
            let wps = word_count as f64 / duration_secs;
            let wps_color = reading_speed_color(wps);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let text = format!("{char_count} chars \u{00B7} {wps:.1} w/s");
                    let mut rt = egui::RichText::new(&text).size(10.0);
                    if let Some(c) = wps_color {
                        rt = rt.color(c);
                    } else {
                        rt = rt.color(egui::Color32::from_rgb(0x70, 0x70, 0x80));
                    }
                    ui.label(rt);
                });
            });
        });
    });
}

fn emit_trim_start(row: &SubRow, new_timeline_in: Timecode, ripple: bool, cmds: &mut Vec<AppCommand>) {
    // For title clips, source timing mirrors timeline timing (no media seek).
    // Adjust source_in by the same delta as timeline_in.
    let delta = new_timeline_in - row.timeline_in;
    let new_source_in = row.source_in + delta;
    cmds.push(AppCommand::TrimClip {
        clip_id: row.clip_id,
        old_source_in: row.source_in,
        old_source_out: row.source_out,
        old_timeline_in: row.timeline_in,
        old_timeline_out: row.timeline_out,
        new_source_in,
        new_source_out: row.source_out,
        new_timeline_in,
        new_timeline_out: row.timeline_out,
        ripple,
        linked: true,
    });
}

fn emit_trim_end(row: &SubRow, new_timeline_out: Timecode, ripple: bool, cmds: &mut Vec<AppCommand>) {
    let delta = new_timeline_out - row.timeline_out;
    let new_source_out = row.source_out + delta;
    cmds.push(AppCommand::TrimClip {
        clip_id: row.clip_id,
        old_source_in: row.source_in,
        old_source_out: row.source_out,
        old_timeline_in: row.timeline_in,
        old_timeline_out: row.timeline_out,
        new_source_in: row.source_in,
        new_source_out,
        new_timeline_in: row.timeline_in,
        new_timeline_out,
        ripple,
        linked: true,
    });
}

/// Flush the current text edit buffer as a final UpdateTitleData command.
fn flush_text_edit(
    ed: &mut crate::state::SubtitleEditorState,
    rows: &[SubRow],
    cmds: &mut Vec<AppCommand>,
) {
    if let Some(cid) = ed.editing_text {
        if let Some(row) = rows.iter().find(|r| r.clip_id == cid) {
            if row.text != ed.text_buf {
                cmds.push(AppCommand::UpdateTitleData {
                    clip_id: cid,
                    text: ed.text_buf.clone(),
                    font_size: row.font_size,
                    font_name: row.font_name.clone(),
                    font_style: row.font_style_u8,
                    color: row.color,
                });
            }
        }
    }
    ed.editing_text = None;
    ed.text_buf.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

    fn make_title_clip(text: &str, start_frames: i64, end_frames: i64) -> Clip {
        let ti = Timecode::from_frames(start_frames, 24);
        let to = Timecode::from_frames(end_frames, 24);
        Clip {
            id: ClipId::new(),
            kind: ClipKind::Title({
    let tb = klipp_timeline::TextBox {
        text: text.to_string(),
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
}),
            timeline_in: ti,
            timeline_out: to,
            source_in: Timecode::from_frames(0, 24),
            source_out: to - ti,
            ..Clip::gap(ti, to)
        }
    }

    fn make_seq_with_tracks() -> Sequence {
        let mut seq = Sequence::new_default();
        // tracks[0] = Video 1, tracks[1] = Audio 1
        // Add a Subtitle track and a Text track
        let mut sub_track = Track::new("Subs", TrackKind::Subtitle);
        sub_track.clips.push(make_title_clip("B-subtitle", 48, 72));
        sub_track.clips.push(make_title_clip("A-subtitle", 0, 24));
        seq.tracks.push(sub_track);

        let mut text_track = Track::new("Text", TrackKind::Text);
        text_track.clips.push(make_title_clip("C-text", 96, 120));
        seq.tracks.push(text_track);
        seq
    }

    #[test]
    fn collect_sorts_across_tracks() {
        let seq = make_seq_with_tracks();
        let rows = collect_subtitle_clips(&seq);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].text, "A-subtitle");
        assert_eq!(rows[1].text, "B-subtitle");
        assert_eq!(rows[2].text, "C-text");
    }

    #[test]
    fn collect_empty_for_sequence_with_no_subtitles() {
        let seq = Sequence::new_default();
        let rows = collect_subtitle_clips(&seq);
        assert!(rows.is_empty());
    }

    #[test]
    fn active_subtitle_playhead_inside() {
        let seq = make_seq_with_tracks();
        let rows = collect_subtitle_clips(&seq);
        // Playhead at frame 12 — inside "A-subtitle" (0-24)
        let playhead = Timecode::from_frames(12, 24);
        assert_eq!(active_subtitle_at(&rows, playhead), Some(0));
    }

    #[test]
    fn active_subtitle_playhead_at_start_is_inclusive() {
        let seq = make_seq_with_tracks();
        let rows = collect_subtitle_clips(&seq);
        let playhead = Timecode::from_frames(0, 24);
        assert_eq!(active_subtitle_at(&rows, playhead), Some(0));
    }

    #[test]
    fn active_subtitle_playhead_at_end_is_exclusive() {
        let seq = make_seq_with_tracks();
        let rows = collect_subtitle_clips(&seq);
        // Playhead exactly at frame 24 — outside "A-subtitle" (0..24), inside nothing
        let playhead = Timecode::from_frames(24, 24);
        assert_eq!(active_subtitle_at(&rows, playhead), None);
    }

    #[test]
    fn active_subtitle_playhead_in_gap() {
        let seq = make_seq_with_tracks();
        let rows = collect_subtitle_clips(&seq);
        // Playhead at frame 30 — between subtitle A (ends at 24) and B (starts at 48)
        let playhead = Timecode::from_frames(30, 24);
        assert_eq!(active_subtitle_at(&rows, playhead), None);
    }

    #[test]
    fn reading_speed_amber_threshold() {
        // 3.6 w/s → amber
        assert_eq!(
            reading_speed_color(3.6),
            Some(egui::Color32::from_rgb(0xe0, 0xa0, 0x30)),
        );
    }

    #[test]
    fn reading_speed_red_threshold() {
        // 5.5 w/s → red
        assert_eq!(
            reading_speed_color(5.5),
            Some(egui::Color32::from_rgb(0xff, 0x60, 0x60)),
        );
    }

    #[test]
    fn reading_speed_normal_returns_none() {
        // 2.0 w/s → no warning
        assert_eq!(reading_speed_color(2.0), None);
    }

    #[test]
    fn reading_speed_boundary_35_is_not_amber() {
        // Exactly 3.5 w/s → no warning (strict > 3.5)
        assert_eq!(reading_speed_color(3.5), None);
    }

    #[test]
    fn character_count_from_multi_word_text() {
        // 42 chars, 3 seconds → correct wps
        let text = "The quick brown fox jumps over the lazy d."; // 42 chars, 9 words
        let char_count = text.chars().count();
        let word_count = text.split_whitespace().count();
        let wps = word_count as f64 / 3.0;
        assert_eq!(char_count, 42);
        assert_eq!(word_count, 9);
        assert!((wps - 3.0).abs() < 0.001);
    }
}
