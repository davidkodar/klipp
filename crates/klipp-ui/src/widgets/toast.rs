use crate::theme::KlippTheme;

/// Toast notification state — stored in AppState.
#[derive(Default)]
pub struct ToastState {
    message: Option<String>,
    /// A button beside the message and what it does (#2983).
    action: Option<(String, ToastAction)>,
    appear_time: Option<std::time::Instant>,
    /// Configurable visible duration (set from prefs.advanced.toast_duration_secs).
    pub visible_duration: f64,
}

/// What a toast's button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastAction {
    /// Undo the history step with this stamp, while it is still the last.
    UndoStep(u64),
    /// A trim stopped at its media's end: stretch the clip from that edge
    /// to where the pointer went instead (#3055).
    Stretch { clip_id: klipp_core::ClipId, at_end: bool, to: klipp_core::Timecode },
    /// A clip slowed by a stretch: draw its in-between frames by optical
    /// flow (#3055).
    SmoothMotion(klipp_core::ClipId),
}

/// How long a toast with a button stays at least, so it can be reached.
const ACTION_VISIBLE_SECS: f64 = 8.0;

const TOAST_FADE_IN: f64 = 0.15;
const TOAST_FADE_OUT: f64 = 0.2;

impl ToastState {
    #[cfg(test)]
    pub(crate) fn message_for_test(&self) -> Option<&str> { self.message.as_deref() }

    /// Show a new toast message. Replaces any existing toast.
    pub fn show(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
        self.action = None;
        self.appear_time = Some(std::time::Instant::now());
    }

    /// Show a message with a button labelled `label` that does `action`.
    pub fn show_with_action(&mut self, message: impl Into<String>, label: impl Into<String>, action: ToastAction) {
        self.show(message);
        self.action = Some((label.into(), action));
    }

    /// How long it stays fully shown.
    fn visible(&self) -> f64 {
        let v = self.visible_duration.max(0.5);
        if self.action.is_some() { v.max(ACTION_VISIBLE_SECS) } else { v }
    }

    /// Render the toast if active. Call every frame from the top-level
    /// update. Its button's action, when it was clicked.
    pub fn render(&self, ctx: &egui::Context, theme: &KlippTheme) -> Option<ToastAction> {
        let (msg, appear) = match (&self.message, self.appear_time) {
            (Some(m), Some(t)) => (m, t),
            _ => return None,
        };
        let mut clicked = None;

        let elapsed = appear.elapsed().as_secs_f64();
        let total_duration = TOAST_FADE_IN + self.visible() + TOAST_FADE_OUT;
        if elapsed > total_duration {
            return None;
        }

        // Calculate alpha
        let alpha = if elapsed < TOAST_FADE_IN {
            // Fade in
            (elapsed / TOAST_FADE_IN) as f32
        } else if elapsed < TOAST_FADE_IN + self.visible() {
            // Fully visible
            1.0
        } else {
            // Fade out
            let fade_elapsed = elapsed - TOAST_FADE_IN - self.visible();
            1.0 - (fade_elapsed / TOAST_FADE_OUT) as f32
        }
        .clamp(0.0, 1.0);

        // Slide-up offset during fade in
        let y_offset = if elapsed < TOAST_FADE_IN {
            10.0 * (1.0 - elapsed / TOAST_FADE_IN) as f32
        } else {
            0.0
        };

        egui::Area::new(egui::Id::new("klipp_toast"))
            .anchor(
                egui::Align2::CENTER_BOTTOM,
                egui::vec2(0.0, -40.0 - y_offset),
            )
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                // Polarity-aware bg so dark text on light theme (and
                // vice versa) stays readable. Was hardcoded 0x1e1e26
                // — invisible against `text_primary` on light themes.
                let bg = egui::Color32::from_rgba_premultiplied(
                    theme.bg_raised.r(),
                    theme.bg_raised.g(),
                    theme.bg_raised.b(),
                    (255.0 * alpha) as u8,
                );
                let border = egui::Color32::from_rgba_premultiplied(
                    theme.accent_dim.r(),
                    theme.accent_dim.g(),
                    theme.accent_dim.b(),
                    (255.0 * alpha) as u8,
                );
                let text_color = egui::Color32::from_rgba_premultiplied(
                    theme.text_primary.r(),
                    theme.text_primary.g(),
                    theme.text_primary.b(),
                    (255.0 * alpha) as u8,
                );

                egui::Frame::none()
                    .fill(bg)
                    .stroke(egui::Stroke::new(1.0_f32, border))
                    .rounding(4.0)
                    .inner_margin(egui::Margin::symmetric(16.0, 8.0))
                    .shadow(egui::Shadow {
                        spread: 0.0,
                        blur: 20.0,
                        offset: egui::vec2(0.0, 4.0),
                        color: egui::Color32::from_rgba_premultiplied(
                            0,
                            0,
                            0,
                            (128.0 * alpha) as u8,
                        ),
                    })
                    .show(ui, |ui| {
                        // Without an explicit width cap, egui's Area
                        // collapses to a sliver and wraps the label one
                        // word per line. Cap to a readable column and
                        // let multi-line messages wrap at that width.
                        // The longest toast reaches ~91 characters
                        // ("Recording saved. Drag from media pool onto a
                        // track, then use Audio Sync to align if
                        // needed."), so wrapping has to work, not just
                        // not crash.
                        ui.set_max_width(560.0);
                        let text = egui::RichText::new(msg.as_str()).size(12.0).color(text_color);
                        match &self.action {
                            // A note with a button is one line, the button
                            // after it (#2983).
                            Some((label, action)) => {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Label::new(text).extend());
                                    ui.label(egui::RichText::new("\u{b7}").size(12.0).color(text_color));
                                    if ui.link(egui::RichText::new(label.as_str()).size(12.0)).clicked() {
                                        clicked = Some(*action);
                                    }
                                });
                            }
                            None => {
                                ui.add(egui::Label::new(text));
                            }
                        }
                    });
            });

        // Request repaint while toast is animating
        if elapsed < total_duration {
            ctx.request_repaint();
        }
        clicked
    }

    /// Clear expired toast messages.
    pub fn cleanup(&mut self) {
        if let Some(appear) = self.appear_time {
            let total = TOAST_FADE_IN + self.visible() + TOAST_FADE_OUT;
            if appear.elapsed().as_secs_f64() > total {
                self.message = None;
                self.action = None;
                self.appear_time = None;
            }
        }
    }
}
