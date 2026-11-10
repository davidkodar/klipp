//! Appearance: the UI scale and the system theme followed (#890).

use super::*;

/// How long a window's size, or its being maximised, holds before the
/// interface follows it. A file dialog, or the compositor making room for
/// one, can shrink the window for a moment: Auto made the interface 80%
/// for it, put up the small-screen note, and saved the window as not
/// maximised for the next start.
pub(crate) const SETTLE_SECS: f64 = 1.5;

/// A value that counts once it has held for [`SETTLE_SECS`].
#[derive(Default)]
pub(crate) struct Settling<T> {
    seen: Option<(T, f64)>,
}

impl<T: PartialEq + Copy> Settling<T> {
    /// Whether `value`, seen at `now` (seconds), has held for
    /// [`SETTLE_SECS`] since it was first seen.
    pub(crate) fn held(&mut self, value: T, now: f64) -> bool {
        match self.seen {
            Some((v, since)) if v == value => now - since >= SETTLE_SECS,
            _ => {
                self.seen = Some((value, now));
                false
            }
        }
    }
}

/// The interface size last worked out, and the window size it waits on.
#[derive(Default)]
pub(crate) struct UiScale {
    /// The preference and the window size (points at the system scale) the
    /// size was last worked out for, so it is set once per change rather
    /// than every frame (and never fights a zoom made another way).
    applied: Option<(u32, u32, u32)>,
    /// The window's size, counting once it holds.
    settling: Settling<(u32, u32)>,
}

/// What [`UiScale::step`] asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ScaleStep {
    /// Nothing has changed.
    Same,
    /// A new window size, not yet held: ask again after [`SETTLE_SECS`].
    Wait,
    /// The interface at this zoom.
    Set(f32),
}

impl UiScale {
    /// The interface size for preference `pct` (0 is Auto) in a window
    /// `size` points at the system scale, seen at `now`. Auto follows the
    /// window both ways: smaller for a small one, and back up when it is
    /// large again, as a tiling compositor gives a window half the screen
    /// beside a dialog and then the whole of it back. A size chosen stays.
    pub(crate) fn step(&mut self, pct: u32, size: egui::Vec2, now: f64) -> ScaleStep {
        let key = (pct, size.x.round() as u32, size.y.round() as u32);
        if self.applied == Some(key) {
            return ScaleStep::Same;
        }
        // A new size waits until it holds, but the first one, at the start,
        // and a size chosen in the View menu apply at once.
        let chosen = self.applied.is_none_or(|(applied, _, _)| applied != pct);
        if !chosen && !self.settling.held((key.1, key.2), now) {
            return ScaleStep::Wait;
        }
        self.applied = Some(key);
        ScaleStep::Set(if pct == 0 { super::auto_zoom_factor(size) } else { pct as f32 / 100.0 })
    }
}

impl KlippApp {
    /// Apply the interface size: a View menu choice is saved first, then
    /// the preference is set on the context whenever it or the screen
    /// changes. Auto follows the system scale, and adds a quarter on a
    /// screen wider than 2800 points at that scale — a 4K panel at 1x or
    /// 1.25x, where the defaults read as small print.
    pub(super) fn apply_ui_scale(&mut self, ctx: &egui::Context) {
        if let Some(pct) = self.state.ui_scale_request.take() {
            self.prefs.appearance.ui_scale_percent = pct;
            let _ = self.prefs.save();
        }
        let pct = self.prefs.appearance.ui_scale_percent;
        self.state.ui_scale_percent = pct;
        // Sizes in points shrink as the zoom grows; times the zoom they
        // are the screen at the system scale, which is what Auto reads,
        // and what keeps this key the same once the zoom is set.
        // Whether the window is maximised, kept for the next start (#2944);
        // not in the first seconds, while the start's own maximise lands.
        let now = ctx.input(|i| i.time);
        if let Some(maximized) = ctx.input(|i| (i.time > 3.0).then(|| i.viewport().maximized).flatten()) {
            if maximized != self.prefs.general.open_maximized {
                if self.maximized_settling.held(maximized, now) {
                    self.prefs.general.open_maximized = maximized;
                    let _ = self.prefs.save();
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(SETTLE_SECS));
                }
            }
        }
        // The window, not the monitor (#2944): a small window on a large
        // screen is small, and was given the large screen's 125%.
        let zoom = ctx.zoom_factor();
        let size = ctx.input(|i| i.screen_rect().size()) * zoom;
        self.state.small_window = size.x < SMALL_WINDOW.x || size.y < SMALL_WINDOW.y;
        let want = match self.ui_scale.step(pct, size, now) {
            ScaleStep::Same => return,
            ScaleStep::Wait => {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(SETTLE_SECS));
                return;
            }
            ScaleStep::Set(want) => want,
        };
        tracing::debug!("interface size: {pct}% (0 = Auto), window {:.0}x{:.0} pt at the system scale (zoom {zoom}), zoom {want}", size.x, size.y);
        if (zoom - want).abs() > 0.001 {
            ctx.set_zoom_factor(want);
        }
        if crate::welcome::small_screen_note_now(&mut self.state, &mut self.prefs.appearance, want) {
            let _ = self.prefs.save();
        }
    }

    /// Keep the system-theme watcher in step with the preference and
    /// apply a changed Omarchy palette or desktop portal setting. Runs
    /// once per frame; cheap unless a flag is set.
    pub(super) fn sync_system_theme(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;

        if self.desktop_appearance.take_changed() {
            let status = self.desktop_appearance.status();
            self.theme_registry.set_portal(status);
            // The portal is the fallback: its dark or light applies only
            // where there is no full palette; its accent wherever it is
            // followed.
            let a = &self.prefs.appearance;
            if (a.follow_system_theme && self.theme_registry.omarchy_name().is_none()) || a.follow_system_accent {
                self.theme = KlippTheme::from_prefs(&self.prefs.appearance, &self.theme_registry);
                self.theme.apply_to_egui(ctx);
            }
        }

        let follow = self.prefs.appearance.follow_system_theme
            && self.theme_registry.omarchy_name().is_some();
        match (follow, self.system_theme_watcher.is_some()) {
            (true, false) => {
                if let Some(state) = crate::theme::omarchy::detect() {
                    self.system_theme_watcher = crate::theme::omarchy::start_watcher(
                        &state,
                        std::sync::Arc::clone(&self.system_theme_changed),
                        ctx.clone(),
                    );
                    // The desktop may have changed since the registry
                    // was built (before the watcher existed).
                    self.system_theme_changed.store(true, Ordering::Release);
                }
            }
            (false, true) => {
                self.system_theme_watcher = None;
                self.system_theme_changed.store(false, Ordering::Release);
            }
            _ => {}
        }

        if !self.system_theme_changed.swap(false, Ordering::AcqRel) {
            return;
        }
        match self.theme_registry.reload_omarchy() {
            Some(name) => {
                tracing::info!("System theme: now following Omarchy '{name}'");
                if follow {
                    self.theme = KlippTheme::from_prefs(&self.prefs.appearance, &self.theme_registry);
                    self.theme.apply_to_egui(ctx);
                }
            }
            None => {
                // Mid-switch (the directory is being rebuilt) or an
                // unparseable palette: keep the previous theme and
                // look again next frame.
                self.system_theme_changed.store(true, Ordering::Release);
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
    }
}

