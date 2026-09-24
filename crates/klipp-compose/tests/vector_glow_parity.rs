use klipp_title::{AlphaMaskProcessor, TitleRenderer};
struct Gpu(klipp_effects::gpu::GpuEffectsPipeline);
impl AlphaMaskProcessor for Gpu {
    fn blur(&mut self, m: &[u8], w: u32, h: u32, r: f32) -> Vec<u8> {
        self.0.blur_alpha_gpu(m, w, h, r)
    }
    fn dilate(&mut self, m: &[u8], w: u32, h: u32, r: f32) -> Vec<u8> {
        self.0.dilate_alpha_gpu(m, w, h, r)
    }
}
#[test]
fn revealed_stroke_halo_matches_cpu_and_gpu() {
    let Some(g) = klipp_effects::gpu::GpuEffectsPipeline::new() else {
        return;
    };
    let mut gpu = Gpu(g);
    let mut path = klipp_timeline::VectorPath::default();
    path.pose.fill = [0.0; 4];
    path.pose.stroke_width = 20.0;
    path.trim = Some(klipp_timeline::PathTrim {
        range: klipp_timeline::TrimRange {
            end: 0.6,
            ..Default::default()
        },
        ..Default::default()
    });
    for radius in [6.0, 42.0, 120.0] {
        path.pose.stroke_glow = Some(klipp_timeline::StrokeGlow {
            radius,
            strength: 0.8,
        });
        let mut title = klipp_timeline::TitleData::default();
        title.objects.clear();
        title.add_object(
            klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape {
                kind: klipp_timeline::ShapeKind::Path(path.clone()),
                fill: None,
                stroke: None,
                fit: None,
            }),
            klipp_timeline::ObjectTransform {
                size: klipp_timeline::TitlePosition { x: 0.8, y: 0.8 },
                ..klipp_timeline::default_text_transform()
            },
        );
        let cpu = TitleRenderer::new().render_with_time(&title, 320, 180, None, 0.0);
        let device = TitleRenderer::new().render_with_time(&title, 320, 180, Some(&mut gpu), 0.0);
        let delta = cpu
            .iter()
            .zip(&device)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(delta <= 1, "radius {radius}: {delta}");
    }
}
