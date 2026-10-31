//! Geometry preparation for GPU 3D text; camera/light/motion are frame data.
use super::*;
use crate::text3d_mesh::GlyphMesh;
use crate::text3d_render::{Pose, Scene};
use std::sync::Arc;

pub struct Text3dGeometry {
    pub id: u64,
    pub meshes: Vec<GlyphMesh>,
    pub pivot: [f32; 3],
    units: UnitMap,
    glyph_units: Vec<usize>,
    em: f32,
    bytes: usize,
}
pub struct PreparedText3d {
    pub geometry: Arc<Text3dGeometry>,
    pub settings: klipp_timeline::Text3d,
    pub poses: Vec<Pose>,
    pub face: [u8; 4],
    pub fx: LayerFx,
    pub padding: u32,
}
impl PreparedText3d {
    pub fn scene(&self) -> Scene<'_> {
        Scene {
            meshes: &self.geometry.meshes,
            sprites: &[],
            settings: &self.settings,
            pivot: self.geometry.pivot,
            face: self.face,
            side: self.settings.side_color,
            bevel: self.settings.bevel_color.unwrap_or(self.face),
            poses: &self.poses,
            texture: None,
        }
    }
}
pub(super) struct GeometryCache {
    key: TitleData,
    frame: (u32, u32),
    geometry: Option<Arc<Text3dGeometry>>,
}
impl TitleRenderer {
    /// Eligible plain 3D text. Rich fills, bitmap glyphs, paths and mixed layers
    /// keep the reference renderer until their GPU interactions are covered.
    pub fn prepare_text3d(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        time: f32,
        window: Option<f32>,
    ) -> Option<PreparedText3d> {
        self.fonts_current();
        if title.objects.len() != 1
            || title.background != TitleBackground::None
            || title.mixes_layers()
        {
            return None;
        }
        let o = &title.objects[0];
        let TitleObjectContent::TextBox(tb) = &o.content else {
            return None;
        };
        if !o.visible
            || o.opacity != 1.0
            || !o.opacity_keyframes.is_empty()
            || !tb.three_d.as_ref().is_some_and(|d| d.enabled)
        {
            return None;
        }
        let fx = o.fx();
        // A still solid is a cached raster, unless it is metal (#2706),
        // which only the GPU draws.
        if !tb.has_keyframes() && !fx.motion.is_active() && !fx.animators.iter().any(|a| a.enabled) && tb.material.is_none()
        {
            return None;
        }
        let allowed_fx = LayerFx {
            motion: fx.motion,
            animators: fx.animators.clone(),
            glows: fx.glows.clone(),
            glow_stack_enabled: fx.glow_stack_enabled,
            shadows: fx.shadows.clone(),
            shadow_stack_enabled: fx.shadow_stack_enabled,
            outlines: fx.outlines.clone(),
            outline_stack_enabled: fx.outline_stack_enabled,
            inner_shadows: fx.inner_shadows.clone(),
            inner_shadow_stack_enabled: fx.inner_shadow_stack_enabled,
            ..Default::default()
        };
        if *fx != allowed_fx {
            return None;
        }
        let transform = klipp_timeline::ObjectTransform {
            center: o.transform.center,
            size: o.transform.size,
            anchor_norm: o.transform.anchor_norm,
            ..TitleData::single_text(Default::default())
                .objects
                .remove(0)
                .transform
        };
        if o.transform != transform {
            return None;
        }
        let allowed = klipp_timeline::TextBox {
            text: tb.text.clone(),
            font_name: tb.font_name.clone(),
            font_style: tb.font_style,
            font_size: tb.font_size,
            font_size_keyframes: tb.font_size_keyframes.clone(),
            color: tb.color,
            color_keyframes: tb.color_keyframes.clone(),
            alignment: tb.alignment,
            tracking: tb.tracking,
            tracking_keyframes: tb.tracking_keyframes.clone(),
            line_spacing: tb.line_spacing,
            line_spacing_keyframes: tb.line_spacing_keyframes.clone(),
            vertical_align: tb.vertical_align,
            baseline_shift: tb.baseline_shift,
            case_transform: tb.case_transform,
            features: tb.features.clone(),
            font_weight: tb.font_weight,
            font_axes: tb.font_axes.clone(),
            three_d: tb.three_d.clone(),
            material: tb.material,
            ..Default::default()
        };
        if *tb != allowed {
            return None;
        }
        let mut evaluated = tb.at(time as f64);
        let settings = evaluated.three_d.clone()?;
        let face = settings.face_color.unwrap_or(evaluated.color);
        // Only these settings change the mesh. The remaining settings are GPU
        // uniforms and must not invalidate geometry when they animate.
        evaluated.three_d = Some(klipp_timeline::Text3d {
            depth: settings.depth,
            front_bevel: settings.front_bevel,
            back_bevel: settings.back_bevel,
            ..Default::default()
        });
        evaluated.color = [255; 4];
        let mut key = TitleData::single_text(evaluated);
        key.objects[0].transform = transform;
        let frame = (width, height);
        let geometry = if let Some(i) = self
            .geometry3d_cache
            .iter()
            .position(|c| c.key == key && c.frame == frame)
        {
            let cached = self.geometry3d_cache.remove(i)?;
            let geometry = cached.geometry.clone();
            self.geometry3d_cache.push_front(cached);
            geometry?
        } else {
            let geometry = self
                .build_text3d_geometry(&key, width, height)
                .map(Arc::new);
            while self.geometry3d_cache.len() >= 4 {
                self.geometry3d_cache.pop_back();
            }
            let bytes = geometry.as_ref().map_or(0, |g| g.bytes);
            if bytes <= 64 << 20 {
                while self
                    .geometry3d_cache
                    .iter()
                    .filter_map(|c| c.geometry.as_ref())
                    .map(|g| g.bytes)
                    .sum::<usize>()
                    + bytes
                    > 64 << 20
                {
                    self.geometry3d_cache.pop_back()?;
                }
                self.geometry3d_cache.push_front(GeometryCache {
                    key,
                    frame,
                    geometry: geometry.clone(),
                });
            }
            geometry?
        };
        let poses = if fx.motion.is_active() || fx.animators.iter().any(|a| a.enabled) {
            let glyphs = glyph_poses(
                &geometry.units,
                &fx.motion,
                &fx.animators,
                time,
                window,
                geometry.em,
                (0.0, 0.0, 1.0),
                false,
            );
            geometry
                .glyph_units
                .iter()
                .map(|u| match glyphs.get(*u) {
                    Some(p) => Pose {
                        anchor: [p.anchor.0, p.anchor.1, geometry.pivot[2]],
                        scale: p.scale,
                        rotation: p.rotation,
                        offset: [p.dx, p.dy],
                        alpha: p.alpha.clamp(0.0, 1.0),
                    },
                    None => Pose::default(),
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(PreparedText3d {
            geometry,
            settings,
            poses,
            face,
            fx: fx.clone(),
            padding: effect_padding(title, frame_scale(width, height)).max(0) as u32 + 2,
        })
    }
    fn build_text3d_geometry(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
    ) -> Option<Text3dGeometry> {
        let tb = title.primary_text()?;
        let settings = tb.three_d.as_ref()?;
        let layout = self.render_layout_at_time(title, width, height, None);
        let placed =
            std::mem::take(&mut *self.last_glyphs3d.lock().unwrap_or_else(|e| e.into_inner()));
        if placed
            .glyphs
            .iter()
            .any(|g| g.sprite.is_some() || g.turn.is_some())
            || !placed.stripes.is_empty()
            || placed.reveal.is_some()
        {
            return None;
        }
        let em = tb.font_size * frame_scale(width, height);
        let depth = settings.depth * em;
        let bevel = |b: &Option<klipp_timeline::Bevel>| {
            b.map(|b| text3d_mesh::BevelPx {
                size: b.size * em,
                segments: b.segments,
                round: b.profile == klipp_timeline::BevelProfile::Round,
            })
        };
        let mut meshes = Vec::new();
        let mut glyph_units = Vec::new();
        let mut bytes = 0usize;
        for g in &placed.glyphs {
            let font = self.font_cache.get(&g.font).unwrap_or(&self.fallback_font);
            let Some(contours) = font.outline(g.glyph, g.px, g.origin, 0.2) else {
                continue;
            };
            let contours =
                text3d_mesh::contours_of(contours.into_iter().map(|c| c.points).collect());
            if contours.is_empty() {
                continue;
            }
            let mesh = text3d_mesh::extrude(
                &contours,
                depth,
                bevel(&settings.front_bevel),
                bevel(&settings.back_bevel),
                25.0,
            );
            bytes += mesh.tris.len() * std::mem::size_of::<text3d_mesh::Tri>()
                + mesh
                    .front
                    .contours
                    .iter()
                    .chain(&mesh.back.contours)
                    .map(|c| c.len() * 8)
                    .sum::<usize>();
            if bytes > 64 << 20 {
                return None;
            }
            meshes.push(mesh);
            glyph_units.push(g.unit);
        }
        if meshes.is_empty() {
            return None;
        }
        let pivot = [
            (layout.text_left + layout.text_right) * 0.5,
            (layout.text_top + layout.text_bottom) * 0.5,
            depth * 0.5,
        ];
        let units = UnitMap {
            x0: 0,
            y0: 0,
            w: 0,
            h: 0,
            index: Vec::new(),
            boxes: layout.glyph_boxes,
            parents: layout.glyph_parents,
            pad: 0,
            path: None,
            tracking_origins: Vec::new(),
            line_boxes: Vec::new(),
            line_ranges: Vec::new(),
            lines_vertical: false,
        };
        let id = self.next_cache_id;
        self.next_cache_id = self.next_cache_id.wrapping_add(1);
        Some(Text3dGeometry {
            id,
            meshes,
            pivot,
            units,
            glyph_units,
            em,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Interpolation, Keyframe};
    fn key(t: f64, v: f32) -> Keyframe {
        Keyframe {
            time_secs: t,
            value: v,
            interpolation: Interpolation::Linear,
            tangent_in: (0.0, 0.0),
            tangent_out: (0.0, 0.0),
            handles: Default::default(),
        }
    }
    fn fixture() -> TitleData {
        let mut settings = klipp_timeline::Text3d::default();
        settings.keys.rotate_y = vec![key(0.0, -30.0), key(2.0, 30.0)];
        settings.keys.light_azimuth = vec![key(0.0, 20.0), key(2.0, 90.0)];
        settings.keys.fov = vec![key(0.0, 35.0), key(2.0, 60.0)];
        TitleData::single_text(klipp_timeline::TextBox {
            text: "Cache".into(),
            font_size: 180.0,
            three_d: Some(settings),
            ..Default::default()
        })
    }
    #[test]
    fn camera_light_and_motion_reuse_geometry_but_mesh_edits_invalidate() {
        let mut r = TitleRenderer::new();
        let title = fixture();
        let a = r.prepare_text3d(&title, 640, 360, 0.2, Some(3.0)).unwrap();
        let b = r.prepare_text3d(&title, 640, 360, 1.2, Some(3.0)).unwrap();
        assert!(Arc::ptr_eq(&a.geometry, &b.geometry));
        assert_ne!(a.settings, b.settings);
        let mut fade = title.clone();
        fade.objects[0].fx_mut().motion = klipp_timeline::TitleMotion::enter(
            klipp_timeline::Motion::new(klipp_timeline::MotionKind::Fade).duration(1.0),
        );
        let c = r.prepare_text3d(&fade, 640, 360, 0.2, Some(3.0)).unwrap();
        let d = r.prepare_text3d(&fade, 640, 360, 0.8, Some(3.0)).unwrap();
        assert!(Arc::ptr_eq(&a.geometry, &c.geometry));
        assert!(c.poses[0].alpha < d.poses[0].alpha);
        for change in 0..6 {
            let mut t = title.clone();
            let TitleObjectContent::TextBox(tb) = &mut t.objects[0].content else {
                unreachable!()
            };
            match change {
                0 => tb.text.push('!'),
                1 => tb.font_size += 20.0,
                2 => tb.three_d.as_mut().unwrap().depth += 0.1,
                3 => {
                    tb.three_d
                        .as_mut()
                        .unwrap()
                        .front_bevel
                        .as_mut()
                        .unwrap()
                        .size += 0.02
                }
                4 => tb.tracking += 1.0,
                _ => tb.font_style = klipp_timeline::FontStyle::Bold,
            }
            let next = r.prepare_text3d(&t, 640, 360, 0.2, Some(3.0)).unwrap();
            assert_ne!(a.geometry.id, next.geometry.id);
        }
        assert!(r.geometry3d_cache.len() <= 4);
        let resized = r.prepare_text3d(&title, 1280, 720, 0.2, Some(3.0)).unwrap();
        assert_ne!(a.geometry.id, resized.geometry.id);
        r.invalidate_cache();
        assert!(r.geometry3d_cache.is_empty());
        let reset = r.prepare_text3d(&title, 640, 360, 0.2, Some(3.0)).unwrap();
        assert_ne!(a.geometry.id, reset.geometry.id);
        let mut unsupported = title;
        unsupported.objects[0].opacity = 0.5;
        assert!(r
            .prepare_text3d(&unsupported, 640, 360, 0.2, Some(3.0))
            .is_none());
    }
}
