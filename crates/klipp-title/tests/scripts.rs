//! Superscript and subscript runs (#1721): set in the face's superior
//! and inferior figures where it has them, else smaller and raised or
//! lowered; either way the run sits high or low and small.
use klipp_timeline::{Script, StyleRun, TextBox, TitleData};
use klipp_title::TitleRenderer;

const W: u32 = 1920;
const H: u32 = 1080;

fn title(font: &str, script: Option<Script>) -> TitleData {
    let mut tb = TextBox { text: "x2".into(), font_name: font.into(), font_size: 200.0, ..Default::default() };
    if let Some(script) = script {
        tb.style_runs.push(StyleRun { script: Some(script), ..StyleRun::default_for_range(1, 2) });
    }
    TitleData::single_text(tb)
}

/// The rows (top, bottom) of the ink of the frame's last letter: the
/// rightmost run of columns with ink.
fn last_letter(px: &[u8]) -> (usize, usize) {
    let w = W as usize;
    let inked = |x: usize| (0..H as usize).any(|y| px[(y * w + x) * 4 + 3] > 60);
    let right = (0..w).rev().find(|&x| inked(x)).expect("ink");
    let left = (0..right).rev().find(|&x| !inked(x)).map_or(0, |x| x + 1);
    let rows: Vec<usize> = (0..H as usize).filter(|&y| (left..=right).any(|x| px[(y * w + x) * 4 + 3] > 60)).collect();
    (rows[0], *rows.last().unwrap())
}

fn check(font: &str) {
    let mut r = TitleRenderer::new();
    let plain = last_letter(&r.render(&title(font, None), W, H));
    let sup = last_letter(&r.render(&title(font, Some(Script::Superscript)), W, H));
    let sub = last_letter(&r.render(&title(font, Some(Script::Subscript)), W, H));
    let height = |b: (usize, usize)| (b.1 - b.0) as f32;
    assert!(height(sup) < height(plain) * 0.8, "{font}: a superscript is small: {sup:?} vs {plain:?}");
    assert!(height(sub) < height(plain) * 0.8, "{font}: a subscript is small: {sub:?} vs {plain:?}");
    assert!(sup.1 + 20 < plain.1, "{font}: a superscript sits above the baseline: {sup:?} vs {plain:?}");
    assert!(sub.1 > sup.1 + 40, "{font}: a subscript sits lower than a superscript: {sub:?} vs {sup:?}");
    assert!(sup.0 <= plain.0 + 10, "{font}: a superscript reaches the top of the type: {sup:?} vs {plain:?}");
}

#[test]
fn a_face_with_superior_figures_sets_them() {
    check("Inter");
}

#[test]
fn a_face_without_them_sets_the_run_smaller_raised_and_lowered() {
    // A family the registry cannot find has no features to read: the
    // run is set at 62% of its size, raised or lowered.
    check("No Such Family");
}

#[test]
fn a_script_round_trips_and_none_is_unwritten() {
    let td = title("Inter", Some(Script::Subscript));
    let back: TitleData = serde_json::from_str(&serde_json::to_string(&td).unwrap()).unwrap();
    assert_eq!(back, td);
    let plain = StyleRun::default_for_range(0, 1);
    assert!(!serde_json::to_string(&plain).unwrap().contains("script"));
}

/// A run that is only a superscript is a style of its own: not one that
/// inherits everything, and not the same as a plain run beside it.
#[test]
fn a_script_is_a_style_of_its_own() {
    let plain = StyleRun::default_for_range(0, 1);
    let sup = StyleRun { script: Some(Script::Superscript), ..StyleRun::default_for_range(1, 2) };
    assert!(plain.is_all_inherited() && !sup.is_all_inherited());
    assert!(!plain.same_style(&sup));
}
