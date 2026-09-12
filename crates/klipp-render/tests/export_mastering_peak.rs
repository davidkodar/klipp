//! The mastering peak of an HDR export: what the user picks, what
//! overrides it, and what a settings blob written before the peak was
//! choosable loads as.
//!
//! `ExportSettings::mastering_peak_nits` is the user's choice;
//! `ExportSettings::hdr_peak_nits()` is the peak the render and the
//! encoder actually use. They differ wherever the delivery states a peak
//! of its own — an ACES Rec.2100 output transform, or HLG — so every
//! call site reads the method. These tests pin that difference.
//!
//! Serde lives here rather than in `klipp-core` because that crate is
//! kept free of `serde_json` (workspace dependency policy, commit
//! 3e2ca08); this is the same home as the ODT round-trip beside it.

use klipp_core::{
    ExportSettings, HdrMode, OdtChoice, DEFAULT_MASTERING_PEAK_NITS, MASTERING_PEAK_CHOICES,
    MASTERING_PEAK_RANGE,
};

/// A PQ delivery through the viewer's own picture — the case where the
/// user's chosen peak is the one that counts.
fn pq_export(peak: f32) -> ExportSettings {
    ExportSettings {
        pan_law: Default::default(),
        bit_depth: 10,
        hdr_mode: HdrMode::Pq,
        odt: OdtChoice::MatchViewer,
        mastering_peak_nits: peak,
        ..Default::default()
    }
}

#[test]
fn default_settings_master_to_1000_nits() {
    // Every export built through `Default::default()` must keep
    // mastering where it was before the peak became a setting.
    let s = ExportSettings::default();
    assert_eq!(s.mastering_peak_nits, DEFAULT_MASTERING_PEAK_NITS);
    assert_eq!(ExportSettings { hdr_mode: HdrMode::Pq, ..s }.hdr_peak_nits(), 1000.0);
}

#[test]
fn a_chosen_peak_is_the_peak_a_pq_export_masters_to() {
    for peak in MASTERING_PEAK_CHOICES {
        assert_eq!(pq_export(peak).hdr_peak_nits(), peak, "PQ delivery at {peak} nits");
    }
}

#[test]
fn every_offered_peak_round_trips_through_serde() {
    for peak in MASTERING_PEAK_CHOICES {
        let json = serde_json::to_string(&pq_export(peak)).unwrap();
        let back: ExportSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mastering_peak_nits, peak, "{peak} nits did not survive serde");
        assert_eq!(back.hdr_peak_nits(), peak);
    }
}

#[test]
fn settings_written_before_the_peak_existed_load_as_1000() {
    // A preset or queue entry from a build that had no peak field must
    // restore the 1000 nits it was exported at — not an arbitrary 0.
    let json = serde_json::to_string(&pq_export(4000.0)).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let removed = value.as_object_mut().unwrap().remove("mastering_peak_nits");
    assert!(removed.is_some(), "expected the field in default-serialised settings");

    let back: ExportSettings = serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
    assert_eq!(back.mastering_peak_nits, DEFAULT_MASTERING_PEAK_NITS);
    assert_eq!(back.hdr_peak_nits(), 1000.0);
}

#[test]
fn an_aces_pq_transform_states_its_own_peak() {
    // The ACES output transforms are named for the display they master
    // to, so the transform wins over whatever the picker last held.
    assert_eq!(
        ExportSettings { odt: OdtChoice::Rec2100Pq4000, ..pq_export(1000.0) }.hdr_peak_nits(),
        4000.0,
    );
    assert_eq!(
        ExportSettings { odt: OdtChoice::Rec2100Pq1000, ..pq_export(4000.0) }.hdr_peak_nits(),
        1000.0,
    );
}

#[test]
fn hlg_masters_to_its_nominal_1000_nit_display() {
    // BT.2100 defines HLG on a 1000-nit nominal display. The dialog
    // hides the picker for HLG, but a settings blob can still carry a
    // peak from a PQ export the user switched away from.
    let s = ExportSettings { hdr_mode: HdrMode::Hlg, ..pq_export(4000.0) };
    assert_eq!(s.delivery(), HdrMode::Hlg);
    assert_eq!(s.hdr_peak_nits(), 1000.0);
}

#[test]
fn a_peak_from_outside_the_range_is_held_inside_it() {
    // Hand-edited or corrupted settings must not reach the encoder as a
    // mastering luminance no display can mean.
    let (lo, hi) = (*MASTERING_PEAK_RANGE.start(), *MASTERING_PEAK_RANGE.end());
    assert_eq!(pq_export(0.0).hdr_peak_nits(), lo);
    assert_eq!(pq_export(-5000.0).hdr_peak_nits(), lo);
    assert_eq!(pq_export(1.0e9).hdr_peak_nits(), hi);
    // A NaN would otherwise pass straight through `clamp` and turn the
    // whole picture into NaN downstream.
    assert_eq!(pq_export(f32::NAN).hdr_peak_nits(), DEFAULT_MASTERING_PEAK_NITS);
    assert_eq!(pq_export(f32::INFINITY).hdr_peak_nits(), DEFAULT_MASTERING_PEAK_NITS);
}

#[test]
fn the_peak_does_not_disturb_an_sdr_export() {
    // SDR renders through an output transform and never reads the peak;
    // pin that choosing one cannot change whether it renders HDR.
    let s = ExportSettings { hdr_mode: HdrMode::Sdr, ..pq_export(4000.0) };
    assert!(!s.renders_hdr());
    assert_eq!(s.delivery(), HdrMode::Sdr);
}
