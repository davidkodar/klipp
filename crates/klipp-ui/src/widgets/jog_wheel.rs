//! Software jog wheel widget for the transport bar (#38, scoped).
//!
//! True HID/USB JogShuttle support is explicitly deferred — that would
//! require a new hidapi/evdev dependency, a background thread for HID
//! reports, hot-plug handling, and a device-discovery UI. The software
//! wheel is a ~150 LOC substitute that delivers the same interaction:
//! horizontal drag = frame-by-frame jog.
//!
//! ## Behaviour
//!
//! The widget accumulates horizontal drag delta in `drag_accum` and emits
//! whole-frame steps every `PIXELS_PER_FRAME` pixels. The fractional
//! remainder is carried across frames so sub-pixel drags don't get lost
//! and fast drags don't round unpredictably.
//!
//! Release clears `drag_accum` — lingering sub-pixel remainders after a
//! drag ends would otherwise fire a spurious step on the next drag start.

/// Drag accumulator state for the jog wheel. Owned by `AppState` so the
/// widget survives panel relayouts (the wheel must not lose its
/// in-progress drag when egui invalidates the containing frame).
#[derive(Debug, Clone, Default)]
pub struct JogWheelState {
    /// Accumulated horizontal drag in pixels since the last whole-frame
    /// emit. Reset on drag release so the next drag starts at zero.
    pub drag_accum: f32,
}

impl JogWheelState {
    pub fn new() -> Self {
        Self { drag_accum: 0.0 }
    }
}

/// Pixels of horizontal drag per frame emitted. Tuned for typical
/// trackpad / mouse sensitivity — slower than video scrub-bar drag so
/// users can reliably hit single frames without overshoot.
pub const PIXELS_PER_FRAME: f32 = 8.0;

/// Draw the jog wheel and return the number of frames to step this
/// frame (positive = forward, negative = backward, 0 = no change).
///
/// `radius` is half the widget's allocated size. 18 px works well in a
/// 44 px transport bar; 24 px for a larger touch target.
pub fn jog_wheel(
    ui: &mut egui::Ui,
    state: &mut JogWheelState,
    theme: &crate::theme::KlippTheme,
    radius: f32,
) -> i32 {
    let size = egui::vec2(radius * 2.0, radius * 2.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::drag());

    let center = rect.center();
    let painter = ui.painter();
    let is_active = response.dragged();

    // Outer ring — amber when dragging, muted grey otherwise.
    let ring_color = if is_active {
        theme.accent
    } else if response.hovered() {
        theme.text_secondary
    } else {
        theme.border_light
    };
    painter.circle_stroke(center, radius, egui::Stroke::new(2.0_f32, ring_color));

    // Inner fill — dark disc so the tick marks show up cleanly.
    painter.circle_filled(center, radius - 3.0, theme.bg_raised);

    // Tick marks at 12 clock positions.
    for i in 0..12 {
        let angle = i as f32 * std::f32::consts::TAU / 12.0;
        let inner = radius - 8.0;
        let outer = radius - 3.0;
        let dir = egui::vec2(angle.sin(), -angle.cos());
        painter.line_segment(
            [center + dir * inner, center + dir * outer],
            egui::Stroke::new(1.0_f32, theme.border_light),
        );
    }

    // Centre label.
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        "JOG",
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );

    // ── Drag → frame-step conversion ──
    let frames_out = if response.dragged() {
        let delta = response.drag_delta().x;
        state.drag_accum += delta;
        let whole = (state.drag_accum / PIXELS_PER_FRAME) as i32;
        if whole != 0 {
            state.drag_accum -= whole as f32 * PIXELS_PER_FRAME;
        }
        whole
    } else {
        // Clear any lingering sub-pixel remainder so the next drag
        // starts from zero rather than a random fractional bias.
        state.drag_accum = 0.0;
        0
    };

    response.on_hover_text("Drag left/right to jog frame by frame");

    frames_out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jog_wheel_accumulates_drag_correctly() {
        let mut state = JogWheelState::new();
        // 16 px of drag = 2 frames at 8 px/frame.
        state.drag_accum += 16.0;
        let whole = (state.drag_accum / PIXELS_PER_FRAME) as i32;
        assert_eq!(whole, 2);
        state.drag_accum -= whole as f32 * PIXELS_PER_FRAME;
        assert_eq!(state.drag_accum, 0.0);
    }

    #[test]
    fn jog_wheel_negative_drag_is_backward() {
        let mut state = JogWheelState::new();
        state.drag_accum += -8.0;
        let whole = (state.drag_accum / PIXELS_PER_FRAME) as i32;
        assert_eq!(whole, -1);
    }

    #[test]
    fn jog_wheel_sub_pixel_drag_does_not_emit() {
        // A 4-pixel drag at 8 px/frame is half a frame — should emit
        // zero and carry the remainder for the next drag tick.
        let mut state = JogWheelState::new();
        state.drag_accum += 4.0;
        let whole = (state.drag_accum / PIXELS_PER_FRAME) as i32;
        assert_eq!(whole, 0);
        assert_eq!(state.drag_accum, 4.0);
    }

    #[test]
    fn jog_wheel_remainder_carries_across_ticks() {
        // Two 6-pixel drags (both sub-frame individually) should produce
        // one frame total — emits 0 after the first, 1 after the second.
        let mut state = JogWheelState::new();

        state.drag_accum += 6.0;
        let whole = (state.drag_accum / PIXELS_PER_FRAME) as i32;
        assert_eq!(whole, 0);
        // Don't subtract since nothing emitted.

        state.drag_accum += 6.0; // now 12.0, one full frame + 4 remainder
        let whole = (state.drag_accum / PIXELS_PER_FRAME) as i32;
        assert_eq!(whole, 1);
        state.drag_accum -= whole as f32 * PIXELS_PER_FRAME;
        assert!((state.drag_accum - 4.0).abs() < 1e-6);
    }
}
