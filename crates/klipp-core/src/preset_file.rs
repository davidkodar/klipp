//! Named data kept as files (#2887): built-in presets in
//! `assets/presets/<kind>/*.toml`, read here, so an entry is a data change
//! and not a release.
//!
//! A file says what it holds, `kind`, and the form it is written in,
//! `format_version`; its entries are a list under one table name
//! (`[[spec]]`). A file someone else wrote is read as untrusted: one past
//! [`MAX_FILE_BYTES`], of another kind, from a newer Klipp, with more than
//! [`MAX_ENTRIES`] entries or an entry that doesn't read is refused, said,
//! and nothing is read from it.

use serde::de::DeserializeOwned;

/// The largest preset file read, bytes.
pub const MAX_FILE_BYTES: usize = 1 << 20;

/// The most entries one file holds.
pub const MAX_ENTRIES: usize = 1024;

/// The entries under `table` of a preset file of `kind`, written in a
/// form up to `version`.
pub fn read<T: DeserializeOwned>(text: &str, kind: &str, version: u32, table: &str) -> Result<Vec<T>, String> {
    if text.len() > MAX_FILE_BYTES {
        return Err(format!("a {kind} file of {} bytes is past the {MAX_FILE_BYTES} read", text.len()));
    }
    let doc: toml::Table = text.parse().map_err(|e: toml::de::Error| format!("not a {kind} file: {}", e.message()))?;
    match doc.get("kind").and_then(toml::Value::as_str) {
        Some(k) if k == kind => {}
        Some(other) => return Err(format!("a {other} file, not a {kind} file")),
        None => return Err(format!("not a {kind} file: it says no kind")),
    }
    let written = doc.get("format_version").and_then(toml::Value::as_integer).ok_or_else(|| format!("a {kind} file says no format_version"))?;
    if written < 1 {
        return Err(format!("a {kind} file of format {written}, which is none"));
    }
    if written > i64::from(version) {
        return Err(format!("a {kind} file of format {written}, made by a newer Klipp; this one reads up to {version}"));
    }
    let entries = match doc.get(table) {
        None => return Ok(Vec::new()),
        Some(toml::Value::Array(a)) => a,
        Some(_) => return Err(format!("a {kind} file's {table} is a list of entries")),
    };
    if entries.len() > MAX_ENTRIES {
        return Err(format!("a {kind} file holds at most {MAX_ENTRIES} entries, not {}", entries.len()));
    }
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| e.clone().try_into().map_err(|err: toml::de::Error| format!("{kind} entry {}: {}", i + 1, err.message())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Entry {
        name: String,
        size: u32,
    }

    /// A preset file is read whole or not at all, saying why: another
    /// kind, a newer or missing format, a non-list, too many entries or
    /// bytes, an entry of the wrong shape, nesting past reason and text
    /// that isn't TOML are refused, never a panic.
    #[test]
    fn a_preset_file_reads_or_is_refused_saying_why() {
        let read = |t: &str| super::read::<Entry>(t, "thing", 2, "entry");
        let ok = "kind = \"thing\"\nformat_version = 2\n[[entry]]\nname = \"a\"\nsize = 1\n[[entry]]\nname = \"b\"\nsize = 2\n";
        assert_eq!(read(ok).unwrap(), [Entry { name: "a".into(), size: 1 }, Entry { name: "b".into(), size: 2 }]);
        assert_eq!(read("kind = \"thing\"\nformat_version = 1\n").unwrap(), [], "none is none");
        let refused = [
            ("kind = \"other\"\nformat_version = 1\n", "a other file"),
            ("format_version = 1\n", "says no kind"),
            ("kind = \"thing\"\n", "no format_version"),
            ("kind = \"thing\"\nformat_version = 3\n", "made by a newer Klipp"),
            ("kind = \"thing\"\nformat_version = 0\n", "which is none"),
            ("kind = \"thing\"\nformat_version = 1\nentry = 4\n", "is a list of entries"),
            ("kind = \"thing\"\nformat_version = 1\n[[entry]]\nname = \"a\"\nsize = -1\n", "thing entry 1"),
            ("kind = \"thing\"\nformat_version = 1\n[[entry]]\nname = 7\n", "thing entry 1"),
            ("kind = [[[", "not a thing file"),
        ];
        for (text, says) in refused {
            let why = read(text).unwrap_err();
            assert!(why.contains(says), "{text:?}: {why}");
        }
        let many = format!("kind = \"thing\"\nformat_version = 1\n{}", "[[entry]]\nname = \"a\"\nsize = 1\n".repeat(MAX_ENTRIES + 1));
        assert!(read(&many).unwrap_err().contains("at most"));
        let big = format!("kind = \"thing\"\nformat_version = 1\n# {}\n", "x".repeat(MAX_FILE_BYTES));
        assert!(read(&big).unwrap_err().contains("bytes"));
        let deep = format!("kind = \"thing\"\nformat_version = 1\nx = {}{}\n", "[".repeat(100_000), "]".repeat(100_000));
        assert!(read(&deep).is_err(), "nesting past reason is refused, not followed");
    }
}
