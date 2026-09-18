//! Bake the user's audio-search keys into the binary at build time, so a
//! release build carries them and end users search with no setup — while
//! the public source stays key-free. The keys are read from the local
//! `~/.config/klipp/keys.toml` (never committed); a machine without that
//! file bakes nothing, and the runtime falls back to a user-supplied key.

use std::path::PathBuf;

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("klipp").join("keys.toml"))
}

fn main() {
    // A build-time env var wins, for CI or an explicit release build.
    for env in ["KLIPP_JAMENDO_CLIENT_ID_BAKED", "KLIPP_FREESOUND_TOKEN_BAKED"] {
        println!("cargo:rerun-if-env-changed={env}");
    }
    let Some(path) = config_path() else { return };
    println!("cargo:rerun-if-changed={}", path.display());
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    for line in text.lines() {
        let line = line.trim();
        for (key, env) in [
            ("jamendo_client_id", "KLIPP_JAMENDO_CLIENT_ID_BAKED"),
            ("freesound_token", "KLIPP_FREESOUND_TOKEN_BAKED"),
        ] {
            if let Some(rest) = line.strip_prefix(key) {
                if let Some(val) = rest.trim().strip_prefix('=') {
                    let val = val.trim().trim_matches('"').trim();
                    // Skip if a build-time env already set it.
                    if !val.is_empty() && std::env::var(env).is_err() {
                        println!("cargo:rustc-env={env}={val}");
                    }
                }
            }
        }
    }
}
