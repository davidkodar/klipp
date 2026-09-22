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
        // Written the way a column shows them and the way an editor
        // says them: "35mm", "f/2.8", "ISO 800" all mean their number.
        Field::FocalLength => v.to_lowercase().replace("mm", " ").trim().parse().ok(),
        Field::Aperture => {
            let t = v.to_lowercase();
            let t = t.trim().trim_start_matches("f/").trim_start_matches('f').trim();
            t.parse().ok()
        }
        Field::Iso => v.to_lowercase().replace("iso", " ").trim().parse().ok(),
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

    /// A clip with the camera metadata a real card carries.
    fn shot(focal: f32, iso: u32, aperture: f32) -> MediaItem {
        let mut m = item("1", 0, &[]);
        m.shot.focal_length_mm = Some(focal);
        m.shot.iso = Some(iso);
        m.shot.aperture = Some(aperture);
        m.shot.shutter = Some("1/48".into());
        m.shot.gamma = Some("S-Log3".into());
        m.shot.colour_gamut = Some("S-Gamut3.Cine".into());
        m
    }

    #[test]
    fn everything_shot_wider_than_35mm() {
        // The canonical reason to read this metadata at all, and the
        // example the issue was written around. As a string compare
        // "9" sorts above "35", which is what made it useless.
        let wide = shot(24.0, 800, 2.8);
        let long = shot(85.0, 800, 2.8);
        let nifty = shot(9.0, 800, 2.8);

        assert!(rule(Field::FocalLength, RuleOp::AtMost, "35").matches(&wide));
        assert!(!rule(Field::FocalLength, RuleOp::AtMost, "35").matches(&long));
        assert!(
            rule(Field::FocalLength, RuleOp::AtMost, "35").matches(&nifty),
            "9 mm is wider than 35 mm — as a number, not as text"
        );
        assert!(rule(Field::FocalLength, RuleOp::AtLeast, "50").matches(&long));
    }

    #[test]
    fn a_rule_takes_the_spelling_an_editor_would_type() {
        let m = shot(35.0, 800, 2.8);
        for written in ["35", "35mm", "35 mm", "35MM"] {
            assert!(rule(Field::FocalLength, RuleOp::Is, written).matches(&m), "{written}");
        }
        for written in ["2.8", "f/2.8", "F/2.8", "f2.8"] {
            assert!(rule(Field::Aperture, RuleOp::Is, written).matches(&m), "{written}");
        }
        for written in ["800", "ISO 800", "iso800"] {
            assert!(rule(Field::Iso, RuleOp::Is, written).matches(&m), "{written}");
        }
        // And the comparisons that make those useful.
        assert!(rule(Field::Aperture, RuleOp::AtMost, "f/4").matches(&m), "2.8 is wider than f/4");
        assert!(rule(Field::Iso, RuleOp::AtLeast, "ISO 400").matches(&m));
    }

    #[test]
    fn the_text_ones_are_still_matched_as_text() {
        // Shutter stays text because "1/48" and "172.8°" are both real,
        // and gamma is how a colourist finds what needs a transform.
        let m = shot(35.0, 800, 2.8);
        assert!(rule(Field::Gamma, RuleOp::Contains, "log").matches(&m));
        assert!(rule(Field::ColourGamut, RuleOp::Contains, "S-Gamut").matches(&m));
        assert!(rule(Field::Shutter, RuleOp::Is, "1/48").matches(&m));
        // Nothing recorded is not a match for something.
        let bare = item("1", 0, &[]);
        assert!(rule(Field::Gamma, RuleOp::IsEmpty, "").matches(&bare));
        assert!(!rule(Field::FocalLength, RuleOp::IsSet, "").matches(&bare));
    }

    #[test]
    fn a_column_shows_the_unit() {
        use crate::FieldValue;
        let m = shot(35.0, 800, 2.8);
        assert_eq!(Field::FocalLength.text(&m), "35 mm");
        assert_eq!(Field::Aperture.text(&m), "f/2.8");
        assert_eq!(Field::Iso.text(&m), "800");
        // A fractional lens keeps its decimal; a whole stop loses it.
        assert_eq!(Field::FocalLength.text(&shot(10.5, 100, 2.0)), "10.5 mm");
        assert_eq!(Field::Aperture.text(&shot(10.5, 100, 2.0)), "f/2");
        // Nothing recorded shows nothing, rather than a zero.
        assert_eq!(Field::FocalLength.text(&item("1", 0, &[])), "");
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
