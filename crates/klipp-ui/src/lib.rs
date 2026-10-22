//! The Klipp desktop UI: the entire editor surface.
//!
//! This is by far the largest crate in the workspace — it binds every
//! other klipp-* crate into a running egui application. [`app::KlippApp`]
//! implements `eframe::App`, drains engine events into UI state, and
//! dispatches user commands back to the engine.
//!
//! # Module map
//!
//! * [`app`] — `KlippApp` + the main `update()` loop. Per-frame flow:
//!   drain events → build panel layout → render each panel → apply
//!   returned commands.
//! * [`state`] — `AppState`, the large UI state struct (playback,
//!   selection, viewer, thumbnails, skim, colour workspace, …).
//! * [`history`] — undo/redo stack on top of `EditCommand`. Drag
//!   coalescing via `begin_drag` / `end_drag`.
//! * [`panels`] — timeline, viewer, transport, toolbar, launch
//!   screen, preferences and the modal dialogs.
//! * [`inspector`] — the per-clip inspector (video / audio / effects /
//!   title / info tabs).
//! * [`browser`] — media browser with thumbnails, filmstrips, skim
//!   preview, bin tree, tags, sort, and the effect, title, transition,
//!   generator and audio libraries.
//! * [`timeline`] — the timeline subsystem inside a panel:
//!   ruler, tracks, clips, filmstrips, drags, context menus.
//! * [`widgets`] — reusable egui primitives (scopes, jog wheel,
//!   scrubby slider, curves editor, timecode edit, toggle, …).
//! * [`prefs`] — persisted user preferences (`preferences.json`).
//! * [`ui_util`] — small egui helpers shared across panels.
//!
//! # Concurrency
//!
//! The UI thread owns `AppState` and `KlippApp`. It does **not**
//! block. All heavy work (decode, thumbnail generation, filmstrip
//! generation, export, scene detect, stabilise, …) runs on background
//! threads and reports back through crossbeam channels.
//!
//! # Conventions
//!
//! * Shared state between threads goes through `Arc<Mutex<_>>` or
//!   `Arc<RwLock<_>>`. Lock-poison is recovered from via
//!   `.unwrap_or_else(|e| e.into_inner())` so a worker panic doesn't
//!   cascade into a UI freeze.
//! * The engine never knows about egui. All communication flows
//!   through `AppCommand` / `AppEvent` (defined in `klipp-core`).

pub mod app;
pub mod project_paths;
pub mod auto_align;
pub mod audio_plugins_disabled;
pub mod browser;
pub mod effect_targets;
pub mod selection;
pub mod edit_helpers;
pub mod timeline_sync;
pub mod punch_in;
pub mod clip_clipboard;
pub mod source_edit;
pub mod trim_view;
pub mod ai_tools;
pub mod assistant_clients;
pub mod cache_manager;
pub mod cli;
mod clip_edits;
pub mod command_palette;
pub mod drag_drop;
pub mod file_dialog;
pub mod model_download;
/// The edit history lives in the document crate now; the UI's paths
/// (`crate::history::…`) still resolve.
pub use klipp_document::history;
pub mod cut_templates;
pub mod hyperzoom;
pub mod hyperzoom_presets;
pub mod grade_library;
pub mod shared_library;
pub mod subject_track;
pub mod graded_thumbs;
pub mod icons;
pub mod inspector;
pub mod keybindings;
mod keymap_presets;
pub mod layout_state;
pub(crate) mod layout;
pub mod media_watcher;
pub mod multi_edit;
pub mod multicam_grid;
pub mod navigation;
pub mod panels;
pub mod picture_mattes;
pub mod mask_edit;
pub mod mask_tracks;
pub mod key_selection;
pub mod param_focus;
pub mod aaf_install;
pub mod parameterized_templates;
pub mod templates_tab;
pub mod prefs;
pub mod presets;
pub mod effect_presets;
pub mod export_hook;
pub mod title_templates;
pub mod credits_dialog;
pub mod music_fit_dialog;
pub mod normalize_dialog;
pub mod visual_search_dialog;
pub mod missing_fonts;
pub mod import_report;
pub mod state;
pub mod theme;
pub mod trash;
pub mod transcript;
pub mod timeline;
pub mod tracks_undo;
pub mod edit_point;
pub mod ui_util;
pub mod viewer_lut;
pub mod widgets;

pub use app::KlippApp;
pub use prefs::Preferences;

/// Resolve a libonnxruntime.so on disk and set `ORT_DYLIB_PATH`
/// so users don't have to. Called by `klipp-app::main` very
/// early — before any thread spawns or any ort code touches the
/// env. No-op when:
///
/// * `ORT_DYLIB_PATH` is already set (respects user override).
/// * The `ai-ort` feature isn't compiled in (stub generator only).
/// * No libonnxruntime is found at the candidate paths
///   (probe_resolve.rs's `find_libonnxruntime`).
///
/// Logs the resolved path at info level so users can see what
/// Klipp picked when they're debugging "why isn't GPU working".
#[cfg(feature = "ai-ort")]
pub fn auto_set_ort_dylib_path() {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    if let Some(path) = klipp_ml_ort::find_libonnxruntime() {
        // SAFETY: called from main, before any thread spawns.
        // Rust 2024 marked set_var unsafe because env mutation
        // is process-global and racy with reads from other
        // threads. We're single-threaded here.
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", &path);
        }
        tracing::info!(
            target: "klipp_ui",
            "ORT_DYLIB_PATH auto-set to {}",
            path.display(),
        );
    }
}

/// Stub when ort isn't in the build. Lets `main.rs` call the
/// function unconditionally without an explicit feature gate.
#[cfg(not(feature = "ai-ort"))]
pub fn auto_set_ort_dylib_path() {}

mod updates;
