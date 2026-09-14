//! Gesture capture for the AI Tools prompt layer.
//!
//! This module is **pure data**: no egui, no threading, no engine
//! coupling. The viewer panel collects pointer events (click, drag
//! start / extend / end) at the point where it already does scrub-bar
//! and transport interaction, normalises the cursor position into
//! `[0,1]` image space, and feeds those samples into a
//! [`PromptCapture`]. The capture accumulates user-intent prompts
//! until the user hits "Generate", at which point [`PromptCapture::build`] emits a
//! `klipp_ml::MaskPrompts` ready to ship into a `MaskRequest`.
//!
//! ## Why a pure-data type
//!
//! Two reasons:
//!
//! 1. **Testability.** Unit tests can drive the capture as a state
//!    machine without spinning up egui or worker threads. This file's
//!    `#[cfg(test)]` block exercises every transition end-to-end.
//! 2. **Mode independence.** SAM-family models accept clicks, boxes,
//!    AND scribbles in one request. The capture holds all three
//!    collections side-by-side; switching `PromptMode` only changes
//!    which collection a new gesture *appends to*, never wipes the
//!    others. That matches FCP / Resolve where you can drop a click
//!    inside a box you already drew.
//!
//! ## Coordinate space
//!
//! All inputs are normalised image-space `[0,1]` floats. The viewer
//! is responsible for the conversion (cursor pos → image rect → unit
//! coords) before calling into this module. Prompts go to the model
//! in the same space — see `klipp_ml::ClickPrompt::x` / `.y`. This
//! makes prompts resolution-independent: the same click works at
//! 1080p preview and 4K render.

use klipp_ml::{BoxPrompt, ClickPrompt, MaskPrompts, ScribblePrompt};

/// Which mouse button the user pressed. Mapped at the viewer layer
/// from egui's `PointerButton`. Right-click flips `positive` to
/// `false` for SAM-family negative prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickButton {
    /// Left mouse button — positive prompt ("include this region").
    Left,
    /// Right mouse button — negative prompt ("exclude this region").
    Right,
}

impl ClickButton {
    /// Whether this button maps to a positive prompt.
    pub fn is_positive(self) -> bool {
        matches!(self, ClickButton::Left)
    }
}

/// Which prompt-collection a fresh gesture appends to. The viewer's
/// AI Tools toolbar lets the user toggle between these.
///
/// Existing prompts of OTHER modes are preserved — switching from
/// `PointPrompt` to `BoxPrompt` does not drop the clicks the user
/// already placed. Use [`PromptCapture::clear`] to wipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum PromptMode {
    /// Standard click: left = positive, right = negative.
    #[default]
    PointPrompt,
    /// Drag a rectangle from one corner to the other.
    BoxPrompt,
    /// Free-form scribble. Left-drag = positive, right-drag =
    /// negative.
    Scribble,
}


/// Accumulated user prompts ready to ship to a `MaskGenerator`.
///
/// Lives in `AppState::ai_tools_state.prompts` (see
/// `inference.rs::INTEGRATION_NOTES`). The viewer mutates it in
/// response to pointer events, the AI Tools panel reads it for a
/// summary ("3 clicks, 1 box"), and the inference dispatcher snapshots
/// it via [`Self::build`] when the user hits "Generate".
#[derive(Debug, Clone, Default)]
pub struct PromptCapture {
    /// Active mode — fresh gestures append to the matching collection.
    pub mode: PromptMode,
    /// Single-click prompts. Order is preserved.
    pub clicks: Vec<ClickPrompt>,
    /// Finalised box prompts (drag-released).
    pub boxes: Vec<BoxPrompt>,
    /// Drag-in-progress for the current scribble or box. For
    /// `Scribble` mode this is the polyline being extended; for
    /// `BoxPrompt` mode the first element is the drag origin and the
    /// last is the current cursor (only the two endpoints are
    /// significant). `None` when no drag is active.
    pub current_scribble: Option<DragState>,
    /// Scribble polylines that have been finalised by drag-release.
    pub finalized_scribbles: Vec<ScribblePrompt>,
}

/// State held during an in-progress drag. Carries the originating
/// mouse button so right-drag scribbles emit negative prompts.
#[derive(Debug, Clone)]
pub struct DragState {
    pub points: Vec<(f32, f32)>,
    pub button: ClickButton,
}

impl PromptCapture {
    /// Fresh capture in `PointPrompt` mode with no prompts.
    pub fn new() -> Self {
        Self::default()
    }

    /// Switch the active mode without clearing existing prompts.
    pub fn set_mode(&mut self, mode: PromptMode) {
        // Drop any in-progress drag — switching modes mid-drag would
        // produce nonsense. The user explicitly clicked the toolbar,
        // so stomping on the half-drawn box / scribble is the right
        // behaviour.
        self.current_scribble = None;
        self.mode = mode;
    }

    /// Record a single click at normalised coordinates `(x_norm,
    /// y_norm)`. No-op unless the active mode is `PointPrompt`.
    ///
    /// In `BoxPrompt` and `Scribble` modes a click is a zero-length
    /// drag, which is too ambiguous to interpret reliably (is that a
    /// degenerate box, a single-point scribble, or just a misclick?).
    /// The viewer is expected to call `start_drag` / `end_drag` for
    /// those modes; bare clicks fall through silently.
    pub fn record_click(&mut self, x_norm: f32, y_norm: f32, button: ClickButton) {
        if !matches!(self.mode, PromptMode::PointPrompt) {
            return;
        }
        let (x, y) = clamp_unit(x_norm, y_norm);
        self.clicks.push(ClickPrompt {
            x,
            y,
            positive: button.is_positive(),
        });
    }

    /// Begin a drag at `(x_norm, y_norm)` with `button`. Behaviour
    /// depends on the active mode:
    /// - `PointPrompt`: drag is treated as a click at the start point;
    ///   the captured click sits in `clicks` immediately. `extend_drag`
    ///   / `end_drag` are no-ops in this mode.
    /// - `BoxPrompt`: stores the origin in `current_scribble`; the box
    ///   isn't finalised until `end_drag`.
    /// - `Scribble`: starts a fresh polyline.
    pub fn start_drag(&mut self, x_norm: f32, y_norm: f32, button: ClickButton) {
        let (x, y) = clamp_unit(x_norm, y_norm);
        match self.mode {
            PromptMode::PointPrompt => {
                self.clicks.push(ClickPrompt {
                    x,
                    y,
                    positive: button.is_positive(),
                });
                // Leave current_scribble empty — extend / end are
                // no-ops for point mode.
            }
            PromptMode::BoxPrompt | PromptMode::Scribble => {
                self.current_scribble = Some(DragState {
                    points: vec![(x, y)],
                    button,
                });
            }
        }
    }

    /// Extend the in-progress drag with another sample. No-op if no
    /// drag is in progress, or if the active mode doesn't use drags.
    ///
    /// For `BoxPrompt`, only the first and last samples matter, but
    /// we still append every sample so the live preview can paint
    /// along the cursor's actual path. The finaliser
    /// (`end_drag`) extracts the corners.
    ///
    /// For `Scribble`, every sample is recorded — that polyline is
    /// what the model receives.
    pub fn extend_drag(&mut self, x_norm: f32, y_norm: f32) {
        if matches!(self.mode, PromptMode::PointPrompt) {
            return;
        }
        let Some(drag) = self.current_scribble.as_mut() else {
            return;
        };
        let (x, y) = clamp_unit(x_norm, y_norm);
        // Skip dupes — egui can fire multiple drag events at the
        // same screen pos when the cursor pauses, and we don't want
        // the scribble polyline to balloon with redundant points.
        if drag.points.last().copied() != Some((x, y)) {
            drag.points.push((x, y));
        }
    }

    /// Finalise the in-progress drag. Behaviour:
    /// - `PointPrompt`: no-op (the click was already recorded by
    ///   `start_drag`).
    /// - `BoxPrompt`: emits one `BoxPrompt` from the first and last
    ///   points. Degenerate boxes (start == end) are dropped to avoid
    ///   shipping zero-area prompts to the model.
    /// - `Scribble`: emits one `ScribblePrompt` containing the full
    ///   sample polyline. Single-point scribbles are dropped (same
    ///   rationale as degenerate boxes — they're indistinguishable
    ///   from a misclick).
    pub fn end_drag(&mut self, x_norm: f32, y_norm: f32) {
        if matches!(self.mode, PromptMode::PointPrompt) {
            return;
        }
        let Some(mut drag) = self.current_scribble.take() else {
            return;
        };
        let (x, y) = clamp_unit(x_norm, y_norm);
        if drag.points.last().copied() != Some((x, y)) {
            drag.points.push((x, y));
        }
        match self.mode {
            PromptMode::BoxPrompt => {
                let (x1, y1) = drag.points.first().copied().unwrap_or((x, y));
                let (x2, y2) = (x, y);
                if (x1 - x2).abs() > f32::EPSILON || (y1 - y2).abs() > f32::EPSILON {
                    // Normalise so x1 <= x2 and y1 <= y2 — downstream
                    // code expects a canonical AABB and shouldn't
                    // have to handle inverted boxes.
                    let (x1, x2) = if x1 <= x2 { (x1, x2) } else { (x2, x1) };
                    let (y1, y2) = if y1 <= y2 { (y1, y2) } else { (y2, y1) };
                    self.boxes.push(BoxPrompt { x1, y1, x2, y2 });
                }
            }
            PromptMode::Scribble => {
                if drag.points.len() >= 2 {
                    self.finalized_scribbles.push(ScribblePrompt {
                        points: drag.points,
                        positive: drag.button.is_positive(),
                    });
                }
            }
            PromptMode::PointPrompt => unreachable!(),
        }
    }

    /// Cancel an in-progress drag without emitting anything. Used when
    /// the cursor leaves the viewer mid-drag, or when the user hits
    /// Escape.
    pub fn cancel_drag(&mut self) {
        self.current_scribble = None;
    }

    /// Wipe every prompt — clicks, boxes, scribbles, and any
    /// in-progress drag. Use when the user clears the canvas.
    pub fn clear(&mut self) {
        self.clicks.clear();
        self.boxes.clear();
        self.finalized_scribbles.clear();
        self.current_scribble = None;
    }

    /// `true` when no prompts and no in-progress drag exist.
    pub fn is_empty(&self) -> bool {
        self.clicks.is_empty()
            && self.boxes.is_empty()
            && self.finalized_scribbles.is_empty()
            && self.current_scribble.is_none()
    }

    /// Snapshot the current capture as the `MaskPrompts` value the
    /// model wants. Excludes any in-progress drag — only finalised
    /// prompts ship.
    pub fn build(&self) -> MaskPrompts {
        MaskPrompts {
            clicks: self.clicks.clone(),
            boxes: self.boxes.clone(),
            scribbles: self.finalized_scribbles.clone(),
            text: None,
        }
    }
}

/// Clamp `(x, y)` into the unit square. The viewer ought to clamp
/// before calling but pointer events near the image edge can land
/// outside on subpixel rounding, so we defend at the boundary.
fn clamp_unit(x: f32, y: f32) -> (f32, f32) {
    (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn default_capture_is_empty_and_in_point_mode() {
        let c = PromptCapture::new();
        assert!(c.is_empty());
        assert_eq!(c.mode, PromptMode::PointPrompt);
        assert!(c.build().is_empty());
    }

    #[test]
    fn record_click_left_emits_positive_prompt() {
        let mut c = PromptCapture::new();
        c.record_click(0.25, 0.75, ClickButton::Left);
        let p = c.build();
        assert_eq!(p.clicks.len(), 1);
        assert!(approx(p.clicks[0].x, 0.25));
        assert!(approx(p.clicks[0].y, 0.75));
        assert!(p.clicks[0].positive);
    }

    #[test]
    fn record_click_right_emits_negative_prompt() {
        let mut c = PromptCapture::new();
        c.record_click(0.5, 0.5, ClickButton::Right);
        let p = c.build();
        assert_eq!(p.clicks.len(), 1);
        assert!(!p.clicks[0].positive);
    }

    #[test]
    fn record_click_clamps_out_of_range_to_unit() {
        let mut c = PromptCapture::new();
        c.record_click(-0.5, 1.7, ClickButton::Left);
        let p = c.build();
        assert!(approx(p.clicks[0].x, 0.0));
        assert!(approx(p.clicks[0].y, 1.0));
    }

    #[test]
    fn multiple_clicks_preserve_order() {
        let mut c = PromptCapture::new();
        c.record_click(0.1, 0.1, ClickButton::Left);
        c.record_click(0.2, 0.2, ClickButton::Right);
        c.record_click(0.3, 0.3, ClickButton::Left);
        let p = c.build();
        assert_eq!(p.clicks.len(), 3);
        assert!(approx(p.clicks[0].x, 0.1));
        assert!(approx(p.clicks[1].x, 0.2));
        assert!(approx(p.clicks[2].x, 0.3));
        assert!(p.clicks[0].positive);
        assert!(!p.clicks[1].positive);
        assert!(p.clicks[2].positive);
    }

    #[test]
    fn record_click_is_no_op_outside_point_mode() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::BoxPrompt);
        c.record_click(0.5, 0.5, ClickButton::Left);
        assert!(c.clicks.is_empty());
    }

    #[test]
    fn box_drag_emits_one_box_with_normalised_corners() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::BoxPrompt);
        c.start_drag(0.2, 0.3, ClickButton::Left);
        c.extend_drag(0.5, 0.5);
        c.end_drag(0.8, 0.7);
        let p = c.build();
        assert_eq!(p.boxes.len(), 1);
        let b = p.boxes[0];
        assert!(approx(b.x1, 0.2));
        assert!(approx(b.y1, 0.3));
        assert!(approx(b.x2, 0.8));
        assert!(approx(b.y2, 0.7));
    }

    #[test]
    fn box_drag_inverted_corners_canonicalised() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::BoxPrompt);
        // User drags upper-right to lower-left.
        c.start_drag(0.8, 0.7, ClickButton::Left);
        c.end_drag(0.2, 0.3);
        let p = c.build();
        let b = p.boxes[0];
        assert!(approx(b.x1, 0.2));
        assert!(approx(b.y1, 0.3));
        assert!(approx(b.x2, 0.8));
        assert!(approx(b.y2, 0.7));
    }

    #[test]
    fn degenerate_box_drag_is_dropped() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::BoxPrompt);
        c.start_drag(0.5, 0.5, ClickButton::Left);
        c.end_drag(0.5, 0.5);
        let p = c.build();
        assert!(p.boxes.is_empty());
    }

    #[test]
    fn scribble_drag_records_full_polyline() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.1, 0.1, ClickButton::Left);
        c.extend_drag(0.2, 0.2);
        c.extend_drag(0.3, 0.3);
        c.extend_drag(0.4, 0.4);
        c.end_drag(0.5, 0.5);
        let p = c.build();
        assert_eq!(p.scribbles.len(), 1);
        let s = &p.scribbles[0];
        assert_eq!(s.points.len(), 5);
        assert!(approx(s.points[0].0, 0.1));
        assert!(approx(s.points.last().unwrap().0, 0.5));
        assert!(s.positive);
    }

    #[test]
    fn scribble_right_drag_is_negative() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.1, 0.1, ClickButton::Right);
        c.extend_drag(0.2, 0.2);
        c.end_drag(0.3, 0.3);
        let p = c.build();
        assert_eq!(p.scribbles.len(), 1);
        assert!(!p.scribbles[0].positive);
    }

    #[test]
    fn scribble_dedups_repeated_samples() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.1, 0.1, ClickButton::Left);
        c.extend_drag(0.2, 0.2);
        c.extend_drag(0.2, 0.2); // dupe
        c.extend_drag(0.2, 0.2); // dupe
        c.extend_drag(0.3, 0.3);
        c.end_drag(0.3, 0.3); // dupe (same as last extend)
        let p = c.build();
        let s = &p.scribbles[0];
        // Expect: 0.1, 0.2, 0.3 — three distinct points.
        assert_eq!(s.points.len(), 3);
    }

    #[test]
    fn single_point_scribble_is_dropped() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.5, 0.5, ClickButton::Left);
        c.end_drag(0.5, 0.5);
        let p = c.build();
        assert!(p.scribbles.is_empty());
    }

    #[test]
    fn cancel_drag_drops_in_progress_scribble() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.1, 0.1, ClickButton::Left);
        c.extend_drag(0.2, 0.2);
        c.cancel_drag();
        // Continue extending — should be ignored.
        c.extend_drag(0.3, 0.3);
        c.end_drag(0.4, 0.4);
        assert!(c.finalized_scribbles.is_empty());
        assert!(c.current_scribble.is_none());
    }

    #[test]
    fn switching_mode_drops_in_progress_drag() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.1, 0.1, ClickButton::Left);
        c.extend_drag(0.2, 0.2);
        c.set_mode(PromptMode::PointPrompt);
        // Original drag should be gone.
        assert!(c.current_scribble.is_none());
        // end_drag in the new mode should be a no-op (no drag).
        c.end_drag(0.3, 0.3);
        assert!(c.is_empty() || !c.clicks.is_empty()); // clicks==[]
        assert!(c.clicks.is_empty());
    }

    #[test]
    fn switching_mode_preserves_finalised_prompts() {
        let mut c = PromptCapture::new();
        c.record_click(0.1, 0.1, ClickButton::Left);
        c.set_mode(PromptMode::BoxPrompt);
        c.start_drag(0.2, 0.2, ClickButton::Left);
        c.end_drag(0.6, 0.6);
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.7, 0.7, ClickButton::Left);
        c.extend_drag(0.8, 0.8);
        c.end_drag(0.9, 0.9);
        let p = c.build();
        assert_eq!(p.clicks.len(), 1);
        assert_eq!(p.boxes.len(), 1);
        assert_eq!(p.scribbles.len(), 1);
    }

    #[test]
    fn clear_wipes_everything_including_in_progress_drag() {
        let mut c = PromptCapture::new();
        c.record_click(0.1, 0.1, ClickButton::Left);
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.5, 0.5, ClickButton::Left);
        c.extend_drag(0.6, 0.6);
        assert!(!c.is_empty());
        c.clear();
        assert!(c.is_empty());
        assert!(c.current_scribble.is_none());
    }

    #[test]
    fn point_mode_start_drag_records_a_click_at_origin() {
        // Some viewers treat tap-and-drag as a click in point mode;
        // we keep that behaviour explicit so the test pins it.
        let mut c = PromptCapture::new();
        c.start_drag(0.4, 0.6, ClickButton::Right);
        // extend / end are no-ops in point mode
        c.extend_drag(0.5, 0.5);
        c.end_drag(0.5, 0.5);
        let p = c.build();
        assert_eq!(p.clicks.len(), 1);
        assert!(approx(p.clicks[0].x, 0.4));
        assert!(approx(p.clicks[0].y, 0.6));
        assert!(!p.clicks[0].positive);
        assert!(p.boxes.is_empty());
        assert!(p.scribbles.is_empty());
    }

    #[test]
    fn build_excludes_in_progress_drag() {
        let mut c = PromptCapture::new();
        c.set_mode(PromptMode::Scribble);
        c.start_drag(0.1, 0.1, ClickButton::Left);
        c.extend_drag(0.2, 0.2);
        // Don't end_drag — capture is mid-stroke.
        let p = c.build();
        assert!(p.scribbles.is_empty());
    }

    #[test]
    fn click_button_is_positive_matches_intent() {
        assert!(ClickButton::Left.is_positive());
        assert!(!ClickButton::Right.is_positive());
    }
}
