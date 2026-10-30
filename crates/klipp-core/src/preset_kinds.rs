//! The preset kinds a person keeps and a pack carries (#3095), each by
//! its folder: what its files say they are, the table its entries are
//! under and the field that names one, and what of a folder's would be
//! skipped, read as that kind reads it.

/// One kind of preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetKind {
    /// Its folder, under a person's presets and a pack's `presets/`.
    pub folder: &'static str,
    /// What its files say they are, `kind`.
    pub kind: &'static str,
    /// The newest `format_version` read.
    pub version: u32,
    /// The table its entries are a list under.
    pub table: &'static str,
    /// The field that names an entry.
    pub key: &'static str,
}

/// Every kind, by folder.
pub const PRESET_KINDS: [PresetKind; 7] = [
    PresetKind { folder: crate::generator_entries::USER_FOLDER, kind: "generator_entry", version: 1, table: "generator", key: "name" },
    PresetKind { folder: crate::project_formats::USER_FOLDER, kind: "project_formats", version: 1, table: "format", key: "name" },
    PresetKind { folder: crate::color_grade::ZONE_PRESETS_FOLDER, kind: "hdr_zones_preset", version: 1, table: "preset", key: "name" },
    PresetKind { folder: crate::export_preset::DELIVERY_TEMPLATES_FOLDER, kind: "delivery_template", version: 1, table: "template", key: "name" },
    PresetKind { folder: crate::false_colour::PALETTES_FOLDER, kind: "false_colour_palette", version: 1, table: "palette", key: "name" },
    PresetKind { folder: crate::title_motion::ANIMATOR_PRESETS_FOLDER, kind: "animator_preset", version: 1, table: "preset", key: "id" },
    PresetKind { folder: crate::audio_effect_registry::AUDIO_PRESETS_FOLDER, kind: "audio_effect_preset", version: 1, table: "preset", key: "name" },
];

/// The kind kept in `folder`.
pub fn kind_in(folder: &str) -> Option<&'static PresetKind> {
    PRESET_KINDS.iter().find(|k| k.folder == folder)
}

/// What of the files in `dirs` would be skipped, read after Klipp's as
/// the kind in `folder` reads them, said by file or name; `None` for a
/// folder no kind is kept in.
pub fn skipped_in(folder: &str, dirs: &[std::path::PathBuf]) -> Option<Vec<String>> {
    Some(match kind_in(folder)?.folder {
        f if f == crate::generator_entries::USER_FOLDER => crate::generator_entries::generator_listing_in(dirs).skipped,
        f if f == crate::project_formats::USER_FOLDER => crate::project_formats::skipped_in(dirs),
        f if f == crate::color_grade::ZONE_PRESETS_FOLDER => crate::color_grade::zone_preset_listing_in(dirs).skipped,
        f if f == crate::export_preset::DELIVERY_TEMPLATES_FOLDER => crate::export_preset::delivery_template_listing_in(dirs).skipped,
        f if f == crate::false_colour::PALETTES_FOLDER => crate::false_colour::palette_listing_in(dirs).skipped,
        f if f == crate::title_motion::ANIMATOR_PRESETS_FOLDER => crate::title_motion::animator_preset_listing_in(dirs).skipped,
        _ => crate::audio_effect_registry::audio_preset_listing_in(dirs).skipped,
    })
}

/// How many entries of the kind in `folder` the files in `dirs` add
/// after Klipp's, as the kind reads them.
pub fn added_in(folder: &str, dirs: &[std::path::PathBuf]) -> usize {
    let count = |n: usize, built_in: usize| n - built_in;
    match folder {
        f if f == crate::generator_entries::USER_FOLDER => { let l = crate::generator_entries::generator_listing_in(dirs); count(l.entries.len(), l.built_in) }
        f if f == crate::color_grade::ZONE_PRESETS_FOLDER => { let l = crate::color_grade::zone_preset_listing_in(dirs); count(l.entries.len(), l.built_in) }
        f if f == crate::export_preset::DELIVERY_TEMPLATES_FOLDER => { let l = crate::export_preset::delivery_template_listing_in(dirs); count(l.entries.len(), l.built_in) }
        f if f == crate::false_colour::PALETTES_FOLDER => { let l = crate::false_colour::palette_listing_in(dirs); count(l.entries.len(), l.built_in) }
        f if f == crate::title_motion::ANIMATOR_PRESETS_FOLDER => { let l = crate::title_motion::animator_preset_listing_in(dirs); count(l.entries.len(), l.built_in) }
        f if f == crate::audio_effect_registry::AUDIO_PRESETS_FOLDER => { let l = crate::audio_effect_registry::audio_preset_listing_in(dirs); count(l.entries.len(), l.built_in) }
        f if f == crate::project_formats::USER_FOLDER => crate::project_formats::added_in(dirs),
        _ => 0,
    }
}
