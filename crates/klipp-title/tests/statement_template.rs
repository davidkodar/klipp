//! #2609: the Statement template, a bold opener, fills its frame as one:
//! its two lines span more than a third of the frame's width at 1080p and
//! at 2160p, inside title safe, and its accent bar lies under the text
//! without touching it. At 130 px its text spanned a quarter of the width,
//! top-left: "two short lines".

/// The text's and the bar's extents as fractions of the frame: the white
/// ink, then the blue: (x0, x1, y0, y1).
fn extents(w: u32, h: u32) -> ([f32; 4], [f32; 4]) {
    let t = klipp_timeline::title_library::TITLES.iter().find(|t| t.name == "Statement").expect("the Statement template");
    let data = klipp_timeline::title_library::for_frame(t.build(), w, h);
    let px = klipp_title::TitleRenderer::new().render(&data, w, h);
    let mut text = [1.0f32, 0.0, 1.0, 0.0];
    let mut bar = [1.0f32, 0.0, 1.0, 0.0];
    let grow = |e: &mut [f32; 4], x: u32, y: u32| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        *e = [e[0].min(fx), e[1].max(fx), e[2].min(fy), e[3].max(fy)];
    };
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, b, a) = (px[i], px[i + 1], px[i + 2], px[i + 3]);
            if a > 128 && r > 200 && g > 200 && b > 200 {
                grow(&mut text, x, y);
            } else if a > 128 && b > 200 && r < 120 {
                grow(&mut bar, x, y);
            }
        }
    }
    (text, bar)
}

#[test]
fn the_statement_card_fills_its_frame() {
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let (text, bar) = extents(w, h);
        eprintln!("{w}x{h}: text {text:?}, bar {bar:?}");
        assert!(text[1] - text[0] > 1.0 / 3.0, "{w}x{h}: the text spans {:.3} of the width", text[1] - text[0]);
        assert!(text[0] >= 0.05 && text[1] <= 0.95 && text[2] >= 0.05 && text[3] <= 0.95, "{w}x{h}: inside title safe: {text:?}");
        assert!(bar[2] > text[3], "{w}x{h}: the bar under the text, not on it: bar from {:.3}, text to {:.3}", bar[2], text[3]);
        assert!(bar[2] - text[3] < 0.06, "{w}x{h}: and close under it: a gap of {:.3}", bar[2] - text[3]);
    }
}
