//! #3305: every example in the user guide's "Making your own presets"
//! page is a file Klipp reads as a person's own: each adds its entries
//! and none is skipped.

#[test]
fn the_guides_preset_examples_read() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/reference/preset-examples");
    for kind in klipp_core::preset_kinds::PRESET_KINDS {
        let file = examples.join(format!("{}.toml", kind.folder));
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: no example: {e}", file.display()));
        let dir = std::env::temp_dir().join(format!("klipp-preset-example-{}-{}", std::process::id(), kind.folder));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("example.toml"), &text).unwrap();
        let dirs = [dir.clone()];
        let entries = text.matches(&format!("[[{}]]", kind.table)).count();
        assert_eq!(klipp_core::preset_kinds::skipped_in(kind.folder, &dirs), Some(Vec::new()), "{}", kind.folder);
        assert_eq!(klipp_core::preset_kinds::added_in(kind.folder, &dirs), entries, "{}: each entry added", kind.folder);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
