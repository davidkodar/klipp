//! Visual letter picker for the HYPERZOOM inspector section.
//!
//! Resolves which `Title` clip's glyphs are pickable for the
//! currently-selected hyperzoom source, caches the per-character
//! positions across frames (invalidating when any field that
//! affects layout changes), and paints the inspector preview slot
//! with click + drag-to-snap behaviour.
//!
//! # Data model
//!
//! The picker is **snap-only**: clicking a glyph snaps the focal
//! point to the glyph's centroid in clip-space (writes back to
//! `HyperzoomScratch.focal_x` / `focal_y`). No glyph reference is
//! stored; if the user later re-shapes the title, the focal point
//! sits at the OLD coordinates — same limitation as any focal-point
//! coords today. Live-binding to a glyph (so the focal point follows
//! the glyph through animated titles + text edits) is the scope of
//! `HZ-NEXT-GLYPH-LOCK`, the next item in the queue.
//!
//! # Cache invalidation
//!
//! `glyph_layout_hash` hashes only the `TitleData` fields that
//! `klipp_title::TitleRenderer::compute_char_positions` reads —
//! text, font_name, font_style, font_size, case_transform,
//! alignment, tracking, line_spacing, baseline_shift, style_runs
//! (start/end/font_size only — the renderer ignores per-run
//! font_name/font_style today), title position, plus the render
//! frame dimensions. Tests pin every field. Other fields (color,
//! gradient, glow, outline, motion blur, character animation, …)
//! deliberately don't invalidate the cache so the picker survives
//! a colour tweak without recomputing.

use egui::{Color32, Pos2, Rect, Stroke, Ui};
use klipp_core::{ClipId, Timecode};
use klipp_timeline::{Clip, Sequence, TitleData};
use klipp_title::{CharPosition, TitleRenderer};

/// Snap radius in **logical screen pixels** (egui's pos2 units —
/// already DPI-aware, so `12.0` reads as the same physical distance
/// regardless of monitor scaling). When dragging the focal-point
/// crosshair, snap to the nearest glyph centroid within this
/// distance; outside it, free-position.
pub const SNAP_RADIUS_LOGICAL_PX: f32 = 12.0;

/// Resolve the title clip whose glyphs should be pickable for the
/// hyperzoom-source clip identified by `selected_clip_id`. Walks
/// in priority order:
///
/// 1. **Self** — `selected_clip_id` IS a `Title` clip. The user is
///    hyperzooming directly on a title; pick from its own glyphs.
/// 2. **Matte source** — `selected_clip_id` has a `matte_source`
///    binding pointing at a `Title` clip. Canonical "fly into the
///    letter where the letter is the matte" workflow — the title's
///    pixels aren't visible directly (the source clip is cookie-
///    cut to letter shapes), so the picker's preview slot is the
///    user's only way to see the letters.
/// 3. **Overlapping title** — A `Title` clip on a **higher** track
///    whose timeline range contains the playhead. The "title sits
///    over the shot" no-matte case. Higher-track because that's
///    the convention for overlay titles in Klipp.
/// 4. **None** — no title context; picker preview hidden, user
///    falls back to the spinboxes + draggable focal-point gizmo
///    on the main viewer.
///
/// Returns the clip itself so the caller can pull `title_data()`
/// + `id` without re-walking the sequence.
pub fn picker_target_for_clip<'a>(
    seq: &'a Sequence,
    selected_clip_id: ClipId,
    playhead: Timecode,
) -> Option<&'a Clip> {
    let selected = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .find(|c| c.id == selected_clip_id)?;

    // 1. Self
    if selected.title_data().is_some() {
        return Some(selected);
    }

    // 2. Matte source
    if let Some(binding) = selected.matte_source.as_ref() {
        if let Some(matte_clip) = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == binding.source_clip_id)
        {
            if matte_clip.title_data().is_some() {
                return Some(matte_clip);
            }
        }
    }

    // 3. Overlapping title on a higher track
    let selected_track_idx = seq
        .tracks
        .iter()
        .position(|t| t.clips.iter().any(|c| c.id == selected_clip_id))?;
    let pl = playhead.to_secs_f64();
    for (ti, track) in seq.tracks.iter().enumerate() {
        if ti <= selected_track_idx {
            continue;
        }
        for c in &track.clips {
            if c.title_data().is_none() {
                continue;
            }
            let in_secs = c.timeline_in.to_secs_f64();
            let out_secs = c.timeline_out.to_secs_f64();
            // Half-open: [in, out). A title that just-ended at the
            // playhead doesn't qualify — its glyphs aren't on screen.
            if pl >= in_secs && pl < out_secs {
                return Some(c);
            }
        }
    }

    None
}

/// Hash every `TitleData` field that affects glyph layout. Used as
/// the cache key so position recomputation fires when (and only
/// when) one of these fields changes. Frame dimensions are folded
/// into the hash too — a 1920×1080 layout differs from 1280×720
/// for the same TitleData.
///
/// Deliberately **excludes** fields that don't affect glyph
/// positions: color, gradient, glow, outline, shadow, extrusion,
/// motion blur, character animation, texture fill, … A colour
/// tweak shouldn't invalidate the cache.
pub fn glyph_layout_hash(td: &TitleData, frame_w: u32, frame_h: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();

    let tb = td.primary_text_or_default();
    tb.text.hash(&mut h);
    tb.font_name.hash(&mut h);
    std::mem::discriminant(&tb.font_style).hash(&mut h);
    tb.font_size.to_bits().hash(&mut h);
    std::mem::discriminant(&tb.case_transform).hash(&mut h);
    std::mem::discriminant(&tb.alignment).hash(&mut h);
    tb.tracking.to_bits().hash(&mut h);
    tb.line_spacing.to_bits().hash(&mut h);
    tb.baseline_shift.to_bits().hash(&mut h);

    // Style runs: only start, end, and font_size are read by
    // `compute_char_positions` — per-run font_name and font_style
    // are silently dropped today (the renderer uses the base
    // tb.font_name + tb.font_style for the whole text). Include
    // only the fields that actually affect layout.
    for run in &tb.style_runs {
        run.start.hash(&mut h);
        run.end.hash(&mut h);
        run.font_size.map(f32::to_bits).hash(&mut h);
    }

    let pos = td.position();
    pos.x.to_bits().hash(&mut h);
    pos.y.to_bits().hash(&mut h);

    frame_w.hash(&mut h);
    frame_h.hash(&mut h);

    h.finish()
}

/// One per-clip entry in the picker's position cache. Lives on
/// `AppState.glyph_position_cache`; recomputed when `layout_hash`
/// no longer matches the current `TitleData` + render dimensions.
#[derive(Debug, Clone)]
pub struct GlyphCacheEntry {
    pub layout_hash: u64,
    pub frame_w: u32,
    pub frame_h: u32,
    pub positions: Vec<CharPosition>,
}

/// Look up or refresh the glyph positions for `title_clip`.
/// `cache` is mutably borrowed because a stale entry triggers a
/// recompute via `renderer.compute_char_positions`. Returns
/// `&[CharPosition]` valid for the lifetime of the cache entry.
pub fn ensure_glyph_positions<'a>(
    cache: &'a mut std::collections::HashMap<ClipId, GlyphCacheEntry>,
    renderer: &mut TitleRenderer,
    title_clip: &Clip,
    frame_w: u32,
    frame_h: u32,
) -> &'a [CharPosition] {
    let Some(td) = title_clip.title_data() else {
        // Caller's responsibility to pass a Title clip; if not, an
        // empty slice keeps the picker silently no-op rather than
        // panicking.
        return &[];
    };
    let target_hash = glyph_layout_hash(td, frame_w, frame_h);
    let needs_refresh = cache
        .get(&title_clip.id)
        .map(|e| e.layout_hash != target_hash || e.frame_w != frame_w || e.frame_h != frame_h)
        .unwrap_or(true);
    if needs_refresh {
        let positions = renderer.compute_char_positions(td, frame_w, frame_h);
        cache.insert(
            title_clip.id,
            GlyphCacheEntry {
                layout_hash: target_hash,
                frame_w,
                frame_h,
                positions,
            },
        );
    }
    &cache.get(&title_clip.id).expect("just inserted").positions
}

/// Hit-test result of a pointer position against a glyph list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HitTest {
    /// Pointer is inside a glyph's bounding rectangle. The contained
    /// `usize` is the `CharPosition::char_index` of the snapped
    /// glyph (NOT the index into the positions slice — those are
    /// the same in practice, but using char_index keeps the API
    /// stable even if the positions vec ever gets sorted differently).
    InsideGlyph { char_index: usize },
    /// Pointer is outside every glyph rect but within
    /// `SNAP_RADIUS_LOGICAL_PX` screen pixels of the nearest
    /// centroid. Snap to that glyph.
    NearGlyph { char_index: usize },
    /// Free-positioned: outside every glyph rect AND past the snap
    /// radius. Caller should write the raw cursor norm coords.
    FreePosition,
}

/// Hit-test a pointer's screen position against a glyph layout.
///
/// `frame_to_screen` projects glyph frame-pixel coords (from
/// `CharPosition`) to the same screen-pixel space as `pointer_screen`.
/// Caller is the inspector preview, so this is
/// [`crate::panels::viewer::viewer_frame_px_to_screen`] partially
/// applied with the preview's image rect + frame dimensions.
///
/// Bbox-priority: if the pointer is INSIDE any glyph's rect, that
/// wins immediately (no further work). Otherwise we pick the
/// glyph whose **bbox edge** (not centroid) is nearest the pointer,
/// snapping when that edge-distance ≤ `SNAP_RADIUS_LOGICAL_PX`.
///
/// Edge-distance over centroid-distance because typical title
/// glyphs are 50–100 px tall — the centroid sits 25–50 px from any
/// edge, so a 12-px centroid radius would never fire for normal-
/// sized text. Edge-distance behaves like the user expects: drag
/// near a glyph from any angle, snap kicks in.
pub fn hit_test_glyphs<F>(
    positions: &[CharPosition],
    pointer_screen: Pos2,
    frame_to_screen: F,
) -> HitTest
where
    F: Fn(f32, f32) -> Pos2,
{
    // Bbox pass.
    for cp in positions {
        if cp.advance <= 0.5 {
            continue; // skip zero-advance entries (newlines, end-of-text marker)
        }
        let tl = frame_to_screen(cp.x, cp.y);
        let br = frame_to_screen(cp.x + cp.advance, cp.y + cp.height);
        let rect = Rect::from_two_pos(tl, br);
        if rect.contains(pointer_screen) {
            return HitTest::InsideGlyph { char_index: cp.char_index };
        }
    }
    // Nearest-edge pass (clamped Euclidean — 0 inside, else
    // distance to nearest edge).
    let mut best: Option<(usize, f32)> = None;
    for cp in positions {
        if cp.advance <= 0.5 {
            continue;
        }
        let tl = frame_to_screen(cp.x, cp.y);
        let br = frame_to_screen(cp.x + cp.advance, cp.y + cp.height);
        let rect = Rect::from_two_pos(tl, br);
        let cx = pointer_screen.x.clamp(rect.min.x, rect.max.x);
        let cy = pointer_screen.y.clamp(rect.min.y, rect.max.y);
        let dx = pointer_screen.x - cx;
        let dy = pointer_screen.y - cy;
        let d = (dx * dx + dy * dy).sqrt();
        if d <= SNAP_RADIUS_LOGICAL_PX
            && best.map(|(_, bd)| d < bd).unwrap_or(true)
        {
            best = Some((cp.char_index, d));
        }
    }
    match best {
        Some((ci, _)) => HitTest::NearGlyph { char_index: ci },
        None => HitTest::FreePosition,
    }
}

/// Convert a glyph's centroid (frame-pixel coords) to clip-space
/// `(focal_x, focal_y)` ∈ [0, 1]. The focal-point gizmo stores
/// `(focal_x, focal_y)`; this is the writeback function from a
/// snap event.
pub fn glyph_centroid_to_focal(
    cp: &CharPosition,
    frame_w: u32,
    frame_h: u32,
) -> (f32, f32) {
    let cx = cp.x + cp.advance * 0.5;
    let cy = cp.y + cp.height * 0.5;
    (
        (cx / frame_w.max(1) as f32).clamp(0.0, 1.0),
        (cy / frame_h.max(1) as f32).clamp(0.0, 1.0),
    )
}

/// Paint the picker preview onto `image_rect`:
///
/// * Faint glyph hit-rects so the user sees what's clickable.
/// * Brightened rect on `hover_index` (currently-hovered glyph,
///   if any) — provides feedback before commit.
/// * Bright rect + centroid dot on `snapped_index` (the
///   persistently-highlighted glyph from a previous click; survives
///   pointer leaving the rect).
///
/// Rendering of the title bitmap into `image_rect` is the caller's
/// responsibility (the inspector uploads the bitmap as an egui
/// texture before calling this).
pub fn paint_glyph_overlays(
    ui: &Ui,
    image_rect: Rect,
    positions: &[CharPosition],
    frame_w: u32,
    frame_h: u32,
    hover_index: Option<usize>,
    snapped_index: Option<usize>,
    accent: Color32,
) {
    let painter = ui.painter_at(image_rect);
    for cp in positions {
        // Skip zero-advance entries (newlines, end-of-text marker).
        if cp.advance <= 0.5 {
            continue;
        }
        let tl = crate::panels::viewer::viewer_frame_px_to_screen(image_rect, frame_w as f32, frame_h as f32, cp.x, cp.y);
        let br = crate::panels::viewer::viewer_frame_px_to_screen(image_rect, frame_w as f32, frame_h as f32, cp.x + cp.advance, cp.y + cp.height);
        let rect = Rect::from_two_pos(tl, br);
        let is_snapped = snapped_index == Some(cp.char_index);
        let is_hover = hover_index == Some(cp.char_index);
        let alpha: u8 = if is_snapped { 220 } else if is_hover { 170 } else { 70 };
        let stroke_w = if is_snapped { 1.5 } else { 1.0 };
        let stroke_col = Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha);
        painter.rect_stroke(rect, 0.0, Stroke::new(stroke_w, stroke_col));
        if is_snapped {
            // Centroid dot — confirms the snap visually even when
            // the user has dragged the focal-point gizmo elsewhere.
            painter.circle_filled(rect.center(), 2.5, accent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{
        Clip, ClipKind, FontStyle, MatteBinding, Sequence, StyleRun, TextAlignment,
        TextBox, TextCase, TitleData, TitlePosition, Track, TrackKind,
    };

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn make_title(text: &str) -> TitleData {
        let mut tb = TextBox::default();
        tb.text = text.to_string();
        tb.font_name = "Sans".to_string();
        tb.font_size = 72.0;
        tb.font_style = FontStyle::Regular;
        tb.alignment = TextAlignment::Center;
        TitleData::single_text(tb)
    }

    fn title_clip(text: &str, in_f: i64, out_f: i64) -> Clip {
        // No `Clip::title` ctor exists; bend `Clip::media`'s output
        // by swapping the kind. Same pattern used elsewhere when
        // tests need a title clip with a specific text.
        let mut c = Clip::media(MediaId::new(), tc(in_f), tc(out_f), tc(0), tc(out_f - in_f));
        c.kind = ClipKind::Title(make_title(text));
        c
    }

    fn media_clip(in_f: i64, out_f: i64) -> Clip {
        Clip::media(MediaId::new(), tc(in_f), tc(out_f), tc(0), tc(out_f - in_f))
    }

    fn track_with(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, kind);
        t.clips = clips;
        t
    }

    fn seq_with(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = tracks;
        s
    }

    // ───────────────── picker_target_for_clip ─────────────────

    #[test]
    fn picker_target_resolves_self_when_title_selected() {
        let title = title_clip("Hi", 0, 24);
        let title_id = title.id;
        let seq = seq_with(vec![track_with("V1", TrackKind::Video, vec![title])]);
        let r = picker_target_for_clip(&seq, title_id, tc(12)).expect("title-self resolves");
        assert_eq!(r.id, title_id);
    }

    #[test]
    fn picker_target_resolves_matte_source_when_video_selected() {
        // Canonical case: V2 video has matte_source pointing at title.
        let title = title_clip("Hi", 0, 48);
        let title_id = title.id;
        let mut consumer = media_clip(0, 48);
        consumer.matte_source = Some(MatteBinding::new(title_id));
        let consumer_id = consumer.id;
        let seq = seq_with(vec![
            track_with("V1", TrackKind::Video, vec![consumer]),
            track_with("V2", TrackKind::Video, vec![title]),
        ]);
        let r = picker_target_for_clip(&seq, consumer_id, tc(24)).expect("matte source resolves");
        assert_eq!(r.id, title_id);
    }

    #[test]
    fn picker_target_resolves_overlapping_title_on_higher_track() {
        // V1 video + V2 title overlapping at playhead, no matte.
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let title = title_clip("Hi", 0, 48);
        let title_id = title.id;
        let seq = seq_with(vec![
            track_with("V1", TrackKind::Video, vec![consumer]),
            track_with("V2", TrackKind::Video, vec![title]),
        ]);
        let r = picker_target_for_clip(&seq, consumer_id, tc(24)).expect("overlapping title resolves");
        assert_eq!(r.id, title_id);
    }

    #[test]
    fn picker_target_skips_overlapping_title_on_lower_track() {
        // Title BELOW the consumer doesn't qualify (Klipp's overlay
        // convention is higher-track-on-top).
        let title = title_clip("Hi", 0, 48);
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let seq = seq_with(vec![
            track_with("V1", TrackKind::Video, vec![title]),
            track_with("V2", TrackKind::Video, vec![consumer]),
        ]);
        let r = picker_target_for_clip(&seq, consumer_id, tc(24));
        assert!(r.is_none(), "lower-track title shouldn't resolve");
    }

    #[test]
    fn picker_target_skips_non_overlapping_title() {
        // Title on a higher track but its time range doesn't contain
        // the playhead.
        let title = title_clip("Hi", 60, 80);
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let seq = seq_with(vec![
            track_with("V1", TrackKind::Video, vec![consumer]),
            track_with("V2", TrackKind::Video, vec![title]),
        ]);
        let r = picker_target_for_clip(&seq, consumer_id, tc(24));
        assert!(r.is_none());
    }

    #[test]
    fn picker_target_returns_none_when_clip_missing() {
        // Stale dispatch — clip removed between event and lookup.
        let seq = seq_with(vec![track_with("V1", TrackKind::Video, vec![media_clip(0, 48)])]);
        let r = picker_target_for_clip(&seq, ClipId::new(), tc(0));
        assert!(r.is_none());
    }

    #[test]
    fn picker_target_returns_none_for_pure_video_no_matte_no_overlap() {
        // No matte source, no overlapping title — picker stays
        // hidden, focal-point gizmo on main viewer is the only UI.
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let seq = seq_with(vec![track_with("V1", TrackKind::Video, vec![consumer])]);
        let r = picker_target_for_clip(&seq, consumer_id, tc(24));
        assert!(r.is_none());
    }

    // ────────────────── glyph_layout_hash ──────────────────
    //
    // Per-field invalidation pin: change ONE layout-affecting field
    // → hash changes. Change a non-layout field (color, glow, etc)
    // → hash UNCHANGED. Tests document which fields are layout vs
    // visual-fill.

    fn baseline_layout_hash() -> u64 {
        glyph_layout_hash(&make_title("Hello"), 1920, 1080)
    }

    fn modify_baseline_and_hash(modify: impl FnOnce(&mut TitleData)) -> u64 {
        let mut td = make_title("Hello");
        modify(&mut td);
        glyph_layout_hash(&td, 1920, 1080)
    }

    fn modify_text_box(td: &mut TitleData, modify: impl FnOnce(&mut TextBox)) {
        let tb = td.primary_text_mut().expect("make_title produced a TextBox object");
        modify(tb);
    }

    #[test]
    fn hash_changes_on_text_edit() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.text = "World".into()));
        assert_ne!(baseline, modified, "text edit must invalidate cache");
    }

    #[test]
    fn hash_changes_on_font_name() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.font_name = "Mono".into()));
        assert_ne!(baseline, modified, "font_name must invalidate cache");
    }

    #[test]
    fn hash_changes_on_font_style() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.font_style = FontStyle::Bold));
        assert_ne!(baseline, modified, "font_style must invalidate cache");
    }

    #[test]
    fn hash_changes_on_font_size() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.font_size = 144.0));
        assert_ne!(baseline, modified, "font_size must invalidate cache");
    }

    #[test]
    fn hash_changes_on_case_transform() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.case_transform = TextCase::Uppercase));
        assert_ne!(baseline, modified, "case_transform must invalidate cache");
    }

    #[test]
    fn hash_changes_on_alignment() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.alignment = TextAlignment::Left));
        assert_ne!(baseline, modified, "alignment must invalidate cache");
    }

    #[test]
    fn hash_changes_on_tracking() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.tracking = 5.0));
        assert_ne!(baseline, modified, "tracking must invalidate cache");
    }

    #[test]
    fn hash_changes_on_line_spacing() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.line_spacing = 1.5));
        assert_ne!(baseline, modified, "line_spacing must invalidate cache");
    }

    #[test]
    fn hash_changes_on_baseline_shift() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.baseline_shift = 20.0));
        assert_ne!(baseline, modified, "baseline_shift must invalidate cache");
    }

    #[test]
    fn hash_changes_on_style_run_addition() {
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| {
            tb.style_runs.push(StyleRun {
                start: 0, end: 1,
                font_name: None, font_size: Some(120.0),
                color: None, font_style: None, tracking: None, baseline_offset: None,
            });
        }));
        assert_ne!(baseline, modified, "style_run addition must invalidate cache");
    }

    #[test]
    fn hash_changes_on_position() {
        let mut td = make_title("Hello");
        let h0 = glyph_layout_hash(&td, 1920, 1080);
        // `TitleData::position()` reads from `objects[0].transform.center`;
        // mutate that directly — there's no `set_position` setter today.
        td.objects.first_mut().expect("default object exists").transform.center =
            TitlePosition { x: 0.25, y: 0.75 };
        let h1 = glyph_layout_hash(&td, 1920, 1080);
        assert_ne!(h0, h1, "position change must invalidate cache");
    }

    #[test]
    fn hash_changes_on_frame_dimensions() {
        let td = make_title("Hello");
        let h_1080 = glyph_layout_hash(&td, 1920, 1080);
        let h_720 = glyph_layout_hash(&td, 1280, 720);
        assert_ne!(h_1080, h_720, "frame dimensions must invalidate cache");
    }

    #[test]
    fn hash_does_not_change_on_color_only_edit() {
        // Pin: color is a fill-only field; doesn't move glyphs.
        // Changing it must NOT invalidate the cache or the picker
        // recomputes wastefully on every colour drag.
        let baseline = baseline_layout_hash();
        let modified = modify_baseline_and_hash(|td| modify_text_box(td, |tb| tb.color = [255, 0, 0, 255]));
        assert_eq!(baseline, modified, "color is fill-only, must NOT invalidate");
    }

    // ────────────────── hit_test_glyphs ──────────────────

    fn fake_position(char_index: usize, x: f32, y: f32, advance: f32, height: f32) -> CharPosition {
        CharPosition { char_index, byte_offset: char_index, x, y, advance, height }
    }

    /// Identity frame_to_screen (frame px == screen px) for
    /// hit-test math tests — keeps the assertions readable.
    fn identity_proj(x: f32, y: f32) -> Pos2 {
        Pos2 { x, y }
    }

    #[test]
    fn hit_test_inside_glyph_bbox_wins() {
        let positions = vec![
            fake_position(0, 100.0, 100.0, 50.0, 60.0),
            fake_position(1, 200.0, 100.0, 50.0, 60.0),
        ];
        let h = hit_test_glyphs(&positions, Pos2::new(125.0, 130.0), identity_proj);
        assert!(matches!(h, HitTest::InsideGlyph { char_index: 0 }));
    }

    #[test]
    fn hit_test_near_glyph_within_snap_radius() {
        // Glyph bbox (100, 100) to (150, 160). Pointer 8 px below
        // the bottom edge — inside SNAP_RADIUS_LOGICAL_PX (12).
        // Edge-distance from (125, 168) to bbox bottom y=160 is 8.
        let positions = vec![fake_position(0, 100.0, 100.0, 50.0, 60.0)];
        let h = hit_test_glyphs(&positions, Pos2::new(125.0, 168.0), identity_proj);
        assert!(matches!(h, HitTest::NearGlyph { char_index: 0 }), "got {:?}", h);
    }

    #[test]
    fn hit_test_free_position_outside_radius() {
        // Bbox bottom at y=160; pointer at y=200 is 40 px away — well
        // past the 12 px snap radius. FreePosition.
        let positions = vec![fake_position(0, 100.0, 100.0, 50.0, 60.0)];
        let h = hit_test_glyphs(&positions, Pos2::new(125.0, 200.0), identity_proj);
        assert!(matches!(h, HitTest::FreePosition), "got {:?}", h);
    }

    #[test]
    fn hit_test_picks_nearest_edge_when_two_glyphs_in_range() {
        // Two 20×20 glyphs separated by a 10-px gap on x.
        //   Glyph 0: bbox (100,100)–(120,120)
        //   Glyph 1: bbox (130,100)–(150,120)
        // Pointer at (123, 110) is in the gap: 3 px from glyph 0's
        // right edge, 7 px from glyph 1's left edge. Both within
        // snap radius; glyph 0 wins.
        let positions = vec![
            fake_position(0, 100.0, 100.0, 20.0, 20.0),
            fake_position(1, 130.0, 100.0, 20.0, 20.0),
        ];
        let h = hit_test_glyphs(&positions, Pos2::new(123.0, 110.0), identity_proj);
        assert!(matches!(h, HitTest::NearGlyph { char_index: 0 }), "got {:?}", h);
    }

    #[test]
    fn hit_test_centroid_inside_large_bbox_is_inside_not_near() {
        // Pin: a pointer near the centroid of a typical-sized title
        // glyph (50×60 px) lands INSIDE the bbox, returning
        // InsideGlyph — not NearGlyph. The previous attempt at this
        // function used centroid-distance and would have produced
        // NearGlyph for any pointer within 12 px of (125, 130),
        // even though those points are deep inside the bbox.
        let positions = vec![fake_position(0, 100.0, 100.0, 50.0, 60.0)];
        let h = hit_test_glyphs(&positions, Pos2::new(125.0, 130.0), identity_proj);
        assert!(matches!(h, HitTest::InsideGlyph { char_index: 0 }), "got {:?}", h);
    }

    #[test]
    fn hit_test_skips_zero_advance_entries() {
        // The picker skips zero-advance positions (newlines,
        // end-of-text marker). Verify hit_test_glyphs still
        // handles them correctly — bbox check on a zero-width
        // rect can't match, so they fall to centroid distance,
        // and a zero-width glyph's centroid is at (x, y+h/2),
        // which can be near a pointer if it sits there.
        let positions = vec![
            fake_position(0, 100.0, 100.0, 0.0, 60.0),  // zero-advance newline
            fake_position(1, 200.0, 100.0, 50.0, 60.0), // visible glyph
        ];
        // Pointer near the visible glyph's centroid.
        let h = hit_test_glyphs(&positions, Pos2::new(225.0, 130.0), identity_proj);
        assert!(matches!(h, HitTest::InsideGlyph { char_index: 1 }));
    }

    #[test]
    fn glyph_centroid_to_focal_centres_correctly() {
        let cp = fake_position(0, 940.0, 520.0, 40.0, 40.0); // centroid (960, 540)
        let (fx, fy) = glyph_centroid_to_focal(&cp, 1920, 1080);
        assert!((fx - 0.5).abs() < 1e-6);
        assert!((fy - 0.5).abs() < 1e-6);
    }

    #[test]
    fn glyph_centroid_to_focal_clamps_overflow() {
        // Defensive: a glyph centroid outside the frame (e.g.
        // tracking pushed it off the right edge) must clamp to
        // [0, 1] before being written to focal_x/y.
        let cp = fake_position(0, 2000.0, 1100.0, 40.0, 40.0);
        let (fx, fy) = glyph_centroid_to_focal(&cp, 1920, 1080);
        assert!((fx - 1.0).abs() < 1e-6);
        assert!((fy - 1.0).abs() < 1e-6);
    }
}
