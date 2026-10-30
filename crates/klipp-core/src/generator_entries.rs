//! The generators the Library's Generators tab offers (#2887), as Klipp's
//! file `assets/presets/generators/builtin.toml` holds them, then a
//! person's own from their `generators` presets folder (#3095), in the
//! same form.

use crate::GeneratorData;

/// One generator the tab offers: its name, its group, whether its picture
/// moves over the clip, and the clip it adds.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratorEntry {
    pub name: String,
    pub category: String,
    /// Its picture advances over the clip: the tab's "Animated only"
    /// filter and the card's badge.
    #[serde(default)]
    pub animated: bool,
    pub data: GeneratorData,
}

/// Why entries someone wrote can't be used: a name twice, one empty, or
/// a still frame, which is made from a frame of footage, not offered.
fn fault(entries: &[GeneratorEntry]) -> Option<String> {
    entries.iter().enumerate().find_map(|(i, e)| {
        if e.name.trim().is_empty() || e.category.trim().is_empty() {
            Some(format!("entry {}: a name and a category each say something", i + 1))
        } else if entries[..i].iter().any(|o| o.name == e.name) {
            Some(format!("\"{}\" twice", e.name))
        } else if matches!(e.data, GeneratorData::StillFrame { .. }) {
            Some(format!("\"{}\": a still frame is made from footage, not offered", e.name))
        } else {
            None
        }
    })
}

fn read(text: &str) -> Result<Vec<GeneratorEntry>, String> {
    crate::preset_file::read::<GeneratorEntry>(text, "generator_entry", 1, "generator").and_then(|entries| fault(&entries).map_or(Ok(entries), Err))
}

/// The presets folder a person's own generators are read from.
pub const USER_FOLDER: &str = "generators";

/// The longest text a person's generator carries, in characters: a
/// placeholder's label, a slate's field.
const MAX_TEXT_CHARS: usize = 256;

/// Why a generator someone wrote can't be offered, beyond what
/// [`crate::preset_file::merge`] asks of every entry: no group, a still
/// frame, a number that isn't one, or text past [`MAX_TEXT_CHARS`].
fn user_fault(e: &GeneratorEntry) -> Result<(), String> {
    fn walk(v: &serde_json::Value) -> Result<(), String> {
        match v {
            serde_json::Value::Number(n) if n.as_f64().is_none_or(|f| !f.is_finite()) => Err("a number that isn't one".into()),
            serde_json::Value::String(t) if t.chars().count() > MAX_TEXT_CHARS => Err(format!("text of {} characters, past {MAX_TEXT_CHARS}", t.chars().count())),
            serde_json::Value::Array(a) => a.iter().try_for_each(walk),
            serde_json::Value::Object(o) => o.values().try_for_each(walk),
            _ => Ok(()),
        }
    }
    if e.category.trim().is_empty() {
        return Err("a category says something".into());
    }
    if matches!(e.data, GeneratorData::StillFrame { .. }) {
        return Err("a still frame is made from footage, not offered".into());
    }
    // Floats past f32 or NaN don't come through TOML as numbers JSON can
    // hold: written as JSON, they are null.
    let value = serde_json::to_value(&e.data).map_err(|e| e.to_string())?;
    if serde_json::to_string(&value).is_ok_and(|t| t.contains("null")) {
        return Err("a number that isn't one".into());
    }
    walk(&value)
}

/// Klipp's generators, then those in `dirs` (a person's folder, then the
/// packs'), each read as someone else's, and what was skipped.
pub fn generator_listing_in(dirs: &[std::path::PathBuf]) -> crate::preset_file::Merged<GeneratorEntry> {
    let built_in = read(include_str!("../../../assets/presets/generators/builtin.toml")).unwrap_or_else(|why| {
        tracing::error!("the built-in generators don't read: {why}");
        Vec::new()
    });
    crate::preset_file::merge(built_in, dirs, "generator_entry", 1, "generator", |e: &GeneratorEntry| &e.name, user_fault)
}

/// Every generator the tab offers, with what was skipped, read once a
/// run: Klipp's, then the person's own. A file that doesn't read is said
/// in the log, never a crash.
pub fn generator_listing() -> &'static crate::preset_file::Merged<GeneratorEntry> {
    static LISTING: std::sync::OnceLock<crate::preset_file::Merged<GeneratorEntry>> = std::sync::OnceLock::new();
    LISTING.get_or_init(|| {
        let listing = generator_listing_in(&crate::preset_file::user_dir(USER_FOLDER).into_iter().collect::<Vec<_>>());
        for why in &listing.skipped {
            tracing::warn!("a generator of yours was skipped: {why}");
        }
        listing
    })
}

/// Every generator the tab offers, in order.
pub fn generator_entries() -> &'static [GeneratorEntry] {
    &generator_listing().entries
}

/// The generator named `name`, in any case: Klipp's or a person's own.
pub fn generator_named(name: &str) -> Option<&'static GeneratorEntry> {
    generator_entries().iter().find(|e| e.name.eq_ignore_ascii_case(name.trim()))
}

/// The tab's groups, in the order their first generator comes.
pub fn generator_categories() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for e in generator_entries() {
        if !out.contains(&e.category.as_str()) {
            out.push(&e.category);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tab's sixteen generators read from Klipp's file, in their five
    /// groups, each the clip it added when the list was code: the circle
    /// at the side count drawn as a true circle, a gradient left without
    /// grain carrying Klipp's default.
    #[test]
    fn the_generators_read_as_they_were() {
        let e = generator_entries();
        let names: Vec<&str> = e.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, [
            "Solid Colour", "Gradient", "Four-Corner", "Living Gradient", "Grain", "SMPTE 75% Bars", "SMPTE 100% Bars", "SMPTE Full Bars",
            "Countdown", "Timer Up", "Timer Down", "Slate", "Placeholder", "Polygon", "Circle", "Star",
        ]);
        assert_eq!(generator_categories(), ["Backgrounds", "Test", "Timing", "Slate", "Shape"]);
        let animated: Vec<&str> = e.iter().filter(|g| g.animated).map(|g| g.name.as_str()).collect();
        assert_eq!(animated, ["Living Gradient", "Grain", "Countdown", "Timer Up", "Timer Down"]);
        let data = |n: &str| &e.iter().find(|g| g.name == n).unwrap().data;
        assert_eq!(*data("Circle"), GeneratorData::Polygon { sides: crate::generator::CIRCLE_SIDES, color: [255; 4], outline_width_px: 0.0, fill: true });
        assert_eq!(*data("Gradient"), GeneratorData::Gradient { top: [36, 52, 92, 255], bottom: [8, 10, 18, 255], angle_deg: 0.0, four_point: None, grain: crate::DEFAULT_GENERATOR_GRAIN });
        assert_eq!(*data("SMPTE Full Bars"), GeneratorData::ColorBarsSmpte);
        assert_eq!(
            *data("Timer Down"),
            GeneratorData::Timer { mode: crate::TimerMode::CountDown, initial_seconds: 60.0, format: crate::TimerFormat::MmSs, text_color: [255; 4], background_color: [0, 0, 0, 255] }
        );
        let GeneratorData::Gradient { four_point: Some(c), grain, .. } = data("Four-Corner") else { panic!("four-corner: a four-point gradient") };
        assert_eq!((c.top_left, c.bottom_right, *grain), ([255, 140, 60, 255], [20, 30, 90, 255], crate::DEFAULT_GENERATOR_GRAIN));
    }

    /// An entry that can't be offered is refused, saying why, and the
    /// file with it: a still frame (a path on disk), a name twice, an
    /// empty name, a field Klipp doesn't know, a generator it hasn't got.
    #[test]
    fn an_entry_that_cant_be_offered_is_refused() {
        let file = |entries: &str| format!("kind = \"generator_entry\"\nformat_version = 1\n{entries}");
        let entry = |name: &str, data: &str| format!("[[generator]]\nname = \"{name}\"\ncategory = \"Test\"\ndata = {data}\n");
        assert_eq!(read(&file(&entry("Bars", "\"ColorBars\""))).unwrap().len(), 1);
        let still = entry("Still", "{ StillFrame = { png_path = \"/etc/passwd\", width = 1, height = 1 } }");
        assert!(read(&file(&still)).unwrap_err().contains("made from footage"));
        assert!(read(&file(&(entry("Bars", "\"ColorBars\"") + &entry("Bars", "\"Countdown\"")))).unwrap_err().contains("\"Bars\" twice"));
        assert!(read(&file(&entry(" ", "\"ColorBars\""))).unwrap_err().contains("a name and a category"));
        assert!(read(&file(&(entry("Bars", "\"ColorBars\"") + "price = 2\n"))).unwrap_err().contains("unknown field"));
        assert!(read(&file(&entry("Plasma", "\"Plasma\""))).is_err());
    }

    /// A file dropped in a person's generators folder lists after Klipp's
    /// sixteen (#3095); a hostile one is refused, said, and Klipp's stay
    /// whole: a link, one past the size, an unknown field, the wrong
    /// kind, a built-in's name, a still frame, text past its length.
    #[test]
    fn a_person_s_generator_lists_after_klipp_s_and_a_hostile_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("klipp-user-generators-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |n: &str, body: &str| std::fs::write(dir.join(n), format!("kind = \"generator_entry\"\nformat_version = 1\n{body}")).unwrap();
        let entry = |name: &str, data: &str| format!("[[generator]]\nname = \"{name}\"\ncategory = \"Mine\"\ndata = {data}\n");
        file("a.toml", &entry("Brand Red", "{ SolidColor = { color = [200, 20, 30, 255] } }"));
        file("b.toml", &(entry("Odd", "\"ColorBars\"") + "price = 2\n"));
        std::fs::write(dir.join("c.toml"), "kind = \"effect_preset\"\nformat_version = 1\n").unwrap();
        file("d.toml", &format!("# {}\n", "x".repeat(crate::preset_file::MAX_FILE_BYTES)));
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("a.toml"), dir.join("e.toml")).unwrap();
        file("f.toml", &(entry("gradient", "\"ColorBars\"") + &entry("Still", "{ StillFrame = { png_path = \"/etc/passwd\", width = 1, height = 1 } }") + &entry("Card", &format!("{{ Placeholder = {{ label = \"{}\", color = [0, 0, 0, 255] }} }}", "x".repeat(300)))));
        let listing = generator_listing_in(std::slice::from_ref(&dir));
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 17, "{names:?}");
        assert_eq!(names[..16], generator_entries()[..16].iter().map(|e| e.name.as_str()).collect::<Vec<_>>()[..], "Klipp's, whole and first");
        assert_eq!(names[16], "Brand Red");
        assert_eq!(listing.built_in, 16);
        let said = |what: &str| assert!(listing.skipped.iter().any(|s| s.contains(what)), "{what}: {:?}", listing.skipped);
        said("b.toml: generator_entry entry 1: unknown field");
        said("c.toml: a effect_preset file, not a generator_entry file");
        said("d.toml: ");
        #[cfg(unix)]
        said("e.toml: not a plain file");
        said("\"gradient\" is the name of one of Klipp's own");
        said("\"Still\": a still frame");
        said("\"Card\": text of 300 characters");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
