//! A grade value's drag: one rule, so the generated rows and the old
//! panel's store the same value for the same drag, bit for bit (M3-4: a
//! drag's step is never an intended difference). The value moves in its
//! stored units by `dx` pixels at `span / 800` a pixel, `span` the row's
//! range, unrounded; the caller clamps it.

/// `stored` moved by a drag of `dx` pixels over a row whose range spans
/// `span`.
pub fn grade_drag(stored: f32, dx: f32, span: f32) -> f32 {
    stored + dx * span / 800.0
}

#[cfg(test)]
mod tests {
    use super::grade_drag;

    /// 800 pixels move the value by the range's span; a pixel by 1/800 of
    /// it, unrounded.
    #[test]
    fn a_drag_moves_a_span_per_800_pixels() {
        assert_eq!(grade_drag(0.0, 800.0, 6.0), 6.0);
        assert_eq!(grade_drag(1.0, -400.0, 2.0), 0.0);
        assert_eq!(grade_drag(0.0, 24.0, 360.0), 10.8);
    }
}
