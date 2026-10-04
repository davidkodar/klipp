//! Appearance: the UI scale and the system theme followed (#890).

use super::*;

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
        let zoom = ctx.zoom_factor();
        let (monitor, window) = ctx.input(|i| (i.viewport().monitor_size, i.screen_rect().width()));
        let width = monitor.map_or(window, |m| m.x.max(window)) * zoom;
        let key = (pct, width.round() as u32);
        if self.ui_scale_applied == Some(key) {
            return;
        }
        self.ui_scale_applied = Some(key);
        let want = if pct == 0 { auto_zoom_factor(width) } else { pct as f32 / 100.0 };
        tracing::debug!("interface size: {pct}% (0 = Auto), screen {width:.0} pt at the system scale (monitor {monitor:?}, window {window:.0}, zoom {zoom}), zoom {want}");
        if (zoom - want).abs() > 0.001 {
            ctx.set_zoom_factor(want);
        }
    }

    /// Keep the system-theme watcher in step with the preference and
    /// apply a changed Omarchy palette. Runs once per frame; cheap
    /// unless the flag is set.
    pub(super) fn sync_system_theme(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;

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
