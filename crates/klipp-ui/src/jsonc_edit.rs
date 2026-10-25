//! Klipp's entry in an assistant's JSON settings, set or removed without
//! touching anything else in the file (#2649). The settings are the
//! person's: their other servers, their other settings, the order they
//! keep them in, their comments (several assistants read JSON with `//`
//! and `/* */` comments, and trailing commas) all stay as they were. The
//! file is edited as text, at the one member that changes, rather than
//! parsed and written out again.

use serde_json::Value;

/// `text` read as JSON, its comments and trailing commas allowed.
pub fn parse(text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(&plain(text)).map_err(|e| format!("the settings are not JSON: {e}"))
}

/// `text` without its comments and trailing commas: plain JSON, every
/// other character where it was.
fn plain(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let end = string_end(b, i);
                out.push_str(&text[i..end]);
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            }
            b',' => {
                // A comma before a closing bracket is dropped.
                let next = skip_blank(b, i + 1);
                if !matches!(b.get(next), Some(b'}') | Some(b']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                let ch = text[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// The index after the string starting at `start` (a `"`).
fn string_end(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// The index of the next character that is not white space or a comment.
fn skip_blank(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b.get(i) == Some(&b'/') && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b.get(i) == Some(&b'/') && b.get(i + 1) == Some(&b'*') {
            i = std::str::from_utf8(&b[i + 2..]).ok().and_then(|s| s.find("*/")).map_or(b.len(), |e| i + 2 + e + 2);
        } else {
            return i;
        }
    }
}

/// The index after the value starting at `i`.
fn value_end(b: &[u8], i: usize) -> usize {
    match b.get(i) {
        Some(b'"') => string_end(b, i),
        Some(b'{') | Some(b'[') => {
            let mut depth = 0;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => {
                        j = string_end(b, j);
                        continue;
                    }
                    b'/' if matches!(b.get(j + 1), Some(b'/') | Some(b'*')) => {
                        j = skip_blank(b, j);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return j + 1;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            b.len()
        }
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace() {
                j += 1;
            }
            j
        }
    }
}

/// A member of an object, by where its parts are in the text.
struct Member {
    key: String,
    /// Where its key's quote starts.
    start: usize,
    value_start: usize,
    value_end: usize,
}

/// The members of the object whose `{` is at `open`, and the index of its
/// `}`.
fn members(text: &str, open: usize) -> Result<(Vec<Member>, usize), String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = skip_blank(b, open + 1);
    loop {
        match b.get(i) {
            Some(b'}') => return Ok((out, i)),
            Some(b'"') => {
                let key_end = string_end(b, i);
                let key: String = serde_json::from_str(&text[i..key_end]).map_err(|_| "a key is not a string".to_string())?;
                let colon = skip_blank(b, key_end);
                if b.get(colon) != Some(&b':') {
                    return Err(format!("no ':' after \"{key}\""));
                }
                let value_start = skip_blank(b, colon + 1);
                let value_end = value_end(b, value_start);
                out.push(Member { key, start: i, value_start, value_end });
                i = skip_blank(b, value_end);
                if b.get(i) == Some(&b',') {
                    i = skip_blank(b, i + 1);
                }
            }
            _ => return Err("the settings are not a JSON object".into()),
        }
    }
}

/// The indentation of the line `at` is on.
fn indent_at(text: &str, at: usize) -> &str {
    let line = text[..at].rfind('\n').map_or(0, |n| n + 1);
    let rest = &text[line..];
    &rest[..rest.len() - rest.trim_start_matches([' ', '\t']).len()]
}

/// `value` pretty-printed, every line after the first indented by `indent`.
fn pretty(value: &Value, indent: &str) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default().replace('\n', &format!("\n{indent}"))
}

/// `text` with `root[section][name]` set to `value`: replaced in place
/// when it is there, else added first in `section`, which is made when
/// there is none. Nothing else in `text` changes.
pub fn set(text: &str, section: &str, name: &str, value: &Value) -> Result<String, String> {
    parse(text)?;
    let text = if text.trim().is_empty() { "{}\n" } else { text };
    let b = text.as_bytes();
    let root = skip_blank(b, 0);
    if b.get(root) != Some(&b'{') {
        return Err("the settings are not a JSON object".into());
    }
    let (top, close) = members(text, root)?;
    let step = detect_step(text, &top);
    let Some(sec) = top.iter().find(|m| m.key == section) else {
        // A new section, first in the file.
        let inner = indent_at(text, top.first().map_or(close, |m| m.start)).to_string();
        let inner = if top.is_empty() { step.clone() } else { inner };
        let entry = Value::Object([(name.to_string(), value.clone())].into_iter().collect());
        let member = format!("{}: {}", serde_json::to_string(section).unwrap_or_default(), pretty(&entry, &inner));
        let sep = if top.is_empty() { "\n".to_string() } else { ",".to_string() };
        let after_brace = root + 1;
        let lead = if top.is_empty() { "\n" } else { "" };
        let insert = format!("{lead}\n{inner}{member}{sep}");
        let insert = if top.is_empty() { format!("\n{inner}{member}\n") } else { insert };
        return Ok(format!("{}{}{}", &text[..after_brace], insert, &text[after_brace..]));
    };
    if b.get(sec.value_start) != Some(&b'{') {
        return Err(format!("\"{section}\" in the settings is not an object"));
    }
    let (servers, sclose) = members(text, sec.value_start)?;
    if let Some(m) = servers.iter().find(|m| m.key == name) {
        let indent = indent_at(text, m.start).to_string();
        return Ok(format!("{}{}{}", &text[..m.value_start], pretty(value, &indent), &text[m.value_end..]));
    }
    let outer = indent_at(text, sec.start).to_string();
    let inner = servers.first().map(|m| indent_at(text, m.start).to_string()).unwrap_or_else(|| format!("{outer}{step}"));
    let member = format!("{}: {}", serde_json::to_string(name).unwrap_or_default(), pretty(value, &inner));
    let at = sec.value_start + 1;
    let insert = if servers.is_empty() {
        // An empty section, `{}` or one with only white space or comments.
        let rest = &text[at..sclose];
        if rest.trim().is_empty() {
            return Ok(format!("{}\n{inner}{member}\n{outer}{}", &text[..at], &text[sclose..]));
        }
        format!("\n{inner}{member}")
    } else {
        format!("\n{inner}{member},")
    };
    Ok(format!("{}{}{}", &text[..at], insert, &text[at..]))
}

/// `text` without `root[section][name]`, and with nothing else changed.
/// `None` when it has none.
pub fn remove(text: &str, section: &str, name: &str) -> Result<Option<String>, String> {
    parse(text)?;
    let b = text.as_bytes();
    let root = skip_blank(b, 0);
    if b.get(root) != Some(&b'{') {
        return Ok(None);
    }
    let (top, _) = members(text, root)?;
    let Some(sec) = top.iter().find(|m| m.key == section && b.get(m.value_start) == Some(&b'{')) else { return Ok(None) };
    let (servers, _) = members(text, sec.value_start)?;
    let Some(i) = servers.iter().position(|m| m.key == name) else { return Ok(None) };
    let m = &servers[i];
    // From the start of its line (or the member before's end) to the next
    // member's start, or to its own end and the comma before it when it
    // is the last.
    let (from, to) = match servers.get(i + 1) {
        Some(next) => (line_start_or(text, m.start), line_start_or(text, next.start)),
        None => {
            let from = if i > 0 { servers[i - 1].value_end } else { line_start_or(text, m.start) };
            let mut to = m.value_end;
            let after = skip_blank(b, to);
            if b.get(after) == Some(&b',') {
                to = after + 1;
            }
            (from, to)
        }
    };
    Ok(Some(format!("{}{}", &text[..from], &text[to..])))
}

/// Where the line `at` is on starts, when only white space is before `at`
/// on it; else `at`.
fn line_start_or(text: &str, at: usize) -> usize {
    let line = text[..at].rfind('\n').map_or(0, |n| n + 1);
    if text[line..at].trim().is_empty() { line } else { at }
}

/// One step of the file's indentation: what its first member is indented
/// by, else two spaces.
fn detect_step(text: &str, top: &[Member]) -> String {
    top.first().map(|m| indent_at(text, m.start).to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "  ".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn klipp() -> Value {
        serde_json::json!({"command": "/usr/bin/klipp-mcp", "args": []})
    }

    /// Klipp's entry goes into the section, first, and everything else in
    /// the file stays as it was: the other servers, other settings, the
    /// order, the comments and a trailing comma.
    #[test]
    fn setting_keeps_everything_else() {
        let before = "{\n  // my theme\n  \"theme\": \"One Dark\",\n  \"mcpServers\": {\n    \"other\": { \"command\": \"x\" }, // keep\n  },\n  \"z\": 1,\n}\n";
        let after = set(before, "mcpServers", "klipp", &klipp()).unwrap();
        assert!(after.contains("// my theme") && after.contains("// keep") && after.contains("\"z\": 1,\n}"), "{after}");
        let v = parse(&after).unwrap();
        assert_eq!(v["mcpServers"]["klipp"]["command"], "/usr/bin/klipp-mcp");
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["theme"], "One Dark");
        assert!(after.find("\"theme\"").unwrap() < after.find("\"mcpServers\"").unwrap(), "the order kept");
    }

    /// Set again, the entry is replaced in place, not added twice.
    #[test]
    fn setting_again_replaces_the_entry() {
        let once = set("{\n  \"mcpServers\": {}\n}\n", "mcpServers", "klipp", &klipp()).unwrap();
        let moved = serde_json::json!({"command": "/opt/klipp/klipp-mcp", "args": []});
        let twice = set(&once, "mcpServers", "klipp", &moved).unwrap();
        assert_eq!(twice.matches("\"klipp\"").count(), 1, "{twice}");
        assert_eq!(parse(&twice).unwrap()["mcpServers"]["klipp"]["command"], "/opt/klipp/klipp-mcp");
    }

    /// No section, no file: one is made.
    #[test]
    fn a_missing_section_or_file_is_made() {
        for before in ["", "{}", "{\n  \"theme\": \"x\"\n}\n"] {
            let after = set(before, "context_servers", "klipp", &klipp()).unwrap();
            let v = parse(&after).unwrap();
            assert_eq!(v["context_servers"]["klipp"]["command"], "/usr/bin/klipp-mcp", "{before:?} -> {after}");
            if before.contains("theme") {
                assert_eq!(v["theme"], "x");
            }
        }
    }

    /// Removing takes Klipp's entry out and leaves the rest; with none,
    /// there is nothing to write.
    #[test]
    fn removing_takes_only_klipps_entry() {
        let with = "{\n  \"mcpServers\": {\n    \"a\": {\"command\": \"a\"},\n    \"klipp\": {\"command\": \"k\"},\n    \"b\": {\"command\": \"b\"}\n  }\n}\n";
        let without = remove(with, "mcpServers", "klipp").unwrap().unwrap();
        let v = parse(&without).unwrap();
        assert!(v["mcpServers"].get("klipp").is_none());
        assert_eq!(v["mcpServers"]["a"]["command"], "a");
        assert_eq!(v["mcpServers"]["b"]["command"], "b");
        for last in ["{\"mcpServers\": {\"a\": {}, \"klipp\": {}}}", "{\"mcpServers\": {\"klipp\": {}}}"] {
            let out = remove(last, "mcpServers", "klipp").unwrap().unwrap();
            assert!(parse(&out).unwrap()["mcpServers"].get("klipp").is_none(), "{out}");
        }
        assert_eq!(remove("{\"mcpServers\": {\"a\": {}}}", "mcpServers", "klipp").unwrap(), None);
    }

    /// Settings that are not JSON are not written to.
    #[test]
    fn broken_settings_are_left_alone() {
        assert!(set("{ \"a\": ", "mcpServers", "klipp", &klipp()).is_err());
        assert!(set("[1, 2]", "mcpServers", "klipp", &klipp()).is_err());
    }
}
