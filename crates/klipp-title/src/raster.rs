//! A title's raster as it leaves the renderer (#190): the box of the
//! frame it covers, straight alpha, sRGB-encoded, in half floats packed
//! two to a `u32` (red and green, blue and alpha) as the generators' f16
//! frames are, so the GPU reads both with `unpack2x16float`. Nothing
//! outside the box is stored; it is transparent.
//!
//! Half floats keep what the renderer's f32 layer stack made, about 11
//! bits, where bytes rounded it to 8, and hold values above 1.0. Over
//! its box, a lower third at 4K takes less than its frame did as bytes.

/// Packed half-float pixels over the box `x0..x0+w`, `y0..y0+h` of a
/// `frame` sized frame.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct BoxRaster {
    pub frame: (u32, u32),
    pub x0: u32,
    pub y0: u32,
    pub w: u32,
    pub h: u32,
    /// Two `u32` a pixel, row by row over the box.
    pub texels: Vec<u32>,
}

#[inline]
fn half(v: f32) -> u32 {
    half::f16::from_f32(v).to_bits() as u32
}

#[inline]
fn unhalf(bits: u32) -> f32 {
    half::f16::from_bits(bits as u16).to_f32()
}

impl BoxRaster {
    /// The box of a frame-sized layer stack (`klipp_title`'s f32 working
    /// frame, straight alpha, sRGB-encoded in byte units 0..255), over
    /// `cols` × `rows`.
    pub fn from_stack(stack: &[f32], frame: (u32, u32), rows: (u32, u32), cols: (u32, u32)) -> Self {
        let (fw, fh) = frame;
        let (y0, y1) = (rows.0.min(fh), rows.1.min(fh));
        let (x0, x1) = (cols.0.min(fw), cols.1.min(fw));
        let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        if w == 0 || h == 0 || stack.len() < (fw as usize) * (fh as usize) * 4 {
            return Self { frame, ..Default::default() };
        }
        let mut texels = Vec::with_capacity((w as usize) * (h as usize) * 2);
        for y in y0..y1 {
            let row = &stack[((y as usize) * (fw as usize) + x0 as usize) * 4..][..(w as usize) * 4];
            for p in row.chunks_exact(4) {
                let k = 1.0 / 255.0;
                texels.push(half(p[0] * k) | (half(p[1] * k) << 16));
                texels.push(half(p[2] * k) | (half(p[3] * k) << 16));
            }
        }
        Self { frame, x0, y0, w, h, texels }
    }

    /// A box's own pixels, `w` × `h` from `(x0, y0)` of a `frame`
    /// sized frame, as an f32 stack in byte units laid row by row over
    /// the box alone.
    pub fn from_box_stack(stack: &[f32], frame: (u32, u32), x0: u32, y0: u32, w: u32, h: u32) -> Self {
        let k = 1.0 / 255.0;
        let texels = stack.chunks_exact(4).flat_map(|p| [half(p[0] * k) | (half(p[1] * k) << 16), half(p[2] * k) | (half(p[3] * k) << 16)]).collect();
        Self { frame, x0, y0, w, h, texels }
    }

    /// Frame-sized RGBA8 as the byte path stored it.
    pub fn from_bytes(rgba: &[u8], frame: (u32, u32), rows: (u32, u32), cols: (u32, u32)) -> Self {
        let stack: Vec<f32> = rgba.iter().map(|&v| v as f32).collect();
        Self::from_stack(&stack, frame, rows, cols)
    }

    /// Whether nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// What the pixels take, in bytes.
    pub fn byte_size(&self) -> usize {
        self.texels.len() * 4
    }

    /// The pixel at frame position `(x, y)`, straight alpha,
    /// sRGB-encoded in byte units 0..255; transparent outside the box.
    #[inline]
    pub fn at(&self, x: i64, y: i64) -> [f32; 4] {
        let (bx, by) = (x - self.x0 as i64, y - self.y0 as i64);
        if bx < 0 || by < 0 || bx >= self.w as i64 || by >= self.h as i64 {
            return [0.0; 4];
        }
        let i = ((by as usize) * (self.w as usize) + bx as usize) * 2;
        let (a, b) = (self.texels[i], self.texels[i + 1]);
        [unhalf(a & 0xFFFF) * 255.0, unhalf(a >> 16) * 255.0, unhalf(b & 0xFFFF) * 255.0, unhalf(b >> 16) * 255.0]
    }

    /// The whole frame as an f32 stack in byte units, transparent outside
    /// the box.
    pub fn to_stack(&self) -> Vec<f32> {
        let (fw, fh) = (self.frame.0 as usize, self.frame.1 as usize);
        let mut out = vec![0.0f32; fw * fh * 4];
        for by in 0..self.h as usize {
            let y = self.y0 as usize + by;
            for bx in 0..self.w as usize {
                let x = self.x0 as usize + bx;
                out[(y * fw + x) * 4..][..4].copy_from_slice(&self.at(x as i64, y as i64));
            }
        }
        out
    }

    /// The whole frame as packed half floats, transparent outside the
    /// box: what a generator's f16 frame is.
    pub fn to_frame_f16(&self) -> Vec<u32> {
        let (fw, fh) = (self.frame.0 as usize, self.frame.1 as usize);
        let mut out = vec![0u32; fw * fh * 2];
        let row = self.w as usize * 2;
        for by in 0..self.h as usize {
            let at = ((self.y0 as usize + by) * fw + self.x0 as usize) * 2;
            out[at..at + row].copy_from_slice(&self.texels[by * row..(by + 1) * row]);
        }
        out
    }

    /// The whole frame as straight-alpha sRGB RGBA8, for what wants
    /// bytes: previews, the engine's paused overlay, the byte APIs.
    pub fn to_bytes(&self) -> Vec<u8> {
        crate::to_bytes(&self.to_stack())
    }

    /// Alpha at `(x, y)` as the byte path stored it, 0..255.
    #[inline]
    pub fn alpha8(&self, x: i64, y: i64) -> u8 {
        self.at(x, y)[3].round().clamp(0.0, 255.0) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stack_round_trips_within_a_thousandth_of_a_level_scale() {
        let (w, h) = (16u32, 8u32);
        let stack: Vec<f32> = (0..w * h * 4).map(|i| (i % 256) as f32 * 0.9 + 0.37).collect();
        let r = BoxRaster::from_stack(&stack, (w, h), (0, h), (0, w));
        let back = r.to_stack();
        for (a, b) in stack.iter().zip(&back) {
            // Half floats over 0..1 hold about 11 bits: well under a level.
            assert!((a - b).abs() <= a.abs() * 1e-3 + 1e-3, "{a} came back {b}");
        }
    }

    #[test]
    fn outside_the_box_is_transparent_and_not_stored() {
        let (w, h) = (20u32, 10u32);
        let mut stack = vec![0.0f32; (w * h * 4) as usize];
        for y in 3..6 {
            for x in 5..9 {
                stack[((y * w + x) * 4) as usize..][..4].copy_from_slice(&[200.0, 100.0, 50.0, 255.0]);
            }
        }
        let r = BoxRaster::from_stack(&stack, (w, h), (3, 6), (5, 9));
        assert_eq!((r.w, r.h, r.texels.len()), (4, 3, 4 * 3 * 2));
        assert_eq!(r.at(4, 4), [0.0; 4]);
        assert_eq!(r.alpha8(5, 3), 255);
        assert_eq!(r.to_bytes(), crate::to_bytes(&stack), "the frame back as it was");
    }

    #[test]
    fn a_lower_third_takes_less_than_its_frame_did_as_bytes() {
        // A 4K lower third: a band 300 rows tall across 2400 columns.
        let (fw, fh) = (3840u32, 2160u32);
        let r = BoxRaster { frame: (fw, fh), x0: 300, y0: 1700, w: 2400, h: 300, texels: vec![0; 2400 * 300 * 2] };
        assert!(r.byte_size() < (fw * fh * 4) as usize, "{} bytes against {}", r.byte_size(), fw * fh * 4);
    }
}
