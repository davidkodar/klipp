//! Frame Glide's data (#611): a clip's move between two framings. The
//! keys it writes and when are `klipp_timeline::frame_glide`'s.

use serde::{Deserialize, Serialize};

/// One framing: the part of the frame the output shows, with the output's
/// aspect. `x` and `y` are its centre's offset from the frame's centre, as
/// fractions of the frame's width and height; `width` its width as a
/// fraction of the frame's (1 is the whole frame, 0.5 a 2x push in).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GlideFrame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

impl GlideFrame {
    /// The whole frame: no move.
    pub const FULL: GlideFrame = GlideFrame { x: 0.0, y: 0.0, width: 1.0 };

    /// Narrowest a framing may be: a 20x push.
    pub const MIN_WIDTH: f32 = 0.05;

    /// The transform that shows this framing filling the frame of
    /// `frame_w` x `frame_h` pixels: (scale, position x, position y), the
    /// position in frame pixels as the Transform effect takes it.
    pub fn transform(self, frame_w: f32, frame_h: f32) -> (f32, f32, f32) {
        self.transform_about([0.5, 0.5], frame_w, frame_h)
    }

    /// [`Self::transform`] for a picture scaled about `anchor` (fractions
    /// of the frame) rather than its centre: the framing's centre still
    /// lands on the frame's.
    pub fn transform_about(self, anchor: [f32; 2], frame_w: f32, frame_h: f32) -> (f32, f32, f32) {
        let w = self.width.max(Self::MIN_WIDTH);
        let scale = 1.0 / w;
        let (ax, ay) = (anchor[0] * frame_w, anchor[1] * frame_h);
        let (cx, cy) = (frame_w * 0.5, frame_h * 0.5);
        let (fx, fy) = (cx + self.x * frame_w, cy + self.y * frame_h);
        (scale, (cx - ax) - (fx - ax) * scale, (cy - ay) - (fy - ay) * scale)
    }

    /// The framing a transform shows: [`Self::transform`] undone.
    pub fn from_transform(scale: f32, pos_x: f32, pos_y: f32, frame_w: f32, frame_h: f32) -> GlideFrame {
        let scale = scale.max(1.0 / 20.0);
        GlideFrame {
            x: -pos_x / (scale * frame_w.max(1.0)),
            y: -pos_y / (scale * frame_h.max(1.0)),
            width: 1.0 / scale,
        }
    }
}

/// How the move runs between the two framings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GlideEasing {
    Linear,
    /// Starts slowly and gathers speed.
    EaseIn,
    /// Starts at speed and settles.
    EaseOut,
    #[default]
    EaseInOut,
}

impl GlideEasing {
    pub const ALL: [GlideEasing; 4] = [GlideEasing::Linear, GlideEasing::EaseIn, GlideEasing::EaseOut, GlideEasing::EaseInOut];

    pub fn label(self) -> &'static str {
        match self {
            GlideEasing::Linear => "Linear",
            GlideEasing::EaseIn => "Ease In",
            GlideEasing::EaseOut => "Ease Out",
            GlideEasing::EaseInOut => "Ease In and Out",
        }
    }
}

/// A clip's Frame Glide.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameGlide {
    pub start: GlideFrame,
    pub end: GlideFrame,
    #[serde(default)]
    pub easing: GlideEasing,
}

impl Default for FrameGlide {
    fn default() -> Self {
        FrameGlide { start: GlideFrame::FULL, end: GlideFrame::FULL, easing: GlideEasing::default() }
    }
}
