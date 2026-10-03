//! Which field of a JSON request a parse error is in (#783). serde names
//! a column in the line, and an agent would have to count characters in
//! its own JSON to find it; this reads the line, records where each value
//! sits, and gives back the field path at that column with what is
//! there, so the reply can say "brief.arc must be a string (got a list)".

/// Where one value sits in the line: bytes `start..end`, its path, and
/// what kind of value it is.
struct Span {
    start: usize,
    end: usize,
    path: String,
    kind: &'static str,
}

struct Scan<'a> {
    b: &'a [u8],
    i: usize,
    spans: Vec<Span>,
}

impl Scan<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let start = self.i;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    let s = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
                    self.i += 1;
                    return Some(s);
                }
                _ => self.i += 1,
            }
        }
        None
    }

    fn value(&mut self, path: &str) -> Option<()> {
        self.ws();
        let start = self.i;
        let kind = match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        break;
                    }
                    let key = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return None;
                    }
                    self.i += 1;
                    let inner = if path.is_empty() { key } else { format!("{path}.{key}") };
                    self.value(&inner)?;
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                "an object"
            }
            b'[' => {
                self.i += 1;
                let mut n = 0;
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') {
                        self.i += 1;
                        break;
                    }
                    self.value(&format!("{path}[{n}]"))?;
                    n += 1;
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                "a list"
            }
            b'"' => {
                self.string()?;
                "text"
            }
            b't' | b'f' => {
                while self.i < self.b.len() && self.b[self.i].is_ascii_alphabetic() {
                    self.i += 1;
                }
                "true or false"
            }
            b'n' => {
                self.i += 4;
                "null"
            }
            _ => {
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                if self.i == start {
                    return None;
                }
                "a number"
            }
        };
        self.spans.push(Span { start, end: self.i, path: path.to_string(), kind });
        Some(())
    }
}

/// The field path and kind of the innermost value at 1-based `column` of
/// `line`, a single-line JSON request. `None` when the line is not JSON
/// or the column falls on the request itself rather than a field.
pub(crate) fn field_at(line: &str, column: usize) -> Option<(String, &'static str)> {
    let mut scan = Scan { b: line.as_bytes(), i: 0, spans: Vec::new() };
    scan.value("")?;
    // serde names the last character it took: the value it stopped in,
    // or, for a value it only looked at (a list where text was wanted),
    // the `:`, `,` or `[` just before it.
    let at = column.saturating_sub(1);
    let b = line.as_bytes();
    if matches!(b.get(at), Some(b':' | b',' | b'[')) {
        let next = (at + 1..b.len()).find(|&i| !b[i].is_ascii_whitespace());
        if let Some(s) = next.and_then(|n| scan.spans.iter().find(|s| s.start == n && !s.path.is_empty())) {
            return Some((s.path.clone(), s.kind));
        }
    }
    scan.spans
        .into_iter()
        .filter(|s| !s.path.is_empty() && s.start <= at && at < s.end)
        .min_by_key(|s| s.end - s.start)
        .map(|s| (s.path, s.kind))
}

/// serde's "invalid type: X, expected Y" in words: what was there.
fn got(what: &str) -> String {
    match what {
        "sequence" => "a list".into(),
        "map" => "an object".into(),
        "null" | "unit value" => "null".into(),
        w if w.starts_with("boolean") => w.trim_start_matches("boolean ").trim_matches('`').into(),
        w if w.starts_with("integer") || w.starts_with("floating point") => format!("the number {}", w.split('`').nth(1).unwrap_or("")),
        w if w.starts_with("string") => format!("the text {}", w.trim_start_matches("string ")),
        w => w.into(),
    }
}

/// A request's parse error said by the field it is in: "brief.arc must
/// be a string (got a list)" for a wrong type, "brief: missing field
/// `title`" otherwise. `None` when the field cannot be found.
pub(crate) fn explain(line: &str, e: &serde_json::Error) -> Option<String> {
    let (path, _) = field_at(line, e.column())?;
    let full = e.to_string();
    let message = full.split(" at line ").next().unwrap_or(&full);
    if let Some(rest) = message.strip_prefix("invalid type: ") {
        if let Some((have, want)) = rest.split_once(", expected ") {
            return Some(format!("{path} must be {want} (got {})", got(have)));
        }
    }
    Some(format!("{path}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, Debug)]
    #[allow(dead_code)]
    struct Brief {
        title: String,
        arc: String,
    }

    #[derive(serde::Deserialize, Debug)]
    #[allow(dead_code)]
    struct Request {
        method: String,
        brief: Brief,
    }

    fn explained(line: &str) -> String {
        let e = serde_json::from_str::<Request>(line).unwrap_err();
        explain(line, &e).unwrap_or_else(|| format!("unexplained: {e}"))
    }

    #[test]
    fn a_wrong_type_names_its_field() {
        let line = r#"{"method":"set_brief","brief":{"title":"Two Shores","arc":["open","build","close"]}}"#;
        assert_eq!(explained(line), "brief.arc must be a string (got a list)");
        let line = r#"{"method":"set_brief","brief":{"title":7,"arc":"x"}}"#;
        assert_eq!(explained(line), "brief.title must be a string (got the number 7)");
    }

    #[test]
    fn a_missing_field_names_where() {
        let line = r#"{"method":"set_brief","brief":{"title":"T"}}"#;
        let said = explained(line);
        assert!(said.starts_with("brief: missing field `arc`"), "{said}");
    }

    #[test]
    fn list_items_are_named_by_index() {
        let line = r#"{"a":[1,{"b":[true,"x"]}]}"#;
        let at = line.find("\"x\"").unwrap() + 1;
        assert_eq!(field_at(line, at), Some(("a[1].b[1]".into(), "text")));
    }

    #[test]
    fn not_json_is_not_explained() {
        assert_eq!(field_at("{\"a\": ", 4), None);
    }
}
