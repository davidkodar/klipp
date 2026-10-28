use klipp_timeline::GradeAccess;
use crate::widgets::inspector_group::section_card;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// Cache for LUT preview thumbnails, stored in egui context data.
#[derive(Clone, Default)]
struct LutPreviewCache(std::collections::HashMap<std::path::PathBuf, egui::TextureHandle>);

/// The Corrector's rows the Colour tab's Basic Correction draws (#2952),
/// the Colour page's own for them.
pub(crate) const BASIC_CORRECTION: [&str; 5] = ["exposure", "contrast", "saturation", "temperature", "tint"];

/// Show the Color tab content: the basic correction (`grade`, the
/// selected clip's grade as the Colour page reads it, taken by the app),
/// per-clip Masks (if any), color redirect, global LUT, and LUT presets.
/// `auto_colour` is set when Auto Colour is pressed.
pub(crate) fn show_color_and_luts(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    grade: Option<&crate::inspector::grade_view::GradeSnapshot>,
    auto_colour: &mut bool,
    cmds: &mut Vec<AppCommand>,
) {
    egui::ScrollArea::vertical().auto_shrink([false; 2]).id_salt("color_lut_scroll").show(ui, |ui| {
        // ── Basic correction (#2952) ──
        // The Colour page's Adjustments rows on the Corrector it edits, so
        // a value set here is the one the page shows; a clip never graded
        // draws the Corrector its first edit makes there.
        let clip = state.selected_clip.and_then(|id| state.sequence().all_clips().find(|c| c.id == id));
        if let Some(clip) = clip {
            let frame = crate::inspector::keyframe_selection::FrameSelection {
                time_secs: crate::inspector::keyframe_selection::clip_time(state, clip),
                tolerance: crate::inspector::keyframe_selection::frame_tolerance(state.sequence()),
                clip_start: clip.timeline_in,
            };
            section_card(ui, "BASIC CORRECTION", None, theme, |ui| {
                let start = cmds.len();
                if let Some((grade, node)) = grade.and_then(|g| Some((g, g.corrector()?))) {
                    for key in BASIC_CORRECTION {
                        grade.row(ui, theme, node, key, cmds, frame);
                    }
                }
                // A key the rows name at the playhead is the stored key in its frame.
                for cmd in &mut cmds[start..] {
                    crate::inspector::keyframe_selection::resolve_for_clip(clip, frame.tolerance, cmd);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    let w = ((ui.available_width() - 16.0) / 2.0).max(60.0);
                    if ui.add_sized([w, crate::theme::control::REGULAR], egui::Button::new("Auto Colour"))
                        .on_hover_text("A starting grade measured from the shot: exposure, white balance and contrast. One undo.")
                        .clicked()
                    {
                        *auto_colour = true;
                    }
                    if ui.add_sized([w, crate::theme::control::REGULAR], egui::Button::new("Open in Colour page"))
                        .on_hover_text(crate::keybindings::tip(ui.ctx(), "Grade this clip on the Colour page", "toggle_color_panel"))
                        .clicked()
                    {
                        cmds.push(AppCommand::ToggleColorPanel);
                    }
                });
            });
        }

        // ── Masks section ──
        // Lets Edit-workspace users edit masks without jumping to
        // the Color workspace. Clicking a mask on the Program
        // viewer routes inspector focus here via
        // FocusMaskInspector. Render the full mask editor against a
        // local clone of the stack — the helper emits SetNodeMasks
        // on any change so the persistent path matches every other
        // mask-edit site.
        if let Some(clip_id) = state.selected_clip {
            let mask_entry: Option<(klipp_nodes::NodeId, klipp_core::MaskStack, f64, Vec<usize>, klipp_core::Timecode)> = state
                .sequences
                .get(state.active_sequence_idx)
                .and_then(|seq| seq.tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .and_then(|c| {
                        let clip_time = crate::inspector::keyframe_selection::clip_time(state, c);
                        let mask_node = c.grade().nodes.iter()
                            .find(|n| n.kind == klipp_nodes::NodeKind::Mask);
                        mask_node.map(|n| {
                            // Build the list of mask indices that have
                            // a tracker bound to them, so the Re-track /
                            // Untrack UI lights up for already-tracked
                            // masks.
                            let nid = n.id;
                            let tracked: Vec<usize> = c.track_points.iter()
                                .filter_map(|tp| tp.linked_mask
                                    .filter(|(tn, _)| *tn == nid)
                                    .map(|(_, mi)| mi))
                                .collect();
                            (nid, n.params.masks.clone().unwrap_or_default(), clip_time, tracked, c.timeline_in)
                        })
                    }));
            if let Some((node_id, mut masks, clip_time_secs, tracked, clip_start)) = mask_entry {
                let frame = crate::inspector::keyframe_selection::FrameSelection {
                    time_secs: clip_time_secs,
                    tolerance: crate::inspector::keyframe_selection::frame_tolerance(state.sequence()),
                    clip_start,
                };
                section_card(ui, "MASKS", None, theme, |ui| {
                    crate::panels::node_inspector::NodeInspectorPanel::render_masks_body(
                        ui, &mut masks, node_id, clip_id, cmds, theme, frame, &tracked,
                    );
                });
            }
        }

        // Input Color Space selector
        if let Some(clip_id) = state.selected_clip {
            let (current_log, detected) = clip_input_colour_space(state, clip_id);
            section_card(ui, "INPUT COLOUR SPACE", None, theme, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                input_colour_space_combo(ui, "color_tab_log_format", clip_id, current_log, detected, cmds);
            });
            });
        }

        // Bake Grade — export the active clip's full grade chain to a .cube file.
        // Spatial nodes (qualifier / masks) are auto-skipped by the bake
        // function; the LUT captures the per-pixel colour mapping only.
        section_card(ui, "BAKE GRADE", None, theme, |ui| {
        ui.horizontal(|ui| { ui.add_space(12.0); ui.vertical(|ui| {
        if let Some(clip_id) = state.selected_clip {
            let mut size = state.bake_lut_size;
            let size_label = match size {
                17 => "17 (preview)".to_string(),
                33 => "33 (standard)".to_string(),
                65 => "65 (HQ)".to_string(),
                n => format!("{n}"),
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Size").size(10.0).color(theme.text_dim));
                egui::ComboBox::from_id_salt("bake_lut_size")
                    .selected_text(size_label)
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut size, 17, "17 (preview)");
                        ui.selectable_value(&mut size, 33, "33 (standard)");
                        ui.selectable_value(&mut size, 65, "65 (HQ)");
                    });
            });
            // Persist size selection back into AppState via a setter command —
            // a direct mutation here would fight the &AppState borrow.
            if size != state.bake_lut_size {
                cmds.push(AppCommand::SetBakeLutSize(size));
            }
            ui.add_space(4.0);
            if ui.button(
                egui::RichText::new("Bake Grade as .cube...").size(11.0)
            ).clicked() {
                crate::file_dialog::spawn(move || {
                    let Some(path) = rfd::FileDialog::new()
                        .set_title("Save baked grade as .cube")
                        .add_filter("3D LUT (.cube)", &["cube"])
                        .set_file_name(format!("klipp_grade_{size}.cube"))
                        .save_file()
                    else {
                        return Vec::new();
                    };
                    let output_path = if path.extension().is_some() {
                        path
                    } else {
                        path.with_extension("cube")
                    };
                    vec![AppCommand::BakeGradeToLut {
                        clip_id, output_path, lut_size: size,
                    }]
                });
            }
            ui.add_space(2.0);
            ui.label(egui::RichText::new(
                "Captures the current clip's grade chain. Qualifier / mask / matte nodes are skipped."
            ).size(10.0).color(theme.text_dim));
        } else {
            ui.label(egui::RichText::new("Select a clip to bake its grade.")
                .size(10.0).color(theme.text_dim));
        }
        }); });
        });

        // Global LUT
        section_card(ui, "GLOBAL LUT", None, theme, |ui| {
        ui.horizontal(|ui| { ui.add_space(12.0); ui.vertical(|ui| {
        if let Some(path) = &state.global_lut_path {
            let filename = path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown".into());
            // Enable/disable + filename + clear on one row
            ui.horizontal(|ui| {
                let mut enabled = state.global_lut_enabled;
                if ui.checkbox(&mut enabled, "").changed() {
                    cmds.push(AppCommand::SetGlobalLutEnabled(enabled));
                }
                let label_color = if state.global_lut_enabled {
                    theme.marker_success
                } else {
                    theme.text_dim
                };
                ui.colored_label(label_color, &filename);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Clear").clicked() {
                        cmds.push(AppCommand::ClearGlobalLut);
                    }
                });
            });
            ui.add_space(4.0);
            let mut opacity = state.global_lut_opacity;
            ui.label("Opacity");
            if ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0)
                .step_by(0.01).min_decimals(2)
            ).changed() {
                cmds.push(AppCommand::SetGlobalLutOpacity(opacity));
            }
            ui.add_space(4.0);
            if ui.button("Load .cube LUT...").clicked() {
                crate::file_dialog::spawn(pick_global_lut);
            }
        } else {
            ui.label("No LUT loaded");
            ui.add_space(4.0);
            if ui.button("Load .cube LUT...").clicked() {
                crate::file_dialog::spawn(pick_global_lut);
            }
        }

        }); });
        });

        // Bundled LUT Presets
        section_card(ui, "LUT PRESETS", None, theme, |ui| {
        let luts_dir = find_bundled_luts_dir();
        if let Some(ref base) = luts_dir {
            let categories = [
                ("film-bw", "Film B&W"),
                ("film-slide", "Film Slide"),
                ("film-negative", "Film Negative"),
                ("film-print", "Film Print"),
                ("film-instant", "Film Instant"),
                ("fuji-xtrans", "Fuji X-Trans"),
                // A camera's log or flat profile to normal: on normal footage they
                // lift its blacks (#2769). The folder keeps its old name, which
                // saved grades point into.
                ("cinematic", "Camera conversion (log footage)"),
            ];
            for (dir_name, label) in &categories {
                let cat_dir = base.join(dir_name);
                if !cat_dir.exists() { continue; }
                let id = egui::Id::new(format!("lut_preset_{dir_name}"));
                egui::CollapsingHeader::new(*label)
                    .id_salt(id)
                    .show(ui, |ui| {
                        let mut entries: Vec<_> = std::fs::read_dir(&cat_dir)
                            .into_iter().flatten()
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().extension()
                                .is_some_and(|ext| ext == "cube" || ext == "3dl" || ext == "csp"))
                            .collect();
                        entries.sort_by_key(|e| e.file_name());
                        let ctx = ui.ctx().clone();
                        for entry in entries {
                            let path = entry.path();
                            let name = path.file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let is_active = state.global_lut_path.as_ref() == Some(&path);
                            let thumb_size = 32.0;
                            let row_h = thumb_size + 4.0;
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_h),
                                egui::Sense::click(),
                            );
                            // Background: subtle highlight for active, hover for others
                            if is_active {
                                ui.painter().rect_filled(rect, 2.0, theme.accent_subtle);
                            } else if resp.hovered() {
                                ui.painter().rect_filled(rect, 2.0, theme.bg_hover);
                            }
                            // LUT preview thumbnail (lazy-loaded only when visible)
                            let thumb_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.min.x + 4.0, rect.min.y + 2.0),
                                egui::vec2(thumb_size, thumb_size),
                            );
                            if ui.is_rect_visible(thumb_rect) {
                                if let Some(tex) = get_lut_preview(&ctx, &path) {
                                    ui.painter().image(
                                        tex.id(),
                                        thumb_rect,
                                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                        egui::Color32::WHITE,
                                    );
                                    // Subtle border around thumbnail
                                    ui.painter().rect_stroke(thumb_rect, 1.0, egui::Stroke::new(0.5_f32, theme.border));
                                }
                            }
                            // Text: white for active, dim for others
                            let text_color = if is_active {
                                theme.text_primary
                            } else {
                                theme.text_dim
                            };
                            let text_x = rect.min.x + thumb_size + 12.0;
                            // Active indicator dot
                            if is_active {
                                ui.painter().circle_filled(
                                    egui::pos2(text_x, rect.center().y),
                                    3.0,
                                    theme.accent,
                                );
                            }
                            ui.painter().text(
                                egui::pos2(text_x + if is_active { 8.0 } else { 0.0 }, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &name,
                                egui::FontId::proportional(11.0),
                                text_color,
                            );
                            if resp.clicked() {
                                if is_active {
                                    cmds.push(AppCommand::ClearGlobalLut);
                                } else {
                                    cmds.push(AppCommand::LoadGlobalLut { path });
                                }
                            }
                        }
                    });
            }
        }
        });
    });
}

/// Get or create a LUT preview thumbnail texture for the given LUT file path.
/// Returns None if the LUT cannot be loaded. Uses lazy loading (only generates
/// preview when first requested). Cache is stored in egui context data so it
/// is shared across all inspector instances.
fn get_lut_preview(
    ctx: &egui::Context,
    path: &std::path::Path,
) -> Option<egui::TextureHandle> {
    let cache_id = egui::Id::new("lut_preview_cache");
    // Check if already cached
    let existing: Option<egui::TextureHandle> = ctx.data_mut(|d| {
        d.get_temp::<LutPreviewCache>(cache_id)
            .and_then(|c| c.0.get(path).cloned())
    });
    if let Some(handle) = existing {
        return Some(handle);
    }
    // Load LUT and generate preview
    let lut = klipp_color::load_lut(path).ok()?;
    let rgba = klipp_color::generate_lut_preview(&lut);
    let image = egui::ColorImage::from_rgba_unmultiplied([64, 64], &rgba);
    let name = path.file_stem()
        .map(|s| format!("lut_preview_{}", s.to_string_lossy()))
        .unwrap_or_else(|| "lut_preview".into());
    let handle = crate::app::alloc_texture_logged(ctx, &name, image, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| {
        let mut cache = d.get_temp::<LutPreviewCache>(cache_id)
            .unwrap_or_default();
        cache.0.insert(path.to_path_buf(), handle.clone());
        d.insert_temp(cache_id, cache);
    });
    Some(handle)
}

/// The bundled LUTs directory: next to the executable when installed,
/// in the source tree in development. The lookup projects share.
fn find_bundled_luts_dir() -> Option<std::path::PathBuf> {
    klipp_core::assets::bundled_luts_dir().map(|p| p.to_path_buf())
}


/// The input colour space a clip's file is decoded from, and whether
/// detection chose it rather than the user.
///
/// Read from the media item, which is what the decoder converts from; the
/// clip's InputTransform node only carries a copy made when the clip was
/// added. Falls back to that copy for a clip with no media item.
pub(crate) fn clip_input_colour_space(state: &AppState, clip_id: klipp_core::ClipId) -> (klipp_core::LogFormat, bool) {
    let clip = state.sequences.get(state.active_sequence_idx)
        .and_then(|seq| seq.all_clips().find(|c| c.id == clip_id));
    let media = clip.and_then(|c| c.media_id()).and_then(|id| state.media_pool.get(&id));
    if let Some(m) = media {
        let format = m.effective_log_format();
        let detected = !m.log_format_user_set && format != klipp_core::LogFormat::Srgb;
        return (format, detected);
    }
    let node_format = clip
        .and_then(|c| c.grade().nodes.iter().find_map(|n| n.params.log_format))
        .unwrap_or_default();
    (node_format, false)
}

/// The input colour space picker, shared by the Edit inspector's COLOUR
/// tab and the Colour page's InputTransform node. A value detection chose
/// says so, so a wrong guess can be told from a choice.
/// The input formats a clip can be given, as the inspector and Colour
/// Management list them.
pub(crate) const INPUT_FORMATS: &[(klipp_core::LogFormat, &str)] = {
    use klipp_core::LogFormat as LF;
    &[
        (LF::Srgb, "Rec.709 / sRGB (no conversion)"), (LF::Linear, "Linear"),
        (LF::LogC3, "ARRI LogC3"), (LF::LogC4, "ARRI LogC4"),
        (LF::SLog2, "Sony S-Log2"), (LF::SLog3, "Sony S-Log3"),
        (LF::CanonLog, "Canon Log"), (LF::CanonLog3, "Canon Log 3"),
        (LF::VLog, "Panasonic V-Log"), (LF::NLog, "Nikon N-Log"),
        (LF::FLog, "Fuji F-Log"), (LF::DJIDLog, "DJI D-Log"),
        (LF::GoProFlat, "GoPro ProTune Flat"),
        (LF::AppleLog, "Apple Log"), (LF::LeicaLLog, "Leica L-Log"),
        (LF::LeicaLLogBt709, "Leica L-Log, SL (Typ 601)"),
        (LF::RedLog3G10, "RED Log3G10"), (LF::BmdFilmGen5, "BMD Film Gen 5"),
        (LF::HLG, "HLG (BT.2100)"), (LF::PQ, "PQ (ST 2084)"),
    ]
};

pub(crate) fn input_colour_space_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    clip_id: klipp_core::ClipId,
    current: klipp_core::LogFormat,
    detected: bool,
    cmds: &mut Vec<AppCommand>,
) {
    let formats = INPUT_FORMATS;
    let label = formats.iter()
        .find(|(f, _)| *f == current)
        .map(|(_, l)| *l)
        .unwrap_or(formats[0].1);
    let selected_text = if detected { format!("{label} (detected)") } else { label.to_string() };
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .width(ui.available_width() - 12.0)
        .show_ui(ui, |ui| {
            for &(fmt, label) in formats {
                if ui.selectable_label(current == fmt, label).clicked() {
                    cmds.push(AppCommand::SetClipLogFormat { clip_id, log_format: fmt });
                }
            }
        });
}

/// The picker behind both Load LUT buttons. Shaped for `file_dialog::spawn`.
pub(crate) fn pick_global_lut() -> Vec<AppCommand> {
    rfd::FileDialog::new()
        .set_title("Load .cube LUT")
        .add_filter("LUT files", &["cube", "3dl", "csp"])
        .pick_file()
        .map(|path| vec![AppCommand::LoadGlobalLut { path }])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{LogFormat, RationalTime};

    fn state_with_clip(edit: impl FnOnce(&mut klipp_media::MediaItem)) -> (AppState, klipp_core::ClipId) {
        let mut state = AppState::default();
        let mut item = klipp_media::MediaItem::synthetic_for_test(std::path::PathBuf::from("/tmp/a6700.mp4"), None);
        edit(&mut item);
        let media_id = item.id;
        state.media_pool.add(item);
        let t = |v| RationalTime { value: v, rate: 24 };
        let clip = klipp_timeline::Clip::media(media_id, t(0), t(24), t(0), t(24));
        let clip_id = clip.id;
        state.sequences[0].tracks[0].clips.push(clip);
        (state, clip_id)
    }

    #[test]
    fn a_detected_log_format_says_so_and_a_choice_does_not() {
        let (state, clip) = state_with_clip(|m| m.detected_log_format = LogFormat::SLog3);
        assert_eq!(clip_input_colour_space(&state, clip), (LogFormat::SLog3, true));

        let (state, clip) = state_with_clip(|m| {
            m.detected_log_format = LogFormat::SLog3;
            m.log_format = LogFormat::SLog3;
            m.log_format_user_set = true;
        });
        assert_eq!(clip_input_colour_space(&state, clip), (LogFormat::SLog3, false));

        let (state, clip) = state_with_clip(|m| {
            m.detected_log_format = LogFormat::SLog3;
            m.log_format = LogFormat::Srgb;
            m.log_format_user_set = true;
        });
        assert_eq!(clip_input_colour_space(&state, clip), (LogFormat::Srgb, false));
    }
}
