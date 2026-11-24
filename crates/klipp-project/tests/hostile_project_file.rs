//! A project file whose clips say what no clip can be, written by hand
//! or damaged: a time with no rate, an end before its start, a speed of
//! nought. It opens with each clip put right rather than playing with a
//! time divided by nothing.

use klipp_core::{MediaId, RationalTime};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, Sequence};

#[test]
fn a_clip_the_file_got_wrong_opens_put_right() {
    let dir = std::env::temp_dir().join(format!("klipp-hostile-project-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let at = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut seq = Sequence::new_default();
    let clip = Clip::media(MediaId::new(), at(1.0), at(3.0), at(0.5), at(2.5));
    let id = clip.id;
    seq.tracks[0].clips = vec![clip];
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();

    // The clip's times and speed spoiled in the file.
    let path = dir.join("project.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut spoiled = 0;
    fn spoil(v: &mut serde_json::Value, id: &str, spoiled: &mut usize) {
        match v {
            serde_json::Value::Object(map) => {
                if map.get("id").and_then(|i| i.as_str()) == Some(id) && map.contains_key("timeline_in") {
                    map.insert("rate".into(), 0.into());
                    let (a, b) = (map["timeline_in"].clone(), map["timeline_out"].clone());
                    map.insert("timeline_in".into(), b);
                    map.insert("timeline_out".into(), a);
                    map.insert("speed".into(), 0.0.into());
                    *spoiled += 1;
                }
                map.values_mut().for_each(|x| spoil(x, id, spoiled));
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(|x| spoil(x, id, spoiled)),
            _ => {}
        }
    }
    spoil(&mut json, &id.0.to_string(), &mut spoiled);
    assert_eq!(spoiled, 1, "the clip is in the file");
    std::fs::write(&path, json.to_string()).unwrap();

    let (loaded, ..) = load_project(&dir).expect("it opens");
    let clip = loaded[0].all_clips().find(|c| c.id == id).expect("the clip is there");
    assert!(klipp_timeline::invariants::clip_problems(clip).is_empty(), "{:?}", klipp_timeline::invariants::clip_problems(clip));
    assert!(clip.timeline_in.rate > 0 && clip.timeline_out >= clip.timeline_in, "{:?}..{:?}", clip.timeline_in, clip.timeline_out);
    assert_eq!(clip.speed, 1.0, "a speed of nought plays at 1x");
    assert!(clip.timeline_in.to_secs_f64().is_finite());
}
