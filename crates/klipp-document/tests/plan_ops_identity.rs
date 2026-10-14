//! A plan without scene layers lowers to the ops it always did (#1926):
//! the ops of a plan using every other part of the format, against the
//! list the format gave before layers existed. `KLIPP_BLESS=1` writes
//! the list again; only do that when a change to plans means to change it.

use klipp_core::{Fps, MediaId, Timecode};
use klipp_document::edit_plan::to_ops;
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{EditPlan, Sequence};

fn item(n: u128, path: &str, secs: i64, video: bool) -> MediaItem {
    let mut it = MediaItem::synthetic_for_test(path.into(), None);
    it.id = MediaId(uuid::Uuid::from_u128(n));
    it.duration = Timecode { value: secs * 25, rate: 25 };
    it.has_audio = true;
    if video {
        it.video_codec = "h264".into();
    }
    it
}

#[test]
fn a_plan_without_layers_lowers_to_the_ops_it_always_did() {
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    let mut pool = MediaPool::new();
    pool.add(item(1, "/tmp/screen.mov", 60, true));
    pool.add(item(2, "/tmp/bed.wav", 120, false));
    pool.add(item(3, "/tmp/whoosh.wav", 2, false));
    let screen = "00000000-0000-0000-0000-000000000001";
    let bed = "00000000-0000-0000-0000-000000000002";
    let whoosh = "00000000-0000-0000-0000-000000000003";
    let plan: EditPlan = serde_json::from_value(serde_json::json!({
        "segments": [
            {"media_id": screen, "source_in_secs": 0.0, "source_out_secs": 3.0, "title": {"text": "Klipp", "style": "card", "secs": 2.0}},
            {"media_id": screen, "source_in_secs": 10.0, "source_out_secs": 16.0, "with_audio": true,
             "zoom": {"from": 1.0, "to": 1.15, "x": 0.2, "y": 0.1}, "title": {"text": "Search", "enter": {"kind": "rise"}},
             "sfx": {"media_id": whoosh, "at_secs": 0.5, "volume_db": -6.0}},
            {"media_id": screen, "source_in_secs": 30.0, "source_out_secs": 34.0, "transition": "cross_dissolve",
             "placement": {"x": 0.5, "y": 0.5, "width": 0.7, "height": 0.7, "rotation_deg": -3.0},
             "background": {"generator": "gradient", "colors": ["#101820", "#304050"]},
             "title": {"text": "Try it", "style": "callout", "secs": 9.0}}
        ],
        "music": {"media_id": bed, "duck_db": 9.0, "fade_out_secs": 1.0},
        "approved": true
    }))
    .unwrap();
    let got = serde_json::to_string_pretty(&to_ops(&plan, &seq, &pool, None)).unwrap();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/plan_ops_without_layers.json");
    if std::env::var_os("KLIPP_BLESS").is_some() {
        std::fs::write(path, &got).unwrap();
        return;
    }
    assert_eq!(got, std::fs::read_to_string(path).unwrap(), "a plan without layers lowers as it did before scenes");
}
