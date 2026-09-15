//! A still's EXIF: the camera and lens, the exposure, and when it was
//! taken. Read at import for every still so a photo shows what a clip's
//! container tags show for video.

use klipp_core::{shot::civil_to_epoch, ShotMetadata};
use std::path::Path;

/// What a still's EXIF says about it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Still {
    pub shot: ShotMetadata,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    /// When it was taken, by the camera's clock, as Unix seconds.
    pub taken_at: Option<u64>,
}

/// Read `path`'s EXIF. `None` when the file has none we can read.
pub fn read_exif(path: &Path) -> Option<Still> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    Some(still_from(&exif))
}

/// The record in a parsed EXIF block.
pub fn still_from(exif: &exif::Exif) -> Still {
    use exif::{In, Tag, Value};
    let mut out = Still::default();
    let text = |tag: Tag| -> Option<String> {
        let f = exif.get_field(tag, In::PRIMARY)?;
        let s = f.display_value().with_unit(exif).to_string();
        let s = s.trim().trim_matches('"').trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    };
    let number = |tag: Tag| -> Option<f64> {
        let f = exif.get_field(tag, In::PRIMARY)?;
        match &f.value {
            Value::Rational(v) => v.first().filter(|r| r.denom != 0).map(|r| r.to_f64()),
            Value::SRational(v) => v.first().filter(|r| r.denom != 0).map(|r| r.to_f64()),
            _ => f.value.get_uint(0).map(f64::from),
        }
    };
    out.camera_make = text(Tag::Make);
    out.camera_model = text(Tag::Model);
    let shot = &mut out.shot;
    shot.lens = text(Tag::LensModel);
    shot.iso = number(Tag::PhotographicSensitivity).map(|v| v.round() as u32);
    // The exposure time as photographers write it: "1/50", or "2" for
    // two seconds.
    if let Some(f) = exif.get_field(Tag::ExposureTime, In::PRIMARY) {
        if let Value::Rational(v) = &f.value {
            if let Some(r) = v.first().filter(|r| r.denom != 0 && r.num != 0) {
                let t = r.to_f64();
                shot.shutter = Some(if t < 1.0 { format!("1/{}", (1.0 / t).round() as u32) } else { trim(t) });
            }
        }
    }
    shot.aperture = number(Tag::FNumber).map(|v| v as f32);
    shot.focal_length_mm = number(Tag::FocalLength).map(|v| v as f32);
    shot.white_balance = exif.get_field(Tag::WhiteBalance, In::PRIMARY).and_then(|f| f.value.get_uint(0)).map(|v| match v {
        0 => "Auto".to_string(),
        1 => "Manual".to_string(),
        other => format!("{other}"),
    });
    out.taken_at = text(Tag::DateTimeOriginal).or_else(|| text(Tag::DateTime)).and_then(|s| parse_exif_datetime(&s));
    if shot.lens.is_some() || shot.iso.is_some() || shot.shutter.is_some() || shot.aperture.is_some() || shot.focal_length_mm.is_some() {
        shot.exposure_source = Some("EXIF".to_string());
    }
    // Every field as written, under its EXIF name.
    for f in exif.fields() {
        if f.ifd_num != In::PRIMARY && f.ifd_num != In::THUMBNAIL {
            continue;
        }
        if f.ifd_num == In::THUMBNAIL {
            continue;
        }
        let value = f.display_value().with_unit(exif).to_string();
        let value = value.trim().trim_matches('"').trim();
        if value.is_empty() || value.len() > 200 {
            continue;
        }
        shot.tags.push((format!("exif:{}", f.tag), value.to_string()));
    }
    out
}

fn trim(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// "2024:03:15 10:30:00" as the camera writes it, or "2024-03-15
/// 10:30:00" as the EXIF crate displays it.
fn parse_exif_datetime(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, time) = s.split_once(' ')?;
    let mut d = date.split([':', '-']);
    let mut t = time.split(':');
    let y = d.next()?.parse().ok()?;
    let mo = d.next()?.parse().ok()?;
    let da = d.next()?.parse().ok()?;
    let h = t.next()?.parse().ok()?;
    let mi = t.next()?.parse().ok()?;
    let se = t.next()?.split('.').next()?.parse().ok()?;
    civil_to_epoch(y, mo, da, h, mi, se)
}

#[cfg(test)]
mod tests {
    use super::*;
    use exif::experimental::Writer;
    use exif::{Field, In, Rational, Tag, Value};

    fn block() -> Vec<u8> {
        let fields = [
            Field { tag: Tag::Make, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Canon".to_vec()]) },
            Field { tag: Tag::Model, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Canon EOS R5".to_vec()]) },
            Field { tag: Tag::LensModel, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"RF24-70mm F2.8 L IS USM".to_vec()]) },
            Field { tag: Tag::PhotographicSensitivity, ifd_num: In::PRIMARY, value: Value::Short(vec![800]) },
            Field { tag: Tag::ExposureTime, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 1, denom: 250 }]) },
            Field { tag: Tag::FNumber, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 28, denom: 10 }]) },
            Field { tag: Tag::FocalLength, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 50, denom: 1 }]) },
            Field { tag: Tag::WhiteBalance, ifd_num: In::PRIMARY, value: Value::Short(vec![0]) },
            Field { tag: Tag::DateTimeOriginal, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"2024:03:15 10:30:00".to_vec()]) },
        ];
        let mut writer = Writer::new();
        for f in &fields {
            writer.push_field(f);
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        writer.write(&mut buf, false).unwrap();
        buf.into_inner()
    }

    #[test]
    fn a_stills_exif_gives_camera_lens_exposure_and_when_it_was_taken() {
        let exif = exif::Reader::new().read_raw(block()).unwrap();
        let still = still_from(&exif);
        assert_eq!(still.camera_make.as_deref(), Some("Canon"));
        assert_eq!(still.camera_model.as_deref(), Some("Canon EOS R5"));
        assert_eq!(still.shot.lens.as_deref(), Some("RF24-70mm F2.8 L IS USM"));
        assert_eq!(still.shot.iso, Some(800));
        assert_eq!(still.shot.shutter.as_deref(), Some("1/250"));
        assert_eq!(still.shot.aperture, Some(2.8));
        assert_eq!(still.shot.focal_length_mm, Some(50.0));
        assert_eq!(still.shot.white_balance.as_deref(), Some("Auto"));
        assert_eq!(still.taken_at, Some(1_710_498_600));
        assert_eq!(still.shot.exposure_source.as_deref(), Some("EXIF"));
        assert!(still.shot.tags.iter().any(|(k, _)| k == "exif:FNumber"));
    }

    #[test]
    fn a_file_without_exif_reads_as_none() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("klipp-noexif-{}.jpg", std::process::id()));
        std::fs::write(&p, b"not a picture").unwrap();
        assert_eq!(read_exif(&p), None);
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn long_exposures_read_in_seconds() {
        assert_eq!(parse_exif_datetime("2024:03:15 10:30:00"), Some(1_710_498_600));
        assert_eq!(parse_exif_datetime("2024-03-15 10:30:00"), Some(1_710_498_600));
        assert_eq!(parse_exif_datetime("garbage"), None);
        assert_eq!(trim(2.0), "2");
        assert_eq!(trim(2.5), "2.5");
    }
}
