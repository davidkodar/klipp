//! OpenTimelineIO (OTIO) project import.
//!
//! Parses `.otio` JSON files (plain, not bundled) and produces a Klipp
//! `Sequence` plus a populated `MediaPool`. Unsupported features (effects,
//! transitions, nested stacks) are dropped with warnings that the caller
//! is expected to write alongside the imported project.
//!
//! Scope is deliberately MVP: tracks, clips, gaps, media references
//! (external + missing), markers (timeline and clip level), and rate-
//! accurate times. Plain `.otio` only — `.otiod` directory bundles and
//! `.otioz` zip archives are out of scope.
//!
//! Parser is serde_json-driven: children of Stacks and Tracks are mixed-
//! schema, so they're parsed as `serde_json::Value` first, then dispatched
//! on the `OTIO_SCHEMA` string. Never panics — all errors are returned as
//! `OtioImportError`.
//!
//! The `opentimelineio` crate on crates.io is a placeholder and is not used.

use klipp_core::{Fps, MarkerColor, RationalTime};
use serde::Deserialize;

pub use crate::mlt_import::ImportResult;

/// Error type for OTIO import failures.
#[derive(Debug, thiserror::Error)]
pub enum OtioImportError {
    #[error("Failed to read file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse OTIO JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("OTIO schema error: {0}")]
    Schema(String),
    #[error("No timeline found in OTIO file")]
    NoTimeline,
}

/// `RationalTime.1` payload — both fields are `f64` in OTIO.
///
/// Kept internal; callers should always go through `rt_from_otio` or
/// `fps_from_otio_rate` to reach Klipp's RationalTime/Fps conventions.
#[derive(Debug, Clone, Copy, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct RawRationalTime {
    #[serde(default)]
    pub rate: f64,
    #[serde(default)]
    pub value: f64,
}

/// `TimeRange.1` payload — `start_time` + `duration` (NOT start + end).
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct RawTimeRange {
    pub start_time: RawRationalTime,
    pub duration: RawRationalTime,
}

/// Map an OTIO `rate` (plain `f64`) to a Klipp `Fps` using the SMPTE
/// num/den convention already used elsewhere in the project.
///
/// Recognised fractional rates collapse to the canonical constructors so
/// timeline maths stays frame-accurate. Anything unrecognised is rounded
/// to the nearest integer rate — good enough for MVP; real NTSC content
/// should hit the explicit branches.
pub(crate) fn fps_from_otio_rate(rate: f64) -> Fps {
    const EPS: f64 = 0.01;
    if (rate - 23.976).abs() < EPS || (rate - 23.98).abs() < EPS {
        Fps::ntsc_23976()
    } else if (rate - 29.97).abs() < EPS {
        Fps::ntsc_29970()
    } else if (rate - 59.94).abs() < EPS {
        Fps::new(60000, 1001)
    } else {
        // Integer or unrecognised: round up to a sane minimum of 1.
        Fps::new(rate.round().max(1.0) as u32, 1)
    }
}

/// Convert an OTIO `RationalTime` to a Klipp `RationalTime` at a caller-
/// specified target rate (normally the sequence fps's `rate()`).
///
/// The conversion goes via seconds to sidestep the difference between
/// OTIO's frames-at-rate convention and Klipp's num/den-derived rate.
/// For example OTIO `{rate: 23.976, value: 24.0}` (= 1.001 s) at target
/// rate 24000 produces `{value: 24024, rate: 24000}` — i.e. 24 NTSC
/// frames × 1001 ticks/frame, matching `Fps::ntsc_23976()`.
pub(crate) fn rt_from_otio(rt: &RawRationalTime, target_rate: u32) -> RationalTime {
    let secs = if rt.rate > 0.0 { rt.value / rt.rate } else { 0.0 };
    RationalTime::from_secs_f64(secs, target_rate)
}

/// Map an OTIO marker color string (UPPERCASE, e.g. `"RED"`, `"CYAN"`)
/// to the nearest Klipp `MarkerColor`. Unknown or missing colors default
/// to `Blue` — the same default Klipp uses elsewhere.
pub(crate) fn otio_color_to_klipp(color: Option<&str>) -> MarkerColor {
    match color.unwrap_or("") {
        "RED" => MarkerColor::Red,
        "ORANGE" => MarkerColor::Orange,
        "YELLOW" => MarkerColor::Yellow,
        "GREEN" => MarkerColor::Green,
        "BLUE" | "CYAN" => MarkerColor::Blue,
        "PURPLE" | "MAGENTA" | "PINK" => MarkerColor::Purple,
        _ => MarkerColor::Blue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_integer_rate() {
        assert_eq!(fps_from_otio_rate(24.0), Fps::film_24());
        assert_eq!(fps_from_otio_rate(25.0), Fps::pal_25());
        assert_eq!(fps_from_otio_rate(30.0), Fps::new(30, 1));
        assert_eq!(fps_from_otio_rate(60.0), Fps::new(60, 1));
    }

    #[test]
    fn fps_ntsc_fractional_rates() {
        assert_eq!(fps_from_otio_rate(23.976), Fps::ntsc_23976());
        assert_eq!(fps_from_otio_rate(23.98), Fps::ntsc_23976());
        assert_eq!(fps_from_otio_rate(29.97), Fps::ntsc_29970());
        assert_eq!(fps_from_otio_rate(59.94), Fps::new(60000, 1001));
    }

    #[test]
    fn fps_unrecognised_rate_rounds_to_integer() {
        // Exotic rate: defaults to rounded integer rather than scaling.
        assert_eq!(fps_from_otio_rate(48.0), Fps::new(48, 1));
        // Degenerate input doesn't panic.
        assert_eq!(fps_from_otio_rate(0.0), Fps::new(1, 1));
        assert_eq!(fps_from_otio_rate(-5.0), Fps::new(1, 1));
    }

    #[test]
    fn rt_integer_rate_round_trip() {
        // 12 frames at 24 fps → 0.5 s → 12 frames at target rate 24.
        let r = RawRationalTime { rate: 24.0, value: 12.0 };
        assert_eq!(rt_from_otio(&r, 24), RationalTime { value: 12, rate: 24 });
    }

    #[test]
    fn rt_ntsc_fractional_preserves_frame_count() {
        // 24 frames at 23.976 fps = 1.001 s. Target rate 24000 (ntsc_23976
        // canonical) should map to value 24024 = 24 × 1001 ticks/frame.
        let r = RawRationalTime { rate: 23.976, value: 24.0 };
        let converted = rt_from_otio(&r, 24000);
        // Allow ±1 tick tolerance for f64 rounding (24.0/23.976 is not
        // exactly 1.001).
        assert_eq!(converted.rate, 24000);
        let diff = (converted.value - 24024).abs();
        assert!(diff <= 1, "expected ~24024 ticks, got {}", converted.value);
    }

    #[test]
    fn rt_zero_rate_is_safe() {
        // Malformed input must not divide by zero.
        let r = RawRationalTime { rate: 0.0, value: 100.0 };
        assert_eq!(rt_from_otio(&r, 24), RationalTime { value: 0, rate: 24 });
    }

    #[test]
    fn rt_audio_rate_normalises_to_video_sequence_rate() {
        // Audio at 48 kHz sampled at value=48000 (= 1 second) should produce
        // 24 frames at a 24 fps sequence rate — mixed-rate content converges.
        let r = RawRationalTime { rate: 48000.0, value: 48000.0 };
        assert_eq!(rt_from_otio(&r, 24), RationalTime { value: 24, rate: 24 });
    }

    #[test]
    fn color_known_mappings() {
        assert_eq!(otio_color_to_klipp(Some("RED")), MarkerColor::Red);
        assert_eq!(otio_color_to_klipp(Some("ORANGE")), MarkerColor::Orange);
        assert_eq!(otio_color_to_klipp(Some("YELLOW")), MarkerColor::Yellow);
        assert_eq!(otio_color_to_klipp(Some("GREEN")), MarkerColor::Green);
        assert_eq!(otio_color_to_klipp(Some("BLUE")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("CYAN")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("PURPLE")), MarkerColor::Purple);
        assert_eq!(otio_color_to_klipp(Some("MAGENTA")), MarkerColor::Purple);
        assert_eq!(otio_color_to_klipp(Some("PINK")), MarkerColor::Purple);
    }

    #[test]
    fn color_unknown_and_missing_default_to_blue() {
        assert_eq!(otio_color_to_klipp(Some("BLACK")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("WHITE")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(Some("chartreuse")), MarkerColor::Blue);
        assert_eq!(otio_color_to_klipp(None), MarkerColor::Blue);
    }

    #[test]
    fn raw_rational_time_deserialises() {
        let raw: RawRationalTime = serde_json::from_str(
            r#"{"OTIO_SCHEMA":"RationalTime.1","rate":24.0,"value":48.0}"#,
        )
        .unwrap();
        assert_eq!(raw, RawRationalTime { rate: 24.0, value: 48.0 });
    }

    #[test]
    fn raw_rational_time_missing_fields_default_to_zero() {
        let raw: RawRationalTime = serde_json::from_str("{}").unwrap();
        assert_eq!(raw, RawRationalTime { rate: 0.0, value: 0.0 });
    }
}
