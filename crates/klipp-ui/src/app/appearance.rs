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
        // Whether the window is maximised, kept for the next start (#2944);
        // not in the first seconds, while the start's own maximise lands.
        if let Some(maximized) = ctx.input(|i| (i.time > 3.0).then(|| i.viewport().maximized).flatten()) {
            if maximized != self.prefs.general.open_maximized {
                self.prefs.general.open_maximized = maximized;
                let _ = self.prefs.save();
            }
        }
        // The window, not the monitor (#2944): a small window on a large
        // screen is small, and was given the large screen's 125%.
        let zoom = ctx.zoom_factor();
        let size = ctx.input(|i| i.screen_rect().size()) * zoom;
        self.state.small_window = size.x < SMALL_WINDOW.x || size.y < SMALL_WINDOW.y;
        let key = (pct, size.x.round() as u32, size.y.round() as u32);
        if self.ui_scale_applied == Some(key) {
            return;
        }
        self.ui_scale_applied = Some(key);
        let want = if pct == 0 { auto_zoom_factor(size) } else { pct as f32 / 100.0 };
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
            if adopt_portal_answer(&mut self.prefs.appearance, &self.theme_registry) {
                let _ = self.prefs.save();
            }
            // The portal is the fallback: it applies only where there is
            // no full palette.
            if self.prefs.appearance.follow_system_theme && self.theme_registry.omarchy_name().is_none() {
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

/// A fresh install's first portal answer: follow the system theme when
/// the desktop prefers dark or light (#2625). Whether the preference
/// changed, to be saved.
fn adopt_portal_answer(appearance: &mut crate::prefs::AppearancePrefs, registry: &crate::theme::ThemeRegistry) -> bool {
    if !appearance.follow_system_theme_pending || registry.portal() == crate::theme::portal::PortalStatus::Looking {
        return false;
    }
    appearance.follow_system_theme_pending = false;
    if appearance.follow_system_theme || !registry.has_system_theme() {
        return false;
    }
    tracing::info!("Desktop prefers dark or light: following the system theme by default");
    appearance.follow_system_theme = true;
    true
}

#[cfg(test)]
mod portal_default_tests {
    use super::adopt_portal_answer;
    use crate::theme::portal::{Appearance, ColorScheme, PortalStatus};
    use crate::theme::ThemeRegistry;

    /// #2625: a fresh install follows the system theme once the portal
    /// says dark or light; with no preference, or no portal, it keeps
    /// its own theme. An install that was not fresh is left alone.
    #[test]
    fn a_fresh_install_follows_when_the_desktop_has_a_preference() {
        let fresh = || crate::prefs::AppearancePrefs { follow_system_theme_pending: true, ..Default::default() };
        let mut reg = ThemeRegistry::builtins_only();
        let mut prefs = fresh();
        assert!(!adopt_portal_answer(&mut prefs, &reg), "no answer yet");
        assert!(prefs.follow_system_theme_pending, "still waiting");
        reg.set_portal(PortalStatus::Found(Appearance { scheme: ColorScheme::Dark, accent: None }));
        assert!(adopt_portal_answer(&mut prefs, &reg));
        assert!(prefs.follow_system_theme && !prefs.follow_system_theme_pending);
        for status in [PortalStatus::Found(Appearance::default()), PortalStatus::Absent] {
            reg.set_portal(status);
            let mut prefs = fresh();
            assert!(!adopt_portal_answer(&mut prefs, &reg), "{status:?}");
            assert!(!prefs.follow_system_theme && !prefs.follow_system_theme_pending, "{status:?}");
        }
        reg.set_portal(PortalStatus::Found(Appearance { scheme: ColorScheme::Light, accent: None }));
        let mut kept = crate::prefs::AppearancePrefs::default();
        assert!(!adopt_portal_answer(&mut kept, &reg));
        assert!(!kept.follow_system_theme, "not a fresh install");
    }
}
