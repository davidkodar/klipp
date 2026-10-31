//! #3209: an audition's takes are saved with the project and read back as
//! they were, and a project file's audition is read as untrusted: one that
//! doesn't name the take in the slot is dropped with the slot kept, and a
//! take's own clip never holds an audition of its own.

use klipp_core::{RationalTime, TakeId};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project_with_color, ColorSettings};
use klipp_timeline::audition::{Audition, Take};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

fn tc(f: i64) -> RationalTime {
    RationalTime::from_frames(f, 24)
}

/// A slot with its sound linked on A1, holding a second take of another
/// file with a blur, its own sound with it.
fn with_takes() -> (Sequence, TakeId) {
    let mut seq = Sequence::new_default();
    let (m, other) = (klipp_core::MediaId::new(), klipp_core::MediaId::new());
    let mut slot = Clip::media(m, tc(0), tc(48), tc(0), tc(48));
    let mut sound = Clip::media(m, tc(0), tc(48), tc(0), tc(48));
    slot.linked_clip_id = Some(sound.id);
    sound.linked_clip_id = Some(slot.id);
    let mut take = klipp_timeline::test_clip::TestClip::of(Clip::media(other, tc(0), tc(72), tc(24), tc(96))).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 7.0 }).build();
    let mut take_sound = Clip::media(other, tc(0), tc(72), tc(24), tc(96));
    take.linked_clip_id = Some(take_sound.id);
    take_sound.linked_clip_id = Some(take.id);
    let mut a1 = Track::new("A1", TrackKind::Audio);
    let first = Take { id: TakeId::new(), name: "Take 1".into(), clip: None, sound: Vec::new() };
    let wide = Take { id: TakeId::new(), name: "Wide".into(), clip: Some(Box::new(take)), sound: vec![(a1.id, take_sound)] };
    let wide_id = wide.id;
    slot.audition = Some(Box::new(Audition { active: first.id, takes: vec![first, wide] }));
    seq.tracks[0].clips = vec![slot];
    a1.clips = vec![sound];
    seq.tracks.push(a1);
    (seq, wide_id)
}

fn project_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_audition_{}_{}", label, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Saved, the project file changed by `edit`, and read back.
fn round_trip(label: &str, seq: Sequence, edit: impl FnOnce(&mut serde_json::Value)) -> Sequence {
    let dir = project_dir(label);
    save_project_with_color(&dir, &[seq], &MediaPool::new(), &ColorSettings::default(), None, None, "").unwrap();
    let file = dir.join("project.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    edit(&mut json);
    std::fs::write(&file, serde_json::to_string(&json).unwrap()).unwrap();
    let (seqs, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    seqs.into_iter().next().expect("one sequence")
}

/// The first saved clip's audition, found by its place in the file.
fn audition(json: &mut serde_json::Value) -> &mut serde_json::Value {
    fn path(v: &serde_json::Value, at: String) -> Option<String> {
        match v {
            serde_json::Value::Object(m) if m.get("audition").is_some_and(|a| !a.is_null()) => Some(format!("{at}/audition")),
            serde_json::Value::Object(m) => m.iter().find_map(|(k, v)| path(v, format!("{at}/{k}"))),
            serde_json::Value::Array(a) => a.iter().enumerate().find_map(|(n, v)| path(v, format!("{at}/{n}"))),
            _ => None,
        }
    }
    let at = path(json, String::new()).expect("a saved audition");
    json.pointer_mut(&at).expect("the audition")
}

#[test]
fn an_auditions_takes_come_back() {
    let (seq, wide_id) = with_takes();
    let back = round_trip("takes", seq.clone(), |_| {});
    let slot = &back.tracks[0].clips[0];
    let a = slot.audition.as_deref().expect("the audition");
    let was = seq.tracks[0].clips[0].audition.as_deref().unwrap();
    assert_eq!(a.active, was.active, "the take in the slot");
    assert_eq!(a.takes.iter().map(|t| (t.id, t.name.as_str())).collect::<Vec<_>>(), was.takes.iter().map(|t| (t.id, t.name.as_str())).collect::<Vec<_>>(), "the takes, in order");
    let wide = a.takes.iter().find(|t| t.id == wide_id).unwrap();
    let (got, want) = (wide.clip.as_deref().unwrap(), was.takes[1].clip.as_deref().unwrap());
    assert_eq!((got.id, got.media_id(), got.source_in, got.source_out, got.timeline_out), (want.id, want.media_id(), want.source_in, want.source_out, want.timeline_out), "its clip");
    assert_eq!(klipp_timeline::test_clip::effects(got).len(), 1, "its blur");
    assert_eq!(wide.sound.len(), 1);
    assert_eq!((wide.sound[0].0, wide.sound[0].1.linked_clip_id), (back.tracks.iter().find(|t| t.name == "A1").unwrap().id, Some(got.id)), "its sound, with its track");
    assert!(a.takes[0].clip.is_none(), "the active take is the slot");
}

#[test]
fn a_malformed_audition_is_dropped_and_the_slot_kept() {
    // The active take names no take.
    let (seq, _) = with_takes();
    let back = round_trip("no-active", seq.clone(), |j| audition(j)["active"] = serde_json::json!(TakeId::new().to_string()));
    assert!(back.tracks[0].clips[0].audition.is_none(), "dropped");
    assert_eq!(back.tracks[0].clips[0].timeline_out, tc(48), "the slot kept");
    // Two takes with no clip: two slots.
    let back = round_trip("two-slots", seq.clone(), |j| {
        let a = audition(j);
        a["takes"][1].as_object_mut().unwrap().remove("clip");
    });
    assert!(back.tracks[0].clips[0].audition.is_none());
    // A take's clip holding an audition of its own: that one is left out.
    let back = round_trip("nested", seq, |j| {
        let a = audition(j).clone();
        audition(j)["takes"][1]["clip"]["audition"] = a;
    });
    let a = back.tracks[0].clips[0].audition.as_deref().expect("the slot's own audition kept");
    assert!(a.takes[1].clip.as_deref().unwrap().audition.is_none(), "the nested one left out");
}

/// #3209: media and fonts only an inactive take uses still count as in
/// use.
#[test]
fn an_inactive_takes_media_is_in_use() {
    let (seq, _) = with_takes();
    let take_media = seq.tracks[0].clips[0].audition.as_ref().unwrap().takes[1].clip.as_ref().unwrap().media_id().unwrap();
    assert!(klipp_project::consolidate_trim::used_media(&MediaPool::new(), &[seq]).contains(&take_media));
}
