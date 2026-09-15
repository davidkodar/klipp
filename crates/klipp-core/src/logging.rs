//! What an editor or assistant writes about a clip while logging it:
//! the scene, shot and take it belongs to, a description, comments,
//! keywords, whether it is a good take, and the reel it goes under.
//! Nothing here comes from the file; it is the human's record, kept in
//! the project, and what the list view, the filters, smart bins and the
//! metadata sheet sort and find by.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipLog {
    pub scene: String,
    pub shot: String,
    pub take: String,
    pub description: String,
    pub comments: String,
    pub keywords: Vec<String>,
    pub good_take: bool,
    /// The reel as the editor names it; empty means the file's own.
    pub reel: String,
}

impl ClipLog {
    /// Nothing has been logged.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The keywords on one line, comma separated, as they are edited.
    pub fn keywords_line(&self) -> String {
        self.keywords.join(", ")
    }

    /// Keywords from one edited line: split on commas, trimmed, empties
    /// dropped, order kept, repeats dropped.
    pub fn set_keywords_line(&mut self, line: &str) {
        let mut out: Vec<String> = Vec::new();
        for k in line.split(',').map(str::trim).filter(|k| !k.is_empty()) {
            if !out.iter().any(|o| o.eq_ignore_ascii_case(k)) {
                out.push(k.to_string());
            }
        }
        self.keywords = out;
    }

    /// "Scene 12 · Shot 3 · Take 2", from whatever parts are set.
    pub fn slate_line(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.scene.is_empty() {
            parts.push(format!("Scene {}", self.scene));
        }
        if !self.shot.is_empty() {
            parts.push(format!("Shot {}", self.shot));
        }
        if !self.take.is_empty() {
            parts.push(format!("Take {}", self.take));
        }
        if parts.is_empty() { None } else { Some(parts.join(" \u{00B7} ")) }
    }

    /// Whether `needle` (already lower-case) appears in any logged text.
    pub fn matches(&self, needle: &str) -> bool {
        [&self.scene, &self.shot, &self.take, &self.description, &self.comments, &self.reel]
            .iter()
            .any(|s| s.to_lowercase().contains(needle))
            || self.keywords.iter().any(|k| k.to_lowercase().contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_edited_as_one_line_and_kept_tidy() {
        let mut log = ClipLog::default();
        log.set_keywords_line(" interview, b-roll ,, Interview, wide ");
        assert_eq!(log.keywords, vec!["interview", "b-roll", "wide"]);
        assert_eq!(log.keywords_line(), "interview, b-roll, wide");
        assert!(log.matches("roll"));
        assert!(!log.matches("drone"));
    }

    #[test]
    fn the_slate_line_names_what_is_set() {
        let log = ClipLog { scene: "12".into(), take: "2".into(), ..Default::default() };
        assert_eq!(log.slate_line().as_deref(), Some("Scene 12 \u{00B7} Take 2"));
        assert_eq!(ClipLog::default().slate_line(), None);
        assert!(ClipLog::default().is_empty());
    }
}
