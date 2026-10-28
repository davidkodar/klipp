//! Shared egui layout helpers.

/// A panel's region, as [`allocate_clipped_ui`], whose widgets' ids are
/// rooted at `id` (#2962). A child `Ui` counts its widgets' ids on from
/// whatever its parent drew before it, so a status message or a progress
/// bar coming or going earlier in the frame gave every widget of a later
/// panel a new id: a field being typed in vanished mid-word, its value
/// was dropped and the next keys went to the timeline (Ctrl+A selected
/// every clip). Here they stay what they were.
pub fn panel_ui<R>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: impl std::hash::Hash,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let mut builder = egui::UiBuilder::new().max_rect(rect).layout(*ui.layout()).style(ui.style().clone());
    if !ui.is_enabled() {
        builder = builder.disabled();
    }
    let mut panel = egui::Ui::new(ui.ctx().clone(), ui.layer_id(), egui::Id::new(id), builder);
    panel.set_clip_rect(rect.intersect(ui.clip_rect()));
    add_contents(&mut panel)
}

/// Allocate a child UI region whose `max_rect` AND `clip_rect` are both set
/// to the requested rect (intersected with the parent clip rect).
///
/// Use this instead of `ui.allocate_new_ui(UiBuilder::new().max_rect(r), ...)`
/// followed by a manual `ui.set_clip_rect(...)`. It prevents child widgets
/// from painting outside their allocated rect — the root cause of the
/// historical status-bar bleed bugs.
pub fn allocate_clipped_ui<R>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let clipped_rect = rect.intersect(ui.clip_rect());
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.set_clip_rect(clipped_rect);
        add_contents(ui)
    })
    .inner
}

/// `s` cut to at most `max` characters, the last three a "...", or as it
/// is when it fits. Counted in characters, never bytes: slicing a name at
/// a byte count split a multi-byte letter (å, é) and panicked, which a
/// release build turns into a closed app (#822).
pub fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = s.chars().take(keep).collect();
    out.push_str("...");
    out
}

/// Decode a PNG into an `egui::ColorImage`, returning a 1×1 transparent
/// placeholder on any decode failure or unsupported pixel format.
///
/// Used for logo / icon assets where a broken or stripped PNG should not
/// crash the UI on open. The asset is baked in via `include_bytes!` today,
/// but a corrupted file in the tree (or a future move to runtime asset
/// loading) would otherwise take the whole app down at startup.
pub fn decode_png_or_fallback(bytes: &[u8]) -> egui::ColorImage {
    decode_png_inner(bytes)
}

/// An image of `size` from `rgba`, or `None`, said in the log as `what`,
/// when the bytes are not `size`'s. A rendered picture of the wrong size
/// (#1913) is a failed render, not a crash: egui's own constructor
/// panics on it.
pub fn checked_image(size: [usize; 2], rgba: &[u8], what: &str) -> Option<egui::ColorImage> {
    let want = size[0] * size[1] * 4;
    if rgba.len() != want || want == 0 {
        tracing::error!("{what}: {} bytes for a {}x{} picture, which takes {want}; not shown", rgba.len(), size[0], size[1]);
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba))
}

fn decode_png_inner(bytes: &[u8]) -> egui::ColorImage {
    fn fallback() -> egui::ColorImage {
        egui::ColorImage {
            size: [1, 1],
            pixels: vec![egui::Color32::TRANSPARENT],
        }
    }

    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let Ok(mut reader) = decoder.read_info() else {
        tracing::warn!("decode_png_or_fallback: PNG header read failed");
        return fallback();
    };
    // png 0.18 returns Option<usize> here — None when the buffer
    // size would overflow usize (huge dimensions). Treat that as a
    // decode failure and fall back to the placeholder.
    let Some(buf_size) = reader.output_buffer_size() else {
        tracing::warn!("decode_png_or_fallback: PNG dimensions overflow buffer size");
        return fallback();
    };
    let mut buf = vec![0u8; buf_size];
    let Ok(info) = reader.next_frame(&mut buf) else {
        tracing::warn!("decode_png_or_fallback: PNG frame decode failed");
        return fallback();
    };
    let bytes = &buf[..info.buffer_size()];
    let pixels: Vec<egui::Color32> = match info.color_type {
        png::ColorType::Rgba => bytes
            .chunks_exact(4)
            .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
            .collect(),
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .map(|c| egui::Color32::from_rgb(c[0], c[1], c[2]))
            .collect(),
        other => {
            tracing::warn!(
                "decode_png_or_fallback: unsupported PNG colour type {:?}",
                other
            );
            return fallback();
        }
    };
    egui::ColorImage {
        size: [info.width as usize, info.height as usize],
        pixels,
    }
}

/// Open `target` (a URL, a file or a folder) in the desktop's default
/// application, through `xdg-open`. Errors only when it could not start.
pub fn open_external(target: impl AsRef<std::ffi::OsStr>) -> std::io::Result<()> {
    spawn_reaped(std::process::Command::new("xdg-open").arg(target)).map(|_| ())
}

/// `path` as a `file://` URI for the desktop's opener, when it is an
/// absolute path to a regular file: what "Open in External App" hands
/// over, so nothing a project names can reach the opener as an option, a
/// folder or another kind of target. Bytes outside the unreserved set and
/// `/` are percent-encoded.
pub fn file_uri(path: &std::path::Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    if !path.is_absolute() || !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    Some(uri)
}

/// Start `cmd` with a null stdin and wait for it on a short-lived thread,
/// so it never shares the terminal the app was started from (#1767) and
/// never lingers as a zombie once it exits (#1785). Returns its pid.
pub fn spawn_reaped(cmd: &mut std::process::Command) -> std::io::Result<u32> {
    let mut child = cmd.stdin(std::process::Stdio::null()).spawn()?;
    let pid = child.id();
    let reaper = std::thread::Builder::new().name("reap-child".into()).spawn(move || {
        let _ = child.wait();
    });
    if let Err(e) = reaper {
        tracing::warn!("could not start a thread to reap pid {pid}: {e}");
    }
    Ok(pid)
}

#[cfg(test)]
mod tests {

    /// Only an absolute path to a regular file becomes a URI, every odd
    /// byte encoded: a folder, a relative or option-like name, and a path
    /// that isn't there give none.
    #[test]
    fn only_a_regular_file_is_handed_to_the_opener() {
        let dir = std::env::temp_dir().join(format!("klipp-file-uri-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let odd = dir.join("a b#c%d.mov");
        std::fs::write(&odd, b"x").unwrap();
        let uri = file_uri(&odd).expect("a file");
        assert!(uri.starts_with("file:///") && uri.ends_with("/a%20b%23c%25d.mov"), "{uri}");
        assert_eq!(file_uri(&dir), None, "a folder");
        assert_eq!(file_uri(std::path::Path::new("--version")), None, "an option-like name");
        assert_eq!(file_uri(&dir.join("gone.mov")), None, "a path that isn't there");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #822: names are cut by characters, so a multi-byte letter at the
    /// cut point is kept whole instead of panicking.
    #[test]
    fn elide_cuts_by_characters_not_bytes() {
        assert_eq!(super::elide("short", 14), "short");
        assert_eq!(super::elide("Göteborg kväll vid hamnen", 14), "Göteborg kv...");
        assert_eq!(super::elide(&format!("{}é tail", "a".repeat(10)), 14), "aaaaaaaaaaé...");
        assert_eq!(super::elide("ååååååååååååååå", 10).chars().count(), 10);
    }

    use super::*;

    // ═══════════════════════════════════════════════════════════════
    // PANIC-VIEWER regression.
    //
    // The original tracker entry pointed at three sites
    // (viewer.rs:276-289, toolbar.rs:193, launch_screen.rs:70) that
    // used `decoder.read_info().expect("...")` on baked-in PNG
    // assets. If the asset pipeline ever stripped a PNG (or a
    // corrupt file landed in a future runtime asset path), startup
    // would crash. All three sites now route through
    // decode_png_or_fallback, which returns a 1×1 transparent
    // placeholder on every PNG-error path. These tests pin that
    // contract so a future tightening can't silently re-introduce
    // the panic.
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn decode_png_recovers_from_garbage_bytes() {
        // Random non-PNG bytes — must NOT panic, must return the
        // 1×1 transparent placeholder.
        let img = decode_png_or_fallback(b"this is not a PNG file at all");
        assert_eq!(img.size, [1, 1], "garbage must yield 1×1 placeholder");
        assert_eq!(img.pixels.len(), 1);
        assert_eq!(img.pixels[0], egui::Color32::TRANSPARENT);
    }

    #[test]
    fn decode_png_recovers_from_empty_bytes() {
        // Empty buffer — header read fails immediately.
        let img = decode_png_or_fallback(&[]);
        assert_eq!(img.size, [1, 1]);
        assert_eq!(img.pixels[0], egui::Color32::TRANSPARENT);
    }

    #[test]
    fn decode_png_recovers_from_truncated_png() {
        // Valid PNG signature but the rest is missing — read_info
        // succeeds far enough to advance, then next_frame hits the
        // truncated body.
        let mut bytes = vec![137, 80, 78, 71, 13, 10, 26, 10]; // PNG magic
        bytes.extend_from_slice(b"truncated junk after the magic");
        let img = decode_png_or_fallback(&bytes);
        assert_eq!(img.size, [1, 1],
            "truncated PNG must fall through to placeholder, not panic");
    }

    #[test]
    fn decode_png_loads_a_valid_2x2_rgba_png() {
        // Build a minimal 2×2 RGBA PNG inline so the test doesn't
        // depend on any on-disk asset. Sanity: the happy path
        // returns the right dimensions, not the placeholder.
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("write header");
            // 4 RGBA pixels: red, green, blue, white.
            writer.write_image_data(&[
                255, 0, 0, 255,
                0, 255, 0, 255,
                0, 0, 255, 255,
                255, 255, 255, 255,
            ]).expect("write image data");
        }
        let img = decode_png_or_fallback(&buf);
        assert_eq!(img.size, [2, 2], "valid PNG must decode at full size");
        assert_eq!(img.pixels.len(), 4);
        // Spot-check the red pixel.
        assert_eq!(img.pixels[0].r(), 255);
        assert_eq!(img.pixels[0].g(), 0);
    }

    /// A process's state letter from /proc, or None once it is gone.
    #[cfg(target_os = "linux")]
    fn proc_state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit(") ").next()?.chars().next()
    }

    /// A child started through `spawn_reaped` is gone after it exits,
    /// where one spawned and dropped stays behind as a zombie (#1785).
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(clippy::zombie_processes, reason = "the control leaves a zombie on purpose")]
    fn a_finished_child_is_reaped() {
        let settle = |pid: u32, want_gone: bool| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let state = proc_state(pid);
                if (want_gone && state.is_none()) || (!want_gone && state == Some('Z')) || std::time::Instant::now() > until {
                    return state;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        let pid = spawn_reaped(&mut std::process::Command::new("true")).unwrap();
        assert_eq!(settle(pid, true), None, "reaped child still listed");

        // The control: spawned and dropped, it stays a zombie.
        let dropped = std::process::Command::new("true").spawn().unwrap();
        let pid = dropped.id();
        drop(dropped);
        assert_eq!(settle(pid, false), Some('Z'), "the control should be a zombie");
    }

    /// #1913: bytes that are not the size's picture give no image, where
    /// egui's own constructor would panic.
    #[test]
    fn a_picture_of_the_wrong_size_gives_no_image() {
        assert!(super::checked_image([192, 108], &[], "test").is_none());
        assert!(super::checked_image([2, 2], &[0; 12], "test").is_none());
        let image = super::checked_image([2, 2], &[7; 16], "test").expect("the right size");
        assert_eq!(image.size, [2, 2]);
    }
}

#[cfg(test)]
mod panel_tests {
    /// #2962: every panel the layout places is rooted at an id of its own,
    /// so a panel shown or hidden before it (a curve editor, the browser's
    /// rail, a scope overlay) does not give its fields new ids mid-typing.
    #[test]
    fn every_layout_panel_has_ids_of_its_own() {
        let sources = [
            ("app/main_layout.rs", include_str!("app/main_layout.rs")),
            ("app/colour_workspace.rs", include_str!("app/colour_workspace.rs")),
            ("app/graph_dock.rs", include_str!("app/graph_dock.rs")),
        ];
        let mut ids: Vec<&str> = Vec::new();
        for (file, src) in sources {
            assert!(!src.contains(concat!("allocate_clipped", "_ui(")), "{file} places a panel whose ids count on from the panels before it");
            for at in src.match_indices("ui_util::panel_ui(").map(|(i, _)| i) {
                let id = src[at..].split('"').nth(1).expect("a panel id");
                assert!(!ids.contains(&id), "{id} is used twice");
                ids.push(id);
            }
        }
        for panel in ["toolbar_panel", "viewer_panel", "timeline_panel", "inspector_panel", "browser_panel", "status_bar_panel", "colour_tools_panel", "colour_nodes_panel"] {
            assert!(ids.contains(&panel), "{panel}: {ids:?}");
        }
    }
}
