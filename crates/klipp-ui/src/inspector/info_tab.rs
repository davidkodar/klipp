//! The Info tab: what the project, a bin item or a timeline clip is,
//! each fact a plain row and each setting a control, in the inspector's
//! one row style (`super::rows`).

use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::Clip;

use super::rows::{colour_row, mono_row, note, row, text_row, TEXT};
use crate::widgets::inspector_group::section_card;

/// Nothing selected: the sequence camera, the project, the media pool,
/// the sequence's make-up and the storage the project uses.
pub fn show_no_selection(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    cmds: &mut Vec<klipp_core::AppCommand>,
) {
    let seq = state.sequence();

    camera_section(ui, state, theme, cmds);

    section_card(ui, "PROJECT", None, theme, |ui| {
        let name = state.project_path.as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".into());
        text_row(ui, theme, "Name", name);

        let created = state.created_at.as_deref().map(|iso| {
            let clean = iso.trim_end_matches('Z');
            chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(clean, "%Y-%m-%dT%H:%M:%S"))
                .map(|dt| dt.format("%d %b %Y, %H:%M").to_string())
                .unwrap_or_else(|_| iso.to_string())
        });
        match created {
            Some(created) => text_row(ui, theme, "Created", created),
            None => colour_row(ui, theme, "Created", "Unknown", theme.text_secondary),
        }

        // How long ago the file was last written.
        let modified = state.project_path.as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .map(|t| ago(t.elapsed().unwrap_or_default().as_secs()));
        match modified {
            Some(modified) => text_row(ui, theme, "Last modified", modified),
            None => colour_row(ui, theme, "Last modified", "Not saved", theme.text_secondary),
        }

        // The revision a save writes; an older file is refused on load,
        // so an open project is at this one.
        text_row(ui, theme, "Format version", format!("Revision {}", klipp_project::FORMAT_REVISION));
        text_row(ui, theme, "Klipp version", klipp_core::build_info::get().version);
        if let Some(path) = &state.project_path {
            note(ui, theme, &path.to_string_lossy());
        }
        ui.add_space(4.0);
    });

    section_card(ui, "MEDIA", None, theme, |ui| {
        let all: Vec<_> = state.media_pool.all().collect();
        let video = all.iter().filter(|m| !m.video_codec.is_empty()).count();
        let audio = all.iter().filter(|m| m.video_codec.is_empty() && m.has_audio).count();
        text_row(ui, theme, "Clips in bin", format!("{} \u{00B7} {video} video \u{00B7} {audio} audio", all.len()));
        let secs: f64 = all.iter().map(|m| m.duration.to_secs_f64()).sum();
        mono_row(ui, theme, "Total bin duration", hms(secs));
        // A file is offline when the pool marks it so or the file watcher
        // saw it go; both are the one fact, counted once.
        let offline = all.iter().filter(|m| m.offline || state.offline_media_ids.contains(&m.id)).count();
        let status = if offline == 0 { theme.marker_success } else { theme.marker_error };
        colour_row(ui, theme, "Offline media", format!("{offline}"), status);
        ui.add_space(4.0);
    });

    section_card(ui, "SEQUENCE", None, theme, |ui| {
        text_row(ui, theme, "Resolution", format!("{} \u{00D7} {}", seq.resolution.width, seq.resolution.height));
        text_row(ui, theme, "Frame rate", fps_text(seq.fps.num as f64 / seq.fps.den.max(1) as f64));
        mono_row(ui, theme, "Duration", state.sequence_duration().to_timecode_string());
        let count = |kind| seq.tracks.iter().filter(|t| t.kind == kind).count();
        text_row(ui, theme, "Video tracks", format!("{}", count(klipp_timeline::TrackKind::Video)));
        text_row(ui, theme, "Audio tracks", format!("{}", count(klipp_timeline::TrackKind::Audio)));
        text_row(ui, theme, "Total clips", format!("{}", seq.tracks.iter().map(|t| t.clips.len()).sum::<usize>()));
        ui.add_space(4.0);
    });

    section_card(ui, "STORAGE", None, theme, |ui| {
        let project_file = state.project_path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
        match project_file {
            Some(bytes) => text_row(ui, theme, "Project file", format_size(bytes)),
            None => colour_row(ui, theme, "Project file", "Not saved", theme.text_secondary),
        }
        text_row(ui, theme, "Media files", format_size(state.media_pool.all().map(|m| m.file_size_bytes).sum()));

        match cache_sizes() {
            Some((cache, proxies)) => {
                text_row(ui, theme, "Cache", format_size(cache));
                if proxies > 0 {
                    text_row(ui, theme, "Proxies", format_size(proxies));
                } else {
                    colour_row(ui, theme, "Proxies", "None generated", theme.text_secondary);
                }
            }
            None => {
                // Measured on a thread; drawn again when it is in.
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                colour_row(ui, theme, "Cache", "Measuring\u{2026}", theme.text_secondary);
                colour_row(ui, theme, "Proxies", "Measuring\u{2026}", theme.text_secondary);
            }
        }

        let home = dirs::home_dir();
        match state.project_path.as_deref().or(home.as_deref()).and_then(disk_free_bytes) {
            Some(free) => {
                let status = if free < 1_000_000_000 {
                    theme.marker_error
                } else if free < 10_000_000_000 {
                    theme.marker_warning
                } else {
                    theme.marker_success
                };
                colour_row(ui, theme, "Free disk space", format_size(free), status);
            }
            None => colour_row(ui, theme, "Free disk space", "Unknown", theme.text_secondary),
        }
        ui.add_space(4.0);
    });

    ui.add_space(16.0);
}

/// "just now", "5 mins ago", "3 hours ago", "2 days ago".
fn ago(secs: u64) -> String {
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{} mins ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hours ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86400)
    }
}

/// Seconds as "h:mm:ss".
fn hms(secs: f64) -> String {
    let (h, m, s) = ((secs / 3600.0) as u32, ((secs % 3600.0) / 60.0) as u32, (secs % 60.0) as u32);
    format!("{h}:{m:02}:{s:02}")
}

/// "24 fps", or "29.970 fps" for a rate that is not whole.
fn fps_text(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.01 { format!("{} fps", fps.round() as u32) } else { format!("{fps:.3} fps") }
}

/// The cache's and the proxies' sizes in bytes, measured on a thread at
/// most every ten seconds: walking a cache of many gigabytes on every
/// repaint held the inspector up. `None` until the first measure is in.
fn cache_sizes() -> Option<(u64, u64)> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static SIZES: Mutex<(Option<Instant>, bool, u64, u64)> = Mutex::new((None, false, 0, 0));
    let mut s = SIZES.lock().unwrap_or_else(|e| e.into_inner());
    let stale = s.0.is_none_or(|at| at.elapsed() > Duration::from_secs(10));
    if stale && !s.1 {
        s.1 = true;
        let started = std::thread::Builder::new().name("cache-size".into()).spawn(|| {
            let cache = dirs::cache_dir().map(|d| d.join("klipp"));
            let all = cache.as_deref().and_then(dir_size_bytes).unwrap_or(0);
            let proxies = cache.map(|d| d.join("proxies")).and_then(|p| dir_size_bytes(&p)).unwrap_or(0);
            let mut s = SIZES.lock().unwrap_or_else(|e| e.into_inner());
            *s = (Some(Instant::now()), false, all, proxies);
        });
        if started.is_err() {
            s.1 = false;
        }
    }
    s.0.map(|_| (s.2, s.3))
}

/// A bin item selected: what the file is, its log and marks, its tags,
/// and where the timeline uses it.
pub fn show_bin_media(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    media: &klipp_media::MediaItem,
    tag_input: &mut String,
    cmds: &mut Vec<AppCommand>,
) {
    media_section(ui, state, theme, media);
    logging_section(ui, theme, media, cmds);
    markers_section(ui, theme, media, cmds);
    tags_section(ui, theme, media, tag_input, cmds);

    // Every clip on the timeline that uses this file.
    let seq = state.sequence();
    let mut usages: Vec<(String, String)> = Vec::new();
    let mut used_secs = 0.0_f64;
    for (idx, track) in seq.tracks.iter().enumerate() {
        for clip in track.clips.iter().filter(|c| c.media_id() == Some(media.id)) {
            usages.push((track_name(seq, idx), format!("{} \u{2013} {}", clip.timeline_in.to_timecode_string(), clip.timeline_out.to_timecode_string())));
            used_secs += clip.duration().to_secs_f64();
        }
    }
    let count = usages.len();
    section_card(ui, "TIMELINE USAGE", Some(format!("{count}")), theme, |ui| {
        if count == 0 {
            colour_row(ui, theme, "Used in", "Not used", theme.text_secondary);
        } else {
            text_row(ui, theme, "Used in", format!("{count} place{}", if count == 1 { "" } else { "s" }));
            mono_row(ui, theme, "Total used", klipp_core::Timecode::from_secs_f64(used_secs, seq.fps.num).to_timecode_string());
            for (track, range) in usages {
                mono_row(ui, theme, &track, range);
            }
        }
        ui.add_space(4.0);
    });

    ui.add_space(16.0);
}

/// A timeline clip selected: what its file is, its log and marks, its
/// tags, and where the clip sits.
#[allow(clippy::too_many_arguments)]
pub fn show_clip(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    track_idx: usize,
    clip: &Clip,
    media: &klipp_media::MediaItem,
    tag_input: &mut String,
    cmds: &mut Vec<AppCommand>,
) {
    media_section(ui, state, theme, media);
    logging_section(ui, theme, media, cmds);
    markers_section(ui, theme, media, cmds);
    tags_section(ui, theme, media, tag_input, cmds);

    section_card(ui, "TIMELINE", None, theme, |ui| {
        // Where the clip sits in the sequence, as a bar.
        let seq_dur = state.sequence_duration().to_secs_f64();
        if seq_dur > 0.0 {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(super::rows::ROW_PAD);
                let w = ui.available_width() - super::rows::ROW_PAD;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 4.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, theme.bg_raised);
                let frac = |t: f64| (t / seq_dur).clamp(0.0, 1.0) as f32;
                let span = egui::Rect::from_min_max(
                    egui::pos2(rect.min.x + rect.width() * frac(clip.timeline_in.to_secs_f64()), rect.min.y),
                    egui::pos2(rect.min.x + rect.width() * frac(clip.timeline_out.to_secs_f64()), rect.max.y),
                );
                ui.painter().rect_filled(span, 2.0, theme.text_secondary);
            });
            ui.add_space(4.0);
        }
        mono_row(ui, theme, "In", clip.timeline_in.to_timecode_string());
        mono_row(ui, theme, "Out", clip.timeline_out.to_timecode_string());
        mono_row(ui, theme, "Duration", clip.duration().to_timecode_string());
        text_row(ui, theme, "Track", track_name(state.sequence(), track_idx));
        text_row(ui, theme, "Speed", format!("{:.0}%", clip.speed * 100.0));
        ui.add_space(4.0);
    });
}

/// A track's name, or its kind and number when it has none ("V1", "A2").
fn track_name(seq: &klipp_timeline::Sequence, idx: usize) -> String {
    let Some(track) = seq.tracks.get(idx) else { return String::new() };
    if !track.name.is_empty() {
        return track.name.clone();
    }
    let n = seq.tracks[..=idx].iter().filter(|t| t.kind == track.kind).count();
    let letter = if track.kind == klipp_timeline::TrackKind::Audio { 'A' } else { 'V' };
    format!("{letter}{n}")
}

/// What the file is: its picture, sound, colour and size, the camera's
/// record of the shot, its proxy and where it is on disk. Shown alike
/// for a bin item and a timeline clip.
fn media_section(ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme, media: &klipp_media::MediaItem) {
    let dim = theme.text_secondary;
    section_card(ui, "CLIP INFO", None, theme, |ui| {
        text_row(ui, theme, "Resolution", format!("{} \u{00D7} {}", media.resolution.width, media.resolution.height));
        if media.is_still {
            text_row(ui, theme, "Type", "Still image");
        } else {
            text_row(ui, theme, "Frame rate", fps_text(media.fps.as_f64()));
        }
        let codecs: Vec<String> = [&media.video_codec, &media.audio_codec].into_iter().filter(|c| !c.is_empty()).map(|c| c.to_uppercase()).collect();
        if codecs.is_empty() {
            colour_row(ui, theme, "Codec", "Unknown", dim);
        } else {
            text_row(ui, theme, "Codec", codecs.join(" \u{00B7} "));
        }
        if media.has_audio {
            match audio_text(media) {
                Some(text) => text_row(ui, theme, "Audio", text),
                None => colour_row(ui, theme, "Audio", "Format unknown", dim),
            }
        }
        mono_row(ui, theme, "Duration", media.duration.to_timecode_string());
        text_row(ui, theme, "File size", format_size(media.file_size_bytes));

        // The colour class the file was read as, then the transfer and
        // primaries its stream is tagged with, when it is.
        if media.colour_space == klipp_media::ColourSpace::Unknown {
            colour_row(ui, theme, "Colour", media.colour_space.label(), dim);
        } else {
            text_row(ui, theme, "Colour", media.colour_space.label());
        }
        if media.color_transfer.is_some() || media.color_primaries.is_some() {
            let transfer = media.color_transfer.as_deref().unwrap_or("Unspecified");
            let primaries = media.color_primaries.as_deref().unwrap_or("");
            let text = if !primaries.is_empty() && primaries != "Unspecified" && primaries != transfer {
                format!("{transfer} / {primaries}")
            } else {
                transfer.to_string()
            };
            text_row(ui, theme, "Transfer", text);
        }

        if let Some(epoch_secs) = media.media_created_at.or(media.file_modified_at).filter(|&t| t > 0) {
            text_row(ui, theme, "Shoot date", format_epoch_date(epoch_secs));
        }
        if media.camera_make.is_some() || media.camera_model.is_some() {
            let make = media.camera_make.as_deref().unwrap_or("");
            let model = media.camera_model.as_deref().unwrap_or("");
            let sep = if !make.is_empty() && !model.is_empty() { " " } else { "" };
            text_row(ui, theme, "Camera", format!("{make}{sep}{model}"));
        }
        if let Some(claimed) = media.truncated_from_secs {
            let text = format!("{} of {} the header claims", klipp_media::describe_secs(media.duration.to_secs_f64()), klipp_media::describe_secs(claimed));
            colour_row(ui, theme, "Cut short", text, theme.marker_warning);
        }
        if let Some(sync) = &media.synced_audio {
            let name = state.media_pool.get(&sync.audio_media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "missing recording".to_string());
            text_row(ui, theme, "Synced audio", format!("{name} ({})", sync.describe()));
        }
        if let Some(span) = media.span {
            text_row(ui, theme, "Split take", span.label());
        }
        shot_rows(ui, theme, &media.shot);
        if let Some(enc) = &media.encoder {
            text_row(ui, theme, "Encoder", enc.clone());
        }
        if media.bit_depth.is_some() || media.pixel_format.is_some() {
            let mut parts = Vec::new();
            if let Some(bd) = media.bit_depth { parts.push(format!("{bd}-bit")); }
            if let Some(pf) = &media.pixel_format { parts.push(pf.clone()); }
            text_row(ui, theme, "Format", parts.join(" \u{00B7} "));
        }
        if let Some(vbr) = media.video_bitrate_kbps {
            text_row(ui, theme, "Bitrate", if vbr >= 1000 { format!("{:.1} Mbps", vbr as f64 / 1000.0) } else { format!("{vbr} kbps") });
        }
        tag_list(ui, theme, &media.shot);

        if media.proxy_ready {
            colour_row(ui, theme, "Proxy", "Ready", theme.marker_success);
        } else if state.proxy_generating.contains(&media.id) {
            let progress = state.proxy_progress.get(&media.id).copied().unwrap_or(0.0);
            text_row(ui, theme, "Proxy", format!("Making, {:.0}%", progress * 100.0));
            ui.ctx().request_repaint();
        } else {
            colour_row(ui, theme, "Proxy", "None", dim);
        }
        note(ui, theme, &media.path.to_string_lossy());
        ui.add_space(4.0);
    });
}

/// "Stereo · 48 kHz" from what the file says; `None` when it says
/// neither its channels nor its rate.
fn audio_text(media: &klipp_media::MediaItem) -> Option<String> {
    let channels = media.audio_channels.map(|n| match n {
        1 => "Mono".to_string(),
        2 => "Stereo".to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => format!("{n} channels"),
    });
    let rate = media.sample_rate.map(|r| {
        let khz = r as f64 / 1000.0;
        if khz == khz.floor() { format!("{} kHz", khz as u32) } else { format!("{khz:.1} kHz") }
    });
    let parts: Vec<String> = channels.into_iter().chain(rate).collect();
    (!parts.is_empty()).then(|| parts.join(" \u{00B7} "))
}

/// The file's tags as removable chips, and a field to add one.
fn tags_section(ui: &mut egui::Ui, theme: &KlippTheme, media: &klipp_media::MediaItem, tag_input: &mut String, cmds: &mut Vec<AppCommand>) {
    let pad = super::rows::ROW_PAD;
    section_card(ui, "TAGS", Some(format!("{}", media.tags.len())), theme, |ui| {
        ui.add_space(4.0);
        if media.tags.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(egui::RichText::new("No tags added").size(TEXT).color(theme.text_secondary));
            });
        } else {
            let mut remove: Option<usize> = None;
            ui.horizontal_wrapped(|ui| {
                ui.add_space(pad);
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for (i, tag) in media.tags.iter().enumerate() {
                    egui::Frame::none()
                        .fill(theme.bg_raised)
                        .rounding(4.0)
                        .inner_margin(egui::Margin { left: 8.0, right: 4.0, top: 3.0, bottom: 3.0 })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 3.0;
                                ui.label(egui::RichText::new(tag.as_str()).size(11.5).color(theme.text_primary));
                                if ui.add(egui::Button::new(egui::RichText::new("\u{00D7}").size(12.0).color(theme.text_secondary)).frame(false))
                                    .on_hover_text(format!("Remove the tag {tag}"))
                                    .clicked()
                                {
                                    remove = Some(i);
                                }
                            });
                        });
                }
            });
            if let Some(idx) = remove {
                let mut tags = media.tags.clone();
                tags.remove(idx);
                cmds.push(AppCommand::SetMediaTags { media_id: media.id, tags });
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(pad);
            let w = ui.available_width() - pad;
            let resp = ui.add(egui::TextEdit::singleline(tag_input).hint_text("+  Add tag\u{2026}").desired_width(w).font(egui::TextStyle::Small));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !tag_input.trim().is_empty() {
                let tag = tag_input.trim().to_lowercase();
                if !media.tags.contains(&tag) {
                    let mut tags = media.tags.clone();
                    tags.push(tag);
                    cmds.push(AppCommand::SetMediaTags { media_id: media.id, tags });
                }
                tag_input.clear();
            }
        });
        ui.add_space(8.0);
    });
}

// ── Helpers ──────────────────────────────────────────────

/// The marks the editor left on the clip: notes, favourite stretches,
/// rejected ones — each named in place and removable. Shown only when
/// there are any.
fn markers_section(ui: &mut egui::Ui, theme: &KlippTheme, media: &klipp_media::MediaItem, cmds: &mut Vec<AppCommand>) {
    if media.markers.is_empty() {
        return;
    }
    let pad = super::rows::ROW_PAD;
    let dim = theme.text_secondary;
    let rate = media.fps.rate();
    section_card(ui, "MARKERS", Some(format!("{}", media.markers.len())), theme, |ui| {
        ui.add_space(4.0);
        let mut remove: Option<usize> = None;
        for (i, m) in media.markers.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                let (glyph, colour) = match m.kind {
                    klipp_core::MediaMarkerKind::Note => ("\u{25CF}", egui::Color32::from_rgb(90, 160, 255)),
                    klipp_core::MediaMarkerKind::Favourite => ("\u{2665}", egui::Color32::from_rgb(80, 200, 120)),
                    klipp_core::MediaMarkerKind::Reject => ("\u{2717}", egui::Color32::from_rgb(220, 70, 70)),
                    klipp_core::MediaMarkerKind::Analysis => ("\u{25C6}", egui::Color32::from_rgb(170, 180, 200)),
                };
                ui.label(egui::RichText::new(glyph).size(12.0).color(colour));
                let when = klipp_core::Timecode::from_secs_f64(m.secs, rate).to_timecode_string();
                let span = if m.is_range() {
                    format!("{when} \u{2013} {}", klipp_core::Timecode::from_secs_f64(m.end_secs(), rate).to_timecode_string())
                } else {
                    when
                };
                ui.label(egui::RichText::new(span).monospace().size(11.5).color(dim));
                let mut name = m.name.clone();
                let w = (ui.available_width() - 28.0).max(60.0);
                if ui.add(egui::TextEdit::singleline(&mut name).desired_width(w).font(egui::TextStyle::Small).hint_text(m.kind.label())).changed() {
                    cmds.push(AppCommand::RenameMediaMarker { media_id: media.id, index: i, name });
                }
                if ui.add(egui::Button::new(egui::RichText::new("\u{00D7}").size(12.0).color(dim)).frame(false)).clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            cmds.push(AppCommand::RemoveMediaMarker { media_id: media.id, index: i });
        }
        ui.add_space(6.0);
    });
}

/// The editor's log of the clip — scene, shot, take, reel, keywords,
/// description, comments, good take — edited in place. Each change goes
/// out as one `SetMediaLog`; the keywords line is committed when it is
/// left, so a comma being typed is not swept away mid-edit.
fn logging_section(ui: &mut egui::Ui, theme: &KlippTheme, media: &klipp_media::MediaItem, cmds: &mut Vec<AppCommand>) {
    let pad = super::rows::ROW_PAD;
    let dim = theme.text_secondary;
    let badge = media.log.slate_line().or_else(|| (!media.log.is_empty()).then(|| "logged".to_string()));
    section_card(ui, "LOGGING", badge, theme, |ui| {
        let mut log = media.log.clone();
        let mut changed = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(pad);
            let total = ui.available_width() - pad;
            egui::Grid::new(("clip_log_grid", media.id)).num_columns(2).spacing([8.0, 4.0]).min_col_width(64.0).show(ui, |ui| {
                let edit_w = (total - 80.0).max(60.0);
                let mut line = |ui: &mut egui::Ui, label: &str, value: &mut String| {
                    ui.label(egui::RichText::new(label).size(TEXT).color(dim));
                    if ui.add(egui::TextEdit::singleline(value).desired_width(edit_w).font(egui::TextStyle::Small)).changed() {
                        changed = true;
                    }
                    ui.end_row();
                };
                line(ui, "Scene", &mut log.scene);
                line(ui, "Shot", &mut log.shot);
                line(ui, "Take", &mut log.take);
                line(ui, "Reel", &mut log.reel);

                // Keywords: edited as one line in a buffer of their own,
                // normalised when the field is left.
                ui.label(egui::RichText::new("Keywords").size(TEXT).color(dim));
                let buf_id = egui::Id::new(("clip_log_keywords", media.id));
                let mut buf: String = ui.ctx().data_mut(|d| d.get_temp::<String>(buf_id)).unwrap_or_else(|| log.keywords_line());
                let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width(edit_w).font(egui::TextStyle::Small).hint_text("interview, wide, b-roll"));
                if resp.has_focus() || resp.changed() {
                    ui.ctx().data_mut(|d| d.insert_temp(buf_id, buf.clone()));
                }
                if resp.lost_focus() {
                    ui.ctx().data_mut(|d| d.remove::<String>(buf_id));
                    let mut edited = log.clone();
                    edited.set_keywords_line(&buf);
                    if edited.keywords != log.keywords {
                        log.keywords = edited.keywords;
                        changed = true;
                    }
                }
                ui.end_row();

                ui.label(egui::RichText::new("Description").size(TEXT).color(dim));
                if ui.add(egui::TextEdit::multiline(&mut log.description).desired_width(edit_w).desired_rows(2).font(egui::TextStyle::Small)).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label(egui::RichText::new("Comments").size(TEXT).color(dim));
                if ui.add(egui::TextEdit::multiline(&mut log.comments).desired_width(edit_w).desired_rows(2).font(egui::TextStyle::Small)).changed() {
                    changed = true;
                }
                ui.end_row();
                ui.label("");
                if ui.checkbox(&mut log.good_take, egui::RichText::new("Good take").size(TEXT)).changed() {
                    changed = true;
                }
                ui.end_row();
            });
        });
        if let Some(reel) = media.shot.reel_name.as_deref().filter(|_| log.reel.is_empty()) {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.label(egui::RichText::new(format!("Reel from the file: {reel}")).size(11.0).color(dim));
            });
        }
        ui.add_space(8.0);
        if changed {
            cmds.push(AppCommand::SetMediaLog { media_id: media.id, log });
        }
    });
}

/// The shot as the camera recorded it: lens, exposure, white balance,
/// gamma and gamut, timecode and reel — each row only when known.
fn shot_rows(ui: &mut egui::Ui, theme: &KlippTheme, shot: &klipp_core::ShotMetadata) {
    let rows: [(&str, Option<String>); 6] = [
        ("Lens", shot.lens.clone()),
        ("Exposure", shot.exposure_summary()),
        ("White balance", shot.white_balance.clone()),
        (
            "Gamma",
            match (&shot.gamma, &shot.colour_gamut) {
                (Some(g), Some(c)) => Some(format!("{g} / {c}")),
                (Some(g), None) => Some(g.clone()),
                (None, Some(c)) => Some(c.clone()),
                (None, None) => None,
            },
        ),
        ("Timecode", shot.start_timecode.clone()),
        ("Reel", shot.reel_name.clone()),
    ];
    for (label, value) in rows {
        if let Some(value) = value {
            text_row(ui, theme, label, value);
        }
    }
}

/// Every tag the file carried, as written, folded away under a header.
fn tag_list(ui: &mut egui::Ui, theme: &KlippTheme, shot: &klipp_core::ShotMetadata) {
    if shot.tags.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(super::rows::ROW_PAD - 4.0);
        egui::CollapsingHeader::new(egui::RichText::new(format!("All metadata ({})", shot.tags.len())).size(TEXT).color(theme.text_secondary))
        .id_salt("shot_tag_list")
        .show(ui, |ui| {
            egui::Grid::new("shot_tag_grid").num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
                for (k, v) in &shot.tags {
                    ui.label(egui::RichText::new(k).monospace().size(10.0).color(theme.text_dim));
                    ui.add(egui::Label::new(egui::RichText::new(v).size(10.0).color(theme.text_primary)).wrap());
                    ui.end_row();
                }
            });
        });    });
}

/// Format Unix epoch seconds as "DD Mon YYYY, HH:MM".
fn format_epoch_date(epoch_secs: u64) -> String {
    let secs = epoch_secs;
    let (days, rem) = (secs / 86400, secs % 86400);
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

fn format_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn dir_size_bytes(path: &std::path::Path) -> Option<u64> {
    if !path.is_dir() { return None; }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = match entry.metadata() { Ok(m) => m, Err(_) => continue };
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                total += dir_size_bytes(&entry.path()).unwrap_or(0);
            }
        }
    }
    Some(total)
}

pub(crate) fn disk_free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::ffi::CString;
    let c_path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            Some(stat.f_bavail * stat.f_frsize)
        } else {
            None
        }
    }
}

/// The sequence camera's move, keyed at the playhead.
///
/// The camera had a shutter in Sequence Settings and nothing for the
/// move itself: where it looks, how far in, its roll. Those were
/// reachable only through the `camera` op over the control socket,
/// which is no use to someone editing.
///
/// Keys are taken at `state.playhead`, the frame-quantised one, not at
/// `playhead_smooth_secs`. The smooth value is the raw pointer position
/// during a scrub and is never resynced, so keying from it writes keys
/// a few milliseconds off the frame the user is looking at — reachable
/// by nothing afterwards.
fn camera_section(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    cmds: &mut Vec<klipp_core::AppCommand>,
) {
    use klipp_core::CameraParam;
    let seq = state.sequence();
    let idx = state.active_sequence_idx;
    let at = state.playhead.to_secs_f64();
    let tolerance = super::keyframe_selection::frame_tolerance(seq);
    section_card(ui, "CAMERA", None, theme, |ui| {
        for param in [CameraParam::X, CameraParam::Y, CameraParam::Zoom, CameraParam::Roll] {
            let keys = match param {
                CameraParam::X => &seq.camera.x_keyframes,
                CameraParam::Y => &seq.camera.y_keyframes,
                CameraParam::Zoom => &seq.camera.zoom_keyframes,
                CameraParam::Roll => &seq.camera.roll_keyframes,
            };
            let row_at = CameraRow::at(keys, at, tolerance, param.resting());
            let mut value = row_at.value;
            row(ui, theme, param.label(), |ui| {
                // Right to left: the key mark at the edge, the value before it.
                let mark = if row_at.keyed { "\u{25C6}" } else { "\u{25C7}" };
                let tint = if row_at.keyed { theme.accent } else { theme.text_secondary };
                if ui.add(egui::Button::new(egui::RichText::new(mark).color(tint)).frame(false))
                    .on_hover_text(if row_at.keyed { "Remove this key" } else { "Key at the playhead" })
                    .clicked()
                {
                    cmds.push(if row_at.keyed { row_at.remove(idx, param) } else { row_at.set(idx, param, value) });
                }
                let speed = match param {
                    CameraParam::Roll => 0.25,
                    CameraParam::Zoom => 0.01,
                    _ => 0.002,
                };
                if ui.add(egui::DragValue::new(&mut value).speed(speed)).changed() {
                    cmds.push(row_at.set(idx, param, value));
                }
            });
        }
        ui.add_space(4.0);
    });
}

/// The key a camera row stands on, or the moment it would key.
///
/// The row is asked at a displayed frame, but a key is stored at an exact
/// timestamp that need not sit on one: an agent writes 0.7 s into a 24 fps
/// sequence, and no frame of that sequence is at 0.7 s. So the row selects
/// the nearest key within the inspector's frame tolerance, as every other
/// inspector row does, and edits or removes *that* key at its own stored
/// timestamp. Only when
/// no key is near does the canonical playhead become the time. Asking the
/// remover instead, at the playhead, left the diamond dark on a key the
/// playhead was on and stacked a second key beside it on every drag.
struct CameraRow {
    /// Where an edit or removal goes: the selected key's stored timestamp,
    /// or the playhead when there is none.
    time_secs: f64,
    /// Whether that moment already holds a key.
    keyed: bool,
    /// What the row shows: the key's value, or the curve at the playhead.
    value: f32,
}

impl CameraRow {
    fn at(keys: &[klipp_core::Keyframe], playhead_secs: f64, tolerance: f64, resting: f32) -> Self {
        match super::keyframe_selection::nearest(keys, playhead_secs, tolerance) {
            Some(i) => Self { time_secs: keys[i].time_secs, keyed: true, value: keys[i].value },
            None => Self {
                time_secs: playhead_secs,
                keyed: false,
                value: klipp_core::keyframe::evaluate(keys, playhead_secs, resting),
            },
        }
    }

    fn set(&self, sequence_idx: usize, param: klipp_core::CameraParam, value: f32) -> AppCommand {
        AppCommand::SetSequenceCameraKey { sequence_idx, param, time_secs: self.time_secs, value }
    }

    fn remove(&self, sequence_idx: usize, param: klipp_core::CameraParam) -> AppCommand {
        AppCommand::RemoveSequenceCameraKey { sequence_idx, param, time_secs: self.time_secs }
    }
}

#[cfg(test)]
mod camera_row_tests {
    use super::*;
    use crate::inspector::keyframe_selection::frame_tolerance;
    use klipp_core::keyframe::{remove_keyframe_at, upsert_keyframe};
    use klipp_core::{CameraParam, Fps, Interpolation, Keyframe};
    use klipp_timeline::Sequence;

    /// The tolerance the inspector uses in a sequence at that rate — the
    /// one owner of that number, not a copy of it.
    fn tolerance_at(fps: Fps) -> f64 {
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        frame_tolerance(&seq)
    }

    fn keys(times: &[f64]) -> Vec<Keyframe> {
        times
            .iter()
            .map(|&time_secs| Keyframe {
                time_secs,
                value: 0.5,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
                handles: Default::default(),
            })
            .collect()
    }

    /// The timestamp a camera command carries, whichever it is.
    fn time_of(cmd: &AppCommand) -> f64 {
        match cmd {
            AppCommand::SetSequenceCameraKey { time_secs, .. }
            | AppCommand::RemoveSequenceCameraKey { time_secs, .. } => *time_secs,
            other => panic!("not a camera command: {other:?}"),
        }
    }

    /// A key at 0.7 s in a 24 fps sequence sits between frames 16 and
    /// 17; standing on frame 17 (0.708333…) the row is on that key,
    /// and what it emits goes to 0.7 exactly — so the edit replaces the
    /// key and the removal takes it, rather than adding one beside it.
    #[test]
    fn an_off_frame_key_is_edited_where_it_is_stored() {
        let stored = keys(&[0.7]);
        let row = CameraRow::at(&stored, 17.0 / 24.0, tolerance_at(Fps::film_24()), 0.0);
        assert!(row.keyed, "the diamond should light on frame 17 for the key at 0.7 s");
        assert_eq!(row.value, 0.5);
        let set = row.set(0, CameraParam::X, 0.3);
        let remove = row.remove(0, CameraParam::X);
        assert_eq!(time_of(&set), 0.7);
        assert_eq!(time_of(&remove), 0.7);

        let mut edited = stored.clone();
        upsert_keyframe(&mut edited, time_of(&set), 0.3);
        assert_eq!(edited.len(), 1, "editing the key should not add a second one at the frame");
        assert_eq!(edited[0].value, 0.3);
        let mut cleared = stored.clone();
        remove_keyframe_at(&mut cleared, time_of(&remove));
        assert!(cleared.is_empty(), "removing from frame 17 should take the key at 0.7 s");
    }

    /// Keys on consecutive frames at 60, 120 and 29.97 fps are distinct
    /// keys: each frame — and a scrub a quarter-frame past it — selects
    /// its own, emits its stored time, and leaves its neighbours alone.
    #[test]
    fn consecutive_keys_stay_distinct_at_high_and_fractional_rates() {
        for rate in [Fps::new(60, 1), Fps::new(120, 1), Fps::ntsc_29970()] {
            let fps = rate.as_f64();
            let times: Vec<f64> = (0..48).map(|i| i as f64 / fps).collect();
            let stored = keys(&times);
            for (i, &t) in times.iter().enumerate() {
                for probe in [t, t + 0.25 / fps] {
                    let row = CameraRow::at(&stored, probe, tolerance_at(rate), 0.0);
                    assert!(row.keyed, "frame {i} at {fps} fps holds a key");
                    assert_eq!(row.time_secs, t, "frame {i} at {fps} fps should select its own key, probed at {probe}");
                    let set = row.set(0, CameraParam::Zoom, 1.5);
                    let mut edited = stored.clone();
                    upsert_keyframe(&mut edited, time_of(&set), 1.5);
                    assert_eq!(edited.len(), stored.len(), "editing frame {i} at {fps} fps should not add a key");
                    assert_eq!(edited[i].value, 1.5, "…and should change only that key");
                    assert!(edited.iter().enumerate().all(|(j, k)| j == i || k.value == 0.5));
                    let mut cleared = stored.clone();
                    remove_keyframe_at(&mut cleared, time_of(&row.remove(0, CameraParam::Zoom)));
                    assert_eq!(cleared.len(), stored.len() - 1, "removing frame {i} at {fps} fps should take one key");
                    assert!(cleared.iter().all(|k| k.time_secs != t), "…and that key");
                }
            }
        }
    }

    /// Between keys, the row is the curve at the playhead and a key goes
    /// to the playhead — the canonical one it was given.
    #[test]
    fn with_no_key_near_the_playhead_is_the_moment() {
        let stored = keys(&[1.0, 2.0]);
        let row = CameraRow::at(&stored, 1.5, tolerance_at(Fps::film_24()), 0.0);
        assert!(!row.keyed);
        assert_eq!(row.time_secs, 1.5);
        assert_eq!(row.value, 0.5);
        assert_eq!(time_of(&row.set(0, CameraParam::Roll, 2.0)), 1.5);
        let empty: Vec<Keyframe> = Vec::new();
        assert_eq!(CameraRow::at(&empty, 1.5, tolerance_at(Fps::film_24()), 1.0).value, 1.0, "no keys: the resting value");
    }

    /// A key exactly half a frame from a frame time, in a rate whose
    /// frame is not a binary fraction: 17/24 + 1/48 is one ulp outside
    /// a bare half-frame comparison at 17/24. The row uses the shared
    /// window, which has the slack for that, so the key is on the row
    /// from the frame on either side of it — and its stored time is
    /// what the commands carry.
    #[test]
    fn a_key_on_the_decimal_half_frame_boundary_is_still_the_rows_key() {
        let stored_at = 17.0 / 24.0 + 1.0 / 48.0;
        let stored = keys(&[stored_at]);
        let tolerance = tolerance_at(Fps::film_24());
        for frame in [17.0, 18.0] {
            let row = CameraRow::at(&stored, frame / 24.0, tolerance, 0.0);
            assert!(row.keyed, "frame {frame} should be on the key half a frame from it");
            assert_eq!(row.time_secs, stored_at);
            assert_eq!(time_of(&row.set(0, CameraParam::Y, 0.1)), stored_at);
            assert_eq!(time_of(&row.remove(0, CameraParam::Y)), stored_at);
            let mut edited = stored.clone();
            upsert_keyframe(&mut edited, time_of(&row.set(0, CameraParam::Y, 0.1)), 0.1);
            assert_eq!(edited.len(), 1, "editing from frame {frame} should not add a key");
        }
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;
    use crate::inspector::rows::test_frame::{click, frame};
    use klipp_core::{MediaId, RationalTime};

    /// The texts a frame drew, and how many hairline rules it drew.
    fn drawn(ctx: &egui::Context, draw: impl FnMut(&mut egui::Ui, &KlippTheme, &mut Vec<AppCommand>)) -> (Vec<String>, Vec<(String, egui::Pos2)>) {
        let (texts, _) = frame(ctx, Vec::new(), draw);
        (texts.iter().map(|(t, _)| t.clone()).collect(), texts)
    }

    /// Hairlines a frame painted: the section headers' own, and any
    /// rule between rows.
    fn rules(ctx: &egui::Context, mut draw: impl FnMut(&mut egui::Ui, &KlippTheme, &mut Vec<AppCommand>)) -> usize {
        let theme = KlippTheme::default();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 6000.0))), ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| draw(ui, &theme, &mut Vec::new()));
            },
        );
        out.shapes.iter().filter(|s| matches!(s.shape, egui::Shape::LineSegment { .. })).count()
    }

    /// The value drawn on `label`'s row: the text furthest right at its height.
    fn value_of(texts: &[(String, egui::Pos2)], label: &str) -> String {
        let (_, at) = texts.iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("no {label}: {texts:?}"));
        texts.iter().filter(|(_, p)| (p.y - at.y).abs() < 4.0 && p.x > at.x).max_by(|a, b| a.1.x.total_cmp(&b.1.x)).map(|(t, _)| t.clone()).unwrap_or_default()
    }

    fn assert_has(all: &[String], labels: &[&str]) {
        for label in labels {
            assert!(all.iter().any(|t| t == label), "{label} is no longer shown: {all:?}");
        }
    }

    /// A file that says little about itself: no channel count, no rate,
    /// no colour tags. The Info tab must not make those up.
    fn media() -> klipp_media::MediaItem {
        let mut m = klipp_media::MediaItem::new_synthetic_title(MediaId::new(), "shot.mov".into(), RationalTime::from_secs_f64(10.0, 24), klipp_core::Fps::film_24());
        m.path = "/footage/shot.mov".into();
        m.video_codec = "h264".into();
        m.audio_codec = "aac".into();
        m.has_audio = true;
        m.file_size_bytes = 5 * 1024 * 1024;
        m.media_created_at = Some(1_700_000_000);
        m.camera_make = Some("Acme".into());
        m.encoder = Some("Lavf".into());
        m.bit_depth = Some(10);
        m.video_bitrate_kbps = Some(25_000);
        m.color_transfer = Some("bt709".into());
        m.truncated_from_secs = Some(12.0);
        m.shot.lens = Some("35mm".into());
        m.tags = vec!["wide".into(), "interview".into()];
        m
    }

    /// Nothing selected: every fact the tab gave is still there (the two
    /// offline counts, which counted the one thing twice, are one row),
    /// with real values: the format revision a save writes, no project
    /// file size for a project never saved, and the media's size under
    /// its own name.
    #[test]
    fn nothing_selected_keeps_every_fact_with_real_values() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        let mut m = media();
        m.offline = true;
        state.offline_media_ids.push(m.id);
        state.media_pool.add(m);
        let draw = |ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>| show_no_selection(ui, &state, theme, cmds);
        let (all, texts) = drawn(&ctx, draw);
        assert_has(&all, &[
            "CAMERA", "X", "Y", "Zoom", "Roll",
            "PROJECT", "Name", "Created", "Last modified", "Format version", "Klipp version",
            "MEDIA", "Clips in bin", "Total bin duration", "Offline media",
            "SEQUENCE", "Resolution", "Frame rate", "Duration", "Video tracks", "Audio tracks", "Total clips",
            "STORAGE", "Project file", "Media files", "Cache", "Proxies", "Free disk space",
        ]);
        assert_eq!(value_of(&texts, "Format version"), format!("Revision {}", klipp_project::FORMAT_REVISION));
        assert!(!all.iter().any(|t| t.contains("klipp-v2")), "a fixed version string: {all:?}");
        assert_eq!(value_of(&texts, "Project file"), "Not saved", "an unsaved project has no file");
        assert_eq!(value_of(&texts, "Media files"), "5 MB", "the media's size");
        assert_eq!(value_of(&texts, "Offline media"), "1", "one offline file, counted once");
        assert_eq!(value_of(&texts, "Clips in bin"), "1 \u{00B7} 1 video \u{00B7} 0 audio");
        assert_eq!(value_of(&texts, "Klipp version"), klipp_core::build_info::get().version);
        assert_eq!(rules(&ctx, draw), 5, "a rule under each section header and none between rows");
    }

    /// A bin item: every fact the tab gave is still there, and what the
    /// file does not say is not filled in (no "Stereo", no "48 kHz", no
    /// "SDR" for an untagged file).
    #[test]
    fn a_bin_item_keeps_every_fact_and_invents_none() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        let m = media();
        state.media_pool.add(m.clone());
        let seq = state.sequence_mut();
        seq.tracks[0].clips.push(Clip::media(m.id, RationalTime::from_secs_f64(0.0, 24), RationalTime::from_secs_f64(2.0, 24), RationalTime::from_secs_f64(0.0, 24), RationalTime::from_secs_f64(2.0, 24)));
        let mut tag_input = String::new();
        let mut draw = |ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>| show_bin_media(ui, &state, theme, &m, &mut tag_input, cmds);
        let (all, texts) = drawn(&ctx, &mut draw);
        assert_has(&all, &[
            "CLIP INFO", "Resolution", "Frame rate", "Codec", "Audio", "Duration", "File size", "Colour", "Transfer",
            "Shoot date", "Camera", "Cut short", "Lens", "Encoder", "Format", "Bitrate", "Proxy", "/footage/shot.mov",
            "LOGGING", "Scene", "Shot", "Take", "Reel", "Keywords", "Description", "Comments", "Good take",
            "TAGS", "wide", "interview", "TIMELINE USAGE", "Used in", "Total used",
        ]);
        assert_eq!(value_of(&texts, "Codec"), "H264 \u{00B7} AAC");
        assert_eq!(value_of(&texts, "Audio"), "Format unknown");
        assert_eq!(value_of(&texts, "Colour"), "Unknown", "an untagged file's colour");
        for made_up in ["Stereo", "48 kHz", "SDR", "BT.709"] {
            assert!(!all.iter().any(|t| t.contains(made_up)), "{made_up} is not what the file says: {all:?}");
        }
        let track = track_name(state.sequence(), 0);
        assert!(!track.is_empty());
        assert!(all.contains(&track), "the track the file is used on: {all:?}");
        assert_eq!(rules(&ctx, &mut draw), 4, "a rule under each section header and none between rows");
    }

    /// A timeline clip: the file's facts and where the clip sits, in
    /// plain text; the track named even when it has no name of its own.
    #[test]
    fn a_clip_keeps_every_fact() {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let m = media();
        let clip = Clip::media(m.id, RationalTime::from_secs_f64(1.0, 24), RationalTime::from_secs_f64(3.0, 24), RationalTime::from_secs_f64(0.0, 24), RationalTime::from_secs_f64(2.0, 24));
        let mut tag_input = String::new();
        let draw = |ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>| show_clip(ui, &state, theme, 0, &clip, &m, &mut tag_input, cmds);
        let (all, texts) = drawn(&ctx, draw);
        assert_has(&all, &[
            "CLIP INFO", "Resolution", "Frame rate", "Codec", "Audio", "Duration", "File size", "Colour", "Proxy",
            "LOGGING", "TAGS", "TIMELINE", "In", "Out", "Track", "Speed", "100%",
        ]);
        let name = track_name(state.sequence(), 0);
        assert!(!name.is_empty());
        assert_eq!(value_of(&texts, "Track"), name);
        assert_eq!(value_of(&texts, "Speed"), "100%");
    }

    /// The controls answer real clicks: the camera's key mark keys the
    /// playhead, a tag's cross removes that tag and no other.
    #[test]
    fn the_controls_do_what_they_say() {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let draw = |ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>| show_no_selection(ui, &state, theme, cmds);
        let (_, texts) = drawn(&ctx, draw);
        let zoom_y = texts.iter().find(|(t, _)| t == "Zoom").expect("a Zoom row").1.y;
        let mark = texts.iter().find(|(t, p)| t == "\u{25C7}" && (p.y - zoom_y).abs() < 4.0).expect("Zoom's key mark").1;
        let cmds = click(&ctx, mark, draw);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetSequenceCameraKey { param: klipp_core::CameraParam::Zoom, .. }]), "{cmds:?}");

        let ctx = egui::Context::default();
        let m = media();
        let mut tag_input = String::new();
        let mut draw = |ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>| show_bin_media(ui, &state, theme, &m, &mut tag_input, cmds);
        let (_, texts) = drawn(&ctx, &mut draw);
        let wide = texts.iter().find(|(t, _)| t == "wide").expect("the wide tag").1;
        let cross = texts.iter().filter(|(t, p)| t == "\u{00D7}" && p.x > wide.x && (p.y - wide.y).abs() < 4.0).map(|(_, p)| *p).min_by(|a, b| a.x.total_cmp(&b.x)).expect("its cross");
        let cmds = click(&ctx, cross, &mut draw);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetMediaTags { tags, .. }] if tags == &vec!["interview".to_string()]), "{cmds:?}");
    }
}
