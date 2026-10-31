//! Loads a compiled native bundle, never the built-in raw test plugin.
//! The suite's fixtures are built for Linux x86-64 only; elsewhere the
//! runner reports the platform unsupported, so the test is not run.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn native_ofx_pixels_and_negative_controls() {
    let runner = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/plugin-tests.py");
    let output = std::process::Command::new("python3")
        .arg(runner)
        .args(["--host", env!("CARGO_BIN_EXE_klipp-plugin-test-host"), "--self-test"])
        .output()
        .expect("python3 is required for native plugin tests");
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
