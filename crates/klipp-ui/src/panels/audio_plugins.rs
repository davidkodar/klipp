//! Audio Plugins management dialog.
//!
//! Lists every discovered LADSPA plugin grouped by maker, the search
//! paths that were scanned, and exposes a Rescan action. Future LV2 /
//! VST3 sections will be added under additional headings here once
//! those backends ship.
//!
//! Wired identically to the AI Models dialog:
//!   * `AppState::show_audio_plugins_dialog: bool` toggles visibility
//!   * `AppCommand::OpenAudioPluginsDialog` opens it
//!   * `AppCommand::RescanAudioPlugins` clears the on-disk cache and
//!     re-runs `discover_ladspa_plugins_cached`
//!   * `AppState::audio_plugins_dialog` holds the per-frame UI state
//!     (search query, last-scanned timestamp)
//!
//! Filter / disable functionality lands in a follow-up; this version
//! is read-only management plus rescan.

use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_audio::ladspa::LadspaPluginInfo;
use klipp_core::AppCommand;
use std::path::PathBuf;
use std::time::SystemTime;

/// Persistent (across-frame) state for the dialog.
///
/// `last_scanned_secs` is the unix-epoch second at which the most
/// recent rescan completed in *this* process. The cache file's mtime
/// tracks across-restart freshness, but for the in-dialog "Last
/// scanned: 2 min ago" indicator we want a process-local clock so a
/// user-initiated rescan visibly resets it.
#[derive(Debug, Default)]
pub struct AudioPluginsDialogState {
    pub search_query: String,
    /// Set to the wall-clock time when the dialog opens (so the user
    /// sees a real "last scanned" value even on cache-hit launches),
    /// then bumped every time a Rescan completes.
    pub last_scanned_secs: Option<u64>,
}

/// LADSPA search paths in the order `discover_ladspa_plugins` scans
/// them. Re-implemented here so the dialog can display per-path
/// status (plugin count + present/missing) without exposing the
/// internal helper from klipp-audio.
fn ladspa_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(env_path) = std::env::var("LADSPA_PATH") {
        for p in env_path.split(':') {
            if !p.is_empty() {
                paths.push(PathBuf::from(p));
            }
        }
    }
    paths.push(PathBuf::from("/usr/lib/ladspa"));
    paths.push(PathBuf::from("/usr/local/lib/ladspa"));
    paths.push(PathBuf::from("/usr/lib64/ladspa"));
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".ladspa"));
    } else if let Ok(home) = std::env::var("HOME") {
        paths.push(PathBuf::from(home).join(".ladspa"));
    }
    paths
}

/// Render the dialog. Pushes any user-driven `AppCommand`s (Rescan,
/// future Disable / Enable) onto `cmds`.
pub fn show_audio_plugins_dialog(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
    cmds: &mut Vec<AppCommand>,
) {
    if !state.show_audio_plugins_dialog {
        return;
    }

    // Open-on-first-frame: stamp last_scanned_secs so the indicator
    // shows a meaningful value even before the user clicks Rescan.
    // We treat the moment the dialog opens as "we've seen the current
    // discovery state" which is honest: state.ladspa_plugins is the
    // result of the most recent discovery (cache hit or cold scan).
    if state.audio_plugins_dialog.last_scanned_secs.is_none() {
        state.audio_plugins_dialog.last_scanned_secs = Some(now_secs());
    }

    // Screen-clamped sizing so the dialog never grows past 80% of the
    // viewport even when the plugin list is huge. Default starts ~70% so
    // there's visible margin around it. Same pattern as the AI Models
    // dialog.
    let screen_rect = ctx.screen_rect();
    let max_h = (screen_rect.height() * 0.80).clamp(360.0, 720.0);
    let max_w = (screen_rect.width() * 0.80).clamp(480.0, 820.0);
    let default_h = (screen_rect.height() * 0.70).clamp(360.0, max_h);
    let default_w = (screen_rect.width() * 0.55).clamp(480.0, max_w);

    // Default-centre the window on first open. Using `default_pos`
    // (not `anchor`) means the window is draggable; `anchor` would
    // pin it and break the move handle entirely. egui remembers the
    // user's chosen position across opens via window memory.
    let centre = screen_rect.center();
    let default_pos = egui::pos2(
        centre.x - default_w * 0.5,
        centre.y - default_h * 0.5,
    );

    let mut open = true;
    egui::Window::new("Audio Plugins")
        .open(&mut open)
        .resizable(true)
        .movable(true)
        .default_size([default_w, default_h])
        .default_pos(default_pos)
        .min_width(420.0)
        .min_height(360.0)
        .max_width(max_w)
        .max_height(max_h)
        .show(ctx, |ui| {
            render_body(ui, state, theme, cmds, max_h);
        });

    if !open {
        state.show_audio_plugins_dialog = false;
    }
}

fn render_body(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    cmds: &mut Vec<AppCommand>,
    max_h: f32,
) {
    // ── Header: search + Rescan ──
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.audio_plugins_dialog.search_query)
                .hint_text("Search plugins...")
                .desired_width(ui.available_width() - 90.0),
        );
        if ui.button("Rescan").clicked() {
            cmds.push(AppCommand::RescanAudioPlugins);
        }
    });

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    // Filter the plugin list.
    let search_lower = state.audio_plugins_dialog.search_query.to_lowercase();
    let mut visible: Vec<&LadspaPluginInfo> = state
        .ladspa_plugins
        .iter()
        .filter(|p| {
            if search_lower.is_empty() {
                return true;
            }
            let display = if p.name.is_empty() { &p.label } else { &p.name };
            display.to_lowercase().contains(&search_lower)
                || p.maker.to_lowercase().contains(&search_lower)
                || p.label.to_lowercase().contains(&search_lower)
                || p.path.to_string_lossy().to_lowercase().contains(&search_lower)
        })
        .collect();
    visible.sort_by(|a, b| {
        a.maker
            .to_lowercase()
            .cmp(&b.maker.to_lowercase())
            .then_with(|| {
                let an = if a.name.is_empty() { &a.label } else { &a.name };
                let bn = if b.name.is_empty() { &b.label } else { &b.name };
                an.to_lowercase().cmp(&bn.to_lowercase())
            })
    });

    // ── Body: plugin list grouped by maker, then search-paths panel ──
    //
    // The list scroll's max_height is computed from the dialog's
    // hard cap (max_h) minus the rough vertical budget of everything
    // else (header + separators + search-paths section + footer).
    // Using `ui.available_height()` here is a trap — content below the
    // scroll grows it, the Window resizes to fit, available_height
    // grows next frame, and the dialog walks down the screen. Pinning
    // against `max_h` keeps the layout stable.
    //
    // Search-paths section budget assumes up to 5 path rows × ~22 px.
    let other_budget = 64.0  // header
        + 24.0               // separators
        + 22.0 * 5.0         // search-paths rows
        + 56.0;              // search-paths header + footer
    let scroll_max_h = (max_h - other_budget).max(160.0);

    egui::ScrollArea::vertical()
        .max_height(scroll_max_h)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "DETECTED ({})",
                    state.ladspa_plugins.len()
                ))
                .size(10.0)
                .strong()
                .color(theme.text_dim),
            );
            ui.add_space(4.0);

            if state.ladspa_plugins.is_empty() {
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("No LADSPA plugins detected.")
                            .size(11.0)
                            .color(theme.text_dim),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "Install LADSPA plugins via your package manager \
                             (e.g. `pacman -S ladspa-plugins` on Arch / CachyOS, \
                             `apt install ladspa-sdk` on Ubuntu / Debian) and click Rescan.",
                        )
                        .size(10.0)
                        .color(theme.text_dim),
                    );
                });
                return;
            }

            if visible.is_empty() {
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "No plugins match \"{}\"",
                            state.audio_plugins_dialog.search_query
                        ))
                        .size(11.0)
                        .color(theme.text_dim),
                    );
                });
                return;
            }

            let mut last_maker: Option<String> = None;
            for plugin in &visible {
                let maker = if plugin.maker.is_empty() {
                    "Other".to_string()
                } else {
                    plugin.maker.clone()
                };
                if last_maker.as_ref() != Some(&maker) {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(maker.to_uppercase())
                            .size(9.0)
                            .strong()
                            .color(theme.text_secondary),
                    );
                    ui.add_space(2.0);
                    last_maker = Some(maker);
                }

                let is_disabled =
                    state.disabled_audio_plugins.contains(&plugin.unique_id);
                render_plugin_row(ui, theme, plugin, is_disabled, cmds);
            }
        });

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    // ── Search paths section ──
    render_search_paths(ui, state, theme);

    // ── Footer ──
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(2.0);
    let last_scanned = state
        .audio_plugins_dialog
        .last_scanned_secs
        .map(|secs| format_relative_time(now_secs().saturating_sub(secs)))
        .unwrap_or_else(|| "—".to_string());
    let disabled_count = state.disabled_audio_plugins.len();
    ui.horizontal(|ui| {
        let summary = if disabled_count > 0 {
            format!(
                "{} detected ({} disabled)   |   Last scanned: {}",
                state.ladspa_plugins.len(),
                disabled_count,
                last_scanned
            )
        } else {
            format!(
                "{} detected   |   Last scanned: {}",
                state.ladspa_plugins.len(),
                last_scanned
            )
        };
        ui.label(
            egui::RichText::new(summary)
                .size(10.0)
                .color(theme.text_dim),
        );
    });
}

fn render_plugin_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    plugin: &LadspaPluginInfo,
    is_disabled: bool,
    cmds: &mut Vec<AppCommand>,
) {
    let display = if plugin.name.is_empty() {
        plugin.label.as_str()
    } else {
        plugin.name.as_str()
    };
    // Technical LADSPA label alongside the display name — audio
    // technicians know plugins by their label (matches docs, .rdf
    // metadata, debug logs). Hide when the label is just a lower-case
    // form of the display name (no extra information).
    let tech_label = if !plugin.name.is_empty()
        && !plugin.label.is_empty()
        && plugin.name.to_lowercase() != plugin.label.to_lowercase()
    {
        Some(plugin.label.as_str())
    } else {
        None
    };

    let shape_label = if plugin.audio_inputs == 1 && plugin.audio_outputs == 1 {
        "mono"
    } else if plugin.audio_inputs == 2 && plugin.audio_outputs == 2 {
        "stereo"
    } else if plugin.audio_inputs == 1 && plugin.audio_outputs == 2 {
        "mono → stereo"
    } else {
        "multi"
    };

    // Per-plugin expansion state, persisted across frames. Keyed off
    // unique_id so a sort/filter doesn't shuffle which row was open.
    let expand_key = ui.id().with(("plugin_row_expanded", plugin.unique_id));
    let is_expanded: bool = ui
        .ctx()
        .data_mut(|d| d.get_temp(expand_key).unwrap_or(false));

    // Visually mute disabled plugins. Border tint hints "this row is
    // not active" without removing it from the list — the manager
    // dialog's whole point is letting the user re-enable.
    let row_bg = theme.bg_raised;
    let row_border = if is_disabled {
        theme.text_dim
    } else {
        theme.border
    };
    let primary_color = if is_disabled {
        theme.text_dim
    } else {
        theme.text_primary
    };

    egui::Frame::none()
        .fill(row_bg)
        .stroke(egui::Stroke::new(1.0_f32, row_border))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .outer_margin(egui::Margin::symmetric(0.0, 1.0))
        .show(ui, |ui| {
            // The Enable/Disable button sits inside the header's
            // right-to-left section. Capturing its rect lets us skip
            // the row-level "click to expand" toggle when the click
            // landed on the button — without that, clicking Disable
            // would also expand/collapse the row.
            let mut button_rect: Option<egui::Rect> = None;
            // ── Header row (always visible) ─────────────────────────
            let header_resp = ui.horizontal(|ui| {
                let chev = if is_expanded { "\u{25BC}" } else { "\u{25B6}" };
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(chev)
                            .size(9.0)
                            .color(theme.text_dim),
                    )
                    .selectable(false),
                );
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(display)
                                .size(11.0)
                                .strong()
                                .color(primary_color),
                        );
                        if let Some(label) = tech_label {
                            ui.label(
                                egui::RichText::new(label)
                                    .size(10.0)
                                    .monospace()
                                    .color(theme.text_dim),
                            );
                        }
                        if is_disabled {
                            ui.label(
                                egui::RichText::new("DISABLED")
                                    .size(8.0)
                                    .strong()
                                    .color(theme.text_dim),
                            );
                        }
                    });
                    ui.label(
                        egui::RichText::new(plugin.path.to_string_lossy())
                            .size(9.0)
                            .color(theme.text_dim),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (btn_label, hover) = if is_disabled {
                        (
                            "Enable",
                            "Re-enable this plugin so it appears in the Audio FX browser \
                             and the inspector's \"+ Add Effect\" picker.",
                        )
                    } else {
                        (
                            "Disable",
                            "Hide this plugin from the Audio FX browser and the inspector's \
                             \"+ Add Effect\" picker. Existing clips that already use it keep \
                             working.",
                        )
                    };
                    let btn = ui.add(
                        egui::Button::new(egui::RichText::new(btn_label).size(10.0))
                            .min_size(egui::vec2(60.0, 0.0)),
                    );
                    let btn = btn.on_hover_text(hover);
                    button_rect = Some(btn.rect);
                    if btn.clicked() {
                        cmds.push(AppCommand::ToggleAudioPluginDisabled {
                            unique_id: plugin.unique_id,
                        });
                    }
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} | {} ports",
                            shape_label, plugin.port_count
                        ))
                        .size(9.0)
                        .color(theme.text_dim),
                    );
                });
            });

            // Make the rest of the header row clickable to toggle
            // expansion. We register a single Sense::click over the
            // whole row, then suppress the toggle when the click
            // pointer landed inside the Enable/Disable button's rect
            // (the button has already consumed its own click via
            // `btn.clicked()` above, so without this guard we'd
            // double-handle the click and bounce the expansion state
            // every time the user toggled enabled).
            let click_resp = ui.interact(
                header_resp.response.rect,
                expand_key.with("click"),
                egui::Sense::click(),
            );
            if click_resp.clicked() {
                let on_button = click_resp
                    .interact_pointer_pos()
                    .zip(button_rect)
                    .map(|(p, r)| r.contains(p))
                    .unwrap_or(false);
                if !on_button {
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(expand_key, !is_expanded));
                }
            }

            // ── Expanded body ───────────────────────────────────────
            // Enable/Disable lives on the header row now (no need to
            // expand to disable). The expanded body is purely
            // informational: identity, file metadata, control list.
            if is_expanded {
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                render_plugin_details(ui, theme, plugin);
            }

            // is_disabled is unused here today but the row's tile
            // colour was already adjusted at the top — silence the
            // unused-but-necessary compiler warning by referencing
            // it in a no-op binding.
            let _ = is_disabled;
        });
}

/// Per-plugin detail panel shown when a row is expanded. Pulls every
/// piece of metadata we already have on `LadspaPluginInfo` plus a
/// stat() of the .so for file size — no extra LADSPA work, just
/// surfacing what's already extracted.
fn render_plugin_details(ui: &mut egui::Ui, theme: &KlippTheme, plugin: &LadspaPluginInfo) {
    // ── Identity row ────────────────────────────────────────────────
    egui::Grid::new(("plugin_details", plugin.unique_id))
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            detail_label(ui, theme, "Unique ID");
            ui.label(
                egui::RichText::new(plugin.unique_id.to_string())
                    .size(10.0)
                    .monospace()
                    .color(theme.text_secondary),
            );
            ui.end_row();

            detail_label(ui, theme, "Label");
            ui.label(
                egui::RichText::new(&plugin.label)
                    .size(10.0)
                    .monospace()
                    .color(theme.text_secondary),
            );
            ui.end_row();

            detail_label(ui, theme, "Maker");
            ui.label(
                egui::RichText::new(if plugin.maker.is_empty() {
                    "(unknown)"
                } else {
                    plugin.maker.as_str()
                })
                .size(10.0)
                .color(theme.text_secondary),
            );
            ui.end_row();

            // LADSPA `copyright` field — typically a short licence
            // phrase ("GPL", "MIT/X11", "BSD"). Empty when the plugin
            // author left it blank in the descriptor.
            if !plugin.copyright.is_empty() {
                detail_label(ui, theme, "Licence");
                ui.label(
                    egui::RichText::new(&plugin.copyright)
                        .size(10.0)
                        .color(theme.text_secondary),
                );
                ui.end_row();
            }

            detail_label(ui, theme, "Audio I/O");
            ui.label(
                egui::RichText::new(format!(
                    "{} input{}, {} output{}",
                    plugin.audio_inputs,
                    if plugin.audio_inputs == 1 { "" } else { "s" },
                    plugin.audio_outputs,
                    if plugin.audio_outputs == 1 { "" } else { "s" },
                ))
                .size(10.0)
                .color(theme.text_secondary),
            );
            ui.end_row();

            // ── File metadata ───────────────────────────────────────
            // `metadata()` follows symlinks so the size/mtime reflect
            // the real plugin .so. `symlink_metadata()` separately
            // tells us whether the path itself is a symlink and, if
            // so, what it points at — relevant on Debian-style distros
            // that symlink LADSPA plugins from /usr/lib/x86_64-linux-gnu.
            detail_label(ui, theme, "File");
            let file_summary = match std::fs::metadata(&plugin.path) {
                Ok(meta) => format_bytes(meta.len()),
                Err(_) => "(unreadable)".to_string(),
            };
            ui.label(
                egui::RichText::new(file_summary)
                    .size(10.0)
                    .color(theme.text_secondary),
            );
            ui.end_row();

            if let Ok(modified_relative) = std::fs::metadata(&plugin.path)
                .and_then(|m| m.modified())
                .map(|t| {
                    let secs = t
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    format_relative_time(now_secs().saturating_sub(secs))
                })
            {
                detail_label(ui, theme, "Modified");
                ui.label(
                    egui::RichText::new(modified_relative)
                        .size(10.0)
                        .color(theme.text_secondary),
                );
                ui.end_row();
            }

            if let Ok(target) = std::fs::read_link(&plugin.path) {
                detail_label(ui, theme, "Symlink");
                let resolved = if target.is_absolute() {
                    target
                } else {
                    plugin
                        .path
                        .parent()
                        .map(|p| p.join(&target))
                        .unwrap_or(target)
                };
                ui.label(
                    egui::RichText::new(resolved.to_string_lossy())
                        .size(10.0)
                        .monospace()
                        .color(theme.text_secondary),
                );
                ui.end_row();
            }
        });

    // ── Control parameters ──────────────────────────────────────────
    if !plugin.control_inputs.is_empty() {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!(
                "CONTROLS ({})",
                plugin.control_inputs.len()
            ))
            .size(9.0)
            .strong()
            .color(theme.text_dim),
        );
        ui.add_space(2.0);

        for port in &plugin.control_inputs {
            let port_name = if port.name.is_empty() {
                format!("port {}", port.index)
            } else {
                port.name.clone()
            };
            let unit_suffix = match klipp_audio::ladspa::unit_hint_for_port_name(&port_name) {
                klipp_audio::dsp::ParamUnit::Db => " dB",
                klipp_audio::dsp::ParamUnit::Hz => " Hz",
                klipp_audio::dsp::ParamUnit::Ms => " ms",
                klipp_audio::dsp::ParamUnit::Percent => "%",
                klipp_audio::dsp::ParamUnit::Ratio => ":1",
                klipp_audio::dsp::ParamUnit::None => "",
            };
            let mut flags = Vec::new();
            if port.is_toggle {
                flags.push("toggle");
            } else if port.is_integer {
                flags.push("integer");
            }
            if port.is_logarithmic {
                flags.push("log");
            }
            let flag_suffix = if flags.is_empty() {
                String::new()
            } else {
                format!("   ({})", flags.join(", "))
            };

            let range_text = if port.is_toggle {
                format!("default {}", if port.default >= 0.5 { "on" } else { "off" })
            } else {
                let dec = if port.is_integer {
                    0
                } else if (port.max - port.min).abs() > 100.0 {
                    1
                } else {
                    2
                };
                format!(
                    "{:.*}{} – {:.*}{}   default {:.*}{}",
                    dec, port.min, unit_suffix,
                    dec, port.max, unit_suffix,
                    dec, port.default, unit_suffix,
                )
            };

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&port_name)
                        .size(10.0)
                        .color(theme.text_primary),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{}{}", range_text, flag_suffix))
                            .size(9.0)
                            .color(theme.text_dim),
                    );
                });
            });
        }
    } else {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("(no control parameters)")
                .size(9.0)
                .color(theme.text_dim),
        );
    }
}

fn detail_label(ui: &mut egui::Ui, theme: &KlippTheme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(9.0)
            .color(theme.text_dim),
    );
}

/// Humanise a byte count for the file-size detail row. Mirrors
/// what's used elsewhere (cache manager, AI Models footer) without
/// requiring a cross-crate import.
fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

fn render_search_paths(ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme) {
    ui.label(
        egui::RichText::new("SEARCH PATHS")
            .size(10.0)
            .strong()
            .color(theme.text_dim),
    );
    ui.add_space(4.0);

    let paths = ladspa_search_paths();
    for path in &paths {
        let exists = path.exists();
        let count = if exists {
            state
                .ladspa_plugins
                .iter()
                .filter(|p| p.path.starts_with(path))
                .count()
        } else {
            0
        };
        ui.horizontal(|ui| {
            let dot_color = if exists && count > 0 {
                theme.accent
            } else if exists {
                theme.text_secondary
            } else {
                theme.text_dim
            };
            ui.colored_label(dot_color, "●");
            ui.label(
                egui::RichText::new(path.to_string_lossy())
                    .size(10.0)
                    .color(theme.text_secondary),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let suffix = if !exists {
                    "not present".to_string()
                } else if count == 0 {
                    "empty".to_string()
                } else {
                    format!("{count} plugins")
                };
                ui.label(
                    egui::RichText::new(suffix)
                        .size(10.0)
                        .color(theme.text_dim),
                );
            });
        });
    }

    if std::env::var("LADSPA_PATH").is_err() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Tip: set $LADSPA_PATH (colon-separated) to add custom plugin directories.",
            )
            .size(9.0)
            .color(theme.text_dim),
        );
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn format_relative_time(secs_ago: u64) -> String {
    if secs_ago < 5 {
        "just now".to_string()
    } else if secs_ago < 60 {
        format!("{secs_ago} s ago")
    } else if secs_ago < 3600 {
        format!("{} min ago", secs_ago / 60)
    } else if secs_ago < 86_400 {
        format!("{} h ago", secs_ago / 3600)
    } else {
        format!("{} d ago", secs_ago / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_buckets() {
        assert_eq!(format_relative_time(0), "just now");
        assert_eq!(format_relative_time(3), "just now");
        assert_eq!(format_relative_time(10), "10 s ago");
        assert_eq!(format_relative_time(59), "59 s ago");
        assert_eq!(format_relative_time(60), "1 min ago");
        assert_eq!(format_relative_time(3599), "59 min ago");
        assert_eq!(format_relative_time(3600), "1 h ago");
        assert_eq!(format_relative_time(86_399), "23 h ago");
        assert_eq!(format_relative_time(86_400), "1 d ago");
        assert_eq!(format_relative_time(7 * 86_400), "7 d ago");
    }

    #[test]
    fn format_bytes_buckets() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
        // ~227 KB — typical .so size for a single-plugin LADSPA library.
        assert_eq!(format_bytes(232_000), "226.6 KB");
    }

    #[test]
    fn search_paths_includes_standard_dirs() {
        // Save / clear LADSPA_PATH so the test is deterministic regardless
        // of the dev's environment.
        let saved = std::env::var("LADSPA_PATH").ok();
        // SAFETY: tests run single-threaded by default for env mutation;
        // setting unsafe is required on Rust 2024 anyway.
        unsafe { std::env::remove_var("LADSPA_PATH"); }
        let paths = ladspa_search_paths();
        let strs: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(strs.iter().any(|s| s == "/usr/lib/ladspa"));
        assert!(strs.iter().any(|s| s == "/usr/local/lib/ladspa"));
        assert!(strs.iter().any(|s| s == "/usr/lib64/ladspa"));
        assert!(strs.iter().any(|s| s.ends_with("/.ladspa")));
        if let Some(v) = saved {
            unsafe { std::env::set_var("LADSPA_PATH", v); }
        }
    }
}
