//! Record the commit the binary is built from, for
//! `klipp_core::build_info`: the running app reports it, so which build is
//! running is never a guess.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    // No optional locks: `git status` would otherwise refresh and rewrite
    // the index, and a build that writes what it watches never settles.
    let out = Command::new("git").env("GIT_OPTIONAL_LOCKS", "0").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn watch(path: &str) {
    // A path that does not exist would make every build rerun this script.
    if Path::new(path).exists() {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn main() {
    let commit = git(&["rev-parse", "--short=8", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    // The build number: how many commits lead up to the one built from,
    // so a later build of main always has a larger number.
    let number = git(&["rev-list", "--count", "HEAD"]).unwrap_or_else(|| "0".to_string());
    println!("cargo:rustc-env=KLIPP_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=KLIPP_BUILD_NUMBER={number}");
    println!("cargo:rustc-env=KLIPP_GIT_DIRTY={dirty}");
    println!("cargo:rustc-env=KLIPP_BUILD_PROFILE={}", std::env::var("PROFILE").unwrap_or_default());
    // Again when HEAD moves, the branch advances, or a source changes.
    for name in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            watch(&path);
        }
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &branch]) {
            watch(&path);
        }
    }
    watch("../../crates");
    watch("../../Cargo.lock");
    watch("../../assets/craft");
}
