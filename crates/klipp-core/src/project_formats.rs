//! The project formats the New Project dialog, the welcome screen and
//! Sequence Settings offer (#1864, #2887), as Klipp's file
//! `assets/presets/project_formats/builtin.toml` holds them, with the
//! dialogs' picture sizes and frame rates, then a person's own formats
//! from their `project_formats` presets folder (#3095). Here, below the
//! app, so the control socket's `new_project` takes a format by name.

/// A frame rate, as numerator and denominator.
pub type Rate = (u32, u32);

/// A project format: what it is for, its picture, and the rates it is
/// made at, the first the usual one. Shared by the New Project dialog and
/// the welcome screen.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Format {
    pub name: String,
    /// 0×0 takes the picture from the first clip.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// Empty takes the rate from the first clip.
    #[serde(default)]
    pub rates: Vec<Rate>,
    /// Graded in a wide-gamut space for HLG or PQ delivery.
    #[serde(default)]
    pub hdr: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Size {
    name: String,
    width: u32,
    height: u32,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameRate {
    name: String,
    rate: Rate,
}

/// The dialogs' lists: the formats, the picture sizes and the frame
/// rates, each as offered (#2887).
#[derive(PartialEq)]
struct Presets {
    formats: Vec<Format>,
    sizes: Vec<(String, u32, u32)>,
    rates: Vec<(String, u32, u32)>,
    /// How many of `formats` are Klipp's own.
    built_in: usize,
    /// What was skipped of a person's own.
    skipped: Vec<String>,
}

/// The largest picture side a format or a size may give.
const MAX_SIDE: u32 = 16384;

/// Why a project formats file can't be used: an entry without a name or
/// with one twice in its list, a picture with no size or past
/// [`MAX_SIDE`], a format with no rate, or a rate of nothing.
fn presets_fault(formats: &[Format], sizes: &[Size], rates: &[FrameRate]) -> Option<String> {
    let names = |list: Vec<&str>, what: &str| {
        list.iter().enumerate().find_map(|(i, n)| {
            if n.trim().is_empty() {
                Some(format!("{what} {}: a name says something", i + 1))
            } else {
                list[..i].contains(n).then(|| format!("\"{n}\" twice among the {what}s"))
            }
        })
    };
    let side = |w: u32, h: u32| (1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h);
    let rate = |(n, d): Rate| n > 0 && d > 0 && n / d <= 1000;
    names(formats.iter().map(|f| f.name.as_str()).collect(), "format")
        .or_else(|| names(sizes.iter().map(|s| s.name.as_str()).collect(), "size"))
        .or_else(|| names(rates.iter().map(|r| r.name.as_str()).collect(), "rate"))
        .or_else(|| formats.iter().find(|f| !side(f.width, f.height)).map(|f| format!("\"{}\": its picture is 1 to {MAX_SIDE} pixels a side", f.name)))
        .or_else(|| formats.iter().find(|f| f.rates.is_empty() || !f.rates.iter().all(|r| rate(*r))).map(|f| format!("\"{}\": it is made at one rate or more, none of nothing", f.name)))
        .or_else(|| sizes.iter().find(|s| !side(s.width, s.height)).map(|s| format!("\"{}\": its picture is 1 to {MAX_SIDE} pixels a side", s.name)))
        .or_else(|| rates.iter().find(|r| !rate(r.rate)).map(|r| format!("\"{}\": a rate of nothing", r.name)))
}

fn read_presets(text: &str) -> Result<Presets, String> {
    fn read<T: serde::de::DeserializeOwned>(text: &str, table: &str) -> Result<Vec<T>, String> {
        crate::preset_file::read(text, "project_formats", 1, table)
    }
    let (formats, sizes, rates): (Vec<Format>, Vec<Size>, Vec<FrameRate>) = (read(text, "format")?, read(text, "size")?, read(text, "rate")?);
    if let Some(why) = presets_fault(&formats, &sizes, &rates) {
        return Err(why);
    }
    // What the dialogs do, besides the presets: take the first clip's
    // picture and rate, or a size typed in.
    let first = Format { name: "Match first clip".into(), width: 0, height: 0, rates: Vec::new(), hdr: false };
    let automatic = ("Automatic (from first clip)".to_string(), 0, 0);
    Ok(Presets {
        built_in: formats.len() + 1,
        skipped: Vec::new(),
        formats: std::iter::once(first).chain(formats).collect(),
        sizes: std::iter::once(automatic.clone()).chain(sizes.into_iter().map(|s| (s.name, s.width, s.height))).chain(std::iter::once(("Custom...".to_string(), 0, 0))).collect(),
        rates: std::iter::once(automatic).chain(rates.into_iter().map(|r| (r.name, r.rate.0, r.rate.1))).collect(),
    })
}

/// The presets folder a person's own project formats are read from.
pub const USER_FOLDER: &str = "project_formats";

/// Why a format someone wrote can't be offered, beyond what
/// [`crate::preset_file::merge`] asks of every entry: a picture of no
/// size or past [`MAX_SIDE`], or no rate, or a rate of nothing.
fn user_fault(f: &Format) -> Result<(), String> {
    presets_fault(std::slice::from_ref(f), &[], &[]).map_or(Ok(()), Err)
}

/// Klipp's file, then the formats in `dirs` (a person's folder, then the
/// packs'), each read as someone else's (#3095), and what was skipped. A
/// person's file adds formats only: the dialogs' picture sizes and frame
/// rates are Klipp's, and a preference keeps its place among them.
fn presets_in(dirs: &[std::path::PathBuf]) -> (Presets, Vec<String>) {
    let mut presets = read_presets(include_str!("../../../assets/presets/project_formats/builtin.toml")).unwrap_or_else(|why| {
        tracing::error!("the built-in project formats don't read: {why}");
        read_presets("kind = \"project_formats\"\nformat_version = 1\n").expect("no presets read")
    });
    let built_in = std::mem::take(&mut presets.formats);
    let mut merged = crate::preset_file::merge(built_in, dirs, "project_formats", 1, "format", |f: &Format| &f.name, user_fault);
    for dir in dirs {
        for table in ["size", "rate"] {
            if !crate::preset_file::read_dir::<toml::Value>(dir, "project_formats", 1, table).0.is_empty() {
                merged.skipped.push(format!("{table} entries in {}: a person's file adds formats; the sizes and rates are Klipp's", dir.display()));
            }
        }
    }
    presets.formats = merged.entries;
    presets.built_in = merged.built_in;
    (presets, merged.skipped)
}

/// The dialogs' lists, read once a run: Klipp's, then the person's own
/// formats. A file that doesn't read is said in the log, never a crash.
fn presets() -> &'static Presets {
    static PRESETS: crate::preset_file::Reloading<Presets> = crate::preset_file::Reloading::new();
    PRESETS.get(|| {
        let (presets, skipped) = presets_in(&crate::preset_places::dirs_for(USER_FOLDER));
        for why in &skipped {
            tracing::warn!("a project format of yours was skipped: {why}");
        }
        Presets { skipped, ..presets }
    })
}

/// How many formats the files in `dirs` add after Klipp's.
pub fn added_in(dirs: &[std::path::PathBuf]) -> usize {
    let (presets, _) = presets_in(dirs);
    presets.formats.len() - presets.built_in
}

/// What of the formats in `dirs` would be skipped, read after Klipp's.
pub fn skipped_in(dirs: &[std::path::PathBuf]) -> Vec<String> {
    presets_in(dirs).1
}

/// How many of [`formats`], from the start, are Klipp's own ("Match
/// first clip" among them).
pub fn built_in_formats() -> usize {
    presets().built_in
}

/// What was skipped of a person's project formats, said by file or name.
pub fn skipped_formats() -> &'static [String] {
    &presets().skipped
}

/// The format named `name`, in any case: Klipp's or a person's own.
pub fn format_named(name: &str) -> Option<&'static Format> {
    formats().iter().find(|f| f.name.eq_ignore_ascii_case(name.trim()))
}

/// The formats, "Match first clip" first.
pub fn formats() -> &'static [Format] {
    &presets().formats
}

/// The picture sizes: "Automatic" first, "Custom..." last.
pub fn resolution_presets() -> &'static [(String, u32, u32)] {
    &presets().sizes
}

/// The frame rates, "Automatic" first.
pub fn fps_presets() -> &'static [(String, u32, u32)] {
    &presets().rates
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project formats file with an entry that can't be offered is
    /// refused, saying why: a name twice or empty, a picture of no size or
    /// past 16384, a format with no rate, a rate of nothing, an unknown
    /// field.
    #[test]
    fn a_bad_project_formats_file_is_refused() {
        let file = |body: &str| format!("kind = \"project_formats\"\nformat_version = 1\n{body}");
        assert!(read_presets(&file("[[size]]\nname = \"Tiny\"\nwidth = 64\nheight = 64\n")).is_ok());
        for (body, says) in [
            ("[[size]]\nname = \"A\"\nwidth = 64\nheight = 64\n[[size]]\nname = \"A\"\nwidth = 32\nheight = 32\n", "\"A\" twice"),
            ("[[rate]]\nname = \" \"\nrate = [25, 1]\n", "a name says something"),
            ("[[size]]\nname = \"Huge\"\nwidth = 99999\nheight = 64\n", "1 to 16384 pixels"),
            ("[[format]]\nname = \"Flat\"\nwidth = 0\nheight = 0\nrates = [[25, 1]]\n", "1 to 16384 pixels"),
            ("[[format]]\nname = \"Still\"\nwidth = 64\nheight = 64\nrates = []\n", "one rate or more"),
            ("[[rate]]\nname = \"Zero\"\nrate = [25, 0]\n", "a rate of nothing"),
            ("[[size]]\nname = \"X\"\nwidth = 64\nheight = 64\ncolour = 1\n", "unknown field"),
        ] {
            let why = read_presets(&file(body)).err().unwrap_or_default();
            assert!(why.contains(says), "{body}: {why}");
        }
    }

    /// A file dropped in a person's project formats folder lists its
    /// format after Klipp's (#3095); a hostile one is refused, said, and
    /// Klipp's stay whole: a link, one past the size, an unknown field,
    /// the wrong kind, a built-in's name, a picture past 16384, sizes and
    /// rates (Klipp's own lists).
    #[test]
    fn a_person_s_format_lists_after_klipp_s_and_a_hostile_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("klipp-user-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |n: &str, body: &str| std::fs::write(dir.join(n), format!("kind = \"project_formats\"\nformat_version = 1\n{body}")).unwrap();
        let format = |name: &str, w: u32| format!("[[format]]\nname = \"{name}\"\nwidth = {w}\nheight = 1080\nrates = [[50, 1]]\n");
        file("a.toml", &format("Studio 50p", 1920));
        file("b.toml", &(format("Odd", 1920) + "price = 2\n"));
        std::fs::write(dir.join("c.toml"), "kind = \"generator_entry\"\nformat_version = 1\n").unwrap();
        file("d.toml", &format!("# {}\n", "x".repeat(crate::preset_file::MAX_FILE_BYTES)));
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("a.toml"), dir.join("e.toml")).unwrap();
        file("f.toml", &(format("hd 1080P", 1920) + &format("Huge", 99_999)));
        file("g.toml", "[[size]]\nname = \"Mine\"\nwidth = 64\nheight = 64\n");
        let (presets, skipped) = presets_in(std::slice::from_ref(&dir));
        let names: Vec<&str> = presets.formats.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Match first clip", "HD 1080p", "UHD 4K", "DCI 4K (cinema)", "Vertical (social)", "Square (social)", "HDR UHD (HLG / PQ)", "Studio 50p"]);
        assert_eq!(presets.built_in, 7);
        assert_eq!(presets.sizes.len(), 10, "the sizes are Klipp's");
        let said = |what: &str| assert!(skipped.iter().any(|s| s.contains(what)), "{what}: {skipped:?}");
        said("b.toml: project_formats entry 1: unknown field");
        said("c.toml: a generator_entry file, not a project_formats file");
        said("d.toml: ");
        #[cfg(unix)]
        said("e.toml: not a plain file");
        said("\"hd 1080P\" is the name of one of Klipp's own");
        said("\"Huge\": its picture is 1 to 16384 pixels a side");
        said("size entries in");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
