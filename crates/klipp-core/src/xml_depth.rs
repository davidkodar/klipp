//! How deep an XML file's elements nest, read before it is parsed. The XML
//! parser recurses once per level, so a file that nests deeply enough stops
//! the app before the parser can answer, on any file read from elsewhere.

/// The deepest nesting taken, well past any real timeline, caption or
/// camera file (those nest a few dozen levels).
pub const MAX_XML_DEPTH: usize = 256;

/// What a file nesting past [`MAX_XML_DEPTH`] is told.
pub fn too_deep() -> String {
    format!("its elements nest deeper than {MAX_XML_DEPTH} levels")
}

/// Whether `xml`'s elements nest no deeper than [`MAX_XML_DEPTH`]. Not a
/// check that it is well formed: where it stops making sense as XML the
/// parser refuses it at that same point, before going any deeper.
pub fn xml_depth_ok(xml: &str) -> bool {
    let b = xml.as_bytes();
    let after = |from: usize, end: &[u8]| b[from..].windows(end.len()).position(|w| w == end).map(|p| from + p + end.len());
    let (mut i, mut depth) = (0, 0usize);
    while let Some(open) = b[i..].iter().position(|&c| c == b'<').map(|p| i + p) {
        let rest = &b[open..];
        let next = if rest.starts_with(b"<!--") {
            after(open, b"-->")
        } else if rest.starts_with(b"<![CDATA[") {
            after(open, b"]]>")
        } else if rest.starts_with(b"<?") {
            after(open, b"?>")
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            after(open, b">")
        } else if rest.starts_with(b"<!") {
            after(open, b">")
        } else {
            let Some(close) = tag_end(b, open) else { return true };
            if b[close - 1] != b'/' {
                depth += 1;
                if depth > MAX_XML_DEPTH {
                    return false;
                }
            }
            Some(close + 1)
        };
        let Some(next) = next else { return true };
        i = next;
    }
    true
}

/// The `>` ending the tag opened at `open`, past any in a quoted value.
fn tag_end(b: &[u8], open: usize) -> Option<usize> {
    let mut quote = None;
    for (at, &c) in b.iter().enumerate().skip(open) {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'>' => return Some(at),
            None => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested(levels: usize) -> String {
        format!("<?xml version=\"1.0\"?>{}{}", "<a>".repeat(levels), "</a>".repeat(levels))
    }

    #[test]
    fn nesting_is_counted_to_the_limit() {
        assert!(xml_depth_ok(&nested(MAX_XML_DEPTH)));
        assert!(!xml_depth_ok(&nested(MAX_XML_DEPTH + 1)));
        assert!(!xml_depth_ok(&"<a b=\"c\">".repeat(MAX_XML_DEPTH + 1)), "an unclosed file counts too");
    }

    /// Only an open element is a level: what looks like one in a value, a
    /// comment or CDATA isn't, and neither is an element closed at once
    /// or one after another at the same level.
    #[test]
    fn only_open_elements_count() {
        let many = MAX_XML_DEPTH * 2;
        assert!(xml_depth_ok(&format!("<r>{}</r>", "<a/>".repeat(many))));
        assert!(xml_depth_ok(&format!("<r>{}</r>", "<a></a>".repeat(many))));
        assert!(xml_depth_ok(&format!("<r><!--{}--></r>", "<a>".repeat(many))));
        assert!(xml_depth_ok(&format!("<r><![CDATA[{}]]></r>", "<a>".repeat(many))));
        assert!(xml_depth_ok(&format!("<r><?p {}?></r>", "<a>".repeat(many))));
        assert!(xml_depth_ok(&format!("<r>{}</r>", "<a b=\"/>\" c='>'/>".repeat(many))));
    }
}
