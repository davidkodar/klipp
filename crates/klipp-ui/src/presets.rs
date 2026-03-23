use klipp_timeline::ClipColorGrade;
use std::path::{Path, PathBuf};

/// Get the user presets directory, creating it if needed.
pub fn user_presets_dir() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("klipp").join("presets").join("color");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).ok()?;
    }
    Some(dir)
}

/// Load all presets: built-in + user directory.
pub fn load_all_presets() -> Vec<(String, ClipColorGrade)> {
    let mut presets = builtin_presets();
    presets.extend(load_user_presets());
    presets
}

/// Save a grade preset to the user directory.
pub fn save_preset(name: &str, grade: &ClipColorGrade) -> Result<(), String> {
    let dir = user_presets_dir().ok_or("Cannot find config directory")?;
    let safe_name: String = name.chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = dir.join(format!("{safe_name}.json"));
    let json = serde_json::to_string_pretty(grade)
        .map_err(|e| format!("Serialize error: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Write error: {e}"))?;
    Ok(())
}

/// Load presets from user directory.
fn load_user_presets() -> Vec<(String, ClipColorGrade)> {
    let Some(dir) = user_presets_dir() else { return Vec::new() };
    let mut presets = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return presets };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Some(preset) = load_preset_file(&path) {
                presets.push(preset);
            }
        }
    }
    presets.sort_by(|a, b| a.0.cmp(&b.0));
    presets
}

fn load_preset_file(path: &Path) -> Option<(String, ClipColorGrade)> {
    let text = std::fs::read_to_string(path).ok()?;
    let grade: ClipColorGrade = serde_json::from_str(&text).ok()?;
    let name = path.file_stem()?.to_string_lossy().to_string();
    Some((name, grade))
}

/// Built-in presets.
fn builtin_presets() -> Vec<(String, ClipColorGrade)> {
    vec![
        ("Teal & Orange".into(), teal_orange()),
        ("Film Look".into(), film_look()),
        ("High Contrast B&W".into(), high_contrast_bw()),
        ("Bleach Bypass".into(), bleach_bypass()),
        ("Cool Shadows".into(), cool_shadows()),
    ]
}

fn teal_orange() -> ClipColorGrade {
    ClipColorGrade {
        enabled: true,
        temperature: 15.0,
        tint: -5.0,
        contrast: 0.15,
        saturation: 1.25,
        lift_x: -0.08, lift_y: 0.04, // shadows → teal
        gain_x: 0.06, gain_y: -0.03, // highlights → warm orange
        ..Default::default()
    }
}

fn film_look() -> ClipColorGrade {
    ClipColorGrade {
        enabled: true,
        contrast: 0.10,
        saturation: 0.85,
        blacks: 0.08,       // lifted blacks (milky shadows)
        highlights: -0.10,  // rolled-off highlights
        temperature: 5.0,
        ..Default::default()
    }
}

fn high_contrast_bw() -> ClipColorGrade {
    ClipColorGrade {
        enabled: true,
        saturation: 0.0, // fully desaturated
        contrast: 0.35,
        blacks: -0.05,
        whites: 0.05,
        ..Default::default()
    }
}

fn bleach_bypass() -> ClipColorGrade {
    ClipColorGrade {
        enabled: true,
        saturation: 0.60,
        contrast: 0.25,
        highlights: -0.15,
        blacks: 0.05,
        ..Default::default()
    }
}

fn cool_shadows() -> ClipColorGrade {
    ClipColorGrade {
        enabled: true,
        lift_x: -0.10, lift_y: 0.06, // shadows → blue
        gain_x: 0.03, gain_y: -0.02, // highlights → warm
        contrast: 0.08,
        saturation: 1.10,
        ..Default::default()
    }
}
