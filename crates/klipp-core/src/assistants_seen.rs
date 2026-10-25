//! When each AI assistant last talked to Klipp (#2649): `klipp-mcp`
//! notes the name an assistant gives when it starts a session, and the
//! time; the assistant setup shows it, so a person can see that the one
//! they connected reached Klipp. Kept in
//! `$XDG_STATE_HOME/klipp/assistants-seen.json` (else
//! `~/.local/state/klipp/`), a map of name to Unix seconds.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Where the record is kept.
pub fn path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("klipp").join("assistants-seen.json"))
}

/// Every assistant seen, by the name it gave, with when (Unix seconds).
pub fn read() -> BTreeMap<String, u64> {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse(&t)).unwrap_or_default()
}

/// The record's text read; anything unreadable is no record.
pub fn parse(text: &str) -> BTreeMap<String, u64> {
    serde_json::from_str(text).unwrap_or_default()
}

/// Note that the assistant named `name` started a session now. A failure
/// to write is ignored: the record is a convenience, never in the way.
pub fn record(name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    let Some(path) = path() else { return };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut seen = std::fs::read_to_string(&path).map(|t| parse(&t)).unwrap_or_default();
    seen.insert(name.to_string(), now);
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Written beside and moved into place, so a reader never sees half.
    let tmp = dir.join(format!(".assistants-seen.{}.tmp", std::process::id()));
    if let Ok(text) = serde_json::to_string_pretty(&seen) {
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_record_reads_back_and_bad_text_is_none() {
        let seen = super::parse(r#"{"claude-code": 1700000000, "Zed": 1700000100}"#);
        assert_eq!(seen.get("Zed"), Some(&1_700_000_100));
        assert!(super::parse("not json").is_empty());
    }
}
