//! What a smart bin's rules make of a clip.

use crate::{FieldValue, MediaItem};
use klipp_core::{Field, RuleOp, SmartBin, SmartRule};

/// A rule, or a bin of them, tried against a clip.
pub trait SmartMatch {
    fn matches(&self, item: &MediaItem) -> bool;
}

impl SmartMatch for SmartRule {
    fn matches(&self, item: &MediaItem) -> bool {
        let text = self.field.text(item);
        let wanted = self.value.trim();
        match self.op {
            RuleOp::IsSet => !text.is_empty(),
            RuleOp::IsEmpty => text.is_empty(),
            RuleOp::Contains => !wanted.is_empty() && text.to_lowercase().contains(&wanted.to_lowercase()),
            RuleOp::NotContains => !text.to_lowercase().contains(&wanted.to_lowercase()),
            RuleOp::Is | RuleOp::IsNot => {
                let same = match (number_of(self.field, item), rule_number(self.field, wanted)) {
                    (Some(a), Some(b)) if !self.field.is_date() => (a - b).abs() < 1e-6,
                    // A date "is" a day: the same civil date.
                    (Some(a), Some(b)) => (a - b).abs() < 86_400.0 && a >= b,
                    _ => text.eq_ignore_ascii_case(wanted),
                };
                if self.op == RuleOp::Is { same } else { !same }
            }
            RuleOp::AtLeast | RuleOp::After => compare(self.field, item, wanted, |a, b| a >= b, &text),
            RuleOp::AtMost | RuleOp::Before => compare(self.field, item, wanted, |a, b| a <= b, &text),
        }
    }
}

impl SmartMatch for SmartBin {
    fn matches(&self, item: &MediaItem) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        if self.match_all {
            self.rules.iter().all(|r| r.matches(item))
        } else {
            self.rules.iter().any(|r| r.matches(item))
        }
    }
}

/// Numbers compare as numbers, dates as dates, and anything else as
/// text, so "Scene is at least 10" works on numbered scenes and on
/// lettered ones alike.
fn compare(field: Field, item: &MediaItem, wanted: &str, ok: impl Fn(f64, f64) -> bool, text: &str) -> bool {
    match (number_of(field, item), rule_number(field, wanted)) {
        (Some(a), Some(b)) => ok(a, b),
        _ => !text.is_empty() && ok(cmp_text(text, wanted) as i32 as f64, 0.0),
    }
}

fn cmp_text(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

fn number_of(field: Field, item: &MediaItem) -> Option<f64> {
    field.number(item)
}

/// The rule's value as the field's number: a date as "YYYY-MM-DD", a
/// duration as seconds or "m:ss", a size in MB, a resolution as
/// "3840x2160" or its width, a rating as stars or a count.
fn rule_number(field: Field, v: &str) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    if field.is_date() {
        let mut parts = v.split(['-', ':', ' ', 'T']).filter(|p| !p.is_empty());
        let y = parts.next()?.parse().ok()?;
        let m = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
        let d = parts.next().and_then(|p| p.parse().ok()).unwrap_or(1);
        let h = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        let mi = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        return klipp_core::shot::civil_to_epoch(y, m, d, h, mi, 0).map(|s| s as f64);
    }
    match field {
        Field::Duration => {
            if v.contains(':') {
                // "h:mm:ss" or "m:ss".
                let mut total = 0.0;
                for part in v.split(':') {
                    total = total * 60.0 + part.trim().parse::<f64>().ok()?;
                }
                Some(total)
            } else {
                v.parse().ok()
            }
        }
        Field::FileSize => v.to_lowercase().trim_end_matches("mb").trim().parse::<f64>().ok().map(|mb| mb * 1024.0 * 1024.0),
        Field::Resolution => match v.to_lowercase().split_once('x') {
            Some((w, h)) => Some(w.trim().parse::<f64>().ok()? * h.trim().parse::<f64>().ok()?),
            None => match v.to_lowercase().as_str() {
                "4k" | "uhd" => Some(3840.0 * 2160.0),
                "1080p" | "hd" => Some(1920.0 * 1080.0),
                "720p" => Some(1280.0 * 720.0),
                _ => v.parse::<f64>().ok().map(|w| w * w * 9.0 / 16.0),
            },
        },
        Field::Rating => Some(v.chars().filter(|c| *c == '\u{2605}').count() as f64).filter(|n| *n > 0.0).or_else(|| v.parse().ok()),
        _ => v.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaItemKind;

    fn item(scene: &str, rating: u8, keywords: &[&str]) -> MediaItem {
        let mut m = MediaItem::synthetic_for_test("/tmp/c.mov".into(), None);
        m.kind = MediaItemKind::File;
        m.filename = "c.mov".into();
        m.rating = rating;
        m.log.scene = scene.into();
        m.log.keywords = keywords.iter().map(|k| k.to_string()).collect();
        m.camera_make = Some("Sony".into());
        m.media_created_at = Some(1_710_498_600); // 2024-03-15 10:30
        m.resolution = klipp_core::Resolution { width: 3840, height: 2160 };
        m.duration = klipp_core::RationalTime::from_secs_f64(90.0, 25);
        m
    }

    fn rule(field: Field, op: RuleOp, value: &str) -> SmartRule {
        SmartRule { field, op, value: value.into() }
    }

    #[test]
    fn rules_compare_text_numbers_dates_and_flags_the_way_they_read() {
        let m = item("12", 4, &["interview", "wide"]);
        assert!(rule(Field::Scene, RuleOp::Is, "12").matches(&m));
        assert!(rule(Field::Scene, RuleOp::AtLeast, "10").matches(&m), "12 is at least 10 as a number");
        assert!(rule(Field::Scene, RuleOp::AtLeast, "9").matches(&item("10", 0, &[])), "10 is at least 9, not \"1\" before \"9\"");
        assert!(rule(Field::Rating, RuleOp::AtLeast, "\u{2605}\u{2605}\u{2605}").matches(&m));
        assert!(!rule(Field::Rating, RuleOp::AtLeast, "5").matches(&m));
        assert!(rule(Field::Keywords, RuleOp::Contains, "Interview").matches(&m));
        assert!(rule(Field::Camera, RuleOp::Is, "sony").matches(&m));
        assert!(rule(Field::ShootDate, RuleOp::After, "2024-03-01").matches(&m));
        assert!(!rule(Field::ShootDate, RuleOp::Before, "2024-03-01").matches(&m));
        assert!(rule(Field::ShootDate, RuleOp::Is, "2024-03-15").matches(&m));
        assert!(rule(Field::Resolution, RuleOp::AtLeast, "4k").matches(&m));
        assert!(rule(Field::Duration, RuleOp::AtMost, "2:00").matches(&m));
        assert!(!rule(Field::Duration, RuleOp::AtMost, "60").matches(&m));
        assert!(rule(Field::GoodTake, RuleOp::IsEmpty, "").matches(&m));
        assert!(rule(Field::Lens, RuleOp::IsEmpty, "").matches(&m));
        assert!(!rule(Field::Lens, RuleOp::Contains, "24").matches(&m), "nothing contains what is not there");
    }

    #[test]
    fn a_bin_gathers_by_all_or_any_of_its_rules() {
        let keeper = item("12", 5, &["interview"]);
        let other = item("3", 5, &[]);
        let mut bin = SmartBin { id: "b".into(), name: "Keepers".into(), match_all: true, rules: vec![rule(Field::Rating, RuleOp::AtLeast, "4"), rule(Field::Keywords, RuleOp::Contains, "interview")] };
        assert!(bin.matches(&keeper));
        assert!(!bin.matches(&other));
        bin.match_all = false;
        assert!(bin.matches(&other));
        bin.rules.clear();
        assert!(!bin.matches(&keeper), "no rules gather nothing");
    }
}
