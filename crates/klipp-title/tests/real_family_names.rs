//! A title naming a font by its family, as the font's name table gives
//! it ("DejaVu Sans"), draws in that font; so does one naming it as
//! Klipp's list used to, from its file name ("Deja Vu Sans").

use klipp_timeline::{TextBox, TitleData};

fn draw(r: &mut klipp_title::TitleRenderer, font: &str) -> Vec<u8> {
    let td = TitleData::single_text(TextBox { text: "Grumpy wizards 0123".into(), font_name: font.into(), font_size: 72.0, color: [255, 255, 255, 255], ..Default::default() });
    r.render(&td, 960, 240)
}

#[test]
fn a_title_naming_a_fonts_own_family_draws_in_it() {
    // B612 Mono saved under a file name that is not its family.
    let dir = std::env::temp_dir().join(format!("klipp-real-family-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/B612Mono/B612Mono-Regular.ttf"), dir.join("Bsixtwelve-Regular.ttf")).unwrap();
    let fonts = klipp_title::font_discovery::scan_folder(&dir);
    let mut r = klipp_title::TitleRenderer::with_font_registry(std::sync::Arc::new(fonts));
    let fallback = draw(&mut r, "Nothing Like It");
    let real = draw(&mut r, "B612 Mono");
    let old = draw(&mut r, "Bsixtwelve");
    let differs = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x != y).count();
    assert!(differs(&real, &fallback) > 1000, "\"B612 Mono\" draws in the font, not the fallback: {} bytes differ", differs(&real, &fallback));
    assert_eq!(differs(&real, &old), 0, "the old name from its file draws the same face");
    let _ = std::fs::remove_dir_all(&dir);
}
