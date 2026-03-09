/// Multiply every pixel's RGBA channels by `value` (premultiplied alpha style).
/// This dims the entire pixel so it composites correctly over backgrounds.
pub fn apply(mut rgba: Vec<u8>, value: f32) -> Vec<u8> {
    if (value - 1.0).abs() < f32::EPSILON {
        return rgba;
    }
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = (pixel[0] as f32 * value).clamp(0.0, 255.0) as u8;
        pixel[1] = (pixel[1] as f32 * value).clamp(0.0, 255.0) as u8;
        pixel[2] = (pixel[2] as f32 * value).clamp(0.0, 255.0) as u8;
        pixel[3] = (pixel[3] as f32 * value).clamp(0.0, 255.0) as u8;
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        let input = vec![100, 150, 200, 255, 50, 50, 50, 128];
        let output = apply(input.clone(), 1.0);
        assert_eq!(input, output);
    }

    #[test]
    fn half_opacity() {
        let input = vec![100, 150, 200, 200];
        let output = apply(input, 0.5);
        assert_eq!(output[0], 50);  // 100 * 0.5
        assert_eq!(output[1], 75);  // 150 * 0.5
        assert_eq!(output[2], 100); // 200 * 0.5
        assert_eq!(output[3], 100); // 200 * 0.5
    }

    #[test]
    fn zero_opacity() {
        let input = vec![100, 150, 200, 255];
        let output = apply(input, 0.0);
        assert_eq!(output, vec![0, 0, 0, 0]);
    }

    #[test]
    fn rgb_also_scaled() {
        let input = vec![10, 20, 30, 200];
        let output = apply(input, 0.3);
        assert_eq!(output[0], 3);  // 10 * 0.3
        assert_eq!(output[1], 6);  // 20 * 0.3
        assert_eq!(output[2], 9);  // 30 * 0.3
        assert_eq!(output[3], 60); // 200 * 0.3
    }
}
