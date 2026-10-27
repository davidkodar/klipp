//! A title drawn while its catalogue face was being fetched is drawn again
//! once the fetch lands (#2734), not kept as the fallback drew it. The
//! fetch here finds its files already in place, so it reads nothing from
//! the network; it is a test binary of its own because a fetch is counted
//! for the whole process.

use klipp_title::{catalogue, TitleRenderer};

#[test]
fn a_fetched_face_draws_its_titles_again() {
    let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Arrives later".into(), font_size: 80.0, ..Default::default() });
    if let Some(tb) = td.primary_text_mut() {
        tb.font_name = "Oswald".into();
    }
    let mut r = TitleRenderer::new();
    let draw = |r: &mut TitleRenderer| {
        let _ = r.render_with_window(&td, 320, 180, None, 2.0, Some(5.0));
        r.resting_renders()
    };
    let made = draw(&mut r);
    assert_eq!(draw(&mut r), made, "drawn once, then taken from the cache");

    // A fetch of a family whose files are all there already.
    let root = std::env::temp_dir().join(format!("klipp-fetched-face-{}", std::process::id()));
    let family = catalogue::family("Oswald").expect("Oswald is in the catalogue");
    let dir = root.join(family.name.to_lowercase().replace(' ', "-"));
    std::fs::create_dir_all(&dir).unwrap();
    let licence = family.licence_url.rsplit('/').next().unwrap().to_string();
    for (name, size) in family.files.iter().map(|f| (f.name.clone(), f.size)).chain([(licence, family.licence_size)]) {
        std::fs::write(dir.join(name), vec![0u8; size as usize]).unwrap();
    }
    let before = catalogue::fetches();
    catalogue::fetch_in(&root, family, &std::sync::atomic::AtomicBool::new(false)).expect("the files are there");
    assert_eq!(catalogue::fetches(), before + 1, "the fetch is counted");
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(draw(&mut r), made + 1, "drawn again once a face arrived");
}
