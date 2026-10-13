//! Piece transitions (#985): the picture breaks into strips, panels, two
//! halves or shards that slide, tilt, turn, fall or shrink away to reveal
//! the next shot, or the next shot's pieces come in and lock together.
//! Sliding strips, panels and halves are inverse-mapped in the transition
//! shader; every other motion, and the shards, are posed pieces drawn
//! forward (`klipp_effects::pieces`).
//!
//! Their settings are typed here, as [`PieceParams`], and kept in the
//! transition's four parameter slots, so the shader uniform, the CPU twin
//! and the project file stay as they are for every other kind:
//!
//! - `param0`: the number of strips, or of panel rows (panels take as
//!   many columns as make them square); for halves, 0 parts them across
//!   the line and 1 slides them along it.
//! - `param1`: the angle in degrees. Strips and halves take any angle:
//!   the direction strips slide along, the line halves split down.
//!   Panels move along the nearest of right (0), down (90), left (180)
//!   and up (270).
//! - `param2`: the stagger, 0 to 0.9: how much of the transition the
//!   pieces' starts are spread over.
//! - `param3`: a whole number holding the rest, bit fields read the same
//!   by the shader: the order (bits 0-2), the easing (bits 3-4), whether
//!   the next shot's pieces arrive instead of the outgoing ones leaving
//!   (bit 5), whether neighbours move opposite ways (bit 6), the gap
//!   between pieces in pixels (bits 7-11), the motion (bits 12-14),
//!   shadows (bit 15) and the shards' and the tumbling's seed (bits
//!   16-23).

use serde::{Deserialize, Serialize};

use crate::{TransitionKind, TransitionUserParams};

/// Which pieces move first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PieceOrder {
    /// One after another along the move: the piece nearest the way out first.
    #[default]
    InSequence,
    /// One after another, from the other end.
    Reversed,
    /// From the middle outwards.
    CentreOut,
    /// From the edges inwards.
    EdgesIn,
    /// In a shuffled order, the same on every render.
    Random,
    /// Together.
    AllAtOnce,
}

impl PieceOrder {
    pub const ALL: [PieceOrder; 6] = [PieceOrder::InSequence, PieceOrder::Reversed, PieceOrder::CentreOut, PieceOrder::EdgesIn, PieceOrder::Random, PieceOrder::AllAtOnce];

    pub fn label(self) -> &'static str {
        match self {
            PieceOrder::InSequence => "In sequence",
            PieceOrder::Reversed => "Reversed",
            PieceOrder::CentreOut => "Centre out",
            PieceOrder::EdgesIn => "Edges in",
            PieceOrder::Random => "Random",
            PieceOrder::AllAtOnce => "All at once",
        }
    }
}

/// How each piece speeds up and slows down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PieceEase {
    /// Eases in and out.
    #[default]
    Smooth,
    Linear,
    /// Starts slow and leaves fast.
    Accelerate,
    /// Goes past where it lands and settles back.
    Overshoot,
}

impl PieceEase {
    pub const ALL: [PieceEase; 4] = [PieceEase::Smooth, PieceEase::Linear, PieceEase::Accelerate, PieceEase::Overshoot];

    pub fn label(self) -> &'static str {
        match self {
            PieceEase::Smooth => "Smooth",
            PieceEase::Linear => "Linear",
            PieceEase::Accelerate => "Accelerate",
            PieceEase::Overshoot => "Overshoot",
        }
    }
}

/// How each piece moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PieceMotion {
    /// Slides away along its way out.
    #[default]
    Slide,
    /// Swings away into the picture about its trailing edge.
    Tilt,
    /// Spins on its middle as it is flung out.
    Turn,
    /// Is pushed out and falls, tumbling.
    Fall,
    /// Shrinks to nothing where it is.
    Shrink,
}

impl PieceMotion {
    pub const ALL: [PieceMotion; 5] = [PieceMotion::Slide, PieceMotion::Tilt, PieceMotion::Turn, PieceMotion::Fall, PieceMotion::Shrink];

    pub fn label(self) -> &'static str {
        match self {
            PieceMotion::Slide => "Slide",
            PieceMotion::Tilt => "Tilt away",
            PieceMotion::Turn => "Turn",
            PieceMotion::Fall => "Fall",
            PieceMotion::Shrink => "Shrink",
        }
    }
}

/// A piece transition's settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PieceParams {
    /// Strips: how many; panels: how many rows.
    pub count: u32,
    /// Strips: the direction they slide; halves: the line they split
    /// down; panels: the nearest of right, down, left, up. Degrees,
    /// clockwise from right.
    pub angle: f32,
    /// 0 to 0.9: how much of the transition the starts are spread over.
    pub stagger: f32,
    pub order: PieceOrder,
    pub ease: PieceEase,
    /// The next shot's pieces arrive, in place of the outgoing ones leaving.
    pub assemble: bool,
    /// Neighbouring strips or rows of panels move opposite ways.
    pub alternate: bool,
    /// The gap that opens between pieces as they move, in pixels (0-31).
    pub gap: u32,
    /// Halves slide along the line in place of parting across it.
    pub shear: bool,
    pub motion: PieceMotion,
    /// Soft shadows under the moving pieces (posed motions and shards).
    pub shadow: bool,
    /// Which shards, and which way each tumbles (0-255).
    pub seed: u32,
}

impl Default for PieceParams {
    fn default() -> Self {
        PieceParams { count: 8, angle: 0.0, stagger: 0.35, order: PieceOrder::InSequence, ease: PieceEase::Smooth, assemble: false, alternate: true, gap: 0, shear: false, motion: PieceMotion::Slide, shadow: false, seed: 0 }
    }
}

/// The piece transitions.
pub fn is_piece_kind(kind: TransitionKind) -> bool {
    matches!(kind, TransitionKind::SlideStrips | TransitionKind::SlidePanels | TransitionKind::PartHalves | TransitionKind::Shatter)
}

impl PieceParams {
    /// The settings `params` holds for `kind`.
    pub fn read(kind: TransitionKind, params: &TransitionUserParams) -> Self {
        let bits = params.param3.max(0.0).round() as u32;
        let order = PieceOrder::ALL.get((bits & 7) as usize).copied().unwrap_or_default();
        let ease = PieceEase::ALL[((bits >> 3) & 3) as usize];
        let halves = kind == TransitionKind::PartHalves;
        PieceParams {
            count: if halves { 2 } else { params.param0.round().clamp(1.0, if kind == TransitionKind::Shatter { 96.0 } else { 64.0 }) as u32 },
            angle: params.param1,
            stagger: params.param2.clamp(0.0, 0.9),
            order,
            ease,
            assemble: bits & 32 != 0,
            alternate: bits & 64 != 0,
            gap: (bits >> 7) & 31,
            shear: halves && params.param0 > 0.5,
            motion: PieceMotion::ALL.get(((bits >> 12) & 7) as usize).copied().unwrap_or_default(),
            shadow: bits & (1 << 15) != 0,
            seed: (bits >> 16) & 255,
        }
    }

    /// `params` holding these settings for `kind`; its colour is kept.
    pub fn write(&self, kind: TransitionKind, params: &mut TransitionUserParams) {
        params.param0 = if kind == TransitionKind::PartHalves {
            if self.shear { 1.0 } else { 0.0 }
        } else {
            (match kind {
                // Posed panels are fewer: each is drawn forward.
                TransitionKind::SlidePanels if self.motion != PieceMotion::Slide => self.count.clamp(1, 8),
                TransitionKind::SlidePanels => self.count.clamp(1, 16),
                TransitionKind::Shatter => self.count.clamp(4, 96),
                _ => self.count.clamp(1, 64),
            }) as f32
        };
        params.param1 = self.angle;
        params.param2 = self.stagger.clamp(0.0, 0.9);
        let order = PieceOrder::ALL.iter().position(|o| *o == self.order).unwrap_or(0) as u32;
        let ease = PieceEase::ALL.iter().position(|e| *e == self.ease).unwrap_or(0) as u32;
        let motion = PieceMotion::ALL.iter().position(|m| *m == self.motion).unwrap_or(0) as u32;
        params.param3 = (order | ease << 3 | (self.assemble as u32) << 5 | (self.alternate as u32) << 6 | self.gap.min(31) << 7 | motion << 12 | (self.shadow as u32) << 15 | self.seed.min(255) << 16) as f32;
    }

    /// `kind`'s default parameters with these settings.
    pub fn to_user(&self, kind: TransitionKind) -> TransitionUserParams {
        let mut p = crate::spec_for_kind(kind).default_params;
        self.write(kind, &mut p);
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every setting survives the four slots, for each piece kind, and
    /// the registry's defaults read as the settings meant.
    #[test]
    fn settings_round_trip_through_the_slots() {
        let all = PieceParams { count: 12, angle: 37.5, stagger: 0.6, order: PieceOrder::Random, ease: PieceEase::Overshoot, assemble: true, alternate: false, gap: 9, shear: false, motion: PieceMotion::Fall, shadow: true, seed: 201 };
        for kind in [TransitionKind::SlideStrips, TransitionKind::Shatter, TransitionKind::SlidePanels] {
            let all = if kind == TransitionKind::SlidePanels { PieceParams { count: 6, ..all } } else { all };
            assert_eq!(PieceParams::read(kind, &all.to_user(kind)), all, "{kind:?}");
        }
        let halves = PieceParams { count: 2, shear: true, ..all };
        assert_eq!(PieceParams::read(TransitionKind::PartHalves, &halves.to_user(TransitionKind::PartHalves)), halves);
        for order in PieceOrder::ALL {
            for (ease, motion) in PieceEase::ALL.into_iter().zip(PieceMotion::ALL) {
                let p = PieceParams { order, ease, motion, ..Default::default() };
                assert_eq!(PieceParams::read(TransitionKind::SlideStrips, &p.to_user(TransitionKind::SlideStrips)), p);
            }
        }
        // Too many panel rows are held to what the shader draws.
        let rows = PieceParams { count: 40, ..Default::default() }.to_user(TransitionKind::SlidePanels);
        assert_eq!(rows.param0, 16.0);

        let strips = PieceParams::read(TransitionKind::SlideStrips, &crate::spec_for_kind(TransitionKind::SlideStrips).default_params);
        assert_eq!((strips.count, strips.alternate, strips.assemble, strips.order), (8, true, false, PieceOrder::InSequence));
        let panels = PieceParams::read(TransitionKind::SlidePanels, &crate::spec_for_kind(TransitionKind::SlidePanels).default_params);
        assert_eq!((panels.count, panels.alternate), (3, false));
        let halves = PieceParams::read(TransitionKind::PartHalves, &crate::spec_for_kind(TransitionKind::PartHalves).default_params);
        assert!(!halves.shear && halves.count == 2);
        let shatter = PieceParams::read(TransitionKind::Shatter, &crate::spec_for_kind(TransitionKind::Shatter).default_params);
        assert_eq!((shatter.count, shatter.motion, shatter.order, shatter.ease, shatter.shadow), (24, PieceMotion::Fall, PieceOrder::CentreOut, PieceEase::Accelerate, true));
        // Posed panels are held to fewer rows.
        assert_eq!(PieceParams { count: 12, motion: PieceMotion::Tilt, ..Default::default() }.to_user(TransitionKind::SlidePanels).param0, 8.0);
        assert!(is_piece_kind(TransitionKind::SlidePanels) && !is_piece_kind(TransitionKind::PushLeft));
    }
}
