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
    /// Lands and bounces a few times before settling.
    OutBounce,
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
    Scroll { direction: ScrollDirection, speed_px_per_sec: f32 },
    /// Ticker: the layer travels across the frame. `repeat` starts it
    /// again as soon as it has left.
    Crawl { direction: CrawlDirection, speed_px_per_sec: f32, repeat: bool },
    /// The text becomes a number counting from `from` to `to`.
    Counter { from: f64, to: f64, duration_secs: f32, decimals: u8 },
}

impl TitleLoop {
    /// One of each, with sensible values, for pickers.
    pub fn catalogue() -> Vec<TitleLoop> {
        vec![
            TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0 },
            TitleLoop::Crawl { direction: CrawlDirection::Left, speed_px_per_sec: 0.0, repeat: false },
            TitleLoop::Blink { rate_hz: 2.0, duty: 0.5 },
            TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.4 },
            TitleLoop::Shake { amplitude: 0.05, rate_hz: 12.0 },
            TitleLoop::Wave { amplitude: 0.12, rate_hz: 1.0 },
            TitleLoop::ColorCycle { rate_hz: 0.25 },
            TitleLoop::ColorBlend { color_a: [255, 255, 255, 255], color_b: [255, 200, 0, 255], rate_hz: 0.5 },
            TitleLoop::Counter { from: 0.0, to: 100.0, duration_secs: 3.0, decimals: 0 },
        ]
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
}

impl Default for LoopPose {
    fn default() -> Self {
        Self { opacity: 1.0, dx: 0.0, dy: 0.0, wave: None, color: None, text: None }
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
            TitleLoop::Scroll { direction, speed_px_per_sec } => {
                let offset = ((geo.frame_h + geo.text_h) * 0.5 * 1.1).max(geo.frame_h);
                let y = travel(offset, *speed_px_per_sec, t, window, false);
                pose.dy += match direction { ScrollDirection::Up => y, ScrollDirection::Down => -y };
            }
            TitleLoop::Crawl { direction, speed_px_per_sec, repeat } => {
                let offset = ((geo.frame_w + geo.text_w) * 0.5 * 1.1).max(geo.frame_w);
                let x = travel(offset, *speed_px_per_sec, t, window, *repeat);
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
    use super::*;

    fn geo() -> LoopGeometry {
        LoopGeometry { frame_w: 1920.0, frame_h: 1080.0, text_w: 400.0, text_h: 80.0, font_px: 72.0 }
    }

    #[test]
    fn a_scroll_with_no_speed_fits_the_clip() {
        let loops = [TitleLoop::Scroll { direction: ScrollDirection::Up, speed_px_per_sec: 0.0 }];
        let start = eval_loops(&loops, 0.0, Some(4.0), geo(), [255; 4]);
        let end = eval_loops(&loops, 4.0, Some(4.0), geo(), [255; 4]);
        assert!(start.dy > 1000.0 && end.dy < -1000.0, "{} {}", start.dy, end.dy);
        assert!((start.dy + end.dy).abs() < 0.01);
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
            TitleLoop::Crawl { direction: CrawlDirection::Left, speed_px_per_sec: 100.0, repeat: false },
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
