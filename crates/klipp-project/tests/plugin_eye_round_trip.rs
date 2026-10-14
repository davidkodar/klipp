//! A plug-in's eye switched off, as the stack and the A/B bypass switch it
//! (`Clip::toggle_effect_node`), is saved and comes back off: the eye
//! writes the entry's own flag, which the stored plug-in node keeps
//! (picture revision 4's `klipp.ofx` and `klipp.frei0r`).

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

fn clip_with_plugins() -> klipp_timeline::Clip {
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut c = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let glow = serde_json::from_value(serde_json::json!({"plugin_name": "org.example.Glow", "enabled": true, "context": "Filter", "transition_amount": 0.0, "param_values": {"amount": {"Double": 0.1}}})).unwrap();
    c.set_ofx_effects(vec![glow]);
    let frei0r: klipp_effects::BuiltinEffect = serde_json::from_value(serde_json::json!({"type": "frei0r", "type_id": "frei0r:/x.so:X", "params": [{"Double": 0.1}]})).unwrap();
    c.set_effect_list(vec![frei0r], vec![true]);
    c
}

#[test]
fn a_plug_ins_eye_switched_off_comes_back_off() {
    let dir = std::env::temp_dir().join(format!("klipp_plugin_eye_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut seq = Sequence::new_default();
    let mut c = clip_with_plugins();
    let ofx = c.effect_node_at(true, 0).expect("the OFX node");
    let frei0r = c.effect_node_at(false, 0).expect("the frei0r node");
    assert!(c.toggle_effect_node(ofx), "the OFX eye");
    assert!(c.toggle_effect_node(frei0r), "the frei0r eye");
    let id = c.id;
    seq.tracks[0].clips.push(c);
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(written.contains(klipp_graph_format::lifted::types::OFX) && written.contains(klipp_graph_format::lifted::types::FREI0R), "stored as their own nodes");
    let (sequences, ..) = load_project(&dir).unwrap();
    let back = sequences[0].all_clips().find(|c| c.id == id).expect("the clip");
    assert!(!back.ofx_effects()[0].enabled, "the OFX effect is off");
    assert_eq!(back.builtin_effects_enabled().first(), Some(&false), "the frei0r effect is off");
    let _ = std::fs::remove_dir_all(&dir);
}

/// #1500: an OFX entry's own id is saved and comes back, so its plug-in
/// runs on the same node, and instance, after a reload. Control: an entry
/// without one comes back without one.
#[test]
fn a_plug_ins_own_id_survives_save_and_load() {
    let dir = std::env::temp_dir().join(format!("klipp_plugin_id_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = clip_with_plugins();
    let echo = klipp_plugin::AppliedEffect { plugin_name: "org.example.Echo".into(), enabled: true, ..Default::default() };
    c.apply_ofx_edits(&[klipp_timeline::OfxEdit::Add(&echo)]).unwrap();
    let ids_of = |c: &klipp_timeline::Clip| c.picture_ofx().unwrap().into_iter().map(|o| o.entry.id).collect::<Vec<_>>();
    let ids = ids_of(&c);
    assert!(ids[0].is_none() && ids[1].is_some(), "{ids:?}");
    let id = c.id;
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(c);
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let back = sequences[0].all_clips().find(|c| c.id == id).expect("the clip");
    assert_eq!(ids_of(back), ids, "each entry's id, or none, as saved");
    let _ = std::fs::remove_dir_all(&dir);
}
