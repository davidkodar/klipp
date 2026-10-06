//! The app never contains today's v1 reader and writer (#1089): klipp-project's
//! `v1-oracle` feature is enabled only through klipp-convert's
//! dev-dependency, which a normal build of the app doesn't see (resolver 2).
//! This holds it: the app's normal dependency graph must not list it.

#[test]
fn the_app_build_has_no_v1_oracle() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args(["tree", "--offline", "-e", "features,normal", "-p", "klipp-app", "-i", "klipp-project"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree runs");
    assert!(out.status.success(), "cargo tree failed: {}", String::from_utf8_lossy(&out.stderr));
    let tree = String::from_utf8_lossy(&out.stdout);
    assert!(tree.contains("klipp-project"), "the tree names klipp-project: {tree}");
    assert!(!tree.contains("v1-oracle"), "the app's build enables klipp-project/v1-oracle:\n{tree}");
}
