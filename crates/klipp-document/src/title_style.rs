//! The looks a title may be given by name: the brief's type styles,
//! and the three built in. One table for the title op and the edit
//! plan, so a "heading" is the same heading wherever it is asked for.

use klipp_timeline::{Brief, TypeStyle};

/// The look called `name`: the brief's own, else a built-in one.
pub fn look(brief: Option<&Brief>, name: &str) -> Option<TypeStyle> {
    match brief {
        Some(b) => b.type_style(name),
        None => TypeStyle::builtin(&name.trim().to_lowercase()),
    }
}

/// Every name `look` answers to, the brief's first.
pub fn names(brief: Option<&Brief>) -> Vec<String> {
    match brief {
        Some(b) => b.type_style_names(),
        None => TypeStyle::BUILTIN.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brief_replaces_a_built_in_look_by_name_and_adds_its_own() {
        let brief = Brief {
            type_styles: vec![
                TypeStyle { name: "heading".into(), font: Some("Inter".into()), size: Some(60.0), ..Default::default() },
                TypeStyle { name: "kicker".into(), size: Some(28.0), ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(look(Some(&brief), "Heading ").unwrap().font.as_deref(), Some("Inter"));
        assert_eq!(look(Some(&brief), "kicker").unwrap().size, Some(28.0));
        assert_eq!(look(Some(&brief), "card").unwrap().full_frame, Some(true));
        assert!(look(Some(&brief), "banner").is_none());
        assert_eq!(names(Some(&brief)), vec!["heading", "kicker", "callout", "card"]);
        assert_eq!(look(None, "callout").unwrap().y, Some(0.85));
    }
}
