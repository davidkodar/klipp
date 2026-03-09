use crate::blur;

/// Unsharp mask: output = input + amount * (input - blur(input, 1.0))
pub fn apply(rgba: Vec<u8>, width: u32, height: u32, amount: f32) -> Vec<u8> {
    if amount.abs() < f32::EPSILON {
        return rgba;
    }

    let blurred = blur::apply(rgba.clone(), width, height, 1.0);
    let mut output = rgba;

    for i in (0..output.len()).step_by(4) {
        for ch in 0..3 {
            let orig = output[i + ch] as f32;
            let blur_val = blurred[i + ch] as f32;
            let sharpened = orig + amount * (orig - blur_val);
            output[i + ch] = sharpened.clamp(0.0, 255.0) as u8;
        }
        // Alpha unchanged
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_amount_passthrough() {
        let input = vec![100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255]; // 2x2
        let output = apply(input.clone(), 2, 2, 0.0);
        assert_eq!(input, output);
    }

    #[test]
    fn sharpening_increases_edge_contrast() {
        // 3x1: dark, bright, dark — sharpening should make bright brighter
        let input = vec![
            50, 50, 50, 255,
            200, 200, 200, 255,
            50, 50, 50, 255,
        ];
        let output = apply(input.clone(), 3, 1, 1.0);
        // Center pixel should be brighter or equal (unsharp mask boosts)
        assert!(output[4] >= input[4]);
    }

    #[test]
    fn alpha_unchanged() {
        let input = vec![100, 150, 200, 42, 100, 150, 200, 42, 100, 150, 200, 42, 100, 150, 200, 42]; // 2x2
        let output = apply(input, 2, 2, 1.0);
        for pixel in output.chunks_exact(4) {
            assert_eq!(pixel[3], 42);
        }
    }
}
