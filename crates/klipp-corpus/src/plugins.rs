//! Native fixtures built only from the repository's C test sources.
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

const FLAGS: &[&str] = &[
    "-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-shared", "-fPIC",
];

pub fn generate(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let fixtures = [
        (
            "crates/klipp-frei0r/tests/fixtures/frei0r/fixture.c",
            "plugins/colour.so",
            Some("-DKLIPP_KIND=0"),
        ),
        (
            "crates/klipp-plugin/tests/fixtures/ofx/fixture.c",
            "plugins/ofx/KlippFixture.ofx.bundle/Contents/Linux-x86-64/KlippFixture.ofx",
            None,
        ),
    ];
    let mut hashes = BTreeMap::new();
    for (source, output, define) in fixtures {
        let source_path = crate::repository().join(source);
        let bytes = std::fs::read(&source_path).map_err(|e| format!("{source}: {e}"))?;
        hashes.insert(source.into(), crate::sha256(&bytes));
        let output = root.join(output);
        std::fs::create_dir_all(output.parent().ok_or("fixture has no parent")?)
            .map_err(|e| e.to_string())?;
        let cc = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
        let mut command = Command::new(cc);
        command.args(FLAGS);
        if let Some(define) = define {
            command.arg(define);
        }
        let made = command
            .arg(&source_path)
            .arg("-o")
            .arg(&output)
            .output()
            .map_err(|e| format!("compile {source}: {e}; a C compiler is required"))?;
        if !made.status.success() {
            return Err(format!(
                "compile {source}: {}",
                String::from_utf8_lossy(&made.stderr)
            ));
        }
    }
    Ok(hashes)
}
