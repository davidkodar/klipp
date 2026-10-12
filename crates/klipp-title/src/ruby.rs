//! Ruby (#1704): a style run's reading gloss, furigana over kanji, set
//! at half the run's size beside the text it reads: centred above it in
//! a horizontal box, centred down the right of its column in a vertical
//! one. The gloss's glyphs are laid as the text's are and drawn with
//! them, in the run's colour.

use fontdue::layout::{CharacterData, GlyphPosition, GlyphRasterConfig};
use klipp_timeline::StyleRun;

use crate::shaping::{shape_horizontal, Face};

/// The gloss's size, as a share of its base text's.
pub(crate) const RUBY_SCALE: f32 = 0.5;

/// Every run's gloss laid beside its text among `glyphs` (the text's,
/// placed, in frame pixels from the block's top left), runs side by side
/// with one gloss sharing it: each gloss glyph carries its first run's
/// first byte, so it takes that run's colour. `run_px`
/// is a run's size in pixels. Runs without a gloss, or whose text drew
/// no glyph, add none.
pub(crate) fn lay(faces: &[Face], glyphs: &[GlyphPosition], runs: &[StyleRun], run_px: impl Fn(&StyleRun) -> f32, vertical: bool) -> Vec<GlyphPosition> {
    let mut out = Vec::new();
    // Runs side by side with one gloss are one stretch it reads: a
    // selection glossed across two runs, or a glossed word styled in
    // part, is glossed once.
    let mut sorted: Vec<&StyleRun> = runs.iter().filter(|r| r.ruby.as_deref().is_some_and(|g| !g.trim().is_empty())).collect();
    sorted.sort_by_key(|r| r.start);
    let mut spans: Vec<(&StyleRun, usize)> = Vec::new();
    for run in sorted {
        match spans.last_mut() {
            Some((first, end)) if *end == run.start && first.ruby == run.ruby => *end = run.end,
            _ => spans.push((run, run.end)),
        }
    }
    for (run, end) in spans {
        let Some(gloss) = run.ruby.as_deref() else { continue };
        let base: Vec<&GlyphPosition> = glyphs.iter().filter(|g| run.start <= g.byte_offset && g.byte_offset < end && g.width > 0 && g.height > 0).collect();
        let Some(first) = base.first() else { continue };
        // The base on the first glyph's line (or column) alone, when the
        // run breaks across two.
        let base: Vec<&GlyphPosition> = if vertical {
            base.iter().copied().filter(|g| (g.x + g.width as f32 * 0.5 - (first.x + first.width as f32 * 0.5)).abs() < run_px(run) * 0.6).collect()
        } else {
            base.iter().copied().filter(|g| (g.y + g.height as f32 * 0.5 - (first.y + first.height as f32 * 0.5)).abs() < run_px(run) * 0.6).collect()
        };
        let x0 = base.iter().map(|g| g.x).fold(f32::MAX, f32::min);
        let x1 = base.iter().map(|g| g.x + g.width as f32).fold(f32::MIN, f32::max);
        let y0 = base.iter().map(|g| g.y).fold(f32::MAX, f32::min);
        let y1 = base.iter().map(|g| g.y + g.height as f32).fold(f32::MIN, f32::max);
        let face_i = first.font_index;
        let face = &faces[face_i];
        let px = run_px(run) * RUBY_SCALE;
        let gap = px * 0.2;
        // Each gloss character and its advance along the gloss.
        let set: Vec<(u16, char, f32)> = match shape_horizontal(face, gloss, false).filter(|_| !vertical) {
            Some(shaped) => {
                let scale = face.font.font.scale_factor(px);
                shaped.glyph_infos().iter().zip(shaped.glyph_positions()).map(|(info, pos)| (u16::try_from(info.glyph_id).unwrap_or(0), gloss[info.cluster as usize..].chars().next().unwrap_or(' '), pos.x_advance as f32 * scale)).collect()
            }
            None => gloss
                .chars()
                .map(|c| {
                    let gid = face.font.font.lookup_glyph_index(c);
                    // Down a column, a character an em apart.
                    let advance = if vertical { px } else { face.font.metrics_indexed(gid, px).advance_width };
                    (gid, c, advance)
                })
                .collect(),
        };
        let length: f32 = set.iter().map(|g| g.2).sum();
        let (ascent, descent) = face.font.font.horizontal_line_metrics(px).map_or((px * 0.8, -px * 0.2), |m| (m.ascent, m.descent));
        let mut pen = 0.0f32;
        for (gid, c, advance) in set {
            let m = face.font.metrics_indexed(gid, px);
            // The drawing origin: across and down on the frame.
            let (ox, oy) = if vertical {
                // Each character upright, centred across a column of
                // its own right of the base's, the gloss centred along it.
                let middle = x1 + gap + px * 0.5;
                let top = (y0 + y1) * 0.5 - length * 0.5 + pen;
                (middle - m.advance_width * 0.5, top + px * 0.5 + (ascent + descent) * 0.5)
            } else {
                // A line above the base, its middle over the base's.
                ((x0 + x1) * 0.5 - length * 0.5 + pen, y0 - gap + descent)
            };
            out.push(GlyphPosition {
                key: GlyphRasterConfig { glyph_index: gid, px, font_hash: face.font.hash() },
                font_index: face_i,
                parent: c,
                x: (ox + m.xmin as f32).floor(),
                y: (oy - m.height as f32 - m.ymin as f32).floor(),
                width: m.width,
                height: m.height,
                byte_offset: run.start,
                char_data: CharacterData::classify(c, gid),
                user_data: (),
            });
            pen += advance;
        }
    }
    out
}
