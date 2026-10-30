//! Title motion: how a text layer's glyphs enter and leave, with
//! easing and stagger. The renderer evaluates it per glyph; the
//! inspector edits it; templates ship presets built from it.

use serde::{Deserialize, Serialize};

/// Easing for title motion: how progress 0..1 is shaped in time.
/// A named curve saves as its name, a custom one as its four handles.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum Easing {
    // The snake_case aliases take the spelling the title op, compose and
    // pan use, so a vector path's keys accept `linear` as well as the
    // saved `Linear` (#1920). What is saved does not change.
    #[serde(alias = "linear")]
    Linear,
    #[default]
    #[serde(alias = "out_cubic")]
    OutCubic,
    #[serde(alias = "out_quint")]
    OutQuint,
    #[serde(alias = "out_expo")]
    OutExpo,
    /// Overshoots the rest pose a little before settling.
    #[serde(alias = "out_back")]
    OutBack,
    #[serde(alias = "in_out_cubic")]
    InOutCubic,
    #[serde(alias = "in_cubic")]
    InCubic,
    /// Lands and bounces a few times before settling.
    #[serde(alias = "out_bounce")]
    OutBounce,
    /// A curve of its own (#1511).
    Custom(CubicBezier),
}

/// A cubic Bézier easing from (0, 0) to (1, 1) through two handles, as
/// CSS `cubic-bezier(x1, y1, x2, y2)` draws it: x is time, y progress.
/// The handles' times are kept within 0..1, so each time has one
/// progress; their progress is free, so a curve can overshoot.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl Default for CubicBezier {
    /// The curve the default named easing draws, near enough to start
    /// shaping from.
    fn default() -> Self {
        Self { x1: 0.33, y1: 1.0, x2: 0.68, y2: 1.0 }
    }
}

impl CubicBezier {
    pub fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1: x1.clamp(0.0, 1.0), y1, x2: x2.clamp(0.0, 1.0), y2 }
    }

    /// The progress at time `t` in 0..1: the curve's parameter for that
    /// time found by Newton's method, by bisection where that stalls.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let (x1, x2) = (self.x1.clamp(0.0, 1.0) as f64, self.x2.clamp(0.0, 1.0) as f64);
        let (y1, y2) = (self.y1 as f64, self.y2 as f64);
        let at = |a: f64, b: f64, u: f64| 3.0 * a * u * (1.0 - u) * (1.0 - u) + 3.0 * b * u * u * (1.0 - u) + u * u * u;
        let slope = |a: f64, b: f64, u: f64| 3.0 * a * (1.0 - u) * (1.0 - u) + 6.0 * (b - a) * u * (1.0 - u) + 3.0 * (1.0 - b) * u * u;
        let x = t as f64;
        let mut u = x;
        let mut solved = false;
        for _ in 0..8 {
            let err = at(x1, x2, u) - x;
            if err.abs() < 1e-7 {
                solved = true;
                break;
            }
            let d = slope(x1, x2, u);
            if d.abs() < 1e-6 {
                break;
            }
            u = (u - err / d).clamp(0.0, 1.0);
        }
        if !solved {
            // The curve's time rises with its parameter, as the handles'
            // times are within 0..1: halve the range to the time.
            let (mut lo, mut hi) = (0.0f64, 1.0f64);
            u = x;
            for _ in 0..60 {
                let v = at(x1, x2, u);
                if (v - x).abs() < 1e-7 {
                    break;
                }
                if v < x { lo = u } else { hi = u }
                u = (lo + hi) * 0.5;
            }
        }
        at(y1, y2, u) as f32
    }
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
            Easing::OutBounce => {
                let (n1, d1) = (7.5625, 2.75);
                if t < 1.0 / d1 {
                    n1 * t * t
                } else if t < 2.0 / d1 {
                    let t = t - 1.5 / d1;
                    n1 * t * t + 0.75
                } else if t < 2.5 / d1 {
                    let t = t - 2.25 / d1;
                    n1 * t * t + 0.9375
                } else {
                    let t = t - 2.625 / d1;
                    n1 * t * t + 0.984375
                }
            }
            Easing::Custom(curve) => curve.apply(t),
        }
    }

    pub const ALL: [Easing; 8] = [
        Easing::Linear, Easing::OutCubic, Easing::OutQuint, Easing::OutExpo,
        Easing::OutBack, Easing::OutBounce, Easing::InOutCubic, Easing::InCubic,
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
            Easing::OutBounce => "Bounce",
            Easing::Custom(_) => "Custom",
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
    /// Springs up from small, overshooting a little before it settles.
    Pop,
    /// Lands from very large, fast.
    Slam,
    /// Flies in from far off the left.
    FlyLeft,
    /// Flies in from far off the right.
    FlyRight,
    /// Flies down from far above.
    FlyTop,
    /// Flies up from far below.
    FlyBottom,
    /// Falls into place and bounces.
    Bounce,
    /// Flickers on like a failing sign.
    Flicker,
    /// Comes into focus while it settles down from a little larger.
    FocusIn,
    /// Rises into place out of a blur.
    SoftRise,
    /// Scales from larger, `amount` times its size (4 unless set), with
    /// no fade: as an exit the title grows until, held as a matte, the
    /// picture inside its letters fills the frame (#736).
    Larger,
    /// Scales from smaller, `amount` times its size (nothing unless set),
    /// with no fade: as an exit the title shrinks away.
    Smaller,
}

impl MotionKind {
    pub const ALL: [MotionKind; 23] = [
        MotionKind::Fade, MotionKind::Blur, MotionKind::FocusIn, MotionKind::Flicker,
        MotionKind::Rise, MotionKind::Drop, MotionKind::SoftRise, MotionKind::Bounce, MotionKind::SlideLeft, MotionKind::SlideRight,
        MotionKind::FlyLeft, MotionKind::FlyRight, MotionKind::FlyTop, MotionKind::FlyBottom,
        MotionKind::ScaleUp, MotionKind::ScaleDown, MotionKind::Pop, MotionKind::Slam,
        MotionKind::MaskRise, MotionKind::MaskDrop, MotionKind::Typewriter,
        MotionKind::Larger, MotionKind::Smaller,
    ];

    /// Whether the motion blurs: a blur cannot be posed glyph by glyph,
    /// so a staggered one is rastered.
    pub fn blurs(self) -> bool {
        matches!(self, MotionKind::Blur | MotionKind::FocusIn | MotionKind::SoftRise)
    }

    /// The easing a new motion of this kind takes: the overshoot of a pop,
    /// the bounce of a bounce, a flicker's steps unsmoothed.
    pub fn default_easing(self) -> Easing {
        match self {
            MotionKind::Pop => Easing::OutBack,
            MotionKind::Bounce => Easing::OutBounce,
            MotionKind::Slam | MotionKind::FlyLeft | MotionKind::FlyRight | MotionKind::FlyTop | MotionKind::FlyBottom => Easing::OutExpo,
            MotionKind::Flicker => Easing::Linear,
            _ => Easing::OutCubic,
        }
    }

    /// The easing a new exit of this kind takes: an exit's easing runs over
    /// its own time, so the slow start of an `in` curve, the title easing
    /// away and then leaving; a flicker's steps unsmoothed.
    pub fn default_exit_easing(self) -> Easing {
        match self {
            MotionKind::Flicker => Easing::Linear,
            _ => Easing::InCubic,
        }
    }

    /// The picker's group for this motion: what it does to the text.
    pub fn category(self) -> &'static str {
        match self {
            MotionKind::Fade | MotionKind::Blur | MotionKind::FocusIn | MotionKind::Flicker => "Fade",
            MotionKind::Rise | MotionKind::Drop | MotionKind::SlideLeft | MotionKind::SlideRight | MotionKind::SoftRise | MotionKind::Bounce => "Move",
            MotionKind::FlyLeft | MotionKind::FlyRight | MotionKind::FlyTop | MotionKind::FlyBottom => "Fly",
            MotionKind::ScaleUp | MotionKind::ScaleDown | MotionKind::Pop | MotionKind::Slam | MotionKind::Larger | MotionKind::Smaller => "Scale",
            MotionKind::MaskRise | MotionKind::MaskDrop | MotionKind::Typewriter => "Reveal",
        }
    }

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
            MotionKind::Pop => "Pop",
            MotionKind::Slam => "Slam",
            MotionKind::FlyLeft => "Fly from left",
            MotionKind::FlyRight => "Fly from right",
            MotionKind::FlyTop => "Fly from top",
            MotionKind::FlyBottom => "Fly from bottom",
            MotionKind::Bounce => "Bounce",
            MotionKind::Flicker => "Flicker",
            MotionKind::FocusIn => "Focus in",
            MotionKind::SoftRise => "Soft rise",
            MotionKind::Larger => "Scale from large",
            MotionKind::Smaller => "Scale from small",
        }
    }
}

/// A motion the picker offers by name: a kind with its timing, and for
/// the ones that go piece by piece, the stagger (#596).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionPreset {
    pub name: &'static str,
    pub group: &'static str,
    pub motion: Motion,
}

impl MotionPreset {
    /// Every motion kind as it comes, then the ones that read best piece
    /// by piece, letter, word or line at a time.
    pub fn all() -> Vec<MotionPreset> {
        let mut out: Vec<MotionPreset> = MotionKind::ALL
            .iter()
            .map(|&k| {
                let motion = match k {
                    // Typing is one letter at a time or it is not typing.
                    MotionKind::Typewriter => Motion::new(k).duration(0.02).stagger(StaggerUnit::Char, 0.04),
                    _ => Motion::new(k),
                };
                MotionPreset { name: k.label(), group: k.category(), motion }
            })
            .collect();
        let each = |name, kind, unit, secs| MotionPreset { name, group: "By letter or word", motion: Motion::new(kind).stagger(unit, secs) };
        out.extend([
            each("Fade letters", MotionKind::Fade, StaggerUnit::Char, 0.03),
            each("Fade words", MotionKind::Fade, StaggerUnit::Word, 0.08),
            each("Rise letters", MotionKind::Rise, StaggerUnit::Char, 0.03),
            each("Rise words", MotionKind::Rise, StaggerUnit::Word, 0.08),
            each("Rise lines", MotionKind::Rise, StaggerUnit::Line, 0.15),
            each("Pop letters", MotionKind::Pop, StaggerUnit::Char, 0.035),
            each("Pop words", MotionKind::Pop, StaggerUnit::Word, 0.08),
            each("Bouncing letters", MotionKind::Bounce, StaggerUnit::Char, 0.04),
            each("Slide words", MotionKind::SlideRight, StaggerUnit::Word, 0.08),
            each("Mask rise lines", MotionKind::MaskRise, StaggerUnit::Line, 0.12),
            each("Flicker letters", MotionKind::Flicker, StaggerUnit::Char, 0.025),
            MotionPreset { name: "Typewriter words", group: "By letter or word", motion: Motion::new(MotionKind::Typewriter).duration(0.02).stagger(StaggerUnit::Word, 0.12) },
        ]);
        out
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

/// Where a scaling motion grows or shrinks about (#787): a point of the
/// title's covered box, as fractions (0.5, 0.5 its centre), or the
/// deepest point inside the letters, so a word grown past the frame
/// fills it with a stroke rather than the gap between its lines.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionAnchor {
    Point { x: f32, y: f32 },
    Solid,
}

/// The longest a motion's one unit may take, in seconds: an hour, as
/// long as its delay may be, so a slow push into the words can run a
/// title's whole length (#1585). One longer than its title plays in part.
pub const MAX_DURATION_SECS: f32 = 3600.0;

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
    /// How far the scaling kinds scale when away from rest, as a factor
    /// of the resting size; `None` for the kind's own (#736).
    #[serde(default)]
    pub amount: Option<f32>,
    /// What a scaling motion scales about; the box's centre when `None`.
    #[serde(default)]
    pub anchor: Option<MotionAnchor>,
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
            amount: None,
            anchor: None,
        }
    }
}

impl Motion {
    pub fn new(kind: MotionKind) -> Self {
        Self { kind, easing: kind.default_easing(), ..Default::default() }
    }
    /// A new exit of `kind`, with the kind's exit easing.
    pub fn exit(kind: MotionKind) -> Self {
        Self::new(kind).as_exit()
    }
    /// This motion as an exit: an easing left at the kind's entrance
    /// default takes the kind's exit default; one chosen is kept.
    pub fn as_exit(mut self) -> Self {
        if self.easing == self.kind.default_easing() {
            self.easing = self.kind.default_exit_easing();
        }
        self
    }
    pub fn duration(mut self, secs: f32) -> Self { self.duration_secs = secs; self }
    pub fn delay(mut self, secs: f32) -> Self { self.delay_secs = secs; self }
    pub fn easing(mut self, easing: Easing) -> Self { self.easing = easing; self }
    pub fn stagger(mut self, unit: StaggerUnit, secs: f32) -> Self { self.stagger = unit; self.stagger_secs = secs; self }
    pub fn distance(mut self, fraction: f32) -> Self { self.distance = fraction; self }
    pub fn amount(mut self, factor: f32) -> Self { self.amount = Some(factor); self }

    /// The pose of a unit at eased progress `p` (0 = fully away, 1 =
    /// resting): (alpha, dx, dy, scale, blur sigma, clip to rest box).
    /// Offsets are in pixels for a font of `font_px`.
    pub fn pose(&self, p: f32, font_px: f32) -> (f32, f32, f32, f32, f32, bool) {
        let travel = font_px * self.distance;
        let away = |own: f32| self.amount.unwrap_or(own);
        let scale_from = |from: f32| from + (1.0 - from) * p;
        match self.kind {
            MotionKind::Fade => (p, 0.0, 0.0, 1.0, 0.0, false),
            MotionKind::Rise => (p, 0.0, (1.0 - p) * travel, 1.0, 0.0, false),
            MotionKind::Drop => (p, 0.0, -(1.0 - p) * travel, 1.0, 0.0, false),
            MotionKind::SlideLeft => (p, (1.0 - p) * travel * 1.5, 0.0, 1.0, 0.0, false),
            MotionKind::SlideRight => (p, -(1.0 - p) * travel * 1.5, 0.0, 1.0, 0.0, false),
            MotionKind::ScaleUp => (p, 0.0, 0.0, scale_from(away(0.6)), 0.0, false),
            MotionKind::ScaleDown => (p, 0.0, 0.0, scale_from(away(1.4)), 0.0, false),
            MotionKind::Larger => (1.0, 0.0, 0.0, scale_from(away(4.0)), 0.0, false),
            MotionKind::Smaller => (1.0, 0.0, 0.0, scale_from(away(0.0)), 0.0, false),
            MotionKind::Blur => (p, 0.0, 0.0, 1.0, (1.0 - p) * font_px * 0.12, false),
            MotionKind::MaskRise => (1.0, 0.0, (1.0 - p) * font_px * 1.15, 1.0, 0.0, true),
            MotionKind::MaskDrop => (1.0, 0.0, -(1.0 - p) * font_px * 1.15, 1.0, 0.0, true),
            MotionKind::Typewriter => (if p > 0.0 { 1.0 } else { 0.0 }, 0.0, 0.0, 1.0, 0.0, false),
            MotionKind::Pop => ((p * 3.0).min(1.0), 0.0, 0.0, 0.4 + 0.6 * p, 0.0, false),
            MotionKind::Slam => ((p * 2.5).min(1.0), 0.0, 0.0, 2.6 - 1.6 * p, 0.0, false),
            MotionKind::FlyLeft => ((p * 3.0).min(1.0), -(1.0 - p) * travel * 8.0, 0.0, 1.0, 0.0, false),
            MotionKind::FlyRight => ((p * 3.0).min(1.0), (1.0 - p) * travel * 8.0, 0.0, 1.0, 0.0, false),
            MotionKind::FlyTop => ((p * 3.0).min(1.0), 0.0, -(1.0 - p) * travel * 6.0, 1.0, 0.0, false),
            MotionKind::FlyBottom => ((p * 3.0).min(1.0), 0.0, (1.0 - p) * travel * 6.0, 1.0, 0.0, false),
            MotionKind::Bounce => ((p * 4.0).min(1.0), 0.0, -(1.0 - p) * travel * 2.5, 1.0, 0.0, false),
            MotionKind::Flicker => (flicker(p), 0.0, 0.0, 1.0, 0.0, false),
            MotionKind::FocusIn => (p, 0.0, 0.0, 1.25 - 0.25 * p, (1.0 - p) * font_px * 0.14, false),
            MotionKind::SoftRise => (p, 0.0, (1.0 - p) * travel, 1.0, (1.0 - p) * font_px * 0.08, false),
        }
    }
}

/// A failing sign coming on at progress `p`: dark and lit in steps, lit
/// more and more often, and steadily lit at the end. The same numbers on
/// every machine: a hash of the step, not a random number.
fn flicker(p: f32) -> f32 {
    if p >= 1.0 {
        return 1.0;
    }
    if p <= 0.0 {
        return 0.0;
    }
    let step = (p.max(0.0) * 14.0).floor() as u32;
    let r = (step.wrapping_mul(2_654_435_761) >> 8) as f32 / (1u32 << 24) as f32;
    if r < p * 1.15 { 1.0 } else { 0.12 }
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
    /// The anchor of the motion under way at eased progress (`pe`, `px`):
    /// the exit's while it runs, else the entrance's.
    pub fn anchor_at(&self, pe: f32, px: f32) -> Option<MotionAnchor> {
        match (self.enter, self.exit) {
            (_, Some(m)) if px < 1.0 => m.anchor,
            (Some(m), _) if pe < 1.0 => m.anchor,
            _ => None,
        }
    }

    /// The eased progress of unit `unit` at `t` seconds into a clip of
    /// `window` seconds (unknown window: no exit). 1 = resting. Each
    /// motion's easing runs over its own time: an exit's from its start to
    /// its end, so `in_cubic` leaves slowly first, as it arrives slowly
    /// first on an entrance (#1566).
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
                let left = (remaining - m.delay_secs - unit as f32 * m.stagger_secs) / m.duration_secs.max(0.001);
                // How far through the exit, eased: how far it has gone.
                1.0 - m.easing.apply(1.0 - left)
            }
            _ => 1.0,
        };
        (enter, exit)
    }
}



// ─── Animators: part of the text, in any order, unit by unit ─────────

/// An animator (#1510): a range of a text layer's characters, words or
/// lines, picked out by its selector, and what it does to them at full
/// selection. Each unit takes the change in proportion to how much of
/// it the selector picks, on top of the layer's entrance and exit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Animator {
    #[serde(default)]
    pub name: String,
    #[serde(default = "animator_on", skip_serializing_if = "animator_is_on")]
    pub enabled: bool,
    #[serde(default)]
    pub selector: RangeSelector,
    #[serde(default)]
    pub props: AnimatorProps,
    /// A second selector the first is limited to: a unit's weight is the
    /// two weights multiplied, so a random order can play out inside
    /// one word, the other words untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<RangeSelector>,
}

fn animator_on() -> bool {
    true
}
fn animator_is_on(b: &bool) -> bool {
    *b
}

impl Default for Animator {
    fn default() -> Self {
        Self { name: String::new(), enabled: true, selector: RangeSelector::default(), props: AnimatorProps::default(), within: None }
    }
}

/// The units an animator picks from, counted as a staggered motion
/// counts them: characters (spaces not counted), words or lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SelectUnit {
    #[default]
    Char,
    Word,
    Line,
}

impl SelectUnit {
    /// The stagger unit counted the same way.
    pub fn stagger(self) -> StaggerUnit {
        match self {
            SelectUnit::Char => StaggerUnit::Char,
            SelectUnit::Word => StaggerUnit::Word,
            SelectUnit::Line => StaggerUnit::Line,
        }
    }
}

/// How the selection rises and falls across its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SelectShape {
    /// Whole inside the range, a unit at its edge by how much of it is in.
    #[default]
    Square,
    /// Rising from the range's start to its end.
    RampUp,
    /// Falling from the range's start to its end.
    RampDown,
    /// Rising to the range's middle and falling again.
    Triangle,
    /// A half circle over the range.
    Round,
    /// A cosine bump over the range.
    Smooth,
}

/// The order the units are laid along the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SelectOrder {
    #[default]
    Forward,
    Reverse,
    /// The middle unit first, then outwards, the left before the right.
    FromCentre,
    /// Shuffled by `seed`: the same order on every render of the seed.
    Random { seed: u32 },
}

/// Which units an animator picks and how much: the range from `start`
/// to `end`, moved by `offset`, as fractions of the units in `order`,
/// each keyable; the weight shaped by `shape`, eased by `easing` and
/// scaled by `amount` (a negative amount picks the units outside).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeSelector {
    #[serde(default)]
    pub unit: SelectUnit,
    #[serde(default)]
    pub start: f32,
    #[serde(default = "selector_end")]
    pub end: f32,
    #[serde(default)]
    pub offset: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub start_keyframes: Vec<crate::keyframe::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub end_keyframes: Vec<crate::keyframe::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offset_keyframes: Vec<crate::keyframe::Keyframe>,
    #[serde(default)]
    pub shape: SelectShape,
    #[serde(default = "selector_easing")]
    pub easing: Easing,
    #[serde(default)]
    pub order: SelectOrder,
    #[serde(default = "selector_amount")]
    pub amount: f32,
}

fn selector_end() -> f32 {
    1.0
}
fn selector_easing() -> Easing {
    Easing::Linear
}
fn selector_amount() -> f32 {
    1.0
}

impl Default for RangeSelector {
    fn default() -> Self {
        Self {
            unit: SelectUnit::Char,
            start: 0.0,
            end: 1.0,
            offset: 0.0,
            start_keyframes: Vec::new(),
            end_keyframes: Vec::new(),
            offset_keyframes: Vec::new(),
            shape: SelectShape::Square,
            easing: Easing::Linear,
            order: SelectOrder::Forward,
            amount: 1.0,
        }
    }
}

/// What an animator does to a unit it picks wholly; a unit picked by a
/// weight `w` takes `w` of it. Offsets are in font sizes, blur in
/// font sizes too, the colour blended toward in linear light.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimatorProps {
    #[serde(default)]
    pub dx: f32,
    #[serde(default)]
    pub dy: f32,
    #[serde(default = "selector_amount")]
    pub scale: f32,
    /// Degrees, clockwise, about the unit's own centre.
    #[serde(default)]
    pub rotation: f32,
    #[serde(default = "selector_amount")]
    pub opacity: f32,
    #[serde(default)]
    pub blur: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<[u8; 4]>,
    /// A box of this colour behind each word picked, as far as it is
    /// picked (#2710): the spoken word of an animated caption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backing: Option<[u8; 4]>,
}

impl AnimatorProps {
    /// Whether it paints: a colour or a backing, which the text's
    /// raster draws per unit.
    pub fn paints(&self) -> bool {
        self.colour.is_some() || self.backing.is_some()
    }
}

impl Default for AnimatorProps {
    fn default() -> Self {
        Self { dx: 0.0, dy: 0.0, scale: 1.0, rotation: 0.0, opacity: 1.0, blur: 0.0, colour: None, backing: None }
    }
}

/// A glyph's pose from its animators: offset in pixels, scale, rotation
/// in degrees, opacity, blur in pixels, and the colour it is blended
/// toward with how much of it (in linear light, applied to the glyph's
/// own colour by [`UnitPose::colour_of`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnitPose {
    pub dx: f32,
    pub dy: f32,
    pub scale: f32,
    pub rotation: f32,
    pub alpha: f32,
    pub blur: f32,
    pub colour: Option<([f32; 4], f32)>,
    /// The box behind the unit (#2710): its sRGB colour and how much of
    /// it shows, 0 to 1.
    pub backing: Option<([u8; 4], f32)>,
}

impl Default for UnitPose {
    fn default() -> Self {
        Self { dx: 0.0, dy: 0.0, scale: 1.0, rotation: 0.0, alpha: 1.0, blur: 0.0, colour: None, backing: None }
    }
}

impl UnitPose {
    /// Whether the pose changes nothing.
    pub fn is_rest(&self) -> bool {
        *self == UnitPose::default()
    }

    /// `base`, an sRGB colour, as the pose blends it.
    pub fn colour_of(&self, base: [u8; 4]) -> [u8; 4] {
        let Some((target, mix)) = self.colour else { return base };
        let from = linear_rgba(base);
        let mut out = [0u8; 4];
        for i in 0..4 {
            let v = from[i] + (target[i] - from[i]) * mix;
            out[i] = if i == 3 { (v.clamp(0.0, 1.0) * 255.0).round() as u8 } else { srgb_byte(v) };
        }
        out
    }
}

/// An sRGB colour's channels in linear light, alpha as it is, all 0..1.
fn linear_rgba(c: [u8; 4]) -> [f32; 4] {
    let lin = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [lin(c[0]), lin(c[1]), lin(c[2]), c[3] as f32 / 255.0]
}

/// Linear light, 0..1, as an sRGB byte.
fn srgb_byte(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let c = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round() as u8
}

/// The animator presets' ids, in order, as edit operations and the
/// Title tab name them (#1510, #1701).
pub fn animator_preset_ids() -> Vec<&'static str> {
    animator_presets().iter().map(|p| p.id.as_str()).collect()
}

/// The preset `name` for a title `clip_secs` long, its keys placed on
/// the clip; `None` for a name that is not one.
pub fn animator_preset(name: &str, clip_secs: f64) -> Option<Animator> {
    let p = animator_presets().iter().find(|p| p.id == name)?;
    let clip = clip_secs.max(0.1);
    let mut selector = RangeSelector { unit: p.selector.unit, start: p.selector.start, end: p.selector.end, offset: p.selector.offset, shape: p.selector.shape, easing: p.selector.easing, order: p.selector.order, amount: p.selector.amount, ..Default::default() };
    for k in &p.keys {
        let time_secs = match k.at {
            KeyAt::Start => 0.0,
            KeyAt::Entrance => clip.min(1.2),
            KeyAt::End => clip,
        };
        let key = crate::keyframe::Keyframe { time_secs, value: k.value, interpolation: crate::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        match k.on {
            KeyOn::Start => selector.start_keyframes.push(key),
            KeyOn::End => selector.end_keyframes.push(key),
            KeyOn::Offset => selector.offset_keyframes.push(key),
        }
    }
    let q = &p.props;
    let props = AnimatorProps { dx: q.dx, dy: q.dy, scale: q.scale, rotation: q.rotation, opacity: q.opacity, blur: q.blur, colour: q.colour, backing: q.backing };
    Some(Animator { name: p.name.clone(), selector, props, ..Default::default() })
}

/// An animator preset as `assets/presets/animators` writes it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnimatorPreset {
    id: String,
    name: String,
    #[serde(default)]
    selector: PresetSelector,
    #[serde(default)]
    props: PresetProps,
    #[serde(default)]
    keys: Vec<PresetKey>,
}

/// A preset's selector without its keys, which `keys` places.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
struct PresetSelector {
    unit: SelectUnit,
    start: f32,
    end: f32,
    offset: f32,
    shape: SelectShape,
    easing: Easing,
    order: SelectOrder,
    amount: f32,
}

impl Default for PresetSelector {
    fn default() -> Self {
        let d = RangeSelector::default();
        Self { unit: d.unit, start: d.start, end: d.end, offset: d.offset, shape: d.shape, easing: d.easing, order: d.order, amount: d.amount }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
struct PresetProps {
    dx: f32,
    dy: f32,
    scale: f32,
    rotation: f32,
    opacity: f32,
    blur: f32,
    colour: Option<[u8; 4]>,
    backing: Option<[u8; 4]>,
}

impl Default for PresetProps {
    fn default() -> Self {
        Self { dx: 0.0, dy: 0.0, scale: 1.0, rotation: 0.0, opacity: 1.0, blur: 0.0, colour: None, backing: None }
    }
}

/// A key on the range's `on` at `at` on the clip.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PresetKey {
    on: KeyOn,
    at: KeyAt,
    value: f32,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum KeyOn {
    Start,
    End,
    Offset,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum KeyAt {
    Start,
    Entrance,
    End,
}

/// The most keys one preset places.
const MAX_PRESET_KEYS: usize = 64;

/// The presets folder a person's own animator presets are read from.
pub const ANIMATOR_PRESETS_FOLDER: &str = "animators";

/// Why an animator preset someone wrote can't be offered, beyond what
/// [`crate::preset_file::merge`] asks of its id: an id that isn't a
/// lowercase word, no name or one past its length, too many keys, or a
/// number outside what the animator op takes (start and end 0 to 1,
/// offset and amount -1 to 1, a move ±20 font sizes, scale 0 to 20,
/// rotation ±3600°, opacity 0 to 1, blur 0 to 4 font sizes).
fn user_animator_fault(p: &AnimatorPreset) -> Result<(), String> {
    let word = !p.id.is_empty() && p.id.len() <= 32 && p.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !word {
        return Err("an id of lowercase letters, digits and _, up to 32".into());
    }
    let name = p.name.trim();
    if name.is_empty() || name.chars().count() > crate::preset_file::MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(format!("a name of 1 to {} characters", crate::preset_file::MAX_NAME_CHARS));
    }
    if p.keys.len() > MAX_PRESET_KEYS {
        return Err(format!("at most {MAX_PRESET_KEYS} keys"));
    }
    let (s, q) = (&p.selector, &p.props);
    let key_range = |k: &PresetKey| match k.on {
        KeyOn::Offset => ("an offset key", k.value, -1.0, 1.0),
        KeyOn::Start => ("a start key", k.value, 0.0, 1.0),
        KeyOn::End => ("an end key", k.value, 0.0, 1.0),
    };
    let values = [
        ("start", s.start, 0.0, 1.0), ("end", s.end, 0.0, 1.0), ("offset", s.offset, -1.0, 1.0), ("amount", s.amount, -1.0, 1.0),
        ("dx", q.dx, -20.0, 20.0), ("dy", q.dy, -20.0, 20.0), ("scale", q.scale, 0.0, 20.0), ("rotation", q.rotation, -3600.0, 3600.0),
        ("opacity", q.opacity, 0.0, 1.0), ("blur", q.blur, 0.0, 4.0),
    ];
    match values.into_iter().chain(p.keys.iter().map(key_range)).find(|(_, v, lo, hi)| !(*lo..=*hi).contains(v)) {
        Some((what, v, lo, hi)) => Err(format!("{what} {v} is outside {lo} to {hi}")),
        None => Ok(()),
    }
}

/// Klipp's animator presets, then those in `dirs` (a person's folder,
/// then the packs'), each read as someone else's (#3095), by id: one
/// whose id or name is that of one before it is skipped, said.
pub(crate) fn animator_preset_listing_in(dirs: &[std::path::PathBuf]) -> crate::preset_file::Merged<AnimatorPreset> {
    let built_in = read_animator_presets(include_str!("../../../assets/presets/animators/builtin.toml")).unwrap_or_else(|why| {
        tracing::error!("the built-in animator presets don't read: {why}");
        Vec::new()
    });
    let mut merged = crate::preset_file::merge(built_in, dirs, "animator_preset", 1, "preset", |p: &AnimatorPreset| &p.id, user_animator_fault);
    // The id is what the op takes; the name is what the Title tab shows,
    // and two of one name there are one too many.
    let mut kept: Vec<AnimatorPreset> = merged.entries.drain(..merged.built_in).collect();
    for p in std::mem::take(&mut merged.entries) {
        if kept.iter().any(|k| k.name.trim().to_lowercase() == p.name.trim().to_lowercase()) {
            merged.skipped.push(format!("\"{}\": its name \"{}\" is that of one before it", p.id, p.name.trim()));
        } else {
            kept.push(p);
        }
    }
    merged.entries = kept;
    merged
}

/// Klipp's animator presets, then the person's own, read once a run;
/// what of theirs is skipped is said in the log.
fn animator_presets() -> &'static [AnimatorPreset] {
    static LISTING: crate::preset_file::Reloading<crate::preset_file::Merged<AnimatorPreset>> = crate::preset_file::Reloading::new();
    &LISTING
        .get(|| {
            let listing = animator_preset_listing_in(&crate::preset_places::dirs_for(ANIMATOR_PRESETS_FOLDER));
            for why in &listing.skipped {
                tracing::warn!("an animator preset of yours was skipped: {why}");
            }
            listing
        })
        .entries
}

/// The presets in `text`, or why it is refused: an id that isn't a
/// lowercase word or is given twice, no name, too many keys, or a
/// number that isn't finite.
fn read_animator_presets(text: &str) -> Result<Vec<AnimatorPreset>, String> {
    let presets: Vec<AnimatorPreset> = crate::preset_file::read(text, "animator_preset", 1, "preset")?;
    let mut ids = std::collections::HashSet::new();
    for p in &presets {
        let word = !p.id.is_empty() && p.id.len() <= 32 && p.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !word || !ids.insert(p.id.as_str()) {
            return Err(format!("preset \"{}\": an id of lowercase letters, digits and _, given once", p.id));
        }
        if p.name.trim().is_empty() {
            return Err(format!("preset \"{}\" needs a name", p.id));
        }
        if p.keys.len() > MAX_PRESET_KEYS {
            return Err(format!("preset \"{}\": at most {MAX_PRESET_KEYS} keys", p.id));
        }
        let (s, q) = (&p.selector, &p.props);
        let numbers = [s.start, s.end, s.offset, s.amount, q.dx, q.dy, q.scale, q.rotation, q.opacity, q.blur];
        if !numbers.iter().chain(p.keys.iter().map(|k| &k.value)).all(|v| v.is_finite()) {
            return Err(format!("preset \"{}\": a number that isn't finite", p.id));
        }
    }
    Ok(presets)
}

/// Each unit's place along the selector's range, 0 first: the order
/// laid over `n` units. Random shuffles by the seed alone (splitmix64
/// and Fisher–Yates), so the order is the same on every render.
pub fn select_ranks(order: SelectOrder, n: usize) -> Vec<usize> {
    let by_rank: Vec<usize> = match order {
        SelectOrder::Forward => (0..n).collect(),
        SelectOrder::Reverse => (0..n).rev().collect(),
        SelectOrder::FromCentre => {
            let mut units: Vec<usize> = (0..n).collect();
            let centre = (n as f32 - 1.0) * 0.5;
            // Stable: equally far, the left one first.
            units.sort_by(|a, b| (*a as f32 - centre).abs().total_cmp(&(*b as f32 - centre).abs()));
            units
        }
        SelectOrder::Random { seed } => {
            let mut state = seed as u64 ^ 0x9e37_79b9_7f4a_7c15;
            let mut next = || {
                state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                z ^ (z >> 31)
            };
            let mut units: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                let j = (next() % (i as u64 + 1)) as usize;
                units.swap(i, j);
            }
            units
        }
    };
    let mut rank = vec![0; n];
    for (r, &u) in by_rank.iter().enumerate() {
        rank[u] = r;
    }
    rank
}

impl RangeSelector {
    /// The range at `time_secs`, its keys evaluated: (from, to).
    pub fn range_at(&self, time_secs: f64) -> (f32, f32) {
        let key = |keys: &[crate::keyframe::Keyframe], v: f32| crate::keyframe::evaluate(keys, time_secs, v);
        let offset = key(&self.offset_keyframes, self.offset);
        (key(&self.start_keyframes, self.start) + offset, key(&self.end_keyframes, self.end) + offset)
    }

    /// How much of each of `n` units the selector picks at `time_secs`,
    /// 0..1 (`amount` may scale it beyond), in unit order.
    pub fn weights(&self, n: usize, time_secs: f64) -> Vec<f32> {
        if n == 0 {
            return Vec::new();
        }
        let (from, to) = self.range_at(time_secs);
        let (lo, hi) = (from.min(to), from.max(to));
        let ranks = select_ranks(self.order, n);
        let size = 1.0 / n as f32;
        ranks
            .iter()
            .map(|&r| {
                let (a, b) = (r as f32 * size, (r + 1) as f32 * size);
                let centre = (a + b) * 0.5;
                let span = hi - lo;
                // Where the unit's centre is across the range, 0..1.
                let across = if span > 0.0 { (centre - lo) / span } else { 0.0 };
                let inside = (lo..hi).contains(&centre);
                let w = match self.shape {
                    SelectShape::Square => ((b.min(hi) - a.max(lo)) / size).clamp(0.0, 1.0),
                    _ if !inside => 0.0,
                    SelectShape::RampUp => across,
                    SelectShape::RampDown => 1.0 - across,
                    SelectShape::Triangle => 1.0 - (2.0 * across - 1.0).abs(),
                    SelectShape::Round => (1.0 - (2.0 * across - 1.0).powi(2)).max(0.0).sqrt(),
                    SelectShape::Smooth => 0.5 - 0.5 * (std::f32::consts::TAU * across).cos(),
                };
                let w = self.easing.apply(w.clamp(0.0, 1.0));
                if self.amount < 0.0 { (1.0 - w) * -self.amount } else { w * self.amount }
            })
            .collect()
    }
}

impl Animator {
    /// `pose` with this animator's change for a unit picked by `w`
    /// laid on top, at a font of `font_px`: offsets add, scale and
    /// opacity multiply, rotation adds, blur takes the larger, colour
    /// blends on toward this one's.
    pub fn apply(&self, w: f32, font_px: f32, pose: &mut UnitPose) {
        if !self.enabled || w == 0.0 {
            return;
        }
        let p = &self.props;
        pose.dx += p.dx * font_px * w;
        pose.dy += p.dy * font_px * w;
        pose.scale *= 1.0 + (p.scale - 1.0) * w;
        pose.rotation += p.rotation * w;
        pose.alpha *= (1.0 + (p.opacity - 1.0) * w).clamp(0.0, 1.0);
        pose.blur = pose.blur.max(p.blur * font_px * w.abs());
        if let Some(c) = p.colour {
            let target = linear_rgba(c);
            let mix = w.clamp(0.0, 1.0);
            pose.colour = Some(match pose.colour {
                None => (target, mix),
                Some((before, m)) => {
                    let mut t = [0.0; 4];
                    for i in 0..4 {
                        t[i] = before[i] + (target[i] - before[i]) * mix;
                    }
                    (t, 1.0 - (1.0 - m) * (1.0 - mix))
                }
            });
        }
        if let Some(b) = p.backing {
            let mix = w.clamp(0.0, 1.0);
            if mix > pose.backing.map_or(0.0, |(_, m)| m) {
                pose.backing = Some((b, mix));
            }
        }
    }
}

/// Every unit's pose from a layer's `animators` at `time_secs`, for
/// glyphs whose characters are `parents` (in layout order), at a font of
/// `font_px`: one per glyph. The one evaluation both render paths use.
pub fn animator_poses(animators: &[Animator], parents: &[char], font_px: f32, time_secs: f64) -> Vec<UnitPose> {
    let mut poses = vec![UnitPose::default(); parents.len()];
    // Each glyph's weight under `selector`, by the unit it is in.
    let weigh = |selector: &RangeSelector| -> Vec<f32> {
        let units = units_of(parents, selector.unit.stagger());
        let n = units.iter().zip(parents).filter(|(_, c)| !c.is_whitespace()).map(|(u, _)| u + 1).max().unwrap_or(0);
        let weights = selector.weights(n, time_secs);
        units.iter().map(|&u| weights.get(u).copied().unwrap_or(0.0)).collect()
    };
    for a in animators.iter().filter(|a| a.enabled) {
        let mut weights = weigh(&a.selector);
        if let Some(within) = &a.within {
            for (w, l) in weights.iter_mut().zip(weigh(within)) {
                *w *= l;
            }
        }
        for ((pose, w), c) in poses.iter_mut().zip(weights).zip(parents) {
            if !c.is_whitespace() {
                a.apply(w, font_px, pose);
            }
        }
    }
    poses
}

/// Which unit each glyph belongs to for `unit`: the line, word or
/// non-space character it is in, by its character in `parents`.
pub fn units_of(parents: &[char], unit: StaggerUnit) -> Vec<usize> {
    let mut out = Vec::with_capacity(parents.len());
    let (mut line, mut word, mut ch) = (0usize, 0usize, 0usize);
    let mut in_word = false;
    for &c in parents {
        if c == '\n' {
            line += 1;
            in_word = false;
            out.push(match unit { StaggerUnit::None => 0, StaggerUnit::Line => line, StaggerUnit::Word => word.saturating_sub(1), StaggerUnit::Char => ch });
            continue;
        }
        if c.is_whitespace() {
            in_word = false;
        } else if !in_word {
            in_word = true;
            word += 1;
        }
        out.push(match unit { StaggerUnit::None => 0, StaggerUnit::Line => line, StaggerUnit::Word => word.saturating_sub(1), StaggerUnit::Char => ch });
        if !c.is_whitespace() {
            ch += 1;
        }
    }
    out
}

// ─── Loops: what a layer keeps doing while it is on screen ───────────

/// Direction of a scrolling loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScrollDirection {
    #[default]
    Up,
    Down,
}

/// Direction of a crawling loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CrawlDirection {
    #[default]
    Left,
    Right,
}

/// A layer fading out towards the frame's edges, whatever it is and
/// wherever it moves: over a zone at each edge, as a fraction of the
/// frame (0 to 0.5, 0 for none). A lower third softening into the left
/// edge, credits fading in at the bottom and out at the top.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EdgeFade {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

impl EdgeFade {
    pub const NONE: Self = Self { top: 0.0, bottom: 0.0, left: 0.0, right: 0.0 };

    /// Whether it fades nothing.
    pub fn is_none(&self) -> bool {
        self.top <= 0.0 && self.bottom <= 0.0 && self.left <= 0.0 && self.right <= 0.0
    }
}

/// How a travelling layer (a credits roll, a ticker) changes with where
/// it is along its travel. Stored on the Scroll or Crawl loop; the
/// default changes nothing, so a roll saved before it plays as it did.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RollShape {
    /// A band around the frame's middle where the layer is at full
    /// strength; away from it the layer thins towards the edges.
    pub focus: Option<FocusBand>,
}

/// The focus band of a [`RollShape`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusBand {
    /// How much of the frame, around its middle, is at full strength
    /// (0 to 1).
    pub band: f32,
    /// The layer's opacity at the frame's edges (0 to 1).
    pub edge_opacity: f32,
    /// How soft it goes at the frame's edges: a blur, in pixels of a
    /// 1080p frame, mixed in away from the band. 0 for none.
    pub blur: f32,
}

impl Default for FocusBand {
    fn default() -> Self {
        Self { band: 0.4, edge_opacity: 0.15, blur: 6.0 }
    }
}

impl FocusBand {
    /// How much of the blurred layer shows at `u` along the axis: none
    /// inside the band, all of it at the frame's edges. The twin of
    /// klipp-effects' `FocusBlur` mix.
    pub fn blur_mix(&self, u: f32) -> f32 {
        let d = (u - 0.5).abs() * 2.0;
        let t = ((d - self.band) / (1.0 - self.band).max(0.001)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

/// The frame axis a focus band runs along: down it for a roll, across
/// it for a crawl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RampAxis {
    Vertical,
    Horizontal,
}

/// A layer's fade by where it is on the frame, ready to apply: its
/// [`EdgeFade`] and a travelling loop's [`FocusBand`] on its axis. The
/// CPU title renderer and the GPU sprite shader evaluate
/// [`ScreenFade::alpha_at`] alike, so the viewer and an export fade the
/// same.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenFade {
    pub edges: EdgeFade,
    pub focus: Option<(RampAxis, FocusBand)>,
}

/// GLSL/WGSL `smoothstep`, for an `edge1` above `edge0`.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl ScreenFade {
    /// The fade of a layer with `edges` and a loop's `focus`, or `None`
    /// when it fades nothing.
    pub fn new(edges: EdgeFade, focus: Option<(RampAxis, FocusBand)>) -> Option<Self> {
        let clamp = |z: f32| z.clamp(0.0, 0.5);
        let edges = EdgeFade { top: clamp(edges.top), bottom: clamp(edges.bottom), left: clamp(edges.left), right: clamp(edges.right) };
        let focus = focus.map(|(axis, f)| (axis, FocusBand { band: f.band.clamp(0.0, 0.999), edge_opacity: f.edge_opacity.clamp(0.0, 1.0), blur: f.blur.max(0.0) }));
        (!edges.is_none() || focus.is_some()).then_some(Self { edges, focus })
    }

    /// The factor down the frame at `uy` (0 top, 1 bottom): the top
    /// and bottom zones, and a vertical focus band.
    pub fn along_y(&self, uy: f32) -> f32 {
        let mut a = 1.0;
        if self.edges.top > 0.0 {
            a *= smoothstep(0.0, self.edges.top, uy);
        }
        if self.edges.bottom > 0.0 {
            a *= smoothstep(0.0, self.edges.bottom, 1.0 - uy);
        }
        if let Some((RampAxis::Vertical, f)) = self.focus {
            a *= focus_factor(f, uy);
        }
        a
    }

    /// The factor across the frame at `ux` (0 left, 1 right).
    pub fn along_x(&self, ux: f32) -> f32 {
        let mut a = 1.0;
        if self.edges.left > 0.0 {
            a *= smoothstep(0.0, self.edges.left, ux);
        }
        if self.edges.right > 0.0 {
            a *= smoothstep(0.0, self.edges.right, 1.0 - ux);
        }
        if let Some((RampAxis::Horizontal, f)) = self.focus {
            a *= focus_factor(f, ux);
        }
        a
    }

    /// The opacity at `(ux, uy)`, fractions of the frame. Mirrored by
    /// `shape_alpha` in klipp-effects' sprite shader.
    pub fn alpha_at(&self, ux: f32, uy: f32) -> f32 {
        self.along_y(uy) * self.along_x(ux)
    }

    /// Scale the alpha of a straight-alpha RGBA8 frame by the fade, at
    /// each pixel's centre.
    pub fn apply_rgba8(&self, rgba: &mut [u8], width: u32, height: u32) {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 {
            return;
        }
        let cols: Vec<f32> = (0..w).map(|x| self.along_x((x as f32 + 0.5) / w as f32)).collect();
        for (y, row) in rgba.chunks_exact_mut(w * 4).take(h).enumerate() {
            let ry = self.along_y((y as f32 + 0.5) / h as f32);
            for (px, cx) in row.chunks_exact_mut(4).zip(&cols) {
                let a = ry * cx;
                if a < 1.0 && px[3] > 0 {
                    px[3] = (px[3] as f32 * a).round() as u8;
                }
            }
        }
    }
}

/// A focus band at `u` along its axis: whole inside the band, thinning
/// to its edge opacity at the frame's edges.
fn focus_factor(f: FocusBand, u: f32) -> f32 {
    let d = (u - 0.5).abs() * 2.0;
    1.0 - smoothstep(f.band, 1.0, d) * (1.0 - f.edge_opacity)
}

/// One continuous behaviour of a text layer. A layer holds a list of
/// these and they all run at once, on top of its entrance and exit:
/// a title can blink while it crawls, or pulse while it counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TitleLoop {
    /// Hard on/off at `rate_hz`, visible for `duty` of each period.
    Blink { rate_hz: f32, duty: f32 },
    /// Smooth opacity swell between `min_opacity` and 1.
    Pulse { rate_hz: f32, min_opacity: f32 },
    /// The whole layer jitters, `amplitude` in font sizes.
    Shake { amplitude: f32, rate_hz: f32 },
    /// Each glyph bobs on a travelling sine, `amplitude` in font sizes.
    Wave { amplitude: f32, rate_hz: f32 },
    /// The fill hue rotates once per `1 / rate_hz` seconds.
    ColorCycle { rate_hz: f32 },
    /// The fill swings between two colours.
    ColorBlend { color_a: [u8; 4], color_b: [u8; 4], rate_hz: f32 },
    /// Credits roll: the layer travels through the frame. A speed of
    /// zero fits the whole travel to the clip.
    Scroll {
        direction: ScrollDirection,
        speed_px_per_sec: f32,
        /// How it fades with where it is on screen.
        #[serde(default)]
        shape: RollShape,
    },
    /// Ticker: the layer travels across the frame. `repeat` starts it
    /// again as soon as it has left.
    Crawl {
        direction: CrawlDirection,
        speed_px_per_sec: f32,
        repeat: bool,
        /// How it fades with where it is on screen.
        #[serde(default)]
        shape: RollShape,
    },
    /// The layer bobs gently up and down, `amplitude` in font sizes.
    Float { amplitude: f32, rate_hz: f32 },
    /// The layer sways from side to side, `amplitude` in font sizes.
    Sway { amplitude: f32, rate_hz: f32 },
    /// The layer flickers like a neon sign: now and then it dips to
    /// `depth` of its opacity, `rate_hz` times a second at most.
    Flicker { rate_hz: f32, depth: f32 },
    /// The layer drifts slowly over the clip, `dx` and `dy` as fractions
    /// of the frame from start to end, through its resting place halfway.
    Drift { dx: f32, dy: f32 },
    /// The text becomes a number counting from `from` to `to`.
    Counter { from: f64, to: f64, duration_secs: f32, decimals: u8 },
}

impl TitleLoop {
    /// One of each, with sensible values, for pickers.
    pub fn catalogue() -> Vec<TitleLoop> {
        vec![
            TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0, shape: Default::default() },
            TitleLoop::Crawl { direction: CrawlDirection::Left, speed_px_per_sec: 0.0, repeat: false, shape: Default::default() },
            TitleLoop::Blink { rate_hz: 2.0, duty: 0.5 },
            TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.4 },
            TitleLoop::Shake { amplitude: 0.05, rate_hz: 12.0 },
            TitleLoop::Wave { amplitude: 0.12, rate_hz: 1.0 },
            TitleLoop::ColorCycle { rate_hz: 0.25 },
            TitleLoop::ColorBlend { color_a: [255, 255, 255, 255], color_b: [255, 200, 0, 255], rate_hz: 0.5 },
            TitleLoop::Counter { from: 0.0, to: 100.0, duration_secs: 3.0, decimals: 0 },
        ]
    }

    /// The picker's group for this loop: what it keeps doing.
    pub fn category(&self) -> &'static str {
        match self {
            TitleLoop::Scroll { .. } | TitleLoop::Crawl { .. } => "Travel",
            TitleLoop::Blink { .. } | TitleLoop::Pulse { .. } => "Opacity",
            TitleLoop::Shake { .. } | TitleLoop::Wave { .. } => "Motion",
            TitleLoop::ColorCycle { .. } | TitleLoop::ColorBlend { .. } => "Colour",
            TitleLoop::Counter { .. } => "Text",
            TitleLoop::Float { .. } | TitleLoop::Sway { .. } | TitleLoop::Drift { .. } => "Motion",
            TitleLoop::Flicker { .. } => "Opacity",
        }
    }

    /// The loops the picker offers by name: every kind, and a few set up
    /// for a common use (a strobe, a repeating ticker, a rainbow).
    pub fn presets() -> Vec<(&'static str, TitleLoop)> {
        let mut out: Vec<(&'static str, TitleLoop)> = TitleLoop::catalogue().into_iter().map(|l| (l.label(), l)).collect();
        out.extend([
            ("Float", TitleLoop::Float { amplitude: 0.08, rate_hz: 0.5 }),
            ("Sway", TitleLoop::Sway { amplitude: 0.1, rate_hz: 0.4 }),
            ("Flicker", TitleLoop::Flicker { rate_hz: 6.0, depth: 0.25 }),
            ("Drift", TitleLoop::Drift { dx: -0.04, dy: 0.0 }),
            ("Strobe", TitleLoop::Blink { rate_hz: 8.0, duty: 0.5 }),
            ("Breathe", TitleLoop::Pulse { rate_hz: 0.3, min_opacity: 0.55 }),
            ("Ticker", TitleLoop::Crawl { direction: CrawlDirection::Left, speed_px_per_sec: 160.0, repeat: true, shape: Default::default() }),
            ("Rainbow", TitleLoop::ColorCycle { rate_hz: 0.5 }),
        ]);
        out
    }

    pub fn label(&self) -> &'static str {
        match self {
            TitleLoop::Blink { .. } => "Blink",
            TitleLoop::Pulse { .. } => "Pulse",
            TitleLoop::Shake { .. } => "Shake",
            TitleLoop::Wave { .. } => "Wave",
            TitleLoop::ColorCycle { .. } => "Colour cycle",
            TitleLoop::ColorBlend { .. } => "Colour blend",
            TitleLoop::Scroll { .. } => "Scroll",
            TitleLoop::Crawl { .. } => "Crawl",
            TitleLoop::Counter { .. } => "Counter",
            TitleLoop::Float { .. } => "Float",
            TitleLoop::Sway { .. } => "Sway",
            TitleLoop::Flicker { .. } => "Flicker",
            TitleLoop::Drift { .. } => "Drift",
        }
    }

    /// True when the two are the same kind of loop.
    pub fn same_kind(&self, other: &TitleLoop) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// What the loops of a layer add up to at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopPose {
    /// Multiplier on the layer's opacity.
    pub opacity: f32,
    /// Offset of the whole layer in frame pixels.
    pub dx: f32,
    pub dy: f32,
    /// Per-glyph bob: (amplitude in pixels, rate in Hz).
    pub wave: Option<(f32, f32)>,
    /// A fill colour replacing the layer's own.
    pub color: Option<[u8; 4]>,
    /// Text replacing the layer's own.
    pub text: Option<String>,
    /// A roll's or a crawl's focus band, on the axis it travels.
    pub focus: Option<(RampAxis, FocusBand)>,
}

impl Default for LoopPose {
    fn default() -> Self {
        Self { opacity: 1.0, dx: 0.0, dy: 0.0, wave: None, color: None, text: None, focus: None }
    }
}

/// The frame and text geometry the travelling loops need, in pixels.
#[derive(Debug, Clone, Copy)]
pub struct LoopGeometry {
    pub frame_w: f32,
    pub frame_h: f32,
    pub text_w: f32,
    pub text_h: f32,
    pub font_px: f32,
}

/// How much bigger than on a 1080p frame a title's pixel measures are
/// drawn: by the frame's short side, so a vertical frame reads as a
/// landscape one of the same resolution. `klipp_title::frame_scale` is
/// this.
pub fn frame_scale(frame_w: f32, frame_h: f32) -> f32 {
    frame_w.min(frame_h).max(1.0) / 1080.0
}

/// How far a travelling loop carries its layer, in the pixels `frame`
/// and `extent` are given in: from just past one edge of the frame to
/// just past the other, the layer's own length included.
pub fn travel_distance(frame: f32, extent: f32) -> f32 {
    frame * 1.04 + extent
}

/// Seconds a travel of `distance` takes at `speed` (the same pixels per
/// second), or at the clip's pace when `speed` is zero: fitted, the
/// whole travel takes the clip.
pub fn travel_secs(distance: f32, speed: f32, clip_secs: f32) -> f32 {
    if speed > 0.0 {
        distance / speed
    } else {
        clip_secs
    }
}

/// Where a travelling loop is at `t`: from `+offset` to `-offset`,
/// over the clip when `speed` is zero, else at `speed` px/s centred in
/// the clip when it fits and starting at once when it does not.
fn travel(offset: f32, speed: f32, t: f32, window: Option<f32>, repeat: bool) -> f32 {
    let clip = window.unwrap_or(5.0).max(0.001);
    let full = 2.0 * offset;
    if speed <= 0.0 {
        return offset - full * (t / clip).clamp(0.0, 1.0);
    }
    let span = full / speed;
    if repeat {
        return offset - full * ((t / span).rem_euclid(1.0));
    }
    let pad = if span <= clip { (clip - span) * 0.5 } else { 0.0 };
    offset - speed * (t - pad).clamp(0.0, span)
}

fn rgba_to_hsv(c: [u8; 4]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d < 1e-6 {
        0.0
    } else if (max - r).abs() < 1e-6 {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if (max - g).abs() < 1e-6 {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max < 1e-6 { 0.0 } else { d / max };
    (h, s, max)
}

fn hsv_to_rgba(h: f32, s: f32, v: f32, a: u8) -> [u8; 4] {
    let c = v * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [((r + m) * 255.0).round() as u8, ((g + m) * 255.0).round() as u8, ((b + m) * 255.0).round() as u8, a]
}

/// Evaluate a layer's loops at `t` seconds into a clip of `window`
/// seconds. `base_color` is the layer's resting fill.
pub fn eval_loops(loops: &[TitleLoop], t: f32, window: Option<f32>, geo: LoopGeometry, base_color: [u8; 4]) -> LoopPose {
    let mut pose = LoopPose::default();
    for l in loops {
        match l {
            TitleLoop::Blink { rate_hz, duty } => {
                let phase = (t * rate_hz.max(0.01)).rem_euclid(1.0);
                if phase >= duty.clamp(0.02, 1.0) {
                    pose.opacity = 0.0;
                }
            }
            TitleLoop::Pulse { rate_hz, min_opacity } => {
                let s = 0.5 + 0.5 * (t * rate_hz * std::f32::consts::TAU).sin();
                let min = min_opacity.clamp(0.0, 1.0);
                pose.opacity *= min + (1.0 - min) * s;
            }
            TitleLoop::Float { amplitude, rate_hz } => {
                pose.dy += amplitude * geo.font_px * (t * rate_hz * std::f32::consts::TAU).sin();
            }
            TitleLoop::Sway { amplitude, rate_hz } => {
                pose.dx += amplitude * geo.font_px * (t * rate_hz * std::f32::consts::TAU).sin();
            }
            TitleLoop::Flicker { rate_hz, depth } => {
                // A step of the flicker's clock, hashed: about one step
                // in four dips, and a dip holds for its step.
                let step = (t.max(0.0) * rate_hz.max(0.1)).floor() as u32;
                let r = (step.wrapping_mul(2_654_435_761) >> 8) as f32 / (1u32 << 24) as f32;
                if r < 0.25 {
                    pose.opacity *= depth.clamp(0.0, 1.0);
                }
            }
            TitleLoop::Drift { dx, dy } => {
                let clip = window.unwrap_or(5.0).max(0.001);
                let k = (t / clip).clamp(0.0, 1.0) - 0.5;
                pose.dx += dx * geo.frame_w * k;
                pose.dy += dy * geo.frame_h * k;
            }
            TitleLoop::Shake { amplitude, rate_hz } => {
                let amp = amplitude * geo.font_px;
                let p = t * rate_hz * std::f32::consts::TAU;
                pose.dx += amp * (p.sin() * 0.8 + (p * 2.3).sin() * 0.2);
                pose.dy += amp * 0.6 * ((p * 1.7 + 1.0).sin() * 0.8 + (p * 3.1).cos() * 0.2);
            }
            TitleLoop::Wave { amplitude, rate_hz } => {
                pose.wave = Some((amplitude * geo.font_px, *rate_hz));
            }
            TitleLoop::ColorCycle { rate_hz } => {
                let (_, sat, val) = rgba_to_hsv(base_color);
                let hue = (t * rate_hz * 360.0).rem_euclid(360.0);
                pose.color = Some(hsv_to_rgba(hue, sat.max(0.6), val.max(0.8), base_color[3]));
            }
            TitleLoop::ColorBlend { color_a, color_b, rate_hz } => {
                let mix = 0.5 + 0.5 * (t * rate_hz * std::f32::consts::TAU).sin();
                let mut c = [0u8; 4];
                for i in 0..4 {
                    c[i] = (color_a[i] as f32 + (color_b[i] as f32 - color_a[i] as f32) * mix).round() as u8;
                }
                pose.color = Some(c);
            }
            // The travel runs from just past one edge to just past the
            // other, so the text is on its way in from the first frame.
            // A speed is pixels per second of a 1080p frame, as the
            // field says: scaled to this frame, a roll takes as long on
            // 4K or a vertical frame as on 1080p.
            TitleLoop::Scroll { direction, speed_px_per_sec, shape } => {
                pose.focus = pose.focus.or(shape.focus.map(|f| (RampAxis::Vertical, f)));
                let offset = travel_distance(geo.frame_h, geo.text_h) * 0.5;
                let y = travel(offset, *speed_px_per_sec * frame_scale(geo.frame_w, geo.frame_h), t, window, false);
                pose.dy += match direction { ScrollDirection::Up => y, ScrollDirection::Down => -y };
            }
            TitleLoop::Crawl { direction, speed_px_per_sec, repeat, shape } => {
                pose.focus = pose.focus.or(shape.focus.map(|f| (RampAxis::Horizontal, f)));
                let offset = travel_distance(geo.frame_w, geo.text_w) * 0.5;
                let x = travel(offset, *speed_px_per_sec * frame_scale(geo.frame_w, geo.frame_h), t, window, *repeat);
                pose.dx += match direction { CrawlDirection::Left => x, CrawlDirection::Right => -x };
            }
            TitleLoop::Counter { from, to, duration_secs, decimals } => {
                let p = if *duration_secs <= 0.0 { 1.0 } else { (t / duration_secs).clamp(0.0, 1.0) as f64 };
                let v = from + (to - from) * p;
                pose.text = Some(format_counter_value(v, *decimals));
            }
        }
    }
    pose
}

/// A counter's value as text, with at most three decimals.
pub fn format_counter_value(value: f64, decimals: u8) -> String {
    let d = decimals.min(3) as usize;
    format!("{value:.d$}", d = d)
}

#[cfg(test)]
mod loop_tests {

    /// #1920: an easing is read in the saved spelling and in the
    /// snake_case one the ops use, and saved as before.
    #[test]
    fn an_easing_reads_either_spelling_and_saves_as_before() {
        for (saved, snake, e) in [("\"Linear\"", "\"linear\"", Easing::Linear), ("\"OutCubic\"", "\"out_cubic\"", Easing::OutCubic), ("\"InOutCubic\"", "\"in_out_cubic\"", Easing::InOutCubic), ("\"OutBounce\"", "\"out_bounce\"", Easing::OutBounce)] {
            assert_eq!(serde_json::from_str::<Easing>(saved).unwrap(), e);
            assert_eq!(serde_json::from_str::<Easing>(snake).unwrap(), e);
            assert_eq!(serde_json::to_string(&e).unwrap(), saved, "saved as before");
        }
        // The guide's two-key reveal, as written there.
        let trim: crate::vector_path::PathTrim = serde_json::from_str(r#"{"keyframes":[{"time_secs":0,"range":{"start":0,"end":0,"offset":0},"easing":"linear"},{"time_secs":1,"range":{"start":0,"end":1,"offset":0},"easing":"linear"}]}"#).unwrap();
        assert_eq!(trim.keyframes.len(), 2);
        assert!(trim.keyframes.iter().all(|k| k.easing == Easing::Linear));
    }
    use super::*;

    fn geo() -> LoopGeometry {
        LoopGeometry { frame_w: 1920.0, frame_h: 1080.0, text_w: 400.0, text_h: 80.0, font_px: 72.0 }
    }

    /// #596: every motion rests at progress 1 (whole, in place, its own
    /// size, sharp) and is hidden or away at 0, the new ones included.
    #[test]
    fn every_motion_rests_at_one_and_is_away_at_zero() {
        for k in MotionKind::ALL {
            let m = Motion::new(k);
            let (a, dx, dy, s, b, _) = m.pose(1.0, 100.0);
            assert!((a - 1.0).abs() < 1e-6 && dx.abs() < 1e-3 && dy.abs() < 1e-3 && (s - 1.0).abs() < 1e-6 && b.abs() < 1e-6, "{k:?} at rest: {:?}", m.pose(1.0, 100.0));
            let (a0, dx0, dy0, s0, b0, clip) = m.pose(0.0, 100.0);
            let away = a0 < 0.01 || dx0.abs() > 1.0 || dy0.abs() > 1.0 || (s0 - 1.0).abs() > 0.1 || b0 > 1.0 || clip;
            assert!(away, "{k:?} at the start is not in its resting pose: {:?}", m.pose(0.0, 100.0));
        }
        // The fliers start far off: several fonts away.
        assert!(Motion::new(MotionKind::FlyLeft).pose(0.0, 100.0).1 < -300.0);
        assert_eq!(Motion::new(MotionKind::Pop).easing, Easing::OutBack, "a pop overshoots");
        assert_eq!(Motion::new(MotionKind::Bounce).easing, Easing::OutBounce);
        assert!(MotionKind::FocusIn.blurs() && MotionKind::SoftRise.blurs() && !MotionKind::Pop.blurs());
    }

    /// #736: the scaling kinds take an amount, and the plain ones scale
    /// without fading.
    #[test]
    fn scaling_motions_take_an_amount() {
        let (a, _, _, s, _, _) = Motion::new(MotionKind::Larger).amount(12.0).pose(0.0, 100.0);
        assert_eq!((a, s), (1.0, 12.0), "fully grown, fully opaque");
        let (a, _, _, s, _, _) = Motion::new(MotionKind::Larger).amount(12.0).pose(0.5, 100.0);
        assert_eq!((a, s), (1.0, 6.5));
        assert_eq!(Motion::new(MotionKind::Smaller).pose(0.0, 100.0).3, 0.0, "shrinks to nothing by default");
        assert_eq!(Motion::new(MotionKind::ScaleDown).amount(2.0).pose(0.0, 100.0).3, 2.0);
        assert_eq!(Motion::new(MotionKind::ScaleDown).pose(0.0, 100.0).3, 1.4, "the kind's own without one");
    }

    /// A flicker is the same on every run and every machine, and comes
    /// fully on.
    #[test]
    fn a_flicker_is_deterministic_and_ends_lit() {
        let a: Vec<f32> = (0..=20).map(|i| flicker(i as f32 / 20.0)).collect();
        let b: Vec<f32> = (0..=20).map(|i| flicker(i as f32 / 20.0)).collect();
        assert_eq!(a, b);
        assert_eq!(flicker(0.0), 0.0);
        assert_eq!(flicker(1.0), 1.0);
        assert!(a.contains(&1.0) && a[1..20].iter().any(|v| *v < 1.0), "it flickers on the way: {a:?}");
    }

    /// The picker offers thirty or more entrances and exits, each by a
    /// name of its own, in groups; and the loops by name too.
    #[test]
    fn the_picker_has_a_catalogue() {
        let presets = MotionPreset::all();
        assert!(presets.len() >= 30, "{} presets", presets.len());
        let mut names: Vec<&str> = presets.iter().map(|p| p.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), presets.len(), "no two presets share a name");
        assert!(presets.iter().all(|p| !p.group.is_empty()));
        let loops = TitleLoop::presets();
        let mut loop_names: Vec<&str> = loops.iter().map(|(n, _)| *n).collect();
        loop_names.sort_unstable();
        loop_names.dedup();
        assert_eq!(loop_names.len(), loops.len(), "no two loops share a name");
        assert!(loops.len() >= 15, "{} loops", loops.len());
    }

    /// The new loops: float bobs, sway sways, drift passes through its
    /// resting place halfway, a flicker dips now and then.
    #[test]
    fn the_new_loops_move_and_dip() {
        let at = |l: TitleLoop, t: f32| eval_loops(&[l], t, Some(4.0), geo(), [255; 4]);
        let float = TitleLoop::Float { amplitude: 0.1, rate_hz: 0.5 };
        assert!(at(float.clone(), 0.5).dy.abs() > 1.0 && at(float, 0.5).dx == 0.0);
        let sway = TitleLoop::Sway { amplitude: 0.1, rate_hz: 0.5 };
        assert!(at(sway.clone(), 0.5).dx.abs() > 1.0 && at(sway, 0.5).dy == 0.0);
        let drift = TitleLoop::Drift { dx: -0.1, dy: 0.0 };
        assert!((at(drift.clone(), 2.0).dx).abs() < 1e-3, "at rest halfway");
        assert!(at(drift.clone(), 0.0).dx > 0.0 && at(drift, 4.0).dx < 0.0, "from one side to the other");
        let flicker = TitleLoop::Flicker { rate_hz: 6.0, depth: 0.2 };
        let dips = (0..60).filter(|i| at(flicker.clone(), *i as f32 / 6.0).opacity < 1.0).count();
        assert!(dips > 0 && dips < 40, "it dips now and then: {dips} of 60");
    }

    /// #592: an edge fade ramps in over its zone at each edge and is
    /// whole between; a focus band is whole across the band and thins to
    /// its edge opacity at the frame's edges, on its axis only.
    #[test]
    fn a_screen_fade_fades_at_the_edges_and_away_from_the_band() {
        let fade = ScreenFade::new(EdgeFade { top: 0.2, bottom: 0.2, ..EdgeFade::NONE }, None).unwrap();
        assert_eq!(fade.alpha_at(0.5, 0.0), 0.0);
        assert_eq!(fade.alpha_at(0.5, 1.0), 0.0);
        assert!((fade.alpha_at(0.5, 0.1) - 0.5).abs() < 1e-6, "half way up its zone: {}", fade.alpha_at(0.5, 0.1));
        for u in [0.2, 0.5, 0.8] {
            assert_eq!(fade.alpha_at(0.0, u), 1.0, "whole between the zones at {u}, and at the left edge with no left zone");
        }
        let left = ScreenFade::new(EdgeFade { left: 0.3, ..EdgeFade::NONE }, None).unwrap();
        assert_eq!(left.alpha_at(0.0, 0.5), 0.0);
        assert_eq!(left.alpha_at(1.0, 0.5), 1.0, "only the edge it names");
        let band = ScreenFade::new(EdgeFade::NONE, Some((RampAxis::Horizontal, FocusBand { band: 0.5, edge_opacity: 0.2, blur: 0.0 }))).unwrap();
        for u in [0.26, 0.5, 0.74] {
            assert_eq!(band.alpha_at(u, 0.0), 1.0, "whole inside the band at {u}, whatever the row");
        }
        assert!((band.alpha_at(0.0, 0.5) - 0.2).abs() < 1e-6 && (band.alpha_at(1.0, 0.5) - 0.2).abs() < 1e-6);
        assert!(band.alpha_at(0.1, 0.5) < band.alpha_at(0.2, 0.5) && band.alpha_at(0.2, 0.5) < 1.0, "thinning towards the edge");
        assert!(ScreenFade::new(EdgeFade::default(), None).is_none(), "nothing fades nothing");
    }

    /// A roll saved before its shape existed loads with none.
    #[test]
    fn a_roll_saved_without_a_shape_loads_unshaped() {
        let old: TitleLoop = serde_json::from_str(r#"{"Scroll":{"direction":"Up","speed_px_per_sec":40.0}}"#).unwrap();
        assert_eq!(old, TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 40.0, shape: RollShape::default() });
        let p = eval_loops(std::slice::from_ref(&old), 1.0, Some(4.0), geo(), [255; 4]);
        assert!(p.focus.is_none());
    }

    /// #588: a speed is pixels per second of a 1080p frame, so a roll
    /// is as far along on 4K as on 1080p at any moment.
    #[test]
    fn a_speed_is_1080p_pixels_so_a_roll_keeps_its_pace_on_4k() {
        let loops = [TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 200.0, shape: Default::default() }];
        let along = |w: f32, h: f32, t: f32| {
            let s = frame_scale(w, h);
            let geo = LoopGeometry { frame_w: w, frame_h: h, text_w: 400.0 * s, text_h: 300.0 * s, font_px: 72.0 * s };
            eval_loops(&loops, t, Some(60.0), geo, [255; 4]).dy / h
        };
        for t in [0.0, 20.0, 27.5, 33.0] {
            assert!((along(1920.0, 1080.0, t) - along(3840.0, 2160.0, t)).abs() < 1e-4, "at {t} s: {} vs {}", along(1920.0, 1080.0, t), along(3840.0, 2160.0, t));
        }
    }

    #[test]
    fn a_scroll_with_no_speed_fits_the_clip() {
        let loops = [TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0, shape: Default::default() }];
        let start = eval_loops(&loops, 0.0, Some(4.0), geo(), [255; 4]);
        let end = eval_loops(&loops, 4.0, Some(4.0), geo(), [255; 4]);
        // At the start the text's top edge sits just below the frame;
        // at the end its bottom edge sits just above it.
        let g = geo();
        let just_past = (g.frame_h + g.text_h) * 0.5;
        assert!(start.dy > just_past && start.dy < just_past + g.frame_h * 0.05, "{}", start.dy);
        assert!((start.dy + end.dy).abs() < 0.01, "{} {}", start.dy, end.dy);
    }

    #[test]
    fn a_blink_is_off_for_the_rest_of_its_period() {
        let loops = [TitleLoop::Blink { rate_hz: 1.0, duty: 0.5 }];
        assert_eq!(eval_loops(&loops, 0.25, None, geo(), [255; 4]).opacity, 1.0);
        assert_eq!(eval_loops(&loops, 0.75, None, geo(), [255; 4]).opacity, 0.0);
    }

    #[test]
    fn loops_stack() {
        let loops = [
            TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.0 },
            TitleLoop::Crawl { direction: CrawlDirection::Left, speed_px_per_sec: 100.0, repeat: false, shape: Default::default() },
            TitleLoop::Counter { from: 0.0, to: 10.0, duration_secs: 2.0, decimals: 1 },
        ];
        let p = eval_loops(&loops, 1.0, Some(10.0), geo(), [255; 4]);
        assert!((p.opacity - 0.5).abs() < 0.01, "pulse at a full period sits mid-swell: {}", p.opacity);
        assert!(p.dx.abs() > 0.0);
        assert_eq!(p.text.as_deref(), Some("5.0"));
    }

    #[test]
    fn the_hue_cycle_keeps_the_alpha_and_moves_the_colour() {
        let loops = [TitleLoop::ColorCycle { rate_hz: 1.0 }];
        let a = eval_loops(&loops, 0.0, None, geo(), [255, 0, 0, 200]).color.unwrap();
        let b = eval_loops(&loops, 0.33, None, geo(), [255, 0, 0, 200]).color.unwrap();
        assert_eq!(a[3], 200);
        assert_ne!(a, b);
    }
}

#[cfg(test)]
mod exit_easing_tests {
    use super::*;

    /// An exit's progress `at` (0..1) of the way through a 5 s exit that
    /// ends a 10 s clip: 1 = resting, 0 = gone.
    fn exit_at(m: Motion, at: f32) -> f32 {
        TitleMotion { enter: None, exit: Some(m.duration(5.0)) }.progress(5.0 + 5.0 * at, Some(10.0), 0).1
    }

    /// #1566: an exit's easing runs over the exit's own time. `in_cubic`
    /// leaves slowly first, `out_cubic` fast first, as each arrives.
    #[test]
    fn an_exit_eases_over_its_own_time() {
        for at in [0.0, 0.01, 0.06, 0.25, 0.5, 0.75, 0.92, 1.0] {
            let slow = exit_at(Motion::new(MotionKind::Larger).easing(Easing::InCubic), at);
            assert!((slow - (1.0 - at * at * at)).abs() < 1e-5, "in_cubic {at} of the way: {slow}");
            let fast = exit_at(Motion::new(MotionKind::Larger).easing(Easing::OutCubic), at);
            assert!((fast - (1.0 - at).powi(3)).abs() < 1e-5, "out_cubic {at} of the way: {fast}");
        }
        let grow = |e| exit_at(Motion::new(MotionKind::Larger).easing(e), 0.1);
        assert!(grow(Easing::InCubic) > 0.99, "in_cubic has barely left a tenth of the way: {}", grow(Easing::InCubic));
        assert!(grow(Easing::OutCubic) < 0.75, "out_cubic is well on its way a tenth of the way: {}", grow(Easing::OutCubic));
    }

    /// A new exit takes its kind's exit easing, which draws every exit
    /// whose entrance default was `out_cubic` or linear just as it drew
    /// before #1566; an easing chosen is kept.
    #[test]
    fn a_default_exit_renders_as_before() {
        for kind in MotionKind::ALL {
            let m = Motion::exit(kind);
            assert_eq!(m.easing, kind.default_exit_easing(), "{kind:?}");
            if !matches!(kind.default_easing(), Easing::OutCubic | Easing::Linear) {
                continue;
            }
            for i in 0..=20 {
                let at = i as f32 / 20.0;
                // Before: the entrance default over the exit's remaining time.
                let before = kind.default_easing().apply(1.0 - at);
                let now = exit_at(m, at);
                assert!((now - before).abs() < 1e-5, "{kind:?} {at} of the way: {now}, before {before}");
                let (b, n) = (m.pose(before, 72.0), m.pose(now, 72.0));
                assert!((b.0 - n.0).abs() < 1e-4 && (b.3 - n.3).abs() < 1e-4, "{kind:?} {at} of the way: alpha and scale {n:?}, before {b:?}");
            }
        }
        assert_eq!(Motion::exit(MotionKind::Pop).easing, Easing::InCubic, "a pop's exit doesn't overshoot past gone");
        assert_eq!(Motion::new(MotionKind::Rise).easing(Easing::OutBack).as_exit().easing, Easing::OutBack, "a chosen easing is kept");
    }
}

#[cfg(test)]
mod custom_easing_tests {
    use super::*;

    /// A curve of its own draws as CSS draws the same handles: straight
    /// handles are linear, `ease` is CSS's, and the ends are rest and
    /// arrival (#1511).
    #[test]
    fn a_custom_curve_draws_as_css_draws_it() {
        let linear = CubicBezier::new(0.0, 0.0, 1.0, 1.0);
        let ease = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            assert!((linear.apply(t) - t).abs() < 1e-5, "linear at {t}");
        }
        // CSS `ease` at the half: 0.8024033877399112.
        assert!((ease.apply(0.5) - 0.802_403_4).abs() < 1e-4, "{}", ease.apply(0.5));
        for c in [ease, CubicBezier::new(0.0, 1.0, 1.0, 0.0), CubicBezier::new(1.0, 0.0, 0.0, 1.0)] {
            assert_eq!((c.apply(0.0), c.apply(1.0)), (0.0, 1.0), "{c:?}");
        }
        // Steep handles still give one progress per time, rising.
        let steep = CubicBezier::new(1.0, 0.0, 0.0, 1.0);
        let ps: Vec<f32> = (0..=40).map(|i| steep.apply(i as f32 / 40.0)).collect();
        assert!(ps.windows(2).all(|p| p[1] >= p[0] - 1e-5), "{ps:?}");
        // A handle above arrival overshoots it.
        let over = Easing::Custom(CubicBezier::new(0.3, 1.6, 0.6, 1.0));
        assert!((0..=20).any(|i| over.apply(i as f32 / 20.0) > 1.05));
        // Handles' times are kept within the motion.
        let c = CubicBezier::new(-1.0, 0.5, 2.0, 0.5);
        assert_eq!((c.x1, c.x2), (0.0, 1.0));
    }

    /// A named easing saves as its name, as before; a curve of its own
    /// saves as its handles and reads back the same.
    #[test]
    fn named_easings_save_as_before_and_a_curve_as_its_handles() {
        assert_eq!(serde_json::to_string(&Easing::OutCubic).unwrap(), "\"OutCubic\"");
        assert_eq!(serde_json::from_str::<Easing>("\"InOutCubic\"").unwrap(), Easing::InOutCubic);
        let custom = Easing::Custom(CubicBezier::new(0.2, 1.3, 0.5, 1.0));
        let json = serde_json::to_string(&custom).unwrap();
        assert_eq!(json, r#"{"Custom":{"x1":0.2,"y1":1.3,"x2":0.5,"y2":1.0}}"#);
        assert_eq!(serde_json::from_str::<Easing>(&json).unwrap(), custom);
    }
}

#[cfg(test)]
mod animator_tests {
    use super::*;
    use crate::keyframe::{Interpolation, Keyframe};

    fn key(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    fn selector(start: f32, end: f32, shape: SelectShape, order: SelectOrder) -> RangeSelector {
        RangeSelector { start, end, shape, order, ..Default::default() }
    }

    /// The orders lay the units along the range as named; a random order
    /// is a shuffle fixed by its seed alone (#1510).
    #[test]
    fn orders_lay_the_units_along_the_range() {
        assert_eq!(select_ranks(SelectOrder::Forward, 4), [0, 1, 2, 3]);
        assert_eq!(select_ranks(SelectOrder::Reverse, 4), [3, 2, 1, 0]);
        // The middle first, then outwards, the left before the right.
        assert_eq!(select_ranks(SelectOrder::FromCentre, 5), [3, 1, 0, 2, 4]);
        let a = select_ranks(SelectOrder::Random { seed: 7 }, 12);
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(sorted, (0..12).collect::<Vec<_>>(), "a shuffle of every unit");
        assert_eq!(a, select_ranks(SelectOrder::Random { seed: 7 }, 12), "the same every time");
        assert_ne!(a, select_ranks(SelectOrder::Random { seed: 8 }, 12), "another seed, another order");
        assert_ne!(a, (0..12).collect::<Vec<_>>());
        // Pinned: the shuffle is integer arithmetic on the seed, so it is
        // the same on every machine; a change here changes saved titles.
        assert_eq!(a, [0, 5, 4, 10, 9, 11, 7, 8, 3, 2, 6, 1]);
    }

    /// A square range picks the units inside it whole and a unit at its
    /// edge by how much of it is in; the shapes rise and fall across the
    /// range; a negative amount picks the units outside.
    #[test]
    fn a_selector_weighs_each_unit() {
        let w = |s: &RangeSelector| s.weights(5, 0.0);
        assert_eq!(w(&selector(0.4, 0.6, SelectShape::Square, SelectOrder::Forward)), [0.0, 0.0, 1.0, 0.0, 0.0]);
        let edge = w(&selector(0.3, 0.5, SelectShape::Square, SelectOrder::Forward));
        assert!((edge[1] - 0.5).abs() < 1e-5 && (edge[2] - 0.5).abs() < 1e-5 && edge[0] == 0.0, "{edge:?}");
        let up = w(&selector(0.0, 1.0, SelectShape::RampUp, SelectOrder::Forward));
        assert!(up.windows(2).all(|p| p[1] > p[0]) && (up[2] - 0.5).abs() < 1e-5, "{up:?}");
        let down = w(&selector(0.0, 1.0, SelectShape::RampDown, SelectOrder::Forward));
        assert!(down.windows(2).all(|p| p[1] < p[0]), "{down:?}");
        let tri = w(&selector(0.0, 1.0, SelectShape::Triangle, SelectOrder::Forward));
        assert!((tri[2] - 1.0).abs() < 1e-5 && tri[0] < tri[1] && (tri[0] - tri[4]).abs() < 1e-5, "{tri:?}");
        for shape in [SelectShape::Round, SelectShape::Smooth] {
            let v = w(&selector(0.0, 1.0, shape, SelectOrder::Forward));
            assert!((v[2] - 1.0).abs() < 1e-5 && v[0] < v[1] && (v[0] - v[4]).abs() < 1e-5, "{shape:?} {v:?}");
        }
        // The order moves the shape: reversed, the ramp falls.
        let rev = w(&selector(0.0, 1.0, SelectShape::RampUp, SelectOrder::Reverse));
        assert!(rev.windows(2).all(|p| p[1] < p[0]), "{rev:?}");
        let mut outside = selector(0.4, 0.6, SelectShape::Square, SelectOrder::Forward);
        outside.amount = -1.0;
        assert_eq!(w(&outside), [1.0, 1.0, 0.0, 1.0, 1.0]);
        assert!(selector(0.0, 1.0, SelectShape::Square, SelectOrder::Forward).weights(0, 0.0).is_empty());
    }

    /// The range's start, end and offset are keyed: a start swept from 0
    /// to 1 lets the units go one after another, in the selector's order.
    #[test]
    fn a_keyed_range_sweeps_the_units_in_order() {
        let mut s = selector(0.0, 1.0, SelectShape::Square, SelectOrder::Random { seed: 3 });
        s.start_keyframes = vec![key(0.0, 0.0), key(1.0, 1.0)];
        let ranks = select_ranks(s.order, 6);
        let mut gone_at = vec![f64::MAX; 6];
        for i in 0..=60 {
            let t = i as f64 / 60.0;
            for (u, w) in s.weights(6, t).into_iter().enumerate() {
                if w == 0.0 && gone_at[u] == f64::MAX {
                    gone_at[u] = t;
                }
            }
        }
        let mut by_rank: Vec<usize> = (0..6).collect();
        by_rank.sort_by_key(|&u| ranks[u]);
        assert!(by_rank.windows(2).all(|p| gone_at[p[0]] < gone_at[p[1]]), "{gone_at:?} {ranks:?}");
        s.offset_keyframes = vec![key(0.0, 0.5)];
        assert_eq!(s.range_at(0.0), (0.5, 1.5));
    }

    /// The animators' pose per glyph: only the picked word's letters
    /// change, spaces never, and each property lays on as it should.
    #[test]
    fn animators_pose_only_the_glyphs_they_pick() {
        let parents: Vec<char> = "one MIDDLE two".chars().collect();
        let turn = Animator {
            selector: RangeSelector { unit: SelectUnit::Word, start: 1.0 / 3.0, end: 2.0 / 3.0, ..Default::default() },
            props: AnimatorProps { rotation: 90.0, opacity: 0.5, dy: 1.0, scale: 2.0, colour: Some([0, 0, 255, 255]), ..Default::default() },
            ..Default::default()
        };
        let poses = animator_poses(std::slice::from_ref(&turn), &parents, 40.0, 0.0);
        for (c, p) in parents.iter().zip(&poses) {
            let in_middle = c.is_uppercase();
            assert_eq!(!p.is_rest(), in_middle, "{c:?} {p:?}");
            if in_middle {
                assert_eq!((p.rotation, p.alpha, p.dy, p.scale), (90.0, 0.5, 40.0, 2.0));
                assert_eq!(p.colour_of([255, 0, 0, 255]), [0, 0, 255, 255]);
            }
        }
        // Half picked: half the change, the colour half the light.
        let mut half = turn.clone();
        half.selector.amount = 0.5;
        let p = animator_poses(&[half], &parents, 40.0, 0.0)[4];
        assert_eq!((p.rotation, p.scale), (45.0, 1.5));
        assert_eq!(p.colour_of([255, 0, 0, 255]), [188, 0, 188, 255]);
        // Off, or none: every glyph at rest.
        let off = Animator { enabled: false, ..turn };
        assert!(animator_poses(&[off], &parents, 40.0, 0.0).iter().all(UnitPose::is_rest));
        assert!(animator_poses(&[], &parents, 40.0, 0.0).iter().all(UnitPose::is_rest));
    }

    /// A limiting selector keeps an animator inside its range: a random
    /// order over every letter plays out in the middle word alone.
    #[test]
    fn within_limits_the_selection() {
        let parents: Vec<char> = "one MIDDLE two".chars().collect();
        let a = Animator {
            selector: RangeSelector { order: SelectOrder::Random { seed: 7 }, ..Default::default() },
            within: Some(RangeSelector { start: 3.0 / 12.0, end: 9.0 / 12.0, ..Default::default() }),
            props: AnimatorProps { rotation: 90.0, ..Default::default() },
            ..Default::default()
        };
        let poses = animator_poses(std::slice::from_ref(&a), &parents, 40.0, 0.0);
        for (c, p) in parents.iter().zip(&poses) {
            let want = if c.is_uppercase() { 90.0 } else { 0.0 };
            assert!((p.rotation - want).abs() < 1e-3, "{c:?}: {}", p.rotation);
        }
        let json = serde_json::to_string(&Animator::default()).unwrap();
        assert!(!json.contains("within"), "{json}");
        assert_eq!(serde_json::from_str::<Animator>(&serde_json::to_string(&a).unwrap()).unwrap(), a);
    }

    /// An animator saves its own fields only, and reads back the same.
    #[test]
    fn an_animator_saves_and_reads_back() {
        let a = Animator {
            name: "Spin".into(),
            selector: RangeSelector { unit: SelectUnit::Word, order: SelectOrder::Random { seed: 9 }, start_keyframes: vec![key(0.0, 0.0)], easing: Easing::Custom(CubicBezier::new(0.2, 0.0, 0.4, 1.0)), ..Default::default() },
            props: AnimatorProps { rotation: 360.0, ..Default::default() },
            ..Default::default()
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("enabled") && !json.contains("end_keyframes") && !json.contains("colour"), "{json}");
        assert_eq!(serde_json::from_str::<Animator>(&json).unwrap(), a);
        assert_eq!(serde_json::from_str::<Animator>("{}").unwrap(), Animator::default());
    }

    /// The presets as they were written in code before the file: the
    /// file's read to the same animators on any clip length.
    fn as_written(name: &str, clip_secs: f64) -> Option<Animator> {
        let clip = clip_secs.max(0.1);
        let entrance = clip.min(1.2);
        Some(match name {
            "wave" => Animator {
                name: "Wave".into(),
                selector: RangeSelector { end: 0.3, shape: SelectShape::Smooth, offset_keyframes: vec![key(0.0, -0.3), key(clip, 1.0)], ..Default::default() },
                props: AnimatorProps { dy: -0.3, ..Default::default() },
                ..Default::default()
            },
            "random_fade_in" => Animator {
                name: "Random fade in".into(),
                selector: RangeSelector { order: SelectOrder::Random { seed: 1 }, start_keyframes: vec![key(0.0, 0.0), key(entrance, 1.0)], ..Default::default() },
                props: AnimatorProps { opacity: 0.0, ..Default::default() },
                ..Default::default()
            },
            "spin_in" => Animator {
                name: "Spin in".into(),
                selector: RangeSelector { start_keyframes: vec![key(0.0, 0.0), key(entrance, 1.0)], easing: Easing::OutCubic, ..Default::default() },
                props: AnimatorProps { rotation: 360.0, scale: 0.4, opacity: 0.0, ..Default::default() },
                ..Default::default()
            },
            _ => return None,
        })
    }

    /// The presets file reads to the three presets as they were, keys
    /// placed on a long clip, a short one and one of no length (#2887).
    #[test]
    fn the_animator_presets_read_as_they_were() {
        assert_eq!(animator_preset_ids(), ["wave", "random_fade_in", "spin_in"]);
        for clip in [4.0, 0.5, 0.0, 30.0] {
            for id in animator_preset_ids() {
                assert_eq!(animator_preset(id, clip), as_written(id, clip), "{id} on a {clip} s clip");
            }
        }
        assert_eq!(animator_preset("bounce", 4.0), None);
    }

    /// A presets file with one that can't be offered is refused, saying
    /// why: an id twice or not a word, no name, a field it doesn't know,
    /// a key placed nowhere it knows, a number that isn't finite.
    #[test]
    fn a_bad_animator_presets_file_is_refused() {
        let file = |body: &str| format!("kind = \"animator_preset\"\nformat_version = 1\n{body}");
        let one = "[[preset]]\nid = \"lift\"\nname = \"Lift\"\nprops = { dy = -0.5 }\nkeys = [{ on = \"end\", at = \"entrance\", value = 1.0 }]\n";
        let read = read_animator_presets(&file(one)).unwrap();
        assert_eq!((read.len(), read[0].props.dy, read[0].keys.len()), (1, -0.5, 1));
        for (body, says) in [
            (format!("{one}{one}"), "given once"),
            (one.replace("\"lift\"", "\"Lift Up\""), "given once"),
            (one.replace("\"Lift\"", "\" \""), "needs a name"),
            (one.replace("dy = -0.5", "dy = nan"), "isn't finite"),
            (format!("{one}keys_extra = 1\n"), "unknown field"),
            (one.replace("dy = -0.5", "dy = -0.5, glow = 1"), "unknown field"),
            (one.replace("at = \"entrance\"", "at = \"middle\""), "unknown variant"),
            (one.replace("on = \"end\"", "on = \"amount\""), "unknown variant"),
        ] {
            let why = read_animator_presets(&file(&body)).unwrap_err();
            assert!(why.contains(says), "{body}: {why}");
        }
        let many = format!("[[preset]]\nid = \"many\"\nname = \"Many\"\n{}", "[[preset.keys]]\non = \"start\"\nat = \"start\"\nvalue = 0.0\n".repeat(65));
        assert!(read_animator_presets(&file(&many)).unwrap_err().contains("at most 64 keys"));
        assert!(read_animator_presets(&file(one).replace("animator_preset", "effect_preset")).is_err());
    }
}

#[cfg(test)]
mod user_animator_preset_tests {
    use super::*;

    /// A file dropped in a person's animators folder lists its preset after
    /// Klipp's (#3095); a hostile one is refused, said, and Klipp's stay
    /// whole: a link, one past the size, an unknown field, the wrong kind,
    /// a built-in's id or name, an id that isn't a word, a value past what
    /// the animator op takes.
    #[test]
    fn a_person_s_animator_preset_lists_after_klipp_s_and_a_hostile_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("klipp-user-animators-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |n: &str, body: &str| std::fs::write(dir.join(n), format!("kind = \"animator_preset\"\nformat_version = 1\n{body}")).unwrap();
        let preset = |id: &str, name: &str, extra: &str| format!("[[preset]]\nid = \"{id}\"\nname = \"{name}\"\n{extra}");
        file("a.toml", &preset("drift_up", "Drift up", "props = { dy = -0.5 }\n[[preset.keys]]\non = \"start\"\nat = \"start\"\nvalue = 0.0\n"));
        file("b.toml", &(preset("odd", "Odd", "") + "price = 2\n"));
        std::fs::write(dir.join("c.toml"), "kind = \"generator_entry\"\nformat_version = 1\n").unwrap();
        file("d.toml", &format!("# {}\n", "x".repeat(crate::preset_file::MAX_FILE_BYTES)));
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("a.toml"), dir.join("e.toml")).unwrap();
        let klipp_s = animator_preset_listing_in(&[]).entries;
        let (first_id, first_name) = (klipp_s[0].id.clone(), klipp_s[0].name.to_uppercase());
        file("f.toml", &[
            preset(&first_id, "Mine", ""),
            preset("same_name", &first_name, ""),
            preset("Bad Id", "Bad id", ""),
            preset("huge", "Huge", "props = { scale = 1000000.0 }\n"),
            preset("far", "Far", "[[preset.keys]]\non = \"end\"\nat = \"end\"\nvalue = 9.0\n"),
        ].concat());
        let listing = animator_preset_listing_in(std::slice::from_ref(&dir));
        let ids: Vec<&str> = listing.entries.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.len(), klipp_s.len() + 1, "{ids:?}");
        assert_eq!(ids[..klipp_s.len()], klipp_s.iter().map(|p| p.id.as_str()).collect::<Vec<_>>()[..], "Klipp's, whole and first");
        assert_eq!(ids[klipp_s.len()], "drift_up");
        let said = |what: &str| assert!(listing.skipped.iter().any(|s| s.contains(what)), "{what}: {:?}", listing.skipped);
        said("b.toml: animator_preset entry 1: unknown field");
        said("c.toml: a generator_entry file, not a animator_preset file");
        said("d.toml: ");
        #[cfg(unix)]
        said("e.toml: not a plain file");
        said(&format!("\"{first_id}\" is the name of one of Klipp's own"));
        said(&format!("\"same_name\": its name \"{first_name}\" is that of one before it"));
        said("\"Bad Id\": an id of lowercase letters");
        said("\"huge\": scale 1000000 is outside 0 to 20");
        said("\"far\": an end key 9 is outside 0 to 1");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
