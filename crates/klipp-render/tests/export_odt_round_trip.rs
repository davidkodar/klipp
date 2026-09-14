//! Save / load round-trip for the delivery-target ODT setting on
//! [`klipp_core::ExportSettings`]. Mirrors the `ColorSettings`
//! round-trip pattern in `klipp-project::tests` — non-default value
//! survives a serde round-trip, default round-trips as `MatchViewer`,
//! and a settings file written before the field existed loads with
//! `MatchViewer` via `#[serde(default)]`.
//!
//! ExportSettings isn't currently written into the project file
//! (it's transient — the dialog builds it per export), but the
//! field-level back-compat contract is the same: any user-saved
//! preset / queue entry / preferences blob that misses the field
//! must restore the safe viewer default.

use klipp_core::{ExportSettings, OdtChoice};

#[test]
fn export_settings_default_odt_is_match_viewer() {
    let s = ExportSettings::default();
    assert_eq!(
        s.odt,
        OdtChoice::MatchViewer,
        "default ExportSettings must carry MatchViewer — back-compat for every \
         export call site that uses Default::default()",
    );
}

#[test]
fn match_viewer_round_trips_through_serde() {
    let s = ExportSettings::default();
    let json = serde_json::to_string(&s).unwrap();
    let back: ExportSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(back.odt, OdtChoice::MatchViewer);
}

#[test]
fn rec2100_pq_4000_round_trips_through_serde() {
    // The most "exotic" non-default — exercises the full enum tag
    // serialiser path. If this drifts, every PQ4000 preset on disk
    // breaks.
    let s = ExportSettings {
        odt: OdtChoice::Rec2100Pq4000,
        ..Default::default()
    };
    let json = serde_json::to_string(&s).unwrap();
    let back: ExportSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(back.odt, OdtChoice::Rec2100Pq4000);
}

#[test]
fn every_supported_choice_round_trips_through_serde() {
    // Ranged check across the full menu so a future variant addition
    // can't silently break one without the others noticing.
    for choice in OdtChoice::ALL {
        let s = ExportSettings {
            odt: choice,
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: ExportSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.odt, choice,
            "ODT choice {choice:?} did not survive serde round-trip"
        );
    }
}

#[test]
fn legacy_settings_without_odt_field_loads_as_match_viewer() {
    // Simulates a settings blob written by a build before the ODT
    // field landed. Serde's `#[serde(default)]` should fall back to
    // `MatchViewer` — the safe viewer-matching default.
    let s = ExportSettings::default();
    let json = serde_json::to_string(&s).unwrap();

    // Strip the `odt` field from the JSON object.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    if let Some(obj) = value.as_object_mut() {
        let removed = obj.remove("odt");
        assert!(removed.is_some(), "expected odt field in default-serialised settings");
    }
    let stripped = serde_json::to_string(&value).unwrap();

    let back: ExportSettings = serde_json::from_str(&stripped).unwrap();
    assert_eq!(
        back.odt,
        OdtChoice::MatchViewer,
        "legacy settings (no `odt` field) must load as MatchViewer via serde default",
    );
}

#[test]
fn unknown_odt_tag_loads_as_match_viewer_via_serde_other() {
    // Forwards-compat: a future build picks up an export written by
    // an even-newer build with an ODT we don't know about. The
    // `#[serde(other)] Unknown` arm catches it and the dispatch
    // (klipp_color::display::display_transform_for_export) treats
    // Unknown as MatchViewer at runtime — neither path leaves the
    // user with mystery pixels.
    let s = ExportSettings {
        odt: OdtChoice::MatchViewer,
        ..Default::default()
    };
    let json = serde_json::to_string(&s).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    if let Some(obj) = value.as_object_mut() {
        // Plant a future-only enum tag.
        obj.insert(
            "odt".to_string(),
            serde_json::Value::String("AcesNgRec2390".to_string()),
        );
    }
    let mutated = serde_json::to_string(&value).unwrap();
    let back: ExportSettings = serde_json::from_str(&mutated).unwrap();
    assert_eq!(
        back.odt,
        OdtChoice::Unknown,
        "future ODT tags should land in the Unknown sentinel arm",
    );
}
