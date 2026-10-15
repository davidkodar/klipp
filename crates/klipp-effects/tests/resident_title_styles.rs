use klipp_effects::gpu::{GpuEffectsPipeline, TitleStyle};
use klipp_timeline::{
    InnerShadow, LayerFx, TextBox, TextGlow, TextOutline, TitleData, TitleShadow,
};
use klipp_title::TitleRenderer;

#[test]
fn resident_styles_match_authored_title_composition_without_downloads() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert!(
            std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"),
            "GPU required"
        );
        return;
    };
    let (w, h) = (640, 360);
    let mut renderer = TitleRenderer::new();
    for radius in [0.0, 8.0, 24.0] {
        let mut title = TitleData::single_text(TextBox {
            text: "Light & shade".into(),
            font_size: 120.0,
            color: [80, 140, 210, 255],
            ..Default::default()
        });
        let fx = LayerFx {
            glows: vec![TextGlow {
                enabled: true,
                radius,
                color: [240, 100, 20, 210],
                intensity: 1.4,
                radius_keyframes: vec![],
                intensity_keyframes: vec![],
            }],
            shadows: vec![TitleShadow {
                enabled: true,
                offset_x: -9.0,
                offset_y: 12.0,
                blur: radius,
                color: [10, 20, 40, 190],
                offset_x_keyframes: vec![],
                offset_y_keyframes: vec![],
                blur_keyframes: vec![],
            }],
            outlines: vec![TextOutline {
                enabled: true,
                width: 4.0,
                color: [240, 245, 250, 220],
                width_keyframes: vec![],
            }],
            inner_shadows: vec![InnerShadow {
                enabled: true,
                offset_x: 6.0,
                offset_y: -3.0,
                blur: radius,
                color: [20, 10, 30, 150],
                offset_x_keyframes: vec![],
                offset_y_keyframes: vec![],
                blur_keyframes: vec![],
            }],
            ..Default::default()
        };
        title.objects[0].fx = Some(fx);
        let layout = renderer.render_layout(&title, w, h);
        let reference = TitleRenderer::compose(&layout, &title, w, h, None, Some(0.0));
        let mut plain = title.clone();
        plain.objects[0].fx = Some(LayerFx::default());
        let face = TitleRenderer::compose(&layout, &plain, w, h, None, Some(0.0));
        let scale = layout.scale;
        let styles = [
            TitleStyle::Glow {
                radius: radius * scale,
                color: [240, 100, 20, 210],
                intensity: 1.4,
            },
            TitleStyle::Shadow {
                offset: [(-9.0 * scale).round() as i32, (12.0 * scale).round() as i32],
                radius: radius * scale,
                color: [10, 20, 40, 190],
            },
            TitleStyle::Outline {
                radius: 4.0 * scale,
                color: [240, 245, 250, 220],
            },
            TitleStyle::Face,
            TitleStyle::InnerShadow {
                offset: [(6.0 * scale).round() as i32, (-3.0 * scale).round() as i32],
                radius: radius * scale,
                color: [20, 10, 30, 150],
            },
        ];
        let matrix = if radius == 8.0 {
            [[0.7, 0.2, 0.1], [0.1, 0.8, 0.1], [0.05, 0.15, 0.8]]
        } else {
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        };
        gpu.set_authored_to_working(matrix);
        assert!(gpu.stack_begin(w, h, 1));
        assert!(gpu.stack_layer_from_srgb_u8(0, &face));
        let before = gpu.readback_stats();
        assert!(gpu.stack_title_styles(
            0,
            [layout.mask_x0, layout.mask_y0, layout.mask_w, layout.mask_h],
            &styles
        ));
        assert_eq!(before, gpu.readback_stats());
        assert_eq!(
            gpu.title_style_scratch_bytes(),
            u64::from(layout.mask_w) * u64::from(layout.mask_h) * 28
        );
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let got = gpu.stack_download_linear();
        let mut expected = klipp_effects::srgb_u8_to_linear_f32_rgba(&reference);
        for p in expected.chunks_exact_mut(4) {
            let rgb = [p[0], p[1], p[2]];
            for i in 0..3 {
                p[i] = matrix[i].iter().zip(rgb).map(|(a, b)| a * b).sum();
            }
        }
        let worst = got
            .chunks_exact(4)
            .zip(expected.chunks_exact(4))
            .flat_map(|(a, b)| {
                (0..4).map(move |c| {
                    if c == 3 {
                        (a[c] - b[c]).abs()
                    } else {
                        (a[c] * a[3] - b[c] * b[3]).abs()
                    }
                })
            })
            .fold(0.0f32, f32::max)
            * 255.0;
        assert!(
            worst <= 1.1,
            "radius {radius}: premultiplied linear error {worst} levels"
        );
    }
}
