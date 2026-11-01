//! Cached tracking for plain horizontal text. Unsupported styling/layout stays
//! on the raster path: moving a flattened glow or an overlapping glyph would
//! lose coverage and would not reproduce the text mask's max blending.
use super::*;
use klipp_timeline::TextBox;

pub(super) fn eligible(title: &TitleData) -> bool {
    if title.objects.len() != 1 || title.background != TitleBackground::None || title.mixes_layers()
    {
        return false;
    }
    let o = &title.objects[0];
    let TitleObjectContent::TextBox(tb) = &o.content else {
        return false;
    };
    if !o.visible
        || o.opacity != 1.0
        || !o.opacity_keyframes.is_empty()
        || tb.tracking_keyframes.is_empty()
        || !tb.font_size.is_finite()
        || tb.font_size < 1.0
        || !tb.line_spacing.is_finite()
        || tb.line_spacing < 0.1
        || tb.alignment == TextAlignment::Justify
    {
        return false;
    }
    // An allowlist also keeps future rendering features on their established
    // path until their interaction with cached glyph coverage is tested.
    let allowed = TextBox {
        text: tb.text.clone(),
        font_name: tb.font_name.clone(),
        font_style: tb.font_style,
        font_size: tb.font_size,
        color: tb.color,
        alignment: tb.alignment,
        tracking: tb.tracking,
        tracking_keyframes: tb.tracking_keyframes.clone(),
        line_spacing: tb.line_spacing,
        vertical_align: tb.vertical_align,
        baseline_shift: tb.baseline_shift,
        case_transform: tb.case_transform,
        features: tb.features.clone(),
        font_weight: tb.font_weight,
        font_axes: tb.font_axes.clone(),
        ..Default::default()
    };
    let transform = klipp_timeline::ObjectTransform {
        center: o.transform.center,
        size: o.transform.size,
        anchor_norm: o.transform.anchor_norm,
        ..TitleData::single_text(TextBox::default())
            .objects
            .remove(0)
            .transform
    };
    let fx = o.fx();
    let allowed_fx = klipp_timeline::LayerFx {
        motion: fx.motion,
        ..Default::default()
    };
    let motion_ok = [fx.motion.enter, fx.motion.exit]
        .into_iter()
        .flatten()
        .all(|m| {
            matches!(
                m.kind,
                klipp_timeline::MotionKind::Fade
                    | klipp_timeline::MotionKind::MaskRise
                    | klipp_timeline::MotionKind::Typewriter
            )
        });
    *tb == allowed && o.transform == transform && *fx == allowed_fx && motion_ok
}

fn separated(boxes: &[[i32; 4]]) -> bool {
    boxes.iter().enumerate().all(|(i, a)| {
        a[2] <= a[0]
            || a[3] <= a[1]
            || boxes[..i].iter().all(|b| {
                b[2] <= b[0]
                    || b[3] <= b[1]
                    || a[0] >= b[2]
                    || b[0] >= a[2]
                    || a[1] >= b[3]
                    || b[1] >= a[3]
            })
    })
}

impl TitleRenderer {
    pub(super) fn tracking_frame(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        time: f32,
    ) -> Option<TitleFrame> {
        let tb = title.primary_text()?;
        let sprite = self.resting_sprite(title, width, height, None);
        let u = sprite.units.as_ref()?;
        let n = u.boxes.len();
        if n == 0 || n > MAX_POSED_GLYPHS || u.tracking_origins.len() != n || !separated(&u.boxes) {
            return None;
        }
        // The sprite cannot recover ink clipped at rest when spacing brings it
        // back onto the frame. Target clipping, however, is safe at placement.
        if u.boxes.iter().any(|b| {
            b[2] > b[0]
                && b[3] > b[1]
                && (b[0] < 0 || b[1] < 0 || b[2] > width as i32 || b[3] > height as i32)
        }) {
            return None;
        }
        let spacing =
            klipp_core::keyframe::evaluate(&tb.tracking_keyframes, time as f64, tb.tracking)
                * frame_scale(width, height);
        if !spacing.is_finite() {
            return None;
        }
        let xs: Vec<f32> = u
            .tracking_origins
            .iter()
            .enumerate()
            .map(|(i, p)| p[0] + spacing * i as f32)
            .collect();
        let min = xs.iter().copied().fold(f32::INFINITY, f32::min);
        let max = xs
            .iter()
            .zip(&u.boxes)
            .map(|(x, b)| x + (b[2] - b[0]) as f32)
            .fold(f32::NEG_INFINITY, f32::max);
        let offset = text_offset_x(
            tb.alignment,
            title.position().x * width as f32,
            max - min,
            min,
            0.0,
        );
        if xs
            .iter()
            .any(|x| !(x + offset).is_finite() || (x + offset).abs() > 100_000_000.0)
        {
            return None;
        }
        let boxes: Vec<[i32; 4]> = xs
            .iter()
            .zip(&u.boxes)
            .map(|(x, b)| {
                let x = (x + offset) as i32;
                [x, b[1], x + b[2] - b[0], b[3]]
            })
            .collect();
        if !separated(&boxes) {
            return None;
        }
        // Only the small layout is per-frame; never clone the coverage map.
        let placed = UnitMap {
            x0: 0,
            y0: 0,
            w: 0,
            h: 0,
            index: Vec::new(),
            boxes,
            parents: u.parents.clone(),
            pad: u.pad,
            path: None,
            tracking_origins: Vec::new(),
            line_boxes: Vec::new(),
            line_ranges: Vec::new(),
            lines_vertical: false,
            text_box: u.text_box,
        };
        let mut poses = glyph_poses(
            &placed,
            &title.primary_fx().motion,
            &[],
            time,
            self.motion_window,
            tb.font_size * frame_scale(width, height),
            (0.0, 0.0, 1.0),
            false,
        );
        for (i, p) in poses.iter_mut().enumerate() {
            let shift = (placed.boxes[i][0] - u.boxes[i][0]) as f32;
            p.anchor.0 -= shift;
            p.dx += shift;
            // Match raster motion rounding after adding the displacement,
            // not before: fractional baselines otherwise shift by a pixel.
            p.dy = (u.tracking_origins[i][1] + p.dy) as i32 as f32 - u.boxes[i][1] as f32;
        }
        let y0 = poses
            .iter()
            .filter(|p| p.moved[3] > p.moved[1])
            .map(|p| p.moved[1])
            .min()
            .unwrap_or(0);
        let y1 = poses
            .iter()
            .filter(|p| p.moved[3] > p.moved[1])
            .map(|p| p.moved[3])
            .max()
            .unwrap_or(0);
        let rows = (
            y0.clamp(0, height as i32) as u32,
            y1.clamp(0, height as i32) as u32,
        );
        // A backing per line under the tracked glyphs (#3237).
        let backing = super::posed_backing(title, u, &poses, frame_scale(width, height));
        let rows = super::widened(rows, backing.as_ref(), height);
        self.last_rows = rows;
        self.last_blur = 0.0;
        Some(TitleFrame::Glyphs {
            sprite,
            poses,
            rows,
            backing,
        })
    }
}
