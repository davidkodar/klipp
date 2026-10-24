//! System Probe — single-shot diagnostic snapshot for bug reports.
//!
//! Aggregates state that's already exposed by various subsystems
//! (klipp_ml_ort runtime resolve, ffmpeg-next version, lspci, OS
//! release file) into one read-only dialog. The "Copy report" button
//! produces a plain-text summary the user can paste into an issue.

use crate::state::AppState;
use crate::theme::KlippTheme;

/// Render the System Probe modal. No mutating commands — purely
/// informational, so we don't take a `cmds` Vec.
pub fn show_system_probe_dialog(
    ctx: &egui::Context,
    state: &mut AppState,
    theme: &KlippTheme,
) {
    if !state.show_system_probe {
        return;
    }

    let screen_rect = ctx.screen_rect();
    let max_h = (screen_rect.height() * 0.85).clamp(400.0, 720.0);
    let max_w = (screen_rect.width() * 0.70).clamp(520.0, 720.0);
    let default_w = (screen_rect.width() * 0.50).clamp(520.0, max_w);
    let default_h = (screen_rect.height() * 0.65).clamp(400.0, max_h);
    let centre = screen_rect.center();
    let default_pos = egui::pos2(
        centre.x - default_w * 0.5,
        centre.y - default_h * 0.5,
    );

    let report = collect_report();
    let mut open = true;
    egui::Window::new("System Probe")
        .open(&mut open)
        .resizable(true)
        .movable(true)
        .default_size([default_w, default_h])
        .default_pos(default_pos)
        .min_width(440.0)
        .min_height(360.0)
        .max_width(max_w)
        .max_height(max_h)
        .show(ctx, |ui| {
            render_body(ui, theme, &report, max_h);
        });

    if !open {
        state.show_system_probe = false;
    }
}

fn render_body(ui: &mut egui::Ui, theme: &KlippTheme, report: &Report, max_h: f32) {
    // Header row: copy-to-clipboard.
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(
                "Snapshot of host hardware + Klipp build state. \
                 Paste into bug reports.",
            )
            .size(10.0)
            .color(theme.text_dim),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Copy report").on_hover_text(
                "Copy the full report to the clipboard as plain text."
            ).clicked() {
                ui.ctx().copy_text(report.as_text());
            }
        });
    });

    ui.add_space(6.0);
    ui.separator();
    ui.add_space(6.0);

    // Body — scrollable list of section grids.
    let other_budget = 80.0; // header + footer + padding
    let scroll_max_h = (max_h - other_budget).max(200.0);
    egui::ScrollArea::vertical()
        .max_height(scroll_max_h)
        .show(ui, |ui| {
            for section in &report.sections {
                render_section(ui, theme, section);
            }
        });
}

fn render_section(ui: &mut egui::Ui, theme: &KlippTheme, section: &ReportSection) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(&section.title)
            .size(10.0)
            .strong()
            .color(theme.text_dim),
    );
    ui.add_space(2.0);

    egui::Grid::new(("system_probe_section", &section.title))
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for (key, value) in &section.rows {
                ui.label(
                    egui::RichText::new(key)
                        .size(10.0)
                        .color(theme.text_dim),
                );
                ui.label(
                    egui::RichText::new(value)
                        .size(10.0)
                        .color(theme.text_secondary),
                );
                ui.end_row();
            }
        });
}

// ─── Data collection ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Report {
    pub sections: Vec<ReportSection>,
}

#[derive(Debug, Clone)]
pub struct ReportSection {
    pub title: String,
    pub rows: Vec<(String, String)>,
}

impl Report {
    /// Plain-text rendering for clipboard paste / bug reports. Each
    /// section gets a header in `[brackets]` and indented rows.
    pub fn as_text(&self) -> String {
        let mut out = String::new();
        out.push_str("Klipp System Probe\n");
        out.push_str("==================\n\n");
        for section in &self.sections {
            out.push_str(&format!("[{}]\n", section.title));
            for (k, v) in &section.rows {
                out.push_str(&format!("  {:<24} {}\n", k, v));
            }
            out.push('\n');
        }
        out
    }
}

/// Collect every probe value into a single Report. Each subsystem's
/// helper is best-effort: if a probe fails, we record the failure
/// in the row value rather than throwing the whole report.
pub fn collect_report() -> Report {
    Report {
        sections: vec![
            klipp_section(),
            os_section(),
            cpu_section(),
            gpu_section(),
            ort_section(),
            audio_section(),
        ],
    }
}

fn klipp_section() -> ReportSection {
    let mut rows = Vec::new();
    rows.push((
        "Version".to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    ));
    let mut features: Vec<&'static str> = Vec::new();
    if cfg!(feature = "ai-ort") {
        features.push("ai-ort");
    }
    if cfg!(feature = "ai-ort-cuda") {
        features.push("ai-ort-cuda");
    }
    if cfg!(feature = "ai-candle") {
        features.push("ai-candle");
    }
    if cfg!(feature = "ai-candle-cuda") {
        features.push("ai-candle-cuda");
    }
    rows.push((
        "Build features".to_string(),
        if features.is_empty() {
            "(none)".to_string()
        } else {
            features.join(", ")
        },
    ));
    rows.push((
        "Build profile".to_string(),
        if cfg!(debug_assertions) {
            "debug".to_string()
        } else {
            "release".to_string()
        },
    ));
    ReportSection {
        title: "Klipp".into(),
        rows,
    }
}

fn os_section() -> ReportSection {
    let mut rows = Vec::new();
    let kernel = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "(unreadable)".to_string());
    rows.push(("Kernel".into(), kernel));

    // Distro from /etc/os-release.
    let distro = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "(unreadable)".to_string());
    rows.push(("Distribution".into(), distro));

    // Wayland / X11 / unknown — derived from env vars rather than
    // probing the compositor (cheap, deterministic, no IPC).
    let session_type = std::env::var("XDG_SESSION_TYPE")
        .ok()
        .unwrap_or_else(|| {
            if std::env::var("WAYLAND_DISPLAY").is_ok() {
                "wayland".into()
            } else if std::env::var("DISPLAY").is_ok() {
                "x11".into()
            } else {
                "(unknown)".into()
            }
        });
    rows.push(("Session".into(), session_type));

    ReportSection {
        title: "OS".into(),
        rows,
    }
}

fn cpu_section() -> ReportSection {
    let mut rows = Vec::new();
    rows.push((
        "Logical cores".to_string(),
        std::thread::available_parallelism()
            .map(|n| n.get().to_string())
            .unwrap_or_else(|_| "(unknown)".to_string()),
    ));

    // /proc/cpuinfo model name, first occurrence.
    let model = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("model name"))
                .and_then(|rest| rest.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| "(unknown)".to_string());
    rows.push(("CPU model".into(), model));

    // Total memory from /proc/meminfo (MemTotal: kB).
    let mem_total = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .map(|kb| format!("{:.1} GB", kb as f64 / (1024.0 * 1024.0)))
        .unwrap_or_else(|| "(unreadable)".to_string());
    rows.push(("RAM".into(), mem_total));

    ReportSection {
        title: "CPU".into(),
        rows,
    }
}

#[cfg(feature = "ai-ort")]
fn gpu_section() -> ReportSection {
    let mut rows = Vec::new();
    let vendor = klipp_ml_ort::detect_gpu_vendor();
    rows.push(("Vendor".into(), format!("{vendor:?}")));

    // Lspci VGA + 3D devices for full device strings.
    let pci = std::process::Command::new("lspci")
        .args(["-d", "::0300"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let devices: Vec<String> = pci
        .lines()
        .map(|l| {
            l.splitn(3, ' ')
                .nth(2)
                .unwrap_or(l)
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();
    let device_str = if devices.is_empty() {
        "(none)".to_string()
    } else {
        devices.join("\n  ")
    };
    rows.push(("Devices".into(), device_str));

    // NVIDIA driver version when present.
    if let Ok(driver) = std::fs::read_to_string("/proc/driver/nvidia/version") {
        if let Some(line) = driver.lines().next() {
            // The first line is verbose ("NVRM version: NVIDIA UNIX
            // x86_64 Kernel Module 545.29.06"); pull out just the
            // semver-ish version number.
            let version = line
                .split("Kernel Module")
                .nth(1)
                .map(|rest| {
                    rest.split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string()
                })
                .unwrap_or_else(|| "(unparsed)".into());
            rows.push(("NVIDIA driver".into(), version));
        }
    }

    ReportSection {
        title: "GPU".into(),
        rows,
    }
}

#[cfg(not(feature = "ai-ort"))]
fn gpu_section() -> ReportSection {
    ReportSection {
        title: "GPU".into(),
        rows: vec![(
            "Vendor".into(),
            "(klipp-ml-ort feature off — GPU probe unavailable)".into(),
        )],
    }
}

#[cfg(feature = "ai-ort")]
fn ort_section() -> ReportSection {
    let mut rows = Vec::new();
    let status = klipp_ml_ort::resolve_runtime_status();
    rows.push(("Status".into(), format!("{status:?}")));
    rows.push(("GPU provider".into(), klipp_ml_ort::provider::gpu_provider().unwrap_or("none").into()));
    if let Some(path) = klipp_ml_ort::find_libonnxruntime() {
        rows.push((
            "libonnxruntime".into(),
            path.to_string_lossy().to_string(),
        ));
    } else {
        rows.push(("libonnxruntime".into(), "(not found)".into()));
    }
    if let Ok(env_path) = std::env::var("ORT_DYLIB_PATH") {
        rows.push(("ORT_DYLIB_PATH".into(), env_path));
    }
    ReportSection {
        title: "ONNX Runtime (ort)".into(),
        rows,
    }
}

#[cfg(not(feature = "ai-ort"))]
fn ort_section() -> ReportSection {
    ReportSection {
        title: "ONNX Runtime (ort)".into(),
        rows: vec![(
            "Status".into(),
            "feature ai-ort not compiled in".into(),
        )],
    }
}

fn audio_section() -> ReportSection {
    let mut rows = Vec::new();

    // PipeWire detection — the user-systemd unit is the canonical signal.
    let pipewire = std::process::Command::new("pgrep")
        .arg("-x")
        .arg("pipewire")
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let pulseaudio = std::process::Command::new("pgrep")
        .arg("-x")
        .arg("pulseaudio")
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let backend = if pipewire {
        "PipeWire (active)"
    } else if pulseaudio {
        "PulseAudio (active)"
    } else {
        "(neither PipeWire nor PulseAudio detected — likely raw ALSA)"
    };
    rows.push(("Backend".into(), backend.into()));

    // ALSA card list — first line of /proc/asound/cards is enough
    // to know whether the kernel sees any audio device.
    let alsa_cards = std::fs::read_to_string("/proc/asound/cards")
        .ok()
        .map(|s| {
            let count = s
                .lines()
                .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
                .count();
            format!("{count} card(s)")
        })
        .unwrap_or_else(|| "(unreadable)".into());
    rows.push(("ALSA cards".into(), alsa_cards));

    ReportSection {
        title: "Audio".into(),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_text_has_all_sections() {
        let report = collect_report();
        let text = report.as_text();
        // Header line.
        assert!(text.starts_with("Klipp System Probe"));
        // Every section's title appears as `[Title]` somewhere.
        for section in &report.sections {
            let bracket = format!("[{}]", section.title);
            assert!(
                text.contains(&bracket),
                "expected section header {bracket:?} in:\n{text}"
            );
        }
    }

    #[test]
    fn klipp_section_includes_version() {
        let s = klipp_section();
        let version = s.rows.iter().find(|(k, _)| k == "Version");
        assert!(version.is_some(), "Version row missing");
        // Whatever cargo's package version is, the row value should
        // not be empty or "(unknown)".
        let v = &version.unwrap().1;
        assert!(!v.is_empty());
        assert_ne!(v, "(unknown)");
    }
}
