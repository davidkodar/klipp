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
//! * [`panels`] — timeline, viewer, transport, toolbar, inspector,
//!   browser, launch screen, modal dialogs.
//! * [`inspector`] — the new per-clip inspector (video / audio /
//!   effects / info tabs). The older top-level `panels::inspector` is
//!   now just the static colour-and-luts helper used from here.
//! * [`browser`] — media browser with thumbnails, filmstrips, skim
//!   preview, bin tree, tags, sort.
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
pub mod audio_sync;
pub mod browser;
pub mod cache_manager;
pub mod drag_drop;
pub mod history;
pub mod icons;
pub mod inspector;
pub mod keybindings;
pub mod layout_state;
pub(crate) mod layout;
pub mod media_watcher;
pub mod panels;
pub mod prefs;
pub mod presets;
pub mod effect_presets;
pub mod state;
pub mod theme;
pub mod timeline;
pub mod ui_util;
pub mod widgets;

pub use app::KlippApp;
pub use prefs::Preferences;
