//! What the engine and the UI agree on about the scopes: the sizes the
//! GPU scopes are drawn at, the histogram's layout for a display choice,
//! and the CIE chromaticity scope's place in the active set.
//!
//! The scopes themselves are drawn in `klipp_effects::gpu_scopes`; the
//! engine drives them from `render_scopes_on_gpu`.

/// The GPU scopes' size before the UI has said what size it shows them at.
pub const SCOPE_OUT_W: u32 = 1024;
pub const SCOPE_OUT_H: u32 = 512;
/// The GPU vectorscope's output: a square.
pub const VECTOR_OUT: u32 = 1024;
/// The largest GPU scope image, in physical pixels: wider than a 4K
/// screen, and taller than any pane on one.
pub const MAX_SCOPE_OUT_W: u32 = 4096;
pub const MAX_SCOPE_OUT_H: u32 = 2400;

/// The size to draw GPU scope `mode` at: the physical size the UI shows it
/// at, capped, or the default while the UI has not said. The vectorscope
/// is square, at the smaller side.
pub fn scope_out_size(sizes: &std::collections::BTreeMap<u8, (u32, u32)>, mode: u8) -> (u32, u32) {
    let (w, h) = match sizes.get(&mode) {
        Some(&(w, h)) if w >= 32 && h >= 32 => (w.min(MAX_SCOPE_OUT_W), h.min(MAX_SCOPE_OUT_H)),
        _ if mode == 2 => (VECTOR_OUT, VECTOR_OUT),
        _ => (SCOPE_OUT_W, SCOPE_OUT_H),
    };
    if mode == 2 {
        let side = w.min(h);
        (side, side)
    } else {
        (w, h)
    }
}

/// The GPU histogram's layout for a histogram display choice. The engine
/// draws with it and the UI lays its graticule out with it, so the two
/// agree on where each panel is.
pub fn histogram_layout(mode: klipp_core::HistogramDisplayMode) -> klipp_effects::gpu_scopes::HistogramLayout {
    use klipp_effects::gpu_scopes::HistogramLayout;
    match mode {
        klipp_core::HistogramDisplayMode::Overlay => HistogramLayout::Overlay,
        klipp_core::HistogramDisplayMode::Parade => HistogramLayout::Parade,
        klipp_core::HistogramDisplayMode::Stack => HistogramLayout::Stack,
        klipp_core::HistogramDisplayMode::Luma => HistogramLayout::Luma,
    }
}

/// The CIE chromaticity scope's place in `active_scope_modes`.
///
/// Not a GPU scope — it is drawn in the UI. What the byte asks the engine
/// for is [`crate::texture::FrameBuffer::cie_samples`]: the picture in the
/// project's working space, linear, before the display transform. Sits
/// above the four drawn modes (0..=3) so nothing tries to draw it.
pub const CIE_SAMPLES_MODE: u8 = 4;

/// Longest edge of the sample grid the CIE scope measures.
///
/// The scope plots one dot per sample and the diagram is a few hundred
/// pixels across, so more samples than this buy nothing but memory: a 4K
/// frame reduces to 384x216 RGB f32, under a megabyte.
pub const CIE_SAMPLES_MAX_EDGE: u32 = 384;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_is_drawn_at_the_size_it_is_shown_at_capped_and_the_vectorscope_square() {
        let mut sizes = std::collections::BTreeMap::new();
        assert_eq!(scope_out_size(&sizes, 0), (SCOPE_OUT_W, SCOPE_OUT_H));
        assert_eq!(scope_out_size(&sizes, 2), (VECTOR_OUT, VECTOR_OUT));
        sizes.insert(0, (1750, 875));
        sizes.insert(2, (900, 700));
        sizes.insert(3, (9000, 9000));
        assert_eq!(scope_out_size(&sizes, 0), (1750, 875));
        assert_eq!(scope_out_size(&sizes, 2), (700, 700));
        assert_eq!(scope_out_size(&sizes, 3), (MAX_SCOPE_OUT_W, MAX_SCOPE_OUT_H));
    }
}
