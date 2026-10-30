//! Aux buses and sends (#2861) are saved with the project and read back;
//! a project without any writes nothing for them.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{AuxBus, AuxSend, Sequence, Track, TrackKind};

#[test]
fn buses_and_sends_are_saved_and_read_back() {
    let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    let mut seq = Sequence::new_default();
    let (mut verb, delay) = (AuxBus::new("Verb"), AuxBus::new("Delay"));
    verb.output = Some(delay.id);
    verb.volume = 0.5;
    verb.effects = vec![klipp_core::AudioEffectInstance { effect_type: "reverb".into(), enabled: true, wet_dry: 1.0, params: vec![("room".into(), 0.7)] }];
    let mut music = Track::new("Music", TrackKind::Audio);
    music.sends = vec![AuxSend { level: 0.5, pre_fader: true, muted: true, keys: vec![key(1.0, 1.0), key(2.0, 0.0)], ..AuxSend::new(verb.id) }, AuxSend::new(delay.id)];
    seq.tracks.push(music);
    seq.tracks.push(Track::new("Dialogue", TrackKind::Audio));
    seq.buses = vec![verb, delay];
    let dir = std::env::temp_dir().join(format!("klipp_aux_routing_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq.clone()], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    let (mut sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back = sequences.remove(0);
    assert_eq!(back.buses, seq.buses);
    let sends = |s: &Sequence, name: &str| s.tracks.iter().find(|t| t.name == name).map(|t| t.sends.clone()).unwrap();
    assert_eq!(sends(&back, "Music"), sends(&seq, "Music"));
    assert!(sends(&back, "Dialogue").is_empty());
    assert_eq!(written.matches("\"sends\"").count(), 1, "only the sending track writes them");

    let plain = Sequence::new_default();
    let dir = std::env::temp_dir().join(format!("klipp_aux_routing_none_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[plain], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!written.contains("\"buses\"") && !written.contains("\"sends\""), "nothing new in a project without them");
}
