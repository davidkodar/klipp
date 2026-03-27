use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A discovered font family with its file path and style info.
#[derive(Debug, Clone)]
pub struct FontInfo {
    pub family: String,
    pub style: String, // "Regular", "Bold", "Italic", "Bold Italic", etc.
    pub path: PathBuf,
}

/// Scan system font directories and return discovered fonts grouped by family.
pub fn discover_system_fonts() -> BTreeMap<String, Vec<FontInfo>> {
    let mut fonts: BTreeMap<String, Vec<FontInfo>> = BTreeMap::new();

    let home = std::env::var("HOME").unwrap_or_default();
    let font_dirs = [
        "/usr/share/fonts".to_string(),
        "/usr/local/share/fonts".to_string(),
        format!("{home}/.local/share/fonts"),
    ];

    for dir in &font_dirs {
        scan_directory(Path::new(dir), &mut fonts);
    }

    fonts
}

fn scan_directory(dir: &Path, fonts: &mut BTreeMap<String, Vec<FontInfo>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_directory(&path, fonts);
        } else if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            if ext == "ttf" || ext == "otf" {
                if let Some(info) = parse_font_info(&path) {
                    fonts.entry(info.family.clone()).or_default().push(info);
                }
            }
        }
    }
}

fn parse_font_info(path: &Path) -> Option<FontInfo> {
    // Validate this is actually a loadable font by trying to parse it.
    // We read the file, but only to validate — we don't keep the Font object.
    let data = std::fs::read(path).ok()?;
    let _font =
        fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default()).ok()?;

    let filename = path.file_stem()?.to_string_lossy().to_string();
    let (family, style) = parse_font_name(&filename);

    Some(FontInfo {
        family,
        style,
        path: path.to_path_buf(),
    })
}

fn parse_font_name(filename: &str) -> (String, String) {
    // Try splitting on '-' first (e.g., "NotoSans-Regular", "Roboto-BoldItalic")
    if let Some(idx) = filename.rfind('-') {
        let family_raw = filename[..idx].replace('_', " ");
        let style = &filename[idx + 1..];
        let family = insert_spaces_before_caps(&family_raw);
        return (family, normalize_style(style));
    }

    // Try common style suffixes (longest first to avoid partial matches)
    for suffix in [
        "BoldItalic",
        "Bold Italic",
        "SemiBoldItalic",
        "ExtraBoldItalic",
        "ExtraBold",
        "SemiBold",
        "Bold",
        "Italic",
        "Light",
        "Medium",
        "Thin",
        "Black",
        "Regular",
    ] {
        if let Some(prefix) = filename.strip_suffix(suffix) {
            let family = prefix
                .trim_end_matches([' ', '_', '-'])
                .to_string();
            let family = insert_spaces_before_caps(&family);
            if !family.is_empty() {
                return (family, normalize_style(suffix));
            }
        }
    }

    (insert_spaces_before_caps(filename), "Regular".to_string())
}

fn insert_spaces_before_caps(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() {
            result.push(' ');
        }
        result.push(c);
    }
    result
}

fn normalize_style(s: &str) -> String {
    match s.to_lowercase().as_str() {
        "regular" | "reg" => "Regular".into(),
        "bold" | "bd" => "Bold".into(),
        "italic" | "it" => "Italic".into(),
        "bolditalic" | "bi" => "Bold Italic".into(),
        "light" | "lt" => "Light".into(),
        "medium" | "med" => "Medium".into(),
        "semibold" | "sb" => "Semi Bold".into(),
        "extrabold" => "Extra Bold".into(),
        "extrabolditalic" => "Extra Bold Italic".into(),
        "semibolditalic" => "Semi Bold Italic".into(),
        "thin" => "Thin".into(),
        "black" | "heavy" => "Black".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hyphenated_name() {
        let (family, style) = parse_font_name("NotoSans-Regular");
        assert_eq!(family, "Noto Sans");
        assert_eq!(style, "Regular");
    }

    #[test]
    fn parse_hyphenated_bold() {
        let (family, style) = parse_font_name("Roboto-BoldItalic");
        assert_eq!(family, "Roboto");
        assert_eq!(style, "Bold Italic");
    }

    #[test]
    fn parse_plain_name() {
        let (family, style) = parse_font_name("DejaVuSans");
        assert_eq!(family, "Deja Vu Sans");
        assert_eq!(style, "Regular");
    }

    #[test]
    fn discover_returns_something() {
        // This test just verifies discovery doesn't panic. On CI without fonts it may be empty.
        let fonts = discover_system_fonts();
        // On a typical Linux desktop there should be at least a few fonts
        // but we don't assert count since CI may have none
        let _ = fonts;
    }
}
