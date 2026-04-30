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

                render_plugin_row(ui, theme, plugin);
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
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "{} detected   |   Last scanned: {}",
                state.ladspa_plugins.len(),
                last_scanned
            ))
            .size(10.0)
            .color(theme.text_dim),
        );
    });
}

fn render_plugin_row(ui: &mut egui::Ui, theme: &KlippTheme, plugin: &LadspaPluginInfo) {
    let display = if plugin.name.is_empty() {
        &plugin.label
    } else {
        &plugin.name
    };
    egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0_f32, theme.border))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .outer_margin(egui::Margin::symmetric(0.0, 1.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(display)
                            .size(11.0)
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.label(
                        egui::RichText::new(plugin.path.to_string_lossy())
                            .size(9.0)
                            .color(theme.text_dim),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let suffix = if plugin.audio_inputs == 1 && plugin.audio_outputs == 1 {
                        "mono"
                    } else if plugin.audio_inputs == 2 && plugin.audio_outputs == 2 {
                        "stereo"
                    } else if plugin.audio_inputs == 1 && plugin.audio_outputs == 2 {
                        "mono → stereo"
                    } else {
                        "multi"
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "{} | {} ports",
                            suffix, plugin.port_count
                        ))
                        .size(9.0)
                        .color(theme.text_dim),
                    );
                });
            });
        });
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
