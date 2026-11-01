//! #3305: the user guide's brand kit example is a `kit.toml` Klipp reads.

#[test]
fn the_guides_brand_kit_reads() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/reference/template-examples/kit.toml");
    let kit = klipp_project::brand_kit::BrandKit::read(&std::fs::read_to_string(&path).unwrap()).unwrap_or_else(|why| panic!("{why}"));
    assert_eq!((kit.name.as_str(), kit.colours.accent.as_deref(), kit.fonts.heading.as_deref()), ("Harbour Studio", Some("#f2a541"), Some("Work Sans")));
}
