//! Hex-string serialisation wrapper for `egui::Color32`.
//!
//! `egui::Color32` doesn't implement `serde::{Serialize, Deserialize}`
//! directly. We wrap it in `HexColor` so theme TOML files use the
//! familiar `"#rrggbb"` / `"#rrggbbaa"` format that designers and
//! anyone touching CSS already knows. The wrapper is `#[serde(transparent)]`
//! so a TOML field like `accent = "#e8a020"` deserialises straight
//! into `HexColor` without any container struct.
//!
//! 6-digit hex (`#rrggbb`) → opaque, alpha = 0xff.
//! 8-digit hex (`#rrggbbaa`) → premultiplied alpha; the loader
//! does NOT premultiply for you, the file is expected to store the
//! premultiplied channels directly. This matches how the original
//! hardcoded theme constants used `Color32::from_rgba_premultiplied`.

use egui::Color32;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy)]
pub struct HexColor(pub Color32);

impl From<Color32> for HexColor {
    fn from(c: Color32) -> Self {
        Self(c)
    }
}

impl From<HexColor> for Color32 {
    fn from(h: HexColor) -> Self {
        h.0
    }
}

impl Serialize for HexColor {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let c = self.0;
        let s = if c.a() == 0xff {
            format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", c.r(), c.g(), c.b(), c.a())
        };
        ser.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        let trimmed = s.trim().trim_start_matches('#');
        let parse_byte = |i: usize| -> Result<u8, D::Error> {
            u8::from_str_radix(&trimmed[i..i + 2], 16)
                .map_err(|e| serde::de::Error::custom(format!("invalid hex byte at index {i}: {e}")))
        };
        match trimmed.len() {
            6 => {
                let r = parse_byte(0)?;
                let g = parse_byte(2)?;
                let b = parse_byte(4)?;
                Ok(HexColor(Color32::from_rgb(r, g, b)))
            }
            8 => {
                let r = parse_byte(0)?;
                let g = parse_byte(2)?;
                let b = parse_byte(4)?;
                let a = parse_byte(6)?;
                Ok(HexColor(Color32::from_rgba_premultiplied(r, g, b, a)))
            }
            n => Err(serde::de::Error::custom(format!(
                "HexColor expects #rrggbb or #rrggbbaa (6 or 8 hex digits), got {n}: {s:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_opaque() {
        let c = HexColor(Color32::from_rgb(0xe8, 0xa0, 0x20));
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, "\"#e8a020\"");
        let back: HexColor = serde_json::from_str(&s).unwrap();
        assert_eq!(back.0, c.0);
    }

    #[test]
    fn round_trip_alpha() {
        let c = HexColor(Color32::from_rgba_premultiplied(33, 23, 5, 46));
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(s, "\"#2117052e\"");
        let back: HexColor = serde_json::from_str(&s).unwrap();
        assert_eq!(back.0, c.0);
    }

    #[test]
    fn rejects_short_string() {
        let r: Result<HexColor, _> = serde_json::from_str("\"#abc\"");
        assert!(r.is_err());
    }

    #[test]
    fn accepts_no_hash() {
        let c: HexColor = serde_json::from_str("\"e8a020\"").unwrap();
        assert_eq!(c.0, Color32::from_rgb(0xe8, 0xa0, 0x20));
    }
}
