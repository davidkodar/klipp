//! False colour palettes as files write them (#2887, #3095): Klipp's
//! own, `assets/presets/false_colour/builtin.toml`, then a person's own
//! and the installed packs', each read as someone else's. Here, below the
//! app, so a pack's palettes are checked as the viewer would read them.

use serde::Deserialize;

/// A palette as a file writes it: its name and its stops.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteEntry {
    pub name: String,
    pub stop: Vec<StopEntry>,
}

/// A stop as a file writes it: its 8-bit luma band, both ends included,
/// its colour and what it means.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopEntry {
    pub luma: (u8, u8),
    pub color: [u8; 3],
    pub label: String,
}

/// The presets folder a person's own false colour palettes are read from.
pub const PALETTES_FOLDER: &str = "false_colour";

/// The longest label a stop takes, in characters.
const MAX_STOP_LABEL_CHARS: usize = 64;

/// Why a palette can't be offered: no stops or more than 256, or a stop
/// whose band runs backwards, has no label or one past
/// [`MAX_STOP_LABEL_CHARS`].
fn palette_fault(e: &PaletteEntry) -> Result<(), String> {
    if e.stop.is_empty() || e.stop.len() > 256 {
        return Err("1 to 256 stops".into());
    }
    match e.stop.iter().find(|s| s.luma.0 > s.luma.1 || s.label.trim().is_empty() || s.label.chars().count() > MAX_STOP_LABEL_CHARS) {
        Some(s) => Err(format!("the stop \"{}\" runs from low luma to high and says what it is, in up to {MAX_STOP_LABEL_CHARS} characters", s.label)),
        None => Ok(()),
    }
}

/// The palettes of a false colour palettes file; refused, saying why, for
/// a palette with no name, one named twice, or one with no stops or
/// more than 256, or a stop whose band runs backwards or whose label is
/// empty or past 64 characters.
pub fn read_palette_entries(text: &str) -> Result<Vec<PaletteEntry>, String> {
    let entries: Vec<PaletteEntry> = crate::preset_file::read(text, "false_colour_palette", 1, "palette")?;
    for (i, e) in entries.iter().enumerate() {
        if e.name.trim().is_empty() || entries[..i].iter().any(|p| p.name == e.name) {
            return Err(format!("palette {}: a name, said once", i + 1));
        }
        palette_fault(e).map_err(|why| format!("\"{}\": {why}", e.name))?;
    }
    Ok(entries)
}

/// Klipp's palettes, then those in `dirs` (a person's folder, then the
/// packs'), each read as someone else's (#3095), and what was skipped.
pub fn palette_listing_in(dirs: &[std::path::PathBuf]) -> crate::preset_file::Merged<PaletteEntry> {
    let built_in = read_palette_entries(include_str!("../../../assets/presets/false_colour/builtin.toml")).unwrap_or_else(|why| {
        tracing::error!("the built-in false colour palettes don't read: {why}");
        Vec::new()
    });
    crate::preset_file::merge(built_in, dirs, "false_colour_palette", 1, "palette", |e: &PaletteEntry| &e.name, palette_fault)
}
