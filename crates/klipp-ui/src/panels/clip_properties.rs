//! Multi-tab modal dialog showing detailed properties of a media clip.
//!
//! Feature #47. Opens from:
//! - Timeline clip context menu → "Properties…"
//! - Media-bin item context menu → "Properties…"
//!
//! The dialog is deliberately read-only for most fields (inspector already
//! handles deep edits of rating/tags/notes). For MVP we lift the existing
//! `inspector::info_tab` data into a focused modal with tab navigation.
//! Streams and Metadata tabs use the data already on `MediaItem` — full
//! per-stream enumeration requires probe extension and is deferred.
//!
//! Signature contract: `show_clip_properties_dialog(ctx, state) -> Vec<AppCommand>`.
//! Closes itself by flipping `state.show_clip_properties = false`.

use crate::state::{AppState, ClipPropertiesTab};
use klipp_core::AppCommand;

pub fn show_clip_properties_dialog(
    ctx: &egui::Context,
    state: &mut AppState,
) -> Vec<AppCommand> {
    let mut cmds: Vec<AppCommand> = Vec::new();
    if !state.show_clip_properties {
        return cmds;
    }
    let Some(media_id) = state.clip_properties_media_id else {
        state.show_clip_properties = false;
        return cmds;
    };
    // Snapshot the MediaItem so the dialog body doesn't hold a borrow of
    // state.media_pool while we also need to mutate `state.clip_properties_tab`,
    // walk `state.sequences`, etc. Cheap — MediaItem is ~200 bytes of mostly
    // Strings and numbers.
    let Some(media) = state.media_pool.get(&media_id).cloned() else {
        // Item removed under our feet — close cleanly.
        state.show_clip_properties = false;
        return cmds;
    };

    let mut open = true;
    egui::Window::new("Clip Properties")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(640.0)
        .default_height(500.0)
        .min_width(520.0)
        .min_height(360.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            // ── Header: colour-label stripe + filename ──
            ui.horizontal(|ui| {
                if let Some((r, g, b)) = media.color_label.rgb() {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(3.0, 18.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(
                        rect,
                        1.0,
                        egui::Color32::from_rgb(r, g, b),
                    );
                    ui.add_space(4.0);
                }
                ui.label(egui::RichText::new(&media.filename).strong().size(13.0));
                if media.offline {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("OFFLINE")
                            .color(egui::Color32::from_rgb(0xd0, 0x40, 0x40))
                            .size(10.0)
                            .strong(),
                    );
                }
            });
            ui.add_space(6.0);

            // ── Tab bar ──
            ui.horizontal(|ui| {
                for tab in ClipPropertiesTab::ALL.iter() {
                    let is_active = state.clip_properties_tab == *tab;
                    if ui.selectable_label(is_active, tab.label()).clicked() {
                        state.clip_properties_tab = *tab;
                    }
                }
            });
            ui.separator();

            // ── Tab body ──
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .show(ui, |ui| match state.clip_properties_tab {
                    ClipPropertiesTab::Info => render_info_tab(ui, &media),
                    ClipPropertiesTab::File => render_file_tab(ui, &media),
                    ClipPropertiesTab::Tags => {
                        render_tags_tab(ui, &media, &mut cmds);
                    }
                    ClipPropertiesTab::Markers => {
                        render_markers_tab(ui, &media, &state.sequences);
                    }
                    ClipPropertiesTab::Usage => {
                        render_usage_tab(ui, &media, &state.sequences);
                    }
                    ClipPropertiesTab::Streams => render_streams_tab(ui, &media),
                    ClipPropertiesTab::Metadata => render_metadata_tab(ui, &media),
                });
        });
    if !open {
        state.show_clip_properties = false;
    }
    cmds
}

// ── Tab renderers ─────────────────────────────────────────────────────

fn render_info_tab(ui: &mut egui::Ui, media: &klipp_media::MediaItem) {
    egui::Grid::new("clip_props_info")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            kv(ui, "Resolution", &format!("{} \u{00D7} {}", media.resolution.width, media.resolution.height));
            if !media.is_still {
                let fps = media.fps.as_f64();
                let fps_text = if (fps - fps.round()).abs() < 0.01 {
                    format!("{} fps", fps.round() as u32)
                } else {
                    format!("{:.3} fps", fps)
                };
                kv(ui, "Frame rate", &fps_text);
            }
            kv(ui, "Duration", &klipp_core::TimecodeDisplay::plain(media.fps).duration(media.duration));
            if !media.video_codec.is_empty() {
                kv(ui, "Video codec", &media.video_codec.to_uppercase());
            }
            if media.has_audio {
                let ch = media.audio_channels.unwrap_or(2);
                let sr = media.sample_rate.unwrap_or(48000);
                let ch_label = match ch {
                    1 => "Mono", 2 => "Stereo", 6 => "5.1", 8 => "7.1",
                    _ => "",
                };
                let audio_text = if ch_label.is_empty() {
                    format!("{}  \u{00B7} {} ch \u{00B7} {} Hz", media.audio_codec.to_uppercase(), ch, sr)
                } else {
                    format!("{}  \u{00B7} {} \u{00B7} {} Hz", media.audio_codec.to_uppercase(), ch_label, sr)
                };
                kv(ui, "Audio", &audio_text);
            }
            if let Some(bd) = media.bit_depth {
                kv(ui, "Bit depth", &format!("{bd}-bit"));
            }
            if let Some(ref pf) = media.pixel_format {
                kv(ui, "Pixel format", pf);
            }
            // Colour space badge
            use klipp_media::ColourSpace;
            let cs_text = match media.colour_space {
                ColourSpace::Sdr => "SDR (BT.709)",
                ColourSpace::Hdr10 => "HDR10 (BT.2020 / PQ)",
                ColourSpace::Hlg => "HLG (BT.2020)",
                ColourSpace::Unknown => "Unknown",
            };
            kv(ui, "Colour space", cs_text);
            if media.log_format_user_set {
                kv(ui, "Log format", &format!("{:?}", media.log_format));
            } else if media.detected_log_format != klipp_core::LogFormat::Srgb {
                kv(ui, "Detected log", &format!("{:?}", media.detected_log_format));
            }
            if let Some(vbr) = media.video_bitrate_kbps {
                let text = if vbr >= 1000 {
                    format!("{:.1} Mbps", vbr as f64 / 1000.0)
                } else {
                    format!("{vbr} kbps")
                };
                kv(ui, "Video bitrate", &text);
            }
            if let Some(abr) = media.audio_bitrate_kbps {
                kv(ui, "Audio bitrate", &format!("{abr} kbps"));
            }
            if media.source_rotation != 0 {
                kv(ui, "Source rotation", &format!("{}°", media.source_rotation));
            }
        });
}

fn render_file_tab(ui: &mut egui::Ui, media: &klipp_media::MediaItem) {
    egui::Grid::new("clip_props_file")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            kv(ui, "Filename", &media.filename);
            // Full path — selectable so users can copy it out.
            ui.label("Path");
            ui.add(
                egui::TextEdit::singleline(&mut media.path.to_string_lossy().to_string())
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
            ui.end_row();

            kv(ui, "Size", &format_size(media.file_size_bytes));
            if let Some(mtime) = media.file_modified_at.filter(|&t| t > 0) {
                kv(ui, "Modified", &format_epoch_date(mtime));
            }
            if let Some(created) = media.media_created_at.filter(|&t| t > 0) {
                kv(ui, "Media created", &format_epoch_date(created));
            }
            if let Some(fp) = media.fingerprint {
                // Show the first 16 hex chars — full 64-bit is too long for
                // the row, short prefix is enough to disambiguate visually.
                kv(ui, "Fingerprint", &format!("{fp:016x}"));
            }
            if media.camera_make.is_some() || media.camera_model.is_some() {
                let make = media.camera_make.as_deref().unwrap_or("");
                let model = media.camera_model.as_deref().unwrap_or("");
                let text = if !make.is_empty() && !model.is_empty() {
                    format!("{make} {model}")
                } else {
                    format!("{make}{model}")
                };
                kv(ui, "Camera", &text);
            }
            if let Some(ref enc) = media.encoder {
                kv(ui, "Encoder", enc);
            }
            kv(ui, "Proxy", if media.proxy_ready { "Ready" } else { "Not generated" });
            if let Some(ref pp) = media.proxy_path {
                kv(ui, "Proxy path", &pp.to_string_lossy());
            }
            if let Some(ref sp) = media.skim_file {
                kv(ui, "Skim transcode", &sp.to_string_lossy());
            }
            kv(ui, "Online", if media.offline { "Offline (missing)" } else { "Online" });
        });
}

fn render_tags_tab(
    ui: &mut egui::Ui,
    media: &klipp_media::MediaItem,
    _cmds: &mut Vec<AppCommand>,
) {
    // Read-only summary view for MVP — the inspector Info tab remains the
    // edit surface so we don't duplicate the add/remove chip widgets
    // (which need mutable tag_input state on the panel).
    ui.label(egui::RichText::new("Rating").strong());
    if media.rating == 0 {
        ui.label(egui::RichText::new("No rating").italics().color(egui::Color32::from_gray(140)));
    } else {
        ui.label("\u{2605}".repeat(media.rating as usize));
    }
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Tags").strong());
    if media.tags.is_empty() {
        ui.label(egui::RichText::new("No tags").italics().color(egui::Color32::from_gray(140)));
    } else {
        ui.horizontal_wrapped(|ui| {
            for tag in &media.tags {
                ui.label(
                    egui::RichText::new(tag)
                        .background_color(egui::Color32::from_gray(45))
                        .color(egui::Color32::WHITE)
                        .size(11.0),
                );
            }
        });
    }
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Notes").strong());
    if media.user_notes.is_empty() {
        ui.label(egui::RichText::new("No notes").italics().color(egui::Color32::from_gray(140)));
    } else {
        ui.label(&media.user_notes);
    }

    ui.add_space(12.0);
    ui.label(
        egui::RichText::new("Edit tags, rating, and notes in the Inspector Info tab.")
            .italics()
            .size(10.0)
            .color(egui::Color32::from_gray(120)),
    );
}

fn render_markers_tab(
    ui: &mut egui::Ui,
    media: &klipp_media::MediaItem,
    sequences: &[klipp_timeline::Sequence],
) {
    // Clip-level markers are per-clip-instance — we walk every clip that
    // references this media across every sequence and collect their
    // `clip_markers` together with the owning sequence/track names.
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for seq in sequences {
        for track in &seq.tracks {
            for clip in &track.clips {
                if clip.media_id() != Some(media.id) {
                    continue;
                }
                for m in &clip.clip_markers {
                    rows.push((
                        seq.name.clone(),
                        if track.name.is_empty() {
                            format!("{:?}", track.kind)
                        } else {
                            track.name.clone()
                        },
                        format!("{:.2}s   {}", m.time_secs, m.name),
                    ));
                }
            }
        }
    }

    if rows.is_empty() {
        ui.label(
            egui::RichText::new("No clip markers on any instance of this media.")
                .italics()
                .color(egui::Color32::from_gray(140)),
        );
        return;
    }

    egui::Grid::new("clip_props_markers")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Sequence").strong());
            ui.label(egui::RichText::new("Track").strong());
            ui.label(egui::RichText::new("Marker").strong());
            ui.end_row();
            for (seq, track, marker) in &rows {
                ui.label(seq);
                ui.label(track);
                ui.label(egui::RichText::new(marker).family(egui::FontFamily::Monospace));
                ui.end_row();
            }
        });
}

fn render_usage_tab(
    ui: &mut egui::Ui,
    media: &klipp_media::MediaItem,
    sequences: &[klipp_timeline::Sequence],
) {
    // Walk every sequence (not just the active one) — the user might be
    // looking at a clip that's used in a different sequence than the one
    // currently open.
    let mut rows: Vec<(String, String, String)> = Vec::new();
    let mut total_used_secs = 0.0_f64;
    for seq in sequences {
        for track in &seq.tracks {
            for clip in &track.clips {
                if clip.media_id() != Some(media.id) {
                    continue;
                }
                // This tab walks every sequence, not just the active
                // one, so a position is shown on the clock of the
                // sequence it is in — a clip cut into a 29.97 drop-frame
                // timeline and a 25 sequence reads differently in each.
                let tc = seq.timecode_display();
                let in_tc = tc.position(clip.timeline_in);
                let out_tc = tc.position(clip.timeline_out);
                rows.push((
                    seq.name.clone(),
                    if track.name.is_empty() {
                        format!("{:?}", track.kind)
                    } else {
                        track.name.clone()
                    },
                    format!("{in_tc} \u{2013} {out_tc}"),
                ));
                total_used_secs += clip.duration().to_secs_f64();
            }
        }
    }

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Total uses:").strong());
        ui.label(format!("{}", rows.len()));
        ui.add_space(16.0);
        ui.label(egui::RichText::new("Total time:").strong());
        ui.label(format!("{:.2}s", total_used_secs));
    });
    ui.add_space(8.0);

    if rows.is_empty() {
        ui.label(
            egui::RichText::new("Not used on any timeline.")
                .italics()
                .color(egui::Color32::from_gray(140)),
        );
        return;
    }

    egui::Grid::new("clip_props_usage")
        .num_columns(3)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Sequence").strong());
            ui.label(egui::RichText::new("Track").strong());
            ui.label(egui::RichText::new("Range").strong());
            ui.end_row();
            for (seq, track, range) in &rows {
                ui.label(seq);
                ui.label(track);
                ui.label(egui::RichText::new(range).family(egui::FontFamily::Monospace));
                ui.end_row();
            }
        });
}

fn render_streams_tab(ui: &mut egui::Ui, media: &klipp_media::MediaItem) {
    // MVP: reconstruct per-stream lines from the summary fields we already
    // carry on MediaItem. Full per-stream enumeration (stream index,
    // language tags, subtitle streams) needs a probe extension and is
    // explicitly deferred.
    ui.label(egui::RichText::new("Video stream").strong());
    if media.video_codec.is_empty() {
        ui.label(egui::RichText::new("  None").italics().color(egui::Color32::from_gray(140)));
    } else {
        let fps = media.fps.as_f64();
        let mut parts = vec![
            media.video_codec.to_uppercase(),
            format!("{}\u{00D7}{}", media.resolution.width, media.resolution.height),
        ];
        if !media.is_still {
            parts.push(format!("{:.2} fps", fps));
        }
        if let Some(bd) = media.bit_depth {
            parts.push(format!("{bd}-bit"));
        }
        if let Some(ref pf) = media.pixel_format {
            parts.push(pf.clone());
        }
        if let Some(vbr) = media.video_bitrate_kbps {
            let text = if vbr >= 1000 {
                format!("{:.1} Mbps", vbr as f64 / 1000.0)
            } else {
                format!("{vbr} kbps")
            };
            parts.push(text);
        }
        ui.label(format!("  {}", parts.join(" \u{00B7} ")));
    }
    ui.add_space(8.0);

    ui.label(egui::RichText::new("Audio stream").strong());
    if !media.has_audio {
        ui.label(egui::RichText::new("  None").italics().color(egui::Color32::from_gray(140)));
    } else {
        let mut parts = vec![media.audio_codec.to_uppercase()];
        if let Some(ch) = media.audio_channels {
            let ch_label = match ch {
                1 => "Mono".to_string(),
                2 => "Stereo".to_string(),
                n => format!("{n} ch"),
            };
            parts.push(ch_label);
        }
        if let Some(sr) = media.sample_rate {
            parts.push(format!("{sr} Hz"));
        }
        if let Some(abr) = media.audio_bitrate_kbps {
            parts.push(format!("{abr} kbps"));
        }
        ui.label(format!("  {}", parts.join(" \u{00B7} ")));
    }
    ui.add_space(12.0);

    ui.label(
        egui::RichText::new(
            "Full per-stream enumeration (language tags, subtitle streams, \
             per-stream timebase) requires probe extension — deferred.",
        )
        .italics()
        .size(10.0)
        .color(egui::Color32::from_gray(120)),
    );
}

fn render_metadata_tab(ui: &mut egui::Ui, media: &klipp_media::MediaItem) {
    // MVP: surface the metadata-like fields we already capture during probe.
    // A full `format_ctx.metadata()` → HashMap dump is deferred.
    let mut any = false;
    egui::Grid::new("clip_props_metadata")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            if let Some(ref make) = media.camera_make {
                kv(ui, "Camera make", make);
                any = true;
            }
            if let Some(ref model) = media.camera_model {
                kv(ui, "Camera model", model);
                any = true;
            }
            if let Some(ref enc) = media.encoder {
                kv(ui, "Software / encoder", enc);
                any = true;
            }
            if let Some(claimed) = media.truncated_from_secs {
                kv(ui, "Cut short", &format!("{} of {} the header claims", klipp_media::describe_secs(media.duration.to_secs_f64()), klipp_media::describe_secs(claimed)));
                any = true;
            }
            let shot = &media.shot;
            let shot_rows: [(&str, Option<String>); 8] = [
                ("Lens", shot.lens.clone()),
                ("Exposure", shot.exposure_summary()),
                ("White balance", shot.white_balance.clone()),
                ("Gamma", shot.gamma.clone()),
                ("Colour gamut", shot.colour_gamut.clone()),
                ("Start timecode", shot.start_timecode.clone()),
                ("Reel", shot.reel_name.clone()),
                ("Exposure from", shot.exposure_source.clone()),
            ];
            for (label, value) in shot_rows {
                if let Some(v) = value {
                    kv(ui, label, &v);
                    any = true;
                }
            }
            if let Some(ref pri) = media.color_primaries {
                kv(ui, "Colour primaries", pri);
                any = true;
            }
            if let Some(ref trc) = media.color_transfer {
                kv(ui, "Colour transfer", trc);
                any = true;
            }
            if media.detected_log_format != klipp_core::LogFormat::Srgb {
                kv(ui, "Detected log", &format!("{:?}", media.detected_log_format));
                any = true;
            }
            if let Some(created) = media.media_created_at.filter(|&t| t > 0) {
                kv(ui, "Creation time", &format_epoch_date(created));
                any = true;
            }
        });
    if !any {
        ui.label(
            egui::RichText::new("No extended metadata captured for this item.")
                .italics()
                .color(egui::Color32::from_gray(140)),
        );
    }
    ui.add_space(12.0);
    ui.label(
        egui::RichText::new(
            "Full ffprobe key/value dump (artist, title, comment, custom tags, \
             per-stream metadata) requires probe extension — deferred.",
        )
        .italics()
        .size(10.0)
        .color(egui::Color32::from_gray(120)),
    );
}

// ── Layout helpers ────────────────────────────────────────────────────

fn kv(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).color(egui::Color32::from_gray(150)));
    ui.label(egui::RichText::new(value).strong());
    ui.end_row();
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn format_epoch_date(epoch_secs: u64) -> String {
    // Lightweight mirror of `inspector::info_tab::format_epoch_date`.
    // Avoids reaching into a crate-private helper without making it `pub`.
    let (days, rem) = (epoch_secs / 86400, epoch_secs % 86400);
    let (h, m) = (rem / 3600, (rem % 3600) / 60);
    let mut y = 1970u64;
    let mut d = days;
    loop {
        let dy = if y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400)) { 366 } else { 365 };
        if d < dy { break; }
        d -= dy;
        y += 1;
    }
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let mdays = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    let mut mo = 0usize;
    for md in &mdays {
        if d < *md { break; }
        d -= md;
        mo += 1;
    }
    format!("{} {} {y}, {h:02}:{m:02}", d + 1, months[mo.min(11)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_properties_tab_default_is_info() {
        let tab = ClipPropertiesTab::default();
        assert_eq!(tab, ClipPropertiesTab::Info);
    }

    #[test]
    fn clip_properties_tab_all_has_seven_entries() {
        assert_eq!(ClipPropertiesTab::ALL.len(), 7);
    }

    #[test]
    fn clip_properties_tab_labels_non_empty() {
        for tab in ClipPropertiesTab::ALL.iter() {
            assert!(!tab.label().is_empty());
        }
    }

    /// OpenClipProperties resets the tab to the default, so the panel
    /// always opens on Info rather than wherever it was last left.
    #[test]
    fn the_default_tab_is_info() {
        assert_eq!(ClipPropertiesTab::default(), ClipPropertiesTab::Info);
    }

    #[test]
    fn format_size_scales_units() {
        assert!(format_size(512).ends_with("KB"));
        assert!(format_size(5 * 1024 * 1024).ends_with("MB"));
        assert!(format_size(3 * 1024 * 1024 * 1024).ends_with("GB"));
    }
}
