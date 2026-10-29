//! The generators the Library's Generators tab offers (#2887), as Klipp's
//! file `assets/presets/generators/builtin.toml` holds them.

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

/// Every generator the tab offers, in order, read once; a file that
/// doesn't read leaves none, said, never a crash.
pub fn generator_entries() -> &'static [GeneratorEntry] {
    static ENTRIES: std::sync::OnceLock<Vec<GeneratorEntry>> = std::sync::OnceLock::new();
    ENTRIES.get_or_init(|| {
        read(include_str!("../../../assets/presets/generators/builtin.toml")).unwrap_or_else(|why| {
            tracing::error!("the built-in generators don't read: {why}");
            Vec::new()
        })
    })
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
}
