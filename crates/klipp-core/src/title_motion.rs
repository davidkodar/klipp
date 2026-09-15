//! Title motion: how a text layer's glyphs enter and leave, with
//! easing and stagger. The renderer evaluates it per glyph; the
//! inspector edits it; templates ship presets built from it.

use serde::{Deserialize, Serialize};

/// Easing for title motion: how progress 0..1 is shaped in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Easing {
    Linear,
    #[default]
    OutCubic,
    OutQuint,
    OutExpo,
    /// Overshoots the rest pose a little before settling.
    OutBack,
    InOutCubic,
    InCubic,
}

impl Easing {
    /// The eased value of `t` in 0..1.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::OutCubic => 1.0 - (1.0 - t).powi(3),
            Easing::OutQuint => 1.0 - (1.0 - t).powi(5),
            Easing::OutExpo => if t >= 1.0 { 1.0 } else { 1.0 - 2f32.powf(-10.0 * t) },
            Easing::OutBack => {
                let c1 = 1.70158;
                let c3 = c1 + 1.0;
                1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
            }
            Easing::InOutCubic => if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 },
            Easing::InCubic => t * t * t,
        }
    }

    pub const ALL: [Easing; 7] = [
        Easing::Linear, Easing::OutCubic, Easing::OutQuint, Easing::OutExpo,
        Easing::OutBack, Easing::InOutCubic, Easing::InCubic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Easing::Linear => "Linear",
            Easing::OutCubic => "Ease out",
            Easing::OutQuint => "Ease out (strong)",
            Easing::OutExpo => "Ease out (expo)",
            Easing::OutBack => "Overshoot",
            Easing::InOutCubic => "Ease in and out",
            Easing::InCubic => "Ease in",
        }
    }
}

/// What a title's glyphs do while they enter or leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MotionKind {
    #[default]
    Fade,
    /// Rises into place from below.
    Rise,
    /// Drops into place from above.
    Drop,
    /// Slides in from the right.
    SlideLeft,
    /// Slides in from the left.
    SlideRight,
    /// Grows into place.
    ScaleUp,
    /// Settles down into place.
    ScaleDown,
    /// Comes into focus.
    Blur,
    /// Rises out of an invisible mask at the baseline: the glyph is
    /// clipped to its resting box while it moves.
    MaskRise,
    /// Drops out of a mask above the line.
    MaskDrop,
    /// Appears one unit at a time with no easing.
    Typewriter,
}

impl MotionKind {
    pub const ALL: [MotionKind; 11] = [
        MotionKind::Fade, MotionKind::Rise, MotionKind::Drop, MotionKind::SlideLeft, MotionKind::SlideRight,
        MotionKind::ScaleUp, MotionKind::ScaleDown, MotionKind::Blur, MotionKind::MaskRise, MotionKind::MaskDrop,
        MotionKind::Typewriter,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MotionKind::Fade => "Fade",
            MotionKind::Rise => "Rise",
            MotionKind::Drop => "Drop",
            MotionKind::SlideLeft => "Slide from right",
            MotionKind::SlideRight => "Slide from left",
            MotionKind::ScaleUp => "Grow",
            MotionKind::ScaleDown => "Settle",
            MotionKind::Blur => "Focus",
            MotionKind::MaskRise => "Mask rise",
            MotionKind::MaskDrop => "Mask drop",
            MotionKind::Typewriter => "Typewriter",
        }
    }
}

/// Which pieces of the text start their motion at staggered times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StaggerUnit {
    #[default]
    None,
    Line,
    Word,
    Char,
}

impl StaggerUnit {
    pub const ALL: [StaggerUnit; 4] = [StaggerUnit::None, StaggerUnit::Line, StaggerUnit::Word, StaggerUnit::Char];

    pub fn label(self) -> &'static str {
        match self {
            StaggerUnit::None => "All at once",
            StaggerUnit::Line => "By line",
            StaggerUnit::Word => "By word",
            StaggerUnit::Char => "By character",
        }
    }
}

/// One motion: an entrance or an exit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    pub kind: MotionKind,
    /// How long one unit takes to arrive (or leave).
    pub duration_secs: f32,
    /// Wait before the first unit starts, from the clip's start (or
    /// before the clip's end for an exit).
    pub delay_secs: f32,
    pub easing: Easing,
    pub stagger: StaggerUnit,
    /// Seconds between one unit's start and the next.
    pub stagger_secs: f32,
    /// Travel for the moving kinds, as a fraction of the font size.
    pub distance: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            kind: MotionKind::Fade,
            duration_secs: 0.6,
            delay_secs: 0.0,
            easing: Easing::OutCubic,
            stagger: StaggerUnit::None,
            stagger_secs: 0.05,
            distance: 0.6,
        }
    }
}

impl Motion {
    pub fn new(kind: MotionKind) -> Self {
        Self { kind, ..Default::default() }
    }
    pub fn duration(mut self, secs: f32) -> Self { self.duration_secs = secs; self }
    pub fn delay(mut self, secs: f32) -> Self { self.delay_secs = secs; self }
    pub fn easing(mut self, easing: Easing) -> Self { self.easing = easing; self }
    pub fn stagger(mut self, unit: StaggerUnit, secs: f32) -> Self { self.stagger = unit; self.stagger_secs = secs; self }
    pub fn distance(mut self, fraction: f32) -> Self { self.distance = fraction; self }

    /// The pose of a unit at eased progress `p` (0 = fully away, 1 =
    /// resting): (alpha, dx, dy, scale, blur sigma, clip to rest box).
    /// Offsets are in pixels for a font of `font_px`.
    pub fn pose(&self, p: f32, font_px: f32) -> (f32, f32, f32, f32, f32, bool) {
        let travel = font_px * self.distance;
        match self.kind {
            MotionKind::Fade => (p, 0.0, 0.0, 1.0, 0.0, false),
            MotionKind::Rise => (p, 0.0, (1.0 - p) * travel, 1.0, 0.0, false),
            MotionKind::Drop => (p, 0.0, -(1.0 - p) * travel, 1.0, 0.0, false),
            MotionKind::SlideLeft => (p, (1.0 - p) * travel * 1.5, 0.0, 1.0, 0.0, false),
            MotionKind::SlideRight => (p, -(1.0 - p) * travel * 1.5, 0.0, 1.0, 0.0, false),
            MotionKind::ScaleUp => (p, 0.0, 0.0, 0.6 + 0.4 * p, 0.0, false),
            MotionKind::ScaleDown => (p, 0.0, 0.0, 1.4 - 0.4 * p, 0.0, false),
            MotionKind::Blur => (p, 0.0, 0.0, 1.0, (1.0 - p) * font_px * 0.12, false),
            MotionKind::MaskRise => (1.0, 0.0, (1.0 - p) * font_px * 1.15, 1.0, 0.0, true),
            MotionKind::MaskDrop => (1.0, 0.0, -(1.0 - p) * font_px * 1.15, 1.0, 0.0, true),
            MotionKind::Typewriter => (if p > 0.0 { 1.0 } else { 0.0 }, 0.0, 0.0, 1.0, 0.0, false),
        }
    }
}

/// A text layer's entrance and exit. Both are optional; a title with
/// neither is static.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct TitleMotion {
    pub enter: Option<Motion>,
    pub exit: Option<Motion>,
}

impl TitleMotion {
    pub fn is_active(&self) -> bool {
        self.enter.is_some() || self.exit.is_some()
    }
    pub fn enter(m: Motion) -> Self {
        Self { enter: Some(m), exit: None }
    }
    pub fn both(enter: Motion, exit: Motion) -> Self {
        Self { enter: Some(enter), exit: Some(exit) }
    }
    /// The eased progress of unit `unit` at `t` seconds into a clip of
    /// `window` seconds (unknown window: no exit). 1 = resting.
    pub fn progress(&self, t: f32, window: Option<f32>, unit: usize) -> (f32, f32) {
        let enter = match self.enter {
            Some(m) => {
                let local = (t - m.delay_secs - unit as f32 * m.stagger_secs) / m.duration_secs.max(0.001);
                m.easing.apply(local)
            }
            None => 1.0,
        };
        let exit = match (self.exit, window) {
            (Some(m), Some(w)) => {
                let remaining = w - t;
                let local = (remaining - m.delay_secs - unit as f32 * m.stagger_secs) / m.duration_secs.max(0.001);
                m.easing.apply(local)
            }
            _ => 1.0,
        };
        (enter, exit)
    }
}

