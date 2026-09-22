//! `StubMaskGenerator` — a placeholder `MaskGenerator` that returns
//! a circular mask around the click point (or the frame center for
//! auto-detect mode).
//!
//! ## Why ship a stub
//!
//! It exists to unblock UI work on the AI Tools system before any
//! real model integration lands. With the stub registered:
//!
//! - The AI Tools panel renders.
//! - Click-on-viewer dispatches a real `MaskRequest`.
//! - The trait's full request → response flow exercises end-to-end.
//! - Tests verify mask preview overlays, accept / reject UX, threading.
//!
//! All that without downloading a single model file. When BiRefNet
//! / SAM v1 / RF-DETR-Seg implementations land in their respective
//! backend crates, they replace the stub via the same registry
//! interface — no UI rewrites.
//!
//! The stub is always compiled — it is a few hundred bytes of code
//! with no dependencies — and is what the registry hands back when no
//! real backend is built in.

use crate::mask_generator::{
    BBox, DevicePreference, MaskBuffer, MaskCandidate, MaskCapabilities, MaskError,
    MaskGenerator, MaskRequest, MaskResponse,
};

/// A `MaskGenerator` that returns a circular mask around the
/// strongest prompt point (first positive click), or the frame
/// center for auto-detect mode. Confidence is a sigmoid-shaped
/// function of mask area so multi-prompt UIs see meaningful values
/// when toggling between candidates.
pub struct StubMaskGenerator {
    /// Radius of the circle in normalised image space. 0.15 means
    /// 15% of the smaller image dimension, which produces a clearly
    /// visible mask on typical viewer resolutions.
    radius_norm: f32,
}

impl StubMaskGenerator {
    /// New stub with the default radius (15% of min dimension).
    pub fn new() -> Self {
        Self { radius_norm: 0.15 }
    }

    /// New stub with a custom radius — handy for tests pinning a
    /// specific mask shape.
    pub fn with_radius(radius_norm: f32) -> Self {
        Self {
            radius_norm: radius_norm.clamp(0.01, 0.95),
        }
    }
}

impl Default for StubMaskGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable id, used for registration in the registry.
pub const STUB_GENERATOR_ID: &str = "stub";

impl MaskGenerator for StubMaskGenerator {
    fn id(&self) -> &str {
        STUB_GENERATOR_ID
    }

    fn display_name(&self) -> &str {
        "Stub (placeholder)"
    }

    fn capabilities(&self) -> MaskCapabilities {
        // The stub honours every prompt mode — it just always
        // returns the same circle. This keeps it useful as a UI
        // exerciser regardless of what affordance the user clicks.
        MaskCapabilities::POINT_PROMPT
            | MaskCapabilities::BOX_PROMPT
            | MaskCapabilities::SCRIBBLE
            | MaskCapabilities::AUTO_DETECT
    }

    fn preferred_device(&self) -> DevicePreference {
        DevicePreference::Cpu
    }

    fn generate(&mut self, req: MaskRequest<'_>) -> Result<MaskResponse, MaskError> {
        let started = std::time::Instant::now();
        let w = req.frame.width;
        let h = req.frame.height;
        if w == 0 || h == 0 {
            return Err(MaskError::ShapeMismatch {
                expected_w: 1,
                expected_h: 1,
                actual_w: w,
                actual_h: h,
            });
        }

        // Pick the center of the circle. Priority:
        //   1. First positive click prompt
        //   2. Box prompt center
        //   3. First scribble point
        //   4. Image center (auto-detect mode)
        let (cx_norm, cy_norm) = if let Some(click) =
            req.prompts.clicks.iter().find(|c| c.positive)
        {
            (click.x, click.y)
        } else if let Some(b) = req.prompts.boxes.first() {
            ((b.x1 + b.x2) * 0.5, (b.y1 + b.y2) * 0.5)
        } else if let Some(s) = req.prompts.scribbles.first() {
            s.points
                .first()
                .copied()
                .unwrap_or((0.5, 0.5))
        } else {
            (0.5, 0.5)
        };

        let cx_norm = cx_norm.clamp(0.0, 1.0);
        let cy_norm = cy_norm.clamp(0.0, 1.0);
        let cx = (cx_norm * w as f32) as i32;
        let cy = (cy_norm * h as f32) as i32;

        let min_dim = w.min(h) as f32;
        let radius_px = (self.radius_norm * min_dim).max(2.0);
        let r2 = (radius_px * radius_px) as i32;

        let mut alpha = vec![0u8; (w as usize) * (h as usize)];
        let mut covered_px: u64 = 0;
        for y in 0..h {
            for x in 0..w {
                let dx = x as i32 - cx;
                let dy = y as i32 - cy;
                if dx * dx + dy * dy <= r2 {
                    alpha[(y as usize) * (w as usize) + (x as usize)] = 255;
                    covered_px += 1;
                }
            }
        }

        // Confidence is the fraction of the mask compared to the
        // 'expected' fully-circled region — typically lands near 1.0
        // when the circle fits in-frame, drops when clipped at edges.
        let total_px = (w as u64) * (h as u64);
        let expected_circle_px = (std::f32::consts::PI * radius_px * radius_px) as u64;
        let confidence = if expected_circle_px == 0 {
            0.0
        } else {
            (covered_px as f32 / expected_circle_px.max(1) as f32).clamp(0.0, 1.0)
        };

        let bbox_x = (cx as f32 - radius_px) / w as f32;
        let bbox_y = (cy as f32 - radius_px) / h as f32;
        let bbox_w = (radius_px * 2.0) / w as f32;
        let bbox_h = (radius_px * 2.0) / h as f32;

        let _ = total_px; // currently unused; kept for future "% coverage" diagnostics

        Ok(MaskResponse {
            candidates: vec![MaskCandidate {
                mask: MaskBuffer {
                    width: w,
                    height: h,
                    alpha,
                },
                confidence,
                bbox: Some(BBox {
                    x: bbox_x.max(0.0),
                    y: bbox_y.max(0.0),
                    w: bbox_w,
                    h: bbox_h,
                }),
                label: None,
            }],
            inference_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask_generator::{ClickPrompt, InputFrame, MaskPrompts, MaskTunables};

    fn dummy_frame(w: u32, h: u32) -> Vec<f32> {
        vec![0.0; (w as usize) * (h as usize) * 4]
    }

    fn req_with_click<'a>(frame: &'a [f32], w: u32, h: u32, x: f32, y: f32) -> MaskRequest<'a> {
        MaskRequest {
            frame: InputFrame {
                width: w,
                height: h,
                linear_rgba: frame,
            },
            prompts: MaskPrompts {
                clicks: vec![ClickPrompt {
                    x,
                    y,
                    positive: true,
                }],
                ..Default::default()
            },
            temporal: None,
            tunables: MaskTunables::default(),
        }
    }

    #[test]
    fn click_prompt_centers_mask_around_click() {
        let f = dummy_frame(100, 100);
        let mut gen = StubMaskGenerator::with_radius(0.1);
        let resp = gen
            .generate(req_with_click(&f, 100, 100, 0.5, 0.5))
            .expect("ok");
        assert_eq!(resp.candidates.len(), 1);
        let mask = &resp.candidates[0].mask;
        assert_eq!(mask.width, 100);
        assert_eq!(mask.height, 100);
        // Pixel at center should be lit.
        assert_eq!(mask.at(50, 50), 255);
        // A pixel far from center should be dark.
        assert_eq!(mask.at(0, 0), 0);
    }

    #[test]
    fn auto_detect_centers_at_frame_center() {
        // No prompts at all → auto-detect mode → mask at center.
        let f = dummy_frame(40, 40);
        let mut gen = StubMaskGenerator::with_radius(0.1);
        let resp = gen
            .generate(MaskRequest {
                frame: InputFrame {
                    width: 40,
                    height: 40,
                    linear_rgba: &f,
                },
                prompts: MaskPrompts::default(),
                temporal: None,
                tunables: MaskTunables::default(),
            })
            .expect("auto-detect should succeed");
        let mask = &resp.candidates[0].mask;
        // Center pixel should be lit (within rounding tolerance).
        assert!(mask.at(20, 20) == 255 || mask.at(19, 19) == 255 || mask.at(20, 19) == 255);
    }

    #[test]
    fn click_offcenter_shifts_mask() {
        let f = dummy_frame(200, 200);
        let mut gen = StubMaskGenerator::with_radius(0.05);
        // Click in the upper-left quadrant.
        let resp = gen
            .generate(req_with_click(&f, 200, 200, 0.25, 0.25))
            .expect("ok");
        let mask = &resp.candidates[0].mask;
        // Pixel near (50, 50) should be lit.
        assert_eq!(mask.at(50, 50), 255);
        // Pixel near (150, 150) should be dark.
        assert_eq!(mask.at(150, 150), 0);
    }

    #[test]
    fn zero_size_frame_errors_cleanly() {
        let f: Vec<f32> = vec![];
        let mut gen = StubMaskGenerator::new();
        let err = gen
            .generate(MaskRequest {
                frame: InputFrame {
                    width: 0,
                    height: 0,
                    linear_rgba: &f,
                },
                prompts: MaskPrompts::default(),
                temporal: None,
                tunables: MaskTunables::default(),
            })
            .expect_err("zero-size frame should error");
        assert!(matches!(err, MaskError::ShapeMismatch { .. }));
    }

    #[test]
    fn capabilities_advertise_all_prompt_types() {
        // The stub claims to support every prompt — that's
        // intentional, so UI affordances all get exercised against
        // it during dev. If a future change narrows the stub's
        // capabilities, the UI tests may stop covering branches.
        let cap = StubMaskGenerator::new().capabilities();
        assert!(cap.contains(MaskCapabilities::POINT_PROMPT));
        assert!(cap.contains(MaskCapabilities::BOX_PROMPT));
        assert!(cap.contains(MaskCapabilities::SCRIBBLE));
        assert!(cap.contains(MaskCapabilities::AUTO_DETECT));
    }

    #[test]
    fn confidence_is_in_unit_range() {
        let f = dummy_frame(100, 100);
        let mut gen = StubMaskGenerator::with_radius(0.1);
        let resp = gen
            .generate(req_with_click(&f, 100, 100, 0.5, 0.5))
            .expect("ok");
        let c = resp.candidates[0].confidence;
        assert!((0.0..=1.0).contains(&c), "confidence should be in [0,1], got {c}");
    }

    #[test]
    fn bbox_returned_and_normalised() {
        let f = dummy_frame(100, 100);
        let mut gen = StubMaskGenerator::with_radius(0.1);
        let resp = gen
            .generate(req_with_click(&f, 100, 100, 0.5, 0.5))
            .expect("ok");
        let bbox = resp.candidates[0].bbox.expect("stub returns bbox");
        // Mask centered at (50,50) with radius 10 → bbox covering ~40-60.
        assert!((0.30..=0.45).contains(&bbox.x));
        assert!((0.30..=0.45).contains(&bbox.y));
        assert!(bbox.w > 0.15 && bbox.w < 0.25);
    }

    #[test]
    fn stub_id_matches_constant() {
        let s = StubMaskGenerator::new();
        assert_eq!(s.id(), STUB_GENERATOR_ID);
    }
}
