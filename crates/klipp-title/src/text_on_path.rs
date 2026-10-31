//! Text along a path (#1508): where each glyph of a text set on a path
//! sits and how it turns. The glyphs are laid as one line by the
//! shaper, so kerning, ligatures, tracking and bidi order all hold; a
//! glyph's place along the path is its distance along that line, and it
//! stands on the path by its baseline, turned to the path's tangent.
//!
//! A drawn path is a contour of a path layer, flattened in frame pixels
//! exactly as the layer draws it at that moment; the fixed shapes (arc,
//! circle, wave) are evaluated the way they always were.

use klipp_timeline::{PathTextAlign, TextPath};

/// A polyline measured along its length: the drawn path a text follows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PathLine {
    points: Vec<[f32; 2]>,
    /// Distance from the first point to each point.
    along: Vec<f32>,
    closed: bool,
}

impl PathLine {
    /// The line through `points`, back to the first when `closed`;
    /// `None` for one with no length.
    pub(crate) fn new(mut points: Vec<[f32; 2]>, closed: bool) -> Option<Self> {
        if closed {
            if let Some(&first) = points.first() {
                points.push(first);
            }
        }
        let mut along = Vec::with_capacity(points.len());
        let mut total = 0.0f32;
        for (i, p) in points.iter().enumerate() {
            if i > 0 {
                let q = points[i - 1];
                total += ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
            }
            along.push(total);
        }
        (total > 0.0).then_some(Self { points, along, closed })
    }

    /// The same line run the other way.
    pub(crate) fn reversed(&self) -> Self {
        let total = self.length();
        Self {
            points: self.points.iter().rev().copied().collect(),
            along: self.along.iter().rev().map(|d| total - d).collect(),
            closed: self.closed,
        }
    }

    pub(crate) fn length(&self) -> f32 {
        self.along.last().copied().unwrap_or(0.0)
    }

    /// The point `s` along the line and the tangent's angle there, in
    /// radians; a closed line wraps `s`, an open one has nothing past
    /// its ends.
    pub(crate) fn at(&self, s: f32) -> Option<([f32; 2], f32)> {
        let total = self.length();
        let s = if self.closed {
            s.rem_euclid(total)
        } else if (0.0..=total).contains(&s) {
            s
        } else {
            return None;
        };
        let i = self.along.partition_point(|&d| d <= s).clamp(1, self.points.len() - 1);
        let (a, b) = (self.points[i - 1], self.points[i]);
        let span = (self.along[i] - self.along[i - 1]).max(f32::EPSILON);
        let t = ((s - self.along[i - 1]) / span).clamp(0.0, 1.0);
        Some(([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], (b[1] - a[1]).atan2(b[0] - a[0])))
    }
}

/// One glyph to place: its centre's distance along the laid line, and
/// its centre's height above (negative) or below the line's baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Laid {
    pub(crate) along: f32,
    pub(crate) below_baseline: f32,
}

/// A text on a path as its resting render laid it (#1697): each glyph's
/// place along the text, the text's length, and where each glyph stood
/// on the path at rest. A moving path poses each resting glyph from
/// there to where it stands now.
#[derive(Debug, Clone, PartialEq)]
pub struct PathUnits {
    pub(crate) laid: Vec<Laid>,
    pub(crate) length: f32,
    pub(crate) resting: Vec<Option<Placed>>,
}

/// Where a glyph goes: its centre in frame pixels and its turn, radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placed {
    pub(crate) centre: [f32; 2],
    pub(crate) angle: f32,
}

/// Each glyph of a text `length` long placed along `path`: a drawn
/// path's `line` (with its offset, align and flip), or a fixed shape
/// about `centre`. `shift` moves every glyph down from the path, in
/// pixels. `None` for a glyph off an open path's ends.
pub(crate) fn place(path: &TextPath, line: Option<&PathLine>, offset: f32, glyphs: &[Laid], length: f32, centre: [f32; 2], shift: f32) -> Vec<Option<Placed>> {
    let stand = |p: [f32; 2], angle: f32, below: f32| {
        // Down from the path is the tangent turned a quarter clockwise
        // on a frame whose y runs down.
        let (sin, cos) = angle.sin_cos();
        let d = below + shift;
        Placed { centre: [p[0] - sin * d, p[1] + cos * d], angle }
    };
    let length = length.max(1.0);
    match path {
        TextPath::Layer { align, flip, .. } => {
            let Some(line) = line else { return vec![None; glyphs.len()] };
            let line = if *flip { line.reversed() } else { line.clone() };
            let start = offset * line.length()
                - match align {
                    PathTextAlign::Start => 0.0,
                    PathTextAlign::Center => length / 2.0,
                    PathTextAlign::End => length,
                };
            glyphs.iter().map(|g| line.at(start + g.along).map(|(p, a)| stand(p, a, g.below_baseline))).collect()
        }
        TextPath::Arc { radius, start_angle, sweep } => glyphs
            .iter()
            .map(|g| {
                let a = start_angle.to_radians() + sweep.to_radians() * g.along / length;
                Some(stand([centre[0] + radius * a.cos(), centre[1] + radius * a.sin()], a + std::f32::consts::FRAC_PI_2, g.below_baseline))
            })
            .collect(),
        TextPath::Circle { radius } => glyphs
            .iter()
            .map(|g| {
                let a = std::f32::consts::TAU * g.along / length;
                Some(stand([centre[0] + radius * a.cos(), centre[1] + radius * a.sin()], a + std::f32::consts::FRAC_PI_2, g.below_baseline))
            })
            .collect(),
        TextPath::Wave { amplitude, frequency, phase } => glyphs
            .iter()
            .map(|g| {
                let t = g.along / length;
                let arg = frequency * std::f32::consts::TAU * t + phase.to_radians();
                let x = centre[0] - length * 0.5 + length * t;
                let y = centre[1] + amplitude * arg.sin();
                let slope = amplitude * frequency * std::f32::consts::TAU * arg.cos() / length;
                Some(stand([x, y], slope.atan(), g.below_baseline))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn a_line_is_measured_along_its_length_and_wraps_when_closed() {
        let l = PathLine::new(vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]], false).unwrap();
        assert_eq!(l.length(), 20.0);
        let (p, a) = l.at(15.0).unwrap();
        assert!(close(p, [10.0, 5.0]) && (a - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!(l.at(-1.0).is_none() && l.at(21.0).is_none(), "nothing past an open line's ends");
        let r = l.reversed();
        assert!(close(r.at(0.0).unwrap().0, [10.0, 10.0]) && close(r.at(15.0).unwrap().0, [5.0, 0.0]));
        let square = PathLine::new(vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]], true).unwrap();
        assert_eq!(square.length(), 40.0);
        assert!(close(square.at(45.0).unwrap().0, [5.0, 0.0]), "a closed line wraps");
        assert!(PathLine::new(vec![[3.0, 3.0], [3.0, 3.0]], false).is_none());
    }

    /// #2584: centred text sits on its path where its middle is at the
    /// path's middle; at offset 0, the path's start, the half before it
    /// is drawn nowhere, which is what a centred title showed.
    #[test]
    fn centred_text_at_the_middle_of_a_path_is_all_on_it() {
        let line = PathLine::new(vec![[100.0, 500.0], [500.0, 300.0], [900.0, 500.0]], false).unwrap();
        // 24 glyphs, 20 px each: shorter than the path.
        let glyphs: Vec<Laid> = (0..24).map(|i| Laid { along: 10.0 + 20.0 * i as f32, below_baseline: -10.0 }).collect();
        let centred = TextPath::Layer { layer: klipp_timeline::TitleObjectId(1), contour: 0, offset: 0.5, offset_keyframes: Vec::new(), align: PathTextAlign::Center, flip: false };
        let on = |offset| place(&centred, Some(&line), offset, &glyphs, 480.0, [0.0; 2], 0.0).iter().filter(|g| g.is_some()).count();
        assert_eq!(on(0.5), 24, "all of it on the path");
        assert_eq!(on(0.0), 12, "at the start, the first half nowhere");
    }

    #[test]
    fn glyphs_stand_on_a_drawn_path_by_offset_align_and_flip() {
        let line = PathLine::new(vec![[100.0, 500.0], [900.0, 500.0]], false).unwrap();
        let glyphs = [Laid { along: 10.0, below_baseline: -20.0 }, Laid { along: 90.0, below_baseline: -20.0 }];
        let path = |align, flip| TextPath::Layer { layer: klipp_timeline::TitleObjectId(1), contour: 0, offset: 0.5, offset_keyframes: Vec::new(), align, flip };
        let at = |align, flip| place(&path(align, flip), Some(&line), 0.5, &glyphs, 100.0, [0.0; 2], 0.0);
        let start = at(PathTextAlign::Start, false);
        assert_eq!(start[0].unwrap(), Placed { centre: [510.0, 480.0], angle: 0.0 }, "half way, above the baseline");
        assert!(close(at(PathTextAlign::Center, false)[0].unwrap().centre, [460.0, 480.0]));
        assert!(close(at(PathTextAlign::End, false)[1].unwrap().centre, [490.0, 480.0]));
        let flipped = at(PathTextAlign::Start, true)[0].unwrap();
        assert!(close(flipped.centre, [490.0, 520.0]) && (flipped.angle.abs() - std::f32::consts::PI).abs() < 1e-5, "the other side, the other way: {flipped:?}");
        let off = place(&path(PathTextAlign::Start, false), Some(&line), 0.95, &glyphs, 100.0, [0.0; 2], 0.0);
        assert!(off[0].is_some() && off[1].is_none(), "past an open path's end is not drawn");
        assert!(place(&path(PathTextAlign::Start, false), None, 0.0, &glyphs, 100.0, [0.0; 2], 0.0).iter().all(Option::is_none));
    }
}
