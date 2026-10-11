//! The default test suite runs the same generated-project checks as the CLI.
#[test]
fn generated_saved_projects_keep_their_inputs_documents_and_pixels() {
    let root = std::env::temp_dir().join(format!("klipp-checkpoint-{}", uuid::Uuid::new_v4()));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_klipp-corpus"))
        .arg("all")
        .arg("--out")
        .arg(&root)
        .output()
        .expect("run the corpus checker");
    assert!(
        output.status.success(),
        "saved-project checkpoint failed; artifacts: {}\n{}\n{}",
        root.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(root).unwrap();
}
